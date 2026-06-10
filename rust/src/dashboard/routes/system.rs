pub(super) fn handle(
    path: &str,
    _query_str: &str,
    _method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/profile" => {
            let active_name = crate::core::profiles::active_profile_name();
            let profile = crate::core::profiles::active_profile();
            let all = crate::core::profiles::list_profiles();
            let active_info = all.iter().find(|p| p.name == active_name);
            let available: Vec<serde_json::Value> = all
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "name": p.name,
                        "description": p.description,
                        "source": p.source.to_string(),
                    })
                })
                .collect();
            let payload = serde_json::json!({
                "active_name": active_name,
                "active_source": active_info.map(|i| i.source.to_string()),
                "active_description": active_info.map(|i| i.description.clone()),
                "profile": profile,
                "available": available,
            });
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        "/api/buddy" => {
            let buddy = crate::core::buddy::BuddyState::compute();
            let json = serde_json::to_string(&buddy).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        "/api/version" => {
            let json = crate::core::version_check::version_info_json();
            Some(("200 OK", "application/json", json))
        }
        // Purely cosmetic supporter badge (GL #393): resolved from the local
        // plan cache only — no network on this hot path, never gates anything.
        "/api/billing-badge" => {
            let eff = crate::cloud_client::resolve_effective_plan_cached();
            let supporter = !matches!(eff.plan, crate::core::billing::Plan::Free);
            let source = match eff.source {
                crate::cloud_client::PlanSource::Live => "live",
                crate::cloud_client::PlanSource::Cached => "cached",
                crate::cloud_client::PlanSource::Expired => "expired",
                crate::cloud_client::PlanSource::None => "none",
            };
            let payload = serde_json::json!({
                "plan": eff.plan.as_str(),
                "supporter": supporter,
                "source": source,
            });
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        "/metrics" => {
            let prom = crate::core::telemetry::global_metrics().to_prometheus();
            Some(("200 OK", "text/plain; version=0.0.4; charset=utf-8", prom))
        }
        "/api/anomaly" => {
            let s = crate::core::anomaly::summary();
            let json = serde_json::to_string(&s).unwrap_or_else(|_| "[]".to_string());
            Some(("200 OK", "application/json", json))
        }
        "/api/verification" => {
            let snap = crate::core::output_verification::stats_snapshot();
            let json = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        "/api/slos" => {
            let snap = crate::core::slo::evaluate_quiet();
            let history = crate::core::slo::violation_history(100);
            let payload = serde_json::json!({
                "snapshot": snap,
                "history": history,
            });
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        "/api/feedback" => {
            let store = crate::core::feedback::FeedbackStore::load();
            let json = serde_json::to_string(&store).unwrap_or_else(|_| {
                "{\"error\":\"failed to serialize feedback store\"}".to_string()
            });
            Some(("200 OK", "application/json", json))
        }
        "/api/theme-tokens" => {
            let cfg = crate::core::config::Config::load();
            let theme = crate::core::theme::load_theme(&cfg.theme);
            let payload = serde_json::json!({
                "name": theme.name,
                "tokens": {
                    "primary": format!("{}", { let crate::core::theme::Color::Hex(ref h) = theme.primary; h }),
                    "secondary": format!("{}", { let crate::core::theme::Color::Hex(ref h) = theme.secondary; h }),
                    "accent": format!("{}", { let crate::core::theme::Color::Hex(ref h) = theme.accent; h }),
                    "success": format!("{}", { let crate::core::theme::Color::Hex(ref h) = theme.success; h }),
                    "warning": format!("{}", { let crate::core::theme::Color::Hex(ref h) = theme.warning; h }),
                    "danger": format!("{}", { let crate::core::theme::Color::Hex(ref h) = theme.danger; h }),
                    "muted": format!("{}", { let crate::core::theme::Color::Hex(ref h) = theme.muted; h }),
                    "text": format!("{}", { let crate::core::theme::Color::Hex(ref h) = theme.text; h }),
                    "surface": format!("{}", { let crate::core::theme::Color::Hex(ref h) = theme.surface; h }),
                    "background": format!("{}", { let crate::core::theme::Color::Hex(ref h) = theme.background; h }),
                    "barStart": format!("{}", { let crate::core::theme::Color::Hex(ref h) = theme.bar_start; h }),
                    "barEnd": format!("{}", { let crate::core::theme::Color::Hex(ref h) = theme.bar_end; h }),
                    "border": format!("{}", { let crate::core::theme::Color::Hex(ref h) = theme.border; h }),
                },
                "css": theme.to_css_vars(),
            });
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        _ => None,
    }
}
