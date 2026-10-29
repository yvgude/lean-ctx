// SPDX-License-Identifier: Apache-2.0
//! Authenticated compilation transport; policy execution remains in the public engine.
use super::{InstallError, Result};
use std::collections::BTreeMap;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Compiled {
    schema_version: u32,
    pub rule_count: usize,
    pub redaction: BTreeMap<String, String>,
    pub blocked_patterns: BTreeMap<String, String>,
}

const CAPABILITY: super::exchange::Capability = super::exchange::Capability {
    id: "pro.runtime.personal_protection",
    version: "1.0.0",
    declared: super::health::supports_protection,
    deadline_ms: 5000,
    tag: "protection",
};

pub(crate) fn compile(raw: &str) -> Result<Compiled> {
    if raw.len() > 128 * 1024 {
        return Err(InstallError::Policy);
    }
    let input: serde_json::Value = serde_json::from_str(raw).map_err(|_| InstallError::Policy)?;
    let rules = input["rules"].as_array().ok_or(InstallError::Policy)?;
    if input["schema_version"] != 1 || rules.is_empty() || rules.len() > 32 {
        return Err(InstallError::Policy);
    }
    let output = super::exchange::invoke(&CAPABILITY, raw)?;
    let compiled: Compiled = serde_json::from_str(&output).map_err(|_| InstallError::Exchange)?;
    if compiled.schema_version != 1
        || compiled.rule_count != rules.len()
        || compiled.redaction.len() + compiled.blocked_patterns.len() != rules.len()
    {
        return Err(InstallError::Exchange);
    }
    let mut ids = std::collections::BTreeSet::new();
    for rule in rules {
        let id = rule["id"].as_str().ok_or(InstallError::Exchange)?;
        if id.is_empty()
            || id.len() > 48
            || !id.as_bytes()[0].is_ascii_lowercase()
            || !id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
            || !ids.insert(id)
        {
            return Err(InstallError::Exchange);
        }
        let map = match rule["action"].as_str() {
            Some("mask") => &compiled.redaction,
            Some("block") => &compiled.blocked_patterns,
            _ => return Err(InstallError::Exchange),
        };
        if !map.contains_key(id) {
            return Err(InstallError::Exchange);
        }
    }
    for pattern in compiled
        .redaction
        .values()
        .chain(compiled.blocked_patterns.values())
    {
        if pattern.len() > 4096 {
            return Err(InstallError::Exchange);
        }
        let regex = regex::RegexBuilder::new(pattern)
            .size_limit(1024 * 1024)
            .dfa_size_limit(1024 * 1024)
            .build()
            .map_err(|_| InstallError::Exchange)?;
        if regex.is_match("") {
            return Err(InstallError::Exchange);
        }
    }
    Ok(compiled)
}
