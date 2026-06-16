use super::types::SessionState;
use crate::core::pathutil::has_project_marker;

pub(crate) fn normalize_loaded_session(mut session: SessionState) -> SessionState {
    if matches!(session.project_root.as_deref(), Some(r) if r.trim().is_empty()) {
        session.project_root = None;
    }
    if matches!(session.shell_cwd.as_deref(), Some(c) if c.trim().is_empty()) {
        session.shell_cwd = None;
    }

    if let (Some(root), Some(cwd)) = (&session.project_root, &session.shell_cwd) {
        let root_p = std::path::Path::new(root);
        let cwd_p = std::path::Path::new(cwd);

        // String check first: only an agent/temp root is ever repaired, so a
        // real project root is never stat-ed here. Probing persisted paths on
        // every session load tripped the macOS TCC prompt from the launchd
        // daemon (#356) — has_project_marker is additionally TCC-guarded.
        if is_agent_or_temp_dir(root_p) && !has_project_marker(root_p) && has_project_marker(cwd_p)
        {
            session.project_root = Some(cwd.clone());
        }
    }

    if session.compression_level.is_empty() {
        if session.terse_mode {
            session.compression_level = "lite".to_string();
        } else if let Some(env_level) = crate::core::config::CompressionLevel::from_env() {
            session.compression_level = env_level.label().to_string();
            session.terse_mode = env_level.is_active();
        } else {
            // Do NOT call active_profile() here — this function is called during
            // Config::load() → find_project_root() → SessionState::load_latest(),
            // and active_profile() → active_profile_name() would re-enter Config::load(),
            // causing an OnceLock reentrancy deadlock (#301).
            session.compression_level = "off".to_string();
        }
    } else if !session.terse_mode {
        let level =
            crate::core::config::CompressionLevel::from_str_label(&session.compression_level)
                .unwrap_or_default();
        session.terse_mode = level.is_active();
    }

    session
}

pub(crate) fn session_matches_project_root(
    session: &SessionState,
    target_root: &std::path::Path,
) -> bool {
    if let Some(root) = session.project_root.as_deref() {
        let root_path = resolve_for_match(std::path::Path::new(root));
        if root_path == target_root {
            return true;
        }
        if has_project_marker(&root_path) {
            return false;
        }
    }

    if let Some(cwd) = session.shell_cwd.as_deref() {
        let cwd_path = resolve_for_match(std::path::Path::new(cwd));
        return cwd_path == target_root || cwd_path.starts_with(target_root);
    }

    false
}

/// Resolves a persisted session path for root matching. Lexical comparison is
/// the common case (identical strings); `canonicalize` only matters when the
/// stored path reaches the same directory through a symlink. A launchd-owned
/// process must not canonicalize paths under `~/Documents` & co. — the stat
/// pops the macOS TCC prompt (#356) — so it falls back to the lexical path.
fn resolve_for_match(path: &std::path::Path) -> std::path::PathBuf {
    if crate::core::pathutil::may_probe_path(path) {
        crate::core::pathutil::safe_canonicalize_or_self(path)
    } else {
        path.to_path_buf()
    }
}

fn is_agent_or_temp_dir(dir: &std::path::Path) -> bool {
    let s = dir.to_string_lossy();
    s.contains("/.claude")
        || s.contains("/.codebuddy")
        || s.contains("/.codex")
        || s.contains("/var/folders/")
        || s.contains("/tmp/")
        || s.contains("\\.claude")
        || s.contains("\\.codebuddy")
        || s.contains("\\.codex")
        || s.contains("\\AppData\\Local\\Temp")
        || s.contains("\\Temp\\")
}
