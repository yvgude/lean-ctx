use std::path::Path;

/// JSON response bodies for dashboard mutation APIs (`/api/*` POST handlers).
pub fn json_ok() -> String {
    r#"{"ok":true}"#.to_string()
}

pub fn json_err(msg: &str) -> String {
    serde_json::json!({ "error": msg }).to_string()
}

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

/// Whether a read asks for a fresh answer instead of a cached one: the
/// Cockpit appends `_refresh=1` when the user reloads or re-checks a view.
pub(crate) fn wants_refresh(qs: &str) -> bool {
    extract_query_param(qs, "_refresh").is_some_and(|v| !matches!(v.as_str(), "" | "0" | "false"))
}

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

    // #715: forward slashes always — ledger entries and overlay item ids use
    // the forward-slash canonical form, so emitting `\` on Windows made every
    // dashboard action miss its ledger entry.
    p.trim_start_matches(['\\', '/']).replace('\\', "/")
}

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

/// Where the dashboard's project binding came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardProjectSource {
    /// `--project` / `LEAN_CTX_DASHBOARD_PROJECT`.
    ExplicitFlag,
    /// `LEAN_CTX_PROJECT_ROOT`.
    Env,
    /// The real project the dashboard was started in.
    Cwd,
    /// This request's `X-LeanCTX-Project`, checked against the workspace
    /// pool (`crate::dashboard::project_binding`).
    Request,
    /// None of these: no project data is shown.
    Unbound,
}

/// The dashboard's project and the binding it came from. Never another
/// project's most recent session: a LaunchAgent dashboard started in `/` or
/// `~` is unbound rather than showing whichever project was active last
/// (AP04-G8).
///
/// The binding is the one an MCP session would make: an explicit
/// `--project` / `LEAN_CTX_PROJECT_ROOT` is taken as given (never promoted to
/// an enclosing checkout), and a folder the dashboard was started in is bound
/// by the MCP startup rule ([`crate::server::derive_project_root_from`]).
///
/// Every dashboard route reads its project through here. Inside a read that
/// named its project (`X-LeanCTX-Project`), the request-scoped project wins;
/// it is set per request, never process-wide.
pub fn dashboard_project_binding() -> (Option<String>, DashboardProjectSource) {
    if let Some(project) = request_project() {
        return (Some(project), DashboardProjectSource::Request);
    }
    dashboard_project_binding_in(std::env::current_dir().ok().as_deref())
}

/// The project bound to the current request, if any.
pub(crate) fn request_project() -> Option<String> {
    crate::core::policy::runtime::REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
        .map(|project| project.to_string_lossy().into_owned())
        .filter(|project| !project.is_empty())
}

/// The ledger share of the project this request names (`X-LeanCTX-Project`);
/// `None` when the read is for every project.
pub(crate) fn request_ledger_scope() -> Option<crate::core::savings_ledger::ProjectScope> {
    request_project()
        .map(|project| crate::core::savings_ledger::ProjectScope::for_project(Path::new(&project)))
}

/// Project key → project name for every project this dashboard may show,
/// named after its main checkout as the project switch names it.
pub(crate) fn project_names() -> std::collections::HashMap<String, String> {
    use crate::core::savings_ledger::project_scope::{project_key_of, project_root_of};
    crate::dashboard::project_binding::project_pool()
        .iter()
        .filter_map(|root| {
            let key = project_key_of(Path::new(root))?;
            let name = project_root_of(Path::new(root))
                .file_name()?
                .to_string_lossy()
                .into_owned();
            Some((key, name))
        })
        .collect()
}

pub(crate) fn dashboard_project_binding_in(
    cwd: Option<&Path>,
) -> (Option<String>, DashboardProjectSource) {
    if let Some(explicit) = crate::core::runtime_flags::dashboard_project() {
        return (
            Some(explicit.trim().to_string()),
            DashboardProjectSource::ExplicitFlag,
        );
    }

    if let Ok(explicit) = std::env::var("LEAN_CTX_PROJECT_ROOT")
        && !explicit.trim().is_empty()
    {
        return (
            Some(explicit.trim().to_string()),
            DashboardProjectSource::Env,
        );
    }

    if let Some(root) = cwd
        .filter(|cwd| !crate::core::pathutil::is_broad_or_unsafe_root(cwd))
        .and_then(crate::server::derive_project_root_from)
        .filter(|root| !crate::core::pathutil::is_broad_or_unsafe_root(Path::new(root)))
    {
        return (Some(root), DashboardProjectSource::Cwd);
    }
    (None, DashboardProjectSource::Unbound)
}

/// The dashboard's project root; empty when unbound.
pub fn detect_project_root_for_dashboard() -> String {
    dashboard_project_binding().0.unwrap_or_default()
}
