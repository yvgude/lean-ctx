// SPDX-License-Identifier: Apache-2.0
use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ShellOutcome, ToolContext, ToolOutput, get_int, get_str, require_resolved_path,
};
use lean_ctx::tool_defs::tool_def;

pub struct CtxExecuteTool;

impl McpTool for CtxExecuteTool {
    fn name(&self) -> &'static str {
        "ctx_execute"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_execute",
            "Run code in sandbox (11 languages) — use when conditionals, multi-line or cross-language transforms.\n\
             ANTIPATTERN: for simple one-liners, prefer ctx_shell (lower overhead, auto-compressed).\n\
             language=shell supports multi-line scripts but shares ctx_shell's security policy;\n\
             other languages run only after the user approves them.\n\
             action=code (default) for one-shot; action=batch for parallel multi-language;\n\
             action=file to summarize admitted project text as data, without executing it.\n\
             For code/batch, intent annotates large output; file records the intent alongside its preview. Languages: javascript,\n\
             typescript, python, shell, ruby, go, rust, php, perl, r, elixir.",
            json!({
                "type": "object",
                "properties": {
                    "language": {
                        "type": "string",
                        "description": "javascript|typescript|python|shell|ruby|go|rust|php|perl|r|elixir (for action=code)"
                    },
                    "code": {
                        "type": "string",
                        "description": "Source code for action=code. Set intent to filter large output."
                    },
                    "intent": {
                        "type": "string",
                        "description": "Focus intent; triggers filtering when output is large."
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (default: 30)"
                    },
                    "action": {
                        "type": "string",
                        "description": "code (default, run script) | batch (parallel) | file (project file)"
                    },
                    "items": {
                        "type": "string",
                        "description": "JSON array of [{language, code}] for batch action."
                    },
                    "path": {
                        "type": "string",
                        "description": "Project text file for action=file; content is inspected, never executed."
                    }
                },
                "oneOf": [
                    {
                        "properties": { "action": { "enum": ["code"] } },
                        "required": ["language", "code"]
                    },
                    {
                        "properties": { "action": { "const": "batch" } },
                        "required": ["action", "items"]
                    },
                    {
                        "properties": { "action": { "const": "file" } },
                        "required": ["action", "path"]
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
        let action = get_str(args, "action").unwrap_or_default();

        // action=file inspects text and never executes it; code and batch run
        // code, so restricted clients are refused like ctx_shell refuses them.
        if action != "file"
            && let Some(message) = super::ctx_shell::shell_access_denial(ctx)
        {
            return Ok(ToolOutput {
                shell_outcome: Some(crate::server::tool_trait::ShellOutcome::Blocked),
                content_blocks: None,
                ..ToolOutput::simple(message)
            });
        }
        let (result, outcome) = if action == "batch" {
            let items_str = get_str(args, "items")
                .ok_or_else(|| ErrorData::invalid_params("items is required for batch", None))?;
            let items: Vec<serde_json::Value> = serde_json::from_str(&items_str)
                .map_err(|e| ErrorData::invalid_params(format!("Invalid items JSON: {e}"), None))?;
            let batch: Vec<(String, String)> = items
                .iter()
                .filter_map(|item| {
                    let lang = item.get("language")?.as_str()?.to_string();
                    let code = item.get("code")?.as_str()?.to_string();
                    Some((lang, code))
                })
                .collect();
            let rights = batch
                .iter()
                .try_for_each(|(language, code)| session_rights_check(ctx, language, code));
            match rights.and_then(|()| {
                crate::tools::ctx_execute::refuse_unapproved(
                    batch.iter().map(|(language, _)| language.as_str()),
                )
            }) {
                Ok(()) => {
                    crate::tools::ctx_execute::handle_batch_in(&batch, rights_cwd(ctx).as_deref())
                }
                Err(message) => (message, ShellOutcome::Blocked),
            }
        } else if action == "file" {
            let path = require_resolved_path(ctx, args, "path")?;
            let project_root = if ctx.project_root.is_empty() {
                None
            } else {
                Some(ctx.project_root.as_str())
            };
            let intent = get_str(args, "intent");
            crate::tools::ctx_execute::handle_file(&path, intent.as_deref(), project_root)
        } else {
            let language = get_str(args, "language")
                .ok_or_else(|| ErrorData::invalid_params("language is required", None))?;
            let code = get_str(args, "code")
                .ok_or_else(|| ErrorData::invalid_params("code is required", None))?;
            let intent = get_str(args, "intent");
            let timeout = get_int(args, "timeout").and_then(|t| u64::try_from(t).ok());
            match session_rights_check(ctx, &language, &code)
                .and_then(|()| crate::tools::ctx_execute::refuse_unapproved([language.as_str()]))
            {
                Ok(()) => crate::tools::ctx_execute::handle_in(
                    &language,
                    &code,
                    intent.as_deref(),
                    timeout,
                    rights_cwd(ctx).as_deref(),
                ),
                Err(message) => (message, ShellOutcome::Blocked),
            }
        };

        let result = lean_ctx::core::redaction::redact_text_if_enabled(&result);
        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some(action),
            path: None,
            changed: false,
            shell_outcome: Some(outcome),
            content_blocks: None,
        })
    }

    /// `action=file` reads exactly one source, through the admitted rooted
    /// reader on the handler thread (`tools::ctx_execute::handle_file_in_view`)
    /// and derives its summary from that text alone. `code` and `batch` run
    /// arbitrary programs whose reads nothing here observes: not declared.
    fn publishes_every_source(&self, args: Option<&Map<String, Value>>) -> bool {
        args.and_then(|args| args.get("action"))
            .and_then(Value::as_str)
            == Some("file")
    }
}

/// The session's rights profile (process-v1 §9) for code this tool runs:
/// shell code is analysed like a `ctx_shell` command line, other languages
/// are refused under a profile.
fn session_rights_check(ctx: &ToolContext, language: &str, code: &str) -> Result<(), String> {
    lean_ctx::core::session_rights::current().check_code(language, code, &checked_cwd(ctx))
}

/// The directory the rights check judges relative paths against.
fn checked_cwd(ctx: &ToolContext) -> std::path::PathBuf {
    if ctx.project_root.is_empty() {
        std::env::current_dir().unwrap_or_default()
    } else {
        std::path::PathBuf::from(&ctx.project_root)
    }
}

/// Under a profile the code runs where it was checked (the shared daemon's
/// own directory is not the session's); without one, as before.
fn rights_cwd(ctx: &ToolContext) -> Option<std::path::PathBuf> {
    lean_ctx::core::session_rights::current()
        .is_active()
        .then(|| checked_cwd(ctx))
}

#[cfg(test)]
pub(crate) mod session_rights_fixture {
    //! A Developer rights file over a temporary worktree whose
    //! `tests/locked_test.rs` is locked (spec_commit set).

    pub(crate) struct Fixture {
        pub _dir: tempfile::TempDir,
        pub worktree: std::path::PathBuf,
        pub rights: std::path::PathBuf,
    }

    pub(crate) fn developer(denied_tools: &[&str]) -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let worktree = dir.path().join("wt");
        std::fs::create_dir_all(worktree.join("tests")).expect("tests dir");
        std::fs::create_dir_all(worktree.join("specs/42")).expect("spec dir");
        std::fs::write(worktree.join("tests/locked_test.rs"), "#[test] fn t() {}\n").expect("test");
        let list = serde_json::json!({
            "schema": "leanctx.hive.process.feature-list/v1", "process": "1.0",
            "task": {"vault": "#acme/payments", "number": 42},
            "spec_commit": "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d",
            "test_command": "cargo test",
            "locked_paths": ["tests/locked_test.rs", "specs/42/features.json"],
            "features": [{"id": "F1", "description": "d", "steps": ["s"],
                          "tests": ["tests/locked_test.rs::t"], "passes": false}]
        });
        std::fs::write(worktree.join("specs/42/features.json"), list.to_string()).expect("list");
        let record = serde_json::json!({
            "schema": "leanctx.session-rights/v1", "process": "1.0",
            "profile": {"id": "developer", "name": "Developer", "stages": ["build"],
                        "delivers": "commits in its own worktree", "read": ["repo", "vault"],
                        "write": ["own_worktree"], "deny_write": ["locked", "spec", "tests"],
                        "may_stop": false, "availability": "now"},
            "bindings": {"own_worktree": [worktree.to_string_lossy()]},
            "feature_list": "specs/42/features.json",
            "denied_tools": denied_tools,
        });
        let rights = dir.path().join("rights.json");
        std::fs::write(&rights, record.to_string()).expect("rights");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&rights, std::fs::Permissions::from_mode(0o600))
                .expect("0600");
        }
        Fixture {
            _dir: dir,
            worktree,
            rights,
        }
    }

    impl Fixture {
        pub(crate) fn state(&self) -> lean_ctx::core::session_rights::State {
            let state =
                lean_ctx::core::session_rights::from_file_arg(Some(&self.rights.to_string_lossy()));
            assert!(state.is_active(), "{state:?}");
            state
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::tool_trait::ShellOutcome;

    /// F4: ctx_execute runs code under the session rights: shell code that
    /// writes a locked test and code in another language are refused before
    /// anything runs.
    #[test]
    fn session_rights_ctx_execute_refuses_locked_writes_and_other_languages() {
        let fx = super::session_rights_fixture::developer(&[]);
        let ctx = ToolContext {
            project_root: fx.worktree.to_string_lossy().into_owned(),
            ..ToolContext::default()
        };
        lean_ctx::core::session_rights::sync_scope(fx.state(), || {
            for (language, code) in [
                ("shell", "echo x > tests/locked_test.rs"),
                ("bash", "bash -c 'echo x > tests/locked_test.rs'"),
                ("python", "open('tests/locked_test.rs','w').write('x')"),
            ] {
                let output = CtxExecuteTool
                    .handle(&args(&[("language", language), ("code", code)]), &ctx)
                    .expect("answer");
                assert_eq!(
                    output.shell_outcome,
                    Some(ShellOutcome::Blocked),
                    "{language}"
                );
                assert!(output.text.contains("rule "), "{language}: {}", output.text);
            }
            let batch = args(&[
                ("action", "batch"),
                (
                    "items",
                    r#"[{"language":"shell","code":"echo x >> tests/locked_test.rs"}]"#,
                ),
            ]);
            let output = CtxExecuteTool.handle(&batch, &ctx).expect("answer");
            assert_eq!(output.shell_outcome, Some(ShellOutcome::Blocked));
            assert!(session_rights_check(&ctx, "shell", "echo ok > notes.txt").is_ok());
            // R2-4: the code runs in the directory it was checked against,
            // not in the server's own working directory.
            let output = CtxExecuteTool
                .handle(
                    &args(&[("language", "shell"), ("code", "echo ok > made_here.txt")]),
                    &ctx,
                )
                .expect("answer");
            assert_eq!(
                output.shell_outcome,
                Some(ShellOutcome::Exit(0)),
                "{}",
                output.text
            );
        });
        assert!(
            fx.worktree.join("made_here.txt").is_file(),
            "the checked directory is where the code ran"
        );
        assert_eq!(
            std::fs::read_to_string(fx.worktree.join("tests/locked_test.rs")).expect("read"),
            "#[test] fn t() {}\n",
            "the locked test is unchanged"
        );
    }

    fn args(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), Value::String((*v).to_string())))
            .collect()
    }

    /// GHSA-rxv5-4c3x-g4q6 B: clients that may not run ctx_shell may not run
    /// code through ctx_execute either.
    #[test]
    fn restricted_clients_cannot_execute_code() {
        let code = args(&[("language", "python"), ("code", "print(1)")]);
        let batch = args(&[
            ("action", "batch"),
            ("items", r#"[{"language":"python","code":"print(1)"}]"#),
        ]);
        let contexts = [
            ToolContext {
                client_role: Some("untrusted".to_string()),
                ..ToolContext::default()
            },
            ToolContext {
                client_role: Some("readonly".to_string()),
                ..ToolContext::default()
            },
            ToolContext {
                shell_access: Some(false),
                ..ToolContext::default()
            },
        ];
        for ctx in &contexts {
            for request in [&code, &batch] {
                let output = CtxExecuteTool
                    .handle(request, ctx)
                    .expect("denial is a tool result");
                assert_eq!(output.shell_outcome, Some(ShellOutcome::Blocked));
                assert!(
                    output.text.contains("SHELL ACCESS DENIED"),
                    "{}",
                    output.text
                );
            }
        }
    }
}
