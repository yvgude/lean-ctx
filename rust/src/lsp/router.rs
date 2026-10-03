//! Code-intelligence backend registry.
//!
//! Backends are cached per **(normalized project root, language)**, so a
//! long-lived daemon serving several repositories never answers project B with
//! a server initialized for project A.
//!
//! Locking is two-level: the registry lock only guards lookup/insert/evict and
//! is never held across backend I/O; each backend sits in its own slot mutex.
//! A slow or hung server therefore blocks only callers of that same
//! project+language (who need that server anyway), and concurrent first calls
//! for one key start exactly one server (single-flight).
//!
//! Lifecycle: a stale backend (IDE gone, server exited/crashed) is replaced
//! before use, and evicted after a failed call so the next call recovers. The
//! failed operation itself is never retried — `rename`/`move` have side
//! effects. Idle servers are shut down after the memory profile's TTL.

use lsp_types::Uri;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::backend::LspBackend;
use super::client::{LspClient, file_path_to_uri};
use super::config::{ResolvedServer, check_server_available, language_for_extension};
use super::jetbrains_backend::JetBrainsHttpBackend;
use super::port_discovery;

/// How often the idle reaper wakes up. Bounds how far past its TTL an idle
/// server may live; the TTL itself comes from the memory profile.
const REAP_INTERVAL: Duration = Duration::from_mins(1);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct BackendKey {
    project_root: String,
    language: &'static str,
}

impl BackendKey {
    fn new(project_root: &str, language: &'static str) -> Self {
        Self {
            project_root: crate::core::index_paths::normalize_project_root(project_root),
            language,
        }
    }
}

/// `None` until the first call for this key selects a backend.
type Slot = Arc<Mutex<Option<Box<dyn LspBackend>>>>;

struct Entry {
    slot: Slot,
    last_used: Instant,
}

static BACKENDS: std::sync::LazyLock<Mutex<HashMap<BackendKey, Entry>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// The registry holds no invariant a panicking holder could break mid-update
/// (single insert/remove calls), so a poisoned lock is safe to reuse.
fn registry() -> MutexGuard<'static, HashMap<BackendKey, Entry>> {
    BACKENDS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return format!("{}/{rest}", home.display());
    }
    path.to_string()
}

/// Selects a code-intelligence backend for `language` (§4.3).
///
/// Config `cfg.lsp[language]` (HashMap<String,String>):
///   - absent      → "auto" = B-first (JetBrains if reachable, else rust-analyzer)
///   - "auto"      → same as absent
///   - "jetbrains" → B only (error if the IDE is not reachable; no fallback)
///   - anything else → an explicit language-server binary path = A only
///
/// Reachability = live port file + pid alive + `/health` ping. On any miss in
/// "auto" mode we fall back to Backing A deterministically (one ~300ms timeout max).
///
/// Configuration is read for `project_root` itself, never for the process's
/// working directory: a daemon serving several repositories must select and
/// start the backend `project_root` asks for.
fn select_backend(
    language: &str,
    project_root: &str,
    policy: StartPolicy,
    start_timeout: Option<Duration>,
) -> Result<Box<dyn LspBackend>, String> {
    let cfg = crate::core::config::Config::load_for_project_root(project_root);
    let mode = cfg.lsp.get(language).map(String::as_str);

    let want_b = matches!(mode, None | Some("auto" | "jetbrains"));
    let b_only = mode == Some("jetbrains");

    if want_b {
        if let Some(pf) = port_discovery::read_port_file(project_root)
            && port_discovery::pid_alive(pf.pid)
            && port_discovery::health_ok(&pf)
        {
            return Ok(Box::new(JetBrainsHttpBackend::new(
                pf.port,
                pf.token,
                project_root.to_string(),
                pf.pid,
            )));
        }
        if b_only {
            return Err(format!(
                "LSP backend 'jetbrains' configured for '{language}' but the IDE is not reachable \
                 (no live port file / health check failed)"
            ));
        }
    }

    // A live IDE is attached to, never spawned; a language server is.
    if policy == StartPolicy::ReuseOnly {
        return Err(format!(
            "{NOT_RUNNING}: no running semantic backend for '{language}' in {project_root}"
        ));
    }

    // Backing A: a standalone language server.
    let server = standalone_server_with(&cfg, language, project_root)?;
    let root_uri = file_path_to_uri(project_root)?;
    let client = LspClient::start(&server, &root_uri, start_timeout)?;
    Ok(Box::new(client) as Box<dyn LspBackend>)
}

/// The standalone language server lean-ctx would start for `language` in
/// `project_root`: the binary configured as `[lsp] <language> = "<path>"`,
/// or else the one resolved for the project. Also answers status surfaces,
/// so they report what would actually run.
pub(crate) fn standalone_server(
    language: &str,
    project_root: &str,
) -> Result<ResolvedServer, String> {
    let cfg = crate::core::config::Config::load_for_project_root(project_root);
    standalone_server_with(&cfg, language, project_root)
}

fn standalone_server_with(
    cfg: &crate::core::config::Config,
    language: &str,
    project_root: &str,
) -> Result<ResolvedServer, String> {
    match cfg.lsp.get(language).map(String::as_str) {
        Some(custom) if !matches!(custom, "auto" | "jetbrains") => {
            let command = expand_tilde(custom);
            let command = if Path::new(&command).is_file() {
                command
            } else {
                super::config::find_runnable_server(&command)
                    .ok_or_else(|| {
                        format!(
                            "Configured language server '{command}' for '{language}' not found or not runnable"
                        )
                    })?
                    .to_string_lossy()
                    .into_owned()
            };
            Ok(super::config::configured_server(
                language,
                &command,
                Path::new(project_root),
            ))
        }
        _ => check_server_available(language, Path::new(project_root)),
    }
}

/// What a non-mutating look at the registry finds for a language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveIdentity {
    /// A backend is cached and idle; its identity.
    Known(String),
    /// No backend is cached for this project and language.
    NotRunning,
    /// A backend is cached but serving another call right now.
    Busy,
}

/// Identity of the backend cached for `file_path`'s language, without
/// creating a registry entry, refreshing its idle clock, waiting, or starting
/// anything — safe to call on every cache hit.
pub fn live_identity(file_path: &str, project_root: &str) -> LiveIdentity {
    let Some(language) = Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(language_for_extension)
    else {
        return LiveIdentity::NotRunning;
    };
    let key = BackendKey::new(project_root, language);
    let Some(slot) = registry().get(&key).map(|e| Arc::clone(&e.slot)) else {
        return LiveIdentity::NotRunning;
    };
    match slot.try_lock() {
        Ok(guard) => guard.as_ref().map_or(LiveIdentity::NotRunning, |b| {
            LiveIdentity::Known(b.backend_info().identity())
        }),
        Err(std::sync::TryLockError::WouldBlock) => LiveIdentity::Busy,
        Err(std::sync::TryLockError::Poisoned(_)) => LiveIdentity::NotRunning,
    }
}

/// Returns the slot for `key` (creating an empty one) and marks it used.
fn slot_for(key: &BackendKey) -> Slot {
    let now = Instant::now();
    let mut reg = registry();
    let entry = reg.entry(key.clone()).or_insert_with(|| Entry {
        slot: Arc::default(),
        last_used: now,
    });
    entry.last_used = now;
    Arc::clone(&entry.slot)
}

/// Makes `slot` hold a usable backend: a stale one is dropped, an empty slot
/// is filled by `select`. A healthy cached backend is reused as-is.
fn ensure_backend<'a>(
    slot: &'a mut Option<Box<dyn LspBackend>>,
    project_root: &str,
    select: impl FnOnce() -> Result<Box<dyn LspBackend>, String>,
) -> Result<&'a mut (dyn LspBackend + 'static), String> {
    if slot.as_ref().is_some_and(|b| b.is_stale(project_root)) {
        *slot = None;
    }
    if slot.is_none() {
        *slot = Some(select()?);
    }
    slot.as_deref_mut()
        .ok_or_else(|| "LSP backend slot unexpectedly empty".to_string())
}

/// Error prefix of a [`StartPolicy::ReuseOnly`] call that found nothing running.
pub const NOT_RUNNING: &str = "SEMANTIC_BACKEND_NOT_RUNNING";

/// Whether a call may start a language server that is not running yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartPolicy {
    /// Start one on demand (interactive tools such as `ctx_refactor`).
    Lazy,
    /// Use only a backend that is already warm in this process or a live IDE.
    /// Background work uses this so it never spawns a heavyweight server.
    ReuseOnly,
}

pub fn with_backend<F, R>(file_path: &str, project_root: &str, f: F) -> Result<R, String>
where
    F: FnOnce(&mut dyn LspBackend, &str) -> Result<R, String>,
{
    with_backend_policy(file_path, project_root, StartPolicy::Lazy, f)
}

pub fn with_backend_policy<F, R>(
    file_path: &str,
    project_root: &str,
    policy: StartPolicy,
    f: F,
) -> Result<R, String>
where
    F: FnOnce(&mut dyn LspBackend, &str) -> Result<R, String>,
{
    with_backend_opts(file_path, project_root, policy, BackendOpts::INTERACTIVE, f)
}

/// Error prefix of a non-waiting call that found the backend busy.
pub const BUSY: &str = "SEMANTIC_BACKEND_BUSY";

/// How a call may wait on the backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendOpts {
    /// `false`: a backend serving another call (e.g. an interactive
    /// `ctx_refactor`) is not waited for — fail fast with [`BUSY`].
    pub wait: bool,
    /// Bound for starting a server, if one must be started (`None` = the
    /// server's default `initialize` timeout).
    pub start_timeout: Option<Duration>,
}

impl BackendOpts {
    /// Interactive tools: wait for the backend, default start-up time.
    pub const INTERACTIVE: Self = Self {
        wait: true,
        start_timeout: None,
    };
}

/// Like [`with_backend_policy`], with explicit waiting / start-up limits.
/// Opportunistic semantic work never waits and bounds start-up by its own
/// remaining budget, so it can neither delay interactive tools nor overrun.
pub fn with_backend_opts<F, R>(
    file_path: &str,
    project_root: &str,
    policy: StartPolicy,
    opts: BackendOpts,
    f: F,
) -> Result<R, String>
where
    F: FnOnce(&mut dyn LspBackend, &str) -> Result<R, String>,
{
    let ext = Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let language = language_for_extension(ext).ok_or_else(|| {
        format!(
            "No LSP server configured for extension '.{ext}'. Supported: rs, ts, tsx, js, py, go"
        )
    })?;

    let slot = slot_for(&BackendKey::new(project_root, language));
    let locked = if opts.wait {
        slot.lock().map_err(std::sync::TryLockError::Poisoned)
    } else {
        slot.try_lock()
    };
    let mut guard = match locked {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::WouldBlock) => {
            return Err(format!(
                "{BUSY}: '{language}' backend for {project_root} is serving another call"
            ));
        }
        // A previous call panicked mid-request: the server's protocol state is
        // unknown, so discard it and let this call start a fresh one.
        Err(std::sync::TryLockError::Poisoned(poisoned)) => {
            slot.clear_poison();
            let mut guard = poisoned.into_inner();
            *guard = None;
            guard
        }
    };

    let backend = ensure_backend(&mut guard, project_root, || {
        let backend = select_backend(language, project_root, policy, opts.start_timeout)?;
        start_idle_reaper();
        Ok(backend)
    })?;
    // A backend is live here: let `auto` mode put it to use for the graph
    // (rate-limited; runs on its own thread once this call is done).
    crate::core::graph_enricher::schedule_semantic_refresh(project_root);
    let result = f(backend, language);

    // The server died during the call: evict it so the next call recovers.
    if result.is_err()
        && guard
            .as_ref()
            .is_some_and(|b| b.is_dead_after_error(project_root))
    {
        *guard = None;
    }
    result
}

pub fn open_file(file_path: &str, project_root: &str) -> Result<Uri, String> {
    let ext = Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    language_for_extension(ext).ok_or_else(|| {
        format!(
            "No LSP server configured for extension '.{ext}'. Supported: rs, ts, tsx, js, py, go"
        )
    })?;

    let content = std::fs::read_to_string(file_path)
        .map_err(|e| format!("Cannot read '{file_path}': {e}"))?;

    let uri = file_path_to_uri(file_path)?;

    with_backend(file_path, project_root, |backend, language| {
        backend.open_file(&uri, language, &content)?;
        Ok(uri.clone())
    })
}

/// Removes entries unused for at least `ttl` that no caller currently holds.
/// They are returned rather than dropped so server shutdown (up to a few
/// seconds each) runs after the registry lock is released.
fn take_idle(now: Instant, ttl: Duration) -> Vec<Entry> {
    let mut reg = registry();
    let idle: Vec<BackendKey> = reg
        .iter()
        .filter(|(_, e)| {
            now.saturating_duration_since(e.last_used) >= ttl && Arc::strong_count(&e.slot) == 1
        })
        .map(|(key, _)| key.clone())
        .collect();
    idle.iter().filter_map(|key| reg.remove(key)).collect()
}

/// Started on the first backend selection; one thread for the process.
fn start_idle_reaper() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("lsp-idle-reaper".into())
            .spawn(|| {
                loop {
                    std::thread::sleep(REAP_INTERVAL);
                    let cfg = crate::core::config::Config::load();
                    let ttl = crate::core::config::MemoryProfile::effective(&cfg).lsp_idle_ttl();
                    drop(take_idle(Instant::now(), ttl));
                }
            });
    });
}

/// Whether any semantic backend is live for `project_root` right now: a warm
/// language server in this process, or a reachable JetBrains IDE (port file
/// with a live pid — no HTTP). Never blocks on a busy backend (busy = live).
pub fn has_live_backend(project_root: &str) -> bool {
    let root = crate::core::index_paths::normalize_project_root(project_root);
    let warm = registry().iter().any(|(key, entry)| {
        key.project_root == root
            && match entry.slot.try_lock() {
                Ok(slot) => slot.is_some(),
                Err(std::sync::TryLockError::WouldBlock) => true,
                Err(std::sync::TryLockError::Poisoned(_)) => false,
            }
    });
    warm || port_discovery::read_port_file(project_root)
        .is_some_and(|pf| port_discovery::pid_alive(pf.pid))
}

/// Whether any backend of `project_root` is serving a call right now — a
/// non-blocking registry peek.
pub fn backend_busy(project_root: &str) -> bool {
    let root = crate::core::index_paths::normalize_project_root(project_root);
    let slots: Vec<Slot> = registry()
        .iter()
        .filter(|(key, _)| key.project_root == root)
        .map(|(_, e)| Arc::clone(&e.slot))
        .collect();
    slots
        .iter()
        .any(|s| matches!(s.try_lock(), Err(std::sync::TryLockError::WouldBlock)))
}

pub fn shutdown_all() {
    let drained: Vec<Entry> = registry().drain().map(|(_, entry)| entry).collect();
    drop(drained);
}

#[cfg(test)]
pub(crate) fn seed_stub_backend(
    project_root: &str,
    language: &'static str,
    backend: Box<dyn LspBackend>,
) {
    registry().insert(
        BackendKey::new(project_root, language),
        Entry {
            slot: Arc::new(Mutex::new(Some(backend))),
            last_used: Instant::now(),
        },
    );
}

/// Serializes tests that seed [`BACKENDS`] stubs: `BACKENDS` is process-global,
/// so two parallel tests seeding the same key would race (one stub overwrites
/// the other between seed and use). Hold this for the whole test.
#[cfg(test)]
pub(crate) fn stub_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::backend::Truncation;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn no_port_file_means_no_backing_b() {
        // With no IDE port file for an unlikely root, discovery yields None →
        // select_backend would deterministically fall through to Backing A.
        let pf = port_discovery::read_port_file("/nonexistent/leanctx/proj/xyz");
        assert!(pf.is_none(), "unexpected port file for nonexistent root");
    }

    /// Identifies itself via `last_truncation().total`; `dies_on_call` makes
    /// `references` fail and flip the backend stale, like a crashed server.
    struct Stub {
        id: u32,
        stale: AtomicBool,
        dies_on_call: bool,
    }

    fn stub(id: u32) -> Box<dyn LspBackend> {
        Box::new(Stub {
            id,
            stale: AtomicBool::new(false),
            dies_on_call: false,
        })
    }

    impl LspBackend for Stub {
        fn open_file(&mut self, _u: &Uri, _l: &str, _t: &str) -> Result<(), String> {
            Ok(())
        }
        fn references(
            &mut self,
            _u: &Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            if self.dies_on_call {
                self.stale.store(true, Ordering::Release);
                return Err("LSP server closed connection (EOF)".into());
            }
            Ok(vec![])
        }
        fn definition(
            &mut self,
            _u: &Uri,
            _p: lsp_types::Position,
        ) -> Result<lsp_types::GotoDefinitionResponse, String> {
            Ok(lsp_types::GotoDefinitionResponse::Array(vec![]))
        }
        fn implementations(
            &mut self,
            _u: &Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn rename(
            &mut self,
            _u: &Uri,
            _p: lsp_types::Position,
            _n: &str,
        ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
            Ok(None)
        }
        fn is_stale(&self, _project_root: &str) -> bool {
            self.stale.load(Ordering::Acquire)
        }
        fn last_truncation(&self) -> Option<Truncation> {
            Some(Truncation {
                truncated: false,
                total: self.id,
            })
        }
    }

    fn backend_id(root: &str) -> Result<Option<u32>, String> {
        with_backend(&format!("{root}/x.rs"), root, |b, _| {
            Ok(b.last_truncation().map(|t| t.total))
        })
    }

    #[test]
    fn backends_are_isolated_per_project_root() {
        let _lock = stub_test_lock();
        seed_stub_backend("/leanctx-router-test/a", "rust", stub(1));
        seed_stub_backend("/leanctx-router-test/b", "rust", stub(2));

        assert_eq!(backend_id("/leanctx-router-test/a"), Ok(Some(1)));
        assert_eq!(backend_id("/leanctx-router-test/b"), Ok(Some(2)));
        // Same project spelled with a trailing slash → same normalized key.
        assert_eq!(backend_id("/leanctx-router-test/a/"), Ok(Some(1)));
    }

    #[test]
    fn reuse_only_uses_a_warm_backend_and_never_starts_one() {
        let _lock = stub_test_lock();
        let warm = "/leanctx-router-test/warm";
        seed_stub_backend(warm, "rust", stub(5));
        let id = with_backend_policy(
            &format!("{warm}/x.rs"),
            warm,
            StartPolicy::ReuseOnly,
            |b, _| Ok(b.last_truncation().map(|t| t.total)),
        );
        assert_eq!(id, Ok(Some(5)));

        assert!(has_live_backend(warm));
        assert_eq!(
            live_identity(&format!("{warm}/x.rs"), warm),
            LiveIdentity::Known("lsp:unknown@unknown".into())
        );
        let held = Arc::clone(&registry()[&BackendKey::new(warm, "rust")].slot);
        let guard = held.lock().unwrap();
        assert_eq!(
            live_identity(&format!("{warm}/x.rs"), warm),
            LiveIdentity::Busy
        );
        assert!(backend_busy(warm));
        drop(guard);
        assert!(!backend_busy(warm));

        // A pure peek: it never creates a registry entry for an unseen root.
        let unseen = "/leanctx-router-test/unseen";
        assert_eq!(
            live_identity(&format!("{unseen}/x.rs"), unseen),
            LiveIdentity::NotRunning
        );
        assert!(!registry().contains_key(&BackendKey::new(unseen, "rust")));

        let cold = "/leanctx-router-test/cold";
        assert!(!has_live_backend(cold));
        let err = with_backend_policy(
            &format!("{cold}/x.rs"),
            cold,
            StartPolicy::ReuseOnly,
            |_, _| Ok(()),
        )
        .unwrap_err();
        assert!(err.starts_with(NOT_RUNNING), "got: {err}");
    }

    #[test]
    fn slow_backend_does_not_block_another_project() {
        let _lock = stub_test_lock();
        seed_stub_backend("/leanctx-router-test/slow", "rust", stub(1));
        seed_stub_backend("/leanctx-router-test/fast", "rust", stub(2));

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let slow = std::thread::spawn(move || {
            let root = "/leanctx-router-test/slow";
            with_backend(&format!("{root}/x.rs"), root, |_, _| {
                entered_tx.send(()).unwrap();
                // Released by the main thread only after its own call returned;
                // under a registry-wide lock that call would block until timeout.
                Ok(release_rx.recv_timeout(Duration::from_secs(10)).is_ok())
            })
        });

        entered_rx.recv().unwrap();
        assert_eq!(backend_id("/leanctx-router-test/fast"), Ok(Some(2)));
        release_tx.send(()).unwrap();
        assert_eq!(slow.join().unwrap(), Ok(true));
    }

    #[test]
    fn stale_backend_is_replaced_and_fresh_backend_reused() {
        let mut slot = Some(Box::new(Stub {
            id: 1,
            stale: AtomicBool::new(true),
            dies_on_call: false,
        }) as Box<dyn LspBackend>);
        let replaced = ensure_backend(&mut slot, "/any", || Ok(stub(7))).unwrap();
        assert_eq!(replaced.last_truncation().map(|t| t.total), Some(7));

        let reused = ensure_backend(&mut slot, "/any", || Err("must not reselect".into())).unwrap();
        assert_eq!(reused.last_truncation().map(|t| t.total), Some(7));
    }

    #[test]
    fn backend_that_dies_mid_call_is_evicted() {
        let _lock = stub_test_lock();
        let root = "/leanctx-router-test/dies";
        seed_stub_backend(
            root,
            "rust",
            Box::new(Stub {
                id: 1,
                stale: AtomicBool::new(false),
                dies_on_call: true,
            }),
        );
        let uri = file_path_to_uri(&format!("{root}/x.rs")).unwrap();
        let pos = lsp_types::Position {
            line: 0,
            character: 0,
        };
        let out = with_backend(&format!("{root}/x.rs"), root, |b, _| {
            b.references(&uri, pos, "project")
        });
        assert!(out.is_err());

        let slot = Arc::clone(&registry()[&BackendKey::new(root, "rust")].slot);
        assert!(
            slot.lock().unwrap().is_none(),
            "dead backend must be evicted"
        );
    }

    #[test]
    fn idle_reaper_takes_only_unused_entries() {
        let _lock = stub_test_lock();
        let idle = BackendKey::new("/leanctx-router-test/idle", "rust");
        let busy = BackendKey::new("/leanctx-router-test/busy", "rust");
        seed_stub_backend(&idle.project_root, "rust", stub(1));
        seed_stub_backend(&busy.project_root, "rust", stub(2));
        let in_use = slot_for(&busy);

        let later = Instant::now() + Duration::from_hours(1);
        drop(take_idle(later, Duration::from_mins(1)));

        let reg = registry();
        assert!(!reg.contains_key(&idle), "idle entry must be reaped");
        assert!(reg.contains_key(&busy), "entry held by a caller must stay");
        drop(reg);
        drop(in_use);
    }
}
