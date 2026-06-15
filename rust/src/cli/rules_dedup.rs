//! `lean-ctx rules dedup` — collapse duplicated lean-ctx guidance (#578).
//!
//! A client should pay for lean-ctx rules exactly once per session. Older
//! installs (and parent-directory walks in monorepos) left full rule copies
//! in several auto-loaded files; `doctor overhead` detects the duplication,
//! this command repairs it:
//!
//!  1. lean-ctx-OWNED dedicated rule files outside the canonical global
//!     location (project/parent `.cursor/rules/lean-ctx.mdc`,
//!     `.claude/rules/lean-ctx.md`, …) → deleted.
//!  2. `.cursorrules` lean-ctx blocks → removed when the canonical global
//!     Cursor mdc exists (Cursor auto-loads both; pointer lives in AGENTS.md).
//!  3. Stale compression blocks in `.cursorrules` → removed under the same
//!     condition (the global mdc carries the block).
//!
//! Only lean-ctx-owned files and lean-ctx-marked blocks are ever touched.
//! Unmarked user content is reported, never modified. Default is a dry-run
//! report; `--apply` executes with `.bak` backups for partial edits.

use std::path::{Path, PathBuf};

const COMPRESSION_START: &str = "<!-- lean-ctx-compression -->";
const COMPRESSION_END: &str = "<!-- /lean-ctx-compression -->";
const BLOCK_START: &str = "<!-- lean-ctx -->";
const BLOCK_END: &str = "<!-- /lean-ctx -->";

/// One planned dedup action.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Delete a wholly lean-ctx-owned duplicate rules file.
    DeleteFile { path: PathBuf, reason: String },
    /// Strip lean-ctx-marked blocks from a shared file (keeps user content).
    StripBlocks { path: PathBuf, reason: String },
    /// Informational only — lean-ctx guidance in user-maintained content.
    Report { path: PathBuf, note: String },
}

/// A file is "lean-ctx-owned" when lean-ctx wrote the whole file: dedicated
/// rule files start with the canonical header and carry a rules-version
/// marker, project LEAN-CTX.md carries its ownership marker.
fn is_owned_rules_file(content: &str) -> bool {
    let starts_with_header = content
        .trim_start()
        .starts_with(crate::rules_inject::RULES_MARKER)
        // CursorMdc has YAML frontmatter before the header.
        || (content.trim_start().starts_with("---")
            && content.contains(crate::rules_inject::RULES_MARKER));
    starts_with_header && content.contains("<!-- lean-ctx-rules-")
}

fn has_marked_block(content: &str) -> bool {
    (content.contains(BLOCK_START) && content.contains(BLOCK_END))
        || (content.contains(COMPRESSION_START) && content.contains(COMPRESSION_END))
}

/// Strips every lean-ctx-marked block (rules + compression) from `content`.
pub(crate) fn strip_lean_ctx_blocks(content: &str) -> String {
    let mut out = content.to_string();
    // Repeat until stable — a file can contain both block kinds (and in
    // pathological cases several of the same kind).
    loop {
        let next = if out.contains(BLOCK_START) && out.contains(BLOCK_END) {
            crate::marked_block::remove_content(&out, BLOCK_START, BLOCK_END)
        } else if out.contains(COMPRESSION_START) && out.contains(COMPRESSION_END) {
            crate::marked_block::remove_content(&out, COMPRESSION_START, COMPRESSION_END)
        } else {
            break;
        };
        if next == out {
            break;
        }
        out = next;
    }
    let trimmed = out.trim_end();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

/// Dedicated lean-ctx rule files that may linger in a project / parent chain
/// from older versions. Canonical copies live under `home` (global targets).
fn project_owned_candidates(dir: &Path) -> Vec<PathBuf> {
    vec![
        dir.join(".cursor/rules/lean-ctx.mdc"),
        dir.join(".claude/rules/lean-ctx.md"),
        dir.join(".codebuddy/rules/lean-ctx.md"),
        dir.join(".windsurf/rules/lean-ctx.md"),
        dir.join(".cline/rules/lean-ctx.md"),
        dir.join(".roo/rules/lean-ctx.md"),
    ]
}

/// Plans the dedup for `project` (walking parents up to, excluding, `home`).
pub(crate) fn plan(home: &Path, project: &Path) -> Vec<Action> {
    let mut actions = Vec::new();
    let canonical_cursor_mdc = home.join(".cursor/rules/lean-ctx.mdc");

    // 1. Owned dedicated duplicates in the project + parent chain.
    let mut dir = Some(project.to_path_buf());
    while let Some(d) = dir {
        if d == *home {
            break;
        }
        for candidate in project_owned_candidates(&d) {
            if candidate == canonical_cursor_mdc {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&candidate) else {
                continue;
            };
            if is_owned_rules_file(&content) {
                actions.push(Action::DeleteFile {
                    path: candidate,
                    reason: "lean-ctx-owned duplicate of the global rules file".into(),
                });
            } else if !content.trim().is_empty() {
                actions.push(Action::Report {
                    path: candidate,
                    note: "contains custom edits — not lean-ctx-owned, left untouched".into(),
                });
            }
        }
        dir = d.parent().map(Path::to_path_buf);
    }

    // 2./3. `.cursorrules`: marked blocks are redundant once the canonical
    // global mdc exists (Cursor loads both files every session).
    let cursorrules = project.join(".cursorrules");
    if let Ok(content) = std::fs::read_to_string(&cursorrules) {
        if canonical_cursor_mdc.exists() && has_marked_block(&content) {
            actions.push(Action::StripBlocks {
                path: cursorrules,
                reason: "global ~/.cursor/rules/lean-ctx.mdc already carries these blocks".into(),
            });
        } else if !canonical_cursor_mdc.exists() && content.contains("lean-ctx") {
            actions.push(Action::Report {
                path: cursorrules,
                note: "no global Cursor mdc found — .cursorrules stays the carrier".into(),
            });
        } else if content.contains("lean-ctx") && !has_marked_block(&content) {
            actions.push(Action::Report {
                path: cursorrules,
                note: "mentions lean-ctx without markers (manual rules) — review by hand".into(),
            });
        }
    }

    actions
}

/// Executes one action. Returns a human-readable result line.
fn apply(action: &Action) -> String {
    match action {
        Action::DeleteFile { path, .. } => match std::fs::remove_file(path) {
            Ok(()) => format!("deleted   {}", path.display()),
            Err(e) => format!("FAILED    {} ({e})", path.display()),
        },
        Action::StripBlocks { path, .. } => {
            let Ok(content) = std::fs::read_to_string(path) else {
                return format!("FAILED    {} (unreadable)", path.display());
            };
            let stripped = strip_lean_ctx_blocks(&content);
            if stripped == content {
                return format!("unchanged {}", path.display());
            }
            let bak = path.with_extension("bak");
            if let Err(e) = std::fs::write(&bak, &content) {
                return format!("FAILED    {} (backup: {e})", path.display());
            }
            if stripped.is_empty() {
                match std::fs::remove_file(path) {
                    Ok(()) => format!(
                        "deleted   {} (only lean-ctx blocks, backup: {})",
                        path.display(),
                        bak.display()
                    ),
                    Err(e) => format!("FAILED    {} ({e})", path.display()),
                }
            } else {
                match std::fs::write(path, &stripped) {
                    Ok(()) => format!("stripped  {} (backup: {})", path.display(), bak.display()),
                    Err(e) => format!("FAILED    {} ({e})", path.display()),
                }
            }
        }
        Action::Report { path, note } => format!("info      {} — {note}", path.display()),
    }
}

/// CLI entry: `lean-ctx rules dedup [--apply]`.
pub fn run(apply_changes: bool) -> i32 {
    let Some(home) = dirs::home_dir() else {
        eprintln!("Error: could not determine home directory");
        return 1;
    };
    let project = std::env::current_dir().unwrap_or_else(|_| home.clone());
    let actions = plan(&home, &project);

    if actions.is_empty() {
        println!("No duplicated lean-ctx rules found — every client pays once.");
        return 0;
    }

    println!(
        "{} (project: {})\n",
        if apply_changes {
            "Deduplicating lean-ctx rules"
        } else {
            "Dedup plan (dry-run — pass --apply to execute)"
        },
        project.display()
    );

    let mut fixable = 0usize;
    for action in &actions {
        match action {
            Action::DeleteFile { path, reason } => {
                fixable += 1;
                if apply_changes {
                    println!("  {}", apply(action));
                } else {
                    println!("  delete    {}\n            ({reason})", path.display());
                }
            }
            Action::StripBlocks { path, reason } => {
                fixable += 1;
                if apply_changes {
                    println!("  {}", apply(action));
                } else {
                    println!("  strip     {}\n            ({reason})", path.display());
                }
            }
            Action::Report { .. } => println!("  {}", apply(action)),
        }
    }

    if !apply_changes && fixable > 0 {
        println!("\nRun `lean-ctx rules dedup --apply` to fix {fixable} duplicate(s).");
    }
    if apply_changes && fixable > 0 {
        println!("\nDone. Verify with `lean-ctx doctor overhead`.");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned_mdc() -> String {
        format!(
            "---\ndescription: lean-ctx\n---\n{}\n<!-- lean-ctx-rules-v9 -->\nbody\n",
            crate::rules_inject::RULES_MARKER
        )
    }

    #[test]
    fn detects_owned_dedicated_files() {
        assert!(is_owned_rules_file(&owned_mdc()));
        assert!(is_owned_rules_file(&format!(
            "{}\n<!-- lean-ctx-rules-v11 -->\nbody\n",
            crate::rules_inject::RULES_MARKER
        )));
        // User file mentioning lean-ctx is NOT owned.
        assert!(!is_owned_rules_file("# My rules\nuse lean-ctx tools\n"));
        // Marker buried mid-file (user prepended content) is NOT owned.
        assert!(!is_owned_rules_file(&format!(
            "# my header\n{}\n<!-- lean-ctx-rules-v11 -->\n",
            crate::rules_inject::RULES_MARKER
        )));
    }

    #[test]
    fn strip_removes_rules_and_compression_blocks() {
        let content = "user line\n<!-- lean-ctx -->\nour rules\n<!-- /lean-ctx -->\nmore user\n<!-- lean-ctx-compression -->\nstyle\n<!-- /lean-ctx-compression -->\n";
        let out = strip_lean_ctx_blocks(content);
        assert!(out.contains("user line"));
        assert!(out.contains("more user"));
        assert!(!out.contains("our rules"));
        assert!(!out.contains("style"));
        assert!(!out.contains("lean-ctx-compression"));
    }

    #[test]
    fn strip_of_pure_block_file_yields_empty() {
        let content = "<!-- lean-ctx -->\nonly ours\n<!-- /lean-ctx -->\n";
        assert_eq!(strip_lean_ctx_blocks(content), "");
    }

    #[test]
    fn plan_deletes_project_and_parent_owned_files_only() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = home.join("projects");
        let project = parent.join("app");

        // Canonical global mdc (must never be planned for deletion).
        std::fs::create_dir_all(home.join(".cursor/rules")).unwrap();
        std::fs::write(home.join(".cursor/rules/lean-ctx.mdc"), owned_mdc()).unwrap();
        // Stale project + parent copies.
        std::fs::create_dir_all(project.join(".cursor/rules")).unwrap();
        std::fs::write(project.join(".cursor/rules/lean-ctx.mdc"), owned_mdc()).unwrap();
        std::fs::create_dir_all(parent.join(".cursor/rules")).unwrap();
        std::fs::write(parent.join(".cursor/rules/lean-ctx.mdc"), owned_mdc()).unwrap();
        // User-customized file — must only be reported.
        std::fs::create_dir_all(project.join(".claude/rules")).unwrap();
        std::fs::write(
            project.join(".claude/rules/lean-ctx.md"),
            "# customized by user\nkeep me\n",
        )
        .unwrap();

        let actions = plan(home, &project);
        let deletes: Vec<&PathBuf> = actions
            .iter()
            .filter_map(|a| match a {
                Action::DeleteFile { path, .. } => Some(path),
                _ => None,
            })
            .collect();
        assert_eq!(deletes.len(), 2, "project + parent copies: {actions:?}");
        assert!(deletes.iter().all(|p| !p.starts_with(home.join(".cursor"))));
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::Report { path, .. } if path.ends_with(".claude/rules/lean-ctx.md")
        )));
    }

    #[test]
    fn plan_strips_cursorrules_only_with_canonical_mdc() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let project = home.join("app");
        std::fs::create_dir_all(&project).unwrap();
        let rules = "<!-- lean-ctx -->\npointer\n<!-- /lean-ctx -->\n";
        std::fs::write(project.join(".cursorrules"), rules).unwrap();

        // Without the global mdc, .cursorrules is the carrier — report only.
        let actions = plan(home, &project);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::StripBlocks { .. })),
            "{actions:?}"
        );

        // With the canonical mdc, the block is a duplicate — strip.
        std::fs::create_dir_all(home.join(".cursor/rules")).unwrap();
        std::fs::write(home.join(".cursor/rules/lean-ctx.mdc"), owned_mdc()).unwrap();
        let actions = plan(home, &project);
        assert!(
            actions.iter().any(|a| matches!(
                a,
                Action::StripBlocks { path, .. } if path.ends_with(".cursorrules")
            )),
            "{actions:?}"
        );
    }

    #[test]
    fn apply_strip_writes_backup_and_keeps_user_content() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".cursorrules");
        std::fs::write(
            &path,
            "my custom rule\n<!-- lean-ctx -->\nours\n<!-- /lean-ctx -->\n",
        )
        .unwrap();

        let msg = apply(&Action::StripBlocks {
            path: path.clone(),
            reason: String::new(),
        });
        assert!(msg.starts_with("stripped"), "{msg}");
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, "my custom rule\n");
        let bak = std::fs::read_to_string(path.with_extension("bak")).unwrap();
        assert!(bak.contains("ours"));
    }

    #[test]
    fn apply_strip_deletes_file_that_was_only_ours() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".cursorrules");
        std::fs::write(&path, "<!-- lean-ctx -->\nours\n<!-- /lean-ctx -->\n").unwrap();

        let msg = apply(&Action::StripBlocks {
            path: path.clone(),
            reason: String::new(),
        });
        assert!(msg.starts_with("deleted"), "{msg}");
        assert!(!path.exists());
        assert!(path.with_extension("bak").exists());
    }
}
