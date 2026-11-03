// SPDX-License-Identifier: Apache-2.0
use super::{CrpMode, ReadMode};

/// Pre-counted read output carrying the output string, resolved mode,
/// and token count computed during mode processing.
pub struct ReadOutput {
    pub content: String,
    pub resolved_mode: String,
    /// Approximate output token count from mode processing.
    /// The dispatch layer recounts the final assembled string for accurate savings.
    pub output_tokens: usize,
    /// Structurally determined cache-hit flag (#1133). Set by the code that
    /// serves the read (stub, delta, cached mode), not sniffed from rendered
    /// output. Replaces the fragile `content.contains("[unchanged")` checks.
    pub is_cache_hit: bool,
}

/// SSOT via [`ReadMode`] (#528): the `map`/`signatures` summaries whose rendered
/// body is stored per-file in `compressed_outputs`. Unknown modes are not
/// cacheable, matching the prior `["map","signatures"].contains(mode)`.
pub(crate) fn is_cacheable_mode(mode: &str) -> bool {
    mode.parse::<ReadMode>()
        .is_ok_and(|m| m.is_compressed_cacheable())
}

/// `#361` anti-inflation capping applies to whole-file views (`full` and the
/// lossy summaries `map`/`signatures`/`aggressive`/`entropy`/`task`/…), where the
/// raw file is a strict superset of the information and is therefore never a
/// worse answer when the framing happens to inflate on a small file. `full` is
/// included: an `auto` read can resolve to `full` and reach this path, and its
/// header must not push the cost above raw. Selection and delta views have
/// view-specific semantics — `lines:` returns a window, `reference` a pointer,
/// `diff` a delta, `raw` the bytes — so replacing them with the whole file would
/// be wrong, not cheaper, and they are never capped.
pub(crate) fn mode_allows_raw_cap(mode: &str) -> bool {
    // SSOT via [`ReadMode`] (#528). Unknown modes keep the prior default of
    // `true` (only `lines:`/`reference`/`diff`/`raw` opt out of the #361 cap).
    mode.parse::<ReadMode>()
        .map_or(true, |m| m.allows_raw_cap())
}

pub(crate) fn compressed_cache_key(
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
    aggressiveness: Option<f64>,
    protect: &[String],
) -> String {
    // Bump when the rendered map/signatures body changes shape so stale
    // pre-line-range entries are not served from an older session cache.
    let versioned_mode = match mode {
        "map" => "map:v2",
        "signatures" => "signatures:v2",
        _ => mode,
    };
    let base = if crp_mode.is_tdd() {
        format!("{versioned_mode}:tdd")
    } else {
        versioned_mode.to_string()
    };
    // Structure-preserving modes (map, signatures) produce deterministic
    // structural summaries of the file content. Their output is a pure function
    // of (file_content, crp_mode) — NOT the task parameter. Excluding the task
    // hash from their cache key improves hit rate across task changes and
    // stabilizes output for provider-side prompt caching (#E26).
    //
    // Task-dependent modes (density, aggressive, entropy, task) DO embed
    // task-relevant filtering and MUST keep the task hash to avoid serving
    // stale task-specific content.
    let task_independent = matches!(mode, "map" | "signatures");
    let keyed = if task_independent {
        base
    } else {
        match task.map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                t.hash(&mut h);
                format!("{base}:t{:x}", h.finish())
            }
            None => base,
        }
    };
    // Aggressiveness and the explicit protect list both change lossy output, so
    // both must change the key (#498). Empty fragments keep pre-feature keys
    // byte-identical, so unmodified reads still hit their existing cache entries.
    let mut key = keyed;
    let aggr_frag = lean_ctx::core::aggressiveness::cache_fragment(aggressiveness);
    if !aggr_frag.is_empty() {
        key = format!("{key}:{aggr_frag}");
    }
    let protect_frag = lean_ctx::core::protect::protect_fragment(protect);
    if !protect_frag.is_empty() {
        key = format!("{key}:{protect_frag}");
    }
    key
}

/// Scopes a [`compressed_cache_key`] to the request kind (#1910). An explicit
/// request that falls back to the raw file carries a no-compression banner; the
/// same fallback inside an `auto` read is bannerless. Both resolve to the same
/// concrete mode, so without this suffix one variant would be served for the
/// other and output would depend on read order (#498). Explicit keys stay
/// byte-identical to their pre-#1910 form.
pub(crate) fn request_scoped_key(key: String, auto_request: bool) -> String {
    if auto_request {
        format!("{key}:auto")
    } else {
        key
    }
}

/// Appends the reactive recovery footer to a compressed view, leading with the
/// MCP-free "read the path directly" route. Tier (`off|minimal|full`) and wording
/// are resolved centrally in [`lean_ctx::core::recovery`] so `ctx_read`, the shell
/// tee and archive handles all speak the same grammar. Only lossy/compressed
/// modes reach this helper, so the footer is naturally absent from `full`/`raw`.
pub(super) fn append_compressed_hint(output: &str, file_path: &str) -> String {
    match lean_ctx::core::recovery::read_footer(file_path) {
        Some(footer) => format!("{output}\n{footer}"),
        None => output.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{CrpMode, compressed_cache_key};

    #[test]
    fn map_key_is_task_independent() {
        let k1 = compressed_cache_key("map", CrpMode::Off, Some("fix bug"), None, &[]);
        let k2 = compressed_cache_key("map", CrpMode::Off, Some("add feature"), None, &[]);
        let k3 = compressed_cache_key("map", CrpMode::Off, None, None, &[]);
        assert_eq!(k1, k2, "map key must be task-independent");
        assert_eq!(k1, k3, "map key must be same with or without task");
        assert_eq!(k1, "map:v2");
    }

    #[test]
    fn signatures_key_is_task_independent() {
        let k1 = compressed_cache_key("signatures", CrpMode::Off, Some("task A"), None, &[]);
        let k2 = compressed_cache_key("signatures", CrpMode::Off, Some("task B"), None, &[]);
        assert_eq!(k1, k2, "signatures key must be task-independent");
        assert_eq!(k1, "signatures:v2");
    }

    #[test]
    fn density_key_is_task_dependent() {
        let k1 = compressed_cache_key("density:0.3", CrpMode::Off, Some("fix bug"), None, &[]);
        let k2 = compressed_cache_key("density:0.3", CrpMode::Off, Some("add feature"), None, &[]);
        assert_ne!(k1, k2, "density key MUST vary with task");
    }

    #[test]
    fn map_key_still_includes_crp_mode() {
        let k1 = compressed_cache_key("map", CrpMode::Off, Some("task"), None, &[]);
        let k2 = compressed_cache_key("map", CrpMode::Tdd, Some("task"), None, &[]);
        assert_ne!(k1, k2, "CRP mode must still differentiate keys");
        assert_eq!(k2, "map:v2:tdd");
    }
}
