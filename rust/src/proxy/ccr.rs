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

/// Length of the content hash in a proxy/json tee name ([`hash_short`]).
const TEE_HASH_LEN: usize = 16;
/// Length of the command hash in a shell tee name (`shell::redact::save_tee`).
const SHELL_TEE_HASH_LEN: usize = 8;

/// Deterministic tee path for `content`:
/// `{state}/tee/{prefix}_{blake3(content)[..16]}.log`. Pure (no I/O) so a stub
/// embedding it stays byte-stable regardless of filesystem state. `prefix`
/// segregates the producer (`proxy` for history-prune / live-compression stubs,
/// `conv` for conversation history messages, `json` for the JSON crusher's
/// lossy originals, #936) yet keeps one shared
/// store + one resolver ([`resolve_tee`]).
fn tee_path(content: &str, prefix: &str) -> Option<PathBuf> {
    let dir = crate::core::paths::state_dir().ok()?.join("tee");
    let hash = crate::core::hasher::hash_short(content);
    Some(dir.join(format!("{prefix}_{hash}.log")))
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
    persist_with(content, "proxy")
}

/// Persist a dropped conversation message under a compact `conv_` handle.
///
/// Conversation messages may be short, so unlike tool-output tees this path has
/// no minimum-size gate. The returned basename is sufficient for `ctx_expand` and
/// avoids putting machine-specific absolute paths into provider prompts.
#[cfg_attr(not(test), allow(dead_code))] // conversation tee handle for ctx_expand recovery
pub(crate) fn persist_conversation(content: &str) -> Option<String> {
    let path = persist_with_min(content, "conv", 1)?;
    Path::new(&path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
}

/// Persist a JSON crusher's verbatim original (#936) under the `json_` prefix and
/// return its `{state}/tee/json_{hash}.log` handle. Used by the lossy crush stage
/// so a dropped column is always recoverable out-of-band via [`resolve_tee`] /
/// `ctx_expand`, never reconstructed from the (lossy) text. Shares the
/// content-address and best-effort write contract of [`persist`], so the embedded
/// handle stays deterministic (cache-safe).
pub(crate) fn persist_json(content: &str) -> Option<String> {
    persist_with(content, "json")
}

/// Persist a tabular (CSV/TSV) crusher's verbatim original (#982) under the
/// `tbl_` prefix and return its `{state}/tee/tbl_{hash}.log` handle. Used by the
/// lossy column-drop stage so a dropped column is always recoverable out-of-band
/// via [`resolve_tee`] / `ctx_expand`, never reconstructed from the (lossy) text.
/// Shares the content-address and best-effort write contract of [`persist`].
pub(crate) fn persist_tabular(content: &str) -> Option<String> {
    persist_with(content, "tbl")
}

/// Persist a YAML crusher's verbatim original (#985) under the `yaml_` prefix and
/// return its `{state}/tee/yaml_{hash}.log` handle. Used by the lossy column-drop
/// stage so a dropped column is always recoverable out-of-band via [`resolve_tee`]
/// / `ctx_expand`, never reconstructed from the (lossy) text. Shares the
/// content-address and best-effort write contract of [`persist`].
pub(crate) fn persist_yaml(content: &str) -> Option<String> {
    persist_with(content, "yaml")
}

/// Persist an HTML page's verbatim original (#1124) under the `html_` prefix
/// and return its `{state}/tee/html_{hash}.log` handle. The extracted markdown
/// is the compressed form; the full HTML is recoverable via [`resolve_tee`] /
/// `ctx_expand`. Shares the content-address and best-effort write contract of
/// [`persist`].
pub(crate) fn persist_html(content: &str) -> Option<String> {
    persist_with(content, "html")
}

fn persist_with(content: &str, prefix: &str) -> Option<String> {
    persist_with_min(content, prefix, MIN_TEE_BYTES)
}

fn persist_with_min(content: &str, prefix: &str, min_bytes: usize) -> Option<String> {
    debug_assert!(
        TEE_PREFIXES.contains(&prefix),
        "tee prefix {prefix} must be resolvable (add it to TEE_PREFIXES)"
    );
    if content.len() < min_bytes {
        return None;
    }
    let path = tee_path(content, prefix)?;
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
    if path.is_file() {
        let source_tool = match prefix {
            "json" | "tbl" | "yaml" => "ctx_read",
            "html" => "ctx_shell",
            _ => "proxy",
        };
        crate::core::relevance_tracker::register_compressed(
            handle.clone(),
            content,
            source_tool,
            crate::core::tokens::count_tokens(content),
            0,
        );
    }
    Some(handle)
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Every prefix [`persist_with_min`] writes. The resolver is driven by the same
/// table, so a new producer can never mint a handle `ctx_expand` rejects
/// (#1911: `html_` was persisted but unresolvable).
const TEE_PREFIXES: &[&str] = &["proxy", "conv", "json", "tbl", "yaml", "html"];

/// Canonical `{prefix}_{16hex}.log` name for a prefixed ([`TEE_PREFIXES`]) or
/// bare-hash id, or `None`. A bare 16-hex id defaults to the `proxy_` store
/// (back-compat: that is the only form pre-#936 stubs carry).
fn canonical_tee_name(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".log").unwrap_or(name);
    for prefix in TEE_PREFIXES {
        if let Some(hash) = stem
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('_'))
        {
            return is_hex(hash, TEE_HASH_LEN).then(|| format!("{prefix}_{hash}.log"));
        }
    }
    is_hex(stem, TEE_HASH_LEN).then(|| format!("proxy_{stem}.log"))
}

/// True for a shell tee basename `<slug>_<8hex>.log` (`shell::redact::save_tee`):
/// ends in `.log`, the whole basename is safe (`[A-Za-z0-9_-]`), and the **last**
/// `_`-segment is exactly 8 hex. The slug itself may contain `_`, so the hash is
/// matched as the suffix — never the first segment (the documented parsing trap).
fn is_shell_tee_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".log") else {
        return false;
    };
    if !stem
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return false;
    }
    match stem.rsplit_once('_') {
        Some((slug, hash)) => !slug.is_empty() && is_hex(hash, SHELL_TEE_HASH_LEN),
        None => false,
    }
}

/// Why a canonical tee handle could not be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TeeResolveError {
    Malformed,
    Missing,
    Refused(&'static str),
}

/// Resolve a retrieval ID through the same canonical store path used by
/// `ctx_expand`, while distinguishing malformed IDs and policy refusals.
pub(crate) fn resolve_tee_checked(id: &str) -> Result<PathBuf, TeeResolveError> {
    let name = Path::new(id)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(id);
    let canon = canonical_tee_name(name)
        .or_else(|| is_shell_tee_name(name).then(|| name.to_string()))
        .ok_or(TeeResolveError::Malformed)?;
    let state_dir = crate::core::paths::state_dir().map_err(|_| TeeResolveError::Missing)?;
    let tee_dir = state_dir.join("tee");
    let path = tee_dir.join(canon);
    // The tee store itself must be a real directory: a `tee` link to another
    // store (archives, references) would let a tee-shaped name read that store.
    if std::fs::symlink_metadata(&tee_dir).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(TeeResolveError::Refused(
            "tee store is a symlink to another location",
        ));
    }

    let link_metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            TeeResolveError::Refused("tee metadata read refused by filesystem policy")
        } else {
            TeeResolveError::Missing
        }
    })?;
    if link_metadata.file_type().is_symlink() {
        return Err(TeeResolveError::Refused(
            "tee handles cannot resolve through symlinks",
        ));
    }
    if !link_metadata.is_file() {
        return Err(TeeResolveError::Missing);
    }

    let canonical_state = std::fs::canonicalize(&state_dir).map_err(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            TeeResolveError::Refused("tee store access refused by filesystem policy")
        } else {
            TeeResolveError::Missing
        }
    })?;
    let canonical_tee = std::fs::canonicalize(&tee_dir).map_err(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            TeeResolveError::Refused("tee store access refused by filesystem policy")
        } else {
            TeeResolveError::Missing
        }
    })?;
    if canonical_tee != canonical_state.join("tee") {
        return Err(TeeResolveError::Refused(
            "tee store resolves outside the state directory",
        ));
    }

    let canonical_file = std::fs::canonicalize(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            TeeResolveError::Refused("tee read refused by filesystem policy")
        } else {
            TeeResolveError::Missing
        }
    })?;
    if !canonical_file.starts_with(&canonical_tee) {
        return Err(TeeResolveError::Refused(
            "tee handle resolves outside the tee store",
        ));
    }
    if !canonical_file.is_file() {
        return Err(TeeResolveError::Missing);
    }
    Ok(path)
}

/// Whether `id` uses one of the handle shapes emitted by the tee producers.
pub(crate) fn is_tee_handle(id: &str) -> bool {
    let name = Path::new(id)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(id);
    canonical_tee_name(name).is_some() || is_shell_tee_name(name)
}

/// Whether the input is a legacy bare 16-hex CCR ID that can also be tried as
/// an archive ID after the tee store lookup, matching `ctx_expand` precedence.
pub(crate) fn is_bare_tee_handle(id: &str) -> bool {
    is_hex(id, TEE_HASH_LEN)
}

/// Catch malformed IDs that still declare a known tee producer prefix.
pub(crate) fn looks_like_tee_handle(id: &str) -> bool {
    let name = Path::new(id)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(id);
    let stem = name.strip_suffix(".log").unwrap_or(name);
    TEE_PREFIXES
        .iter()
        .any(|prefix| stem.starts_with(&format!("{prefix}_")))
}

/// Back-compatible resolver used by `ctx_expand`.
pub(crate) fn resolve_tee(id: &str) -> Option<PathBuf> {
    resolve_tee_checked(id).ok()
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

/// Read a tee file that [`resolve_tee`] returned, without following a link
/// planted after resolution (the read is bound to the checked file).
pub(crate) fn read_tee_file(path: &Path) -> Option<String> {
    let tee_dir = crate::core::paths::state_dir().ok()?.join("tee");
    crate::core::atomic_fs::read_store_file(&tee_dir, path).ok()
}

/// Resolve and read a tee handle in one step.
pub(crate) fn read_tee(id: &str) -> Option<String> {
    read_tee_file(&resolve_tee(id)?)
}

/// Recover the verbatim original for a 16-hex CCR `hash` from the local tee
/// store, or `None` when the hash is malformed or the file is gone (past TTL).
fn recover(hash: &str) -> Option<String> {
    if hash.len() != 16 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    read_tee(hash)
}

/// Length of a LiteLLM gateway retrieval hash (#702): LiteLLM's headroom
/// guardrail scans compressed text with `hash=([a-f0-9]{24})`
/// (BerriAI/litellm#31681), so the marker must carry exactly 24 lowercase hex.
pub(crate) const LITELLM_HASH_LEN: usize = 24;

/// The 24-hex gateway retrieval hash for `content` (#702): the first 24 chars
/// of the full blake3 hex. A strict extension of the 16-hex tee name
/// ([`crate::core::hasher::hash_short`] is the same digest truncated to 16),
/// so `hash[..16]` resolves the tee file while the extra 8 chars keep the
/// marker collision-resistant on the gateway side. Pure — the marker embedding
/// it stays byte-stable per content (#498).
pub(crate) fn litellm_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex()[..LITELLM_HASH_LEN].to_string()
}

/// Resolve a LiteLLM `hash=<24hex>` retrieval id (#702) back to the verbatim
/// original in the tee store, for `GET /v1/retrieve/{hash}`. Shape-locked to
/// LiteLLM's own regex (24 lowercase hex — uppercase or any other length is
/// rejected, never coerced) so only a string the guardrail could actually have
/// captured resolves; the first 16 chars are the `proxy_` tee content-address.
pub(crate) fn retrieve_litellm(hash: &str) -> Option<String> {
    if hash.len() != LITELLM_HASH_LEN
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    read_tee(&hash[..TEE_HASH_LEN])
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
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    fn conversation_handle_is_compact_and_deterministic() {
        let _lock = crate::core::data_dir::test_env_lock();
        let a = persist_conversation("{\"role\":\"user\",\"content\":\"hello\"}")
            .expect("conversation handle");
        let b = persist_conversation("{\"role\":\"user\",\"content\":\"hello\"}")
            .expect("conversation handle");
        assert_eq!(a, b);
        assert!(a.starts_with("conv_") && a.ends_with(".log"));
    }

    #[test]
    fn conversation_handle_resolves_short_messages() {
        let _lock = crate::core::data_dir::test_env_lock();
        let handle = persist_conversation("short conversation message").expect("handle");
        assert!(resolve_tee(&handle).is_some());
    }

    #[test]
    fn conversation_original_is_recoverable() {
        let _lock = crate::core::data_dir::test_env_lock();
        let original = "{\"role\":\"tool\",\"content\":\"recover me\"}";
        let handle = persist_conversation(original).expect("handle");
        let path = resolve_tee(&handle).expect("resolved handle");
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
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

    /// #1911: the HTML crush's `html_` handle must round-trip through the
    /// resolver `ctx_expand` uses, in every form the footer can be copied in.
    #[test]
    fn persist_html_handle_resolves_in_every_form() {
        let _lock = crate::core::data_dir::test_env_lock();
        let content = big("<p>html crush original</p>");
        let handle = persist_html(&content).expect("html persisted");
        let hash = crate::core::hasher::hash_short(&content);
        for form in [
            handle.clone(),
            format!("html_{hash}.log"),
            format!("html_{hash}"),
        ] {
            let resolved = resolve_tee(&form).unwrap_or_else(|| panic!("must resolve {form}"));
            assert_eq!(resolved.to_string_lossy(), handle, "form {form}");
        }
        assert!(
            std::fs::read_to_string(&handle)
                .unwrap()
                .contains("html crush original")
        );
    }

    /// Every prefix a producer can write is accepted by the resolver, and a
    /// prefix without its `_` separator is not.
    #[test]
    fn canonical_tee_name_covers_every_producer_prefix() {
        let hash = "0123456789abcdef";
        for prefix in TEE_PREFIXES {
            assert_eq!(
                canonical_tee_name(&format!("{prefix}_{hash}.log")),
                Some(format!("{prefix}_{hash}.log"))
            );
        }
        assert_eq!(canonical_tee_name(&format!("html{hash}")), None);
        assert_eq!(canonical_tee_name("html_nothex000000000"), None);
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
    fn persist_json_is_distinct_prefix_and_resolvable() {
        let _lock = crate::core::data_dir::test_env_lock();
        let content = big("json crusher original");
        let proxy = persist(&content).expect("proxy persisted");
        let json = persist_json(&content).expect("json persisted");
        assert!(
            json.contains("json_"),
            "json handle uses json_ prefix: {json}"
        );
        assert_ne!(
            proxy, json,
            "same content gets distinct files per producer prefix"
        );

        // The json_ handle resolves through the unified resolver in every form a
        // stub / footer can carry: full path, bare file name, and bare json_id.
        let hash = crate::core::hasher::hash_short(&content);
        for form in [
            json.clone(),
            format!("json_{hash}.log"),
            format!("json_{hash}"),
        ] {
            assert_eq!(
                resolve_tee(&form)
                    .expect("json form resolves")
                    .to_string_lossy(),
                json,
                "json form {form} -> {json}"
            );
        }
    }

    #[test]
    fn persist_tabular_is_distinct_prefix_and_resolvable() {
        let _lock = crate::core::data_dir::test_env_lock();
        let content = big("tabular crusher original");
        let json = persist_json(&content).expect("json persisted");
        let tbl = persist_tabular(&content).expect("tbl persisted");
        assert!(
            tbl.contains("tbl_"),
            "tabular handle uses tbl_ prefix: {tbl}"
        );
        assert_ne!(
            json, tbl,
            "same content gets distinct files per producer prefix"
        );

        let hash = crate::core::hasher::hash_short(&content);
        for form in [
            tbl.clone(),
            format!("tbl_{hash}.log"),
            format!("tbl_{hash}"),
        ] {
            assert_eq!(
                resolve_tee(&form)
                    .expect("tbl form resolves")
                    .to_string_lossy(),
                tbl,
                "tbl form {form} -> {tbl}"
            );
        }
    }

    #[test]
    fn persist_yaml_is_distinct_prefix_and_resolvable() {
        let _lock = crate::core::data_dir::test_env_lock();
        let content = big("yaml crusher original");
        let tbl = persist_tabular(&content).expect("tbl persisted");
        let yaml = persist_yaml(&content).expect("yaml persisted");
        assert!(
            yaml.contains("yaml_"),
            "yaml handle uses yaml_ prefix: {yaml}"
        );
        assert_ne!(
            tbl, yaml,
            "same content gets distinct files per producer prefix"
        );

        let hash = crate::core::hasher::hash_short(&content);
        for form in [
            yaml.clone(),
            format!("yaml_{hash}.log"),
            format!("yaml_{hash}"),
        ] {
            assert_eq!(
                resolve_tee(&form)
                    .expect("yaml form resolves")
                    .to_string_lossy(),
                yaml,
                "yaml form {form} -> {yaml}"
            );
        }
    }

    #[test]
    fn resolve_tee_resolves_shell_tee_with_underscored_slug() {
        let _lock = crate::core::data_dir::test_env_lock();
        // A real shell command whose slug contains underscores: the hash is the
        // last `_`-segment (8 hex), never the first — the parsing trap. The full
        // verbatim output the shell already teed is now surgically retrievable.
        let path = crate::shell::save_tee("gh api /repos/foo/bar", &big("api row"))
            .expect("shell tee saved");
        let name = std::path::Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap()
            .to_string();
        assert!(
            is_shell_tee_name(&name),
            "save_tee name must be recognized as a shell tee: {name}"
        );
        for form in [path.clone(), name] {
            assert_eq!(
                resolve_tee(&form)
                    .expect("shell tee form resolves")
                    .to_string_lossy(),
                path,
                "shell tee form -> {path}"
            );
        }
    }

    #[test]
    fn resolve_tee_does_not_capture_reference_ids() {
        let _lock = crate::core::data_dir::test_env_lock();
        // A reference-store id (`ref_<16hex>`, no `.log`) must fall through the
        // tee resolver so ctx_expand routes it to the reference store, not the
        // tee store — the precedence guard for the unified retrieve ladder.
        assert!(resolve_tee("ref_deadbeefcafef00d").is_none());
        // A bare 16-hex archive id with no backing tee file also stays None.
        assert!(resolve_tee("0123456789abcdef").is_none());
    }

    #[test]
    fn litellm_hash_is_24_lowercase_hex_and_extends_tee_hash() {
        let content = big("gateway retrieval body");
        let hash = litellm_hash(&content);
        assert_eq!(hash.len(), LITELLM_HASH_LEN);
        assert!(
            hash.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "must match LiteLLM's [a-f0-9]{{24}} class: {hash}"
        );
        // Same blake3 digest as the tee name, longer prefix: the first 16 chars
        // ARE the tee content-address, which is what makes retrieval work.
        assert_eq!(hash[..16], crate::core::hasher::hash_short(&content));
        // Pure function of content (#498): stable across calls.
        assert_eq!(hash, litellm_hash(&content));
    }

    #[test]
    fn retrieve_litellm_resolves_persisted_content() {
        let _lock = crate::core::data_dir::test_env_lock();
        let content = big("litellm retrievable original");
        persist(&content).expect("persisted");
        let recovered =
            retrieve_litellm(&litellm_hash(&content)).expect("24-hex hash must resolve");
        assert!(recovered.contains("litellm retrievable original"));
    }

    #[test]
    fn retrieve_litellm_is_shape_locked_to_the_guardrail_regex() {
        let _lock = crate::core::data_dir::test_env_lock();
        let content = big("shape locked body");
        persist(&content).expect("persisted");
        let hash = litellm_hash(&content);

        // 16-hex (our tee id), truncated, extended, uppercased, non-hex: all
        // rejected — only the exact shape LiteLLM's regex captures resolves.
        assert!(retrieve_litellm(&hash[..16]).is_none(), "16-hex rejected");
        assert!(retrieve_litellm(&hash[..23]).is_none(), "23-hex rejected");
        assert!(
            retrieve_litellm(&format!("{hash}0")).is_none(),
            "25-hex rejected"
        );
        assert!(
            retrieve_litellm(&hash.to_uppercase()).is_none(),
            "uppercase rejected (regex class is [a-f0-9])"
        );
        assert!(
            retrieve_litellm("zzzzzzzzzzzzzzzzzzzzzzzz").is_none(),
            "non-hex rejected"
        );
        // Traversal attempts die in resolve_tee's name canonicalization.
        assert!(retrieve_litellm("../../etc/passwd00000000").is_none());
        // Right shape, unknown content.
        assert!(retrieve_litellm("0123456789abcdef01234567").is_none());
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
