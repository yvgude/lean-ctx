use std::path::Path;

use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::core::git::shadow::{self, Checkpoint};
use crate::core::tokens::count_tokens;
use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_int, get_str};
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
            "Local shadow git history of the agent's changes — separate from the user's .git.\n\
WORKFLOW: snapshot before+after changes to capture exactly what was modified.\n\
Actions: snapshot (record current state), log (list checkpoints with SHAs),\n\
diff from=... to=... (compare checkpoints), restore ref=... (revert files).\n\
ANTIPATTERN: Never touches the user's repository — completely isolated shadow history.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["snapshot", "log", "diff", "restore"],
                        "description": "snapshot|log|diff|restore (default: log)"
                    },
                    "message": { "type": "string", "description": "Label for the snapshot (action=snapshot only)" },
                    "from": { "type": "string", "description": "Base checkpoint SHA for diff (default: HEAD~1, action=diff only)" },
                    "to": { "type": "string", "description": "Target checkpoint SHA for diff (default: HEAD, action=diff only)" },
                    "ref": { "type": "string", "description": "Checkpoint SHA to restore (required for action=restore)" },
                    "path": { "type": "string", "description": "File/dir to restore (omit for full restore, action=restore only)" },
                    "limit": { "type": "integer", "description": "Max checkpoints in log (default: 20, max: 200)" }
                },
                "allOf": [
                    {
                        "if": { "properties": { "action": { "const": "restore" } }, "required": ["action"] },
                        "then": { "required": ["action", "ref"] }
                    }
                ]
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
                    .and_then(|d| admitted_diff(&d))
                    .inspect(|_| {
                        crate::core::context_admission::capture::note_source_id(&format!(
                            "checkpoint-diff:{}..{}",
                            from.as_deref().unwrap_or(""),
                            to.as_deref().unwrap_or("")
                        ));
                    })
                    .map(|d| budget(&d, DIFF_MAX_TOKENS))
            }
            "restore" => {
                let git_ref = get_str(args, "ref").ok_or_else(|| {
                    "restore requires 'ref' (a checkpoint sha from log)".to_string()
                })?;
                let path = get_str(args, "path")
                    .map(|raw| {
                        crate::core::workspace_authority::confine(&raw, &ctx.project_root, true)
                    })
                    .transpose()?;
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
                shell_outcome: None,
                content_blocks: None,
            }),
            Err(e) => Err(ErrorData::invalid_params(
                format!("ctx_checkpoint failed: {e}"),
                None,
            )),
        }
    }
}

/// A checkpoint diff is content of project files taken from git objects, not
/// from a rooted file read: it is admitted as a context source — project
/// policy on the text, then the gateway — BEFORE it is cut to the token
/// budget (G01). A diff that is withheld yields the content-free reason as
/// the tool's error. Limit: a diff is an excerpt; a marking that sits in an
/// unchanged part of a file does not travel with its hunks.
fn admitted_diff(diff: &str) -> Result<String, String> {
    if diff.trim().is_empty() {
        return Ok(diff.to_string());
    }
    crate::tools::admitted_sources::admit_remote_text(diff, "ctx_checkpoint")
        .map(|admission| admission.text)
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
    crate::core::recovery::with_original_pointer(
        "ctx_checkpoint",
        "diff",
        content,
        format!("{truncated}\n\n…[diff truncated to ~{max_tokens} tokens]"),
    )
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

    /// G01: a checkpoint diff was returned as git printed it — a key in a
    /// changed line reached the model, and a diff the project policy blocks
    /// was delivered. A clean diff keeps its bytes.
    #[test]
    fn a_diff_is_admitted_before_it_is_budgeted() {
        const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
        let _data = crate::core::data_dir::isolated_data_dir();
        let clean = "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-fn a() {}\n+fn b() {}\n";
        {
            let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
            assert_eq!(admitted_diff(clean).as_deref(), Ok(clean));
            assert_eq!(admitted_diff("").as_deref(), Ok(""));
            let keyed = format!("@@ -1 +1 @@\n+let aws_key = \"{AWS_KEY}\";\n");
            assert!(admitted_diff(&keyed).is_ok_and(|diff| !diff.contains(AWS_KEY)));
        }
        let policy = crate::core::policy::load(
            "name = \"ctx-checkpoint-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
             [redaction]\ncustomer = \"CUS-[0-9]{4}\"\n",
        )
        .unwrap();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        let masked = admitted_diff("@@ -1 +1 @@\n+let owner = \"CUS-1234\";\n").unwrap();
        assert!(!masked.contains("CUS-1234"), "{masked}");
    }
}
