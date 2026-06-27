use super::server::LeanCtxServer;

#[derive(Clone, Debug, Default)]
pub(super) struct StartupContext {
    pub(super) project_root: Option<String>,
    pub(super) shell_cwd: Option<String>,
}

/// Creates a new `LeanCtxServer` with default configuration.
pub fn create_server() -> LeanCtxServer {
    LeanCtxServer::new()
}

pub(super) fn has_project_marker(dir: &std::path::Path) -> bool {
    crate::core::pathutil::has_project_marker(dir)
}

pub(super) fn is_suspicious_root(dir: &std::path::Path) -> bool {
    crate::core::pathutil::is_agent_config_dir(dir)
}

pub(super) fn canonicalize_path(path: &std::path::Path) -> String {
    crate::core::pathutil::safe_canonicalize_or_self(path)
        .to_string_lossy()
        .to_string()
}

pub(super) fn detect_startup_context(
    explicit_project_root: Option<&str>,
    startup_cwd: Option<&std::path::Path>,
) -> StartupContext {
    let shell_cwd = startup_cwd.map(canonicalize_path);
    let project_root = explicit_project_root
        .map(|root| canonicalize_path(std::path::Path::new(root)))
        .or_else(|| {
            startup_cwd
                .and_then(maybe_derive_project_root_from_absolute)
                .map(|p| canonicalize_path(&p))
        });

    let shell_cwd = match (shell_cwd, project_root.as_ref()) {
        (Some(cwd), Some(root))
            if std::path::Path::new(&cwd).starts_with(std::path::Path::new(root)) =>
        {
            Some(cwd)
        }
        (_, Some(root)) => Some(root.clone()),
        (cwd, None) => cwd,
    };

    StartupContext {
        project_root,
        shell_cwd,
    }
}

pub(super) fn maybe_derive_project_root_from_absolute(
    abs: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let mut cur = if abs.is_dir() {
        abs.to_path_buf()
    } else {
        abs.parent()?.to_path_buf()
    };
    loop {
        if has_project_marker(&cur) {
            return Some(crate::core::pathutil::safe_canonicalize_or_self(&cur));
        }
        if !cur.pop() {
            break;
        }
    }
    None
}

/// Incremental background consolidation: import only session items newer than the
/// per-session watermark, advancing it after a successful save. Delegates to the
/// canonical engine ([`crate::tools::ctx_knowledge::consolidate_project_knowledge_with`]),
/// which loads the session for the *requested* project root (cwd bug #2362), runs
/// under the shared knowledge lock (#326) and reclaims history losslessly (#995).
pub(crate) fn auto_consolidate_knowledge(project_root: &str) {
    let _ = crate::tools::ctx_knowledge::consolidate_project_knowledge_with(
        project_root,
        &crate::core::consolidation_engine::ConsolidateOptions::incremental_auto(),
    );
}
