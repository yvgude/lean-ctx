use rmcp::ErrorData;
use rmcp::model::{ContentBlock, Tool};
use serde_json::{Map, Value};

/// Stable states exposed by `ctx_shell(background_action="status")`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundJobState {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl BackgroundJobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundDisplay {
    pub header: String,
    pub footer: Option<String>,
}

/// Protocol metadata that must survive output compression and archiving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundShellOutcome {
    pub state: BackgroundJobState,
    pub exit_code: Option<i32>,
    pub job_id: String,
    pub archive_id: Option<String>,
    pub archive_truncated: Option<bool>,
    pub captured_chars: Option<usize>,
    pub archived_chars: Option<usize>,
    pub summary: String,
    pub is_error: bool,
    pub display: Option<BackgroundDisplay>,
}

/// A background job lookup failed without evidence of a lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundLookupError {
    pub job_id: String,
    pub code: String,
    pub reason: String,
}

/// Outcome of a shell execution, carried alongside the rendered text so the
/// MCP dispatch layer can surface failures in protocol metadata instead of
/// only as an `[exit:N]` text footer (GitHub #389: clients had no programmatic
/// way to detect ctx_shell failures and resorted to fragile regex matching).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellOutcome {
    /// The command ran; carries its real exit code (0 = success).
    Exit(i32),
    /// The command never ran (allowlist/validation rejection, or a
    /// precondition failure such as an unreadable/oversized input file).
    Blocked,
    /// A managed background job status. Unlike a foreground `Exit(1)`, a
    /// terminal failed job is never reclassified as grep-like result data.
    Background(BackgroundShellOutcome),
    /// The requested background job is unknown or its retained verdict expired.
    BackgroundLookupError(BackgroundLookupError),
}

impl ShellOutcome {
    /// Whether this outcome must be reported as a tool error (`isError: true`).
    pub fn is_error(&self) -> bool {
        match self {
            ShellOutcome::Exit(code) => *code != 0,
            ShellOutcome::Blocked | ShellOutcome::BackgroundLookupError(_) => true,
            ShellOutcome::Background(outcome) => outcome.is_error,
        }
    }

    /// Structured payload for `CallToolResult.structuredContent`, so guards
    /// can read state/exit/archive data instead of parsing output text.
    /// Ordinary foreground success (exit 0) stays token-neutral; background
    /// status always remains structured because clients may filter its text.
    pub fn structured(&self) -> Option<serde_json::Value> {
        match self {
            ShellOutcome::Exit(0) => None,
            ShellOutcome::Exit(code) => Some(serde_json::json!({ "exitCode": code })),
            ShellOutcome::Blocked => Some(serde_json::json!({ "blocked": true })),
            ShellOutcome::Background(outcome) => {
                let mut fields = serde_json::Map::new();
                fields.insert(
                    "state".to_string(),
                    serde_json::json!(outcome.state.as_str()),
                );
                fields.insert("jobId".to_string(), serde_json::json!(outcome.job_id));
                fields.insert("summary".to_string(), serde_json::json!(outcome.summary));
                if let Some(exit_code) = outcome.exit_code {
                    fields.insert("exitCode".to_string(), serde_json::json!(exit_code));
                }
                if let Some(archive_id) = &outcome.archive_id {
                    fields.insert("archiveId".to_string(), serde_json::json!(archive_id));
                }
                if let Some(archive_truncated) = outcome.archive_truncated {
                    fields.insert(
                        "archiveTruncated".to_string(),
                        serde_json::json!(archive_truncated),
                    );
                }
                if let Some(captured_chars) = outcome.captured_chars {
                    fields.insert(
                        "capturedChars".to_string(),
                        serde_json::json!(captured_chars),
                    );
                }
                if let Some(archived_chars) = outcome.archived_chars {
                    fields.insert(
                        "archivedChars".to_string(),
                        serde_json::json!(archived_chars),
                    );
                }
                Some(serde_json::Value::Object(fields))
            }
            ShellOutcome::BackgroundLookupError(error) => Some(serde_json::json!({
                "jobId": error.job_id,
                "errorCode": error.code,
                "reason": error.reason,
            })),
        }
    }

    /// Whether this is a status body that the common pipeline must secure,
    /// archive and render before delivery.
    pub fn is_background_status(&self) -> bool {
        matches!(
            self,
            ShellOutcome::Background(BackgroundShellOutcome {
                display: Some(_),
                ..
            })
        )
    }
}

/// Result returned by an McpTool handler.
pub struct ToolOutput {
    pub text: String,
    pub original_tokens: usize,
    pub saved_tokens: usize,
    pub mode: Option<String>,
    /// Path associated with the tool call (for record_call_with_path).
    pub path: Option<String>,
    /// True when the tool mutated state that clients should know about
    /// (e.g. dynamic tool categories changed).
    pub changed: bool,
    /// Set by shell-executing tools so dispatch can populate `isError` +
    /// `structuredContent` on the MCP result (GitHub #389). `None` for tools
    /// that don't run shell commands.
    pub shell_outcome: Option<ShellOutcome>,
    /// Override content blocks for non-text responses (e.g. images).
    /// When set, dispatch uses these instead of wrapping `text` in TextContent.
    pub content_blocks: Option<Vec<ContentBlock>>,
}

impl ToolOutput {
    pub fn simple(text: String) -> Self {
        Self {
            text,
            original_tokens: 0,
            saved_tokens: 0,
            mode: None,
            path: None,
            changed: false,
            shell_outcome: None,
            content_blocks: None,
        }
    }

    /// Compact one-line summary for headers_only response verbosity.
    pub fn to_header_line(&self, tool_name: &str) -> String {
        let path_str = self.path.as_deref().unwrap_or("—");
        let mode_str = self.mode.as_deref().unwrap_or("—");
        let sent = self.original_tokens.saturating_sub(self.saved_tokens);
        let pct = if self.original_tokens > 0 {
            (self.saved_tokens as f64 / self.original_tokens as f64 * 100.0) as u32
        } else {
            0
        };
        format!("[{tool_name}: {path_str}, mode={mode_str}, {sent} tok sent, -{pct}%]")
    }

    pub fn with_savings(text: String, original: usize, saved: usize) -> Self {
        Self {
            text,
            original_tokens: original,
            saved_tokens: saved,
            mode: None,
            path: None,
            changed: false,
            shell_outcome: None,
            content_blocks: None,
        }
    }

    /// Construct a ToolOutput for image/binary content blocks.
    /// Bypasses all text processing in the dispatch pipeline.
    pub fn image(blocks: Vec<ContentBlock>, path: String) -> Self {
        Self {
            text: String::new(),
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some("image".to_string()),
            path: Some(path),
            changed: false,
            shell_outcome: None,
            content_blocks: Some(blocks),
        }
    }
}

/// Trait for a self-contained MCP tool. Each tool provides its own schema
/// definition and handler, eliminating the possibility of schema/handler drift.
///
/// This trait is the plugin interface for LcpTools: any implementation can be
/// registered at runtime via `ToolRegistry::register()`. Future plugin system
/// will load implementations from shared libraries or subprocess bridges.
///
/// Handlers are synchronous because all existing tool handlers are sync.
/// The async boundary (cache locks, session reads) is handled by the dispatch
/// layer before calling `handle`.
pub trait McpTool: Send + Sync {
    /// Tool name as registered in the MCP protocol (e.g. "ctx_tree").
    fn name(&self) -> &'static str;

    /// MCP tool definition including JSON schema. This replaces the
    /// corresponding entry in `granular_tool_defs()`.
    fn tool_def(&self) -> Tool;

    /// Execute the tool. Args are the raw JSON-RPC arguments.
    /// `ctx` provides access to resolved paths and project state.
    fn handle(&self, args: &Map<String, Value>, ctx: &ToolContext)
    -> Result<ToolOutput, ErrorData>;

    /// Whether *this* invocation yields a machine-readable payload (e.g. JSON)
    /// that must reach the client byte-exact and parseable. When `true`, the
    /// dispatch pipeline suppresses every human-oriented decoration
    /// (auto-context briefing, verify footer, hints, checkpoints, deprecation
    /// notices) and terse compression for this call, returning the pure body.
    /// Keyed on `args` because most tools emit machine-readable output only for
    /// an opt-in format flag (e.g. `format=json`); defaults to `false` (#990).
    fn produces_machine_readable(&self, _args: Option<&Map<String, Value>>) -> bool {
        false
    }
}

/// Context passed to tool handlers. Contains pre-resolved values that
/// many tools need, avoiding repeated async lock acquisition inside
/// handlers. Extended with shared server state for tools that need
/// cache/session access.
///
/// `Clone` exists for per-op delegation (#1088): a batch op with its own
/// `path` clones the ctx and swaps in that op's resolved path.
#[derive(Clone)]
pub struct ToolContext {
    pub project_root: String,
    /// Session-scoped trusted roots (MCP `roots/list`, config `extra_roots`),
    /// snapshotted from the session so sync handlers can honor them without an
    /// async lock. Empty = single-root jail behaviour (#403).
    pub extra_roots: Vec<String>,
    pub minimal: bool,
    /// Pre-resolved paths keyed by argument name (e.g. "path" -> "/abs/dir").
    pub resolved_paths: std::collections::HashMap<String, String>,
    /// CRP mode for compression-aware tools.
    pub crp_mode: crate::tools::CrpMode,
    /// Shared cache handle for tools that need read/write access.
    pub cache: Option<crate::tools::SharedCache>,
    /// Shared session handle for tools that need session access.
    pub session: Option<std::sync::Arc<tokio::sync::RwLock<crate::core::session::SessionState>>>,
    /// Tool call records for session-aware tools (e.g. ctx_session status).
    pub tool_calls:
        Option<std::sync::Arc<tokio::sync::RwLock<Vec<crate::core::protocol::ToolCallRecord>>>>,
    /// Current agent identity for multi-agent tools.
    pub agent_id: Option<std::sync::Arc<tokio::sync::RwLock<Option<String>>>>,
    /// Active workflow run state.
    pub workflow:
        Option<std::sync::Arc<tokio::sync::RwLock<Option<crate::core::workflow::WorkflowRun>>>>,
    /// Context ledger for handoff operations.
    pub ledger:
        Option<std::sync::Arc<tokio::sync::RwLock<crate::core::context_ledger::ContextLedger>>>,
    /// Client name (cursor, claude, etc.).
    pub client_name: Option<std::sync::Arc<tokio::sync::RwLock<String>>>,
    /// Optional MCP client role supplied by the session/request context.
    /// Absent preserves the backward-compatible permissive default.
    pub client_role: Option<String>,
    /// Optional shell permission supplied by the session/request context.
    /// `None` preserves the backward-compatible permissive default.
    pub shell_access: Option<bool>,
    /// Pipeline stats for metrics/proof tools.
    pub pipeline_stats:
        Option<std::sync::Arc<tokio::sync::RwLock<crate::core::pipeline::PipelineStats>>>,
    /// Global call counter for context tools.
    pub call_count: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    /// Autonomy state for search repeat detection.
    pub autonomy: Option<std::sync::Arc<crate::core::autonomy::AutonomyState>>,
    /// Pre-computed context pressure snapshot for synchronous gate decisions.
    pub pressure_snapshot: Option<crate::core::context_ledger::ContextPressure>,
    /// Errors from path resolution (PathJail rejection, secret path, etc.).
    /// Keyed by argument name (e.g. "path" -> "path escapes project root: ...").
    pub path_errors: std::collections::HashMap<String, String>,
    /// Shared in-memory BM25 index cache for semantic search.
    pub bm25_cache: Option<crate::core::bm25_cache::SharedBm25Cache>,
    /// MCP progress notification sender for long-running operations.
    pub progress_sender: Option<crate::server::progress::SharedProgressSender>,
}

impl Default for ToolContext {
    /// Minimal context: `project_root` empty, all shared-state handles `None`.
    /// Single source for the one-shot CLI ctx (`cli::call_cmd::oneshot_ctx`) and
    /// the `empty_ctx` test helper — adding a field no longer breaks either.
    fn default() -> Self {
        Self {
            project_root: String::new(),
            extra_roots: Vec::new(),
            minimal: false,
            resolved_paths: std::collections::HashMap::new(),
            crp_mode: crate::tools::CrpMode::Off,
            cache: None,
            session: None,
            tool_calls: None,
            agent_id: None,
            workflow: None,
            ledger: None,
            client_name: None,
            client_role: None,
            shell_access: None,
            pipeline_stats: None,
            call_count: None,
            autonomy: None,
            pressure_snapshot: None,
            path_errors: std::collections::HashMap::new(),
            bm25_cache: None,
            progress_sender: None,
        }
    }
}

impl ToolContext {
    pub fn resolved_path(&self, arg: &str) -> Option<&str> {
        self.resolved_paths.get(arg).map(String::as_str)
    }

    /// Returns the path resolution error for a given key, if any.
    pub fn path_error(&self, key: &str) -> Option<&str> {
        self.path_errors.get(key).map(String::as_str)
    }

    /// Sync path resolution using `project_root` + session `extra_roots`. Thin
    /// wrapper over [`crate::core::path_resolve::resolve_tool_path_with_roots`]
    /// for sync tool handlers.
    pub fn resolve_path_sync(&self, path: &str) -> Result<String, String> {
        crate::core::path_resolve::resolve_tool_path_with_roots(
            Some(&self.project_root),
            None,
            path,
            &self.extra_roots,
        )
    }

    /// Default-deny write gate for the read-only tier (#475). Write-capable tool
    /// handlers must call this with an already-resolved absolute path before
    /// touching the filesystem; it errors if the path is inside a configured
    /// `read_only_roots` subtree. A no-op (always `Ok`) when no read-only roots
    /// are configured, so non-users pay nothing. Thin wrapper over the single
    /// choke point [`crate::core::pathjail::enforce_writable`] — the low-level
    /// atomic writers call the same function, so this is the ergonomic,
    /// early-error layer, not the only line of defence.
    pub fn ensure_writable(&self, resolved_path: &str) -> Result<(), String> {
        crate::core::pathjail::enforce_writable(std::path::Path::new(resolved_path))
    }
}

// ── Arg extraction helpers (mirror server/helpers.rs for standalone use) ──

/// Extract a resolved path from context with differentiated error messages.
/// Returns descriptive errors for: missing param, PathJail rejection, wrong type.
pub fn require_resolved_path(
    ctx: &ToolContext,
    args: &Map<String, Value>,
    key: &str,
) -> Result<String, ErrorData> {
    if let Some(path) = ctx.resolved_path(key) {
        return Ok(path.to_string());
    }
    if let Some(err) = ctx.path_error(key) {
        return Err(ErrorData::invalid_params(format!("{key}: {err}"), None));
    }
    if let Some(val) = args.get(key)
        && !val.is_string()
    {
        let type_name = match val {
            Value::Number(_) => "number",
            Value::Bool(_) => "boolean",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
            Value::Null => "null",
            Value::String(_) => unreachable!(),
        };
        return Err(ErrorData::invalid_params(
            format!("{key} must be a string, got {type_name}"),
            None,
        ));
    }
    Err(ErrorData::invalid_params(
        format!("{key} is required"),
        None,
    ))
}

pub fn get_str(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(String::from)
}

pub fn get_int(args: &Map<String, Value>, key: &str) -> Option<i64> {
    args.get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
}

/// Read a non-negative integer argument as `usize`.
///
/// Returns `None` for missing or negative values. This avoids the
/// `negative_i64 as usize` wrap to `usize::MAX`, which previously let an agent
/// trigger unbounded allocations (e.g. `top_k`, `limit`, `max_results`) → OOM.
/// Callers should still apply a sensible upper cap on the result.
pub fn get_usize(args: &Map<String, Value>, key: &str) -> Option<usize> {
    get_int(args, key).and_then(|n| usize::try_from(n).ok())
}

pub fn get_bool(args: &Map<String, Value>, key: &str) -> Option<bool> {
    args.get(key).and_then(serde_json::Value::as_bool)
}

pub fn get_f64(args: &Map<String, Value>, key: &str) -> Option<f64> {
    args.get(key).and_then(serde_json::Value::as_f64)
}

pub fn get_str_array(args: &Map<String, Value>, key: &str) -> Option<Vec<String>> {
    args.get(key).and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn empty_ctx() -> ToolContext {
        ToolContext::default()
    }

    #[test]
    fn require_resolved_path_returns_resolved() {
        let mut ctx = empty_ctx();
        ctx.resolved_paths
            .insert("path".to_string(), "/abs/file.rs".to_string());
        let args: Map<String, Value> = Map::new();
        let result = require_resolved_path(&ctx, &args, "path");
        assert_eq!(result.unwrap(), "/abs/file.rs");
    }

    #[test]
    fn require_resolved_path_surfaces_jail_error() {
        let mut ctx = empty_ctx();
        ctx.path_errors.insert(
            "path".to_string(),
            "path escapes project root /project".to_string(),
        );
        let args: Map<String, Value> = Map::new();
        let result = require_resolved_path(&ctx, &args, "path");
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("escapes project root"), "got: {msg}");
    }

    #[test]
    fn require_resolved_path_detects_non_string() {
        let ctx = empty_ctx();
        let mut args: Map<String, Value> = Map::new();
        args.insert("path".to_string(), json!(42));
        let result = require_resolved_path(&ctx, &args, "path");
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("must be a string, got number"), "got: {msg}");
    }

    #[test]
    fn require_resolved_path_missing_param() {
        let ctx = empty_ctx();
        let args: Map<String, Value> = Map::new();
        let result = require_resolved_path(&ctx, &args, "path");
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("path is required"), "got: {msg}");
    }

    #[test]
    fn get_int_coerces_string_to_number() {
        let mut args = Map::new();
        args.insert("n".into(), json!("42"));
        assert_eq!(super::get_int(&args, "n"), Some(42));
    }

    #[test]
    fn get_int_native_number_still_works() {
        let mut args = Map::new();
        args.insert("n".into(), json!(7));
        assert_eq!(super::get_int(&args, "n"), Some(7));
    }

    #[test]
    fn get_int_invalid_string_returns_none() {
        let mut args = Map::new();
        args.insert("n".into(), json!("not_a_number"));
        assert_eq!(super::get_int(&args, "n"), None);
    }

    #[test]
    fn get_usize_coerces_string() {
        let mut args = Map::new();
        args.insert("n".into(), json!("100"));
        assert_eq!(super::get_usize(&args, "n"), Some(100));
    }

    #[test]
    fn get_usize_negative_string_returns_none() {
        let mut args = Map::new();
        args.insert("n".into(), json!("-5"));
        assert_eq!(super::get_usize(&args, "n"), None);
    }
}
