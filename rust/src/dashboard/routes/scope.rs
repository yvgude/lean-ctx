// SPDX-License-Identifier: Apache-2.0
//! `GET /api/scope`: the project this dashboard and the agents it shows are
//! bound to, and where that binding came from (AP01 §7, AP08).
//!
//! The binding is the dashboard process's own (`--project`,
//! `LEAN_CTX_PROJECT_ROOT`, or the real project it was started in); it is
//! never inferred from another project's activity. It is the binding an MCP
//! session makes for the same folder: an explicit root is used as given,
//! never promoted to an enclosing checkout; a start folder is bound by the
//! MCP startup rule. Unbound means no project
//! data is shown, and the answer says how to bind one. Display texts are the
//! client's: only closed codes leave here.

use serde_json::{Value, json};

use super::helpers::{DashboardProjectSource, dashboard_project_binding};

pub(super) fn handle(
    path: &str,
    _query: &str,
    method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    if path != "/api/scope" {
        return None;
    }
    if !method.eq_ignore_ascii_case("GET") {
        return Some((
            "405 Method Not Allowed",
            "application/json",
            r#"{"error":"method not allowed"}"#.to_string(),
        ));
    }
    let (root, source) = dashboard_project_binding();
    Some((
        "200 OK",
        "application/json",
        scope(root.as_deref(), source).to_string(),
    ))
}

fn source_code(source: DashboardProjectSource) -> &'static str {
    match source {
        DashboardProjectSource::ExplicitFlag => "explicit_flag",
        DashboardProjectSource::Env => "env",
        DashboardProjectSource::Cwd => "cwd",
        DashboardProjectSource::Request => "request",
        DashboardProjectSource::Unbound => "unbound",
    }
}

fn scope(root: Option<&str>, source: DashboardProjectSource) -> Value {
    let Some(root) = root.filter(|root| !root.is_empty()) else {
        return json!({
            "schema_version": 1,
            "binding_source": source_code(DashboardProjectSource::Unbound),
            "reason_code": "project_unresolved",
            "next_step": "bind_project",
        });
    };
    let partition = crate::core::project_hash::hash_project_root(root);
    let label = std::path::Path::new(root).file_name().map_or_else(
        || root.to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
        (out.status.success() && !text.is_empty()).then_some(text)
    };
    let mut answer = json!({
        "schema_version": 1,
        "binding_source": source_code(source),
        "partition_id": partition,
        "workspace_root": root,
        "project": {
            "id": partition,
            "label": label,
            "kind": "local",
            // Each worktree is its own partition; `lean-ctx project link`
            // maps one to a team project (D01).
            "mapping": crate::core::project_link::mapping_json(root),
        },
        "branch": git(&["rev-parse", "--abbrev-ref", "HEAD"]),
        "revision": git(&["rev-parse", "HEAD"]),
    });
    // AP04-G7: present only when this project was chosen among several
    // workspace roots.
    if let Some(selection) = crate::core::root_selection::recorded_for(root) {
        answer["selection"] = selection.scope_json();
    }
    // A launcher-bound Workspace of this process (#2120).
    if let Some(snapshot) = crate::core::workspace_authority::load_current().snapshot() {
        answer["workspace"] = snapshot.to_json();
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AP08: an unbound dashboard says so with codes and shows no project;
    /// a bound one names its partition and where the binding came from.
    #[test]
    fn scope_names_the_binding_and_never_a_foreign_project() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let unbound = scope(None, DashboardProjectSource::Unbound);
        assert_eq!(unbound["reason_code"], "project_unresolved");
        assert_eq!(unbound["next_step"], "bind_project");
        assert!(unbound.get("workspace_root").is_none());

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        let bound = scope(Some(root), DashboardProjectSource::ExplicitFlag);
        assert_eq!(bound["binding_source"], "explicit_flag");
        assert_eq!(bound["workspace_root"], root);
        assert_eq!(
            bound["partition_id"],
            crate::core::project_hash::hash_project_root(root)
        );
        assert_eq!(bound["project"]["mapping"], "unmapped");
        assert!(bound.get("selection").is_none());
    }

    /// D01 and G7: a linked checkout reports its link as `project.mapping`;
    /// `selection` appears only when the project was chosen among several
    /// workspace roots, and the answer is byte-stable.
    #[test]
    fn scope_reports_link_and_multi_root_selection() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let workspace = tempfile::tempdir().unwrap();
        let canonical = |name: &str| {
            let dir = workspace.path().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::canonicalize(dir)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        let (first, second) = (canonical("first"), canonical("second"));

        crate::core::project_link::link(crate::core::project_link::ProjectLink {
            workspace_root: first.clone(),
            team_project_id: "gateway".to_owned(),
            organization_id: None,
            label: None,
            linked_at: "2026-10-19T10:00:00Z".to_owned(),
        })
        .unwrap();
        let alone = crate::core::root_selection::select(vec![first.clone()], &[]).unwrap();
        crate::core::root_selection::record(&alone);
        let linked = scope(Some(&first), DashboardProjectSource::ExplicitFlag);
        assert_eq!(
            linked["project"]["mapping"],
            json!({
                "state": "linked",
                "team_project_id": "gateway",
                "linked_at": "2026-10-19T10:00:00Z",
                "change_with": "lean-ctx project link --project <id>",
                "remove_with": "lean-ctx project unlink",
            })
        );
        assert!(linked.get("selection").is_none());

        let several =
            crate::core::root_selection::select(vec![first.clone(), second.clone()], &[]).unwrap();
        crate::core::root_selection::record(&several);
        let chosen = scope(Some(&first), DashboardProjectSource::ExplicitFlag);
        assert_eq!(
            chosen["selection"],
            json!({
                "rule": "first_root",
                "chosen_root": first,
                "candidates": [first, second],
                "change_with": "lean-ctx project use <path>",
            })
        );
        assert_eq!(
            chosen.to_string(),
            scope(Some(&first), DashboardProjectSource::ExplicitFlag).to_string()
        );
        // Not the chosen one: no selection for the other root.
        assert!(
            scope(Some(&second), DashboardProjectSource::ExplicitFlag)
                .get("selection")
                .is_none()
        );
    }
}
