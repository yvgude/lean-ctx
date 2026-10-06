// SPDX-License-Identifier: Apache-2.0
//! The "no compression applied" banner for raw fallbacks, and the `auto`
//! request scope that silences it (#1587, #1910). Split out of `render`.

/// Below this many raw tokens a silent full-content fallback is not worth a
/// banner: the file is small enough that the framing itself was the expensive
/// part (which is exactly what the #361 cap exists to strip), and a banner
/// would push the read back above the raw file it just protected. Above it, the
/// caller is being handed a whole file they did not order and must be told.
pub(crate) const NO_COMPRESSION_BANNER_MIN_TOKENS: usize = 400;

thread_local! {
    static AUTO_REQUEST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Marks the renders on this thread as serving a `mode=auto` read for the
/// guard's lifetime (#1910). The caller never ordered the concrete mode `auto`
/// picked, so a fallback to the raw file is lean-ctx's own choice working as
/// designed, not a failed request to report — and a banner on it is exactly
/// what made `auto` cost more than a plain read. Restores the previous value on
/// drop, so nested reads compose.
pub(crate) struct AutoRequestGuard {
    prev: bool,
}

impl AutoRequestGuard {
    pub(crate) fn new() -> Self {
        Self {
            prev: AUTO_REQUEST.with(|a| a.replace(true)),
        }
    }
}

impl Drop for AutoRequestGuard {
    fn drop(&mut self) {
        AUTO_REQUEST.with(|a| a.set(self.prev));
    }
}

/// One-line notice that a compression path gave up and returned the untouched
/// file. Without it the caller pays full-file tokens believing a summary was
/// delivered — the failure is invisible in the output and surfaces only on the
/// bill. `None` for files below [`NO_COMPRESSION_BANNER_MIN_TOKENS`], where the
/// fallback is the cap working as designed rather than a degradation, and for
/// `auto` reads (see [`AutoRequestGuard`]).
///
/// The fallback body still passes the per-turn budget, which only compressed
/// requests are held to (`raw=true` gets the larger verbatim one). When the
/// file exceeds it the banner says so instead of promising full content the
/// caller will not receive (#1910).
pub(crate) fn no_compression_banner(requested_mode: &str, raw_tokens: usize) -> Option<String> {
    if raw_tokens < NO_COMPRESSION_BANNER_MIN_TOKENS || AUTO_REQUEST.with(std::cell::Cell::get) {
        return None;
    }
    let limit = crate::core::config::Config::load().turn_fresh_limit_effective();
    let delivery = if limit > 0 && raw_tokens > limit {
        format!(
            "returning full content ({raw_tokens} tok), truncated to the {limit}-token turn \
             budget — use lines= or raw=true for the rest"
        )
    } else {
        format!("returning full content ({raw_tokens} tok)")
    };
    Some(format!(
        "[lean-ctx] no compression applied (mode={requested_mode}): \
         output was not smaller than the file — {delivery}"
    ))
}

#[cfg(test)]
mod tests {
    /// #1587: a compression request that degrades to the whole file says so.
    /// Below the threshold it stays silent, so the #361 cap still guarantees a
    /// read never costs more than the raw file.
    #[test]
    fn no_compression_banner_only_above_threshold() {
        let _lock = crate::core::data_dir::test_env_lock();
        assert!(super::no_compression_banner("signatures", 10).is_none());
        let banner =
            super::no_compression_banner("signatures", super::NO_COMPRESSION_BANNER_MIN_TOKENS)
                .expect("a whole file handed back instead of a summary must be announced");
        assert!(banner.contains("no compression applied"), "{banner}");
        assert!(banner.contains("mode=signatures"), "{banner}");
    }

    /// #1910: `auto` chose the mode itself, so its raw fallback is silent —
    /// otherwise an `auto` read of a small file costs more than a plain read.
    /// The guard is scoped: explicit requests after it drops are announced again.
    #[test]
    fn no_compression_banner_silent_for_auto_requests() {
        let _lock = crate::core::data_dir::test_env_lock();
        {
            let _auto = super::AutoRequestGuard::new();
            assert!(super::no_compression_banner("cognitive", 5_000).is_none());
            {
                let _nested = super::AutoRequestGuard::new();
            }
            assert!(
                super::no_compression_banner("cognitive", 5_000).is_none(),
                "a nested guard must restore, not clear, the outer state"
            );
        }
        assert!(super::no_compression_banner("cognitive", 5_000).is_some());
    }

    /// #1910: the fallback body still passes the per-turn budget, so the banner
    /// must not promise "full content" the caller will not receive.
    #[test]
    fn no_compression_banner_names_turn_budget_truncation() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_TURN_FRESH_LIMIT", "4096");
        let within = super::no_compression_banner("entropy", 4_000).expect("banner");
        let over = super::no_compression_banner("entropy", 9_889).expect("banner");
        crate::test_env::set_var("LEAN_CTX_TURN_FRESH_LIMIT", "0");
        let unlimited = super::no_compression_banner("entropy", 9_889).expect("banner");
        crate::test_env::remove_var("LEAN_CTX_TURN_FRESH_LIMIT");

        assert!(
            within.ends_with("returning full content (4000 tok)"),
            "{within}"
        );
        assert!(!within.contains("truncated"), "{within}");
        assert!(
            over.contains("truncated to the 4096-token turn budget") && over.contains("raw=true"),
            "{over}"
        );
        assert!(
            !unlimited.contains("truncated"),
            "0 = unlimited: {unlimited}"
        );
    }
}
