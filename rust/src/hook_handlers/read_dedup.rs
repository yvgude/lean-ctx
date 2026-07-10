//! PostToolUse re-read dedup for guard hosts (GL #1140, follow-up to GH #637).
//!
//! On Claude Code / CodeBuddy the PreToolUse Read redirect is disabled
//! (`read_redirect = auto`) so the native read-before-write guard stays intact —
//! which also forfeits the redirect's re-read dedup. This handler wins those
//! savings back **after** the native Read ran on the real path:
//!
//! - First read of a file: no output → the host keeps the byte-identical native
//!   result (edit safety: `old_string` always comes from real content, and the
//!   guard has already recorded the real path).
//! - Re-read of the same file, unchanged on disk, same session: the result the
//!   model sees is replaced with a compact `[unchanged]` stub via the documented
//!   `PostToolUse.hookSpecificOutput.updatedToolOutput` channel
//!   (code.claude.com/docs/en/hooks). The file and the guard are untouched — the
//!   tool already ran; only the model-visible copy shrinks.
//!
//! Zero-regression design:
//! - **Shape mirroring**: the incoming `tool_response` is cloned and only the
//!   recognised content-bearing field is swapped; unknown shapes pass through.
//!   (The host additionally ignores schema-mismatched replacements for built-in
//!   tools, keeping the original — a second net under ours.)
//! - **Fail-open everywhere**: any parse/IO/state error → no output → original
//!   result. Emitting nothing is the documented no-op.
//! - Replace only when the stub is **strictly smaller** than the original text.
//! - Windowed reads (`offset`/`limit`) key separately — a different window is a
//!   first read.
//! - No `session_id` → never stub (a cross-session record could otherwise
//!   claim content this conversation has never seen).
//! - Compaction wipes the session's records (see [`purge_session`], wired to the
//!   PreCompact-aware observe handler) so post-compaction re-reads deliver full
//!   content again, mirroring the MCP-side compaction sync (GL #555).

#[allow(clippy::wildcard_imports)]
use super::*;

/// Below this size the savings are negligible and a stub only adds risk.
const MIN_DEDUP_BYTES: usize = 512;

/// Above this size, hashing the file on every Read costs more latency than the
/// dedup is worth in a synchronous PostToolUse hook.
const MAX_HASH_BYTES: u64 = 16 * 1024 * 1024;

/// Session record dirs older than this are swept opportunistically.
const SESSION_TTL: Duration = Duration::from_hours(24);

pub fn handle_read_dedup() {
    if is_disabled() {
        return;
    }
    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        return;
    };
    if let Some(out) = compute_read_dedup(&input) {
        print!("{out}");
    }
}

/// Decide the read-dedup hook's stdout. `None` = emit nothing (passthrough, the
/// documented PostToolUse no-op). Split from [`handle_read_dedup`] for tests.
fn compute_read_dedup(input: &str) -> Option<String> {
    if !crate::core::config::ReadDedup::read_dedup_enabled(&crate::core::config::Config::load()) {
        return None;
    }

    let v: serde_json::Value = serde_json::from_str(input).ok()?;

    // Only successful native Read results are dedupable; other tools have
    // shapes we must not touch (the installer's matcher already scopes to
    // Read, this is the in-process seatbelt).
    if payload::resolve_tool_name(&v).as_deref() != Some("Read") {
        return None;
    }

    let tool_input = payload::resolve_tool_args(&v);
    let (_, path) = payload::resolve_path_field(tool_input.as_ref(), payload::READ_PATH_FIELDS)?;
    let session_id = v.get("session_id").and_then(|s| s.as_str())?.to_string();
    let tool_use_id = v
        .get("tool_use_id")
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_string();

    let tool_response = v.get("tool_response")?;
    let slot = locate_content(tool_response)?;
    let original = slot_text(tool_response, &slot)?;
    if original.len() < MIN_DEDUP_BYTES {
        return None;
    }

    // Key on the exact read window: a different offset/limit is new content.
    let offset = tool_input
        .as_ref()
        .and_then(|ti| ti.get("offset"))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let limit = tool_input
        .as_ref()
        .and_then(|ti| ti.get("limit"))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let disk_hash = hash_file(&path)?;
    let store = record_path(&session_id, &path, offset, limit)?;

    let Ok(prev) = std::fs::read_to_string(&store) else {
        // First read in this session: record it, keep the native result.
        write_record(&store, &disk_hash, &tool_use_id);
        return None;
    };

    let (prev_hash, prev_tool_use) = parse_record(&prev)?;
    // Same tool_use_id = a duplicate fire of the SAME event (Cursor
    // double-fires hooks, #1032). The first fire passed the content
    // through, so its twin must too.
    if prev_hash != disk_hash || (!tool_use_id.is_empty() && prev_tool_use == tool_use_id) {
        write_record(&store, &disk_hash, &tool_use_id);
        return None;
    }

    let line_count = original.lines().count();
    let stub = render_dedup_stub(&path, line_count);
    // Strictly smaller or nothing — the whole point is saving tokens.
    if stub.len() >= original.len() {
        return None;
    }
    let updated = replace_slot(tool_response, &slot, &stub)?;

    // Track dedup savings in stats.json so CEP/dashboard can report them.
    // The hook subprocess is short-lived, so flush immediately.
    let original_tokens = crate::core::tokens::count_tokens(original);
    let stub_tokens = crate::core::tokens::count_tokens(&stub);
    crate::core::stats::record("cli_read_dedup", original_tokens, stub_tokens);
    crate::core::stats::flush();

    debug_log::log_hook_decision(
        "read-dedup",
        "Read",
        Route::LeanCtx,
        &path,
        "re-read of unchanged file → dedup stub",
    );
    Some(
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "updatedToolOutput": updated,
            }
        })
        .to_string(),
    )
}

/// Where the content-bearing text lives inside `tool_response`.
///
/// Claude Code's Read returns the line-numbered text either as a plain string,
/// as `{file: {content: "..."}}`, as `{content: "..."}`, or as an MCP-style
/// content-block array. Anything else is unknown → the caller passes through.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ContentSlot {
    /// `tool_response` is the string itself.
    WholeString,
    /// A nested string field, addressed by object keys (e.g. `["file","content"]`).
    Field(Vec<String>),
    /// `tool_response[idx]` is a `{type:"text", text:"..."}` block.
    TextBlock(usize),
    /// `tool_response.content[idx]` is a `{type:"text", text:"..."}` block.
    ContentTextBlock(usize),
}

fn text_block_index(arr: &[serde_json::Value]) -> Option<usize> {
    arr.iter().position(|b| {
        b.get("type").and_then(|t| t.as_str()) == Some("text")
            && b.get("text").and_then(|t| t.as_str()).is_some()
    })
}

fn locate_content(resp: &serde_json::Value) -> Option<ContentSlot> {
    match resp {
        serde_json::Value::String(_) => Some(ContentSlot::WholeString),
        serde_json::Value::Array(arr) => text_block_index(arr).map(ContentSlot::TextBlock),
        serde_json::Value::Object(obj) => {
            if obj
                .get("file")
                .and_then(|f| f.get("content"))
                .and_then(|c| c.as_str())
                .is_some()
            {
                return Some(ContentSlot::Field(vec![
                    "file".to_string(),
                    "content".to_string(),
                ]));
            }
            match obj.get("content") {
                Some(serde_json::Value::String(_)) => {
                    Some(ContentSlot::Field(vec!["content".to_string()]))
                }
                Some(serde_json::Value::Array(arr)) => {
                    text_block_index(arr).map(ContentSlot::ContentTextBlock)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn slot_text<'a>(resp: &'a serde_json::Value, slot: &ContentSlot) -> Option<&'a str> {
    match slot {
        ContentSlot::WholeString => resp.as_str(),
        ContentSlot::Field(keys) => {
            let mut cur = resp;
            for k in keys {
                cur = cur.get(k)?;
            }
            cur.as_str()
        }
        ContentSlot::TextBlock(i) => resp.get(*i)?.get("text")?.as_str(),
        ContentSlot::ContentTextBlock(i) => resp.get("content")?.get(*i)?.get("text")?.as_str(),
    }
}

/// Clone `resp` and swap ONLY the located content field for `stub` — every other
/// field (paths, line counts, flags) is mirrored verbatim so the replacement
/// matches the tool's output shape (a schema mismatch would make the host drop
/// the replacement; mirroring makes that fallback unnecessary).
fn replace_slot(
    resp: &serde_json::Value,
    slot: &ContentSlot,
    stub: &str,
) -> Option<serde_json::Value> {
    let stub_val = serde_json::Value::String(stub.to_string());
    let mut out = resp.clone();
    match slot {
        ContentSlot::WholeString => Some(stub_val),
        ContentSlot::Field(keys) => {
            let mut cur = &mut out;
            let (last, parents) = keys.split_last()?;
            for k in parents {
                cur = cur.get_mut(k)?;
            }
            *cur.get_mut(last)? = stub_val;
            Some(out)
        }
        ContentSlot::TextBlock(i) => {
            *out.get_mut(*i)?.get_mut("text")? = stub_val;
            Some(out)
        }
        ContentSlot::ContentTextBlock(i) => {
            *out.get_mut("content")?.get_mut(*i)?.get_mut("text")? = stub_val;
            Some(out)
        }
    }
}

/// The replacement body. Deterministic function of (path, line_count) — no
/// timestamps or counters (#498), so identical re-reads stay byte-stable and
/// provider prompt caching applies. Wording mirrors ctx_read's `[unchanged]`
/// stub, adapted for a host whose Read has no `fresh=true` escape: touching the
/// file or editing it changes the hash and the next Read delivers full content.
fn render_dedup_stub(path: &str, line_count: usize) -> String {
    format!(
        "{path} [unchanged {line_count}L · lean-ctx read-dedup]\nUnchanged since your last Read in this session — the full line-numbered content is already in this conversation above. It will be re-delivered automatically once the file changes on disk."
    )
}

fn hash_file(path: &str) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_HASH_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Some(blake3::hash(&bytes).to_hex().to_string())
}

/// `<tmp>/lean-ctx-hook/rd/<session-hash>/<read-key>.rdx`, creating the session
/// dir. Session-scoped so [`purge_session`] and the TTL sweep stay O(1) dirs.
fn record_path(
    session_id: &str,
    path: &str,
    offset: i64,
    limit: i64,
) -> Option<std::path::PathBuf> {
    let root = read_dedup_root()?;
    sweep_stale_sessions(&root);
    let sess = blake3::hash(session_id.as_bytes()).to_hex()[..16].to_string();
    let dir = root.join(sess);
    std::fs::create_dir_all(&dir).ok()?;
    let key = blake3::hash(format!("{path}\u{0}{offset}\u{0}{limit}").as_bytes()).to_hex()[..16]
        .to_string();
    Some(dir.join(format!("{key}.rdx")))
}

fn read_dedup_root() -> Option<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("lean-ctx-hook").join("rd");
    std::fs::create_dir_all(&dir).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Some(dir)
}

/// Record format: `<blake3-hex> <tool_use_id>` on a single line.
fn parse_record(raw: &str) -> Option<(String, String)> {
    let mut parts = raw.trim().splitn(2, ' ');
    let hash = parts.next()?.to_string();
    let tool_use = parts.next().unwrap_or_default().to_string();
    (!hash.is_empty()).then_some((hash, tool_use))
}

fn write_record(store: &std::path::Path, hash: &str, tool_use_id: &str) {
    let _ = std::fs::write(store, format!("{hash} {tool_use_id}"));
}

/// Drop every read record of `session_id` — called when the host compacts its
/// context (PreCompact): the conversation the stub would point into is gone, so
/// the next Read of each file must deliver full content again (GL #555 parity).
pub(super) fn purge_session(session_id: &str) {
    let Some(root) = read_dedup_root() else {
        return;
    };
    let sess = blake3::hash(session_id.as_bytes()).to_hex()[..16].to_string();
    let _ = std::fs::remove_dir_all(root.join(sess));
}

/// Opportunistic TTL sweep of whole session dirs (no background process; runs
/// on the record-write path and touches only our own `rd/` tree).
fn sweep_stale_sessions(root: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > SESSION_TTL);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RAII guard that forces `read_dedup=on` for deterministic tests regardless
    /// of host (Cursor exports its own env vars that would make Auto resolve to
    /// disabled). Restores the environment on drop.
    struct GuardHost;
    impl GuardHost {
        fn claude() -> Self {
            // Force dedup on — avoids depending on host_has_read_before_write_guard()
            // which fails inside Cursor agent shells (CURSOR_* vars present).
            crate::test_env::set_var("LEAN_CTX_READ_DEDUP", "on");
            crate::test_env::set_var("CLAUDE_PROJECT_DIR", "/repo");
            GuardHost
        }
    }
    impl Drop for GuardHost {
        fn drop(&mut self) {
            crate::test_env::remove_var("LEAN_CTX_READ_DEDUP");
            crate::test_env::remove_var("CLAUDE_PROJECT_DIR");
        }
    }

    fn guard_env() -> GuardHost {
        GuardHost::claude()
    }

    fn payload(
        session: &str,
        tool_use: &str,
        path: &std::path::Path,
        response: &serde_json::Value,
    ) -> String {
        serde_json::json!({
            "session_id": session,
            "tool_use_id": tool_use,
            "hook_event_name": "PostToolUse",
            "tool_name": "Read",
            "tool_input": { "file_path": path.to_string_lossy() },
            "tool_response": response,
        })
        .to_string()
    }

    fn unique_session(tag: &str) -> String {
        format!(
            "{tag}-{}-{:?}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            std::thread::current().id()
        )
    }

    fn big_body() -> String {
        "fn main() {}\n".repeat(100)
    }

    #[test]
    fn first_read_passes_through_reread_returns_stub() {
        let _lock = crate::core::data_dir::test_env_lock();
        let _guard = guard_env();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, big_body()).unwrap();
        let session = unique_session("s1");

        let first = compute_read_dedup(&payload(
            &session,
            "toolu_01",
            &file,
            &serde_json::json!(big_body()),
        ));
        assert!(first.is_none(), "first read must keep the native result");

        let second = compute_read_dedup(&payload(
            &session,
            "toolu_02",
            &file,
            &serde_json::json!(big_body()),
        ));
        let out = second.expect("unchanged re-read must be replaced");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let hso = &v["hookSpecificOutput"];
        assert_eq!(hso["hookEventName"], "PostToolUse");
        let replaced = hso["updatedToolOutput"].as_str().expect("string mirrored");
        assert!(
            replaced.contains("[unchanged") && replaced.len() < big_body().len(),
            "stub must be the compact unchanged marker: {replaced}"
        );
    }

    #[test]
    fn changed_file_passes_through_and_rearms() {
        let _lock = crate::core::data_dir::test_env_lock();
        let _guard = guard_env();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, big_body()).unwrap();
        let session = unique_session("s2");

        assert!(
            compute_read_dedup(&payload(
                &session,
                "toolu_01",
                &file,
                &serde_json::json!(big_body())
            ))
            .is_none()
        );

        // File changes on disk → the re-read must deliver the new content.
        std::fs::write(&file, format!("{}\n// changed", big_body())).unwrap();
        assert!(
            compute_read_dedup(&payload(
                &session,
                "toolu_02",
                &file,
                &serde_json::json!(format!("{}\n// changed", big_body()))
            ))
            .is_none(),
            "changed file must pass through"
        );

        // …and the record re-arms on the new hash: the NEXT unchanged re-read stubs.
        assert!(
            compute_read_dedup(&payload(
                &session,
                "toolu_03",
                &file,
                &serde_json::json!(format!("{}\n// changed", big_body()))
            ))
            .is_some(),
            "unchanged re-read after re-arm must stub again"
        );
    }

    #[test]
    fn duplicate_fire_of_same_tool_use_never_stubs() {
        // Cursor double-fires hooks (#1032): the twin of a first read must not
        // be mistaken for a re-read.
        let _lock = crate::core::data_dir::test_env_lock();
        let _guard = guard_env();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, big_body()).unwrap();
        let session = unique_session("s3");

        let p = payload(&session, "toolu_dup", &file, &serde_json::json!(big_body()));
        assert!(compute_read_dedup(&p).is_none());
        assert!(
            compute_read_dedup(&p).is_none(),
            "identical tool_use_id must replay the first fire's passthrough"
        );
    }

    #[test]
    fn different_window_is_a_first_read() {
        let _lock = crate::core::data_dir::test_env_lock();
        let _guard = guard_env();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, big_body()).unwrap();
        let session = unique_session("s4");

        assert!(
            compute_read_dedup(&payload(
                &session,
                "toolu_01",
                &file,
                &serde_json::json!(big_body())
            ))
            .is_none()
        );

        // Same file, but a windowed read → different key → passthrough.
        let windowed = serde_json::json!({
            "session_id": session,
            "tool_use_id": "toolu_02",
            "hook_event_name": "PostToolUse",
            "tool_name": "Read",
            "tool_input": { "file_path": file.to_string_lossy(), "offset": 10, "limit": 50 },
            "tool_response": big_body(),
        })
        .to_string();
        assert!(
            compute_read_dedup(&windowed).is_none(),
            "a windowed read of a fully-read file is new content, never a stub"
        );
    }

    #[test]
    fn object_shape_is_mirrored_with_only_content_swapped() {
        let _lock = crate::core::data_dir::test_env_lock();
        let _guard = guard_env();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, big_body()).unwrap();
        let session = unique_session("s5");

        let resp = serde_json::json!({
            "type": "text",
            "file": {
                "filePath": file.to_string_lossy(),
                "content": big_body(),
                "numLines": 100,
                "startLine": 1,
                "totalLines": 100
            }
        });
        assert!(compute_read_dedup(&payload(&session, "toolu_01", &file, &resp)).is_none());

        let out = compute_read_dedup(&payload(&session, "toolu_02", &file, &resp))
            .expect("object-shaped re-read must stub");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let updated = &v["hookSpecificOutput"]["updatedToolOutput"];
        assert_eq!(updated["type"], "text", "sibling fields mirrored");
        assert_eq!(updated["file"]["numLines"], 100, "metadata mirrored");
        assert_eq!(
            updated["file"]["filePath"],
            file.to_string_lossy().as_ref(),
            "path mirrored"
        );
        assert!(
            updated["file"]["content"]
                .as_str()
                .unwrap()
                .contains("[unchanged"),
            "only the content field is swapped"
        );
    }

    #[test]
    fn unknown_shape_and_non_read_pass_through() {
        let _lock = crate::core::data_dir::test_env_lock();
        let _guard = guard_env();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, big_body()).unwrap();
        let session = unique_session("s6");

        // Unknown response shape (number) → fail-open, no record drama.
        let odd = payload(&session, "toolu_01", &file, &serde_json::json!(42));
        assert!(compute_read_dedup(&odd).is_none());

        // Non-Read tool → never touched, even with a plausible shape.
        let write = serde_json::json!({
            "session_id": session,
            "tool_use_id": "toolu_02",
            "hook_event_name": "PostToolUse",
            "tool_name": "Write",
            "tool_input": { "file_path": file.to_string_lossy() },
            "tool_response": big_body(),
        })
        .to_string();
        assert!(compute_read_dedup(&write).is_none());

        // Missing session_id → never stub (cross-session safety).
        let no_session = serde_json::json!({
            "tool_use_id": "toolu_03",
            "hook_event_name": "PostToolUse",
            "tool_name": "Read",
            "tool_input": { "file_path": file.to_string_lossy() },
            "tool_response": big_body(),
        })
        .to_string();
        assert!(compute_read_dedup(&no_session).is_none());
        assert!(compute_read_dedup(&no_session).is_none());
    }

    #[test]
    fn tiny_files_are_never_stubbed() {
        let _lock = crate::core::data_dir::test_env_lock();
        let _guard = guard_env();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tiny.rs");
        std::fs::write(&file, "fn a() {}\n").unwrap();
        let session = unique_session("s7");

        let p1 = payload(&session, "t1", &file, &serde_json::json!("fn a() {}\n"));
        let p2 = payload(&session, "t2", &file, &serde_json::json!("fn a() {}\n"));
        assert!(compute_read_dedup(&p1).is_none());
        assert!(
            compute_read_dedup(&p2).is_none(),
            "below MIN_DEDUP_BYTES nothing is replaced"
        );
    }

    #[test]
    fn disabled_off_guard_host_stays_passive() {
        let _lock = crate::core::data_dir::test_env_lock();
        // No guard-host marker → auto keeps the hook passive.
        crate::test_env::remove_var("CLAUDE_PROJECT_DIR");
        crate::test_env::remove_var("CLAUDECODE");
        crate::test_env::remove_var("CODEBUDDY");
        crate::test_env::remove_var("LEAN_CTX_READ_DEDUP");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, big_body()).unwrap();
        let session = unique_session("s8");

        for id in ["t1", "t2"] {
            assert!(
                compute_read_dedup(&payload(
                    &session,
                    id,
                    &file,
                    &serde_json::json!(big_body())
                ))
                .is_none(),
                "off guard hosts the PreToolUse redirect owns dedup"
            );
        }
    }

    #[test]
    fn purge_session_resets_reread_state() {
        let _lock = crate::core::data_dir::test_env_lock();
        let _guard = guard_env();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, big_body()).unwrap();
        let session = unique_session("s9");

        assert!(
            compute_read_dedup(&payload(
                &session,
                "t1",
                &file,
                &serde_json::json!(big_body())
            ))
            .is_none()
        );
        // Compaction: the conversation the stub would point into is gone.
        purge_session(&session);
        assert!(
            compute_read_dedup(&payload(
                &session,
                "t2",
                &file,
                &serde_json::json!(big_body())
            ))
            .is_none(),
            "post-compaction re-read must deliver full content (state purged)"
        );
    }

    #[test]
    fn content_block_array_shape_is_supported() {
        let _lock = crate::core::data_dir::test_env_lock();
        let _guard = guard_env();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, big_body()).unwrap();
        let session = unique_session("s10");

        let resp = serde_json::json!([{ "type": "text", "text": big_body() }]);
        assert!(compute_read_dedup(&payload(&session, "t1", &file, &resp)).is_none());
        let out = compute_read_dedup(&payload(&session, "t2", &file, &resp))
            .expect("content-block re-read must stub");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let updated = &v["hookSpecificOutput"]["updatedToolOutput"];
        assert!(
            updated[0]["text"].as_str().unwrap().contains("[unchanged"),
            "text block swapped in place"
        );
        assert_eq!(updated[0]["type"], "text", "block type mirrored");
    }

    #[test]
    fn stub_is_deterministic() {
        // #498: byte-stable output for identical inputs.
        assert_eq!(
            render_dedup_stub("/a/b.rs", 42),
            render_dedup_stub("/a/b.rs", 42)
        );
    }
}
