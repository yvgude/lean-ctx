use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

static RAW: AtomicBool = AtomicBool::new(false);
static COMPRESS: AtomicBool = AtomicBool::new(false);
static QUIET: AtomicBool = AtomicBool::new(false);
static MCP_SERVER: AtomicBool = AtomicBool::new(false);
static HOOK_CHILD: AtomicBool = AtomicBool::new(false);
static DASHBOARD_PROJECT: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static DASHBOARD_SERVER: AtomicBool = AtomicBool::new(false);
static ALLOW_PATHS: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();

pub struct FlagGuard {
    flag: &'static AtomicBool,
    previous: bool,
}

impl Drop for FlagGuard {
    fn drop(&mut self) {
        self.flag.store(self.previous, Ordering::Relaxed);
    }
}

fn set_scoped(flag: &'static AtomicBool) -> FlagGuard {
    let previous = flag.swap(true, Ordering::Relaxed);
    FlagGuard { flag, previous }
}

pub fn enable_raw() {
    RAW.store(true, Ordering::Relaxed);
}

pub fn enable_compress() {
    COMPRESS.store(true, Ordering::Relaxed);
}

pub fn enable_mcp_server() {
    MCP_SERVER.store(true, Ordering::Relaxed);
}

pub fn mark_dashboard_server() {
    DASHBOARD_SERVER.store(true, Ordering::Relaxed);
}

pub(crate) fn is_dashboard_server() -> bool {
    DASHBOARD_SERVER.load(Ordering::Relaxed)
}

pub fn mark_hook_child() {
    HOOK_CHILD.store(true, Ordering::Relaxed);
}

pub fn scoped_quiet() -> FlagGuard {
    set_scoped(&QUIET)
}

pub fn set_dashboard_project(project: String) {
    let slot = DASHBOARD_PROJECT.get_or_init(|| Mutex::new(None));
    if let Ok(mut value) = slot.lock() {
        *value = Some(project);
    }
}

pub(crate) fn add_allow_paths(paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    let slot = ALLOW_PATHS.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut value) = slot.lock() {
        value.extend(paths);
    }
}

pub fn raw_enabled() -> bool {
    RAW.load(Ordering::Relaxed) || std::env::var("LEAN_CTX_RAW").is_ok()
}

pub(crate) fn compress_enabled() -> bool {
    COMPRESS.load(Ordering::Relaxed) || std::env::var("LEAN_CTX_COMPRESS").is_ok()
}

pub fn quiet_enabled() -> bool {
    QUIET.load(Ordering::Relaxed)
        || matches!(std::env::var("LEAN_CTX_QUIET"), Ok(value) if value.trim() == "1")
}

pub fn mcp_server_enabled() -> bool {
    MCP_SERVER.load(Ordering::Relaxed)
        || std::env::var("LEAN_CTX_MCP_SERVER").is_ok_and(|value| value == "1")
}

pub fn hook_child_enabled() -> bool {
    HOOK_CHILD.load(Ordering::Relaxed) || std::env::var("LEAN_CTX_HOOK_CHILD").is_ok()
}

pub fn dashboard_project() -> Option<String> {
    if let Some(value) = DASHBOARD_PROJECT
        .get()
        .and_then(|slot| slot.lock().ok().and_then(|value| value.clone()))
        && !value.trim().is_empty()
    {
        return Some(value);
    }
    std::env::var("LEAN_CTX_DASHBOARD_PROJECT")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

pub(crate) fn allow_paths() -> Vec<PathBuf> {
    ALLOW_PATHS
        .get()
        .and_then(|slot| slot.lock().ok().map(|value| value.clone()))
        .unwrap_or_default()
}

pub(crate) fn allow_path_enabled() -> bool {
    !allow_paths().is_empty()
        || std::env::var("LEAN_CTX_ALLOW_PATH").is_ok()
        || std::env::var("LCTX_ALLOW_PATH").is_ok()
}
