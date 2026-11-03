// SPDX-License-Identifier: Apache-2.0
//! The installed Hive filter set as policy rules: `<data_dir>/hive/policy/filters/set.json`,
//! written only by the Hive connector after it verified a policy bundle (signed packs, digests,
//! complete validation, at most [`HIVE_MAX_RULES`] rules). This side only reads it: the policy
//! snapshot applies its patterns ([`filter_rules`]) and refuses a set that is not as signed
//! ([`required_packs_installed`]).

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use lean_ctx_protocol::hive::policy::{FilterActionV1, FilterPackV1, RequiredFilterPackV1};

pub const FILTER_SET_SCHEMA: &str = "leanctx.hive.filter-set/v1";
/// Hive filter rules have their own budget, apart from the user's policy
/// rules; a bundle above it is refused rather than applied in part.
pub const HIVE_MAX_RULES: usize = 128;
pub const FILTER_SET_FILE: &str = "set.json";

/// The installed filter set of `data_dir`, as text (what the policy
/// snapshot reads), if there is one.
pub fn load_filter_set(data_dir: &Path) -> Option<String> {
    std::fs::read_to_string(
        data_dir
            .join("policy")
            .join("filters")
            .join(FILTER_SET_FILE),
    )
    .ok()
}

/// The verified filter packs in force, as one file the policy snapshot reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterSet {
    pub schema: String,
    pub packs: Vec<FilterPackV1>,
    /// `pack_id → statement_b64`: the exact signed bytes of each pack, so
    /// [`required_packs_installed`] can recompute the digest the org policy
    /// signed and check that the applied pack is the one those bytes hold.
    /// Absent in sets written before C 1.8.
    #[serde(default)]
    pub statements: std::collections::BTreeMap<String, String>,
}

/// `<data_dir>/hive/policy/filters/set.json` of this installation.
pub fn filter_set_path() -> Option<std::path::PathBuf> {
    let data = crate::core::paths::data_dir().ok()?;
    Some(
        data.join("hive")
            .join("policy")
            .join("filters")
            .join(FILTER_SET_FILE),
    )
}

/// Hive filter patterns as policy rules.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterRules {
    /// `(rule label, pattern)`: replace matches with `[REDACTED:<label>]`.
    pub redaction: Vec<(String, String)>,
    /// `(rule label, pattern)`: refuse content that matches.
    pub blocked: Vec<(String, String)>,
    /// Defensive counter: patterns of a set that did not validate here or
    /// whose label repeats. Installation refuses such sets, so this is 0
    /// unless the file was changed by hand; report it as a diagnostic.
    pub dropped: usize,
}

/// Rules from the captured bytes of the filter set. Labels are unique
/// (`hive:<label or id>`, the first pack in id order wins) and every
/// pattern compiled at installation within the policy limits.
pub fn filter_rules(set: Option<&str>) -> FilterRules {
    let mut rules = FilterRules::default();
    let Some(text) = set else {
        return rules;
    };
    let Ok(set) = serde_json::from_str::<FilterSet>(text) else {
        rules.dropped = 1;
        return rules;
    };
    let mut seen = std::collections::BTreeSet::new();
    for pack in &set.packs {
        if pack.validate().is_err() {
            rules.dropped += pack.patterns.len().max(1);
            continue;
        }
        for pattern in &pack.patterns {
            let label = pattern.rule_label();
            if !seen.insert(label.clone()) || seen.len() > HIVE_MAX_RULES {
                rules.dropped += 1;
                continue;
            }
            let rule = (label, pattern.pattern.clone());
            match pattern.action {
                FilterActionV1::Redact => rules.redaction.push(rule),
                FilterActionV1::Block => rules.blocked.push(rule),
            }
        }
    }
    rules
}

/// The filter rules of this installation, read now (for screening, which
/// has no policy snapshot of its own).
pub fn installed_filter_rules() -> FilterRules {
    let text = filter_set_path().and_then(|path| std::fs::read_to_string(path).ok());
    filter_rules(text.as_deref())
}

pub fn digest_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The policy runtime's check for an org policy `artifact` it still holds
/// from before Hive-Protocol 2.1 (verified there, not here): for every
/// filter pack its signed statement requires, the installed `set` must
/// hold statement bytes whose SHA-256 is the signed digest, and the pack
/// it applies must be exactly the one those bytes decode to. A set whose
/// patterns, bytes or listing were changed or removed by hand is refused
/// like any other tampering.
pub fn required_packs_installed(artifact: &str, set: Option<&str>) -> Result<(), String> {
    let artifact: Value =
        serde_json::from_str(artifact).map_err(|_| "org policy input is invalid")?;
    let required: Vec<RequiredFilterPackV1> = match artifact
        .get("statement")
        .and_then(|statement| statement.get("required_filter_packs"))
    {
        Some(list) => serde_json::from_value(list.clone())
            .map_err(|_| "org policy input is invalid".to_string())?,
        None if artifact.get("statement").is_some() => Vec::new(),
        None => return Err("org policy input is invalid".into()),
    };
    if required.is_empty() {
        return Ok(());
    }
    let set: FilterSet = set
        .and_then(|text| serde_json::from_str(text).ok())
        .ok_or("the org policy requires filter packs and none are installed")?;
    if !applies_in_full(&set) {
        return Err("the installed filter set is not as verified".into());
    }
    if let Some(r) = required.iter().find(|r| !installed_as_signed(&set, r)) {
        return Err(format!(
            "required filter pack {} version {} is not installed as signed",
            r.pack_id, r.version
        ));
    }
    Ok(())
}

/// The set applies in full, as verify_bundle wrote it: filter_rules drops
/// repeated labels and rules beyond the budget first come, first served, so
/// a shadow pack placed before a required one could otherwise silence its
/// rules while the required pack is still "there".
fn applies_in_full(set: &FilterSet) -> bool {
    let mut ids = std::collections::BTreeSet::new();
    let mut labels = std::collections::BTreeSet::new();
    for pack in &set.packs {
        if !ids.insert(pack.pack_id.as_str()) || pack.validate().is_err() {
            return false;
        }
        for pattern in &pack.patterns {
            if !labels.insert(pattern.rule_label()) {
                return false;
            }
        }
    }
    labels.len() <= HIVE_MAX_RULES
}

/// The set holds statement bytes whose SHA-256 is the signed digest, and
/// applies exactly the pack those bytes decode to.
fn installed_as_signed(set: &FilterSet, r: &RequiredFilterPackV1) -> bool {
    set.statements
        .get(&r.pack_id)
        .and_then(|b64| STANDARD.decode(b64).ok())
        .filter(|bytes| digest_hex(bytes) == r.sha256)
        .and_then(|bytes| serde_json::from_slice::<FilterPackV1>(&bytes).ok())
        .is_some_and(|pack| {
            pack.pack_id == r.pack_id && pack.version == r.version && set.packs.contains(&pack)
        })
}
