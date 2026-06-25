use std::path::Path;

/// JSON response bodies for dashboard mutation APIs (`/api/*` POST handlers).
#[must_use]
pub fn json_ok() -> String {
    r#"{"ok":true}"#.to_string()
}

#[must_use]
pub fn json_err(msg: &str) -> String {
    serde_json::json!({ "error": msg }).to_string()
}

#[must_use]
pub fn extract_query_param(qs: &str, key: &str) -> Option<String> {
    for pair in qs.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        if k == key {
            return Some(percent_decode_query_component(v));
        }
    }
    None
}

#[must_use]
pub fn percent_decode_query_component(s: &str) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() => {
                let h1 = (b[i + 1] as char).to_digit(16);
                let h2 = (b[i + 2] as char).to_digit(16);
                if let (Some(a), Some(d)) = (h1, h2) {
                    out.push(((a << 4) | d) as u8);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[must_use]
pub fn normalize_dashboard_demo_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let candidate = Path::new(trimmed);
    if candidate.is_absolute() || is_windows_absolute_path(trimmed) {
        return trimmed.to_string();
    }

    let mut p = trimmed;
    while p.starts_with("./") || p.starts_with(".\\") {
        p = &p[2..];
    }

    p.trim_start_matches(['\\', '/'])
        .replace('\\', std::path::MAIN_SEPARATOR_STR)
}

#[must_use]
pub fn is_windows_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
    {
        return true;
    }

    path.starts_with("\\\\") || path.starts_with("//")
}

pub fn detect_project_root_for_dashboard() -> String {
    if let Ok(explicit) = std::env::var("LEAN_CTX_DASHBOARD_PROJECT")
        && !explicit.trim().is_empty()
    {
        return promote_to_git_root(&explicit);
    }

    if let Some(session) = crate::core::session::SessionState::load_latest() {
        if let Some(root) = session.project_root.as_deref()
            && !root.trim().is_empty()
            && Path::new(root).is_dir()
        {
            if let Some(git_root) = git_root_for(root) {
                return git_root;
            }
            if is_real_project(root) {
                return root.to_string();
            }
            tracing::debug!(
                "[dashboard] session root '{root}' is not a recognized project, skipping"
            );
        }
        if let Some(cwd) = session.shell_cwd.as_deref()
            && !cwd.trim().is_empty()
            && Path::new(cwd).is_dir()
        {
            let r = crate::core::protocol::detect_project_root_or_cwd(cwd);
            return promote_to_git_root(&r);
        }
        if let Some(last) = session.files_touched.last()
            && !last.path.trim().is_empty()
        {
            let p_path = Path::new(&last.path);
            if let Some(parent) = p_path.parent()
                && parent.is_dir()
            {
                let p = parent.to_string_lossy().to_string();
                let r = crate::core::protocol::detect_project_root_or_cwd(&p);
                return promote_to_git_root(&r);
            }
        }
    }

    let cwd = std::env::current_dir()
        .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string());
    let r = crate::core::protocol::detect_project_root_or_cwd(&cwd);
    promote_to_git_root(&r)
}

fn is_real_project(path: &str) -> bool {
    let p = Path::new(path);
    // macOS TCC (#356): a launchd-standalone process must not stat under
    // ~/Documents/Desktop/Downloads — `is_dir`/marker probes would pop the
    // privacy prompt. Report "not a project" without touching the filesystem.
    if !crate::core::pathutil::may_probe_path(p) {
        return false;
    }
    if !p.is_dir() {
        return false;
    }
    const MARKERS: &[&str] = &[
        ".git",
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "requirements.txt",
        "pom.xml",
        "build.gradle",
        "CMakeLists.txt",
        ".lean-ctx.toml",
    ];
    MARKERS.iter().any(|m| p.join(m).exists())
}

fn promote_to_git_root(path: &str) -> String {
    git_root_for(path).unwrap_or_else(|| path.to_string())
}

fn git_root_for(path: &str) -> Option<String> {
    let mut p = Path::new(path);
    loop {
        let git = p.join(".git");
        if git.exists() {
            return Some(p.to_string_lossy().to_string());
        }
        p = p.parent()?;
    }
}
