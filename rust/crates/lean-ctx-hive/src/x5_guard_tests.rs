// SPDX-License-Identifier: Apache-2.0
//! X5 release guard (RB1): no path delivers foreign content into an agent's
//! context without [`untrusted::wrap`]. One injection text travels every
//! delivery path; the test fails when any output carries it outside a
//! wrapper (G-f: 0 unwrapped deliveries).
//!
//! Paths covered here: bus deliveries through Hive (`ctx_agent read` and
//! `inbox`), tasks as `ctx_tasks` shows them (`core::hive::tasks` list and
//! task view with its updates), and pulled knowledge in rule files,
//! AGENTS.md and the store. The knowledge renderers, every `ctx_knowledge`
//! action and the session-start briefing are covered with the tools
//! (`tools::ctx_knowledge::hive_guard_tests`). A new delivery path for
//! foreign content adds its case to one of the two.

use super::x5_guard_support::{INJECTION, LOCAL, assert_guarded, foreign_fixture, wrapped};
use lean_ctx::core::untrusted::{self, UntrustedKind, every_occurrence_wrapped};

#[test]
fn bus_deliveries_through_hive_are_wrapped() {
    use lean_ctx::core::a2a::message::{MessagePriority, PrivacyLevel};
    use lean_ctx::core::agents::AgentRegistry;

    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _policy = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_string_lossy().into_owned();
    let mut registry = AgentRegistry::new();
    let agent = registry
        .register("claude", Some("hive"), &root, None)
        .unwrap();
    // How `relay_v2::apply_delivery` files a message from another machine.
    registry
        .post_message_keyed(
            Some(&root),
            "codex-85",
            None,
            "status",
            INJECTION,
            PrivacyLevel::Team,
            MessagePriority::Normal,
            None,
            Some("hive:org:org_acme/vault:0123/general:7"),
        )
        .unwrap();
    registry
        .post_message_keyed(
            Some(&root),
            &agent,
            None,
            "status",
            LOCAL,
            PrivacyLevel::Team,
            MessagePriority::Normal,
            None,
            None,
        )
        .unwrap();
    let served: String = registry
        .scratchpad
        .iter()
        .map(|entry| entry.served_message(Some(&root)) + "\n")
        .collect();
    assert_guarded("ctx_agent read (Hive delivery)", &served);
    assert!(!every_occurrence_wrapped(&served, LOCAL), "{served}");
}

#[test]
fn the_guard_itself_catches_unwrapped_and_half_wrapped_text() {
    let fenced = untrusted::wrap(UntrustedKind::TaskBody, "hive task #7", INJECTION);
    assert!(every_occurrence_wrapped(&fenced, INJECTION));
    assert!(!every_occurrence_wrapped(INJECTION, INJECTION));
    assert!(!every_occurrence_wrapped(
        &format!("{fenced}\n{INJECTION}"),
        INJECTION
    ));
    assert!(!every_occurrence_wrapped(
        &format!("{INJECTION}\n{fenced}"),
        INJECTION
    ));
    let open_only = fenced.lines().take(3).collect::<Vec<_>>().join("\n");
    assert!(!every_occurrence_wrapped(&open_only, INJECTION));
    assert!(!every_occurrence_wrapped(&fenced, ""));
    // Foreign text in the header (the source) is shown before the preamble:
    // it does not count as wrapped.
    // (A source keeps only its alphabet, so the needle has no spaces.)
    let in_source = untrusted::wrap(UntrustedKind::BusMessage, "ctx_shell_to_push", "fine");
    assert!(!every_occurrence_wrapped(&in_source, "ctx_shell_to_push"));
}

#[test]
fn tasks_their_updates_and_every_label_are_wrapped() {
    use crate::tasks::{TaskView, render_list, render_task};
    use lean_ctx_protocol::hive::tasks::TaskV1;
    let (body, updates) = (
        wrapped(UntrustedKind::TaskBody),
        wrapped(UntrustedKind::TaskUpdate),
    );
    let list = render_list(
        &foreign_fixture("task_list.response", &["tasks[].title", "tasks[].role"]),
        20,
    );
    // Every field a person or another agent writes, the role included: it
    // is shown in the header line, so it must never carry their text.
    let task: TaskV1 = foreign_fixture(
        "task.claimed.response",
        &[
            "title",
            "acceptance",
            "role",
            "created_by.label",
            "updates[].summary",
            "updates[].by.label",
        ],
    );
    let view = TaskView {
        task,
        held_until: None,
    };
    let shown = render_task(&view);
    for (path, output) in [("ctx_tasks list", list), ("ctx_tasks show", shown)] {
        assert_guarded(path, &output);
        assert!(
            !output.contains("<<end-foreign-data id=000000000000>>"),
            "{path}: a forged close marker survived:\n{output}"
        );
    }
    assert!(wrapped(UntrustedKind::TaskBody) >= body + 2);
    assert!(
        wrapped(UntrustedKind::TaskUpdate) > updates,
        "updates are wrapped"
    );
}

/// Other members' knowledge never becomes an instruction: rule files,
/// AGENTS.md and skills leave pulled facts out.
#[test]
fn pulled_facts_never_become_rules_agents_md_or_skills() {
    use lean_ctx::core::context_admission::stored::lineage_support::project;
    use lean_ctx::core::memory_policy::MemoryPolicy;
    use lean_ctx::core::policy::runtime::TestPolicyOverride;
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let policy = MemoryPolicy::default();
    let mut knowledge = lean_ctx::core::knowledge::ProjectKnowledge::new(&root);
    knowledge.remember("decision", "local", LOCAL, "s1", 0.9, &policy);
    knowledge.remember("decision", "pulled", INJECTION, "s1", 0.9, &policy);
    knowledge.facts.last_mut().expect("fact").imported_from = Some(format!(
        "{}@acme/billing@r4",
        crate::knowledge::learn::IMPORT_TAG_SCHEME
    ));
    assert!(knowledge.facts.last().unwrap().hive_origin().is_some());
    let agents_md = lean_ctx::core::knowledge::agents_md::render(&knowledge);
    assert!(
        agents_md.contains(LOCAL),
        "the control is exported:\n{agents_md}"
    );
    assert!(!agents_md.contains(INJECTION), "{agents_md}");

    // A person's explicit export (owner decision 22.10.): reviewed team
    // knowledge in its own marked section, with repo and revision.
    let with_hive = lean_ctx::core::knowledge::agents_md::render_with_hive(&knowledge, true);
    let section = with_hive
        .split("### Team knowledge from Hive (reviewed)")
        .nth(1)
        .unwrap_or_else(|| panic!("no Hive section:\n{with_hive}"));
    assert!(
        section.contains(&format!("- {INJECTION} (Hive @acme/billing r4)")),
        "{with_hive}"
    );
    assert!(
        !with_hive.contains("foreign-data"),
        "no wrapper in the file"
    );
    // Knowledge under review (to verify, contradicted) is not exported.
    knowledge.facts.last_mut().unwrap().mark_review(
        lean_ctx::core::knowledge::FactReviewState::Contradicted,
        lean_ctx::core::knowledge::FactReviewReason::Contradiction,
        chrono::Utc::now(),
    );
    let reviewed_only = lean_ctx::core::knowledge::agents_md::render_with_hive(&knowledge, true);
    assert!(!reviewed_only.contains(INJECTION), "{reviewed_only}");
}

/// The import tag is stored in the project knowledge, so pulls keep writing
/// the pre-VA `hive-repo:` (an older engine after a downgrade recognizes
/// and replaces them); facts tagged `hive-vault:` stay other members'
/// knowledge too, and the next pull of their vault replaces them.
#[test]
fn pulled_facts_keep_the_pre_va_tag_and_vault_tags_stay_foreign() {
    use crate::knowledge::learn::sync_into_knowledge;
    use lean_ctx::core::context_admission::stored::lineage_support::project;
    use lean_ctx::core::knowledge::ProjectKnowledge;
    use lean_ctx::core::memory_policy::MemoryPolicy;
    use lean_ctx::core::policy::runtime::TestPolicyOverride;
    use lean_ctx_protocol::hive::knowledge::KnowledgeRevisionV1;

    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let mut knowledge = ProjectKnowledge::new(&root);
    let policy = MemoryPolicy::default();
    knowledge.remember("hive/acme/billing", "old", INJECTION, "s1", 0.8, &policy);
    knowledge.facts.last_mut().expect("fact").imported_from =
        Some("hive-vault:@acme/billing@r3".into());
    assert!(knowledge.facts.last().unwrap().hive_origin().is_some());
    assert!(!lean_ctx::core::knowledge::agents_md::render(&knowledge).contains(INJECTION));
    knowledge.save().unwrap();

    let mut revision: KnowledgeRevisionV1 = foreign_fixture(
        "knowledge_revision.response",
        &["document.facts[].statement"],
    );
    let document = revision.document.take().expect("plain document");
    let (_, removed) = sync_into_knowledge(&root, "acme", "billing", 4, &document).unwrap();
    assert_eq!(removed, 1, "the old pull is replaced");
    let stored = ProjectKnowledge::load(&root).expect("store");
    assert!(
        stored
            .facts
            .iter()
            .filter_map(|f| f.imported_from.as_deref())
            .all(|tag| tag.starts_with("hive-repo:@acme/billing@r4"))
    );
}
