//! Content-addressed recovery (CCR) for the proxy's lossy rewrites (#482).
//!
//! When the proxy prunes an old `tool_result` from conversation history, the
//! lossy stub used to say *"re-read the file"* — which is stale-unsafe by
//! construction: in an agent session files are edited or deleted between turns,
//! so a re-read returns the *current* bytes (or fails), not the historical
//! version the conversation actually showed. The model could then silently
//! reason about the wrong content.
//!
//! CCR fixes this by persisting the **verbatim original** to the shared,
//! content-addressed tee store (`{state}/tee/`, reused from the shell path) and
//! embedding a **retrieval handle** — the absolute path of that file — in the
//! stub. Retrieval is MCP-independent: the agent reads the path with its native
//! file read; no lean-ctx tool has to be attached.
//!
//! ## Cache-safety (#448)
//! The handle is the file path, and the path is a pure function of the content
//! hash ([`crate::core::hasher::hash_short`]). For a fixed pruned message the
//! handle is therefore byte-identical on every later turn, so the provider
//! prompt-cache prefix is never invalidated. The on-disk *write* is best-effort
//! and never affects the returned handle — only retrievability degrades if the
//! write (or the 24h TTL cleanup) loses the file, so a stub can never become
//! non-deterministic based on filesystem state.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// Opening delimiter of an in-band retrieval marker: `<lc_expand:HASH>` (#493).
const EXPAND_OPEN: &str = "<lc_expand:";
/// Closing delimiter of an in-band retrieval marker.
const EXPAND_CLOSE: char = '>';

/// Originals smaller than this are not worth a tee file + handle; the caller
/// keeps its plain stub. Matches the spirit of the prune length thresholds.
pub(crate) const MIN_TEE_BYTES: usize = 512;

/// Throttle the O(dir) TTL cleanup so the prune hot path does at most one
/// directory scan per this interval (the write itself is content-addressed and
/// idempotent, so steady-state cost is a single `stat`).
const CLEANUP_INTERVAL_SECS: u64 = 600;

/// Deterministic tee path for `content`:
/// `{state}/tee/proxy_{blake3(content)[..16]}.log`. Pure (no I/O) so a stub
/// embedding it stays byte-stable regardless of filesystem state.
fn tee_path(content: &str) -> Option<PathBuf> {
    let dir = crate::core::paths::state_dir().ok()?.join("tee");
    let hash = crate::core::hasher::hash_short(content);
    Some(dir.join(format!("proxy_{hash}.log")))
}

/// Run the shared 24h TTL cleanup at most once per [`CLEANUP_INTERVAL_SECS`].
fn maybe_cleanup(tee_dir: &Path) {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return;
    };
    let now = now.as_secs();
    let last = LAST.load(Ordering::Relaxed);
    if now.saturating_sub(last) < CLEANUP_INTERVAL_SECS {
        return;
    }
    // Only one thread wins the slot; the rest skip until the next interval.
    if LAST
        .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        crate::shell::cleanup_old_tee_logs(tee_dir);
    }
}

/// Persist `content` verbatim (best-effort, secret-redacted) to the
/// content-addressed tee store and return its retrieval handle (the absolute
/// path). Returns `None` only when `content` is below [`MIN_TEE_BYTES`] or the
/// state dir can't be resolved — never because the *write* failed, so the
/// returned handle is a pure function of the content and the embedding stub
/// stays deterministic. Re-persisting identical content is idempotent: same
/// content → same path → the existing file is left untouched.
pub(crate) fn persist(content: &str) -> Option<String> {
    if content.len() < MIN_TEE_BYTES {
        return None;
    }
    let path = tee_path(content)?;
    let handle = path.to_string_lossy().to_string();

    if !path.exists() {
        if let Some(dir) = path.parent()
            && std::fs::create_dir_all(dir).is_ok()
        {
            maybe_cleanup(dir);
        }
        // Same redaction the shell tee applies, so a recovered original can never
        // re-introduce a secret the live turn would also have masked.
        let masked = crate::core::redaction::redact_text(content);
        let (redacted, _) = crate::core::secret_detection::scan_and_redact_from_config(&masked);
        if std::fs::write(&path, redacted).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
    Some(handle)
}

/// Resolve a CCR retrieval `id` (as carried in a proxy stub) back to the
/// existing tee file. Accepts any of the forms an agent might copy out of a
/// stub: the absolute tee path, the bare file name `proxy_<hash>.log`,
/// `proxy_<hash>`, or the bare `<hash>`.
///
/// Security: only the *file name* is trusted — the path is always rebuilt from
/// the canonical `{state}/tee/` dir, so a crafted `id` can never escape the tee
/// store (no path traversal) and a non-tee id simply resolves to `None`.
pub(crate) fn resolve_tee(id: &str) -> Option<PathBuf> {
    let name = Path::new(id)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(id);
    let hash = name.strip_prefix("proxy_").unwrap_or(name);
    let hash = hash.strip_suffix(".log").unwrap_or(hash);
    if hash.len() != 16 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let path = crate::core::paths::state_dir()
        .ok()?
        .join("tee")
        .join(format!("proxy_{hash}.log"));
    path.is_file().then_some(path)
}

/// The in-band retrieval marker `<lc_expand:HASH>` for a CCR `handle` (#493).
///
/// `HASH` is the content hash already embedded in the tee handle, so a model can
/// echo the marker verbatim and the proxy can recover the original via
/// [`resolve_tee`] on the next turn. Pure (no I/O, no config) so it is trivially
/// testable; returns `None` for a handle that is not a canonical tee path.
pub(crate) fn inband_marker(handle: &str) -> Option<String> {
    let name = Path::new(handle).file_name().and_then(|n| n.to_str())?;
    let hash = name.strip_prefix("proxy_")?.strip_suffix(".log")?;
    (hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("{EXPAND_OPEN}{hash}{EXPAND_CLOSE}"))
}

/// The in-band marker for `handle` **only when in-band CCR is enabled** (#493),
/// else `None`. Stub sites use this to advertise an echo-able `<lc_expand:HASH>`
/// solely in in-band mode: a normal (shared-filesystem) deployment keeps its
/// path handle, so the model never sees a marker the proxy would not splice.
///
/// Reads the (process-cached) config; the surrounding stub path already does
/// per-message tee I/O via [`persist`], so this adds no new I/O class.
pub(crate) fn inband_locator(handle: &str) -> Option<String> {
    crate::core::config::Config::load()
        .proxy
        .ccr_inband_enabled()
        .then(|| inband_marker(handle))
        .flatten()
}

/// Recover the verbatim original for a 16-hex CCR `hash` from the local tee
/// store, or `None` when the hash is malformed or the file is gone (past TTL).
fn recover(hash: &str) -> Option<String> {
    if hash.len() != 16 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    std::fs::read_to_string(resolve_tee(hash)?).ok()
}

/// Replace every `<lc_expand:HASH>` marker in `s` with the verbatim original
/// recovered from the local tee store. Returns `Some(spliced)` only when at
/// least one marker resolved, else `None` (so the caller leaves the string —
/// and therefore the request bytes — untouched).
///
/// An unresolvable marker (bad hash, or a file dropped past the 24h TTL) is left
/// in place verbatim: the model still sees its own marker rather than a silent
/// deletion, and a later turn can retry once the operator restores the file.
/// The spliced content is inserted **raw** (not `<lc_safe>`-wrapped): this runs
/// on the recent assistant turn the model echoed the marker into, which no proxy
/// compressor rewrites, and the proxy has no global `<lc_safe>` strip — wrapping
/// would instead leak the markers to the provider.
fn splice_str(s: &str) -> Option<String> {
    if !s.contains(EXPAND_OPEN) {
        return None;
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    let mut changed = false;
    while let Some(pos) = rest.find(EXPAND_OPEN) {
        let after = &rest[pos + EXPAND_OPEN.len()..];
        match after.find(EXPAND_CLOSE) {
            Some(end) => {
                let hash = &after[..end];
                if let Some(original) = recover(hash) {
                    out.push_str(&rest[..pos]);
                    out.push_str(&original);
                    rest = &after[end + EXPAND_CLOSE.len_utf8()..];
                    changed = true;
                } else {
                    // Keep the literal marker; resume scanning past this `<` so a
                    // later valid marker in the same string is still spliced.
                    out.push_str(&rest[..pos + EXPAND_OPEN.len()]);
                    rest = after;
                }
            }
            // No closing `>`: nothing more can match — keep the remainder verbatim.
            None => break,
        }
    }
    out.push_str(rest);
    changed.then_some(out)
}

/// Splice in-band `<lc_expand:HASH>` markers throughout a parsed request body
/// (#493), replacing each with the verbatim original recovered from the local
/// tee store. Recurses over every JSON string (object values and array items).
///
/// Returns `true` iff at least one marker was spliced. A request with no marker
/// is left **byte-identical** (the function never allocates a replacement), so a
/// marker-less turn never perturbs the provider prompt-cache prefix — the splice
/// only ever changes the bytes the model explicitly asked to expand.
pub(crate) fn splice_inband_in_place(value: &mut Value) -> bool {
    match value {
        Value::String(s) => {
            if let Some(spliced) = splice_str(s) {
                *s = spliced;
                true
            } else {
                false
            }
        }
        Value::Array(items) => {
            let mut changed = false;
            for item in items {
                changed |= splice_inband_in_place(item);
            }
            changed
        }
        Value::Object(map) => {
            let mut changed = false;
            for (_, v) in map.iter_mut() {
                changed |= splice_inband_in_place(v);
            }
            changed
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big(seed: &str) -> String {
        format!("{seed}\n").repeat(40)
    }

    #[test]
    fn handle_is_content_addressed_and_deterministic() {
        let _lock = crate::core::data_dir::test_env_lock();
        let content = big("file body line");
        let a = persist(&content).expect("persisted");
        let b = persist(&content).expect("persisted again");
        assert_eq!(
            a, b,
            "same content must map to the same handle (cache-safe)"
        );
        assert!(a.contains("proxy_"), "handle is a proxy tee path: {a}");

        let other = persist(&big("different body")).expect("persisted");
        assert_ne!(a, other, "different content must get a different handle");
    }

    #[test]
    fn persisted_original_is_recoverable() {
        let _lock = crate::core::data_dir::test_env_lock();
        let content = big("recoverable verbatim line");
        let handle = persist(&content).expect("persisted");
        let on_disk = std::fs::read_to_string(&handle).expect("tee file readable");
        assert!(
            on_disk.contains("recoverable verbatim line"),
            "the verbatim original must be retrievable from the handle"
        );
    }

    #[test]
    fn small_content_gets_no_handle() {
        let _lock = crate::core::data_dir::test_env_lock();
        assert!(
            persist("too small to bother").is_none(),
            "below MIN_TEE_BYTES there is no handle (the caller keeps its plain stub)"
        );
    }

    #[test]
    fn resolve_tee_accepts_every_stub_form() {
        let _lock = crate::core::data_dir::test_env_lock();
        let content = big("resolvable tee body");
        let handle = persist(&content).expect("persisted");
        let hash = crate::core::hasher::hash_short(&content);

        // Full path, bare file name, proxy_<hash>, and bare <hash> all resolve to
        // the same on-disk file — whatever the agent copied out of the stub.
        for form in [
            handle.clone(),
            format!("proxy_{hash}.log"),
            format!("proxy_{hash}"),
            hash.clone(),
        ] {
            let resolved = resolve_tee(&form).unwrap_or_else(|| panic!("must resolve {form}"));
            assert_eq!(
                resolved.to_string_lossy(),
                handle,
                "form {form} -> {handle}"
            );
        }
    }

    #[test]
    fn resolve_tee_rejects_nontee_and_traversal_ids() {
        let _lock = crate::core::data_dir::test_env_lock();
        // No FS escape: a crafted path is reduced to its file name, which is not a
        // valid proxy tee name, so it resolves to None instead of reading it.
        assert!(resolve_tee("/etc/passwd").is_none());
        assert!(resolve_tee("../../secret").is_none());
        assert!(resolve_tee("proxy_nothex0000000.log").is_none());
        // Right shape but no such file in the store.
        assert!(resolve_tee("deadbeefdeadbeef").is_none());
    }

    #[test]
    fn inband_marker_is_derived_from_handle() {
        let _lock = crate::core::data_dir::test_env_lock();
        let content = big("inband marker body");
        let handle = persist(&content).expect("persisted");
        let hash = crate::core::hasher::hash_short(&content);
        // The marker carries the same content hash the handle does, so a model can
        // echo it and the proxy resolves it back to the very same tee file.
        assert_eq!(inband_marker(&handle), Some(format!("<lc_expand:{hash}>")));
        // A non-tee handle has no marker.
        assert!(inband_marker("/tmp/not-a-tee.txt").is_none());
    }

    #[test]
    fn splice_replaces_marker_with_verbatim_original() {
        let _lock = crate::core::data_dir::test_env_lock();
        let content = big("the historical verbatim line");
        let handle = persist(&content).expect("persisted");
        let marker = inband_marker(&handle).expect("marker");

        let mut doc = serde_json::json!({
            "messages": [{ "role": "assistant", "content": format!("recall {marker} please") }]
        });
        assert!(splice_inband_in_place(&mut doc), "a marker must splice");
        let spliced = doc["messages"][0]["content"].as_str().unwrap();
        assert!(
            spliced.contains("the historical verbatim line"),
            "verbatim original must be spliced in: {spliced}"
        );
        assert!(
            !spliced.contains("<lc_expand:"),
            "the marker must be consumed, not left behind"
        );
    }

    #[test]
    fn splice_is_byte_identical_no_op_without_marker() {
        let _lock = crate::core::data_dir::test_env_lock();
        let mut doc = serde_json::json!({
            "messages": [{ "role": "user", "content": "no marker here" }],
            "system": "plain"
        });
        let before = doc.clone();
        assert!(
            !splice_inband_in_place(&mut doc),
            "no marker → must report no change"
        );
        assert_eq!(
            doc, before,
            "marker-less body must stay byte-identical (cache-safe)"
        );
    }

    #[test]
    fn splice_keeps_unresolvable_marker_verbatim() {
        let _lock = crate::core::data_dir::test_env_lock();
        // Right shape, but no such file in the store → leave the model's marker in
        // place rather than silently deleting it.
        let mut doc = serde_json::json!({ "t": "before <lc_expand:deadbeefdeadbeef> after" });
        assert!(!splice_inband_in_place(&mut doc));
        assert_eq!(
            doc["t"].as_str().unwrap(),
            "before <lc_expand:deadbeefdeadbeef> after"
        );
    }

    #[test]
    fn splice_recurses_and_handles_multiple_markers() {
        let _lock = crate::core::data_dir::test_env_lock();
        let a = big("first recovered body");
        let b = big("second recovered body");
        let ma = inband_marker(&persist(&a).unwrap()).unwrap();
        let mb = inband_marker(&persist(&b).unwrap()).unwrap();

        // Two markers in one nested string, plus a deeper array item.
        let mut doc = serde_json::json!({
            "contents": [
                { "parts": [{ "text": format!("{ma} and {mb}") }] }
            ]
        });
        assert!(splice_inband_in_place(&mut doc));
        let text = doc["contents"][0]["parts"][0]["text"].as_str().unwrap();
        assert!(text.contains("first recovered body"));
        assert!(text.contains("second recovered body"));
        assert!(!text.contains("<lc_expand:"));
    }
}
