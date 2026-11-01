// SPDX-License-Identifier: Apache-2.0
//! Outbound model policy — the last check on which model a request may reach.
//!
//! Reads `<config_dir>/lean-ctx/router-policy.toml` (name kept for existing
//! installs) and rechecks the **final** outgoing body after alias rewrites and
//! the determinism guard. A missing file is permissive; an unreadable or
//! malformed file, or an unknown field, fails closed. A cost ceiling fails
//! closed too: no verified per-request cost estimate exists before the
//! request is sent, so any `max_cost_micros` refuses every request until one
//! does. `require_reasoning_budget` checks the real `thinking.budget_tokens`.
//!
//! A refusal is answered like every gateway refusal: HTTP 400 with a reason
//! code (never 403, which hosts read as an authentication failure).

use serde::Deserialize;

/// The policy file could not be read or parsed; nothing is allowed.
const POLICY_INVALID: &str = "destination.model_policy_invalid";
/// The outgoing model is denylisted, not allowlisted, or missing.
const MODEL_NOT_ALLOWED: &str = "destination.model_not_allowed";
/// A `max_cost_micros` ceiling is set, but no verified cost estimate exists.
const COST_CEILING_UNVERIFIABLE: &str = "destination.cost_ceiling_unverifiable";
/// `require_reasoning_budget` is set and the body carries no positive budget.
const REASONING_BUDGET_MISSING: &str = "destination.reasoning_budget_missing";

#[derive(Debug, Default, PartialEq)]
struct OutboundModelPolicy {
    invalid: bool,
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
            invalid: false,
            max_cost_micros: file.max_cost_micros,
            model_allowlist: file.model_allowlist,
            model_denylist: file.model_denylist,
            require_reasoning_budget: file.require_reasoning_budget,
        }
    }

    fn deny_all() -> Self {
        Self {
            invalid: true,
            model_allowlist: Some(Vec::new()),
            ..Self::default()
        }
    }

    /// The reason code of the first violated rule, or `None` when allowed.
    fn violation(&self, body: Option<&serde_json::Value>) -> Option<&'static str> {
        if self.invalid {
            return Some(POLICY_INVALID);
        }
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
        if !model_ok {
            return Some(MODEL_NOT_ALLOWED);
        }
        if self.max_cost_micros.is_some() {
            return Some(COST_CEILING_UNVERIFIABLE);
        }
        let budget_ok = !self.require_reasoning_budget
            || body
                .and_then(|body| body.get("thinking"))
                .and_then(|thinking| thinking.get("budget_tokens"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
                > 0;
        (!budget_ok).then_some(REASONING_BUDGET_MISSING)
    }
}

/// Rechecks the actual outgoing body against `router-policy.toml`.
/// `Some(code)` means the proxy must refuse the request (HTTP 400 with that
/// reason code).
pub(crate) fn outbound_violation(body: Option<&serde_json::Value>) -> Option<&'static str> {
    OutboundModelPolicy::load().violation(body)
}

#[cfg(test)]
mod tests {
    use super::{
        COST_CEILING_UNVERIFIABLE, MODEL_NOT_ALLOWED, OutboundModelPolicy, POLICY_INVALID,
        REASONING_BUDGET_MISSING,
    };
    use serde_json::json;

    #[test]
    fn the_outgoing_model_must_satisfy_the_policy_file() {
        let body = |model: &str| json!({"model": model});
        assert_eq!(
            OutboundModelPolicy::default().violation(Some(&body("any"))),
            None
        );

        let policy = OutboundModelPolicy::parse(
            "model_allowlist = [\"claude-sonnet-4-5\"]\nmodel_denylist = [\"gpt-4o\"]\n",
        );
        assert_eq!(policy.violation(Some(&body("claude-sonnet-4-5"))), None);
        assert_eq!(
            policy.violation(Some(&body("gpt-4o-mini"))),
            Some(MODEL_NOT_ALLOWED),
            "not allowlisted"
        );
        assert_eq!(
            policy.violation(None),
            Some(MODEL_NOT_ALLOWED),
            "a body without a model cannot be checked"
        );

        // Unknown fields fail closed and say the policy itself is invalid.
        assert_eq!(
            OutboundModelPolicy::parse("allow = 1\n").violation(Some(&body("any"))),
            Some(POLICY_INVALID)
        );
        // A cost ceiling without a verified estimate refuses every request,
        // and the refusal names the ceiling rather than the model.
        assert_eq!(
            OutboundModelPolicy::parse("max_cost_micros = 5\n").violation(Some(&body("any"))),
            Some(COST_CEILING_UNVERIFIABLE)
        );

        let budget = OutboundModelPolicy::parse("require_reasoning_budget = true\n");
        assert_eq!(
            budget.violation(Some(&body("m"))),
            Some(REASONING_BUDGET_MISSING)
        );
        assert_eq!(
            budget.violation(Some(
                &json!({"model": "m", "thinking": {"budget_tokens": 1024}})
            )),
            None
        );
    }
}
