// SPDX-License-Identifier: Apache-2.0

use super::*;

const AWS_KEY: &str = concat!("AKIA", "IOSFODNN7EXAMPLE");

fn categories(entries: &[Entry]) -> Vec<(&'static str, &str)> {
    entries
        .iter()
        .map(|entry| (entry.category, entry.text.as_str()))
        .collect()
}

/// Headings pick the category, the nearest one first; code, front matter,
/// comments and tables are no facts; continuation lines join their item.
#[test]
fn sections_bullets_and_paragraphs_become_categorised_entries() {
    let text = "---\ntitle: Project\n---\n# Project\n\nIntro paragraph about the project here.\n\n\
                ## Conventions\n\n- Use snake_case for every module name.\n- Keep files below\n  1500 lines each.\n\n\
                ```sh\ncargo test --all\n```\n\n\
                ## How to release\n\n1. Tag the commit on main first.\n2. Watch the release workflow run.\n\n\
                ## Dead ends\n\n- Sled as the store was tried and dropped.\n\n\
                <!-- a comment that is no fact -->\n| a | b |\n|---|---|\n\n\
                ## Notes\n\n### Gotchas\n\n- Never run lean-ctx stop before a build.\n";
    let entries = parse(text, true);
    assert_eq!(
        categories(&entries),
        vec![
            ("convention", "Intro paragraph about the project here."),
            ("convention", "Use snake_case for every module name."),
            ("convention", "Keep files below 1500 lines each."),
            ("procedure", "Tag the commit on main first."),
            ("procedure", "Watch the release workflow run."),
            ("dead_end", "Sled as the store was tried and dropped."),
            ("gotcha", "Never run lean-ctx stop before a build."),
        ]
    );
    assert_eq!(entries[1].anchor, "project/conventions/1");
    assert_eq!(entries[6].anchor, "project/notes/gotchas/1");
}

/// A document without a categorising heading yields `documentation`; an
/// instruction file yields `convention`. Words match whole (prefix) words.
#[test]
fn uncategorised_items_and_word_matching() {
    let text = "# Information\n\nThis page describes the latest data flow.\n";
    assert_eq!(parse(text, false)[0].category, "documentation");
    assert_eq!(parse(text, true)[0].category, "convention");
    assert_eq!(heading_category("Testing the engine"), Some("procedure"));
    assert_eq!(heading_category("Latest information"), None);
    assert_eq!(heading_category("Why we chose rusqlite"), Some("decision"));
}

/// Read from a project: the instruction files and docs are found, every fact
/// carries its file anchor, a secret in a bullet is never stored raw, and a
/// file outside the project is not read.
#[test]
fn a_project_import_reads_its_files_admitted_and_keeps_no_secret() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let project = tempfile::tempdir().unwrap();
    let root = project.path().canonicalize().unwrap();
    std::fs::write(
        root.join("AGENTS.md"),
        format!(
            "# Agents\n\n## Conventions\n\n- Run cargo fmt before every commit.\n\
             - Deploy with the key {AWS_KEY} from the vault.\n"
        ),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("docs/guides")).unwrap();
    std::fs::write(
        root.join("docs/guides/release.md"),
        "# Release\n\n## Steps\n\n- Build the release binary on pounce first.\n",
    )
    .unwrap();
    std::fs::write(root.join("docs/notes.txt"), "not markdown, not read\n").unwrap();

    let result = import(&root, &[]);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.sessions_found, 2, "AGENTS.md and the one docs page");
    let fact = |needle: &str| result.facts.iter().find(|fact| fact.value.contains(needle));
    let fmt = fact("cargo fmt").expect("convention imported");
    assert_eq!(fmt.category, "convention");
    assert_eq!(fmt.key, "AGENTS.md#agents/conventions/1");
    assert_eq!(fmt.imported_from.as_deref(), Some("docs"));
    assert_eq!(fmt.validity, Some(FactValidity::Convention));
    let release = fact("release binary").expect("docs procedure imported");
    assert_eq!(release.category, "procedure");
    assert_eq!(release.key, "docs/guides/release.md#release/steps/1");
    assert!(
        result
            .facts
            .iter()
            .all(|fact| !fact.value.contains(AWS_KEY)),
        "a secret never reaches a fact"
    );

    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside.path().join("secret.md"),
        "# Rules\n\n- Outside file here.\n",
    )
    .unwrap();
    let named = import(&root, &[outside.path().join("secret.md")]);
    assert!(named.facts.is_empty());
    assert_eq!(named.sources_refused, 1, "outside the project: not read");
}

/// C16/C17: a gitignored, personal instruction file is not imported, and a
/// Markdown name that links to something else in the project (an env file,
/// git's config) is judged by its target and not read.
#[test]
fn personal_and_disguised_files_are_not_imported() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let project = tempfile::tempdir().unwrap();
    let root = project.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join(".git/config"), "[core]\n\tbare = false\n").unwrap();
    std::fs::write(root.join(".gitignore"), "CLAUDE.md\n").unwrap();
    std::fs::write(
        root.join("AGENTS.md"),
        "# Agents\n\n## Rules\n\n- Every module keeps its tests next to it.\n",
    )
    .unwrap();
    std::fs::write(
        root.join("CLAUDE.md"),
        "# Mine\n\n## Notes\n\n- My personal reminder about the payroll project.\n",
    )
    .unwrap();
    std::fs::write(
        root.join("settings.txt"),
        "- A plain text file named as a doc.\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join(".git/config"), root.join("docs/config.md")).unwrap();
        std::os::unix::fs::symlink(root.join("settings.txt"), root.join("docs/settings.md"))
            .unwrap();
    }

    let result = import(&root, &[]);
    assert!(
        result
            .facts
            .iter()
            .any(|f| f.value.contains("tests next to it"))
    );
    assert!(
        result
            .facts
            .iter()
            .all(|f| !f.value.contains("personal reminder")),
        "a gitignored CLAUDE.md stays personal"
    );
    assert!(result.facts.iter().all(|f| !f.value.contains("plain text")));
    #[cfg(unix)]
    assert_eq!(
        result.sources_refused, 2,
        "both disguised links are refused"
    );
    let named = import(&root, &[root.join("settings.txt")]);
    assert!(
        named.facts.is_empty(),
        "a named non-Markdown file is not read"
    );
}
