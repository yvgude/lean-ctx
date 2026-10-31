// SPDX-License-Identifier: Apache-2.0

//! The closed registry of feature codes for telemetry (3.11.1+).
//!
//! A code names *what* LeanCTX did, never with what: `cli.pack.export`,
//! `index.graph`. CLI arguments are matched against the fixed lists below and
//! only the matching literal is counted, so a path, file name, query or any
//! other user input can never become a code. Commands on the hot path of
//! every shell command or editor event are not counted at all; the daily
//! usage record already covers that work.

/// Top-level commands counted as `cli.<command>`. Aliases map to the first
/// spelling the dispatcher accepts.
const COMMANDS: &[(&str, &[&str])] = &[
    ("badge", &[]),
    ("shell", &["--shell"]),
    ("gain", &[]),
    ("spend", &[]),
    ("savings", &[]),
    ("learning", &[]),
    ("autopilot", &[]),
    ("conformance", &["selftest"]),
    ("health", &[]),
    ("billing", &[]),
    ("finops", &[]),
    ("roi", &[]),
    ("output_savings", &["output-savings"]),
    ("value_report", &["value-report"]),
    ("evidence_export", &["evidence-export"]),
    ("evidence", &[]),
    ("shadow", &[]),
    ("triage", &[]),
    ("scenario", &[]),
    ("token_report", &["token-report", "report-tokens"]),
    ("pack", &[]),
    ("migrate", &[]),
    ("policy", &[]),
    ("addon", &["addons"]),
    ("plugin", &["plugins"]),
    ("embeddings", &[]),
    ("model", &[]),
    ("gpu", &["enable-gpu"]),
    ("rules", &[]),
    ("proof", &[]),
    ("value", &[]),
    ("claude_mod", &["claude-mod"]),
    ("inspect", &[]),
    ("snapshot", &[]),
    ("verify", &[]),
    ("verify_cache", &["verify-cache", "cache-selftest"]),
    ("visualize", &[]),
    ("audit", &[]),
    ("compliance", &[]),
    ("agent", &[]),
    ("instructions", &[]),
    ("index", &[]),
    ("semantic_search", &["semantic-search", "search-code"]),
    ("explore", &[]),
    ("repomap", &["repo-map"]),
    ("cep", &[]),
    ("demo", &[]),
    // `cockpit` is the Cockpit's name since 3.12; it stays one feature with
    // its `dashboard` alias under the existing `cli.dashboard` code.
    ("dashboard", &["cockpit"]),
    ("team", &[]),
    ("provider", &[]),
    ("serve", &[]),
    ("watch", &[]),
    ("proxy", &[]),
    ("daemon", &[]),
    ("init", &[]),
    ("setup", &[]),
    ("onboard", &[]),
    ("install", &[]),
    ("bootstrap", &[]),
    ("wrap", &[]),
    ("unwrap", &[]),
    ("status", &[]),
    ("cognitive", &[]),
    ("deps", &[]),
    ("discover", &[]),
    ("engine", &[]),
    ("ghost", &[]),
    ("filter", &[]),
    ("heatmap", &[]),
    ("graph", &[]),
    ("smells", &[]),
    ("session", &[]),
    ("ledger", &[]),
    ("ocla", &[]),
    ("control", &["context-control"]),
    ("plan", &["context-plan"]),
    ("compile", &["context-compile"]),
    ("import", &[]),
    ("checkpoints", &[]),
    ("knowledge", &[]),
    ("kit", &["kits"]),
    ("skillify", &[]),
    ("summary", &[]),
    ("overview", &[]),
    ("compress", &[]),
    ("wrapped", &[]),
    ("sessions", &["session-store"]),
    ("benchmark", &[]),
    ("benchmark_run", &["benchmark-run", "bench-run"]),
    ("calibrate", &[]),
    ("compact", &[]),
    ("profile", &[]),
    ("tools", &[]),
    ("config", &[]),
    ("allow", &[]),
    ("allow_path", &["allow-path"]),
    ("security", &[]),
    ("yolo", &[]),
    ("secure", &["lockdown"]),
    ("trust", &[]),
    ("untrust", &[]),
    ("stats", &[]),
    ("introspect", &[]),
    ("cache", &[]),
    ("theme", &[]),
    ("enterprise", &[]),
    ("tee", &[]),
    ("terse", &["compression"]),
    ("slow_log", &["slow-log"]),
    ("debug_log", &["debug-log"]),
    ("editor_bridge", &["editor-bridge"]),
    ("update", &["--self-update"]),
    ("restart", &[]),
    ("stop", &[]),
    ("doctor", &[]),
    ("harden", &[]),
    ("export_rules", &["export-rules"]),
    ("gotchas", &["bugs"]),
    ("learn", &[]),
    ("report_issue", &["report-issue", "report"]),
    ("uninstall", &[]),
    ("safety_levels", &["safety-levels", "safety"]),
    ("cheat", &["cheatsheet", "cheat-sheet"]),
    ("login", &[]),
    ("register", &[]),
    ("sync", &[]),
    ("contribute", &[]),
    ("telemetry", &[]),
    ("cloud", &[]),
    ("upgrade", &[]),
];

/// Second words counted as `cli.<command>.<verb>` when they follow a counted
/// command. Product verbs only; anything else is counted as the bare command.
const VERBS: &[&str] = &[
    "add",
    "apply",
    "auto-load",
    "build",
    "build-full",
    "build-semantic",
    "check",
    "checkpoint-inspect",
    "checkpoint-seal",
    "clean",
    "clear",
    "create",
    "delete",
    "delete-remote",
    "diff",
    "disable",
    "doctor",
    "enable",
    "explain",
    "export",
    "export-html",
    "gc",
    "get",
    "impact",
    "import",
    "info",
    "init",
    "inspect",
    "install",
    "list",
    "login",
    "logout",
    "ls",
    "migrate",
    "neighbors",
    "off",
    "on",
    "parity",
    "path",
    "prune",
    "publish",
    "pull",
    "push",
    "rebuild",
    "receive",
    "related",
    "remove",
    "reset",
    "restart",
    "rm",
    "rollback",
    "run",
    "search",
    "send",
    "set",
    "show",
    "snapshot",
    "start",
    "status",
    "stop",
    "symbol",
    "uninstall",
    "update",
    "verify",
    "warm",
    "watch",
];

/// Commands that run on every shell command, editor event or prompt render.
/// Counting them would add a locked file write to the hot path.
const NOT_COUNTED: &[&str] = &[
    "-c",
    "exec",
    "-t",
    "--track",
    "hook",
    "__complete",
    "completions",
    "mcp",
    "read",
    "call",
    "diff",
    "grep",
    "glob",
    "find",
    "ls",
    "raw",
    "bypass",
    "statusline",
    "editor-signal",
    "editor-session",
    "prompt-segment",
    "git-trailer",
    "help",
    "--help",
    "-h",
    "codex-protected",
    "dev-install",
    "codesign-setup",
    "forgot-password",
];

fn segment(word: &str) -> String {
    word.trim_start_matches('-').replace('-', "_")
}

/// The feature code for a CLI invocation, or `None` when it is not counted.
/// A `--dry-run` promises to write nothing, the usage counter included.
#[must_use]
pub fn cli_code(args: &[String]) -> Option<String> {
    let first = args.get(1)?.as_str();
    if NOT_COUNTED.contains(&first) || args.iter().skip(2).any(|a| a == "--dry-run") {
        return None;
    }
    let (command, _) = COMMANDS.iter().find(|(name, aliases)| {
        *name == first || segment(first) == *name || aliases.contains(&first)
    })?;
    let verb = args
        .get(2)
        .map(String::as_str)
        .filter(|word| VERBS.contains(word))
        .map(segment);
    let code = match verb {
        Some(verb) => format!("cli.{command}.{verb}"),
        None => format!("cli.{command}"),
    };
    crate::core::telemetry_v2::valid_feature_code(&code).then_some(code)
}

/// Counts a CLI invocation. Never fails the command.
pub fn record_cli(args: &[String]) {
    if let Some(code) = cli_code(args) {
        let _ = crate::core::telemetry_aggregate::record_feature(&code, true);
    }
}

/// Background features with a fixed code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    GraphIndex,
    Bm25Index,
    SemanticIndex,
}

impl Feature {
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::GraphIndex => "index.graph",
            Self::Bm25Index => "index.bm25",
            Self::SemanticIndex => "index.semantic",
        }
    }
}

/// Counts one run of a background feature and whether it succeeded.
pub fn record(feature: Feature, ok: bool) {
    let _ = crate::core::telemetry_aggregate::record_feature(feature.code(), ok);
}

/// An MCP server that completed the client handshake.
const MCP_SESSION: &str = "mcp.session";
/// One that then ended without answering a single tool call: a client that
/// starts every configured server (often in a throwaway container) and whose
/// agent never uses LeanCTX. Counted, not hidden, so Ops can tell such starts
/// from real sessions.
const MCP_SESSION_EMPTY: &str = "mcp.session.empty";
/// Every other ended session gets exactly one of three outcomes (3.11.5+):
/// no tool call failed;
const MCP_SESSION_CLEAN: &str = "mcp.session.clean";
/// calls failed, but the last call of every tool succeeded;
const MCP_SESSION_RECOVERED: &str = "mcp.session.recovered";
/// the last call of some tool failed. Also counted as a failure when one of
/// those last failures lies on LeanCTX's side rather than the caller's.
const MCP_SESSION_UNRESOLVED: &str = "mcp.session.unresolved";
/// Calls made right after a failed call of the same tool; a failure when the
/// retry failed again.
const MCP_SESSION_RETRY: &str = "mcp.session.retry";

/// Counts an MCP server start after the handshake.
pub fn record_mcp_session_start() {
    let _ = crate::core::telemetry_aggregate::record_feature(MCP_SESSION, true);
}

/// A failure LeanCTX itself has to fix: a timeout, an unavailable
/// dependency, a size limit or an unclassified error. Bad input, a missing
/// file, a stale edit, a permission or a deliberate policy block are the
/// caller's to resolve.
fn leanctx_side(kind: crate::core::telemetry_failure::FailureKind) -> bool {
    use crate::core::telemetry_failure::FailureKind;
    matches!(
        kind,
        FailureKind::Timeout
            | FailureKind::Unavailable
            | FailureKind::TooLarge
            | FailureKind::Other
    )
}

/// The `(code, count, failures)` tallies an ending MCP server adds.
fn mcp_session_end_tallies(
    outcome: &crate::core::telemetry::SessionOutcome,
) -> Vec<(&'static str, u64, u64)> {
    if outcome.calls == 0 {
        return vec![(MCP_SESSION_EMPTY, 1, 0)];
    }
    let ending = if outcome.failures == 0 {
        (MCP_SESSION_CLEAN, 1, 0)
    } else if outcome.unresolved.is_empty() {
        (MCP_SESSION_RECOVERED, 1, 0)
    } else {
        let ours = outcome.unresolved.iter().any(|kind| leanctx_side(*kind));
        (MCP_SESSION_UNRESOLVED, 1, u64::from(ours))
    };
    vec![
        ending,
        (MCP_SESSION_RETRY, outcome.retries, outcome.retry_failures),
    ]
}

/// Counts how an MCP session ended. Every exit path (transport closed, parent
/// gone, idle exit) calls this before its final send; only the first call of
/// a process counts.
pub fn record_mcp_session_end() {
    static ENDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if ENDED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let outcome = crate::core::telemetry::global_metrics().session_outcome();
    let _ =
        crate::core::telemetry_aggregate::record_feature_counts(&mcp_session_end_tallies(&outcome));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        std::iter::once("lean-ctx")
            .chain(words.iter().copied())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn dry_runs_are_not_counted() {
        for words in [
            &["setup", "--dry-run"][..],
            &["onboard", "--agents", "cursor", "--dry-run"][..],
            &["update", "--dry-run"][..],
            &["uninstall", "--dry-run"][..],
        ] {
            assert_eq!(cli_code(&args(words)), None, "{words:?}");
        }
        assert_eq!(cli_code(&args(&["setup"])).as_deref(), Some("cli.setup"));
    }

    #[test]
    fn commands_and_registry_verbs_become_codes() {
        assert_eq!(
            cli_code(&args(&["pack", "export", "my-secret-pack"])).as_deref(),
            Some("cli.pack.export")
        );
        assert_eq!(
            cli_code(&args(&["pack", "import", "/home/a/x.ctxpkg"])).as_deref(),
            Some("cli.pack.import")
        );
        assert_eq!(
            cli_code(&args(&["graph", "build"])).as_deref(),
            Some("cli.graph.build")
        );
        assert_eq!(
            cli_code(&args(&["index", "build-full"])).as_deref(),
            Some("cli.index.build_full")
        );
        assert_eq!(
            cli_code(&args(&["report-tokens"])).as_deref(),
            Some("cli.token_report")
        );
        assert_eq!(
            cli_code(&args(&["--self-update"])).as_deref(),
            Some("cli.update")
        );
        assert_eq!(
            cli_code(&args(&["cockpit"])).as_deref(),
            Some("cli.dashboard")
        );
        assert_eq!(
            cli_code(&args(&["telemetry", "off"])).as_deref(),
            Some("cli.telemetry.off")
        );
    }

    #[test]
    fn user_input_never_becomes_part_of_a_code() {
        assert_eq!(
            cli_code(&args(&["knowledge", "remember", "the db password"])).as_deref(),
            Some("cli.knowledge")
        );
        assert_eq!(
            cli_code(&args(&["graph", "/Users/a/private-repo"])).as_deref(),
            Some("cli.graph")
        );
        assert_eq!(
            cli_code(&args(&["semantic-search", "export"])).as_deref(),
            Some("cli.semantic_search.export")
        );
        assert!(cli_code(&args(&["my-custom-thing"])).is_none());
        assert!(cli_code(&args(&[])).is_none());
    }

    #[test]
    fn hot_path_commands_are_not_counted() {
        for command in [
            "-c",
            "exec",
            "hook",
            "read",
            "grep",
            "statusline",
            "mcp",
            "__complete",
        ] {
            assert!(cli_code(&args(&[command, "build"])).is_none(), "{command}");
        }
    }

    #[test]
    fn every_registry_code_is_a_valid_wire_code() {
        for (command, aliases) in COMMANDS {
            for spelling in std::iter::once(*command).chain(aliases.iter().copied()) {
                let code = cli_code(&args(&[spelling])).unwrap_or_else(|| panic!("{spelling}"));
                assert_eq!(code, format!("cli.{command}"));
                for verb in VERBS {
                    assert!(
                        cli_code(&args(&[spelling, verb])).is_some(),
                        "{spelling} {verb}"
                    );
                }
            }
        }
        for feature in [
            Feature::GraphIndex,
            Feature::Bm25Index,
            Feature::SemanticIndex,
        ] {
            assert!(crate::core::telemetry_v2::valid_feature_code(
                feature.code()
            ));
        }
        for code in [
            MCP_SESSION,
            MCP_SESSION_EMPTY,
            MCP_SESSION_CLEAN,
            MCP_SESSION_RECOVERED,
            MCP_SESSION_UNRESOLVED,
            MCP_SESSION_RETRY,
        ] {
            assert!(
                crate::core::telemetry_v2::valid_feature_code(code),
                "{code}"
            );
        }
    }

    #[test]
    fn every_ended_session_gets_exactly_one_outcome() {
        use crate::core::telemetry::SessionOutcome;
        use crate::core::telemetry_failure::FailureKind;
        let outcome = |calls, failures, unresolved: Vec<FailureKind>| SessionOutcome {
            calls,
            failures,
            unresolved,
            retries: 0,
            retry_failures: 0,
        };
        assert_eq!(
            mcp_session_end_tallies(&outcome(0, 0, vec![])),
            [("mcp.session.empty", 1, 0)]
        );
        assert_eq!(
            mcp_session_end_tallies(&outcome(4, 0, vec![]))[0],
            ("mcp.session.clean", 1, 0)
        );
        assert_eq!(
            mcp_session_end_tallies(&outcome(4, 2, vec![]))[0],
            ("mcp.session.recovered", 1, 0)
        );
        assert_eq!(
            mcp_session_end_tallies(&outcome(4, 2, vec![FailureKind::NotFound]))[0],
            ("mcp.session.unresolved", 1, 0),
            "a missing file is the caller's to resolve"
        );
        assert_eq!(
            mcp_session_end_tallies(&outcome(
                4,
                2,
                vec![FailureKind::NotFound, FailureKind::Timeout]
            ))[0],
            ("mcp.session.unresolved", 1, 1),
            "a timeout is LeanCTX's"
        );
    }

    #[test]
    fn retries_ride_along_with_the_outcome() {
        let outcome = crate::core::telemetry::SessionOutcome {
            calls: 9,
            failures: 3,
            unresolved: vec![],
            retries: 3,
            retry_failures: 1,
        };
        assert_eq!(
            mcp_session_end_tallies(&outcome),
            [("mcp.session.recovered", 1, 0), ("mcp.session.retry", 3, 1)]
        );
    }
}
