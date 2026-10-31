// SPDX-License-Identifier: Apache-2.0
//! Per-request project binding for dashboard reads (`X-LeanCTX-Project`).
//!
//! The Cockpit shows several projects from one dashboard. A read names its
//! project in the `X-LeanCTX-Project` header; the dashboard accepts it only
//! when it is a project this dashboard may show — its own binding or a
//! workspace in the `/api/workspaces` pool — and then answers that one read
//! inside a request-scoped project ([`crate::core::policy::runtime::REQUEST_PROJECT`]),
//! never by changing anything process-wide. Two concurrent reads for two
//! projects therefore cannot see each other's data. A route that can answer
//! neither for the named project nor from device-wide data refuses the
//! request ([`project_scope`]) instead of answering for another project.
//!
//! Validation follows the MCP root binding: an existing absolute directory,
//! no traversal, never a broad or unsafe root, compared after
//! canonicalisation. The read is bound to the pool's own spelling of the
//! root, so it lands in the same data partition
//! ([`crate::core::project_hash::hash_project_root`]) the agents wrote to.

use std::path::{Component, Path, PathBuf};

const MAX_PROJECT_HEADER: usize = 4096;

/// Reads answered for the project the request names: the Cockpit's
/// `READ_ROUTES`, plus every other read whose handler finds its project
/// through `routes::helpers` (`detect_project_root_for_dashboard`,
/// `request_project`, `request_ledger_scope`) or `SessionState::load_latest`,
/// which all follow the request-scoped project.
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
    "/api/events",
    "/api/snapshots",
    "/api/snapshot",
    "/api/settings",
    "/api/doctor",
    "/api/roi",
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
    // Reads the Cockpit does not list, answered from the project root.
    "/api/tree",
    "/api/architecture",
    "/api/compression-demo",
    "/api/tools-health",
    "/api/search-index",
    "/api/intent",
    "/api/hotspots",
    "/api/communities",
    "/api/smells",
    "/api/smells/summary",
    "/api/gateway/receipts",
    "/api/gateway/control",
    "/api/gateway/deliver",
    "/api/gateway/connect",
];

/// Reads whose data is the device's, not any project's: the answer is the
/// same whichever project the request names, so a valid project header is
/// accepted and has no effect.
///
/// - `/api/telemetry`: this installation's anonymous heartbeat status.
/// - `/api/context-summary`, `/api/context-history` and the context
///   diagnostics below: the context ledger (`state_dir`), the context radar,
///   proxy introspection and the bounce tracker are kept per device and
///   agent, not per project.
/// - `/api/context-capabilities`, `/api/context-dynamic-tools`, `/api/mcp`:
///   the connected client and its tool list.
/// - `/api/profile`, `/api/theme-tokens`: the active profile and theme.
/// - `/api/pulse`, `/api/pipeline-stats`: process counters.
/// - `/api/gateway/detectors`: the detectors this engine ships.
/// - `/metrics`: the process's Prometheus metrics.
const GLOBAL_READ_ROUTES: &[&str] = &[
    "/api/telemetry",
    "/api/context-summary",
    "/api/context-history",
    "/api/context-capabilities",
    "/api/context-field",
    "/api/context-pressure",
    "/api/context-bounce",
    "/api/context-radar",
    "/api/context-model",
    "/api/context-events",
    "/api/context-dynamic-tools",
    "/api/mcp",
    "/api/profile",
    "/api/theme-tokens",
    "/api/pulse",
    "/api/pipeline-stats",
    "/api/gateway/detectors",
    "/metrics",
];

/// Writes made in the project the request names. Their handlers resolve the
/// project like the reads above; without the request's project they would
/// write to the dashboard's own project instead of the one the UI selected.
/// The analysis bridge (`routes::analysis_bridge`) collects and applies one
/// analysis for the selected project; without it nothing is bound when the
/// dashboard was started outside a project, and every analysis page fails.
const PROJECT_WRITE_ROUTES: &[&str] = &[
    "/api/dashboard-analysis/inputs",
    "/api/dashboard-analysis/apply",
    "/api/session/note",
    "/api/episodes/annotate",
    "/api/knowledge/edit",
    "/api/knowledge-relations/edit",
    "/api/context-overlay",
    "/api/context-policy",
];

/// Writes to device-wide state (`config.toml`, the feedback outbox): a valid
/// project header is accepted and has no effect.
const GLOBAL_WRITE_ROUTES: &[&str] = &["/api/settings", "/api/feedback"];

/// How a request that names a project (`X-LeanCTX-Project`) is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectScope {
    /// Answered inside the named project.
    Project,
    /// Answered from device-wide data, the same for every project.
    Global,
    /// The route cannot answer for a named project: such a request is
    /// refused rather than answered for another project.
    Refused,
}

/// Older names the Cockpit still sends.
const ROUTE_ALIASES: &[(&str, &str)] = &[("/api/context-overlays", "/api/context-overlay-history")];

/// The engine route that answers `path` (aliases resolved).
pub(crate) fn canonical_route(path: &str) -> &str {
    ROUTE_ALIASES
        .iter()
        .find_map(|(alias, route)| (*alias == path).then_some(*route))
        .unwrap_or(path)
}

/// How a request for `route` (already canonical) that names a project is
/// answered. Paths outside `/api/` carry no project data and are global.
pub(crate) fn project_scope(route: &str, method: &str) -> ProjectScope {
    let read = method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD");
    let write = method.eq_ignore_ascii_case("POST");
    if (read && PROJECT_READ_ROUTES.contains(&route))
        || (write && PROJECT_WRITE_ROUTES.contains(&route))
    {
        ProjectScope::Project
    } else if (read && GLOBAL_READ_ROUTES.contains(&route))
        || (write && GLOBAL_WRITE_ROUTES.contains(&route))
        || !route.starts_with("/api/")
    {
        ProjectScope::Global
    } else {
        ProjectScope::Refused
    }
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
        assert_eq!(
            project_scope(canonical_route("/api/context-overlays"), "GET"),
            ProjectScope::Project
        );
        assert_eq!(project_scope("/api/roi", "GET"), ProjectScope::Project);
        assert_eq!(project_scope("/api/roi", "POST"), ProjectScope::Refused);
        assert_eq!(
            project_scope("/api/agents/work-graph", "GET"),
            ProjectScope::Refused
        );
    }

    #[test]
    fn the_analysis_bridge_posts_are_project_scoped() {
        for route in [
            "/api/dashboard-analysis/inputs",
            "/api/dashboard-analysis/apply",
        ] {
            assert_eq!(project_scope(route, "POST"), ProjectScope::Project);
        }
        assert_eq!(
            project_scope("/api/dashboard-analysis/inputs", "GET"),
            ProjectScope::Refused
        );
        assert_eq!(project_scope("/api/settings", "POST"), ProjectScope::Global);
    }

    /// F12a: reads that answer from the project root follow the named
    /// project, device-wide reads ignore it, and a route that can do
    /// neither refuses instead of answering for another project.
    #[test]
    fn every_read_with_a_named_project_is_bound_global_or_refused() {
        for route in [
            "/api/tree",
            "/api/architecture",
            "/api/compression-demo",
            "/api/tools-health",
            "/api/intent",
            "/api/gateway/receipts",
        ] {
            assert_eq!(
                project_scope(route, "GET"),
                ProjectScope::Project,
                "{route}"
            );
        }
        for route in [
            "/api/telemetry",
            "/api/context-summary",
            "/api/context-history",
            "/api/gateway/detectors",
            "/metrics",
            "/",
        ] {
            assert_eq!(project_scope(route, "GET"), ProjectScope::Global, "{route}");
        }
        for (route, method) in [
            ("/api/live", "GET"),
            ("/api/roi-report", "GET"),
            ("/api/doctor/fix", "POST"),
            ("/api/agents/sessions", "POST"),
            ("/api/tree", "DELETE"),
        ] {
            assert_eq!(
                project_scope(route, method),
                ProjectScope::Refused,
                "{method} {route}"
            );
        }
    }

    /// F12b: project writes follow the named project.
    #[test]
    fn project_writes_follow_the_named_project() {
        for route in [
            "/api/session/note",
            "/api/episodes/annotate",
            "/api/knowledge/edit",
            "/api/knowledge-relations/edit",
            "/api/context-overlay",
        ] {
            assert_eq!(
                project_scope(route, "POST"),
                ProjectScope::Project,
                "{route}"
            );
        }
    }

    /// No route is listed twice with two different answers.
    #[test]
    fn the_route_classes_do_not_overlap() {
        for route in PROJECT_READ_ROUTES {
            assert!(!GLOBAL_READ_ROUTES.contains(route), "{route}");
        }
        for route in PROJECT_WRITE_ROUTES {
            assert!(!GLOBAL_WRITE_ROUTES.contains(route), "{route}");
        }
    }
}
