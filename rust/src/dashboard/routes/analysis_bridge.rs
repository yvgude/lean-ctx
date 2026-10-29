// SPDX-License-Identifier: Apache-2.0
//! `POST /api/dashboard-analysis/inputs` and `/apply`: the cockpit's analysis
//! routes. The engine checks the request and collects the bound project's
//! inputs; the cockpit computes the answer; the engine then checks it and
//! executes the planned writes ([`crate::core::dashboard_pro`]).

use crate::core::dashboard_pro::ProjectScope;
use crate::core::dashboard_pro::bridge::{self, Env};
use crate::core::dashboard_pro::collect::RouteSource;
use crate::core::dashboard_pro::effects::SystemNotifier;

pub(super) const INPUTS: &str = "/api/dashboard-analysis/inputs";
pub(super) const APPLY: &str = "/api/dashboard-analysis/apply";

/// Engine read routes, bound to the same project as this request.
struct EngineRoutes;

impl RouteSource for EngineRoutes {
    fn get(&self, path: &str) -> Option<serde_json::Value> {
        let (status, _, body) = super::route_response(path, "", None, None, false, "GET", "");
        (status == "200 OK")
            .then(|| serde_json::from_str(&body).ok())
            .flatten()
    }
}

fn reply(reply: &bridge::Reply) -> (&'static str, &'static str, String) {
    (
        bridge::status_line(reply.status),
        "application/json",
        reply.body.to_string(),
    )
}

/// Whether the request body names an op that reads only device-wide stores.
fn device_wide(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|request| {
            request
                .get("op")
                .and_then(serde_json::Value::as_str)
                .and_then(crate::core::dashboard_pro::request::Op::parse)
        })
        .is_some_and(crate::core::dashboard_pro::request::Op::device_wide)
}

/// `authenticated`: the dashboard runs with a bearer token (project data and
/// writes are never served in no-auth mode).
pub(super) fn handle(
    path: &str,
    authenticated: bool,
    method: &str,
    body: &str,
) -> (&'static str, &'static str, String) {
    if !authenticated {
        return (
            "403 Forbidden",
            "application/json",
            r#"{"error":"authenticated dashboard required"}"#.into(),
        );
    }
    if !method.eq_ignore_ascii_case("POST") {
        return (
            bridge::status_line(405),
            "application/json",
            r#"{"error":"POST required"}"#.into(),
        );
    }
    let root = super::helpers::detect_project_root_for_dashboard();
    // "All projects": an op over device-wide stores reads every project; one
    // over a project's own knowledge still needs a project.
    let scoped = if root.trim().is_empty() && device_wide(body) {
        ProjectScope::device(&crate::dashboard::project_binding::project_pool())
    } else {
        ProjectScope::bind(&root)
    };
    let scope = match scoped {
        Ok(scope) => scope,
        Err(error) => {
            return (
                bridge::status_line(error.status),
                "application/json",
                crate::core::dashboard_pro::error_body(&error).to_string(),
            );
        }
    };
    let env = Env {
        scope: &scope,
        routes: &EngineRoutes,
        notifier: &SystemNotifier,
        now: chrono::Utc::now(),
    };
    if path == INPUTS {
        reply(&bridge::handle_inputs(&env, body))
    } else {
        reply(&bridge::handle_apply(&env, body))
    }
}
