//! Shared accounting for the rules files an agent auto-loads each session.
//!
//! A "rules file" (CLAUDE.md, AGENTS.md, `.cursor/rules/*.mdc`, …) is injected
//! into every session as fixed context — it costs tokens before lean-ctx saves
//! anything. This module enumerates those files, attributes the lean-ctx-owned
//! share of each, and flags clients that load the same guidance more than once.
//!
//! Used by `lean-ctx doctor overhead` (fixed-cost board) and `lean-ctx tools
//! health` (token-budget / rot report, #848). Lives in `core` so neither caller
//! has to depend on the other.

use std::path::Path;

use crate::core::tokens::count_tokens;

/// One rules file a client auto-loads into context.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RulesFileCost {
    pub path: String,
    /// Tokens of the whole file (what the client actually injects).
    pub file_tokens: usize,
    /// Tokens inside lean-ctx marker blocks (our share of the file).
    pub lean_ctx_tokens: usize,
    /// True when the file carries a *full* lean-ctx payload (canonical rules or
    /// the compression block), as opposed to only the lightweight
    /// `<!-- lean-ctx -->` pointer. Pointer-only files cross-reference the
    /// canonical source and do not duplicate guidance (#684).
    pub carries_full: bool,
    /// Clients that auto-load this file.
    pub clients: Vec<&'static str>,
}

/// Tokens of the lean-ctx-owned portions of a rules file: every block that
/// starts at a line containing `<!-- lean-ctx` or the canonical rules marker
/// and ends at `<!-- /lean-ctx... -->` (inclusive). Files without markers
/// contribute 0 lean-ctx tokens (they still cost their full size).
#[must_use]
pub fn lean_ctx_block_tokens(content: &str) -> usize {
    let mut tokens = 0;
    let mut in_block = false;
    let mut block = String::new();
    for line in content.lines() {
        if !in_block
            && (line.contains("<!-- lean-ctx")
                || line.contains(crate::core::rules_canonical::START_MARK))
        {
            in_block = true;
        }
        if in_block {
            block.push_str(line);
            block.push('\n');
            if line.contains("<!-- /lean-ctx") {
                in_block = false;
                tokens += count_tokens(&block);
                block.clear();
            }
        }
    }
    if !block.is_empty() {
        // Unterminated block (e.g. whole-file rules like .mdc without an end
        // marker) — count what we collected.
        tokens += count_tokens(&block);
    }
    tokens
}

fn push_rules_file(out: &mut Vec<RulesFileCost>, path: &Path, clients: Vec<&'static str>) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    if content.trim().is_empty() {
        return;
    }
    out.push(RulesFileCost {
        path: path.to_string_lossy().to_string(),
        file_tokens: count_tokens(&content),
        lean_ctx_tokens: lean_ctx_block_tokens(&content),
        carries_full: crate::core::rules_channel::carries_full_rules(&content),
        clients,
    });
}

fn scan_mdc_dir(out: &mut Vec<RulesFileCost>, dir: &Path, clients: &[&'static str]) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("mdc") {
            push_rules_file(out, &path, clients.to_vec());
        }
    }
}

/// Collects every rules file that an agent auto-loads for work in `project`:
/// global per-client files, the project root, and the parent chain up to
/// `home` (Cursor merges parent `.cursor/rules/` and AGENTS.md in monorepos).
pub fn collect_rules_files(home: &Path, project: &Path) -> Vec<RulesFileCost> {
    let mut out = Vec::new();

    // Global, per-client.
    push_rules_file(
        out.as_mut(),
        &home.join(".claude/CLAUDE.md"),
        vec!["claude"],
    );
    push_rules_file(
        out.as_mut(),
        &home.join(".codebuddy/CODEBUDDY.md"),
        vec!["codebuddy"],
    );
    push_rules_file(out.as_mut(), &home.join(".codex/AGENTS.md"), vec!["codex"]);
    push_rules_file(
        out.as_mut(),
        &home.join(".gemini/GEMINI.md"),
        vec!["gemini"],
    );
    scan_mdc_dir(out.as_mut(), &home.join(".cursor/rules"), &["cursor"]);

    // Project root + parent chain (stop at home or filesystem root).
    let mut dir = Some(project.to_path_buf());
    while let Some(d) = dir {
        push_rules_file(out.as_mut(), &d.join(".cursorrules"), vec!["cursor"]);
        scan_mdc_dir(out.as_mut(), &d.join(".cursor/rules"), &["cursor"]);
        // AGENTS.md is the shared instruction file: Cursor, Codex and several
        // other agents auto-load it.
        push_rules_file(out.as_mut(), &d.join("AGENTS.md"), vec!["cursor", "codex"]);
        push_rules_file(out.as_mut(), &d.join("CLAUDE.md"), vec!["claude"]);
        push_rules_file(out.as_mut(), &d.join("CODEBUDDY.md"), vec!["codebuddy"]);
        push_rules_file(out.as_mut(), &d.join("GEMINI.md"), vec!["gemini"]);

        if d == *home {
            break;
        }
        dir = d.parent().map(Path::to_path_buf);
    }

    // The parent walk can reach directories the global scan already covered
    // (e.g. ~/.cursor/rules when the walk ends at home) — count each file once.
    let mut seen = std::collections::HashSet::new();
    out.retain(|f| seen.insert(f.path.clone()));

    out
}

/// Clients that auto-load more than one file carrying a *full* lean-ctx
/// payload — the same guidance billed multiple times per session (#578/#684).
///
/// Pointer-only files (a thinned `AGENTS.md` / `.cursorrules` that merely
/// cross-references the canonical source) are not counted: they exist precisely
/// to avoid duplication and cost only a handful of tokens (#684).
#[must_use]
pub fn duplicate_clients(files: &[RulesFileCost]) -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();
    for f in files {
        if f.lean_ctx_tokens == 0 || !f.carries_full {
            continue;
        }
        for c in &f.clients {
            *counts.entry(c).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(c, n)| (c.to_string(), n))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rules_canonical::START_MARK;

    #[test]
    fn block_tokens_counts_only_marked_regions() {
        let content = format!(
            "\
# Some user rules
custom stuff here

{}
## lean-ctx
Prefer ctx_read over Read.
{}

more user stuff that is not ours
",
            crate::core::rules_canonical::START_MARK,
            crate::core::rules_canonical::END_MARK,
        );
        let ours = lean_ctx_block_tokens(&content);
        assert!(ours > 0, "must count the marked block");
        assert!(
            ours < count_tokens(&content),
            "must not count unmarked user content"
        );
    }

    #[test]
    fn block_tokens_zero_without_markers() {
        assert_eq!(lean_ctx_block_tokens("just user rules\nno markers"), 0);
    }

    #[test]
    fn block_tokens_handles_canonical_marker_without_end() {
        // Dedicated files start with the canonical header and the whole
        // remainder counts as lean-ctx content.
        let content = format!("{START_MARK}\n<!-- version: 1 -->\n\nrule body\nmore rules\n");
        assert!(lean_ctx_block_tokens(&content) > 0);
    }

    #[test]
    fn duplicates_flag_clients_with_multiple_lean_ctx_sources() {
        let files = vec![
            RulesFileCost {
                path: "a/.cursorrules".into(),
                file_tokens: 100,
                lean_ctx_tokens: 50,
                carries_full: true,
                clients: vec!["cursor"],
            },
            RulesFileCost {
                path: "a/.cursor/rules/lean-ctx.mdc".into(),
                file_tokens: 200,
                lean_ctx_tokens: 200,
                carries_full: true,
                clients: vec!["cursor"],
            },
            RulesFileCost {
                path: "a/CLAUDE.md".into(),
                file_tokens: 80,
                lean_ctx_tokens: 40,
                carries_full: true,
                clients: vec!["claude"],
            },
        ];
        let dups = duplicate_clients(&files);
        assert_eq!(dups, vec![("cursor".to_string(), 2)]);
    }

    #[test]
    fn duplicates_ignore_files_without_lean_ctx_content() {
        let files = vec![
            RulesFileCost {
                path: "a/.cursorrules".into(),
                file_tokens: 100,
                lean_ctx_tokens: 0,
                carries_full: false,
                clients: vec!["cursor"],
            },
            RulesFileCost {
                path: "a/.cursor/rules/user.mdc".into(),
                file_tokens: 200,
                lean_ctx_tokens: 0,
                carries_full: false,
                clients: vec!["cursor"],
            },
        ];
        assert!(duplicate_clients(&files).is_empty());
    }

    #[test]
    fn duplicates_ignore_pointer_only_files() {
        // #684: a thinned AGENTS.md keeps the `<!-- lean-ctx -->` pointer (so
        // lean_ctx_tokens > 0) but is not a second full source — Cursor's only
        // full carrier is the global mdc, so there is no duplication.
        let files = vec![
            RulesFileCost {
                path: "a/.cursor/rules/lean-ctx.mdc".into(),
                file_tokens: 200,
                lean_ctx_tokens: 200,
                carries_full: true,
                clients: vec!["cursor"],
            },
            RulesFileCost {
                path: "a/AGENTS.md".into(),
                file_tokens: 120,
                lean_ctx_tokens: 60,
                carries_full: false,
                clients: vec!["cursor", "codex"],
            },
        ];
        assert!(
            duplicate_clients(&files).is_empty(),
            "pointer-only AGENTS.md must not count as a duplicate source"
        );
    }

    #[test]
    fn collect_walks_parent_chain_and_dedups_nothing_silently() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let project = home.join("projects/app");
        std::fs::create_dir_all(project.join(".cursor/rules")).unwrap();
        std::fs::create_dir_all(home.join("projects/.cursor/rules")).unwrap();

        std::fs::write(
            project.join(".cursor/rules/lean-ctx.mdc"),
            format!("{START_MARK}\n<!-- version: 1 -->\n\nbody\n"),
        )
        .unwrap();
        std::fs::write(
            home.join("projects/.cursor/rules/lean-ctx.mdc"),
            format!("{START_MARK}\n<!-- version: 1 -->\n\nbody\n"),
        )
        .unwrap();
        std::fs::write(
            project.join("AGENTS.md"),
            format!(
                "{}\nx\n{}\n",
                crate::core::rules_canonical::AGENTS_BLOCK_START,
                crate::core::rules_canonical::AGENTS_BLOCK_END,
            ),
        )
        .unwrap();

        let files = collect_rules_files(home, &project);
        assert_eq!(
            files.len(),
            3,
            "project mdc + parent mdc + AGENTS.md: {files:?}"
        );

        // The two mdc files are full carriers; the AGENTS.md here holds only the
        // `<!-- lean-ctx -->` pointer, so it is NOT counted as a third source
        // (#684 — pointers cross-reference, they do not duplicate).
        let dups = duplicate_clients(&files);
        assert!(
            dups.iter().any(|(c, n)| c == "cursor" && *n == 2),
            "cursor loads 2 full lean-ctx sources (pointer AGENTS.md excluded): {dups:?}"
        );
    }

    #[test]
    fn collect_counts_each_file_once_when_walk_overlaps_globals() {
        // Project directly under home: the parent walk ends AT home, whose
        // .cursor/rules the global scan already covered.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let project = home.join("app");
        std::fs::create_dir_all(home.join(".cursor/rules")).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            home.join(".cursor/rules/lean-ctx.mdc"),
            format!("{START_MARK}\n<!-- version: 1 -->\n\nbody\n"),
        )
        .unwrap();

        let files = collect_rules_files(home, &project);
        let global_count = files
            .iter()
            .filter(|f| f.path.ends_with("lean-ctx.mdc"))
            .count();
        assert_eq!(
            global_count, 1,
            "global mdc must be counted once: {files:?}"
        );
    }
}
