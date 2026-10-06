// SPDX-License-Identifier: Apache-2.0
//! Current project content admission for immutable signed transfer evidence.
//!
//! A rewrite cannot preserve the source signature/digests. Refuse that package
//! and require a newly prepared safe checkpoint; never silently rewrite history.
//! This is content admission only, not current access to its original sources.

use std::path::Path;

use anyhow::{Result, ensure};
use serde_json::Value;

use super::CheckpointTransferPackageV1;
use crate::core::policy::{content, diagnostics, runtime};

pub(super) fn admit(package: &CheckpointTransferPackageV1, root: &Path) -> Result<()> {
    if let Ok(Some(bound)) = runtime::REQUEST_PROJECT.try_with(|slot| slot.borrow().clone()) {
        ensure!(bound == root, "checkpoint policy scope mismatch");
    }
    let root = root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("checkpoint policy root unavailable"))?;
    runtime::with_project_source_view(root, || {
        ensure!(
            runtime::REQUEST_PROJECT
                .try_with(|slot| slot.borrow().as_deref() == Some(Path::new(root)))
                .unwrap_or(false),
            "checkpoint policy scope mismatch"
        );
        ensure!(
            crate::core::roles::active_role().is_tool_allowed("ctx_read"),
            "checkpoint content withheld"
        );
        let active = runtime::active();
        if let Some(policy) = active.as_ref() {
            ensure!(
                policy.tool_allowed("ctx_read"),
                "checkpoint content withheld"
            );
        }
        // Inspect the transport as well as each decoded embedded document. A
        // regex over serialized JSON alone misses escapes and anchored fields.
        inspect(&serde_json::to_value(package)?, active.as_deref())?;
        inspect(
            &serde_json::from_str(&package.envelope_json)?,
            active.as_deref(),
        )?;
        for entry in package.receipts.iter().chain(&package.evidence) {
            inspect(&serde_json::from_str(&entry.json)?, active.as_deref())?;
        }
        Ok(())
    })
    .map_err(|_| anyhow::anyhow!("checkpoint policy changed or unavailable"))?
}

pub(super) fn inspect_personal_sync_payload(root: &str, value: &Value) -> Result<()> {
    if let Ok(Some(bound)) = runtime::REQUEST_PROJECT.try_with(|slot| slot.borrow().clone()) {
        ensure!(
            bound == Path::new(root),
            "personal sync policy scope mismatch"
        );
    }
    runtime::with_project_source_view(root, || {
        ensure!(
            runtime::REQUEST_PROJECT
                .try_with(|slot| slot.borrow().as_deref() == Some(Path::new(root)))
                .unwrap_or(false),
            "personal sync policy scope mismatch"
        );
        ensure!(
            crate::core::roles::active_role().is_tool_allowed("ctx_read"),
            "personal content withheld"
        );
        let active = runtime::active();
        if let Some(policy) = active.as_ref() {
            ensure!(policy.tool_allowed("ctx_read"), "personal content withheld");
        }
        inspect(value, active.as_deref())
    })
    .map_err(|_| anyhow::anyhow!("personal sync policy changed or unavailable"))?
}

fn inspect(value: &Value, active: Option<&runtime::ActivePolicy>) -> Result<()> {
    // The existing bounded walker inspects decoded keys, strings and scalars;
    // equality is mandatory because all these documents are digest-bound.
    ensure!(
        diagnostics::inspect(value, active).as_ref() == Some(value),
        "checkpoint content withheld"
    );
    inspect_secrets(value)?;
    // Also check the complete document so multi-field patterns cannot bypass
    // the field-by-field scan. Warnings are not mutations or blocking rules.
    if let Some(active) = active {
        let text = serde_json::to_string(value)?;
        let result = content::evaluate_text(&text, active);
        ensure!(
            !result.blocked && result.text == text,
            "checkpoint content withheld"
        );
    }
    Ok(())
}

fn inspect_secrets(value: &Value) -> Result<()> {
    // This walk follows the shared bounded inspector above, so recursion and
    // total input have already been limited. Community retains its secret floor.
    let text = |value: &str| {
        ensure!(
            crate::core::secret_detection::detect_secrets(value).is_empty(),
            "checkpoint contains credential-shaped material"
        );
        Ok(())
    };
    match value {
        Value::String(value) => text(value),
        Value::Array(values) => values.iter().try_for_each(inspect_secrets),
        Value::Object(values) => values.iter().try_for_each(|(key, value)| {
            text(key)?;
            inspect_secrets(value)
        }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(extra: &str) -> runtime::ActivePolicy {
        runtime::ActivePolicy::from_resolved(
            crate::core::policy::load(&format!(
                "name = 'checkpoint'\nversion = '1.0.0'\ndescription = 'test'\n{extra}"
            ))
            .unwrap(),
        )
    }

    #[test]
    fn decoded_fields_cannot_hide_anchored_rules_or_secrets() {
        let active = policy("[redaction]\ncustomer = '^K-[0-9]{6}$'");
        let value: Value = serde_json::from_str(r#"{"summary":"\u004b-482193"}"#).unwrap();
        assert!(inspect(&value, Some(&active)).is_err());
        assert!(
            inspect(
                &serde_json::json!({"summary":"safe continuation"}),
                Some(&active)
            )
            .is_ok()
        );
        // Assembled at runtime so no source line carries a token-shaped literal
        // that repository secret scanners would flag.
        let token = format!("{}{}", "gh", "p_abcdefghijklmnopqrstuvwxyz1234567890");
        assert!(inspect(&serde_json::json!({ "note": token }), None).is_err());
    }

    #[test]
    fn signed_document_is_never_rewritten_and_blocking_stays_distinct() {
        let mask = policy("[redaction]\ncustomer = 'K-[0-9]{6}'");
        let block = policy("[filters]\nclassification = 'block'");
        let value = serde_json::json!({"notes":["K-482193", "CONFIDENTIAL"]});
        let before = value.clone();
        assert!(inspect(&value, Some(&mask)).is_err());
        assert!(inspect(&value, Some(&block)).is_err());
        assert_eq!(value, before);
    }
}
