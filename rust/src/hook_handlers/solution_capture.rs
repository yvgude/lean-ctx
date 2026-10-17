//! Solution Intelligence auto-capture for native edit operations.
//!
//! Hooks into the PostToolUse `observe` pipeline to record solution decisions
//! and LOC metrics for EVERY edit — not just `ctx_edit`/`ctx_patch` MCP calls.
//! This ensures Solution Intelligence works across all IDEs (Cursor, Claude Code,
//! Codex, Copilot, Windsurf, JetBrains) that emit PostToolUse events.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::payload;

/// Number of recent edit hashes to remember for de-duplication.
const DEDUP_CAPACITY: usize = 32;

/// Edit tool names that carry `old_string` + `new_string` replacements.
const EDIT_TOOLS: &[&str] = &[
    "str_replace_editor",
    "edit_file",
    "StrReplace",
    "MultiEdit",
    "str_replace",
    "replace_in_file",
];

/// Tools that create/overwrite files (LOC-only, no old_string available).
const WRITE_TOOLS: &[&str] = &["Write", "write_to_file", "create_file", "write_file"];

static RECENT_EDITS: Mutex<RecentEdits> = Mutex::new(RecentEdits::new());

struct RecentEdits {
    hashes: [u64; DEDUP_CAPACITY],
    cursor: usize,
    count: usize,
}

impl RecentEdits {
    const fn new() -> Self {
        Self {
            hashes: [0; DEDUP_CAPACITY],
            cursor: 0,
            count: 0,
        }
    }

    fn contains(&self, hash: u64) -> bool {
        let len = self.count.min(DEDUP_CAPACITY);
        self.hashes[..len].contains(&hash)
    }

    fn insert(&mut self, hash: u64) {
        self.hashes[self.cursor] = hash;
        self.cursor = (self.cursor + 1) % DEDUP_CAPACITY;
        if self.count < DEDUP_CAPACITY {
            self.count += 1;
        }
    }
}

/// Entry point called from `observe.rs` after `edit_health::maybe_emit`.
/// Detects native edit tool payloads and triggers solution capture + LOC metering.
pub fn maybe_capture(input: &str) {
    let Ok(v) = serde_json::from_str::<Value>(input) else {
        return;
    };

    let Some(tool) = payload::resolve_tool_name(&v) else {
        return;
    };

    // Skip lean-ctx MCP tools — they have their own capture path.
    if tool.starts_with("ctx_") || tool.starts_with("mcp__lean-ctx__") {
        return;
    }

    let Some(args) = payload::resolve_tool_args(&v) else {
        return;
    };

    let root = resolve_root(&v);
    if root.is_empty() {
        return;
    }

    // Codex edits through `apply_patch`; its hook payload carries the raw
    // patch text (`tool_input.command`, a string or `["apply_patch", patch]`).
    // Report every file the patch touches to the edit beacon.
    if is_apply_patch_tool(&tool) {
        let mut texts = Vec::new();
        collect_strings(&args, &mut texts);
        for text in texts {
            crate::core::bounce_tracker::note_host_patch_edits(text, &root);
        }
        return;
    }

    let is_edit = is_edit_tool(&tool);
    if (is_edit || is_write_tool(&tool))
        && let Some((_, path)) = payload::resolve_path_field(Some(&args), payload::READ_PATH_FIELDS)
    {
        // The MCP server's bounce tracker never sees native edits: report the
        // edit so a provisional compressed→full bounce of this file is
        // cancelled as read-before-edit (see `bounce_tracker` edit beacon).
        let absolute = std::path::Path::new(&root).join(&path);
        crate::core::bounce_tracker::note_host_edit(&absolute.to_string_lossy());
    }

    let provenance_config = crate::core::config::Config::load().provenance;
    let capture_provenance = provenance_config.enabled && provenance_config.capture_native_edits;

    if is_edit {
        handle_edit_tool(&v, &args, &tool, &root, capture_provenance);
    } else if is_write_tool(&tool) {
        handle_write_tool(&v, &args, &tool, &root, capture_provenance);
    }
}

/// Records a checkpoint for the commit that just completed.
///
/// Intended for a Git `post-commit` hook invoking `lean-ctx hook post-commit`.
/// The tracker resolves and links the latest session for this project before it
/// persists the checkpoint record.
pub fn handle_post_commit() {
    let config = crate::core::config::Config::load();
    if !config.provenance.enabled || !config.provenance.checkpoint_on_commit {
        return;
    }

    let Ok(root) = std::env::current_dir() else {
        return;
    };
    let Ok(output) = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&root)
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let commit_sha = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if commit_sha.is_empty() {
        return;
    }

    let root = root.to_string_lossy().into_owned();
    let session_id = crate::core::session::SessionState::load_latest_for_project_root(&root)
        .map(|session| session.id)
        .unwrap_or_else(|| "git-hook".to_owned());
    let _ = crate::core::provenance::ProvenanceTracker::new(&root)
        .and_then(|tracker| tracker.observe_commit(&commit_sha, &session_id));
}

fn is_edit_tool(tool: &str) -> bool {
    EDIT_TOOLS.iter().any(|&t| tool.contains(t))
}

fn is_write_tool(tool: &str) -> bool {
    WRITE_TOOLS.iter().any(|&t| tool.contains(t))
}

fn is_apply_patch_tool(tool: &str) -> bool {
    matches!(tool, "apply_patch" | "ApplyPatch")
}

/// Every string inside `value` (patch text may be nested in an array).
fn collect_strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) => out.push(s),
        Value::Array(items) => items.iter().for_each(|v| collect_strings(v, out)),
        Value::Object(map) => map.values().for_each(|v| collect_strings(v, out)),
        _ => {}
    }
}

/// Handle StrReplace-style edits that carry old_string + new_string.
fn handle_edit_tool(v: &Value, args: &Value, tool: &str, root: &str, capture_provenance: bool) {
    let Some((_field, path)) = payload::resolve_path_field(Some(args), payload::READ_PATH_FIELDS)
    else {
        return;
    };

    // Collect all edits (single or MultiEdit array).
    let edits = collect_edits(args);
    if edits.is_empty() {
        return;
    }

    for edit in &edits {
        let hash = compute_edit_hash(&path, &edit.old, &edit.new);

        let mut dedup = RECENT_EDITS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if dedup.contains(hash) {
            continue;
        }
        dedup.insert(hash);
        drop(dedup);

        // Solution decision heuristics
        crate::core::solution_auto_capture::capture_edit_decisions(
            root, &path, &edit.old, &edit.new,
        );

        // LOC metering
        let old_lines = edit.old.lines().count() as u64;
        let new_lines = edit.new.lines().count() as u64;
        let added = new_lines.saturating_sub(old_lines);
        let removed = old_lines.saturating_sub(new_lines);
        if added > 0 || removed > 0 {
            crate::core::edit_metering::record_loc_change(Some(&path), added, removed);
        }

        if capture_provenance {
            observe_native_edit(
                v, root, &path, tool, &edit.old, &edit.new, new_lines, old_lines,
            );
        }
    }

    // Also count via toolResult if available (for Copilot camelCase shape)
    if let Some(result) = v.get("toolResult").and_then(|r| r.get("textResultForLlm")) {
        let _ = result; // Already captured via edits above
    }
}

/// Handle Write/create tools — no old_string, so LOC-only from new_content.
fn handle_write_tool(v: &Value, args: &Value, tool: &str, root: &str, capture_provenance: bool) {
    let content = args
        .get("contents")
        .or_else(|| args.get("content"))
        .or_else(|| args.get("file_text"))
        .and_then(Value::as_str)
        .unwrap_or("");

    if content.is_empty() {
        return;
    }

    let lines = content.lines().count() as u64;
    if lines > 0 {
        crate::core::edit_metering::record_loc_change(None, lines, 0);
        if let Some((_, ref fp)) =
            payload::resolve_path_field(Some(args), payload::READ_PATH_FIELDS)
        {
            crate::core::solution_auto_capture::capture_edit_decisions(root, fp, "", content);
        }
        if let Some((_, file_path)) =
            payload::resolve_path_field(Some(args), payload::READ_PATH_FIELDS)
        {
            if capture_provenance {
                crate::core::savings_ledger::record_edit_event(&file_path, lines, 0);
                observe_native_edit(v, root, &file_path, tool, "", content, lines, 0);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn observe_native_edit(
    event: &Value,
    root: &str,
    path: &str,
    tool: &str,
    before: &str,
    after: &str,
    lines_added: u64,
    lines_removed: u64,
) {
    let tracked_path = Path::new(path)
        .strip_prefix(root)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(|relative| relative.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned());
    let session_id = event
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("native-hook");
    let agent_id = event
        .get("agent_id")
        .or_else(|| event.get("client_name"))
        .and_then(Value::as_str)
        .unwrap_or("native");

    let _ = crate::core::provenance::ProvenanceTracker::new(root).and_then(|tracker| {
        tracker.observe_edit(
            tracked_path,
            tool,
            sha256_hex(before),
            sha256_hex(after),
            lines_added,
            lines_removed,
            session_id,
            agent_id,
        )
    });
}

fn sha256_hex(content: &str) -> String {
    let hash = Sha256::digest(content.as_bytes());
    let mut hex = String::with_capacity(64);
    for b in &hash {
        use std::fmt::Write;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

struct EditReplacement {
    old: String,
    new: String,
}

/// Extract edit replacements from tool args (single or MultiEdit array).
fn collect_edits(args: &Value) -> Vec<EditReplacement> {
    // MultiEdit: `edits` array
    if let Some(arr) = args.get("edits").and_then(Value::as_array) {
        return arr.iter().filter_map(replacement_from).collect();
    }
    // Single replacement
    replacement_from(args).into_iter().collect()
}

fn replacement_from(obj: &Value) -> Option<EditReplacement> {
    let old = obj
        .get("old_string")
        .or_else(|| obj.get("old_str"))
        .and_then(Value::as_str)?;
    let new = obj
        .get("new_string")
        .or_else(|| obj.get("new_str"))
        .or_else(|| obj.get("code_edit"))
        .and_then(Value::as_str)
        .unwrap_or("");
    Some(EditReplacement {
        old: old.to_string(),
        new: new.to_string(),
    })
}

fn compute_edit_hash(path: &str, old: &str, new: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    old.hash(&mut hasher);
    new.hash(&mut hasher);
    hasher.finish()
}

/// Escape raw control characters (\n, \r, \t) inside JSON string values.
/// IDE hooks pipe payloads with literal newlines in `old_string`/`new_string`
/// fields, which `serde_json` rejects as invalid JSON (RFC 8259 §7).
pub(crate) fn sanitize_json_control_chars(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 32);
    let mut in_string = false;
    let mut prev_backslash = false;
    for ch in input.chars() {
        if in_string {
            if prev_backslash {
                prev_backslash = false;
                out.push(ch);
                continue;
            }
            match ch {
                '\\' => {
                    prev_backslash = true;
                    out.push(ch);
                }
                '"' => {
                    in_string = false;
                    out.push(ch);
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c.is_control() => {
                    let _ =
                        std::fmt::Write::write_fmt(&mut out, format_args!("\\u{:04x}", c as u32));
                }
                _ => out.push(ch),
            }
        } else {
            if ch == '"' && !prev_backslash {
                in_string = true;
            }
            prev_backslash = ch == '\\';
            out.push(ch);
        }
    }
    out
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn skips_ctx_tools() {
        let input = json!({
            "tool_name": "ctx_edit",
            "tool_input": { "path": "foo.rs", "old_string": "a", "new_string": "b" },
            "cwd": "/tmp/proj"
        });
        // Should not panic or capture — just returns early
        maybe_capture(&input.to_string());
    }

    #[test]
    fn skips_mcp_lean_ctx_tools() {
        let input = json!({
            "tool_name": "mcp__lean-ctx__ctx_edit",
            "tool_input": { "path": "foo.rs", "old_string": "a", "new_string": "b" },
            "cwd": "/tmp/proj"
        });
        maybe_capture(&input.to_string());
    }

    #[test]
    fn captures_cursor_str_replace() {
        let input = json!({
            "tool_name": "str_replace_editor",
            "tool_input": {
                "path": "/tmp/proj/src/main.rs",
                "old_string": "use serde;",
                "new_string": "use std::collections::HashMap;"
            },
            "cwd": "/tmp/proj"
        });
        maybe_capture(&input.to_string());
    }

    #[test]
    fn captures_claude_edit_file() {
        let input = json!({
            "tool_name": "edit_file",
            "tool_input": {
                "file_path": "/tmp/proj/lib.rs",
                "old_string": "fn old() {}",
                "new_string": "fn new() {}"
            },
            "cwd": "/tmp/proj"
        });
        maybe_capture(&input.to_string());
    }

    #[test]
    fn captures_codex_str_replace() {
        let input = json!({
            "toolName": "str_replace",
            "toolArgs": {
                "path": "/tmp/proj/mod.rs",
                "old_string": "let x = 1;",
                "new_string": "let x = 2;"
            },
            "cwd": "/tmp/proj"
        });
        maybe_capture(&input.to_string());
    }

    #[test]
    fn captures_write_tool_loc_only() {
        let input = json!({
            "tool_name": "Write",
            "tool_input": {
                "path": "/tmp/proj/new.rs",
                "contents": "fn main() {\n    println!(\"hello\");\n}\n"
            },
            "cwd": "/tmp/proj"
        });
        maybe_capture(&input.to_string());
    }

    #[test]
    fn dedup_prevents_double_count() {
        let input = json!({
            "tool_name": "StrReplace",
            "tool_input": {
                "path": "/tmp/proj/dedup_test.rs",
                "old_string": "UNIQUE_DEDUP_TEST_STRING_XYZ",
                "new_string": "UNIQUE_DEDUP_TEST_REPLACEMENT_XYZ"
            },
            "cwd": "/tmp/proj"
        });
        let s = input.to_string();
        maybe_capture(&s);
        maybe_capture(&s);
        // Second call is a no-op due to dedup — no assertion needed, just no panic
    }

    #[test]
    fn no_crash_on_missing_fields() {
        let inputs = [
            json!({}),
            json!({"tool_name": "StrReplace"}),
            json!({"tool_name": "StrReplace", "tool_input": {}}),
            json!({"tool_name": "StrReplace", "tool_input": {"path": "x.rs"}}),
            json!({"random": "data"}),
        ];
        for input in &inputs {
            maybe_capture(&input.to_string());
        }
    }

    #[test]
    fn multi_edit_extracts_all() {
        let edits = collect_edits(&json!({
            "edits": [
                { "old_string": "a", "new_string": "b" },
                { "old_string": "c", "new_string": "d" }
            ]
        }));
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].old, "a");
        assert_eq!(edits[1].old, "c");
    }

    #[test]
    fn apply_patch_payload_strings_are_collected() {
        let args = json!({
            "command": ["apply_patch", "*** Begin Patch\n*** Update File: src/a.rs\n*** End Patch"]
        });
        let mut texts = Vec::new();
        collect_strings(&args, &mut texts);
        let targets: Vec<String> = texts
            .iter()
            .flat_map(|t| crate::core::shell_file_effects::apply_patch_targets(t))
            .collect();
        assert_eq!(targets, ["src/a.rs"]);
        assert!(is_apply_patch_tool("apply_patch"));
        assert!(!is_apply_patch_tool("Bash"));
    }

    #[test]
    fn recent_edits_ring_buffer() {
        let mut ring = RecentEdits::new();
        for i in 0..DEDUP_CAPACITY {
            ring.insert(i as u64 + 100);
        }
        assert!(ring.contains(100));
        assert!(ring.contains(100 + DEDUP_CAPACITY as u64 - 1));
        // Insert one more — evicts the oldest
        ring.insert(999);
        assert!(!ring.contains(100));
        assert!(ring.contains(999));
    }
}
