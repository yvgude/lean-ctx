//! Import AI coding-session history into the project knowledge store.

use std::path::Path;

use crate::core::import;
use crate::core::knowledge::{KnowledgeFact, ProjectKnowledge};

pub fn cmd_import(args: &[String]) {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        print_help();
        return;
    }

    let dry_run = args.iter().any(|arg| arg == "--dry-run");
    let all = args.iter().any(|arg| arg == "--all");
    let limit_value = args
        .iter()
        .position(|arg| arg == "--limit")
        .and_then(|index| args.get(index + 1));
    let sources: Vec<&str> = args
        .iter()
        .filter(|arg| Some(*arg) != limit_value)
        .filter_map(|arg| (!arg.starts_with('-')).then_some(arg.as_str()))
        .collect();

    // The project's own sources (K1): `docs [paths…]` and `git [--limit N]`.
    let repo_source = matches!(sources.first().copied(), Some("docs" | "git"));
    if !repo_source && ((all && !sources.is_empty()) || sources.len() > 1) {
        eprintln!("Use one source (claude-code, codex, cursor, opencode, docs, git) or --all.");
        print_help();
        return;
    }

    let (label, mut result) = if repo_source {
        let project_root = match super::common::require_project_root() {
            Ok(root) => root,
            Err(error) => {
                eprintln!("Import failed: {error}");
                return;
            }
        };
        let root = std::path::Path::new(&project_root);
        if sources[0] == "docs" {
            let paths: Vec<std::path::PathBuf> =
                sources[1..].iter().map(std::path::PathBuf::from).collect();
            ("docs", import::markdown::import(root, &paths))
        } else {
            let limit = match limit_value.map(|value| value.parse::<usize>()) {
                None => import::git_history::DEFAULT_LIMIT,
                Some(Ok(limit)) if limit > 0 => limit,
                Some(_) => {
                    eprintln!("--limit takes a positive number of commits.");
                    return;
                }
            };
            ("git", import::git_history::import(root, limit))
        }
    } else if all {
        let mut result = import::claude_code::import();
        result.merge(import::codex::import());
        result.merge(import::cursor::import());
        result.merge(import::opencode::import());
        ("all sources", result)
    } else {
        match sources.first().copied() {
            Some("claude-code") => ("claude-code", import::claude_code::import()),
            Some("codex") => ("codex", import::codex::import()),
            Some("cursor") => ("cursor", import::cursor::import()),
            Some("opencode") => ("opencode", import::opencode::import()),
            _ => {
                eprintln!(
                    "Choose a source: claude-code, codex, cursor, opencode, docs, git, or --all."
                );
                print_help();
                return;
            }
        }
    };

    let extracted = result.facts_extracted;
    let persisted = if dry_run {
        0
    } else {
        let project_root = match super::common::require_project_root() {
            Ok(root) => root,
            Err(error) => {
                eprintln!("Import failed: {error}");
                return;
            }
        };
        let scope = result.repo_scope.take();
        match persist_facts(
            &project_root,
            std::mem::take(&mut result.facts),
            scope.as_ref(),
        ) {
            Ok(count) => count,
            Err(error) => {
                eprintln!("Import failed: {error}");
                return;
            }
        }
    };

    let summary = import_summary(dry_run, label, persisted, extracted, &result);
    super::common::print_released(&released_summary(summary));
    for error in &result.errors {
        eprintln!("Import warning: {error}");
    }
    if repo_source && !dry_run {
        queue_import_drafts();
    }
}

/// A connected machine proposes what the repository import brought to the
/// project's Hive repo (queued only; `lean-ctx hive sync` or the
/// scheduler sends it). Counts only on stdout.
fn queue_import_drafts() {
    use crate::core::hive::HiveStore;
    use crate::core::hive::knowledge::{import_draft, screening::Screen};
    let Ok(store) = HiveStore::open() else {
        return;
    };
    if !matches!(store.instance(), Ok(Some(_))) {
        return;
    }
    let Ok(project_root) = super::common::require_project_root() else {
        return;
    };
    let config = crate::core::config::Config::load().hive.clone();
    let mut screen = Screen::for_installation();
    match import_draft::enqueue_import_drafts(
        &store,
        &config,
        Path::new(&project_root),
        "cli-import",
        &mut screen,
        chrono::Utc::now(),
    ) {
        Ok(ids) if !ids.is_empty() => println!(
            "Queued {} import draft(s) for Hive review (sent with `lean-ctx hive sync`).",
            ids.len()
        ),
        Ok(_) => {}
        Err(error) => eprintln!("Hive: import drafts not queued: {error}"),
    }
}

/// What the command reports on stdout. Counts and the source label only: a
/// fact that admission did not let into the store appears as a number
/// (`facts skipped`), never by its value; a history source that was not
/// read appears as a number (`sources skipped` — above the read cap or
/// unreadable; `sources refused` — outside every named root or withheld),
/// never by its path.
fn import_summary(
    dry_run: bool,
    label: &str,
    persisted: usize,
    extracted: usize,
    result: &import::ImportResult,
) -> String {
    let outcome = if dry_run {
        format!("[DRY RUN] {label}: no facts written")
    } else {
        format!("Imported {persisted} new fact(s) from {label}.")
    };
    format!(
        "{outcome}\nSessions: {}; facts extracted: {extracted}; facts skipped: {}; \
         sources skipped: {}; sources refused: {}; files touched: {}",
        result.sessions_found,
        result.facts_skipped,
        result.sources_skipped,
        result.sources_refused,
        result.files_touched
    )
}

/// The import summary is counts and the source label — a report for the
/// person at the terminal: released, never recorded as a delivery.
fn released_summary(summary: String) -> String {
    let root = super::common::detect_project_root(&[]);
    super::common::CliToolRun::new("ctx_knowledge", &root, None)
        .human_facing()
        .deliver_text(|| summary)
}

fn persist_facts(
    project_root: &str,
    facts: Vec<KnowledgeFact>,
    scope: Option<&import::RepoScope>,
) -> Result<usize, String> {
    let now = chrono::Utc::now();
    ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        if let Some(scope) = scope {
            mark_removed_sections(knowledge, &facts, scope, Path::new(project_root), now);
        }
        let mut added = 0;
        // The facts arrive admitted for a store: `core::import` admits each
        // value where the fact is built, for every caller (E3).
        for fact in facts {
            let exists = knowledge.facts.iter().any(|existing| {
                existing.category == fact.category
                    && existing.value == fact.value
                    && existing.imported_from == fact.imported_from
            });
            if exists {
                // Found again: a section that was gone is back.
                for existing in knowledge.facts.iter_mut().filter(|existing| {
                    existing.key == fact.key && existing.imported_from == fact.imported_from
                }) {
                    clear_source_removed(existing);
                    if !fact.anchors.is_empty() {
                        existing.anchors.clone_from(&fact.anchors);
                    }
                }
                continue;
            }
            // A repository source (docs, git) anchors each fact (`key`): a
            // changed text at the same anchor updates that fact instead of
            // adding a second one next to it.
            let anchored = (!fact.key.is_empty())
                .then(|| {
                    knowledge.facts.iter_mut().find(|existing| {
                        existing.is_current()
                            && existing.key == fact.key
                            && existing.imported_from == fact.imported_from
                    })
                })
                .flatten();
            match anchored {
                Some(existing) => {
                    existing.category.clone_from(&fact.category);
                    existing.value = fact.value;
                    existing.archetype = fact.archetype;
                    existing.last_confirmed = fact.last_confirmed;
                    existing.revision_count += 1;
                    existing.anchors = fact.anchors;
                    clear_source_removed(existing);
                }
                None => knowledge.facts.push(fact),
            }
            added += 1;
        }
        added
    })
    .map(|(_, added)| added)
}

/// A docs fact whose file this run read (or, for a whole-project run, whose
/// file is gone) but whose anchor did not come back lost its section: it is
/// *to verify* (`source_removed`), still recalled, never deleted (K1).
fn mark_removed_sections(
    knowledge: &mut ProjectKnowledge,
    found: &[KnowledgeFact],
    scope: &import::RepoScope,
    root: &Path,
    now: chrono::DateTime<chrono::Utc>,
) {
    use crate::core::knowledge::{FactReviewReason, FactReviewState};
    let docs = Some(import::ImportSource::Docs.as_str());
    let keys: std::collections::HashSet<&str> = found
        .iter()
        .filter(|fact| fact.imported_from.as_deref() == docs)
        .map(|fact| fact.key.as_str())
        .collect();
    for fact in knowledge.facts.iter_mut().filter(|fact| {
        fact.is_current()
            && fact.imported_from.as_deref() == docs
            && !keys.contains(fact.key.as_str())
    }) {
        let Some((file, _)) = fact.key.split_once('#') else {
            continue;
        };
        let covered = scope.files.iter().any(|read| read == file)
            || (scope.whole && !root.join(file).exists());
        if covered {
            fact.mark_review(
                FactReviewState::ToVerify,
                FactReviewReason::SourceRemoved,
                now,
            );
        }
    }
}

fn clear_source_removed(fact: &mut KnowledgeFact) {
    if fact.review.as_ref().is_some_and(|review| {
        review.reason == crate::core::knowledge::FactReviewReason::SourceRemoved
    }) {
        fact.clear_review();
    }
}

fn print_help() {
    println!("Usage: lean-ctx import <claude-code|codex|cursor|opencode> [--dry-run]");
    println!("       lean-ctx import --all [--dry-run]");
    println!("       lean-ctx import docs [<file|dir>...] [--dry-run]");
    println!("         AGENTS.md, CLAUDE.md, .cursorrules, .cursor/rules and docs/ by default");
    println!(
        "       lean-ctx import git [--limit N] [--dry-run]   (reverts, fixes and decisions; default {})",
        import::git_history::DEFAULT_LIMIT
    );
}

#[cfg(test)]
mod tests {
    use crate::core::import::ImportSource;

    #[test]
    fn source_names_match_import_engine() {
        assert_eq!(ImportSource::ClaudeCode.as_str(), "claude-code");
        assert_eq!(ImportSource::Codex.as_str(), "codex");
        assert_eq!(ImportSource::Cursor.as_str(), "cursor");
        assert_eq!(ImportSource::OpenCode.as_str(), "opencode");
    }

    use crate::cli::common::carrier_tests_support::{AWS_KEY, governed, lineage};

    /// The import summary is counts and a label: released, never registered.
    #[test]
    fn the_import_summary_is_human_facing() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        governed();
        let root = crate::cli::common::detect_project_root(&[]);
        let summary =
            "Imported 3 new fact(s) from codex.\nSessions: 2; facts extracted: 5; files touched: 4";
        let printed = super::released_summary(summary.to_string());
        assert_eq!(printed, summary, "the summary keeps its bytes");
        assert_eq!(lineage(&root, &printed), None);
        let leaked = super::released_summary(format!("Imported 1 new fact(s) from {AWS_KEY}."));
        assert!(!leaked.contains(AWS_KEY), "{leaked}");
    }

    /// End to end through the shared import and the CLI's store step: a
    /// clean fact is stored with its bytes, a key-shaped value is never
    /// stored raw, and the facts the store receives are the admitted ones.
    #[test]
    fn imported_facts_reach_the_store_admitted() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let history = tempfile::tempdir().expect("history dir");
        let history_root = history.path().canonicalize().expect("history root");
        let session_dir = history_root.join("project-a");
        std::fs::create_dir_all(&session_dir).expect("session dir");
        let line = |text: &str| {
            serde_json::json!({ "message": { "role": "assistant", "content": text } }).to_string()
        };
        std::fs::write(
            session_dir.join("session.jsonl"),
            format!(
                "{}\n{}\n",
                line("We decided to use JWT RS256 for the session tokens."),
                line(&format!(
                    "We decided to deploy with the key {AWS_KEY} today."
                )),
            ),
        )
        .expect("transcript");
        let mut result = crate::core::import::claude_code::import_from_root(&history_root);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(
            result
                .facts
                .iter()
                .all(|fact| !fact.value.contains(AWS_KEY))
        );

        // The summary states counts only.
        let summary =
            super::import_summary(false, "claude-code", 1, result.facts_extracted, &result);
        assert!(
            summary.contains(&format!("facts skipped: {}", result.facts_skipped)),
            "{summary}"
        );
        assert!(!summary.contains(AWS_KEY), "{summary}");
        assert!(!summary.contains("JWT"), "{summary}");

        // Sources that were not read are counts too — no path of theirs.
        let refused = crate::core::import::ImportResult {
            sources_refused: 2,
            sources_skipped: 1,
            ..Default::default()
        };
        let summary = super::import_summary(true, "opencode", 0, 0, &refused);
        assert_eq!(
            summary,
            "[DRY RUN] opencode: no facts written\nSessions: 0; facts extracted: 0; \
             facts skipped: 0; sources skipped: 1; sources refused: 2; files touched: 0"
        );

        let project = tempfile::tempdir().expect("project dir");
        let root = project.path().to_string_lossy().into_owned();
        let facts = std::mem::take(&mut result.facts);
        let offered = facts.len();
        let added = super::persist_facts(&root, facts, None).expect("persist");
        assert_eq!(added, offered, "one store step, no second filter");
        let stored = crate::core::knowledge::ProjectKnowledge::load(&root).expect("stored");
        assert!(
            stored
                .facts
                .iter()
                .any(|fact| fact.value.contains("JWT RS256")),
            "a clean fact is stored"
        );
        assert!(
            stored
                .facts
                .iter()
                .all(|fact| !fact.value.contains(AWS_KEY)),
            "a masked value is never stored raw"
        );
    }

    /// K1 + J2: an imported section is anchored; a section that disappears
    /// leaves its facts *to verify* (`source_removed`), and they are current
    /// again when it comes back.
    #[test]
    fn a_removed_section_leaves_its_facts_to_verify_until_it_returns() {
        use crate::core::knowledge::{FactReviewReason, FactReviewState, ProjectKnowledge};
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        let root_text = root.to_string_lossy().into_owned();
        let agents = root.join("AGENTS.md");
        let both = "# Agents\n\n## Conventions\n\n- Run cargo fmt before every commit.\n\n\
                    ## Gotchas\n\n- Never run lean-ctx stop before a build.\n";
        let import = |root: &std::path::Path| {
            let mut result = crate::core::import::markdown::import(root, &[]);
            let scope = result.repo_scope.take();
            super::persist_facts(
                &root_text,
                std::mem::take(&mut result.facts),
                scope.as_ref(),
            )
            .unwrap();
        };
        let fact = |needle: &str| {
            ProjectKnowledge::load(&root_text)
                .unwrap()
                .facts
                .into_iter()
                .find(|fact| fact.value.contains(needle))
                .unwrap()
        };
        std::fs::write(&agents, both).unwrap();
        import(&root);
        let gotcha = fact("lean-ctx stop");
        assert_eq!(gotcha.key, "AGENTS.md#agents/gotchas/1");
        assert_eq!(gotcha.anchors.len(), 1);
        assert_eq!(gotcha.anchors[0].symbol.as_deref(), Some("agents/gotchas"));
        assert_eq!(gotcha.review, None);

        std::fs::write(
            &agents,
            "# Agents\n\n## Conventions\n\n- Run cargo fmt before every commit.\n",
        )
        .unwrap();
        import(&root);
        let review = fact("lean-ctx stop").review.expect("marked");
        assert_eq!(
            (review.state, review.reason),
            (FactReviewState::ToVerify, FactReviewReason::SourceRemoved)
        );
        assert_eq!(
            fact("cargo fmt").review,
            None,
            "the kept section stays current"
        );

        std::fs::write(&agents, both).unwrap();
        import(&root);
        assert_eq!(fact("lean-ctx stop").review, None, "back again: current");

        std::fs::remove_file(&agents).unwrap();
        import(&root);
        assert!(
            fact("cargo fmt").review.is_some(),
            "a removed file loses all its facts"
        );
    }
}
