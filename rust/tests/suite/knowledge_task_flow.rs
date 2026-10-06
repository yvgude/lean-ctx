use lean_ctx::core::context_kernel::{ContextState, enrich_with_knowledge};
use lean_ctx::core::knowledge::KnowledgeQuery;
use lean_ctx::core::knowledge::local_store::LocalKnowledgeStore;
use lean_ctx::core::knowledge::store::KnowledgeStore;
use lean_ctx_protocol::ExtensionsV1;
use lean_ctx_protocol::knowledge::{
    AuthorityMetadata, KnowledgeObjectV1, KnowledgeSourceType, SourceReference,
};

fn knowledge_object(id: &str, source_type: KnowledgeSourceType) -> KnowledgeObjectV1 {
    KnowledgeObjectV1 {
        schema_version: 1,
        source_ref: Some(SourceReference {
            source_type: "documentation".to_owned(),
            uri: format!("org://knowledge/{id}"),
            commit_sha: None,
            access_timestamp: "2026-08-09T00:00:00Z".to_owned(),
        }),
        source_type,
        authority: Some(AuthorityMetadata {
            owner: "platform-team".to_owned(),
            confidence_level: 0.95,
            review_status: "approved".to_owned(),
        }),
        owner: "platform-team".to_owned(),
        classification: None,
        validity: None,
        supersedes: None,
        content_hash: id.to_owned(),
        evidence_digest: format!("evidence:{id}"),
        policy_ref: "policy:org-approved".to_owned(),
        evidence_refs: vec![format!("evidence-ref:{id}")],
        extra: ExtensionsV1::default(),
    }
}

#[test]
fn receipt_proves_technical_and_governed_knowledge_context() {
    let mut store = LocalKnowledgeStore::new();
    store
        .put(knowledge_object(
            "knowledge:architecture",
            KnowledgeSourceType::Documentation,
        ))
        .expect("architecture knowledge should be valid");
    store
        .put(knowledge_object(
            "knowledge:policy",
            KnowledgeSourceType::Documentation,
        ))
        .expect("policy knowledge should be valid");

    let mut context = ContextState::new(
        vec!["src/core/context_kernel/mod.rs".to_owned()],
        KnowledgeQuery::default(),
    );
    let references = enrich_with_knowledge("task:wire-knowledge", &mut context, &store);
    let receipt = lean_ctx::core::context_kernel::receipt_builder::ReceiptBuilder::new(
        "task:wire-knowledge".to_owned(),
    )
    .with_knowledge_refs(
        references
            .iter()
            .map(|reference| reference.object_id.clone())
            .collect(),
    )
    .build()
    .expect("receipt should build");

    assert_eq!(context.technical_refs, ["src/core/context_kernel/mod.rs"]);
    assert!(context.knowledge_snapshot.is_some());
    assert_eq!(
        receipt.knowledge_refs,
        ["knowledge:architecture", "knowledge:policy"]
    );
    assert_eq!(references[0].policy_ref, "policy:org-approved");
    assert_eq!(references[1].policy_ref, "policy:org-approved");
}
