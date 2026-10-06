// SPDX-License-Identifier: Apache-2.0
//! Outbound model policy — the last check on which model a request may reach.
//!
//! Reads `<config_dir>/lean-ctx/router-policy.toml` (name kept for existing
//! installs) and rechecks the **final** outgoing body after alias rewrites and
//! the determinism guard. A missing file is permissive; an unreadable or
//! malformed file, or an unknown field, fails closed. A cost ceiling fails
//! closed too, because no verified per-request cost estimate exists here.
//! `require_reasoning_budget` checks the real `thinking.budget_tokens`.

use serde::Deserialize;

#[derive(Debug, Default, PartialEq)]
struct OutboundModelPolicy {
    max_cost_micros: Option<u64>,
    model_allowlist: Option<Vec<String>>,
    model_denylist: Vec<String>,
    require_reasoning_budget: bool,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    max_cost_micros: Option<u64>,
    model_allowlist: Option<Vec<String>>,
    #[serde(default)]
    model_denylist: Vec<String>,
    #[serde(default)]
    require_reasoning_budget: bool,
}

impl OutboundModelPolicy {
    fn load() -> Self {
        let Some(path) = dirs::config_dir().map(|dir| dir.join("lean-ctx/router-policy.toml"))
        else {
            return Self::deny_all();
        };
        match std::fs::read_to_string(path) {
            Ok(contents) => Self::parse(&contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(_) => Self::deny_all(),
        }
    }

    fn parse(contents: &str) -> Self {
        let Ok(file) = toml::from_str::<PolicyFile>(contents) else {
            return Self::deny_all();
        };
        Self {
            max_cost_micros: file.max_cost_micros,
            model_allowlist: file.model_allowlist,
            model_denylist: file.model_denylist,
            require_reasoning_budget: file.require_reasoning_budget,
        }
    }

    fn deny_all() -> Self {
        Self {
            model_allowlist: Some(Vec::new()),
            ..Self::default()
        }
    }

    fn allows(&self, body: Option<&serde_json::Value>) -> bool {
        let model = body
            .and_then(|body| body.get("model"))
            .and_then(serde_json::Value::as_str);
        let model_ok = match model {
            Some(model) => {
                !self.model_denylist.iter().any(|denied| denied == model)
                    && self
                        .model_allowlist
                        .as_ref()
                        .is_none_or(|allowed| allowed.iter().any(|entry| entry == model))
            }
            None => self.model_allowlist.is_none() && self.model_denylist.is_empty(),
        };
        let budget_ok = !self.require_reasoning_budget
            || body
                .and_then(|body| body.get("thinking"))
                .and_then(|thinking| thinking.get("budget_tokens"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
                > 0;
        model_ok && self.max_cost_micros.is_none() && budget_ok
    }
}

/// Rechecks the actual outgoing body against `router-policy.toml`.
/// `false` means the proxy must refuse the request (HTTP 403).
pub(crate) fn allows_outbound(body: Option<&serde_json::Value>) -> bool {
    OutboundModelPolicy::load().allows(body)
}

#[cfg(test)]
mod tests {
    use super::OutboundModelPolicy;
    use serde_json::json;

    #[test]
    fn the_outgoing_model_must_satisfy_the_policy_file() {
        let body = |model: &str| json!({"model": model});
        assert!(OutboundModelPolicy::default().allows(Some(&body("any"))));

        let policy = OutboundModelPolicy::parse(
            "model_allowlist = [\"claude-sonnet-4-5\"]\nmodel_denylist = [\"gpt-4o\"]\n",
        );
        assert!(policy.allows(Some(&body("claude-sonnet-4-5"))));
        assert!(
            !policy.allows(Some(&body("gpt-4o-mini"))),
            "not allowlisted"
        );
        assert!(
            !policy.allows(None),
            "a body without a model cannot be checked"
        );

        // Unknown fields and a cost ceiling without an estimate fail closed.
        assert!(!OutboundModelPolicy::parse("allow = 1\n").allows(Some(&body("any"))));
        assert!(!OutboundModelPolicy::parse("max_cost_micros = 5\n").allows(Some(&body("any"))));

        let budget = OutboundModelPolicy::parse("require_reasoning_budget = true\n");
        assert!(!budget.allows(Some(&body("m"))));
        assert!(budget.allows(Some(
            &json!({"model": "m", "thinking": {"budget_tokens": 1024}})
        )));
    }
}
