//! Import AI coding-session history into the project knowledge store.

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
    let sources: Vec<&str> = args
        .iter()
        .filter_map(|arg| (!arg.starts_with('-')).then_some(arg.as_str()))
        .collect();

    if (all && !sources.is_empty()) || sources.len() > 1 {
        eprintln!("Use one source (claude-code, codex, cursor, opencode) or --all.");
        print_help();
        return;
    }

    let (label, mut result) = if all {
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
                eprintln!("Choose a source: claude-code, codex, cursor, opencode, or --all.");
                print_help();
                return;
            }
        }
    };

    let extracted = result.facts_extracted;
    let persisted = if dry_run {
        0
    } else {
        let project_root = match std::env::current_dir() {
            Ok(path) => path.to_string_lossy().into_owned(),
            Err(error) => {
                eprintln!("Import failed: cannot determine project root: {error}");
                return;
            }
        };
        match persist_facts(&project_root, std::mem::take(&mut result.facts)) {
            Ok(count) => count,
            Err(error) => {
                eprintln!("Import failed: {error}");
                return;
            }
        }
    };

    if dry_run {
        println!("[DRY RUN] {label}: no facts written");
    } else {
        println!("Imported {persisted} new fact(s) from {label}.");
    }
    println!(
        "Sessions: {}; facts extracted: {extracted}; files touched: {}",
        result.sessions_found, result.files_touched
    );
    for error in &result.errors {
        eprintln!("Import warning: {error}");
    }
}

fn persist_facts(project_root: &str, facts: Vec<KnowledgeFact>) -> Result<usize, String> {
    ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        let mut added = 0;
        for fact in facts {
            let exists = knowledge.facts.iter().any(|existing| {
                existing.category == fact.category
                    && existing.value == fact.value
                    && existing.imported_from == fact.imported_from
            });
            if !exists {
                knowledge.facts.push(fact);
                added += 1;
            }
        }
        added
    })
    .map(|(_, added)| added)
}

fn print_help() {
    println!("Usage: lean-ctx import <claude-code|codex|cursor|opencode> [--dry-run]");
    println!("       lean-ctx import --all [--dry-run]");
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
}
