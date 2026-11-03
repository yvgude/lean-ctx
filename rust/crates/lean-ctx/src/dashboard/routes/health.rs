// SPDX-License-Identifier: Apache-2.0
use serde::Serialize;

const RECENT_COMPRESSION_MINUTES: i64 = 30;
// Dashboard responses stay deterministic; every check is evaluated for this request.
const LAST_CHECKED: &str = "on_request";

#[derive(Serialize)]
pub(crate) struct WorkspaceHealth {
    pub(crate) overall_status: &'static str,
    pub(crate) checks: Vec<WorkspaceHealthCheck>,
}

#[derive(Serialize)]
pub(crate) struct WorkspaceHealthCheck {
    pub(crate) name: &'static str,
    pub(crate) status: &'static str,
    pub(crate) message: String,
    last_checked: &'static str,
}

pub(super) fn handle(
    path: &str,
    _query: &str,
    method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/workspace-health" if method.eq_ignore_ascii_case("GET") => {
            let body = serde_json::to_string(&workspace_health())
                .unwrap_or_else(|_| "{\"overall_status\":\"Degraded\",\"checks\":[]}".to_string());
            Some(("200 OK", "application/json", body))
        }
        "/api/workspace-health" => Some((
            "405 Method Not Allowed",
            "application/json",
            "{\"error\":\"method not allowed\"}".to_string(),
        )),
        _ => None,
    }
}

pub(crate) fn workspace_health() -> WorkspaceHealth {
    let daemon_running = lean_ctx::daemon::is_daemon_running();
    let (doctor_pass, doctor_total) = crate::doctor::compact_score();
    let doctor_healthy = doctor_total == 0 || doctor_pass > doctor_total / 2;
    let compression_recent = compression_is_recent();
    let config_error = lean_ctx::core::config::last_config_parse_error();

    let checks = vec![
        check(
            "daemon_running",
            if daemon_running { "OK" } else { "Stopped" },
            if daemon_running {
                "Daemon is running".to_string()
            } else {
                "Daemon is not running".to_string()
            },
        ),
        check(
            "doctor_score",
            if doctor_healthy { "OK" } else { "Degraded" },
            format!("Doctor checks: {doctor_pass}/{doctor_total} passing"),
        ),
        check(
            "last_compression_recent",
            if compression_recent { "OK" } else { "Degraded" },
            if compression_recent {
                format!("Compression activity recorded within {RECENT_COMPRESSION_MINUTES} minutes")
            } else {
                format!("No compression activity within {RECENT_COMPRESSION_MINUTES} minutes")
            },
        ),
        check(
            "config_valid",
            if config_error.is_none() {
                "OK"
            } else {
                "Degraded"
            },
            config_error
                .map(|error| format!("Config parse error: {error}"))
                .unwrap_or_else(|| "Loaded configuration is valid".to_string()),
        ),
    ];

    let overall_status = if checks.iter().any(|check| check.status == "Stopped") {
        "Stopped"
    } else if checks.iter().any(|check| check.status != "OK") {
        "Degraded"
    } else {
        "OK"
    };

    WorkspaceHealth {
        overall_status,
        checks,
    }
}

fn check(name: &'static str, status: &'static str, message: String) -> WorkspaceHealthCheck {
    WorkspaceHealthCheck {
        name,
        status,
        message,
        last_checked: LAST_CHECKED,
    }
}

fn compression_is_recent() -> bool {
    lean_ctx::core::stats::load()
        .last_use
        .as_deref()
        .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
        .is_some_and(|timestamp| {
            let age =
                chrono::Utc::now().signed_duration_since(timestamp.with_timezone(&chrono::Utc));
            age >= chrono::Duration::zero()
                && age <= chrono::Duration::minutes(RECENT_COMPRESSION_MINUTES)
        })
}

#[cfg(test)]
mod tests {
    use super::handle;

    #[test]
    fn health_route_returns_expected_shape() {
        let (_, _, body) = handle("/api/workspace-health", "", "GET", "").expect("health response");
        let value: serde_json::Value = serde_json::from_str(&body).expect("health JSON");
        assert!(matches!(
            value["overall_status"].as_str(),
            Some("OK" | "Degraded" | "Stopped")
        ));
        let names: Vec<_> = value["checks"]
            .as_array()
            .expect("checks array")
            .iter()
            .filter_map(|check| check["name"].as_str())
            .collect();
        assert_eq!(
            names,
            [
                "daemon_running",
                "doctor_score",
                "last_compression_recent",
                "config_valid",
            ]
        );
    }
}
