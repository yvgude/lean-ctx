// SPDX-License-Identifier: Apache-2.0
//! X5 release guard (RB1) for the tools: knowledge pulled from other Hive
//! members reaches the agent only inside the untrusted-content wrapper, on
//! every knowledge renderer, every `ctx_knowledge` action and the
//! session-start briefing. The Hive-side delivery paths are guarded in
//! `core::hive::x5_guard_tests`.

use lean_ctx::core::untrusted::{self, UntrustedKind, every_occurrence_wrapped};
use lean_ctx_hive::x5_guard_support::{INJECTION, LOCAL, assert_guarded, foreign_fixture, wrapped};

#[test]
fn knowledge_pulled_from_hive_is_wrapped_on_every_renderer() {
    use lean_ctx::core::context_admission::stored::lineage_support::project;
    use lean_ctx::core::knowledge::ProjectKnowledge;
    use lean_ctx::core::memory_policy::MemoryPolicy;
    use lean_ctx::core::policy::runtime::TestPolicyOverride;

    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let policy = MemoryPolicy::default();
    let mut knowledge = ProjectKnowledge::new(&root);
    knowledge.remember("deploy", "region", LOCAL, "s1", 0.9, &policy);
    // As `sync_into_knowledge` stores a pulled repo fact.
    knowledge.remember(
        "hive-acme-billing",
        "f-1",
        INJECTION,
        "hive-repo:@acme/billing@r4",
        0.8,
        &policy,
    );
    knowledge.facts.last_mut().expect("fact").imported_from = Some(format!(
        "{}@acme/billing@r4",
        lean_ctx_hive::knowledge::learn::IMPORT_TAG_SCHEME
    ));

    let before = wrapped(UntrustedKind::Knowledge);
    let facts = knowledge.facts.clone();
    let recall = super::format_facts_with_annotations(&root, &facts, facts.len(), None, &[]);
    for (path, output) in [
        ("knowledge summary", knowledge.format_summary()),
        ("aaak injection", knowledge.format_aaak()),
        ("aaak budgeted", knowledge.format_aaak_budgeted(4000)),
        ("aaak delta", knowledge.format_aaak_delta(None).content),
        ("session-start wakeup", knowledge.format_wakeup()),
        ("ctx_knowledge recall", recall),
    ] {
        assert_guarded(path, &output);
        assert!(
            output.contains(LOCAL) && !every_occurrence_wrapped(&output, LOCAL),
            "{path}: local knowledge must stay unwrapped:\n{output}"
        );
    }
    assert!(
        wrapped(UntrustedKind::Knowledge) >= before + 6,
        "G-f counts every wrap"
    );
}

/// The actions `ctx_knowledge` advertises, read from its own schema: a new
/// action is in the guard the day it is added (no hand-picked list of
/// renderers).
fn ctx_knowledge_actions() -> Vec<String> {
    use crate::server::tool_trait::McpTool as _;
    let tool = crate::tools::registered::ctx_knowledge::CtxKnowledgeTool.tool_def();
    let schema = serde_json::Value::Object((*tool.input_schema).clone());
    let described = schema["properties"]["action"]["description"]
        .as_str()
        .expect("action description")
        .to_string();
    let mut actions: Vec<String> = described
        .split(|c: char| !(c.is_ascii_lowercase() || c == '_' || c == '*'))
        .filter(|word| !word.is_empty() && *word != "also" && !word.ends_with('*'))
        .map(str::to_string)
        .collect();
    actions.sort();
    actions.dedup();
    assert!(actions.len() >= 16, "{described}");
    actions
}

/// Knowledge pulled from other members is stored wrapped, so every renderer
/// of every `ctx_knowledge` action and the session-start briefing deliver it
/// fenced; a store written by an older engine (unwrapped) is wrapped when it
/// is loaded.
#[test]
fn every_ctx_knowledge_action_and_the_briefing_deliver_pulled_facts_wrapped() {
    use lean_ctx::core::context_admission::stored::lineage_support::project;
    use lean_ctx::core::knowledge::ProjectKnowledge;
    use lean_ctx::core::policy::runtime::TestPolicyOverride;
    use lean_ctx_hive::knowledge::learn::sync_into_knowledge;
    use lean_ctx_protocol::hive::knowledge::KnowledgeRevisionV1;

    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let mut revision: KnowledgeRevisionV1 = foreign_fixture(
        "knowledge_revision.response",
        &["document.facts[].statement"],
    );
    let document = revision.document.take().expect("plain document");
    sync_into_knowledge(&root, "acme", "billing", 4, &document).unwrap();

    // The store itself holds every pulled value as one wrapper.
    let stored = ProjectKnowledge::load(&root).expect("store");
    let pulled: Vec<_> = stored
        .facts
        .iter()
        .filter(|f| f.hive_origin().is_some())
        .collect();
    assert!(!pulled.is_empty());
    for fact in &pulled {
        assert!(untrusted::is_wrapped(&fact.value), "{}", fact.value);
        assert!(!fact.key.contains(' '), "key {:?}", fact.key);
    }

    let category = pulled[0].category.clone();
    let key = pulled[0].key.clone();
    let mut delivered_somewhere = false;
    for action in ctx_knowledge_actions() {
        let output = super::handle(
            &root,
            &action,
            Some(&category),
            Some(&key),
            Some("a local note"),
            Some("hive"),
            "s-guard",
            None,
            None,
            None,
            None,
            None,
        );
        if output.contains(INJECTION) {
            delivered_somewhere = true;
            assert!(
                every_occurrence_wrapped(&output, INJECTION),
                "ctx_knowledge action={action}: foreign text outside the X5 wrapper:\n{output}"
            );
        }
    }
    assert!(delivered_somewhere, "no action delivered the pulled fact");

    let briefing = crate::tools::ctx_overview::build_wakeup_briefing(&root, Some("hive"));
    if briefing.contains(INJECTION) {
        assert!(every_occurrence_wrapped(&briefing, INJECTION), "{briefing}");
    }

    // A store an older engine wrote (values unwrapped) is wrapped on load.
    ProjectKnowledge::mutate_locked(&root, |k| {
        for fact in &mut k.facts {
            if fact.hive_origin().is_some() {
                fact.value = format!("legacy {INJECTION}");
            }
        }
    })
    .unwrap();
    let reloaded = ProjectKnowledge::load(&root).expect("store");
    assert!(
        reloaded
            .facts
            .iter()
            .filter(|f| f.hive_origin().is_some())
            .all(|f| untrusted::is_wrapped(&f.value))
    );
}

/// A second `remember` of the same statement that adds the refs the first
/// lacked moves `changed_at`, so the Hive capture gate considers the fact
/// again; an identical third one keeps it, so nothing is captured twice.
#[test]
fn capture_remembered_moves_changed_at_only_when_a_field_changes() {
    use lean_ctx::core::context_admission::stored::lineage_support::project;
    use lean_ctx::core::knowledge::{FactCapture, ProjectKnowledge};
    use lean_ctx::core::memory_policy::MemoryPolicy;

    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let (_directory, root) = project();
    let statement = "The relay keeps one socket per peer for its lifetime.";
    let policy = MemoryPolicy::default();
    ProjectKnowledge::mutate_locked(&root, |k| {
        k.remember("architecture", "socket", statement, "s1", 0.8, &policy);
    })
    .unwrap();
    let changed_at = || {
        ProjectKnowledge::load(&root)
            .expect("store")
            .facts
            .iter()
            .find(|f| f.value == statement)
            .expect("fact")
            .capture
            .changed_at
    };
    assert_eq!(changed_at(), None);
    let with_ref = || FactCapture {
        refs: vec!["file:src/relay.rs".into()],
        ..Default::default()
    };
    super::capture_remembered(&root, "architecture", Some("socket"), statement, with_ref())
        .unwrap();
    let first = changed_at().expect("refs added: changed");
    super::capture_remembered(&root, "architecture", Some("socket"), statement, with_ref())
        .unwrap();
    assert_eq!(changed_at(), Some(first), "unchanged refs keep the stamp");
}
