// SPDX-License-Identifier: Apache-2.0
//! Dependencies of recorded invocations, not a completeness claim for narrative.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, ensure};
use lean_ctx_protocol::{
    EngineContextSourcePlanResponseV1, EngineContextSourceTypeV1, EngineInvocationV1,
    ExecutionPlanV1, Sha256Digest,
};

use super::{VerifiedReceiptDocumentV1, canonical_serialize, digest};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceKind {
    File,
    Provider,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceDependency {
    pub(crate) kind: SourceKind,
    pub(crate) digest: Sha256Digest,
    // Equal object refs in separate receipts must not collapse distinct sources.
    source_id: Option<lean_ctx_protocol::SourceId>,
}

pub(super) struct RecordedSources {
    pub(super) references: BTreeSet<String>,
    pub(super) dependencies: BTreeMap<String, SourceDependency>,
}

fn document<T: serde::de::DeserializeOwned + serde::Serialize>(
    hash: &Sha256Digest,
    read: &impl Fn(&Sha256Digest) -> Result<Vec<u8>>,
) -> Result<T> {
    let bytes = read(hash)?;
    ensure!(
        bytes.len() <= super::MAX_ENTRY_BYTES
            && digest(&bytes).map_err(anyhow::Error::msg)? == *hash,
        "source evidence digest mismatch"
    );
    let value: T = serde_json::from_slice(&bytes)?;
    ensure!(
        canonical_serialize(&value) == bytes,
        "noncanonical source evidence"
    );
    Ok(value)
}

pub(super) fn derive(
    receipts: &[VerifiedReceiptDocumentV1],
    read: impl Fn(&Sha256Digest) -> Result<Vec<u8>>,
) -> Result<RecordedSources> {
    let mut result = RecordedSources {
        references: BTreeSet::new(),
        dependencies: BTreeMap::new(),
    };
    for receipt in receipts {
        let lineage = &receipt.document().lineage;
        let invocation: EngineInvocationV1 = document(&lineage.invocation_ref, &read)?;
        invocation.validate()?;
        result
            .references
            .insert(lineage.invocation_ref.as_str().into());
        let plan: ExecutionPlanV1 = document(&lineage.plan_ref, &read)?;
        let task_ref = format!("task:{}", lineage.task_ref.as_str());
        let plan_ref = format!("plan:{}", lineage.plan_ref.as_str());
        ensure!(
            invocation
                .source_refs
                .iter()
                .any(|r| r.as_str() == task_ref)
                && invocation
                    .source_refs
                    .iter()
                    .any(|r| r.as_str() == plan_ref),
            "invocation source evidence lineage mismatch"
        );
        let sources: Vec<_> = invocation
            .source_refs
            .iter()
            .filter(|r| {
                *r != &invocation.input_ref && r.as_str() != task_ref && r.as_str() != plan_ref
            })
            .collect();
        ensure!(sources.len() == 1, "invocation source coverage unsupported");
        let reference = sources[0].as_str();
        if let Some(hex) = reference.strip_prefix("source:canonical-path-sha256:") {
            Sha256Digest::new(format!("sha256:{hex}"))?;
            ensure!(
                invocation.input_ref.as_str()
                    == format!(
                        "input:ctx-read-snapshot-sha256:{}",
                        invocation.input_digest.hex()
                    ),
                "transformed source snapshot requires new admission"
            );
            insert(
                &mut result.dependencies,
                reference,
                SourceDependency {
                    kind: SourceKind::File,
                    digest: invocation.input_digest,
                    source_id: None,
                },
            )?;
        } else if let Some(hex) = reference.strip_prefix("artifact://execution/evidence/") {
            ensure!(
                invocation.input_ref.as_str()
                    == format!(
                        "input:source-materialization-sha256:{}",
                        invocation.input_digest.hex()
                    ),
                "unsupported source materialization"
            );
            let hash = Sha256Digest::new(format!("sha256:{hex}"))?;
            let source_plan: EngineContextSourcePlanResponseV1 = document(&hash, &read)?;
            source_plan.validate_binding()?;
            ensure!(
                source_plan.result.plan.task_id == plan.task_id
                    && Some(&source_plan.result.plan.context_plan_id)
                        == plan.context_plan_id.as_ref()
                    && !source_plan.source_bindings.is_empty(),
                "source plan lineage or coverage mismatch"
            );
            result.references.insert(hash.as_str().into());
            for binding in source_plan.source_bindings {
                let kind = match binding.source_type {
                    EngineContextSourceTypeV1::Filesystem => SourceKind::File,
                    EngineContextSourceTypeV1::IssueTracker
                    | EngineContextSourceTypeV1::RelationalDatabase => SourceKind::Provider,
                    EngineContextSourceTypeV1::Other => SourceKind::Unsupported,
                };
                insert(
                    &mut result.dependencies,
                    binding.object_ref.as_str(),
                    SourceDependency {
                        kind,
                        digest: binding.content_digest,
                        source_id: Some(binding.source_id),
                    },
                )?;
            }
        } else {
            anyhow::bail!("invocation source coverage unsupported");
        }
    }
    ensure!(
        !result.dependencies.is_empty() && result.dependencies.len() <= 128,
        "recorded source coverage unavailable"
    );
    Ok(result)
}

fn insert(
    map: &mut BTreeMap<String, SourceDependency>,
    reference: &str,
    dependency: SourceDependency,
) -> Result<()> {
    if let Some(existing) = map.get(reference) {
        ensure!(
            *existing == dependency,
            "conflicting source revisions require fresh context"
        );
    } else {
        map.insert(reference.into(), dependency);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_object_refs_and_content_cannot_collapse_distinct_sources() {
        let mut dependencies = BTreeMap::new();
        let first = SourceDependency {
            kind: SourceKind::Provider,
            digest: digest(b"same content").unwrap(),
            source_id: Some(lean_ctx_protocol::SourceId::new("provider-a").unwrap()),
        };
        insert(&mut dependencies, "object:issue-1", first.clone()).unwrap();
        insert(&mut dependencies, "object:issue-1", first.clone()).unwrap();
        let mut second = first.clone();
        second.source_id = Some(lean_ctx_protocol::SourceId::new("provider-b").unwrap());
        assert!(insert(&mut dependencies, "object:issue-1", second).is_err());
        assert_eq!(dependencies.get("object:issue-1"), Some(&first));
    }
}
