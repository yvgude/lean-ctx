// SPDX-License-Identifier: Apache-2.0
//! Review r7 of ed1f9d1a6c: every hook caller reads a line for the shell its
//! tool runs it with, never the shell this hook process resolves for itself.
//!
//! A Bash-tool line is POSIX shell under a `pwsh` process too, so the r6/r7
//! payloads stay denied; a PowerShell-tool line is PowerShell under a bash
//! process too, so `$x = Get-Content a.txt` stays allowed.

use super::*;

/// Bash-tool lines bash would run a payload for: r6 F2 (`$VAR` command word)
/// and r7 F2 (a quoted command word inside `( … )`).
const BASH_PAYLOADS: &[&str] = &[
    "$BASH = cargo",
    "echo 'EVIL' | $BASH",
    "cat payload.sh | $SHELL",
    "echo 'EVIL' | ( $BASH )",
    "echo 'EVIL' | ( \"$BASH\" )",
    "echo 'EVIL' | ( \"${BASH}\" )",
    "echo 'EVIL' | ( \"$SHELL\" )",
    "echo 'EVIL' | ( \"sh\" )",
    "echo 'EVIL' | ( 'sh' )",
    "echo 'EVIL' | (\"sh\")",
    "echo 'EVIL' | ( \"/bin/sh\" )",
    "echo 'EVIL' | ( ( \"sh\" ) )",
    "echo 'EVIL' | ( \"$BASH\" ) | cargo --version",
];

const PROCESS_SHELLS: &[&str] = &["/bin/bash", "pwsh"];

fn denies(out: &str) -> bool {
    out.contains("\"deny\"") || out.contains("\"cancel\":true")
}

fn claude_payload(tool: &str, cmd: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": { "command": cmd },
        "cwd": "/tmp",
    })
    .to_string()
}

fn copilot_payload(tool: &str, cmd: &str) -> String {
    serde_json::json!({
        "toolName": tool,
        "toolArgs": serde_json::json!({ "command": cmd }).to_string(),
    })
    .to_string()
}

/// Every hook caller's verdict on one Bash-tool line: `(caller, denied)`.
fn bash_tool_verdicts(cmd: &str) -> Vec<(&'static str, bool)> {
    let bash = claude_payload("Bash", cmd);
    let cline = serde_json::json!({
        "hookName": "PreToolUse",
        "preToolUse": { "toolName": "execute_command", "parameters": { "command": cmd } },
    })
    .to_string();
    let vibe = serde_json::json!({
        "hook_event_name": "pre_tool",
        "tool_name": "bash",
        "tool_input": { "command": cmd },
    });
    let vibe_out = vibe::decide(&vibe, "lean-ctx", crate::hooks::HookMode::Mcp);
    vec![
        (
            "claude rewrite",
            denies(&file_rewrite::compute_rewrite_for(&bash, "lean-ctx")),
        ),
        (
            "governor enforce",
            denies(&enforce_decision_for_test(&bash)),
        ),
        (
            "codex enforce",
            denies(&codex::codex_enforce_decision(&bash, "lean-ctx")),
        ),
        (
            "codex pre-tool",
            denies(&codex::codex_bash_output(cmd, "lean-ctx", || false).0),
        ),
        (
            "kiro execute_bash",
            kiro_cline::kiro_refusal(&claude_payload("execute_bash", cmd), "lean-ctx").is_some(),
        ),
        (
            "cline execute_command",
            denies(&kiro_cline::cline_output(&cline, "lean-ctx")),
        ),
        ("vibe bash", vibe_out.as_deref().is_some_and(denies)),
        (
            "copilot bash",
            copilot_output(&copilot_payload("bash", cmd), "lean-ctx").is_some_and(|o| denies(&o)),
        ),
    ]
}

/// Every hook caller's verdict on one PowerShell-tool line: `(caller, denied)`.
fn powershell_tool_verdicts(cmd: &str) -> Vec<(&'static str, bool)> {
    let claude = claude_payload("PowerShell", cmd);
    vec![
        (
            "claude rewrite",
            denies(&file_rewrite::compute_rewrite_for(&claude, "lean-ctx")),
        ),
        (
            "governor enforce",
            denies(&enforce_decision_for_test(&claude)),
        ),
        (
            "copilot powershell",
            copilot_output(&copilot_payload("powershell", cmd), "lean-ctx")
                .is_some_and(|o| denies(&o)),
        ),
        (
            "powershell allowlist_denial",
            powershell_rewrite::allowlist_denial(cmd, "lean-ctx").is_some(),
        ),
    ]
}

/// Runs `check` under each process shell with an enforced test allowlist and
/// collects the rows whose verdict is not `want_denied`.
fn mismatches(
    lines: &[&str],
    verdicts: fn(&str) -> Vec<(&'static str, bool)>,
    want_denied: bool,
) -> Vec<String> {
    with_test_allowlist(|| {
        lean_ctx::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
        let mut wrong = Vec::new();
        for process in PROCESS_SHELLS {
            lean_ctx::test_env::set_var("LEAN_CTX_SHELL", process);
            for line in lines {
                for (caller, denied) in verdicts(line) {
                    if denied != want_denied {
                        wrong.push(format!("process {process} | {caller} | `{line}`"));
                    }
                }
            }
        }
        lean_ctx::test_env::remove_var("LEAN_CTX_SHELL");
        lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");
        wrong
    })
}

#[test]
fn every_hook_caller_denies_bash_payloads_under_any_process_shell() {
    let wrong = mismatches(BASH_PAYLOADS, bash_tool_verdicts, true);
    assert!(wrong.is_empty(), "allowed:\n{}", wrong.join("\n"));
}

#[test]
fn every_hook_caller_allows_a_literal_bash_command_under_any_process_shell() {
    let wrong = mismatches(
        &["cargo --version", "git status"],
        bash_tool_verdicts,
        false,
    );
    assert!(wrong.is_empty(), "denied:\n{}", wrong.join("\n"));
}

#[test]
fn every_powershell_caller_reads_powershell_under_any_process_shell() {
    let allowed = mismatches(&["$x = Get-Content a.txt"], powershell_tool_verdicts, false);
    assert!(allowed.is_empty(), "denied:\n{}", allowed.join("\n"));
    let denied = mismatches(
        &["iex 'calc'", "Remove-Item x", "$env:PATH = 'x'"],
        powershell_tool_verdicts,
        true,
    );
    assert!(denied.is_empty(), "allowed:\n{}", denied.join("\n"));
}
