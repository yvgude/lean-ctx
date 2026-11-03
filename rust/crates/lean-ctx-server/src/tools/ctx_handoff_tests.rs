// SPDX-License-Identifier: Apache-2.0
//! What a new handoff ledger copies of facts remembered through
//! `ctx_knowledge`.

use lean_ctx::core::handoff_ledger::{
    CreateLedgerInput, HandoffPackage, create_ledger, format_show,
};
use lean_ctx::core::session::SessionState;

/// A new ledger copies only findings and facts that are served now: a
/// finding and a fact bound to files that are gone are absent from the
/// ledger, from `show` and from the compact handoff block; the finding
/// and the fact without a binding stay.
#[test]
fn a_new_ledger_leaves_out_findings_and_facts_that_are_not_served() {
    use lean_ctx::core::archive::bindings::{self, Item};
    use lean_ctx::core::context_admission::stored::lineage_support::{governed, project, scoped};
    let _data = governed();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let (_directory, root) = project();
    std::fs::write(format!("{root}/pool.rs"), "pub fn lease() {}\n").expect("source");
    std::fs::write(format!("{root}/auth.rs"), "pub fn verify_rs256() {}\n").expect("source");
    let mut session = SessionState::new();
    session.project_root = Some(root.clone());
    session.add_finding_for_file(Some("pool.rs"), Some(1), "the pool leaks one lease");
    session.add_finding_for_file(None, None, "noted without a file");
    let bound_fact = "Tokens are verified with RS256 in the auth module";
    let typed_fact = "Deployments are promoted by the release captain on Fridays";
    for (key, value) in [("token-check", bound_fact), ("release-duty", typed_fact)] {
        let reply = crate::tools::ctx_knowledge::handle(
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
    assert!(bindings::bind(&item, bound_fact, &root, sources));

    // Everything a new ledger hands on: its stored form, `show`, and the
    // compact block `ctx_handoff create` appends.
    let handed_on = || {
        scoped(&root, || {
            let (ledger, path) = create_ledger(CreateLedgerInput {
                agent_id: None,
                client_name: None,
                project_root: Some(root.clone()),
                session: session.clone(),
                tool_calls: Vec::new(),
                workflow: None,
                curated_refs: Vec::new(),
                request_id: None,
            })
            .expect("create ledger");
            format!(
                "{}\n{}\n{}",
                serde_json::to_string(&ledger).expect("json"),
                format_show(&path, &ledger),
                HandoffPackage::build(ledger, None, None).format_compact(),
            )
        })
    };

    let before = handed_on();
    for text in [
        "the pool leaks one lease",
        "noted without a file",
        "RS256",
        "captain",
    ] {
        assert!(before.contains(text), "{text}: {before}");
    }
    std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
    std::fs::remove_file(format!("{root}/auth.rs")).expect("delete");
    let after = handed_on();
    for text in ["the pool leaks one lease", "RS256", "token-check"] {
        assert!(!after.contains(text), "{text}: {after}");
    }
    for text in ["noted without a file", "captain"] {
        assert!(after.contains(text), "{text}: {after}");
    }
}
