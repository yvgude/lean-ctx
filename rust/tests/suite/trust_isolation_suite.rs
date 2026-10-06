// SPDX-License-Identifier: Apache-2.0
//! Public trust-surface isolation and policy tests.

use ed25519_dalek::SigningKey;
use lean_ctx::core::trust::{
    AllowAll, ClassificationBased, ClassificationPolicy, ClassifiedData, CrossTenantLeak, DenyAll,
    EvidenceChain, IdentityContext, PolicyDecision, PolicyEvaluator, PolicyRule,
    ResidencyRequirement, SecurityClassification, TenantBoundary, TenantId, assert_same_tenant,
};

#[derive(Debug)]
struct TaskReference {
    tenant: TenantId,
    task_id: String,
}

impl TaskReference {
    fn new(tenant: impl Into<TenantId>, task_id: impl Into<String>) -> Self {
        Self {
            tenant: tenant.into(),
            task_id: task_id.into(),
        }
    }
}

impl TenantBoundary for TaskReference {
    fn tenant_id(&self) -> &TenantId {
        &self.tenant
    }
}

#[derive(Debug)]
struct ReceiptData {
    tenant: TenantId,
    task_id: String,
}

impl ReceiptData {
    fn for_task(
        receipt_tenant: impl Into<TenantId>,
        task: &TaskReference,
    ) -> Result<Self, CrossTenantLeak> {
        let receipt = Self {
            tenant: receipt_tenant.into(),
            task_id: task.task_id.clone(),
        };
        assert_same_tenant(&receipt, task)?;
        Ok(receipt)
    }
}

impl TenantBoundary for ReceiptData {
    fn tenant_id(&self) -> &TenantId {
        &self.tenant
    }
}

#[derive(Debug)]
struct KnowledgeObject {
    tenant: TenantId,
    content_type: String,
    body: String,
}

impl TenantBoundary for KnowledgeObject {
    fn tenant_id(&self) -> &TenantId {
        &self.tenant
    }
}

fn actor(tenant: &str, agent: &str) -> IdentityContext {
    IdentityContext::new(tenant)
        .with_agent(agent)
        .with_session("trust-test-session")
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

#[test]
fn receipt_data_cannot_reference_cross_tenant_task_ids() {
    let task = TaskReference::new("tenant-a", "task-a");

    let same_tenant = ReceiptData::for_task("tenant-a", &task)
        .expect("same-tenant receipt reference should be accepted");
    assert_eq!(same_tenant.task_id, "task-a");

    let cross_tenant = ReceiptData::for_task("tenant-b", &task);
    assert!(
        cross_tenant.is_err(),
        "cross-tenant task reference must be rejected"
    );
}

#[test]
fn knowledge_objects_respect_classification_and_residency_boundaries() {
    let policy = ClassificationPolicy::new(SecurityClassification::Internal)
        .with_rule("text/plain", SecurityClassification::Public)
        .with_rule(
            "application/knowledge+json",
            SecurityClassification::Confidential,
        );
    assert_eq!(
        policy.classify("text/plain"),
        SecurityClassification::Public
    );
    assert_eq!(
        policy.auto_classify("application/knowledge+json"),
        SecurityClassification::Confidential
    );

    let object = KnowledgeObject {
        tenant: TenantId::from("tenant-a"),
        content_type: "application/knowledge+json".to_owned(),
        body: "synthetic knowledge".to_owned(),
    };
    let classified = ClassifiedData::new(
        object,
        policy.classify("application/knowledge+json"),
        ResidencyRequirement::sovereign(["eu-central"]),
    );

    assert_eq!(
        classified.classification,
        SecurityClassification::Confidential
    );
    assert!(classified.is_allowed_in("eu-central"));
    assert!(!classified.is_allowed_in("us-east"));
    assert_eq!(classified.data.content_type, "application/knowledge+json");
    assert_eq!(classified.data.body, "synthetic knowledge");
}

#[test]
fn evidence_chain_entries_are_tenant_scoped_and_independently_verified() {
    let signing_key = key(7);
    let mut chain = EvidenceChain::new();
    chain
        .append_signed(
            1,
            actor("tenant-a", "agent-a"),
            "task.started",
            "blake3:task-a",
            &signing_key,
        )
        .expect("first evidence entry should append");
    chain
        .append_signed(
            2,
            actor("tenant-a", "agent-a"),
            "task.completed",
            "blake3:receipt-a",
            &signing_key,
        )
        .expect("same-tenant evidence entry should append");

    let valid = chain.verify();
    assert!(valid.valid, "signed chain should verify: {valid:?}");
    assert_eq!(chain.tenant_id(), Some(&TenantId::from("tenant-a")));

    let cross_tenant = lean_ctx::core::trust::EvidenceEntry::signed(
        3,
        actor("tenant-b", "agent-b"),
        "task.foreign",
        "blake3:foreign",
        chain.head_hash(),
        &signing_key,
    );
    assert!(
        chain.append_scoped(cross_tenant).is_err(),
        "a chain must reject an entry from another tenant"
    );
}

#[test]
fn policy_evaluation_respects_classification_levels() {
    let policy = ClassificationBased::new(SecurityClassification::Confidential).with_rule(
        PolicyRule::new("classification>=confidential", "provider.*", true),
    );
    let public = ClassifiedData::new(
        "public",
        SecurityClassification::Public,
        ResidencyRequirement::unrestricted(),
    );
    let internal = ClassifiedData::new(
        "internal",
        SecurityClassification::Internal,
        ResidencyRequirement::unrestricted(),
    );
    let confidential = ClassifiedData::new(
        "confidential",
        SecurityClassification::Confidential,
        ResidencyRequirement::unrestricted(),
    );
    let restricted = ClassifiedData::new(
        "restricted",
        SecurityClassification::Restricted,
        ResidencyRequirement::unrestricted(),
    );

    assert_eq!(
        policy.evaluate(&public, "provider.read"),
        PolicyDecision::Allow
    );
    assert_eq!(
        policy.evaluate(&internal, "provider.read"),
        PolicyDecision::Allow
    );
    assert_eq!(
        policy.evaluate(&confidential, "provider.read"),
        PolicyDecision::Audit
    );
    assert_eq!(
        policy.evaluate(&restricted, "provider.read"),
        PolicyDecision::Deny
    );
    assert_eq!(
        AllowAll.evaluate(&restricted, "provider.read"),
        PolicyDecision::Allow
    );
    assert_eq!(
        DenyAll.evaluate(&public, "provider.read"),
        PolicyDecision::Deny
    );
}
