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
pub mod coverage;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

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
    let text = std::fs::read_to_string(path)
        .map_err(|e| PolicyError::Toml(format!("{}: {e}", path.display())))?;
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
    for (name, pattern) in &pack.redaction {
        if let Err(e) = regex::Regex::new(pattern) {
            return Err(PolicyError::BadRegex {
                pattern_name: name.clone(),
                error: e.to_string(),
            });
        }
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
