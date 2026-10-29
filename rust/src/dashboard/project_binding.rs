// SPDX-License-Identifier: Apache-2.0
//! Per-request project binding for dashboard reads (`X-LeanCTX-Project`).
//!
//! The Cockpit shows several projects from one dashboard. A read names its
//! project in the `X-LeanCTX-Project` header; the dashboard accepts it only
//! when it is a project this dashboard may show — its own binding or a
//! workspace in the `/api/workspaces` pool — and then answers that one read
//! inside a request-scoped project ([`crate::core::policy::runtime::REQUEST_PROJECT`]),
//! never by changing anything process-wide. Two concurrent reads for two
//! projects therefore cannot see each other's data.
//!
//! Validation follows the MCP root binding: an existing absolute directory,
//! no traversal, never a broad or unsafe root, compared after
//! canonicalisation. The read is bound to the pool's own spelling of the
//! root, so it lands in the same data partition
//! ([`crate::core::project_hash::hash_project_root`]) the agents wrote to.

use std::path::{Component, Path, PathBuf};

const MAX_PROJECT_HEADER: usize = 4096;

/// Reads answered for the project the request names (mirrors the Cockpit's
/// `READ_ROUTES`).
const PROJECT_READ_ROUTES: &[&str] = &[
    "/api/health",
    "/api/stats",
    "/api/session",
    "/api/agents",
    "/api/knowledge",
    "/api/knowledge-relations",
    "/api/context-ledger",
    "/api/context-control",
    "/api/context-overlay-history",
    "/api/context-plan",
    "/api/context-handles",
    "/api/context-risk",
    "/api/context-client",
    "/api/context-introspect",
    "/api/live",
    "/api/events",
    "/api/snapshots",
    "/api/snapshot",
    "/api/settings",
    "/api/doctor",
    "/api/roi",
    "/api/roi-report",
    "/api/memory",
    "/api/procedures",
    "/api/gotchas",
    "/api/search",
    "/api/provenance",
    "/api/value",
    "/api/owasp",
    "/api/gain",
    "/api/solution",
    "/api/verification",
    "/api/spend",
    "/api/signals",
    "/api/learning",
    "/api/context-triage",
    "/api/graph/stats",
    "/api/graph-files",
    "/api/graph",
    "/api/heatmap",
    "/api/call-graph",
    "/api/call-graph/status",
    "/api/symbols",
    "/api/episodes",
    "/api/workspaces",
    "/api/routes",
    "/api/version",
    "/api/buddy",
    "/api/kernel",
    "/api/workspace-health",
    "/api/scope",
];

/// Older names the Cockpit still sends.
const ROUTE_ALIASES: &[(&str, &str)] = &[("/api/context-overlays", "/api/context-overlay-history")];

/// The engine route that answers `path` (aliases resolved).
pub(crate) fn canonical_route(path: &str) -> &str {
    ROUTE_ALIASES
        .iter()
        .find_map(|(alias, route)| (*alias == path).then_some(*route))
        .unwrap_or(path)
}

/// The Cockpit's analysis bridge (`routes::analysis_bridge`): POSTs that
/// collect and apply one analysis for the project the UI selected. Without
/// the request's project they bind nothing when the dashboard was started
/// outside a project, and every analysis page fails.
const PROJECT_ANALYSIS_ROUTES: &[&str] = &[
    "/api/dashboard-analysis/inputs",
    "/api/dashboard-analysis/apply",
];

/// Whether a request for `route` (already canonical) is answered for the
/// project it names.
pub(crate) fn is_project_read(route: &str, method: &str) -> bool {
    ((method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD"))
        && PROJECT_READ_ROUTES.contains(&route))
        || (method.eq_ignore_ascii_case("POST") && PROJECT_ANALYSIS_ROUTES.contains(&route))
}

/// The projects this dashboard may show: its own binding and every
/// workspace an agent session is bound to.
pub(crate) fn project_pool() -> Vec<String> {
    let mut pool: Vec<String> = Vec::new();
    if let (Some(own), _) = super::routes::helpers::dashboard_project_binding() {
        pool.push(own);
    }
    for summary in crate::core::session::SessionState::list_sessions() {
        if let Some(root) = summary.project_root.filter(|root| !root.is_empty())
            && !pool.contains(&root)
        {
            pool.push(root);
        }
    }
    pool
}

fn canonical_project_dir(path: &Path) -> Option<PathBuf> {
    let canonical = crate::core::pathutil::safe_canonicalize(path).ok()?;
    (canonical.is_dir() && !crate::core::pathutil::is_broad_or_unsafe_root(&canonical))
        .then_some(canonical)
}

/// The pool root `requested` names, or `None` when it names no project this
/// dashboard may show (the caller answers 403 without saying why).
pub(crate) fn authorise(requested: &str, pool: &[String]) -> Option<String> {
    let requested = requested.trim();
    if requested.is_empty() || requested.len() > MAX_PROJECT_HEADER || requested.contains('\0') {
        return None;
    }
    let path = Path::new(requested);
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let canonical = canonical_project_dir(path)?;
    pool.iter()
        .find(|root| {
            let root_path = Path::new(root.as_str());
            root_path.is_absolute()
                && canonical_project_dir(root_path).is_some_and(|known| known == canonical)
        })
        .cloned()
}

/// Run `operation` for `project` (request-scoped), or unscoped for `None`.
pub(crate) fn in_request_project<T>(project: Option<&str>, operation: impl FnOnce() -> T) -> T {
    match project {
        Some(root) => crate::core::policy::runtime::in_project(root, operation),
        None => operation(),
    }
}

/// The data partition a read belongs to: the hashed project root (as the
/// MCP root binding computes it), or `unbound` without a project.
pub(crate) fn partition_of(project: Option<&str>) -> String {
    let root = project.map_or_else(
        || super::routes::helpers::dashboard_project_binding().0,
        |root| Some(root.to_string()),
    );
    root.as_deref().filter(|root| !root.is_empty()).map_or_else(
        || "unbound".to_string(),
        crate::core::project_hash::hash_project_root,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(dir: &Path, name: &str) -> String {
        let root = dir.join(name);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        root.to_string_lossy().into_owned()
    }

    #[test]
    fn only_a_pool_project_is_accepted_and_bound_to_the_pools_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let alpha = project(dir.path(), "alpha");
        let beta = project(dir.path(), "beta");
        let foreign = project(dir.path(), "foreign");
        let pool = vec![alpha.clone(), beta.clone()];

        assert_eq!(authorise(&alpha, &pool), Some(alpha.clone()));
        assert_eq!(authorise(&format!("{beta}/"), &pool), Some(beta.clone()));
        let detour = format!("{alpha}/.git/..");
        assert_eq!(authorise(&detour, &pool), None, "traversal is refused");
        // Reached through a link, the project still binds to the root the
        // sessions use, so its data partition is the same.
        #[cfg(unix)]
        {
            let link = dir.path().join("link-to-alpha");
            std::os::unix::fs::symlink(&alpha, &link).unwrap();
            assert_eq!(
                authorise(&link.to_string_lossy(), &pool),
                Some(alpha.clone())
            );
        }

        for refused in [
            foreign.as_str(),
            "relative/path",
            "",
            "/definitely/not/here",
            "/",
        ] {
            assert_eq!(authorise(refused, &pool), None, "{refused:?}");
        }
        let with_nul = format!("{alpha}\0");
        assert_eq!(authorise(&with_nul, &pool), None);
        if let Some(home) = dirs::home_dir() {
            let home = home.to_string_lossy().into_owned();
            assert_eq!(authorise(&home, std::slice::from_ref(&home)), None);
        }
    }

    #[test]
    fn the_overlay_alias_and_only_reads_are_project_scoped() {
        assert_eq!(
            canonical_route("/api/context-overlays"),
            "/api/context-overlay-history"
        );
        assert!(is_project_read(
            canonical_route("/api/context-overlays"),
            "GET"
        ));
        assert!(is_project_read("/api/roi", "GET"));
        assert!(!is_project_read("/api/roi", "POST"));
        assert!(!is_project_read("/api/agents/work-graph", "GET"));
    }

    #[test]
    fn the_analysis_bridge_posts_are_project_scoped() {
        assert!(is_project_read("/api/dashboard-analysis/inputs", "POST"));
        assert!(is_project_read("/api/dashboard-analysis/apply", "POST"));
        assert!(!is_project_read("/api/dashboard-analysis/inputs", "GET"));
        assert!(!is_project_read("/api/settings", "POST"));
    }
}
