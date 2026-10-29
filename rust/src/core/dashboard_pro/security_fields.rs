// SPDX-License-Identifier: Apache-2.0
//! The non-list Security Center settings a profile may hold and a plan may
//! change, with the value that counts as protected. A change below it is a
//! weakening and needs the user's confirmation.

use serde_json::{Map, Value};

use super::{ProError, ProResult};

#[derive(Debug, Clone, Copy)]
pub(crate) enum Kind {
    Bool,
    /// Allowed values.
    Enum(&'static [&'static str]),
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Field {
    pub(crate) key: &'static str,
    pub(crate) kind: Kind,
    /// The protected value (`"true"`/`"false"` for booleans).
    pub(crate) secure: Option<&'static str>,
    /// Least to most protective, for ordered enums.
    pub(crate) order: Option<&'static [&'static str]>,
}

const ACTIONS: &[&str] = &["off", "warn", "redact", "block"];

const fn field(
    key: &'static str,
    kind: Kind,
    secure: Option<&'static str>,
    order: Option<&'static [&'static str]>,
) -> Field {
    Field {
        key,
        kind,
        secure,
        order,
    }
}

pub(crate) const FIELDS: &[Field] = &[
    field("context_gateway.enabled", Kind::Bool, Some("true"), None),
    field(
        "context_gateway.mode",
        Kind::Enum(&["developer", "governed", "sovereign"]),
        None,
        Some(&["developer", "governed", "sovereign"]),
    ),
    field(
        "context_gateway.secrets",
        Kind::Enum(ACTIONS),
        Some("redact"),
        Some(ACTIONS),
    ),
    field(
        "context_gateway.pii",
        Kind::Enum(ACTIONS),
        Some("redact"),
        Some(ACTIONS),
    ),
    field(
        "context_gateway.injection",
        Kind::Enum(ACTIONS),
        Some("warn"),
        Some(ACTIONS),
    ),
    field(
        "context_gateway.classification",
        Kind::Enum(ACTIONS),
        Some("warn"),
        Some(ACTIONS),
    ),
    field("secret_detection.enabled", Kind::Bool, Some("true"), None),
    field("secret_detection.redact", Kind::Bool, Some("true"), None),
    field("sensitivity.enabled", Kind::Bool, None, None),
    field(
        "sensitivity.policy_floor",
        Kind::Enum(&["public", "internal", "confidential", "secret"]),
        None,
        Some(&["secret", "confidential", "internal", "public"]),
    ),
    field(
        "sensitivity.action",
        Kind::Enum(&["redact", "drop"]),
        None,
        Some(&["redact", "drop"]),
    ),
    field("path_jail", Kind::Bool, Some("true"), None),
    field(
        "sandbox_level",
        Kind::Enum(&["0", "1", "2"]),
        None,
        Some(&["0", "1", "2"]),
    ),
    field("allow_ide_config_dirs", Kind::Bool, Some("false"), None),
    field(
        "shell_security",
        Kind::Enum(&["enforce", "warn", "off"]),
        Some("enforce"),
        Some(&["off", "warn", "enforce"]),
    ),
    field("shell_strict_mode", Kind::Bool, None, None),
    field("shell_allow_writes", Kind::Bool, Some("false"), None),
    field(
        "boundary_policy.cross_project_search",
        Kind::Bool,
        Some("false"),
        None,
    ),
    field(
        "boundary_policy.cross_project_import",
        Kind::Bool,
        Some("false"),
        None,
    ),
    field(
        "boundary_policy.audit_cross_access",
        Kind::Bool,
        Some("true"),
        None,
    ),
];

pub(crate) fn lookup(key: &str) -> Option<&'static Field> {
    FIELDS.iter().find(|f| f.key == key)
}

/// The wire form (`config set` argument) of a JSON value for `field`.
pub(crate) fn wire_of(field: &Field, value: &Value) -> ProResult<String> {
    let text = match (field.kind, value) {
        (Kind::Bool, Value::Bool(flag)) => flag.to_string(),
        (Kind::Enum(values), Value::String(text)) if values.contains(&text.as_str()) => {
            text.clone()
        }
        (Kind::Enum(values), Value::Number(n)) if values.contains(&n.to_string().as_str()) => {
            n.to_string()
        }
        _ => {
            return Err(ProError::conflict(format!(
                "Unsupported value for {}",
                field.key
            )));
        }
    };
    Ok(text)
}

/// The JSON value the effective configuration reports for a wire value.
pub(crate) fn expected_of(field: &Field, wire: &str) -> Value {
    match field.kind {
        Kind::Bool => Value::Bool(wire == "true"),
        Kind::Enum(_) => Value::String(wire.to_string()),
    }
}

/// True only when a change lowers protection below the protected value.
pub(crate) fn weakening(field: &Field, before: &str, after: &str) -> bool {
    let Some(secure) = field.secure else {
        return false;
    };
    if before == after {
        return false;
    }
    if let Some(order) = field.order {
        let rank = |v: &str| order.iter().position(|o| *o == v);
        let (Some(after_rank), Some(secure_rank)) = (rank(after), rank(secure)) else {
            return true;
        };
        let lowered = rank(before).is_none_or(|b| after_rank < b);
        return lowered && after_rank < secure_rank;
    }
    after != secure
}

/// The effective value of `key` as a wire string, from the engine's own
/// configuration (the same view `lean-ctx config` prints).
pub(crate) fn effective(key: &str) -> Option<String> {
    let config = crate::core::config::Config::load();
    let value = toml::Value::try_from(&config).ok()?;
    lookup_wire(&value, key)
}

fn lookup_wire(root: &toml::Value, key: &str) -> Option<String> {
    let mut current = root;
    for part in key.split('.') {
        current = current.get(part)?;
    }
    match current {
        toml::Value::String(text) => Some(text.clone()),
        toml::Value::Boolean(flag) => Some(flag.to_string()),
        toml::Value::Integer(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Input `security_config`: every field's effective value as JSON.
pub(crate) fn security_config() -> Map<String, Value> {
    let config = crate::core::config::Config::load();
    let Ok(root) = toml::Value::try_from(&config) else {
        return Map::new();
    };
    FIELDS
        .iter()
        .filter_map(|f| lookup_wire(&root, f.key).map(|w| (f.key.to_string(), expected_of(f, &w))))
        .collect()
}
