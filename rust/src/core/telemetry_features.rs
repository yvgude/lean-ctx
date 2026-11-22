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

/// A session without a single MCP tool call during which the host's hooks
/// handled tool calls in the same host session (a governor install: hooks
/// do the work, the ctx tools stay deferred). Replaces `empty` for it.
const MCP_SESSION_HOOKED: &str = "mcp.session.hooked";
/// Tool calls the host's hooks handled in the session's host session while
/// it ran (LeanCTX's own MCP tools excluded); rides along with whatever
/// outcome the session got.
const MCP_SESSION_HOOK_CALLS: &str = "mcp.session.hook_calls";

/// Host session-id variables a hook subprocess and an MCP server both
/// inherit from the agent that launched them (the set `lean-ctx-server`'s
/// activity stream and the hooks' `observe` use).
const HOST_SESSION_ENV: &[&str] = &[
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "CODEX_THREAD_ID",
    "GEMINI_SESSION_ID",
    "CODEBUDDY_SESSION_ID",
];

/// The host session this MCP process serves, from its environment.
fn host_session_id_from_env() -> Option<String> {
    HOST_SESSION_ENV.iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    })
}

/// The per-session hook-call counter `<dir>/<hash of the host session id>`.
/// Keyed by session, not project, so parallel sessions in one checkout each
/// see only their own hooks' calls. The file holds one decimal number.
fn hook_calls_path_in(dir: &std::path::Path, session_id: &str) -> Option<std::path::PathBuf> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return None;
    }
    let key = blake3::hash(session_id.as_bytes()).to_hex();
    Some(dir.join(&key.as_str()[..16]))
}

/// [`hook_calls_path_in`] under `<data dir>/hook_calls/`.
fn hook_calls_path(session_id: &str) -> Option<std::path::PathBuf> {
    let dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
    hook_calls_path_in(&dir.join("hook_calls"), session_id)
}

fn read_hook_counter(path: &std::path::Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .unwrap_or(0)
}

/// Adds one to the counter at `path` (temp file + rename, so a reader never
/// sees a torn number; concurrent hooks may lose an increment, which only
/// lowers a count that is read as "did hooks run"). Writes nothing when
/// telemetry collection is off (`eligible` false).
fn bump_hook_counter(path: &std::path::Path, eligible: bool) {
    if !eligible {
        return;
    }
    let Some(dir) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let next = read_hook_counter(path).saturating_add(1);
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    if write_private(&tmp, next.to_string().as_bytes()).is_ok()
        && std::fs::rename(&tmp, path).is_err()
    {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Writes `bytes` to `path` as a file only the owner can read (0600 on Unix).
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)
}

/// Counter files untouched this long belong to sessions whose MCP server
/// never saw their end (`/clear` or `/resume` changed the host session id
/// under it, an idle exit, a crash, a hook-only install without MCP).
const HOOK_COUNTER_MAX_AGE: std::time::Duration = std::time::Duration::from_hours(168);
/// Upper bound on directory entries one sweep looks at.
const HOOK_COUNTER_SWEEP_LIMIT: usize = 4096;

/// Removes counter and temp files in `dir` last modified before
/// `now - max_age`. Bounded and fail-open: an unreadable entry is skipped.
fn sweep_hook_counters(
    dir: &std::path::Path,
    now: std::time::SystemTime,
    max_age: std::time::Duration,
) {
    let Some(cutoff) = now.checked_sub(max_age) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten().take(HOOK_COUNTER_SWEEP_LIMIT) {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_file() && meta.modified().is_ok_and(|modified| modified < cutoff) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Counts one tool call a host hook handled in the host session
/// `session_id` (the hook payload's `session_id`). Called by the governor's
/// PostToolUse hook; never fails the hook, and writes nothing while
/// telemetry is off.
pub fn record_hook_call(session_id: &str) {
    if let Some(path) = hook_calls_path(session_id) {
        bump_hook_counter(
            &path,
            crate::core::telemetry_aggregate::telemetry_collection_eligible(),
        );
    }
}

/// This MCP process's hook-call counter (its host session's) and its value
/// at the handshake, so the session end can tell how many calls hooks
/// handled.
static HOOK_BASELINE: std::sync::OnceLock<(std::path::PathBuf, u64)> = std::sync::OnceLock::new();

/// Counts an MCP server start after the handshake.
pub fn record_mcp_session_start() {
    let _ = crate::core::telemetry_aggregate::record_feature(MCP_SESSION, true);
    if let Ok(dir) = crate::core::data_dir::lean_ctx_data_dir() {
        sweep_hook_counters(
            &dir.join("hook_calls"),
            std::time::SystemTime::now(),
            HOOK_COUNTER_MAX_AGE,
        );
    }
    if let Some(path) = host_session_id_from_env().and_then(|id| hook_calls_path(&id)) {
        let start = read_hook_counter(&path);
        let _ = HOOK_BASELINE.set((path, start));
    }
}

/// Hook-handled tool calls in this process's host session since the
/// handshake. The counter file is removed afterwards so finished sessions
/// leave nothing behind (a second MCP server of the same session that
/// started earlier then reads fewer calls, never more). `None` when the
/// process knows no host session id, so whether hooks ran is unknown.
fn hook_calls_since_start() -> Option<u64> {
    HOOK_BASELINE.get().map(|(path, start)| {
        let calls = read_hook_counter(path).saturating_sub(*start);
        let _ = std::fs::remove_file(path);
        calls
    })
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
/// `hook_calls` is `None` when the server knew no host session id: a session
/// without MCP tool calls then gets no outcome at all, because it may have
/// been hook-driven and must not be counted as `empty`.
fn mcp_session_end_tallies(
    outcome: &crate::core::telemetry::SessionOutcome,
    hook_calls: Option<u64>,
) -> Vec<(&'static str, u64, u64)> {
    let hooks = hook_calls
        .filter(|calls| *calls > 0)
        .map(|calls| (MCP_SESSION_HOOK_CALLS, calls, 0));
    if outcome.calls == 0 {
        return match (hook_calls, hooks) {
            (None, _) => Vec::new(),
            (Some(_), Some(hooks)) => vec![(MCP_SESSION_HOOKED, 1, 0), hooks],
            (Some(_), None) => vec![(MCP_SESSION_EMPTY, 1, 0)],
        };
    }
    let ending = if outcome.failures == 0 {
        (MCP_SESSION_CLEAN, 1, 0)
    } else if outcome.unresolved.is_empty() {
        (MCP_SESSION_RECOVERED, 1, 0)
    } else {
        let ours = outcome.unresolved.iter().any(|kind| leanctx_side(*kind));
        (MCP_SESSION_UNRESOLVED, 1, u64::from(ours))
    };
    let mut tallies = vec![
        ending,
        (MCP_SESSION_RETRY, outcome.retries, outcome.retry_failures),
    ];
    tallies.extend(hooks);
    tallies
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
    let tallies = mcp_session_end_tallies(&outcome, hook_calls_since_start());
    let _ = crate::core::telemetry_aggregate::record_feature_counts(&tallies);
}

/// A `lean-ctx doctor` run that found LeanCTX Code's presence marker
/// (`docs/contracts/app-presence-v1.md`). Only that the marker was there is
/// counted, never its content.
pub const APP_CODE_DETECTED: &str = "app.code.detected";
/// The one hint about LeanCTX Code was shown after `lean-ctx doctor`.
pub const APP_CODE_HINT_SHOWN: &str = "app.code_hint.shown";

/// Counts a run that found LeanCTX Code managing this machine.
pub fn record_app_detected() {
    let _ = crate::core::telemetry_aggregate::record_feature(APP_CODE_DETECTED, true);
}

/// Counts one shown LeanCTX Code hint.
pub fn record_app_hint_shown() {
    let _ = crate::core::telemetry_aggregate::record_feature(APP_CODE_HINT_SHOWN, true);
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
            MCP_SESSION_HOOKED,
            MCP_SESSION_HOOK_CALLS,
            APP_CODE_DETECTED,
            APP_CODE_HINT_SHOWN,
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
            mcp_session_end_tallies(&outcome(0, 0, vec![]), Some(0)),
            [("mcp.session.empty", 1, 0)]
        );
        assert_eq!(
            mcp_session_end_tallies(&outcome(4, 0, vec![]), Some(0))[0],
            ("mcp.session.clean", 1, 0)
        );
        assert_eq!(
            mcp_session_end_tallies(&outcome(4, 2, vec![]), Some(0))[0],
            ("mcp.session.recovered", 1, 0)
        );
        assert_eq!(
            mcp_session_end_tallies(&outcome(4, 2, vec![FailureKind::NotFound]), Some(0))[0],
            ("mcp.session.unresolved", 1, 0),
            "a missing file is the caller's to resolve"
        );
        assert_eq!(
            mcp_session_end_tallies(
                &outcome(4, 2, vec![FailureKind::NotFound, FailureKind::Timeout]),
                Some(0)
            )[0],
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
            mcp_session_end_tallies(&outcome, Some(0)),
            [("mcp.session.recovered", 1, 0), ("mcp.session.retry", 3, 1)]
        );
    }

    #[test]
    fn hook_handled_sessions_are_not_empty() {
        let dir = tempfile::tempdir().unwrap();
        let counter = hook_calls_path_in(dir.path(), "session-a").unwrap();
        let start = read_hook_counter(&counter);
        for _ in 0..3 {
            bump_hook_counter(&counter, true);
        }
        let hook_calls = read_hook_counter(&counter).saturating_sub(start);
        assert_eq!(hook_calls, 3);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&counter).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "the counter is private to its owner");
        }
        let idle = crate::core::telemetry::SessionOutcome {
            calls: 0,
            failures: 0,
            unresolved: vec![],
            retries: 0,
            retry_failures: 0,
        };
        assert_eq!(
            mcp_session_end_tallies(&idle, Some(hook_calls)),
            [
                ("mcp.session.hooked", 1, 0),
                ("mcp.session.hook_calls", 3, 0)
            ],
            "a governor session whose hooks did the work is not empty"
        );
        assert!(
            mcp_session_end_tallies(&idle, None).is_empty(),
            "without a host session id an idle session is unknown, never empty"
        );
    }

    #[test]
    fn hook_calls_are_kept_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let busy = hook_calls_path_in(dir.path(), "session-busy").unwrap();
        let idle = hook_calls_path_in(dir.path(), "session-idle").unwrap();
        for _ in 0..2 {
            bump_hook_counter(&busy, true);
        }
        assert_eq!(read_hook_counter(&busy), 2);
        assert_eq!(
            read_hook_counter(&idle),
            0,
            "a parallel session in the same checkout sees none of another session's hook calls"
        );
        assert!(hook_calls_path_in(dir.path(), "  ").is_none());
    }

    #[test]
    fn stale_hook_counters_are_swept_and_fresh_ones_kept() {
        let dir = tempfile::tempdir().unwrap();
        let now = std::time::SystemTime::now();
        let stale = hook_calls_path_in(dir.path(), "session-cleared").unwrap();
        let fresh = hook_calls_path_in(dir.path(), "session-running").unwrap();
        bump_hook_counter(&stale, true);
        bump_hook_counter(&fresh, true);
        let stale_tmp = dir.path().join("orphan.123.tmp");
        write_private(&stale_tmp, b"1").unwrap();
        let eight_days_ago = now - std::time::Duration::from_hours(192);
        for old in [&stale, &stale_tmp] {
            std::fs::File::options()
                .write(true)
                .open(old)
                .unwrap()
                .set_modified(eight_days_ago)
                .unwrap();
        }
        sweep_hook_counters(dir.path(), now, HOOK_COUNTER_MAX_AGE);
        assert!(!stale.exists(), "an orphaned counter is swept after 7 days");
        assert!(!stale_tmp.exists(), "an orphaned temp file is swept too");
        assert_eq!(
            read_hook_counter(&fresh),
            1,
            "a live session's counter stays"
        );
    }

    #[test]
    fn telemetry_off_writes_no_hook_counter() {
        let dir = tempfile::tempdir().unwrap();
        let counter = hook_calls_path_in(dir.path(), "session-a").unwrap();
        bump_hook_counter(&counter, false);
        assert!(
            !counter.exists(),
            "telemetry off must not write the counter"
        );
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }
}
