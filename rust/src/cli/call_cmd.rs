use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value};

use crate::core::error::DispatchError;
use crate::server::registry::build_registry;
use crate::server::tool_trait::ToolContext;

/// CLI-level failure — distinct from a tool's *functional* result (a tool that
/// returns "ERROR:" / "BACKEND_REQUIRED:" text is a successful invocation and
/// goes to stdout with exit 0). These variants are wrong *usage* of `call`.
#[derive(Debug)]
pub(crate) enum CallError {
    Usage(String),
    UnknownTool(String),
    BadJson(String),
    UnsafeRoot(String),
    Dispatch(DispatchError),
    /// An operation that ran and failed (`TOOL_EXECUTION_ERROR`): the whole
    /// stderr line as it left the release point, prepared for stderr. It is
    /// written unchanged — nothing is prefixed to it.
    ToolFailed(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Usage(m) => write!(
                f,
                "error: {m}\nusage: lean-ctx call <tool> --project-root <path> --json '<json>' [--json-file <path>]"
            ),
            CallError::UnknownTool(t) => write!(f, "error: unknown tool '{t}'"),
            CallError::BadJson(m) => write!(f, "error: invalid --json: {m}"),
            CallError::UnsafeRoot(p) => {
                write!(f, "error: refusing broad/unsafe --project-root '{p}'")
            }
            CallError::Dispatch(e) => write!(f, "error: {e}"),
            CallError::ToolFailed(line) => f.write_str(line),
        }
    }
}

impl CallError {
    /// All CLI-usage errors map to exit code 2 (distinct from tool functional
    /// errors which exit 0). Reserved 1 for unexpected internal failures.
    /// Takes `&self` so future variants can map to differentiated exit codes
    /// without touching call sites.
    #[allow(clippy::unused_self)]
    pub(crate) fn exit_code(&self) -> i32 {
        2
    }
}

/// Parsed CLI invocation for `lean-ctx call`.
struct CallArgs {
    tool: String,
    project_root: String,
    json: String,
    /// File holding the presenter capability of the calling host, if it
    /// shows one (`--presenter-capability-file`). Never an argument value.
    presenter_capability_file: Option<String>,
}

fn parse_args(args: &[String]) -> Result<CallArgs, CallError> {
    let mut tool: Option<String> = None;
    let mut project_root: Option<String> = None;
    let mut json: Option<String> = None;
    let mut json_file: Option<String> = None;
    let mut presenter_capability_file: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--project-root" => {
                i += 1;
                project_root = Some(
                    args.get(i)
                        .ok_or_else(|| CallError::Usage("--project-root needs a value".into()))?
                        .clone(),
                );
            }
            "--json" => {
                i += 1;
                json = Some(
                    args.get(i)
                        .ok_or_else(|| CallError::Usage("--json needs a value".into()))?
                        .clone(),
                );
            }
            "--json-file" => {
                i += 1;
                json_file = Some(
                    args.get(i)
                        .ok_or_else(|| CallError::Usage("--json-file needs a value".into()))?
                        .clone(),
                );
            }
            "--presenter-capability-file" => {
                i += 1;
                presenter_capability_file = Some(
                    args.get(i)
                        .ok_or_else(|| {
                            CallError::Usage("--presenter-capability-file needs a value".into())
                        })?
                        .clone(),
                );
            }
            other if other.starts_with("--") => {
                return Err(CallError::Usage(format!("unknown flag '{other}'")));
            }
            _ => {
                if tool.is_none() {
                    tool = Some(args[i].clone());
                } else {
                    return Err(CallError::Usage(format!(
                        "unexpected argument '{}'",
                        args[i]
                    )));
                }
            }
        }
        i += 1;
    }

    let tool = tool.ok_or_else(|| CallError::Usage("missing <tool>".into()))?;
    let project_root =
        project_root.ok_or_else(|| CallError::Usage("missing --project-root".into()))?;

    let json = match (json, json_file) {
        (Some(_), Some(_)) => {
            return Err(CallError::Usage(
                "use either --json or --json-file, not both".into(),
            ));
        }
        (Some(j), None) => j,
        (None, Some(path)) => std::fs::read_to_string(&path)
            .map_err(|e| CallError::Usage(format!("cannot read --json-file '{path}': {e}")))?,
        (None, None) => "{}".to_string(),
    };

    Ok(CallArgs {
        tool,
        project_root,
        json,
        presenter_capability_file,
    })
}

/// Load the project's most recent session for a one-shot `lean-ctx call`.
///
/// Index-based with bounded reads: `load_recent_for_project_root` resolves the
/// canonical root, reads its project index, then loads only the indexed session.
/// One-shot CLI calls are short-lived subprocesses that consumers spawn in loops,
/// so their startup cost stays independent of unrelated session-store cardinality.
///
/// This function only READS — it never writes the session back, and there is
/// no implicit post-dispatch save (that belongs to the MCP server,
/// `server/post_dispatch.rs`). It is NOT an invariant that a one-shot call
/// leaves the session untouched: five actions reachable via `lean-ctx call`
/// persist it on purpose — `ctx_session` `save`/`reset`/`import` and
/// `ctx_handoff` `pull`/`import`. `write_to_disk`
/// (`core/session/persistence.rs`) skips a write when the on-disk version is
/// strictly newer, but an equal-version write still lands, so such a call
/// racing a live server on the same session can still clobber it.
///
/// Every failure degrades silently to an empty default session — no sessions
/// directory (the first-run case, not an error), no index for this root, an
/// id the store no longer has, malformed JSON, or a broad/unsafe root
/// rejected by `normalized_safe_project_root`. An empty session is a fully
/// valid context; it costs the task focus that steers `ctx_read` filtering,
/// nothing else.
fn oneshot_session(project_root: &str) -> crate::core::session::SessionState {
    crate::core::session::SessionState::load_recent_for_project_root(project_root, 1)
        .into_iter()
        .next()
        .unwrap_or_default()
}

fn oneshot_ctx(project_root: String, resolved_paths: HashMap<String, String>) -> ToolContext {
    let session = oneshot_session(&project_root);
    ToolContext {
        project_root,
        resolved_paths,
        // Give tools a real (empty) resident cache, as the MCP server does.
        // Without it, cache-aware tools (ctx_edit, ctx_compose) fail with
        // "cache not available" when invoked via `lean-ctx call`.
        cache: Some(std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::core::cache::SessionCache::default(),
        ))),
        bm25_cache: Some(std::sync::Arc::new(std::sync::Mutex::new(None))),
        // Same class of omission as `cache` above: without these three,
        // ten registered tools abort with "<handle> not available".
        // `tool_calls` genuinely starts empty — it is in-process turn history
        // a fresh subprocess cannot have. The ledger is the opposite: it is
        // persisted state, so it is loaded from disk exactly as the MCP server
        // does (`tools/server_lifecycle.rs`). An empty ledger here would not be
        // a clean slate but data loss — tools that hold this handle write it
        // straight back (`ctx_control` saves unconditionally, `ctx_ledger`
        // reset/evict likewise), truncating the user's real, global
        // `state_dir()/context_ledger.json` while every action silently no-ops.
        session: Some(std::sync::Arc::new(tokio::sync::RwLock::new(session))),
        tool_calls: Some(std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new()))),
        ledger: Some(std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::core::context_ledger::ContextLedger::load(),
        ))),
        // Every remaining handle stays None deliberately: no registered tool
        // aborts on their absence, and each would pull in server state a
        // one-shot process has no basis to fabricate.
        ..Default::default()
    }
}

/// Core, testable entry point. Returns the tool's stdout text on success;
/// `CallError` only for CLI-usage problems (never for functional tool errors).
pub(crate) fn run_call(args: &[String]) -> Result<String, CallError> {
    run_call_as(args, true)
}

/// [`run_call`] for a caller that EVALUATES the tool's text and never writes
/// it (`policy enforce`): the handler runs in the same request scope and
/// source capture, but the text is not a delivery — it is not released, no
/// receipt is prepared and no lineage is recorded for it. The returned text
/// and the text of a failed operation are therefore UNRELEASED: whatever the
/// caller writes of them must pass a release of its own first.
pub(crate) fn run_call_undelivered(args: &[String]) -> Result<String, CallError> {
    run_call_as(args, false)
}

fn run_call_as(args: &[String], deliver: bool) -> Result<String, CallError> {
    let parsed = parse_args(args)?;
    // A host that shows a presenter capability shows it for the whole call:
    // the capability is request state of the lineage checks, not a tool
    // argument, and it never reaches the handler, its output or an error.
    let Some(file) = parsed.presenter_capability_file.clone() else {
        return run_parsed(parsed, deliver);
    };
    let capability = crate::core::archive::presented::read_capability_file(Path::new(&file))
        .ok_or_else(|| CallError::Usage("--presenter-capability-file is not usable".into()))?;
    crate::core::archive::presented::with_presenter(Some(capability), || {
        run_parsed(parsed, deliver)
    })
}

fn run_parsed(parsed: CallArgs, deliver: bool) -> Result<String, CallError> {
    // Same defense as MCP root resolution: never operate on a broad/unsafe root.
    if crate::core::pathutil::is_broad_or_unsafe_root(Path::new(&parsed.project_root)) {
        return Err(CallError::UnsafeRoot(parsed.project_root));
    }

    let value: Value =
        serde_json::from_str(&parsed.json).map_err(|e| CallError::BadJson(e.to_string()))?;
    let args_map: Map<String, Value> = match value {
        Value::Object(m) => m,
        _ => return Err(CallError::BadJson("expected a JSON object".into())),
    };

    let registry = build_registry();
    let tool = registry
        .get(&parsed.tool)
        .ok_or_else(|| CallError::UnknownTool(parsed.tool.clone()))?;
    // Same argument spellings the MCP dispatcher accepts (`file_path`, `cmd`).
    let args_map = crate::server::canonicalize_arg_aliases(tool, &args_map).unwrap_or(args_map);

    // Pre-resolve a `path` string arg into resolved_paths so handlers that read
    // ctx.resolved_path("path") (e.g. ctx_tree, require_resolved_path) work.
    // Without this, multi_path falls back to "." (CWD), not project_root. A
    // refusal is recorded in path_errors exactly as the MCP dispatcher does,
    // so each tool reports it (or narrows a broad walk) the same way.
    let mut resolved_paths = HashMap::new();
    let mut path_errors = HashMap::new();
    if let Some(p) = args_map.get("path").and_then(Value::as_str) {
        match crate::core::path_resolve::resolve_tool_path(Some(&parsed.project_root), None, p) {
            Ok(abs) => {
                // `resolve_tool_path` passes "." / "" through unchanged, which a
                // handler would then resolve against its CWD — not project_root.
                // Pin those to the explicit project_root so handlers operate on
                // the root we were given, never the process CWD.
                let resolved = if abs.is_empty() || abs == "." {
                    parsed.project_root.clone()
                } else {
                    abs
                };
                resolved_paths.insert("path".to_string(), resolved);
            }
            Err(e) => {
                path_errors.insert("path".to_string(), e);
            }
        }
    }

    let mut ctx = oneshot_ctx(parsed.project_root.clone(), resolved_paths);
    ctx.path_errors = path_errors;

    // Handlers are synchronous (the JetBrains backend uses blocking `ureq`),
    // so no tokio runtime is required here.
    //
    // Request scope, source capture, release point, output redaction and the
    // lineage record of the printed text are one sequence, owned by
    // `CliToolRun` (see `cli/common.rs`): the handler runs exactly as under
    // the MCP dispatcher, and the text returned here is the text `cmd_call`
    // prints, unchanged.
    //
    // A tool error is never a delivery. The signals that exist in the tool
    // contract are used, not the output text: a handler `Err` (nothing is
    // released or recorded) and `ToolOutput::shell_outcome` reporting an
    // error (`ShellOutcome::is_error`). A tool that answers `Ok` with an
    // error sentence carries no flag today — see the report's requests.
    //
    // `TOOL_EXECUTION_ERROR` is the registry's signal for an operation that
    // ran and failed; its body can quote file excerpts. It is reported on
    // stderr as before, but only after it passed the same release point as a
    // result (as a failed text: released, redacted, never recorded). What
    // is released is the WHOLE stderr line, prepared for stderr, so the
    // receipt binds the bytes `cmd_call` writes (G08).
    let run = super::common::CliToolRun::new(&parsed.tool, &parsed.project_root, Some(&args_map));
    let (handled, capture) = run.acquire(|| tool.handle(&args_map, &ctx));
    match handled {
        // Not a delivery (`run_call_undelivered`): the run ends after the
        // acquisition — nothing is released, receipted or recorded.
        Ok(output) if !deliver => Ok(output.text),
        Ok(output) => {
            let failed = output
                .shell_outcome
                .as_ref()
                .is_some_and(crate::server::tool_trait::ShellOutcome::is_error);
            let printable = super::common::Printable {
                text: output.text,
                source_only: true,
                failed,
            };
            Ok(run.finish(printable, capture))
        }
        Err(mut error) if error.code == crate::server::tool_trait::TOOL_EXECUTION_ERROR => {
            // Nothing but the code and the message of the failure is passed
            // on; the line is the one a dispatch error is reported with.
            error.data = None;
            let failure = CallError::Dispatch(DispatchError::Tool {
                message: error.to_string(),
            });
            if !deliver {
                return Err(failure);
            }
            let line = failure.to_string();
            let printable = super::common::Printable {
                text: line,
                source_only: false,
                failed: true,
            };
            Err(CallError::ToolFailed(
                run.to_stderr().finish(printable, capture),
            ))
        }
        Err(error) => Err(CallError::Dispatch(DispatchError::Tool {
            message: error.to_string(),
        })),
    }
}

/// Thin CLI wrapper: print result to stdout (exit 0, even for functional
/// "ERROR:"/"BACKEND_REQUIRED:" output), or usage error to stderr (exit 2).
pub(crate) fn cmd_call(args: &[String]) {
    // The printed result is all the caller sees: a delivery stub or a
    // cross-agent cache reference would point at content it never received.
    crate::core::conversation::mark_one_shot_process();
    let tool = args.first().cloned().unwrap_or_default();
    spawn_watchdog(tool, call_time_limit());
    match run_call(args) {
        Ok(text) => super::common::print_released(&text),
        Err(e) => {
            let code = e.exit_code();
            match &e {
                // The released line of a failed operation goes through the
                // one observed stderr writer, with the status it ends with.
                CallError::ToolFailed(line) => super::common::eprint_released(line, Some(code)),
                other => eprintln!("{other}"),
            }
            std::process::exit(code);
        }
    }
}

/// How often a long one-shot call reports its phase on stderr (#2063).
const PROGRESS_EVERY: std::time::Duration = std::time::Duration::from_secs(30);
/// Default wall-clock limit of one `lean-ctx call`.
const DEFAULT_TIME_LIMIT_SECS: u64 = 300;

/// `LEAN_CTX_CALL_TIMEOUT_SECS` (0 = no limit), else the default.
fn call_time_limit() -> Option<std::time::Duration> {
    let secs = std::env::var("LEAN_CTX_CALL_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_TIME_LIMIT_SECS);
    (secs > 0).then(|| std::time::Duration::from_secs(secs))
}

/// #2063: a stalled call printed nothing until the caller killed it. Every
/// [`PROGRESS_EVERY`] it now names the phase it is in on stderr (stdout stays
/// the result), and past the limit it stops with exit code 124, like
/// `timeout(1)`, instead of hanging.
fn spawn_watchdog(tool: String, limit: Option<std::time::Duration>) {
    let started = std::time::Instant::now();
    let _ = std::thread::Builder::new()
        .name("call-watchdog".into())
        .spawn(move || {
            let mut next_report = PROGRESS_EVERY;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let elapsed = started.elapsed();
                let over = limit.is_some_and(|limit| elapsed >= limit);
                if !over && elapsed < next_report {
                    continue;
                }
                let phase = crate::core::phase_timing::current().map_or_else(
                    || "outside a named phase".to_string(),
                    |(name, spent)| format!("{name}, {}s so far", spent.as_secs()),
                );
                if over {
                    eprintln!(
                        "lean-ctx call {tool}: no result after {}s (in {phase}) — stopped. \
                         Set LEAN_CTX_CALL_TIMEOUT_SECS to wait longer (0 = no limit) and \
                         LEAN_CTX_PHASE_TIMING=1 to see every phase's duration.",
                        elapsed.as_secs()
                    );
                    std::process::exit(124);
                }
                eprintln!(
                    "lean-ctx call {tool}: still running after {}s (in {phase})",
                    elapsed.as_secs()
                );
                next_report += PROGRESS_EVERY;
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_time_limit_defaults_and_zero_disables() {
        let _env = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_CALL_TIMEOUT_SECS");
        assert_eq!(
            call_time_limit(),
            Some(std::time::Duration::from_secs(DEFAULT_TIME_LIMIT_SECS))
        );
        crate::test_env::set_var("LEAN_CTX_CALL_TIMEOUT_SECS", "0");
        assert_eq!(call_time_limit(), None);
        crate::test_env::set_var("LEAN_CTX_CALL_TIMEOUT_SECS", "45");
        assert_eq!(call_time_limit(), Some(std::time::Duration::from_secs(45)));
        crate::test_env::remove_var("LEAN_CTX_CALL_TIMEOUT_SECS");
    }

    #[test]
    fn unknown_tool_is_cli_error() {
        let args = vec![
            "definitely_not_a_tool".to_string(),
            "--project-root".to_string(),
            std::env::temp_dir().to_string_lossy().to_string(),
            "--json".to_string(),
            "{}".to_string(),
        ];
        let err = run_call(&args).expect_err("expected unknown-tool error");
        assert!(matches!(err, CallError::UnknownTool(_)), "got {err:?}");
    }

    #[test]
    fn invalid_json_is_cli_error() {
        let args = vec![
            "ctx_tree".to_string(),
            "--project-root".to_string(),
            std::env::temp_dir().to_string_lossy().to_string(),
            "--json".to_string(),
            "{not json".to_string(),
        ];
        let err = run_call(&args).expect_err("expected bad-json error");
        assert!(matches!(err, CallError::BadJson(_)), "got {err:?}");
    }

    #[test]
    fn unsafe_root_is_rejected() {
        let args = vec![
            "ctx_tree".to_string(),
            "--project-root".to_string(),
            "/".to_string(),
            "--json".to_string(),
            "{}".to_string(),
        ];
        let err = run_call(&args).expect_err("expected unsafe-root error");
        assert!(matches!(err, CallError::UnsafeRoot(_)), "got {err:?}");
    }

    #[test]
    fn missing_project_root_is_usage_error() {
        let args = vec![
            "ctx_tree".to_string(),
            "--json".to_string(),
            "{}".to_string(),
        ];
        let err = run_call(&args).expect_err("expected usage error");
        assert!(matches!(err, CallError::Usage(_)), "got {err:?}");
    }

    #[test]
    fn happy_path_dispatches_to_real_tool() {
        // Its own data directory: the call stores a receipt, and a parallel
        // test switching the process's data directory must not take it away.
        let _data = crate::core::data_dir::isolated_data_dir();
        let dir = std::env::temp_dir().join(format!("leanctx-call-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("MARKER_FILE.txt");
        std::fs::write(&marker, b"x").unwrap();

        let args = vec![
            "ctx_tree".to_string(),
            "--project-root".to_string(),
            dir.to_string_lossy().to_string(),
            "--json".to_string(),
            r#"{"path": "."}"#.to_string(),
        ];
        let out = run_call(&args).expect("dispatch should succeed");
        assert!(
            out.contains("MARKER_FILE.txt"),
            "tree output missing marker:\n{out}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// G01/G05: `lean-ctx call` printed a tool's text after the legacy output
    /// redaction only; the release point the MCP path applies was skipped. A
    /// text that bypassed admission is now redacted there, a clean text keeps
    /// its bytes.
    #[test]
    fn a_tool_text_passes_the_mcp_release_point() {
        const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let project = tempfile::tempdir().expect("project dir");
        let root = project.path().to_string_lossy().into_owned();
        let run = crate::cli::common::CliToolRun::new("ctx_tree", &root, None);
        let clean = "src/\n  main.rs\n  lib.rs";
        assert_eq!(run.deliver_text(|| clean.to_string()), clean);
        let leaked = format!("deploy.env\n  AWS_ACCESS_KEY_ID={AWS_KEY}");
        let released = run.deliver_text(|| leaked.clone());
        assert!(!released.contains(AWS_KEY), "{released}");
        assert!(released.contains("deploy.env"), "{released}");
    }

    /// Governed mode without a pack: protection is active, so the admitted
    /// readers publish their sources into the capture.
    fn governed() {
        let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
        std::fs::create_dir_all(&dir).expect("config dir");
        std::fs::write(
            dir.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\n",
        )
        .expect("config");
    }

    fn call(tool: &str, root: &str, arguments: &serde_json::Value) -> String {
        let args = vec![
            tool.to_string(),
            "--project-root".to_string(),
            root.to_string(),
            "--json".to_string(),
            arguments.to_string(),
        ];
        run_call(&args).unwrap_or_else(|error| error.to_string())
    }

    /// The lineage on record for exactly `printed`, looked up and judged
    /// inside the project scope, as the proxy does.
    fn lineage(root: &str, printed: &str) -> Option<bool> {
        crate::tools::admitted_sources::as_agent_acquisition(root, || {
            crate::core::archive::delivered::lineage_of(&crate::core::archive::delivered::digest(
                printed,
            ))
            .map(|lineage| lineage.still_admitted().is_ok())
        })
    }

    /// G03/G04: an allowed `lean-ctx call ctx_read` printed checked text whose
    /// lineage stayed unknown — no source capture, no delivered record. The
    /// printed text now has a lineage entry; it stops admitting when the
    /// source is deleted; an undeclared tool and an error record nothing.
    #[test]
    fn a_declared_call_records_the_lineage_of_the_printed_text() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        governed();
        let project = tempfile::tempdir().expect("project dir");
        let root = std::fs::canonicalize(project.path())
            .expect("root")
            .to_string_lossy()
            .into_owned();
        let file = std::path::Path::new(&root).join("sample.rs");
        std::fs::write(&file, "pub fn sample() -> u32 { 7 }\n").expect("write sample");
        let path = file.to_string_lossy().into_owned();

        let printed = call(
            "ctx_read",
            &root,
            &serde_json::json!({ "path": path, "mode": "full" }),
        );
        assert!(printed.contains("pub fn sample"), "{printed}");
        assert_eq!(
            lineage(&root, &printed),
            Some(true),
            "printed text has a lineage"
        );
        // The same delivery as stdout carries it (`println!` adds one `\n`).
        assert_eq!(lineage(&root, &format!("{printed}\n")), Some(true));

        // An undeclared tool: checked output, no lineage entry.
        let outlined = call("ctx_outline", &root, &serde_json::json!({ "path": path }));
        assert!(outlined.contains("sample"), "{outlined}");
        assert_eq!(lineage(&root, &outlined), None);

        // Source deleted: the entry is found and no longer admits.
        std::fs::remove_file(&file).expect("delete source");
        assert_eq!(lineage(&root, &printed), Some(false));

        // A tool error prints an error and records nothing.
        let failed = call(
            "ctx_read",
            &root,
            &serde_json::json!({ "path": path, "mode": "full" }),
        );
        assert_eq!(lineage(&root, &failed), None, "{failed}");
    }

    /// The entry belongs to the text that is printed — after release and
    /// redaction — never to the text the handler returned.
    #[test]
    fn the_lineage_entry_is_for_the_redacted_printed_text() {
        const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
        let _data = crate::core::data_dir::isolated_data_dir();
        // An empty project pack in developer mode: the pack makes protection
        // active, so the admitted reader publishes its source into the
        // capture, and the release point masks the key in a plain text block
        // that bypassed admission. A key alone never withholds the result,
        // with a pack as without one (`policy_guard::release`).
        let pack = crate::core::policy::load(
            "name = \"cli-call-lineage\"\nversion = \"1.0.0\"\ndescription = \"test\"\n",
        )
        .expect("empty pack");
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(pack));
        let project = tempfile::tempdir().expect("project dir");
        let root = std::fs::canonicalize(project.path())
            .expect("root")
            .to_string_lossy()
            .into_owned();
        let file = std::path::Path::new(&root).join("notes.rs");
        std::fs::write(&file, "pub fn notes() {}\n").expect("write");
        let path = file.to_string_lossy().into_owned();

        // A declared tool whose handler read one admitted source and then
        // returned a text that still carries a value the release point masks.
        let raw = format!("pub fn notes() {{}}\nkey {AWS_KEY}");
        let run = crate::cli::common::CliToolRun::new("ctx_read", &root, None);
        let printed = run.deliver_text(|| {
            crate::tools::ctx_read::read_file_for_tool_rooted(&path, &root, "ctx_read")
                .expect("admitted source");
            raw.clone()
        });
        assert!(!printed.contains(AWS_KEY), "{printed}");
        assert_ne!(printed, raw);
        assert!(
            printed.ends_with("pub fn notes() {}\nkey [REDACTED:AWS key]"),
            "masked once, the rest delivered: {printed}"
        );
        assert_eq!(lineage(&root, &raw), None, "never the pre-redaction text");
        assert_eq!(lineage(&root, &printed), Some(true), "{printed}");
    }

    /// The handler runs as an agent acquisition: a file read through
    /// `lean-ctx call` is admitted like under the MCP dispatcher (it was a
    /// plain local read without the request scope).
    #[test]
    fn a_call_reads_as_an_agent_acquisition() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let policy = crate::core::policy::load(
            "name = \"cli-call-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
             [filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )
        .unwrap();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        let project = tempfile::tempdir().expect("project dir");
        let file = project.path().join("plan.rs");
        std::fs::write(&file, "CONFIDENTIAL\npub fn roadmap_marker() {}\n").expect("write");
        let args = vec![
            "ctx_outline".to_string(),
            "--project-root".to_string(),
            project.path().to_string_lossy().to_string(),
            "--json".to_string(),
            format!(
                r#"{{"path": {}}}"#,
                serde_json::to_string(file.to_string_lossy().as_ref()).expect("encode path")
            ),
        ];
        let out = run_call(&args).unwrap_or_else(|error| error.to_string());
        assert!(!out.contains("roadmap_marker"), "{out}");
    }

    /// A failed operation (`TOOL_EXECUTION_ERROR`, or a tool's own error
    /// answer) is reported, and whatever text reports it is never a delivery:
    /// no lineage entry exists for it, and the file is unchanged.
    #[test]
    fn a_failed_operation_registers_nothing() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        governed();
        let project = tempfile::tempdir().expect("project dir");
        let root = std::fs::canonicalize(project.path())
            .expect("root")
            .to_string_lossy()
            .into_owned();
        let file = std::path::Path::new(&root).join("kept.rs");
        std::fs::write(&file, "pub fn kept() {}\n").expect("write");
        let args = vec![
            "ctx_edit".to_string(),
            "--project-root".to_string(),
            root.clone(),
            "--json".to_string(),
            serde_json::json!({
                "path": file.to_string_lossy(),
                "old_string": "pub fn absent() {}",
                "new_string": "pub fn replaced() {}",
            })
            .to_string(),
        ];
        let reported = match run_call(&args) {
            Ok(text) => text,
            Err(error) => error.to_string(),
        };
        assert!(!reported.is_empty());
        assert_eq!(lineage(&root, &reported), None, "{reported}");
        assert_eq!(
            lineage(&root, reported.trim_start_matches("error: ")),
            None,
            "{reported}"
        );
        assert_eq!(
            std::fs::read_to_string(&file).expect("file"),
            "pub fn kept() {}\n"
        );
    }

    #[test]
    fn oneshot_ctx_supplies_session_tool_calls_and_ledger() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().expect("data dir");
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());
        let project = tempfile::tempdir().expect("project dir");

        let ctx = oneshot_ctx(project.path().to_string_lossy().to_string(), HashMap::new());

        assert!(ctx.session.is_some(), "session handle missing");
        assert!(ctx.tool_calls.is_some(), "tool_calls handle missing");
        assert!(ctx.ledger.is_some(), "ledger handle missing");
    }

    #[test]
    fn oneshot_session_without_an_index_returns_a_default() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().expect("data dir");
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());
        let project = tempfile::tempdir().expect("project dir");

        let session = oneshot_session(&project.path().to_string_lossy());

        assert!(
            session.task.is_none(),
            "expected a default session for a root with no index"
        );
    }

    #[test]
    fn ctx_read_via_call_no_longer_aborts_on_a_missing_session() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().expect("data dir");
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());
        let project = tempfile::tempdir().expect("project dir");
        let file = project.path().join("sample.rs");
        std::fs::write(&file, b"pub fn sample() -> u32 { 7 }\n").expect("write sample");

        let args = vec![
            "ctx_read".to_string(),
            "--project-root".to_string(),
            project.path().to_string_lossy().to_string(),
            "--json".to_string(),
            format!(
                r#"{{"path": {}, "mode": "signatures"}}"#,
                serde_json::to_string(file.to_string_lossy().as_ref()).expect("encode path")
            ),
        ];

        let out = run_call(&args).expect("ctx_read should dispatch, not abort");
        assert!(
            out.contains("sample"),
            "ctx_read output missing the file's symbol:\n{out}"
        );
    }

    fn session_file_names(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("sessions dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn an_indexed_session_loads_and_a_one_shot_call_never_writes_it_back() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().expect("data dir");
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());
        let project = tempfile::tempdir().expect("project dir");
        let root = project.path().to_string_lossy().to_string();

        // Seed one session through the public API. `write_to_disk` is what
        // populates the project index that `oneshot_session` reads back.
        // Struct-update syntax, not `default()` + field assignment: the latter
        // trips `clippy::field_reassign_with_default` and the gate is `-D warnings`.
        let mut seeded = crate::core::session::SessionState {
            project_root: Some(root.clone()),
            task: Some(crate::core::session::TaskInfo {
                description: "seeded task description".to_string(),
                intent: None,
                progress_pct: None,
            }),
            ..Default::default()
        };
        let seeded_id = seeded.id.clone();
        seeded
            .prepare_save()
            .expect("prepare_save")
            .write_to_disk()
            .expect("write_to_disk");

        // Half 1 — the index lookup resolves that session, not a default.
        let loaded = oneshot_session(&root);
        assert_eq!(
            loaded.task.as_ref().map(|task| task.description.as_str()),
            Some("seeded task description"),
            "oneshot_session did not resolve the seeded session through the index"
        );

        // Half 2 — a one-shot call that mutates the session writes nothing back.
        let sessions = data.path().join("sessions");
        let seeded_path = sessions.join(format!("{seeded_id}.json"));
        let before = std::fs::read(&seeded_path).expect("seeded session file");
        let files_before = session_file_names(&sessions);

        let args = vec![
            "ctx_session".to_string(),
            "--project-root".to_string(),
            root,
            "--json".to_string(),
            r#"{"action": "task", "value": "a different task"}"#.to_string(),
        ];
        let out = run_call(&args).expect("ctx_session action=task should run to completion");
        assert!(
            out.contains("Task set"),
            "ctx_session did not report success:\n{out}"
        );

        assert_eq!(
            before,
            std::fs::read(&seeded_path).expect("seeded session file"),
            "a one-shot call rewrote the seeded session file"
        );
        assert_eq!(
            files_before,
            session_file_names(&sessions),
            "a one-shot call created or removed a session file"
        );
    }

    /// Regression: a one-shot call must never truncate the user's global
    /// context ledger (`state_dir()/context_ledger.json`, ONE non-project-scoped
    /// file rewritten wholesale). `oneshot_ctx` briefly handed tools a
    /// `ContextLedger::default()`; every ledger-holding tool writes that handle
    /// straight back — `ctx_control` saves unconditionally — so a single
    /// `lean-ctx call ctx_control` erased the real ledger.
    ///
    /// Isolation: `state_dir()` resolves through `paths::category_dir`, whose
    /// explicit `LEAN_CTX_STATE_DIR` override wins even under `#[cfg(test)]`
    /// (`LEAN_CTX_DATA_DIR` does NOT govern it). Pointing it at a tempdir is
    /// what keeps this test off the developer's real `~/.local/state/lean-ctx`.
    #[test]
    fn a_one_shot_call_never_truncates_the_global_ledger() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let state = tempfile::tempdir().expect("state dir");
        crate::test_env::set_var("LEAN_CTX_STATE_DIR", state.path());
        let data = tempfile::tempdir().expect("data dir");
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());
        let project = tempfile::tempdir().expect("project dir");
        let marker_file = project.path().join("LEDGER_SEED_MARKER.rs");
        std::fs::write(&marker_file, b"pub fn seeded() -> u32 { 1 }\n").expect("write marker");

        // Seed an entry a `ContextLedger::default()` could never carry.
        let mut seeded = crate::core::context_ledger::ContextLedger::new();
        seeded.record(&marker_file.to_string_lossy(), "full", 4_000, 1_000);
        seeded.save();

        let ledger_file = state.path().join("context_ledger.json");
        let before = std::fs::read_to_string(&ledger_file).expect("seeded ledger file");
        assert!(
            before.contains("LEDGER_SEED_MARKER"),
            "precondition: the seed never reached {}",
            ledger_file.display()
        );

        // ctx_control writes back whatever ledger `oneshot_ctx` gave it.
        let args = vec![
            "ctx_control".to_string(),
            "--project-root".to_string(),
            project.path().to_string_lossy().to_string(),
            "--json".to_string(),
            r#"{"action": "list"}"#.to_string(),
        ];
        run_call(&args).expect("ctx_control should dispatch");

        let after = crate::core::context_ledger::ContextLedger::load();
        assert!(
            after
                .entries
                .iter()
                .any(|entry| entry.path.contains("LEDGER_SEED_MARKER")),
            "a one-shot call truncated the global ledger — {} entries survived; file now:\n{}",
            after.entries.len(),
            std::fs::read_to_string(&ledger_file).unwrap_or_default()
        );
    }
}
