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
}

fn parse_args(args: &[String]) -> Result<CallArgs, CallError> {
    let mut tool: Option<String> = None;
    let mut project_root: Option<String> = None;
    let mut json: Option<String> = None;
    let mut json_file: Option<String> = None;

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
    })
}

/// Load the project's most recent session for a one-shot `lean-ctx call`.
///
/// Index-based, never a full scan: `load_latest_for_project_root` parses
/// every file in the session store (measured: 160 files / 14.2 MB on one
/// developer machine) before filtering on project root, and `lean-ctx call`
/// is a short-lived subprocess that consumers spawn in loops.
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
    let parsed = parse_args(args)?;

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

    // Pre-resolve a `path` string arg into resolved_paths so handlers that read
    // ctx.resolved_path("path") (e.g. ctx_tree, require_resolved_path) work.
    // Without this, multi_path falls back to "." (CWD), not project_root.
    let mut resolved_paths = HashMap::new();
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
                return Err(CallError::Dispatch(DispatchError::PathResolution {
                    message: e,
                }));
            }
        }
    }

    let ctx = oneshot_ctx(parsed.project_root.clone(), resolved_paths);

    let registry = build_registry();
    let tool = registry
        .get(&parsed.tool)
        .ok_or_else(|| CallError::UnknownTool(parsed.tool.clone()))?;

    // Handlers are synchronous (the JetBrains backend uses blocking `ureq`),
    // so no tokio runtime is required here.
    let output = tool.handle(&args_map, &ctx).map_err(|e| {
        CallError::Dispatch(DispatchError::Tool {
            message: e.to_string(),
        })
    })?;

    Ok(output.text)
}

/// Thin CLI wrapper: print result to stdout (exit 0, even for functional
/// "ERROR:"/"BACKEND_REQUIRED:" output), or usage error to stderr (exit 2).
pub(crate) fn cmd_call(args: &[String]) {
    match run_call(args) {
        Ok(text) => println!("{text}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(e.exit_code());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
