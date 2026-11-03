// SPDX-License-Identifier: Apache-2.0
//! Native-edit code-health notice (#1085).
//!
//! The Phase-4 edit-gate guards lean-ctx's own `ctx_edit`/`ctx_patch`. When an
//! agent edits code with the host's NATIVE Edit/MultiEdit tools the gate is
//! bypassed — this closes that gap. It runs inside the PostToolUse `observe`
//! handler (the only edit-covering hook registered for every host, matcher
//! `.*`) and emits an advisory code-health notice through the model-visible
//! `additionalContext` channel when an edit pushes a function over the
//! navigability threshold.
//!
//! PostToolUse fires AFTER the write, so this is advisory only (it cannot block).
//! The pre-image is reconstructed by reversing the payload's `old_string`→
//! `new_string` diff on the post-edit file — no subprocess (hooks must never
//! spawn children) and no git dependency. Write/create tools are skipped: at
//! PostToolUse their pre-image is gone, so there is no reliable per-edit delta
//! (the background index + session-start block cover those instead).

use super::payload;
use lean_ctx::core::code_health::GateMode;
use lean_ctx::core::code_health::gate::{self, GateOutcome};
use lean_ctx::core::config::Config;
use serde_json::Value;

/// Largest post-edit file we will parse inside a hook. Above this the tree-sitter
/// parse isn't worth the hook's latency budget; the background index covers it.
const MAX_EDIT_BYTES: usize = 1_000_000;

/// A single textual replacement from an edit payload.
struct Replacement {
    old: String,
    new: String,
    replace_all: bool,
}

/// Parse, evaluate, and print a PostToolUse code-health notice for a native edit.
/// No-op for non-edit events, non-source files, or sub-threshold edits.
///
/// #778: By default, notices route to `ctx_knowledge` + dashboard (non-destructive)
/// instead of `additionalContext` stdout (which causes prompt-cache invalidation).
/// Set `[code_health] inject_context = true` to opt into the old stdout behavior.
pub(super) fn maybe_emit(input: &str) {
    let Ok(v) = serde_json::from_str::<Value>(input) else {
        return;
    };
    let root = resolve_root(&v);

    // Guard: detect and auto-restore files corrupted by compression markers.
    // Shadow-mode can compress content that StrReplace/Write then writes back.
    check_and_restore_corrupted(&v, &root);

    if let Some(notice) = edit_health_notice(&v, &root) {
        // Always persist to non-destructive channels (#778)
        persist_notice_to_knowledge(&notice, &v, &root);

        // Only inject additionalContext when explicitly opted in (cache-destructive)
        if inject_context_enabled() {
            emit_post_tool_use_context(&notice);
        }
    }
}

/// Check if `additionalContext` injection is enabled (opt-in, default off).
/// Env `LEAN_CTX_INJECT_CONTEXT=1` force-enables for debugging.
fn inject_context_enabled() -> bool {
    std::env::var("LEAN_CTX_INJECT_CONTEXT").is_ok() || Config::load().code_health.inject_context
}

/// Persist a health notice to `ctx_knowledge` so the agent sees it on the next
/// `ctx_compose` call, and emit a ContextBus event so the dashboard shows the
/// alert in real-time. Fire-and-forget: never blocks the hook, never panics.
fn persist_notice_to_knowledge(notice: &str, payload: &Value, root: &str) {
    // Skip noise paths (IDE caches, Library bundles, terminal artifacts) — these
    // produce meaningless code_health facts from bundled vendor JS.
    let file_check = super::payload::resolve_path_field(
        super::payload::resolve_tool_args(payload).as_ref(),
        super::payload::READ_PATH_FIELDS,
    )
    .map(|(_, p)| p)
    .unwrap_or_default();
    if lean_ctx::core::auto_findings::is_noise_path(&file_check) {
        return;
    }
    let session_id = payload
        .get("session_id")
        .and_then(|s| s.as_str())
        .unwrap_or("unknown");

    let file_path = super::payload::resolve_path_field(
        super::payload::resolve_tool_args(payload).as_ref(),
        super::payload::READ_PATH_FIELDS,
    )
    .map(|(_, p)| p)
    .unwrap_or_default();

    let key = if file_path.is_empty() {
        "edit_regression".to_string()
    } else {
        format!("edit_regression:{file_path}")
    };

    let policy = lean_ctx::core::memory_policy::MemoryPolicy::default();
    let _ = lean_ctx::core::knowledge::ProjectKnowledge::mutate_locked(root, |pk| {
        pk.remember("code_health", &key, notice, session_id, 0.9, &policy);
    });

    // Emit ContextBus event so dashboard + subscribers see it in real-time
    lean_ctx::core::context_os::emit_event(
        root,
        "code_health",
        &lean_ctx::core::context_os::ContextEventKindV1::KnowledgeRemembered,
        Some("edit_health_hook"),
        serde_json::json!({
            "category": "code_health",
            "key": key,
            "notice": notice,
            "file": file_path,
        }),
    );
}

/// Project root for path resolution: the payload `cwd` (every Claude/Cursor hook
/// carries it), falling back to the process working directory.
fn resolve_root(v: &Value) -> String {
    v.get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        })
        .unwrap_or_default()
}

/// The advisory notice for a native edit, or `None` when not applicable. Loads
/// the `[code_health]` config for mode + threshold.
pub(super) fn edit_health_notice(v: &Value, root: &str) -> Option<String> {
    let cfg = Config::load();
    notice_with(
        v,
        root,
        GateMode::parse(&cfg.code_health.gate),
        cfg.code_health.cognitive_threshold,
    )
}

/// Pure-by-injection core: explicit `mode`/`threshold` so it is unit-testable
/// without touching the on-disk config.
fn notice_with(v: &Value, root: &str, mode: GateMode, threshold: u32) -> Option<String> {
    if matches!(mode, GateMode::Off) {
        return None;
    }

    let tool = payload::resolve_tool_name(v)?;
    // ctx_edit / ctx_patch already run the in-tool gate; never double-notice.
    if tool.starts_with("ctx_") || tool.starts_with("mcp__lean-ctx__") {
        return None;
    }

    let args = payload::resolve_tool_args(v)?;
    let (_field, file) = payload::resolve_path_field(Some(&args), payload::READ_PATH_FIELDS)?;
    let edits = collect_edits(&args)?;

    let after = read_jailed(&file, root)?;
    if after.len() > MAX_EDIT_BYTES {
        return None;
    }
    let before = reverse_edits(&after, &edits)?;
    if before == after {
        return None;
    }

    let ext = std::path::Path::new(&file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    match gate::evaluate_with(&before, &after, ext, mode, threshold) {
        GateOutcome::Allow(Some(notice)) | GateOutcome::Block(notice) => Some(notice),
        GateOutcome::Allow(None) => None,
    }
}

/// Extract the edit replacements from a tool-args object. Handles the single-edit
/// shape (`old_string` + `new_string`) and the MultiEdit shape (`edits: [...]`).
/// `None` when the payload carries no recognizable edit (e.g. a Write/create).
fn collect_edits(args: &Value) -> Option<Vec<Replacement>> {
    if let Some(arr) = args.get("edits").and_then(Value::as_array) {
        let edits: Vec<Replacement> = arr.iter().filter_map(replacement_from).collect();
        return (!edits.is_empty()).then_some(edits);
    }
    replacement_from(args).map(|r| vec![r])
}

fn replacement_from(obj: &Value) -> Option<Replacement> {
    let old = obj.get("old_string").and_then(Value::as_str)?.to_string();
    let new = obj.get("new_string").and_then(Value::as_str)?.to_string();
    let replace_all = obj
        .get("replace_all")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Some(Replacement {
        old,
        new,
        replace_all,
    })
}

/// Reconstruct the pre-edit content by reversing each replacement (new→old) on
/// `after`, in reverse application order. Returns `None` when a replacement
/// cannot be reversed reliably — a deletion (`new_string` empty) whose position
/// is unknown, or a `new_string` no longer present (the host normalized it) — so
/// an unreliable delta never produces a false notice.
fn reverse_edits(after: &str, edits: &[Replacement]) -> Option<String> {
    let mut content = after.to_string();
    for e in edits.iter().rev() {
        if e.new.is_empty() {
            return None; // pure deletion: original position is unrecoverable.
        }
        if !content.contains(&e.new) {
            return None; // can't locate the inserted text → bail, don't guess.
        }
        content = if e.replace_all {
            content.replace(&e.new, &e.old)
        } else {
            content.replacen(&e.new, &e.old, 1)
        };
    }
    Some(content)
}

/// Read `file` (resolved against `root`) only if it stays inside the project
/// jail. Returns `None` on any path/IO/jail failure (best-effort hook).
fn read_jailed(file: &str, root: &str) -> Option<String> {
    let p = std::path::Path::new(file);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::path::Path::new(root).join(file)
    };
    lean_ctx::core::pathjail::jail_path(&abs, std::path::Path::new(root)).ok()?;
    std::fs::read_to_string(&abs).ok()
}

/// Emit a PostToolUse notice on the model-visible `additionalContext` channel
/// (honored by Claude Code / Codex; ignored harmlessly by other hosts).
fn emit_post_tool_use_context(notice: &str) {
    let payload = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": notice,
        }
    });
    println!("{payload}");
}

/// Compression markers that indicate a file was corrupted by shadow-mode
/// content being written back to disk by an edit tool.
const CORRUPTION_MARKERS: &[&str] = &[
    "§ function",
    "§ block",
    "§ impl",
    "§ struct",
    "§ enum",
    "§ trait",
    "§ mod",
    "[lean-ctx: omitted",
    "… truncated at ~",
    "use ctx_read with lines= parameter to see specific sections]",
];

/// After a native edit, detect lean-ctx compression markers and report them.
///
/// Hooks must be bounded and non-destructive: invoking git from a PostToolUse
/// hook can outlive the host deadline, and restoring HEAD would discard a
/// user's just-written change. The normal edit path remains responsible for
/// safe recovery; this hook only emits the actionable signal.
fn check_and_restore_corrupted(v: &Value, root: &str) {
    let Some(args) = payload::resolve_tool_args(v) else {
        return;
    };
    let Some((_field, file)) = payload::resolve_path_field(Some(&args), payload::READ_PATH_FIELDS)
    else {
        return;
    };
    let Some(contents) = read_jailed(&file, root) else {
        return;
    };

    let has_marker = CORRUPTION_MARKERS.iter().any(|m| contents.contains(m));
    if !has_marker {
        return;
    }

    let _ = root;
    eprintln!(
        "[lean-ctx] CORRUPTION DETECTED in {file}: compression markers found after edit; file left unchanged."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FLAT: &str = "fn f(a: bool) { if a {} }";
    // 1+2+3+4+5+6 = 21 cognitive → over the default threshold of 15.
    const DEEP: &str = "fn f(a: bool) { if a { if a { if a { if a { if a { if a {} } } } } } }";

    #[test]
    fn reverse_single_edit_reconstructs_before() {
        let after = format!("{DEEP}\n");
        let edits = vec![Replacement {
            old: FLAT.into(),
            new: DEEP.into(),
            replace_all: false,
        }];
        assert_eq!(reverse_edits(&after, &edits).unwrap(), format!("{FLAT}\n"));
    }

    #[test]
    fn reverse_insertion_removes_new_text() {
        // old empty (pure insertion): reversing removes the inserted text.
        let edits = vec![Replacement {
            old: String::new(),
            new: "fn extra() {}\n".into(),
            replace_all: false,
        }];
        let after = "fn extra() {}\nfn keep() {}\n";
        assert_eq!(reverse_edits(after, &edits).unwrap(), "fn keep() {}\n");
    }

    #[test]
    fn reverse_deletion_bails() {
        // new empty (deletion): original position is unrecoverable → None.
        let edits = vec![Replacement {
            old: "fn gone() {}\n".into(),
            new: String::new(),
            replace_all: false,
        }];
        assert!(reverse_edits("fn keep() {}\n", &edits).is_none());
    }

    #[test]
    fn reverse_missing_new_text_bails() {
        let edits = vec![Replacement {
            old: "a".into(),
            new: "NOT_PRESENT".into(),
            replace_all: false,
        }];
        assert!(reverse_edits("some other content", &edits).is_none());
    }

    #[test]
    fn collect_edits_single_and_multi() {
        let single = json!({ "old_string": "a", "new_string": "b" });
        assert_eq!(collect_edits(&single).unwrap().len(), 1);

        let multi = json!({ "edits": [
            { "old_string": "a", "new_string": "b" },
            { "old_string": "c", "new_string": "d", "replace_all": true },
        ]});
        let edits = collect_edits(&multi).unwrap();
        assert_eq!(edits.len(), 2);
        assert!(edits[1].replace_all);

        // A Write payload (content only, no old/new) is not an edit.
        let write = json!({ "content": "whole file" });
        assert!(collect_edits(&write).is_none());
    }

    #[test]
    fn notice_for_native_edit_that_regresses() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("f.rs"), format!("{DEEP}\n")).unwrap();

        let v = json!({
            "tool_name": "Edit",
            "cwd": root,
            "tool_input": {
                "file_path": "f.rs",
                "old_string": FLAT,
                "new_string": DEEP,
            }
        });
        let notice = notice_with(&v, root, GateMode::Warn, 15).expect("notice");
        assert!(notice.contains("[CODE HEALTH]"));
    }

    #[test]
    fn no_notice_for_ctx_edit_tool() {
        // ctx_edit goes through the in-tool gate; the hook must not double-notice.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("f.rs"), format!("{DEEP}\n")).unwrap();
        let v = json!({
            "tool_name": "ctx_edit",
            "cwd": root,
            "tool_input": { "file_path": "f.rs", "old_string": FLAT, "new_string": DEEP }
        });
        assert!(notice_with(&v, root, GateMode::Warn, 15).is_none());
    }

    #[test]
    fn no_notice_in_off_mode() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("f.rs"), format!("{DEEP}\n")).unwrap();
        let v = json!({
            "tool_name": "Edit",
            "cwd": root,
            "tool_input": { "file_path": "f.rs", "old_string": FLAT, "new_string": DEEP }
        });
        assert!(notice_with(&v, root, GateMode::Off, 15).is_none());
    }

    #[test]
    fn no_notice_for_non_edit_event() {
        let v = json!({ "tool_name": "Read", "tool_input": { "file_path": "f.rs" } });
        assert!(notice_with(&v, "/tmp", GateMode::Warn, 15).is_none());
    }

    #[test]
    fn inject_context_disabled_by_default() {
        // #778: with default config, inject_context is false → no stdout emission
        let _lock = lean_ctx::core::data_dir::test_env_lock();
        lean_ctx::test_env::remove_var("LEAN_CTX_INJECT_CONTEXT");
        assert!(!inject_context_enabled());
    }

    #[test]
    fn inject_context_enabled_via_env() {
        // #778: env override forces injection (for debugging/opt-in)
        let _lock = lean_ctx::core::data_dir::test_env_lock();
        lean_ctx::test_env::set_var("LEAN_CTX_INJECT_CONTEXT", "1");
        assert!(inject_context_enabled());
        lean_ctx::test_env::remove_var("LEAN_CTX_INJECT_CONTEXT");
    }
}
