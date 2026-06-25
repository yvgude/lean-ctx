//! The embedding [`Engine`] — a safe, ergonomic handle over the lean-ctx
//! context engine, built via [`EngineBuilder`].
//!
//! ## What it is
//!
//! `Engine` owns a **shared** [`SessionCache`] and dispatches the *real*
//! registered tools (`ctx_read`, `ctx_search`, …) the same way the MCP server
//! does. Because the cache is shared across calls, a read followed by a re-read
//! of the same file collapses to a delta in-process — the property Lean-md
//! needs and the headline acceptance test for this SDK.
//!
//! ```no_run
//! let engine = lean_ctx_sdk::Engine::builder(".").build().unwrap();
//! let first = engine.read("src/main.rs", lean_ctx_sdk::ReadMode::Full).unwrap();
//! let again = engine.read("src/main.rs", lean_ctx_sdk::ReadMode::Full).unwrap();
//! assert!(again.saved_tokens >= first.saved_tokens); // re-read is cheaper
//! ```
//!
//! ## Safe by default
//!
//! [`EngineBuilder::build`] is read-mostly and scoped:
//! - **`PathJail` on** — every path argument is resolved against the project root;
//!   escapes and secret paths are rejected.
//! - **Scoped data dir** — unless you call [`EngineBuilder::data_dir`], engine
//!   state goes to a dedicated temp dir, never your real `~/.lean-ctx`.
//! - **Auto-update off** for the embedded process.
//! - **Write/exec gated** — `ctx_edit`/`ctx_fill` need [`EngineBuilder::allow_write`];
//!   `ctx_shell`/`ctx_execute` need [`EngineBuilder::allow_exec`].
//!
//! ## Runtime constraint
//!
//! Engine methods are synchronous and drive their own multi-threaded Tokio
//! runtime, so they must **not** be called from inside another Tokio runtime
//! worker. From async code, wrap calls in `tokio::task::spawn_blocking`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};
use tokio::sync::RwLock;

use lean_ctx::core::cache::SessionCache;
use lean_ctx::core::session::SessionState;
use lean_ctx::server::registry::{ToolRegistry, build_registry};
use lean_ctx::server::tool_trait::{ToolContext, ToolOutput};
use lean_ctx::tools::{CrpMode, SharedCache};

use crate::error::Error;
use crate::output::Output;
use crate::read::ReadMode;

/// File-mutating tools, gated behind [`EngineBuilder::allow_write`].
const WRITE_TOOLS: &[&str] = &["ctx_edit", "ctx_fill"];
/// Command-executing tools, gated behind [`EngineBuilder::allow_exec`].
const EXEC_TOOLS: &[&str] = &["ctx_shell", "ctx_execute", "shell"];

/// Builds an [`Engine`] with explicit, safe-by-default configuration.
#[derive(Debug, Clone)]
pub struct EngineBuilder {
    project_root: PathBuf,
    data_dir: Option<PathBuf>,
    allow_write: bool,
    allow_exec: bool,
    worker_threads: usize,
}

impl EngineBuilder {
    fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            data_dir: None,
            allow_write: false,
            allow_exec: false,
            worker_threads: 2,
        }
    }

    /// Scope engine state (sessions, caches, indexes) to `dir` instead of the
    /// default throwaway temp dir. Pass your real lean-ctx data dir to share
    /// session memory with the CLI/MCP server.
    #[must_use]
    pub fn data_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.data_dir = Some(dir.into());
        self
    }

    /// Permit file-mutating tools (`ctx_edit`, `ctx_fill`) via [`Engine::call`].
    #[must_use]
    pub fn allow_write(mut self, yes: bool) -> Self {
        self.allow_write = yes;
        self
    }

    /// Permit command-executing tools (`ctx_shell`, `ctx_execute`) via
    /// [`Engine::call`]. The OS sandbox still applies when distributed as an addon.
    #[must_use]
    pub fn allow_exec(mut self, yes: bool) -> Self {
        self.allow_exec = yes;
        self
    }

    /// Number of Tokio worker threads backing the engine (default 2, min 1).
    #[must_use]
    pub fn worker_threads(mut self, n: usize) -> Self {
        self.worker_threads = n.max(1);
        self
    }

    /// Finalize configuration and construct the [`Engine`].
    ///
    /// # Errors
    /// Returns [`Error::Init`] if the project root does not exist / is not a
    /// directory, or the runtime cannot be built.
    pub fn build(self) -> Result<Engine, Error> {
        let project_root = std::fs::canonicalize(&self.project_root).map_err(|e| {
            Error::Init(format!("project root {}: {e}", self.project_root.display()))
        })?;
        if !project_root.is_dir() {
            return Err(Error::Init(format!(
                "project root {} is not a directory",
                project_root.display()
            )));
        }
        let project_root = project_root.to_string_lossy().into_owned();

        configure_process_env(self.data_dir.as_deref());

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(self.worker_threads)
            .enable_all()
            .thread_name("lean-ctx-sdk")
            .build()
            .map_err(|e| Error::Init(format!("tokio runtime: {e}")))?;

        Ok(Engine {
            project_root,
            cache: Arc::new(RwLock::new(SessionCache::new())),
            session: Arc::new(RwLock::new(SessionState::new())),
            registry: build_registry(),
            crp_mode: CrpMode::Off,
            allow_write: self.allow_write,
            allow_exec: self.allow_exec,
            rt,
        })
    }
}

/// An embedded lean-ctx engine. Construct via [`Engine::builder`].
pub struct Engine {
    project_root: String,
    cache: SharedCache,
    session: Arc<RwLock<SessionState>>,
    registry: ToolRegistry,
    crp_mode: CrpMode,
    allow_write: bool,
    allow_exec: bool,
    rt: tokio::runtime::Runtime,
}

impl Engine {
    /// Start configuring an engine rooted at `project_root` (the `PathJail` root).
    #[must_use]
    pub fn builder(project_root: impl Into<PathBuf>) -> EngineBuilder {
        EngineBuilder::new(project_root)
    }

    /// The resolved, absolute project root this engine is jailed to.
    #[must_use]
    pub fn project_root(&self) -> &str {
        &self.project_root
    }

    /// Read a file through the engine, with compression + cache-delta applied.
    ///
    /// `path` may be relative to the project root or absolute inside it; it is
    /// `PathJail`-checked. A re-read of an unchanged file collapses to a delta.
    ///
    /// # Errors
    /// [`Error::Path`] for jail violations, [`Error::Tool`] if the read fails.
    pub fn read(&self, path: impl AsRef<str>, mode: ReadMode) -> Result<Output, Error> {
        let resolved = self.resolve(path.as_ref())?;
        let mut args = Map::new();
        args.insert("path".into(), Value::String(resolved.clone()));
        args.insert("mode".into(), Value::String(mode.to_string()));

        let mut ctx = self.base_ctx();
        ctx.resolved_paths.insert("path".into(), resolved);

        self.dispatch("ctx_read", args, ctx).map(Output::from)
    }

    /// Regex/literal code search. `subdir` (optional) narrows the search to a
    /// directory under the project root; `None` searches the whole root.
    ///
    /// # Errors
    /// [`Error::Path`] for jail violations, [`Error::Tool`] on failure.
    pub fn search(&self, pattern: &str, subdir: Option<&str>) -> Result<String, Error> {
        let mut args = Map::new();
        args.insert("pattern".into(), Value::String(pattern.to_string()));
        let mut ctx = self.base_ctx();
        if let Some(dir) = subdir {
            let resolved = self.resolve(dir)?;
            args.insert("path".into(), Value::String(resolved.clone()));
            ctx.resolved_paths.insert("path".into(), resolved);
        }
        self.dispatch("ctx_search", args, ctx).map(|o| o.text)
    }

    /// Locate a symbol definition by exact name across the project.
    ///
    /// # Errors
    /// [`Error::Tool`] on failure.
    pub fn symbol(&self, name: &str) -> Result<String, Error> {
        let mut args = Map::new();
        args.insert("name".into(), Value::String(name.to_string()));
        self.dispatch("ctx_symbol", args, self.base_ctx())
            .map(|o| o.text)
    }

    /// Structural outline (symbols/headings) of a single file.
    ///
    /// # Errors
    /// [`Error::Path`] for jail violations, [`Error::Tool`] on failure.
    pub fn outline(&self, path: &str) -> Result<String, Error> {
        let resolved = self.resolve(path)?;
        let mut args = Map::new();
        args.insert("path".into(), Value::String(resolved.clone()));
        let mut ctx = self.base_ctx();
        ctx.resolved_paths.insert("path".into(), resolved);
        self.dispatch("ctx_outline", args, ctx).map(|o| o.text)
    }

    /// Directory tree / repo map rooted at `subdir` (or the project root).
    ///
    /// # Errors
    /// [`Error::Path`] for jail violations, [`Error::Tool`] on failure.
    pub fn tree(&self, subdir: Option<&str>) -> Result<String, Error> {
        let mut args = Map::new();
        let mut ctx = self.base_ctx();
        if let Some(dir) = subdir {
            let resolved = self.resolve(dir)?;
            args.insert("path".into(), Value::String(resolved.clone()));
            ctx.resolved_paths.insert("path".into(), resolved);
        }
        self.dispatch("ctx_tree", args, ctx).map(|o| o.text)
    }

    /// Escape hatch: call any registered tool by name with raw JSON arguments.
    ///
    /// A string `path` argument is `PathJail`-resolved before dispatch. Write and
    /// exec tools require the matching builder opt-in.
    ///
    /// # Errors
    /// [`Error::NotPermitted`] when a gated tool is not enabled,
    /// [`Error::UnknownTool`] for an unregistered name, [`Error::Path`] for jail
    /// violations, [`Error::Tool`] on handler failure.
    pub fn call(&self, tool: &str, mut args: Map<String, Value>) -> Result<Output, Error> {
        if EXEC_TOOLS.contains(&tool) && !self.allow_exec {
            return Err(Error::NotPermitted(tool.to_string()));
        }
        if WRITE_TOOLS.contains(&tool) && !self.allow_write {
            return Err(Error::NotPermitted(tool.to_string()));
        }

        let mut ctx = self.base_ctx();
        if let Some(Value::String(raw)) = args.get("path").cloned() {
            let resolved = self.resolve(&raw)?;
            args.insert("path".into(), Value::String(resolved.clone()));
            ctx.resolved_paths.insert("path".into(), resolved);
        }
        self.dispatch(tool, args, ctx).map(Output::from)
    }

    /// `PathJail`-resolve a raw path against the project root.
    fn resolve(&self, raw: &str) -> Result<String, Error> {
        lean_ctx::core::path_resolve::resolve_tool_path_with_roots(
            Some(&self.project_root),
            None,
            raw,
            &[],
        )
        .map_err(Error::Path)
    }

    /// A [`ToolContext`] wired to the shared cache/session for this engine.
    fn base_ctx(&self) -> ToolContext {
        ToolContext {
            project_root: self.project_root.clone(),
            crp_mode: self.crp_mode,
            cache: Some(self.cache.clone()),
            session: Some(self.session.clone()),
            ..ToolContext::default()
        }
    }

    /// Run a synchronous tool handler exactly like the MCP server: on the
    /// blocking pool of a multi-threaded runtime, so the handlers' internal
    /// `block_in_place` / `Handle::block_on` calls are legal.
    fn dispatch(
        &self,
        tool: &str,
        args: Map<String, Value>,
        ctx: ToolContext,
    ) -> Result<ToolOutput, Error> {
        let handler = self
            .registry
            .get_arc(tool)
            .ok_or_else(|| Error::UnknownTool(tool.to_string()))?;
        let tool_owned = tool.to_string();

        let joined = self.rt.block_on(async move {
            tokio::task::spawn_blocking(move || handler.handle(&args, &ctx)).await
        });

        match joined {
            Ok(Ok(out)) => Ok(out),
            Ok(Err(e)) => Err(Error::tool(&tool_owned, e.message)),
            Err(_join) => Err(Error::Incomplete(tool_owned)),
        }
    }
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("project_root", &self.project_root)
            .field("allow_write", &self.allow_write)
            .field("allow_exec", &self.allow_exec)
            .finish_non_exhaustive()
    }
}

/// Apply safe-by-default, process-global engine settings exactly once.
///
/// lean-ctx reads its data location and update policy from environment
/// variables. We set them once per process so the first engine wins; embedders
/// that need different scoping should set these before constructing an engine.
fn configure_process_env(data_dir: Option<&Path>) {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let dir = data_dir.map_or_else(
            || std::env::temp_dir().join("lean-ctx-sdk"),
            Path::to_path_buf,
        );
        let _ = std::fs::create_dir_all(&dir);
        let dir = dir.as_os_str();
        // SAFETY: called once via `Once` during engine init, before the engine
        // spawns its runtime threads. No other thread is reading these vars yet.
        unsafe {
            std::env::set_var("LEAN_CTX_DATA_DIR", dir);
            std::env::set_var("LEAN_CTX_CONFIG_DIR", dir);
            std::env::set_var("LEAN_CTX_STATE_DIR", dir);
            std::env::set_var("LEAN_CTX_CACHE_DIR", dir);
            std::env::set_var("LEAN_CTX_NO_UPDATE_CHECK", "1");
        }
    });
}
