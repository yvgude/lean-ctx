use std::path::Path;

use rmcp::model::Tool;
use rmcp::ErrorData;
use serde_json::{json, Map, Value};

use crate::core::git::shadow::{self, Checkpoint};
use crate::core::tokens::count_tokens;
use crate::server::tool_trait::{get_int, get_str, McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

const DEFAULT_LOG_LIMIT: usize = 20;
const DIFF_MAX_TOKENS: usize = 8000;

/// `ctx_checkpoint` — snapshot, review, diff, and revert the agent's code
/// changes in a shadow git history kept outside the user's own `.git`.
pub struct CtxCheckpointTool;

impl McpTool for CtxCheckpointTool {
    fn name(&self) -> &'static str {
        "ctx_checkpoint"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_checkpoint",
            "Local shadow git history of the agent's changes (separate from the user's .git).\n\
             actions: snapshot (record current state) | log (list checkpoints) | diff (vs a checkpoint) | restore (revert files).\n\
             Snapshot before+after a change to capture exactly what the LLM modified; diff/restore to review or roll back.\n\
             Never touches the user's repository.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["snapshot", "log", "diff", "restore"],
                        "description": "Operation to perform (default: log)"
                    },
                    "message": { "type": "string", "description": "Snapshot label (snapshot)" },
                    "from": { "type": "string", "description": "Base checkpoint sha (diff)" },
                    "to": { "type": "string", "description": "Target checkpoint sha (diff; default: working tree)" },
                    "ref": { "type": "string", "description": "Checkpoint sha to restore from (restore)" },
                    "path": { "type": "string", "description": "Limit restore to this file/dir (restore)" },
                    "limit": { "type": "integer", "description": "Max checkpoints to list (log, default: 20)" }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action").unwrap_or_else(|| "log".to_string());
        let project = Path::new(&ctx.project_root).to_path_buf();
        if ctx.project_root.is_empty() {
            return Err(ErrorData::invalid_params(
                "no project root resolved for ctx_checkpoint",
                None,
            ));
        }

        let result = tokio::task::block_in_place(|| match action.as_str() {
            "snapshot" => {
                let msg = get_str(args, "message").unwrap_or_default();
                shadow::snapshot(&project, &msg).map(|c| render_checkpoint_line("Checkpoint", &c))
            }
            "log" => {
                let limit =
                    get_int(args, "limit").map_or(DEFAULT_LOG_LIMIT, |n| n.clamp(1, 200) as usize);
                shadow::log(&project, limit).map(|cs| render_log(&cs))
            }
            "diff" => {
                let from = get_str(args, "from");
                let to = get_str(args, "to");
                shadow::diff(&project, from.as_deref(), to.as_deref())
                    .map(|d| budget(&d, DIFF_MAX_TOKENS))
            }
            "restore" => {
                let git_ref = get_str(args, "ref").ok_or_else(|| {
                    "restore requires 'ref' (a checkpoint sha from log)".to_string()
                })?;
                let path = get_str(args, "path");
                shadow::restore(&project, &git_ref, path.as_deref())
            }
            other => Err(format!(
                "invalid action '{other}' (use: snapshot, log, diff, restore)"
            )),
        });

        match result {
            Ok(text) => Ok(ToolOutput {
                text,
                original_tokens: 0,
                saved_tokens: 0,
                mode: Some(action),
                path: None,
                changed: matches!(
                    args.get("action").and_then(Value::as_str),
                    Some("snapshot" | "restore")
                ),
            }),
            Err(e) => Err(ErrorData::invalid_params(
                format!("ctx_checkpoint failed: {e}"),
                None,
            )),
        }
    }
}

fn render_checkpoint_line(prefix: &str, c: &Checkpoint) -> String {
    let files = c
        .files_changed
        .map(|n| format!(" · {n} file(s)"))
        .unwrap_or_default();
    format!("{prefix} {}{files} — {}", c.sha, c.message)
}

fn render_log(checkpoints: &[Checkpoint]) -> String {
    if checkpoints.is_empty() {
        return "No checkpoints yet. Run `ctx_checkpoint` with action=snapshot to record one."
            .to_string();
    }
    let mut out = format!("{} checkpoint(s):\n", checkpoints.len());
    for c in checkpoints {
        out.push_str(&format!("- {} · {} — {}\n", c.sha, c.time, c.message));
    }
    out.trim_end().to_string()
}

fn budget(content: &str, max_tokens: usize) -> String {
    if content.trim().is_empty() {
        return "No differences.".to_string();
    }
    let tokens = count_tokens(content);
    if tokens <= max_tokens {
        return content.to_string();
    }
    let ratio = max_tokens as f64 / tokens as f64;
    let keep = ((content.chars().count() as f64 * ratio) as usize).max(1);
    let truncated: String = content.chars().take(keep).collect();
    format!("{truncated}\n\n…[diff truncated to ~{max_tokens} tokens]")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cp(sha: &str, msg: &str, files: Option<usize>) -> Checkpoint {
        Checkpoint {
            sha: sha.to_string(),
            time: "2026-06-07T10:00:00Z".to_string(),
            message: msg.to_string(),
            files_changed: files,
        }
    }

    #[test]
    fn renders_empty_log_with_hint() {
        assert!(render_log(&[]).contains("snapshot"));
    }

    #[test]
    fn renders_checkpoint_line_with_file_count() {
        let line = render_checkpoint_line("Checkpoint", &cp("abc1234", "fix bug", Some(3)));
        assert_eq!(line, "Checkpoint abc1234 · 3 file(s) — fix bug");
    }

    #[test]
    fn renders_log_entries() {
        let out = render_log(&[cp("a1", "one", None), cp("b2", "two", None)]);
        assert!(out.contains("2 checkpoint(s)"));
        assert!(out.contains("- a1 ·"));
        assert!(out.contains("- b2 ·"));
    }

    #[test]
    fn budget_handles_empty_diff() {
        assert_eq!(budget("", 100), "No differences.");
    }
}
