//! Context Policy Packs v1 — Policies-as-Code (GL #489).
//!
//! A policy pack is a declarative, versioned governance preset: which tools an
//! agent may call, the default read mode, redaction patterns for sensitive
//! data, an audit-retention expectation and a context-budget cap. Packs are
//! plain TOML, support single inheritance via `extends`, and resolve into one
//! [`ResolvedPolicy`] a team can review like code.
//!
//! v1 ships the **format, validation, resolution, curated built-ins and the
//! `lean-ctx policy` CLI** (see `cli::policy_cmd`). Runtime enforcement wires
//! in afterward (deliberately decoupled so this module stays free of hot-path
//! churn — see the contract `docs/contracts/context-policy-packs-v1.md`).
//!
//! Inheritance semantics are security-first and predictable:
//! - scalars (`default_read_mode`, `max_context_tokens`,
//!   `audit_retention_days`) — the child **overrides** when set;
//! - `deny_tools` and `[redaction]` — **accumulate** down the chain
//!   (restrictions inherited from a parent can never be silently dropped;
//!   a child may only tighten or re-point a named redaction pattern);
//! - `allow_tools` — the child **overrides** when set (an allowlist is a
//!   deliberate posture choice, not an accumulating set).

pub mod builtin;
pub mod content;
pub mod coverage;
pub(crate) mod diagnostics;
pub mod floor;
pub mod org;
pub mod runtime;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

pub(crate) mod files;

/// Maximum `extends` chain depth (defense against runaway chains; built-ins
/// use at most 2).
const MAX_EXTENDS_DEPTH: usize = 8;

/// Read modes a pack may pin as `default_read_mode` — the documented
/// `ctx_read` mode vocabulary (range reads like `lines:N-M` are call-site
/// specific and make no sense as a policy default).
pub const KNOWN_READ_MODES: &[&str] = &[
    "auto",
    "full",
    "map",
    "signatures",
    "diff",
    "task",
    "reference",
    "aggressive",
    "entropy",
];

// ── Wire format ──────────────────────────────────────────────────────────────

/// One policy pack as written in TOML. Unknown keys are rejected so a typo
/// (`alow_tools`) fails validation instead of silently weakening a policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyPack {
    /// Stable identifier: lowercase, digits and hyphens (`finance-eu`).
    pub name: String,
    /// Semantic version of the pack itself (`1.0.0`).
    pub version: String,
    /// One-line human description.
    pub description: String,
    /// Optional parent pack (built-in name) this pack inherits from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
    /// Context-governance expectations.
    #[serde(default)]
    pub context: ContextRules,
    /// Named redaction patterns: name → regex (matched against content before
    /// it enters the model context).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub redaction: BTreeMap<String, String>,
    /// Inbound content filters (PII / classification / prompt-injection) — the
    /// input side of the Great Filter (GL #675).
    #[serde(default, skip_serializing_if = "FilterRules::is_empty")]
    pub filters: FilterRules,
    /// Egress/output DLP on agent writes & actions (GL #676).
    #[serde(default, skip_serializing_if = "EgressRules::is_empty")]
    pub egress: EgressRules,
    /// Gateway routing governance: model allowlist + downgrade exemptions
    /// (enterprise#25). Enforced by the org gateway under a signed policy.
    #[serde(default, skip_serializing_if = "RoutingPolicyRules::is_empty")]
    pub routing: RoutingPolicyRules,
    /// Hard org spend caps per person/project (enterprise#25).
    #[serde(default, skip_serializing_if = "BudgetRules::is_empty")]
    pub budgets: BudgetRules,
}

/// The `[context]` section of a pack. All fields optional — only what a pack
/// states is constrained; everything else stays at engine defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextRules {
    /// Default `ctx_read` mode the policy expects (see [`KNOWN_READ_MODES`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_read_mode: Option<String>,
    /// Allowlist of tool names; when set, only these may be called.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_tools: Option<Vec<String>>,
    /// Denylist of tool names; always additive down the `extends` chain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny_tools: Vec<String>,
    /// Upper bound on tokens a single context assembly may spend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<u32>,
    /// Audit-retention expectation in days (governance intent; the hosted
    /// plane enforces its own plan window — see org-audit-log-v1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_retention_days: Option<u32>,
}

/// The `[filters]` section — inbound content detectors (GL #675). Each action
/// is one of `off` / `warn` / `redact` / `block` (absent ⇒ `off`). Compiled
/// into a [`crate::core::input_filters::FilterConfig`] at load time and run on
/// tool output before it reaches the agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterRules {
    /// PII detection (Swiss AHV, IBAN, payment cards, email).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pii: Option<String>,
    /// Data-classification marking gate (confidential/secret banners).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<String>,
    /// Prompt-injection detection (OWASP LLM01) on inbound content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub injection: Option<String>,
    /// Classification labels that gate; overrides the built-in default set.
    /// Accumulates down the `extends` chain (a child may add, never drop).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_labels: Vec<String>,
    /// Named inbound block patterns. Evaluated on original content before any
    /// masking; inherited and organizational restrictions accumulate.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub blocked_patterns: BTreeMap<String, String>,
    /// Ownership of locally published personal rules; never infer ownership from
    /// a prefix used by an existing hand-authored policy.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub managed_personal_rules: BTreeSet<String>,
}

impl FilterRules {
    /// True when no filter is configured (all actions absent, no labels).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pii.is_none()
            && self.classification.is_none()
            && self.injection.is_none()
            && self.blocked_labels.is_empty()
            && self.blocked_patterns.is_empty()
            && self.managed_personal_rules.is_empty()
    }
}

/// Same-name restrictions retain both patterns. Bounds are checked again when
/// compiling the resolved policy; oversized composition denies all access.
pub(super) fn merge_block_patterns(
    target: &mut BTreeMap<String, String>,
    incoming: &BTreeMap<String, String>,
) {
    for (name, pattern) in incoming {
        match target.get_mut(name) {
            Some(previous) if previous != pattern => {
                *previous = format!("(?:{previous})|(?:{pattern})");
            }
            Some(_) => {}
            None => {
                target.insert(name.clone(), pattern.clone());
            }
        }
    }
}

/// The `[egress]` section — output/DLP enforcement on agent writes & actions
/// (GL #676). Gates `ctx_edit` writes and `ctx_shell` actions before they
/// execute. Compiled into a `crate::core::egress::EgressConfig` at load time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EgressRules {
    /// Regexes that block a write/action when matched (e.g. a prod-DB DSN).
    /// Accumulates down the `extends` chain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_patterns: Vec<String>,
    /// Block writes/actions carrying detected secrets or PII.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_secrets: Option<bool>,
    /// Rate limit: max agent write/action tool calls per 60 s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_writes_per_min: Option<u32>,
}

impl EgressRules {
    /// True when no egress rule is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.forbidden_patterns.is_empty()
            && self.block_secrets.is_none()
            && self.max_writes_per_min.is_none()
    }
}

/// The `[routing]` section — org gateway routing governance (enterprise#25,
/// Doc 08 §4.3). Enforced in the gateway forward path when this pack arrives
/// via a signed, trusted, `enforced = true` [`org::OrgPolicyV1`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingPolicyRules {
    /// Model allowlist patterns (`"claude-*"`, `"gpt-4o-mini"`). A request for
    /// a model matching none of them is refused org-wide. Empty = no
    /// restriction. Accumulates down the `extends` chain (union — the floor
    /// merge then intersects org vs. local, see `floor`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_models: Vec<String>,
    /// Additional conjunctive ceilings. Every group must match; an empty
    /// group denies every model. Preserves glob intersections without turning
    /// a disjoint intersection into the unrestricted empty primary list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_ceiling_groups: Vec<Vec<String>>,
    /// Projects whose requests the router must never downgrade to a cheaper
    /// tier (`["security", "prod"]`). Accumulates down the chain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbid_downgrade_for: Vec<String>,
}

impl RoutingPolicyRules {
    /// True when no routing governance is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.allowed_models.is_empty()
            && self.model_ceiling_groups.is_empty()
            && self.forbid_downgrade_for.is_empty()
    }

    /// Apply all inherited ceilings to one explicitly selected model.
    #[must_use]
    pub fn model_allowed(&self, model: &str) -> bool {
        model_allowed_by_ceilings(&self.allowed_models, &self.model_ceiling_groups, model)
    }
}

/// Keep the first occurrence of every exact conjunction, including deny-all
/// empty groups. Reapplying a floor must not multiply identical checks.
fn unique_model_ceilings<'a>(groups: impl Iterator<Item = &'a Vec<String>>) -> Vec<Vec<String>> {
    let mut seen = std::collections::BTreeSet::new();
    groups
        .filter(|group| seen.insert(*group))
        .cloned()
        .collect()
}

pub(crate) fn model_allowed_by_ceilings(
    primary: &[String],
    groups: &[Vec<String>],
    model: &str,
) -> bool {
    let matches = |patterns: &[String]| {
        patterns
            .iter()
            .any(|pattern| model_pattern_matches(pattern, model))
    };
    (primary.is_empty() || matches(primary)) && groups.iter().all(|group| matches(group))
}

/// Glob-lite matching shared by project admission and the gateway.
pub(crate) fn model_pattern_matches(pattern: &str, model: &str) -> bool {
    let pattern = pattern.trim().as_bytes();
    let model = model.trim().as_bytes();
    // Dynamic programming avoids exponential wildcard backtracking.
    let mut matched = vec![false; model.len() + 1];
    matched[0] = true;
    for &byte in pattern {
        if byte == b'*' {
            for index in 1..=model.len() {
                matched[index] |= matched[index - 1];
            }
        } else {
            for index in (1..=model.len()).rev() {
                matched[index] = matched[index - 1] && model[index - 1] == byte;
            }
            matched[0] = false;
        }
    }
    matched[model.len()]
}

/// The `[budgets]` section — hard org spend caps (enterprise#25, Doc 08 §4.3).
/// USD amounts against the measured `cost_usd` of the usage meter; breaching a
/// cap makes the gateway refuse further requests (429) until the window rolls.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetRules {
    /// Max measured spend per person per UTC day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd_per_person_per_day: Option<f64>,
    /// Max measured spend per project per UTC month.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd_per_project_per_month: Option<f64>,
    /// Max accepted requests per person per UTC minute (enterprise#66) —
    /// protects shared upstreams from a single runaway agent. Counted per
    /// gateway process; multi-replica deployments multiply accordingly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_requests_per_minute_per_person: Option<u32>,
}

impl BudgetRules {
    /// True when no cap is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.max_cost_usd_per_person_per_day.is_none()
            && self.max_cost_usd_per_project_per_month.is_none()
            && self.max_requests_per_minute_per_person.is_none()
    }
}

// ── Resolved view ────────────────────────────────────────────────────────────

/// A pack with its full `extends` chain folded in — what enforcement and
/// `policy show` consume.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedPolicy {
    pub name: String,
    pub version: String,
    pub description: String,
    /// Inheritance chain, base-most first (`["baseline", "strict-redaction"]`
    /// for a pack extending `strict-redaction`). Empty for root packs.
    pub chain: Vec<String>,
    pub default_read_mode: Option<String>,
    pub allow_tools: Option<Vec<String>>,
    pub deny_tools: Vec<String>,
    pub max_context_tokens: Option<u32>,
    pub audit_retention_days: Option<u32>,
    pub redaction: BTreeMap<String, String>,
    /// Folded inbound-filter actions + label set (GL #675).
    #[serde(default, skip_serializing_if = "FilterRules::is_empty")]
    pub filters: FilterRules,
    /// Folded egress/output DLP rules (GL #676).
    #[serde(default, skip_serializing_if = "EgressRules::is_empty")]
    pub egress: EgressRules,
    /// Folded gateway routing governance (enterprise#25).
    #[serde(default, skip_serializing_if = "RoutingPolicyRules::is_empty")]
    pub routing: RoutingPolicyRules,
    /// Folded org spend caps (enterprise#25).
    #[serde(default, skip_serializing_if = "BudgetRules::is_empty")]
    pub budgets: BudgetRules,
}

// ── Errors ───────────────────────────────────────────────────────────────────

/// Why a pack failed to parse, validate or resolve. Rendered verbatim by the
/// CLI, so every variant names the offending field and value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    Toml(String),
    InvalidName(String),
    InvalidVersion(String),
    EmptyDescription,
    UnknownReadMode(String),
    BadRegex { pattern_name: String, error: String },
    ZeroMaxTokens,
    AllowDenyOverlap(Vec<String>),
    UnknownParent(String),
    ExtendsCycle(Vec<String>),
    ExtendsTooDeep(usize),
    UnknownFilterAction { field: String, value: String },
    InvalidBudget { field: String, value: String },
    EmptyModelPattern,
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PolicyError::Toml(e) => write!(f, "not valid pack TOML: {e}"),
            PolicyError::InvalidName(n) => write!(
                f,
                "invalid pack name '{n}' (use lowercase letters, digits and hyphens)"
            ),
            PolicyError::InvalidVersion(v) => {
                write!(f, "invalid version '{v}' (expected MAJOR.MINOR.PATCH)")
            }
            PolicyError::EmptyDescription => write!(f, "description must not be empty"),
            PolicyError::UnknownReadMode(m) => write!(
                f,
                "unknown default_read_mode '{m}' (one of: {})",
                KNOWN_READ_MODES.join(", ")
            ),
            PolicyError::BadRegex {
                pattern_name,
                error,
            } => write!(
                f,
                "redaction pattern '{pattern_name}' is not a valid regex: {error}"
            ),
            PolicyError::ZeroMaxTokens => write!(f, "max_context_tokens must be greater than 0"),
            PolicyError::AllowDenyOverlap(tools) => write!(
                f,
                "tools listed in both allow_tools and deny_tools: {}",
                tools.join(", ")
            ),
            PolicyError::UnknownParent(p) => write!(
                f,
                "extends '{p}' does not name a known pack (built-ins: {})",
                builtin::names().join(", ")
            ),
            PolicyError::ExtendsCycle(chain) => {
                write!(f, "extends cycle: {}", chain.join(" -> "))
            }
            PolicyError::ExtendsTooDeep(d) => write!(
                f,
                "extends chain deeper than {MAX_EXTENDS_DEPTH} (found {d}) — flatten the hierarchy"
            ),
            PolicyError::UnknownFilterAction { field, value } => write!(
                f,
                "filters.{field} '{value}' is not a valid action (one of: off, warn, redact, block)"
            ),
            PolicyError::InvalidBudget { field, value } => write!(
                f,
                "budgets.{field} must be a positive, finite USD amount (got {value})"
            ),
            PolicyError::EmptyModelPattern => {
                write!(f, "routing.allowed_models must not contain empty patterns")
            }
        }
    }
}

impl std::error::Error for PolicyError {}

// ── Parse + validate ─────────────────────────────────────────────────────────

/// Parse one pack from TOML text (no I/O) and validate it standalone.
/// `extends` is checked against the built-ins during [`resolve`].
pub fn parse(toml_text: &str) -> Result<PolicyPack, PolicyError> {
    let pack: PolicyPack =
        toml::from_str(toml_text).map_err(|e| PolicyError::Toml(e.to_string()))?;
    validate(&pack)?;
    Ok(pack)
}

/// Parse a pack from a file path. Read errors surface as [`PolicyError::Toml`]
/// with the OS message — the CLI shows them verbatim.
pub fn parse_file(path: &Path) -> Result<PolicyPack, PolicyError> {
    let text = files::read(path, true)
        .map_err(PolicyError::Toml)?
        .ok_or_else(|| PolicyError::Toml("policy input is missing".into()))?;
    parse(&text)
}

/// Field-level validation of a single (unresolved) pack.
pub fn validate(pack: &PolicyPack) -> Result<(), PolicyError> {
    if pack.name.is_empty()
        || !pack
            .name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || pack.name.starts_with('-')
        || pack.name.ends_with('-')
    {
        return Err(PolicyError::InvalidName(pack.name.clone()));
    }
    if !valid_semver(&pack.version) {
        return Err(PolicyError::InvalidVersion(pack.version.clone()));
    }
    if pack.description.trim().is_empty() {
        return Err(PolicyError::EmptyDescription);
    }
    if let Some(mode) = pack.context.default_read_mode.as_deref()
        && !KNOWN_READ_MODES.contains(&mode)
    {
        return Err(PolicyError::UnknownReadMode(mode.to_string()));
    }
    if let Some(max) = pack.context.max_context_tokens
        && max == 0
    {
        return Err(PolicyError::ZeroMaxTokens);
    }
    if let Some(allow) = &pack.context.allow_tools {
        let deny: BTreeSet<&str> = pack.context.deny_tools.iter().map(String::as_str).collect();
        let overlap: Vec<String> = allow
            .iter()
            .filter(|t| deny.contains(t.as_str()))
            .cloned()
            .collect();
        if !overlap.is_empty() {
            return Err(PolicyError::AllowDenyOverlap(overlap));
        }
    }
    if pack.redaction.len() + pack.filters.blocked_patterns.len() > content::MAX_REDACTION_RULES
        || pack.egress.forbidden_patterns.len() > content::MAX_REDACTION_RULES
        || pack.filters.managed_personal_rules.len() > 32
    {
        return Err(PolicyError::BadRegex {
            pattern_name: "policy".into(),
            error: "too many policy patterns".into(),
        });
    }
    if pack.filters.managed_personal_rules.iter().any(|name| {
        !name.starts_with("personal_")
            || name.len() <= 9
            || name.len() > content::MAX_RULE_LABEL_BYTES
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
            || (!pack.redaction.contains_key(name)
                && !pack.filters.blocked_patterns.contains_key(name))
    }) {
        return Err(PolicyError::BadRegex {
            pattern_name: "policy".into(),
            error: "invalid personal rule ownership".into(),
        });
    }
    for (name, pattern) in pack.redaction.iter().chain(&pack.filters.blocked_patterns) {
        if name.is_empty()
            || name.len() > content::MAX_RULE_LABEL_BYTES
            || name.chars().any(char::is_control)
            || pattern.len() > content::MAX_RULE_PATTERN_BYTES
        {
            return Err(PolicyError::BadRegex {
                pattern_name: "redaction".into(),
                error: "policy pattern bounds exceeded".into(),
            });
        }
        if let Err(e) = regex::RegexBuilder::new(pattern)
            .size_limit(1024 * 1024)
            .dfa_size_limit(1024 * 1024)
            .build()
        {
            return Err(PolicyError::BadRegex {
                pattern_name: name.clone(),
                error: e.to_string(),
            });
        }
    }
    validate_filter_action("pii", pack.filters.pii.as_deref())?;
    validate_filter_action("classification", pack.filters.classification.as_deref())?;
    validate_filter_action("injection", pack.filters.injection.as_deref())?;
    for pattern in &pack.egress.forbidden_patterns {
        if pattern.len() > content::MAX_RULE_PATTERN_BYTES {
            return Err(PolicyError::BadRegex {
                pattern_name: "egress.forbidden_patterns".into(),
                error: "policy pattern bounds exceeded".into(),
            });
        }
        if let Err(e) = regex::RegexBuilder::new(pattern)
            .size_limit(1024 * 1024)
            .dfa_size_limit(1024 * 1024)
            .build()
        {
            return Err(PolicyError::BadRegex {
                pattern_name: format!("egress.forbidden_patterns: {pattern}"),
                error: e.to_string(),
            });
        }
    }
    if pack
        .routing
        .allowed_models
        .iter()
        .chain(pack.routing.model_ceiling_groups.iter().flatten())
        .any(|p| p.trim().is_empty())
    {
        return Err(PolicyError::EmptyModelPattern);
    }
    validate_budget(
        "max_cost_usd_per_person_per_day",
        pack.budgets.max_cost_usd_per_person_per_day,
    )?;
    validate_budget(
        "max_cost_usd_per_project_per_month",
        pack.budgets.max_cost_usd_per_project_per_month,
    )?;
    Ok(())
}

/// A budget cap must be a positive, finite USD amount.
fn validate_budget(field: &str, value: Option<f64>) -> Result<(), PolicyError> {
    if let Some(v) = value
        && !(v.is_finite() && v > 0.0)
    {
        return Err(PolicyError::InvalidBudget {
            field: field.to_string(),
            value: v.to_string(),
        });
    }
    Ok(())
}

/// Reject a `[filters]` action that is not a known token.
fn validate_filter_action(field: &str, value: Option<&str>) -> Result<(), PolicyError> {
    if let Some(v) = value
        && crate::core::input_filters::FilterAction::parse(v).is_none()
    {
        return Err(PolicyError::UnknownFilterAction {
            field: field.to_string(),
            value: v.to_string(),
        });
    }
    Ok(())
}

/// `MAJOR.MINOR.PATCH`, digits only — packs don't need pre-release tags.
fn valid_semver(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()))
}

// ── Resolve (extends) ────────────────────────────────────────────────────────

/// Fold a pack's `extends` chain (against the built-ins) into one
/// [`ResolvedPolicy`]. See the module docs for the inheritance semantics.
pub fn resolve(pack: &PolicyPack) -> Result<ResolvedPolicy, PolicyError> {
    // Walk to the root, collecting the chain (child first).
    let mut lineage: Vec<PolicyPack> = vec![pack.clone()];
    let mut seen: Vec<String> = vec![pack.name.clone()];
    let mut next_parent = pack.extends.clone();
    while let Some(parent_name) = next_parent.take() {
        if seen.contains(&parent_name) {
            seen.push(parent_name);
            return Err(PolicyError::ExtendsCycle(seen));
        }
        if lineage.len() >= MAX_EXTENDS_DEPTH {
            return Err(PolicyError::ExtendsTooDeep(lineage.len() + 1));
        }
        let parent =
            builtin::get(&parent_name).ok_or(PolicyError::UnknownParent(parent_name.clone()))?;
        seen.push(parent_name);
        next_parent.clone_from(&parent.extends);
        lineage.push(parent);
    }

    // Fold base-most first so children override scalars and accumulate
    // restrictions on top.
    let mut resolved = ResolvedPolicy {
        name: pack.name.clone(),
        version: pack.version.clone(),
        description: pack.description.clone(),
        chain: seen.iter().skip(1).rev().cloned().collect(),
        default_read_mode: None,
        allow_tools: None,
        deny_tools: Vec::new(),
        max_context_tokens: None,
        audit_retention_days: None,
        redaction: BTreeMap::new(),
        filters: FilterRules::default(),
        egress: EgressRules::default(),
        routing: RoutingPolicyRules::default(),
        budgets: BudgetRules::default(),
    };
    for layer in lineage.iter().rev() {
        if let Some(mode) = &layer.context.default_read_mode {
            resolved.default_read_mode = Some(mode.clone());
        }
        if let Some(allow) = &layer.context.allow_tools {
            resolved.allow_tools = Some(allow.clone());
        }
        for tool in &layer.context.deny_tools {
            if !resolved.deny_tools.contains(tool) {
                resolved.deny_tools.push(tool.clone());
            }
        }
        if let Some(max) = layer.context.max_context_tokens {
            resolved.max_context_tokens = Some(max);
        }
        if let Some(days) = layer.context.audit_retention_days {
            resolved.audit_retention_days = Some(days);
        }
        for (name, pattern) in &layer.redaction {
            resolved.redaction.insert(name.clone(), pattern.clone());
        }
        // Filter actions override (child wins); labels accumulate.
        if let Some(v) = &layer.filters.pii {
            resolved.filters.pii = Some(v.clone());
        }
        if let Some(v) = &layer.filters.classification {
            resolved.filters.classification = Some(v.clone());
        }
        if let Some(v) = &layer.filters.injection {
            resolved.filters.injection = Some(v.clone());
        }
        for label in &layer.filters.blocked_labels {
            if !resolved.filters.blocked_labels.contains(label) {
                resolved.filters.blocked_labels.push(label.clone());
            }
        }
        merge_block_patterns(
            &mut resolved.filters.blocked_patterns,
            &layer.filters.blocked_patterns,
        );
        resolved
            .filters
            .managed_personal_rules
            .clone_from(&layer.filters.managed_personal_rules);
        // Egress: forbidden patterns accumulate; scalars override (child wins).
        for pattern in &layer.egress.forbidden_patterns {
            if !resolved.egress.forbidden_patterns.contains(pattern) {
                resolved.egress.forbidden_patterns.push(pattern.clone());
            }
        }
        if let Some(v) = layer.egress.block_secrets {
            resolved.egress.block_secrets = Some(v);
        }
        if let Some(v) = layer.egress.max_writes_per_min {
            resolved.egress.max_writes_per_min = Some(v);
        }
        // Routing governance: restriction lists accumulate down the chain.
        for pattern in &layer.routing.allowed_models {
            if !resolved.routing.allowed_models.contains(pattern) {
                resolved.routing.allowed_models.push(pattern.clone());
            }
        }
        resolved.routing.model_ceiling_groups = unique_model_ceilings(
            resolved
                .routing
                .model_ceiling_groups
                .iter()
                .chain(layer.routing.model_ceiling_groups.iter()),
        );
        for project in &layer.routing.forbid_downgrade_for {
            if !resolved.routing.forbid_downgrade_for.contains(project) {
                resolved.routing.forbid_downgrade_for.push(project.clone());
            }
        }
        // Budgets: scalar caps override (child wins) — the floor merge takes
        // the stricter side across org vs. local separately.
        if let Some(v) = layer.budgets.max_cost_usd_per_person_per_day {
            resolved.budgets.max_cost_usd_per_person_per_day = Some(v);
        }
        if let Some(v) = layer.budgets.max_cost_usd_per_project_per_month {
            resolved.budgets.max_cost_usd_per_project_per_month = Some(v);
        }
    }

    // A resolved allowlist must not collide with accumulated denies.
    if let Some(allow) = &resolved.allow_tools {
        let overlap: Vec<String> = allow
            .iter()
            .filter(|t| resolved.deny_tools.contains(*t))
            .cloned()
            .collect();
        if !overlap.is_empty() {
            return Err(PolicyError::AllowDenyOverlap(overlap));
        }
    }
    Ok(resolved)
}

/// Parse + validate + resolve in one step — the common CLI path.
pub fn load(toml_text: &str) -> Result<ResolvedPolicy, PolicyError> {
    resolve(&parse(toml_text)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal(name: &str, extends: Option<&str>) -> PolicyPack {
        PolicyPack {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            description: "test pack".to_string(),
            extends: extends.map(str::to_string),
            context: ContextRules::default(),
            redaction: BTreeMap::new(),
            filters: FilterRules::default(),
            egress: EgressRules::default(),
            routing: RoutingPolicyRules::default(),
            budgets: BudgetRules::default(),
        }
    }

    #[test]
    fn parses_a_full_pack() {
        let pack = parse(
            r#"
name = "acme-internal"
version = "2.1.0"
description = "ACME internal baseline"
extends = "strict-redaction"

[context]
default_read_mode = "map"
deny_tools = ["ctx_url_read"]
max_context_tokens = 12000
audit_retention_days = 365

[redaction]
employee_id = 'EMP-\d{6}'
"#,
        )
        .expect("parses");
        assert_eq!(pack.name, "acme-internal");
        assert_eq!(pack.extends.as_deref(), Some("strict-redaction"));
        assert_eq!(pack.context.deny_tools, vec!["ctx_url_read"]);
        assert!(pack.redaction.contains_key("employee_id"));
    }

    #[test]
    fn parses_gateway_governance_sections() {
        // enterprise#25: [routing] + [budgets] ride inside the same signed pack.
        let resolved = load(
            r#"
name = "acme-gateway"
version = "1.0.0"
description = "org gateway governance"

[routing]
allowed_models = ["claude-*", "gpt-4o-mini"]
forbid_downgrade_for = ["prod"]

[budgets]
max_cost_usd_per_person_per_day = 50.0
max_cost_usd_per_project_per_month = 20000.0
"#,
        )
        .expect("resolves");
        assert_eq!(
            resolved.routing.allowed_models,
            vec!["claude-*".to_string(), "gpt-4o-mini".to_string()]
        );
        assert_eq!(resolved.routing.forbid_downgrade_for, vec!["prod"]);
        assert_eq!(resolved.budgets.max_cost_usd_per_person_per_day, Some(50.0));
        assert_eq!(
            resolved.budgets.max_cost_usd_per_project_per_month,
            Some(20000.0)
        );
    }

    #[test]
    fn rejects_invalid_budget_and_empty_model_pattern() {
        let neg = parse(
            r#"
name = "bad-budget"
version = "1.0.0"
description = "x"

[budgets]
max_cost_usd_per_person_per_day = -5.0
"#,
        );
        assert!(matches!(neg, Err(PolicyError::InvalidBudget { .. })));

        let empty = parse(
            r#"
name = "bad-pattern"
version = "1.0.0"
description = "x"

[routing]
allowed_models = ["claude-*", " "]
"#,
        );
        assert_eq!(empty.unwrap_err(), PolicyError::EmptyModelPattern);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = parse(
            r#"
name = "typo"
version = "1.0.0"
description = "x"

[context]
alow_tools = ["ctx_read"]
"#,
        )
        .unwrap_err();
        assert!(matches!(err, PolicyError::Toml(_)), "{err}");
    }

    #[test]
    fn validation_catches_each_field() {
        let mut p = minimal("Bad Name", None);
        assert!(matches!(validate(&p), Err(PolicyError::InvalidName(_))));

        p = minimal("ok", None);
        p.version = "1.0".into();
        assert!(matches!(validate(&p), Err(PolicyError::InvalidVersion(_))));

        p = minimal("ok", None);
        p.description = "  ".into();
        assert!(matches!(validate(&p), Err(PolicyError::EmptyDescription)));

        p = minimal("ok", None);
        p.context.default_read_mode = Some("lines:1-5".into());
        assert!(matches!(validate(&p), Err(PolicyError::UnknownReadMode(_))));

        p = minimal("ok", None);
        p.context.max_context_tokens = Some(0);
        assert!(matches!(validate(&p), Err(PolicyError::ZeroMaxTokens)));

        p = minimal("ok", None);
        p.redaction.insert("broken".into(), "(unclosed".into());
        assert!(matches!(validate(&p), Err(PolicyError::BadRegex { .. })));

        p = minimal("ok", None);
        p.context.allow_tools = Some(vec!["ctx_read".into()]);
        p.context.deny_tools = vec!["ctx_read".into()];
        assert!(matches!(
            validate(&p),
            Err(PolicyError::AllowDenyOverlap(_))
        ));
    }

    #[test]
    fn resolve_overrides_scalars_and_accumulates_denies() {
        let mut child = minimal("child", Some("finance-eu"));
        child.context.default_read_mode = Some("signatures".into());
        child.context.deny_tools = vec!["ctx_shell".into()];
        let r = resolve(&child).expect("resolves");

        // Scalar overridden by the child.
        assert_eq!(r.default_read_mode.as_deref(), Some("signatures"));
        // finance-eu's denies survive; the child's add on top.
        assert!(r.deny_tools.contains(&"ctx_url_read".to_string()));
        assert!(r.deny_tools.contains(&"ctx_shell".to_string()));
        // Redaction accumulated from the whole chain (baseline + strict + finance).
        assert!(r.redaction.contains_key("iban"));
        assert!(r.redaction.contains_key("private_key"));
        // Chain is base-most first and excludes the pack itself.
        assert_eq!(r.chain, vec!["baseline", "strict-redaction", "finance-eu"]);
    }

    #[test]
    fn resolve_rejects_unknown_parent_and_cycle() {
        let p = minimal("orphan", Some("no-such-pack"));
        assert!(matches!(resolve(&p), Err(PolicyError::UnknownParent(_))));

        // Self-reference is the minimal cycle reachable without registering
        // custom packs (built-ins are acyclic by construction + test below).
        let p = minimal("loop", Some("loop"));
        assert!(matches!(resolve(&p), Err(PolicyError::ExtendsCycle(_))));
    }

    #[test]
    fn child_redaction_overrides_same_named_parent_pattern() {
        let mut child = minimal("child", Some("baseline"));
        child
            .redaction
            .insert("private_key".into(), "MY-OWN-KEY-\\d+".into());
        let r = resolve(&child).expect("resolves");
        assert_eq!(r.redaction.get("private_key").unwrap(), "MY-OWN-KEY-\\d+");
    }
}
