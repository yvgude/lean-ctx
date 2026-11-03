// SPDX-License-Identifier: Apache-2.0
//! Engine readers of the knowledge store, fed and queried through
//! `ctx_knowledge` itself: the context kernel's knowledge provider and the
//! source view of a bound request.

use lean_ctx::core::context_field::TokenBudget;
use lean_ctx::core::context_kernel::providers::KnowledgeProvider;
use lean_ctx::core::context_kernel::types::{CandidateProvider, RetrievalContext};
use lean_ctx::core::knowledge::ProjectKnowledge;
use lean_ctx::core::memory_policy::MemoryPolicy;

/// The knowledge provider offers a fact bound to a source while the
/// source admits and not once it is deleted; a fact without a binding is
/// offered either way.
#[test]
fn a_fact_whose_source_is_gone_is_not_a_candidate() {
    use lean_ctx::core::archive::bindings::{self, Item};
    use lean_ctx::core::context_admission::stored::lineage_support::{governed, project, scoped};
    let _data = governed();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let (_directory, root) = project();
    std::fs::write(format!("{root}/auth.rs"), "pub fn verify_rs256() {}\n").expect("source");
    let bound = "Tokens are verified with RS256 in the auth module";
    let typed = "Deployments are promoted by the release captain on Fridays";
    for (key, value) in [("token-check", bound), ("release-duty", typed)] {
        let reply = super::handle(
            &root,
            "remember",
            Some("architecture"),
            Some(key),
            Some(value),
            None,
            "session-1",
            None,
            None,
            None,
            None,
            None,
        );
        assert!(reply.starts_with("Remembered"), "{reply}");
    }
    let item = Item::KnowledgeFact {
        project: &root,
        category: "architecture",
        key: "token-check",
    };
    let sources =
        bindings::sources_at(&root, &["auth.rs"], "ctx_knowledge").expect("readable source");
    assert!(bindings::bind(&item, bound, &root, sources));

    let ctx = RetrievalContext {
        query: "tokens deployments".to_string(),
        task: None,
        project_root: root.clone(),
        budget: TokenBudget {
            total: 1_000,
            used: 0,
        },
        max_candidates: 10,
    };
    let offered = || {
        let candidates = scoped(&root, || KnowledgeProvider::new(&root).candidates(&ctx));
        let mut contents: Vec<String> = candidates
            .into_iter()
            .filter_map(|candidate| candidate.content)
            .collect();
        contents.sort();
        contents
    };
    assert_eq!(offered(), [typed, bound]);
    std::fs::remove_file(format!("{root}/auth.rs")).expect("delete");
    assert_eq!(offered(), [typed]);
}

/// A request bound to a project without a project policy can neither load
/// nor recall another project's knowledge; the refusal leaves that store
/// intact for the request that owns it.
#[test]
fn bound_request_without_project_policy_cannot_load_or_recall_foreign_knowledge() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let bound = tempfile::tempdir().unwrap();
    let foreign = tempfile::tempdir().unwrap();
    let foreign_root = foreign.path().to_str().unwrap();
    let mut knowledge = ProjectKnowledge::new(foreign_root);
    knowledge.remember(
        "decision",
        "foreign",
        "private other project note",
        "local",
        0.9,
        &MemoryPolicy::default(),
    );
    knowledge.save().unwrap();
    assert!(
        lean_ctx::core::policy::runtime::for_project(bound.path())
            .unwrap()
            .is_none()
    );
    lean_ctx::core::policy::runtime::REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(bound.path().to_owned())),
        || {
            assert!(ProjectKnowledge::load(foreign_root).is_none());
            let response = super::handle(
                foreign_root,
                "recall",
                None,
                None,
                None,
                None,
                "local",
                None,
                None,
                None,
                None,
                None,
            );
            assert!(response.starts_with("Error:"));
            assert!(!response.contains("private other project note"));
        },
    );
    // Rejection neither modifies the other project's store nor hides it from
    // a later request that actually owns that project.
    assert_eq!(ProjectKnowledge::load(foreign_root).unwrap().facts.len(), 1);
}
