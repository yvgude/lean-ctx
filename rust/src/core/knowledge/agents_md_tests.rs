// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::core::knowledge::KnowledgeFact;

fn fact(category: &str, key: &str, value: &str) -> KnowledgeFact {
    let now = chrono::Utc::now();
    KnowledgeFact {
        origin: Default::default(),
        category: category.into(),
        key: key.into(),
        value: value.into(),
        source_session: "test".into(),
        confidence: 0.8,
        created_at: now,
        last_confirmed: now,
        retrieval_count: 0,
        last_retrieved: None,
        valid_from: None,
        valid_until: None,
        supersedes: None,
        confirmation_count: 0,
        feedback_up: 0,
        feedback_down: 0,
        last_feedback: None,
        privacy: Default::default(),
        sensitivity: SensitivityLevel::Public,
        imported_from: None,
        archetype: Default::default(),
        fidelity: None,
        revision_count: 0,
        status: None,
        validity: None,
        anchors: Vec::new(),
        review: None,
        capture: Default::default(),
    }
}

fn knowledge(facts: Vec<KnowledgeFact>) -> ProjectKnowledge {
    let mut knowledge = ProjectKnowledge::new("/tmp/project");
    knowledge.facts = facts;
    knowledge
}

/// Export → import gives back exactly the exported `(category, value)`
/// pairs; categories outside the export, non-public and superseded facts
/// stay out.
#[test]
fn an_export_imports_back_to_the_same_facts() {
    let mut confidential = fact("convention", "c3", "Confidential rule that never leaves.");
    confidential.sensitivity = SensitivityLevel::Confidential;
    let mut superseded = fact("decision", "d2", "An old decision that was replaced.");
    superseded.valid_until = Some(chrono::Utc::now());
    let source = knowledge(vec![
        fact("convention", "c1", "Use snake_case for every module name."),
        fact("conventions", "c2", "Keep every file below 1500 lines."),
        fact(
            "procedure",
            "p1",
            "Run the gates on pounce before a commit.",
        ),
        fact("decision", "d1", "Switched the index store to rusqlite."),
        fact(
            "architecture",
            "a1",
            "The engine is local-first; Hive is optional.",
        ),
        fact("gotcha", "g1", "Never run lean-ctx stop before a build."),
        fact("dead_end", "x1", "Sled as the store was tried and dropped."),
        fact(
            "experience",
            "e1",
            "Fixed: exit 1 when the project root is missing.",
        ),
        fact(
            "auth",
            "k1",
            "Uses JWT tokens for sessions in this project.",
        ),
        confidential,
        superseded,
    ]);
    let section = render(&source);
    assert!(section.starts_with(BEGIN) && section.trim_end().ends_with(END));
    assert!(!section.contains("Confidential rule"));
    assert!(!section.contains("old decision"));
    assert!(!section.contains("JWT"), "auth is not an AGENTS.md section");

    let mut imported: Vec<(String, String)> = crate::core::import::markdown::parse(&section, true)
        .into_iter()
        .map(|entry| (entry.category.to_owned(), entry.text))
        .collect();
    imported.sort();
    let mut expected: Vec<(String, String)> = [
        (
            "architecture",
            "The engine is local-first; Hive is optional.",
        ),
        ("convention", "Keep every file below 1500 lines."),
        ("convention", "Use snake_case for every module name."),
        ("dead_end", "Sled as the store was tried and dropped."),
        ("decision", "Switched the index store to rusqlite."),
        (
            "experience",
            "Fixed: exit 1 when the project root is missing.",
        ),
        ("gotcha", "Never run lean-ctx stop before a build."),
        ("procedure", "Run the gates on pounce before a commit."),
    ]
    .iter()
    .map(|(c, v)| ((*c).to_owned(), (*v).to_owned()))
    .collect();
    expected.sort();
    assert_eq!(imported, expected);
}

/// The generated section replaces only itself; text around it stays.
#[test]
fn merging_replaces_only_the_generated_section() {
    let first = render(&knowledge(vec![fact(
        "convention",
        "c1",
        "First rule of the project here.",
    )]));
    let second = render(&knowledge(vec![fact(
        "convention",
        "c1",
        "Second rule of the project here.",
    )]));
    let written = "# Agents\n\nHand-written intro.\n";
    let once = merge_into(Some(written), &first);
    assert!(once.starts_with("# Agents\n\nHand-written intro.\n\n"));
    assert!(once.contains("First rule"));
    let footer = format!("{once}\n## Footer\n\nKept text.\n");
    let twice = merge_into(Some(&footer), &second);
    assert!(twice.contains("Second rule") && !twice.contains("First rule"));
    assert!(twice.starts_with("# Agents\n\nHand-written intro.\n"));
    assert!(twice.ends_with("## Footer\n\nKept text.\n"));
    assert_eq!(merge_into(None, &first), first);
}
