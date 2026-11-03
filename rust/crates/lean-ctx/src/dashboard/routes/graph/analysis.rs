// SPDX-License-Identifier: Apache-2.0
use crate::dashboard::routes::helpers::{detect_project_root_for_dashboard, extract_query_param};

pub(super) fn get_route(
    path: &str,
    query_str: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/health" => Some(health()),
        "/api/hotspots" => Some(hotspots()),
        "/api/communities" => Some(communities()),
        "/api/smells" => Some(smells(query_str)),
        "/api/smells/summary" => Some(smells_summary()),
        _ => None,
    }
}

fn health() -> (&'static str, &'static str, String) {
    let root = detect_project_root_for_dashboard();
    let result =
        lean_ctx_server::tools::ctx_architecture::handle("health", None, &root, Some("json"));
    ("200 OK", "application/json", result)
}

fn hotspots() -> (&'static str, &'static str, String) {
    let root = detect_project_root_for_dashboard();
    let result =
        lean_ctx_server::tools::ctx_architecture::handle("hotspots", None, &root, Some("json"));
    ("200 OK", "application/json", result)
}

fn communities() -> (&'static str, &'static str, String) {
    let root = detect_project_root_for_dashboard();
    let result =
        lean_ctx_server::tools::ctx_architecture::handle("communities", None, &root, Some("json"));
    ("200 OK", "application/json", result)
}

fn smells(query_str: &str) -> (&'static str, &'static str, String) {
    let root = detect_project_root_for_dashboard();
    let rule = extract_query_param(query_str, "rule");
    let path_filter = extract_query_param(query_str, "path");
    let result = lean_ctx_server::tools::ctx_smells::handle(
        "scan",
        rule.as_deref(),
        path_filter.as_deref(),
        &root,
        Some("json"),
    );
    ("200 OK", "application/json", result)
}

fn smells_summary() -> (&'static str, &'static str, String) {
    let root = detect_project_root_for_dashboard();
    let result =
        lean_ctx_server::tools::ctx_smells::handle("summary", None, None, &root, Some("json"));
    ("200 OK", "application/json", result)
}
