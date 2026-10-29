// SPDX-License-Identifier: Apache-2.0
//! The Hive effective policy as a floor of the gateway policy engine
//! (Release B 3.1.4).
//!
//! A verified bundle ([`super::effective_policy`]) is installed as two files
//! in `<data_dir>/hive/policy/effective/`, both 0600 and written atomically:
//!
//! - `floor.json` ([`Floor`]): the verified `policy` (default + per vault),
//!   its version, digest and `not_after`;
//! - `required.json`: the highest version ever installed. It only grows, and
//!   it is what makes a missing policy never mean "no rules": while it
//!   exists, a missing, damaged or older floor fails every policy refresh
//!   closed instead of dropping the Hive rules.
//!
//! The policy snapshot ([`crate::core::policy::runtime`]) reads both files
//! and folds the floor *underneath* whatever the engine would enforce
//! otherwise (local pack, org policy), with [`crate::core::policy::floor::merge_floor`]:
//! every field merges toward the stricter side, so a local setting can only
//! tighten the Hive policy, never loosen it. [`loosened_by_floor`] names the
//! local settings the floor overrides, for `hive status`.
//!
//! Only the members the schema marks `x-enforcement: engine` are mapped
//! (their `x-engine` field); everything else is reported as unenforced on
//! the ack. Expiry (`not_after`) does not lift the floor: the last valid
//! rules keep restricting local work offline; only sensitive operations
//! fail closed ([`super::effective_policy::sensitive_gate`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::input_filters::FilterAction;
use crate::core::policy::{
    BudgetRules, EgressRules, FilterRules, ResolvedPolicy, RoutingPolicyRules,
};
use lean_ctx_protocol::hive::effective_policy::{BundlePolicyV1, EffectivePolicyV1};

pub const FLOOR_SCHEMA: &str = "leanctx.hive.policy-floor/v1";
pub(crate) const DIR: &str = "effective";
pub(crate) const FLOOR_FILE: &str = "floor.json";
pub(crate) const REQUIRED_FILE: &str = "required.json";
/// Name of the resolved Hive floor (shows in `policy` diagnostics).
pub const FLOOR_NAME: &str = "hive-policy";

/// What `floor.json` holds: the verified effective policy, nothing else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Floor {
    pub schema: String,
    pub version: u64,
    pub digest: String,
    pub not_after: String,
    pub policy: BundlePolicyV1,
}

/// `required.json`: the highest installed version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Required {
    pub version: u64,
}

/// `<data_dir>/hive/policy/effective` of this installation.
pub fn floor_dir() -> Option<PathBuf> {
    let data = crate::core::paths::data_dir().ok()?;
    Some(data.join("hive").join("policy").join(DIR))
}

/// Whether `target` (canonical) lies in `<data_dir>/hive/policy`, the files
/// only the engine's own policy pull writes: agent file tools are refused
/// there ([`crate::core::pathjail::enforce_writable`]).
pub fn is_policy_path(target: &Path) -> bool {
    let Ok(data) = crate::core::paths::data_dir() else {
        return false;
    };
    let dir = data.join("hive").join("policy");
    let canonical =
        crate::core::pathutil::canonicalize_secure(&dir).unwrap_or_else(|_| dir.clone());
    target.starts_with(&dir) || target.starts_with(&canonical)
}

/// `floor_dir` below a Hive store's data directory (`<data_dir>/hive`).
pub fn floor_dir_in(hive_data: &Path) -> PathBuf {
    hive_data.join("policy").join(DIR)
}

/// The installed floor and requirement, checked against each other: `Ok(None)`
/// only when neither exists (no Hive policy was ever installed here).
pub fn check(floor: Option<&str>, required: Option<&str>) -> Result<Option<Floor>, String> {
    let required = required
        .map(serde_json::from_str::<Required>)
        .transpose()
        .map_err(|_| "the Hive policy requirement is damaged")?;
    let floor = match floor {
        Some(text) => Some(
            serde_json::from_str::<Floor>(text)
                .ok()
                .filter(|f| f.schema == FLOOR_SCHEMA)
                .filter(|f| {
                    f.policy.effective.validate().is_ok()
                        && f.policy
                            .vaults
                            .iter()
                            .all(|v| v.effective.validate().is_ok())
                })
                .ok_or("the installed Hive policy is damaged")?,
        ),
        None => None,
    };
    match (floor, required) {
        (None, Some(_)) => Err("the Hive policy in force is missing".into()),
        (Some(floor), Some(required)) if floor.version < required.version => Err(format!(
            "the installed Hive policy v{} is older than v{} in force",
            floor.version, required.version
        )),
        (floor, _) => Ok(floor),
    }
}

/// Read both files of `dir` (absent = `None`).
pub fn read(dir: &Path) -> Result<(Option<String>, Option<String>), String> {
    let read = |name: &str| match std::fs::read_to_string(dir.join(name)) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(format!("the Hive policy file {name} is unreadable")),
    };
    Ok((read(FLOOR_FILE)?, read(REQUIRED_FILE)?))
}

/// The vault whose effective policy applies to `project_root`, when the
/// floor has per-vault entries: the vault this checkout is linked to.
pub fn vault_for(floor_text: &str, hive_data: &Path, project_root: &Path) -> Option<String> {
    let floor: Floor = serde_json::from_str(floor_text).ok()?;
    if floor.policy.vaults.is_empty() {
        return None;
    }
    let map = super::vault::VaultMap::load(hive_data);
    let record = map.for_root(&project_root.to_string_lossy())?;
    floor
        .policy
        .vaults
        .iter()
        .any(|v| v.vault_id == record.vault_id)
        .then(|| record.vault_id.clone())
}

/// The floor for `vault` (or the default) as a resolved policy, or `None`
/// when no Hive policy is installed. An inconsistent pair is an error, and
/// the caller fails closed.
pub fn resolve(
    floor: Option<&str>,
    required: Option<&str>,
    vault: Option<&str>,
) -> Result<Option<ResolvedPolicy>, String> {
    Ok(check(floor, required)?
        .map(|floor| to_resolved(floor.policy.for_vault(vault), floor.version)))
}

fn cap_u32(value: Option<u64>) -> Option<u32> {
    value.map(|v| u32::try_from(v).unwrap_or(u32::MAX))
}

/// Cents per the schema; the engine counts USD.
#[allow(clippy::cast_precision_loss)] // ≤ 1e8 cents is exact in f64
fn usd(cents: Option<u64>) -> Option<f64> {
    cents.map(|c| c as f64 / 100.0)
}

/// The `x-engine` mapping of every `engine` member onto the gateway pack.
pub fn to_resolved(policy: &EffectivePolicyV1, version: u64) -> ResolvedPolicy {
    let s = &policy.security;
    let c = &policy.context;
    // A pack's label list replaces the built-in set; a Hive list must
    // only add to it, so the built-in labels stay in force with it.
    let blocked_labels = s.blocked_labels.as_ref().map_or_else(Vec::new, |labels| {
        let mut all: Vec<String> = crate::core::input_filters::classification::DEFAULT_LABELS
            .iter()
            .map(|l| (*l).to_string())
            .collect();
        all.extend(labels.iter().cloned());
        all
    });
    ResolvedPolicy {
        name: FLOOR_NAME.into(),
        version: version.to_string(),
        description: format!("Hive effective policy v{version}"),
        chain: Vec::new(),
        default_read_mode: c.read_mode.clone(),
        allow_tools: c.allowed_tools.clone(),
        deny_tools: c.denied_tools.clone().unwrap_or_default(),
        max_context_tokens: cap_u32(c.max_context_tokens),
        audit_retention_days: None,
        redaction: BTreeMap::new(),
        filters: FilterRules {
            pii: s.pii.map(|a| a.as_str().to_string()),
            classification: s.classification.map(|a| a.as_str().to_string()),
            injection: c.injection_filter.map(|a| a.as_str().to_string()),
            blocked_labels,
            ..FilterRules::default()
        },
        egress: EgressRules {
            forbidden_patterns: Vec::new(),
            block_secrets: s.block_secrets,
            max_writes_per_min: cap_u32(c.max_writes_per_min),
        },
        routing: RoutingPolicyRules {
            allowed_models: Vec::new(),
            model_ceiling_groups: s.allowed_models.clone().unwrap_or_default(),
            forbid_downgrade_for: Vec::new(),
        },
        budgets: BudgetRules {
            max_cost_usd_per_person_per_day: usd(c.max_cost_cents_per_person_per_day),
            max_cost_usd_per_project_per_month: usd(c.max_cost_cents_per_project_per_month),
            max_requests_per_minute_per_person: cap_u32(c.max_requests_per_minute_per_person),
        },
    }
}

/// Fold the Hive floor under `effective` (what the engine enforces without
/// it). Tighten-only: see [`crate::core::policy::floor::merge_floor`].
pub fn apply(floor: &ResolvedPolicy, effective: Option<&ResolvedPolicy>) -> ResolvedPolicy {
    crate::core::policy::floor::merge_floor(floor, effective)
}

/// The local settings of the project at `project_root` (its
/// `.lean-ctx/policy.toml`) that the installed Hive floor overrides.
pub fn local_overrides(hive_data: &Path, project_root: &Path) -> Vec<String> {
    let Ok((Some(text), required)) = read(&floor_dir_in(hive_data)) else {
        return Vec::new();
    };
    let Ok(Some(floor)) = check(Some(&text), required.as_deref()) else {
        return Vec::new();
    };
    let pack = project_root.join(".lean-ctx").join("policy.toml");
    let Some(local) = crate::core::policy::parse_file(&pack)
        .ok()
        .and_then(|p| crate::core::policy::resolve(&p).ok())
    else {
        return Vec::new();
    };
    let vault = vault_for(&text, hive_data, project_root);
    let hive = to_resolved(floor.policy.for_vault(vault.as_deref()), floor.version);
    loosened_by_floor(&hive, &local)
}

fn rank(action: Option<&String>) -> Option<u8> {
    action
        .and_then(|a| FilterAction::parse(a))
        .map(FilterAction::rank)
}

/// The settings of `local` that are looser than the Hive `floor` and are
/// therefore ignored, one line each, in a fixed order.
pub fn loosened_by_floor(floor: &ResolvedPolicy, local: &ResolvedPolicy) -> Vec<String> {
    let mut out = Vec::new();
    let mut cap = |field: &str, local: Option<f64>, hive: Option<f64>| {
        if let (Some(l), Some(a)) = (local, hive)
            && l > a
        {
            out.push(format!("{field}: local {l}, Hive {a} applies"));
        }
    };
    let f = |v: Option<u32>| v.map(f64::from);
    cap(
        "context.max_context_tokens",
        f(local.max_context_tokens),
        f(floor.max_context_tokens),
    );
    cap(
        "egress.max_writes_per_min",
        f(local.egress.max_writes_per_min),
        f(floor.egress.max_writes_per_min),
    );
    cap(
        "budgets.max_cost_usd_per_person_per_day",
        local.budgets.max_cost_usd_per_person_per_day,
        floor.budgets.max_cost_usd_per_person_per_day,
    );
    cap(
        "budgets.max_cost_usd_per_project_per_month",
        local.budgets.max_cost_usd_per_project_per_month,
        floor.budgets.max_cost_usd_per_project_per_month,
    );
    cap(
        "budgets.max_requests_per_minute_per_person",
        f(local.budgets.max_requests_per_minute_per_person),
        f(floor.budgets.max_requests_per_minute_per_person),
    );
    for (field, l, a) in [
        ("filters.pii", &local.filters.pii, &floor.filters.pii),
        (
            "filters.classification",
            &local.filters.classification,
            &floor.filters.classification,
        ),
        (
            "filters.injection",
            &local.filters.injection,
            &floor.filters.injection,
        ),
    ] {
        if let (Some(lr), Some(ar)) = (rank(l.as_ref()), rank(a.as_ref()))
            && lr < ar
        {
            out.push(format!(
                "{field}: local {}, Hive {} applies",
                l.as_deref().unwrap_or_default(),
                a.as_deref().unwrap_or_default()
            ));
        }
    }
    if local.egress.block_secrets == Some(false) && floor.egress.block_secrets == Some(true) {
        out.push("egress.block_secrets: local false, Hive true applies".into());
    }
    if let (Some(l), Some(a)) = (&local.default_read_mode, &floor.default_read_mode)
        && l != a
    {
        out.push(format!(
            "context.default_read_mode: local {l}, Hive {a} applies"
        ));
    }
    if let Some(local_allow) = &local.allow_tools {
        let extra: Vec<&str> = local_allow
            .iter()
            .filter(|t| {
                floor.deny_tools.contains(t)
                    || floor
                        .allow_tools
                        .as_ref()
                        .is_some_and(|allow| !allow.contains(t))
            })
            .map(String::as_str)
            .collect();
        if !extra.is_empty() {
            out.push(format!(
                "context.allow_tools: local allows {}, Hive does not",
                extra.join(", ")
            ));
        }
    }
    out
}
