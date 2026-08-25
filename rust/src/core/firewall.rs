//! Context firewall: replace large tool outputs with a compact digest + retrieval ref.
//!
//! When ephemeral mode is active (`[archive].ephemeral`, default on), genuinely large
//! tool results are stored out-of-band via [`crate::core::archive`] and only a
//! deterministic digest — a head/tail excerpt, size stats, and `ctx_expand` drilldown
//! instructions — is returned inline. This keeps the agent's context window small while
//! preserving full, slice-addressable access to the raw output.
//!
//! Scope: tool *outputs* (`ctx_shell`, `ctx_execute`, `ctx_search`, `ctx_tree`). Explicit
//! file reads keep their own read-mode system and are never firewalled.

use crate::core::config::Config;
use serde_json::{Map, Value, json};

const HEAD_LINES: usize = 20;
const TAIL_LINES: usize = 8;
const LONG_LINE_HEAD_CHARS: usize = 800;
const LONG_LINE_TAIL_CHARS: usize = 300;
const JSON_PREVIEW_KEYS: usize = 32;

/// Tools whose large outputs are eligible for the firewall. Explicit file reads are
/// intentionally excluded — they have their own read-mode (`lines:`, `signatures`, …).
pub(crate) fn is_firewallable_tool(name: &str) -> bool {
    matches!(
        name,
        "ctx_shell" | "ctx_execute" | "ctx_search" | "ctx_tree"
    )
}

/// Explicit file-read tools whose result *is* the file content the agent reads and
/// edits against. They must always return that content inline — never a head/tail
/// digest (firewall) nor a stored-reference stub (`reference_results`) — regardless
/// of output size or config. This is the single source of truth for "an explicit
/// read always returns content"; both the firewall and the reference-results path
/// honour it so a `ctx_read` can never degrade to a preview the agent can't edit.
pub(crate) fn is_protected_read(name: &str) -> bool {
    matches!(name, "ctx_read" | "ctx_multi_read" | "ctx_smart_read")
}

/// Effective minimum token count before firewalling (config + env override).
pub(crate) fn min_tokens(config: &Config) -> usize {
    config.archive.ephemeral_min_tokens_effective()
}

/// Whether a result of `output_tokens` from `tool` should be firewalled.
pub(crate) fn should_firewall(tool: &str, output_tokens: usize, config: &Config) -> bool {
    config.archive.ephemeral_effective()
        && is_firewallable_tool(tool)
        && output_tokens >= min_tokens(config)
}

/// Programs whose stdout *is* a dataset: rows or JSON the caller already
/// narrowed with `where` / `limit` / `--json` / a filter expression. Head+tail
/// elision does not compress those — it deletes the interior rows, which for
/// sorted output is exactly where the answer lives (#1260). No size threshold
/// makes that safe, so these bypass the firewall entirely.
pub(crate) const DEFAULT_RAW_COMMANDS: &[&str] = &["sqlite3", "psql", "duckdb", "jq"];

/// Context-window guard for *implicitly* verbatim deliveries (#1541): dataset
/// passthrough (#1260) and `inline=true` stay verbatim at any reasonable size,
/// but above `archive.verbatim_max_tokens` a single delivery would flood the
/// calling agent's entire context window. The content is archived losslessly
/// first, so the digest is a compression, never a loss. Explicit `raw`/`bypass`
/// and the `LEAN_CTX_MINIMAL` escape hatch are never capped — callers gate on
/// those before asking. `0` disables the cap.
pub(crate) fn verbatim_cap_exceeded(tool: &str, output_tokens: usize, config: &Config) -> bool {
    let cap = config.archive.verbatim_max_tokens_effective();
    cap > 0 && is_firewallable_tool(tool) && output_tokens >= cap
}

/// Whether `command` runs a dataset program in any of its pipeline segments.
/// `gh` counts only with `--json`/`--jq` — plain `gh` output is prose and
/// compresses fine.
pub(crate) fn is_raw_command(command: &str, config: &Config) -> bool {
    command
        .split(['|', ';', '&', '\n'])
        .any(|seg| match seg.split_whitespace().next() {
            // ponytail: first word per segment, so `FOO=1 sqlite3 …` and
            // `$(sqlite3 …)` are missed — pass raw=true for those.
            Some(word) => {
                let prog = word.rsplit('/').next().unwrap_or(word);
                config.archive.raw_commands.iter().any(|r| r == prog)
                    || (prog == "gh" && (seg.contains("--json") || seg.contains("--jq")))
            }
            None => false,
        })
}

/// Whether an explicitly requested `ctx_shell(inline=true)` result fits the
/// configured verbatim-delivery cap.
pub(crate) fn should_inline_shell(
    inline_requested: bool,
    output_bytes: usize,
    config: &Config,
) -> bool {
    inline_requested && output_bytes <= config.archive.inline_max_bytes_effective()
}

/// Build the inline digest that replaces a firewalled output. Deterministic (no LLM):
/// a head/tail excerpt for multi-line output, or a char-bounded excerpt for output with
/// few but very long lines (e.g. a single giant JSON line), followed by drilldown
/// instructions keyed on `archive_id`.
pub(crate) fn summarize(
    full: &str,
    archive_id: &str,
    tool: &str,
    output_tokens: usize,
    command: &str,
) -> String {
    let chars = full.len();
    let lines: Vec<&str> = full.lines().collect();
    let line_count = lines.len();

    let mut out = String::new();
    out.push_str(&format!(
        "[Firewalled {tool} output — {chars} chars, {output_tokens} tok, {line_count} lines stored out-of-band]\n"
    ));

    if let Some(preview) = json_structure_preview(full) {
        out.push_str("--- JSON structural preview (complete summary; original archived) ---\n");
        out.push_str(&preview);
        out.push('\n');
    } else if line_count > HEAD_LINES + TAIL_LINES + 1 {
        out.push_str("--- head ---\n");
        out.push_str(&lines[..HEAD_LINES].join("\n"));
        out.push_str(&format!(
            "\n--- … {} lines omitted … ---\n",
            line_count - HEAD_LINES - TAIL_LINES
        ));
        out.push_str("--- tail ---\n");
        out.push_str(&lines[line_count - TAIL_LINES..].join("\n"));
        out.push('\n');
    } else {
        // Few lines but large (e.g. one giant minified JSON line): char-bounded excerpt.
        let head_end = full.floor_char_boundary(LONG_LINE_HEAD_CHARS.min(chars));
        out.push_str(&full[..head_end]);
        if chars > LONG_LINE_HEAD_CHARS + LONG_LINE_TAIL_CHARS {
            out.push_str("\n… (truncated) …\n");
            let tail_start = full.floor_char_boundary(chars - LONG_LINE_TAIL_CHARS);
            out.push_str(&full[tail_start..]);
            out.push('\n');
        }
    }

    // Shape info: helps agents understand why line-based chunking may fail.
    let max_line = lines.iter().map(|l| l.len()).max().unwrap_or(0);
    if max_line > 2000 {
        out.push_str(&format!(
            "⚠ longest line: {max_line} chars — line-based offset/limit chunking will not help; use ctx_expand(search=…) or ctx_expand(json_keys=true).\n"
        ));
    }

    // GH #1432: content-aware guidance instead of a blanket "read 100%" mandate.
    out.push_str("--- guidance ---\n");
    if json_structure_preview(full).is_some() {
        out.push_str("Content is JSON. Use targeted extraction (ctx_expand with json_keys or search) rather than reading sequentially.\n");
        if command.contains("--json") && !command.contains("--jq") {
            out.push_str("TIP: re-run the command with a --jq filter to select only the fields you need at the source.\n");
        }
    } else {
        out.push_str("Use ctx_expand(search=\"KEYWORD\") for targeted extraction. Only read the full archive if you genuinely need every line.\n");
    }

    out.push_str("--- retrieve full output ---\n");
    // Non-MCP route first: the verbatim blob is a real file any tool can read,
    // for agents/orgs where MCP is unavailable or forbidden.
    out.push_str(&format!(
        "Direct:  read {} directly (no MCP)\n",
        crate::core::archive::content_path_str(archive_id)
    ));
    out.push_str(&format!("Full:    ctx_expand(id=\"{archive_id}\")\n"));
    out.push_str(&format!(
        "Range:   ctx_expand(id=\"{archive_id}\", start_line=1, end_line=80)\n"
    ));
    out.push_str(&format!(
        "Head:    ctx_expand(id=\"{archive_id}\", head=120)\n"
    ));
    out.push_str(&format!(
        "Search:  ctx_expand(id=\"{archive_id}\", search=\"ERROR\")\n"
    ));
    out.push_str(&format!(
        "JSON:    ctx_expand(id=\"{archive_id}\", json_keys=true)"
    ));
    out
}

fn json_structure_preview(full: &str) -> Option<String> {
    let value: Value = serde_json::from_str(full).ok()?;
    let root = match value {
        Value::Object(object) => {
            let total = object.len();
            let fields = object
                .into_iter()
                .take(JSON_PREVIEW_KEYS)
                .map(|(key, value)| (key, json_value_shape(&value)))
                .collect::<Map<_, _>>();
            json!({
                "type": "object",
                "keys": total,
                "fields": fields,
                "omitted_keys": total.saturating_sub(JSON_PREVIEW_KEYS),
            })
        }
        other => json_value_shape(&other),
    };
    serde_json::to_string(&json!({
        "preview": "structural",
        "root": root,
    }))
    .ok()
}

/// Max chars for string value previews inside structural JSON summaries.
const JSON_STRING_PREVIEW_CHARS: usize = 120;
/// Max array sample elements shown in structural previews.
const JSON_ARRAY_SAMPLE: usize = 3;
/// Max recursion depth for nested value previews.
const JSON_SHAPE_MAX_DEPTH: usize = 2;

fn json_value_shape(value: &Value) -> Value {
    json_value_shape_depth(value, 0)
}

fn json_value_shape_depth(value: &Value, depth: usize) -> Value {
    match value {
        Value::Null => json!({ "type": "null" }),
        Value::Bool(b) => json!({ "type": "boolean", "value": b }),
        Value::Number(n) => json!({ "type": "number", "value": n }),
        Value::String(text) => {
            let char_count = text.chars().count();
            if char_count <= JSON_STRING_PREVIEW_CHARS {
                json!({ "type": "string", "value": text })
            } else {
                let preview: String = text.chars().take(JSON_STRING_PREVIEW_CHARS).collect();
                json!({
                    "type": "string",
                    "chars": char_count,
                    "preview": format!("{preview}…"),
                })
            }
        }
        Value::Array(items) => {
            if depth >= JSON_SHAPE_MAX_DEPTH {
                return json!({ "type": "array", "items": items.len() });
            }
            let sample: Vec<Value> = items
                .iter()
                .take(JSON_ARRAY_SAMPLE)
                .map(|v| json_value_shape_depth(v, depth + 1))
                .collect();
            json!({
                "type": "array",
                "items": items.len(),
                "sample": sample,
            })
        }
        Value::Object(fields) => {
            if depth >= JSON_SHAPE_MAX_DEPTH {
                return json!({ "type": "object", "keys": fields.len() });
            }
            let preview: Map<String, Value> = fields
                .iter()
                .take(8)
                .map(|(k, v)| (k.clone(), json_value_shape_depth(v, depth + 1)))
                .collect();
            json!({
                "type": "object",
                "keys": fields.len(),
                "fields": preview,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firewallable_tools_are_outputs_not_reads() {
        assert!(is_firewallable_tool("ctx_shell"));
        assert!(is_firewallable_tool("ctx_search"));
        assert!(is_firewallable_tool("ctx_tree"));
        assert!(is_firewallable_tool("ctx_execute"));
        assert!(!is_firewallable_tool("ctx_read"));
        assert!(!is_firewallable_tool("ctx_multi_read"));
        assert!(!is_firewallable_tool("ctx_knowledge"));
    }

    #[test]
    fn protected_reads_are_file_readers_and_never_firewallable() {
        // Explicit reads must always return content (no firewall digest, no
        // reference stub) so the agent can edit against the lines.
        for read in ["ctx_read", "ctx_multi_read", "ctx_smart_read"] {
            assert!(is_protected_read(read), "{read} must be a protected read");
            assert!(
                !is_firewallable_tool(read),
                "{read} must never be firewallable"
            );
        }
        assert!(!is_protected_read("ctx_shell"));
        assert!(!is_protected_read("ctx_search"));
    }

    #[test]
    fn should_firewall_respects_tool_and_threshold() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let mut cfg = Config::default();
        cfg.archive.enabled = true;
        cfg.archive.ephemeral = true;
        cfg.archive.ephemeral_min_tokens = 2000;
        // Env can override ephemeral; clear it for a deterministic test.
        crate::test_env::remove_var("LEAN_CTX_EPHEMERAL");
        crate::test_env::remove_var("LEAN_CTX_EPHEMERAL_MIN_TOKENS");

        assert!(should_firewall("ctx_shell", 5000, &cfg));
        assert!(!should_firewall("ctx_shell", 1000, &cfg)); // below threshold
        assert!(!should_firewall("ctx_read", 5000, &cfg)); // not firewallable
    }

    #[test]
    fn dataset_commands_bypass_the_firewall_but_prose_does_not() {
        let cfg = Config::default();
        assert!(is_raw_command(
            "sqlite3 -header backup.db \"select 1\"",
            &cfg
        ));
        assert!(is_raw_command("/usr/bin/psql -c 'select 1'", &cfg));
        assert!(is_raw_command("cat x.json | jq '.[]'", &cfg));
        assert!(is_raw_command("gh issue list --json number,title", &cfg));
        // Plain gh is prose; a mention of a dataset tool is not an invocation.
        assert!(!is_raw_command("gh issue view 1260", &cfg));
        assert!(!is_raw_command("grep -rn sqlite3 src/", &cfg));
        assert!(!is_raw_command("cargo test", &cfg));

        // Opt out.
        let mut off = Config::default();
        off.archive.raw_commands.clear();
        assert!(!is_raw_command("sqlite3 backup.db 'select 1'", &off));
    }

    #[test]
    fn verbatim_cap_fires_only_above_threshold_and_only_for_firewallable_tools() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_VERBATIM_MAX_TOKENS");
        let mut cfg = Config::default();
        cfg.archive.verbatim_max_tokens = 10_000;
        assert!(verbatim_cap_exceeded("ctx_shell", 10_000, &cfg));
        assert!(!verbatim_cap_exceeded("ctx_shell", 9_999, &cfg));
        assert!(!verbatim_cap_exceeded("ctx_read", 50_000, &cfg));

        // 0 disables the cap entirely — nothing is ever blocked.
        cfg.archive.verbatim_max_tokens = 0;
        assert!(!verbatim_cap_exceeded("ctx_shell", 1_000_000, &cfg));
    }

    #[test]
    fn inline_shell_stays_inline_under_byte_cap() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let mut cfg = Config::default();
        cfg.archive.inline_max_bytes = 1024;
        crate::test_env::remove_var("LEAN_CTX_INLINE_MAX_BYTES");

        assert!(should_inline_shell(true, 1024, &cfg));
        assert!(should_inline_shell(true, 0, &cfg));
    }

    #[test]
    fn inline_shell_over_byte_cap_uses_archive_path() {
        // Clearing the env cap only holds if no other test sets it meanwhile.
        let _env_lock = crate::core::data_dir::test_env_lock();
        let mut cfg = Config::default();
        cfg.archive.inline_max_bytes = 1024;
        crate::test_env::remove_var("LEAN_CTX_INLINE_MAX_BYTES");

        assert!(!should_inline_shell(true, 1025, &cfg));
    }

    #[test]
    fn inline_shell_requires_explicit_request_and_honors_env_cap() {
        // Sets the env cap to 2048. Unlocked, that leaks into the sibling tests
        // above, which assert the behaviour with *no* cap set.
        let _env_lock = crate::core::data_dir::test_env_lock();
        let mut cfg = Config::default();
        cfg.archive.inline_max_bytes = 1024;
        crate::test_env::set_var("LEAN_CTX_INLINE_MAX_BYTES", "2048");

        assert!(!should_inline_shell(false, 1, &cfg));
        assert!(should_inline_shell(true, 2048, &cfg));
        assert!(!should_inline_shell(true, 2049, &cfg));

        crate::test_env::remove_var("LEAN_CTX_INLINE_MAX_BYTES");
    }

    #[test]
    fn summarize_includes_excerpt_stats_and_ref() {
        let full = (1..=200)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let digest = summarize(&full, "abc123", "ctx_shell", 1234, "cargo test");
        assert!(digest.contains("Firewalled ctx_shell output"));
        assert!(digest.contains("1234 tok"));
        assert!(digest.contains("line 1")); // head
        assert!(digest.contains("line 200")); // tail
        assert!(digest.contains("lines omitted"));
        assert!(digest.contains("ctx_expand(id=\"abc123\")"));
        assert!(digest.contains("json_keys=true"));
        // The digest must be far smaller than the original.
        assert!(digest.len() < full.len());
    }

    #[test]
    fn summarize_handles_single_giant_line() {
        let full = "x".repeat(5000);
        let digest = summarize(&full, "id9", "ctx_search", 1300, "rg pattern");
        assert!(digest.contains("Firewalled ctx_search output"));
        assert!(digest.contains("truncated"));
        assert!(digest.len() < full.len());
    }

    #[test]
    fn summarize_json_uses_complete_structural_document() {
        let full = serde_json::to_string(&json!({
            "body": "x".repeat(5000),
            "files": [{"path": "src/a.rs"}, {"path": "src/b.rs"}],
            "state": "MERGED",
        }))
        .unwrap();

        let digest = summarize(
            &full,
            "json1",
            "ctx_shell",
            2000,
            "gh issue view --json comments",
        );
        assert!(!digest.contains("… (truncated) …"));
        let preview = digest
            .lines()
            .find(|line| line.starts_with("{\"preview\":"))
            .expect("structural preview JSON");
        let parsed: Value = serde_json::from_str(preview).expect("preview remains valid JSON");
        // #1453: string fields now include a preview/value, not just char count.
        assert_eq!(parsed["root"]["fields"]["body"]["chars"], 5000);
        assert!(
            parsed["root"]["fields"]["body"]["preview"]
                .as_str()
                .unwrap()
                .starts_with("xxx")
        );
        // Short strings get their full value inline.
        assert_eq!(parsed["root"]["fields"]["state"]["value"], "MERGED");
        // Arrays include a sample of element shapes.
        assert_eq!(parsed["root"]["fields"]["files"]["items"], 2);
        assert!(parsed["root"]["fields"]["files"]["sample"].is_array());
        assert_eq!(parsed["root"]["keys"], 3);
        assert!(digest.contains("original archived"));
        assert!(digest.contains("ctx_expand(id=\"json1\", json_keys=true)"));
    }

    #[test]
    fn json_structure_preview_caps_fields_at_valid_boundary() {
        let object = (0..40)
            .map(|index| (format!("key_{index:02}"), json!(index)))
            .collect::<Map<_, _>>();
        let full = serde_json::to_string(&object).unwrap();
        let preview = json_structure_preview(&full).unwrap();
        let parsed: Value = serde_json::from_str(&preview).unwrap();

        assert_eq!(parsed["root"]["fields"].as_object().unwrap().len(), 32);
        assert_eq!(parsed["root"]["omitted_keys"], 8);
        // #1453: number values are now included in the preview.
        let first_field = &parsed["root"]["fields"]["key_00"];
        assert_eq!(first_field["type"], "number");
        assert_eq!(first_field["value"], 0);
    }

    #[test]
    fn json_value_shape_includes_previews_with_depth_limit() {
        let nested = json!({
            "name": "hello world",
            "count": 42,
            "active": true,
            "tags": ["rust", "mcp", "context"],
            "meta": {
                "deep": {
                    "very_deep": "should not recurse here"
                }
            }
        });
        let shape = json_value_shape(&nested);
        // Top-level object gets field previews.
        assert_eq!(shape["type"], "object");
        assert!(shape["fields"].is_object());
        // Short string → full value inline.
        assert_eq!(shape["fields"]["name"]["value"], "hello world");
        // Number → value inline.
        assert_eq!(shape["fields"]["count"]["value"], 42);
        // Boolean → value inline.
        assert_eq!(shape["fields"]["active"]["value"], true);
        // Array → sample with items.
        assert_eq!(shape["fields"]["tags"]["items"], 3);
        assert_eq!(shape["fields"]["tags"]["sample"][0]["value"], "rust");
        // Depth 2 object → no further recursion, just keys count.
        let deep = &shape["fields"]["meta"]["fields"]["deep"];
        assert_eq!(deep["type"], "object");
        assert_eq!(deep["keys"], 1);
        assert!(deep.get("fields").is_none());
    }

    #[test]
    fn json_value_shape_truncates_long_strings() {
        let long_str = "a".repeat(500);
        let val = json!(long_str);
        let shape = json_value_shape(&val);
        assert_eq!(shape["type"], "string");
        assert_eq!(shape["chars"], 500);
        let preview = shape["preview"].as_str().unwrap();
        assert!(preview.ends_with('…'));
        assert!(preview.len() <= JSON_STRING_PREVIEW_CHARS + 4); // +multibyte …
    }
}
