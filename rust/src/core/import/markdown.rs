// SPDX-License-Identifier: Apache-2.0
//! The project's instruction files and documentation → knowledge facts (K1).
//!
//! `AGENTS.md`, `CLAUDE.md`, `.cursorrules`, `.cursor/rules/*.mdc` and
//! `docs/**/*.md`, or the files a caller names inside the project. Every file
//! is read through the rooted admitted reader as ONE text — path jail,
//! secret-path rule, project policy and gateway decide on the original before
//! a line is parsed (E3) — and every fact built from it is admitted for its
//! store again (`super::push_repo_fact`). A file that is withheld or only
//! readable (restricted) yields nothing and is counted.
//!
//! A fact is one bullet item or one paragraph. Its category comes from the
//! nearest heading above it that names one (`## Conventions`, `### How to
//! release`, `## Dead ends` …); without one, an instruction file yields
//! `convention` and a document `documentation`. Fenced code, front matter,
//! HTML comments and tables are not facts. The section headings of an
//! `AGENTS.md` export ([`crate::core::knowledge::agents_md`]) map back to
//! exactly the categories they were rendered from, so export and import
//! round-trip.
//!
//! On a machine connected to Hive, `lean-ctx import docs` queues the facts
//! it added or changed as import drafts
//! ([`crate::core::hive::knowledge::import_draft`]).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::{ImportResult, ImportSource, RepoFact, SourceRefusal, push_repo_fact};
use crate::core::knowledge::FactValidity;

/// Instruction files at the project root, in this order.
pub const INSTRUCTION_FILES: [&str; 3] = ["AGENTS.md", "CLAUDE.md", ".cursorrules"];
/// The documentation directory below the project root.
pub const DOCS_DIR: &str = "docs";
/// At most this many files per run; the rest is counted as skipped.
const MAX_FILES: usize = 400;
/// Directory depth below `docs/` that is walked.
const MAX_DOCS_DEPTH: usize = 6;
/// A bullet or paragraph shorter than this is a label, not a fact.
const MIN_FACT_CHARS: usize = 12;

/// One fact candidate of a parsed file, before admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub category: &'static str,
    /// `<section-slug>/<n>`: the heading path and the item's place in it.
    pub anchor: String,
    pub text: String,
    /// Byte offset of the item's first line in the parsed text.
    pub offset: usize,
}

/// Categories by the words a heading uses. The first match of the NEAREST
/// heading wins; parent headings are consulted only when a heading names
/// none. Order matters: "dead end" before "decision", "gotcha" before
/// "convention".
const HEADING_CATEGORIES: [(&str, &[&str]); 7] = [
    (
        "dead_end",
        &[
            "dead end",
            "dead-end",
            "dead ends",
            "abandoned",
            "did not work",
            "didn't work",
            "tried",
        ],
    ),
    (
        "gotcha",
        &[
            "gotcha",
            "pitfall",
            "known issue",
            "caveat",
            "avoid",
            "never",
            "don't",
            "do not",
            "anti-pattern",
        ],
    ),
    (
        "experience",
        &[
            "experience",
            "lesson",
            "learned",
            "postmortem",
            "post-mortem",
            "incident",
        ],
    ),
    ("decision", &["decision", "adr", "rationale", "why we"]),
    (
        "procedure",
        &[
            "procedure",
            "how to",
            "how-to",
            "steps",
            "workflow",
            "setup",
            "set up",
            "install",
            "build",
            "test",
            "release",
            "deploy",
            "runbook",
            "commands",
            "development",
        ],
    ),
    (
        "architecture",
        &[
            "architecture",
            "design",
            "structure",
            "overview",
            "components",
            "layout",
        ],
    ),
    (
        "convention",
        &[
            "convention",
            "rule",
            "style",
            "guideline",
            "standard",
            "naming",
            "policy",
            "instruction",
            "must",
            "quality",
            "format",
        ],
    ),
];

/// Whether `heading` names `keyword`: a one-word keyword is a prefix of one
/// of its words ("conventions", "testing"; not "information" for "format"),
/// a phrase appears as whole words.
fn names(words: &[String], keyword: &str) -> bool {
    if keyword.contains(' ') {
        let padded = format!(" {} ", words.join(" "));
        padded.contains(&format!(" {keyword} "))
    } else {
        words.iter().any(|word| word.starts_with(keyword))
    }
}

fn heading_category(heading: &str) -> Option<&'static str> {
    let lower = heading.to_lowercase();
    let words: Vec<String> = lower
        .split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '-'))
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect();
    HEADING_CATEGORIES
        .iter()
        .find(|(_, keywords)| keywords.iter().any(|keyword| names(&words, keyword)))
        .map(|(category, _)| *category)
}

/// A Markdown list marker: `-`, `*`, `+` or `1.` / `1)`, followed by a space.
fn list_item(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some(rest);
        }
    }
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        let rest = &trimmed[digits..];
        if let Some(rest) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some(rest);
        }
    }
    None
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&level) {
        let rest = &line[level..];
        if rest.starts_with(' ') {
            return Some((level, rest.trim().trim_end_matches('#').trim()));
        }
    }
    None
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse one file's text into fact candidates. `instructions`: the file is
/// an agent instruction file (its uncategorised items are conventions).
#[must_use]
pub fn parse(text: &str, instructions: bool) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut headings: Vec<(usize, String)> = Vec::new();
    let mut counter = 0usize;
    // The item being read and the offset of its first line.
    let mut current: Option<(usize, String)> = None;
    let mut in_fence: Option<&str> = None;
    let mut in_comment = false;

    let mut offset = 0usize;
    let mut lines = text
        .split_inclusive('\n')
        .map(|raw| {
            let start = offset;
            offset += raw.len();
            (start, raw.trim_end_matches(['\n', '\r']))
        })
        .peekable();
    // YAML front matter.
    if lines.peek().is_some_and(|(_, line)| line.trim() == "---") {
        lines.next();
        for (_, line) in lines.by_ref() {
            if line.trim() == "---" {
                break;
            }
        }
    }

    let flush = |current: &mut Option<(usize, String)>,
                 counter: &mut usize,
                 headings: &[(usize, String)],
                 entries: &mut Vec<Entry>| {
        let Some((offset, text)) = current.take() else {
            return;
        };
        let text = normalized(&text);
        if text.chars().count() < MIN_FACT_CHARS {
            return;
        }
        let category = headings
            .iter()
            .rev()
            .find_map(|(_, title)| heading_category(title))
            .unwrap_or(if instructions {
                "convention"
            } else {
                "documentation"
            });
        let path: Vec<String> = headings
            .iter()
            .map(|(_, title)| crate::core::knowledge::anchors::heading_slug(title))
            .collect();
        let section = if path.is_empty() {
            "top".to_owned()
        } else {
            path.join("/")
        };
        *counter += 1;
        entries.push(Entry {
            category,
            anchor: format!("{section}/{counter}"),
            text,
            offset,
        });
    };

    for (start, line) in lines {
        let trimmed = line.trim();
        if let Some(fence) = in_fence {
            if trimmed.starts_with(fence) {
                in_fence = None;
            }
            continue;
        }
        if in_comment {
            if trimmed.contains("-->") {
                in_comment = false;
            }
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            flush(&mut current, &mut counter, &headings, &mut entries);
            in_fence = Some(if trimmed.starts_with("```") {
                "```"
            } else {
                "~~~"
            });
            continue;
        }
        if trimmed.starts_with("<!--") {
            flush(&mut current, &mut counter, &headings, &mut entries);
            in_comment = !trimmed.contains("-->");
            continue;
        }
        if let Some((level, title)) = heading(trimmed) {
            flush(&mut current, &mut counter, &headings, &mut entries);
            headings.retain(|(l, _)| *l < level);
            headings.push((level, title.to_owned()));
            counter = 0;
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('|') {
            flush(&mut current, &mut counter, &headings, &mut entries);
            continue;
        }
        let trimmed = trimmed.trim_start_matches('>').trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(item) = list_item(line) {
            flush(&mut current, &mut counter, &headings, &mut entries);
            current = Some((start, item.to_owned()));
            continue;
        }
        match current.as_mut() {
            Some((_, text)) => {
                text.push(' ');
                text.push_str(trimmed);
            }
            None => current = Some((start, trimmed.to_owned())),
        }
    }
    flush(&mut current, &mut counter, &headings, &mut entries);
    entries
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "md" || extension == "mdc" || extension == "markdown")
}

/// The files directly in `dir` that git does not ignore (hidden ones
/// included): a personal, gitignored instruction file is no team knowledge.
fn tracked_entries(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .build()
        .flatten()
        .filter(|entry| entry.depth() == 1)
        .map(ignore::DirEntry::into_path)
        .filter(|path| path.is_file())
        .collect();
    found.sort();
    found
}

/// The files a default run reads, relative to `root`, sorted, at most
/// [`MAX_FILES`] (the count beyond is returned as the second value).
fn default_files(root: &Path) -> (Vec<PathBuf>, usize) {
    let present = tracked_entries(root);
    let mut files: Vec<PathBuf> = INSTRUCTION_FILES
        .iter()
        .map(|name| root.join(name))
        .filter(|path| present.contains(path))
        .collect();
    let rules = root.join(".cursor").join("rules");
    if rules.is_dir() {
        files.extend(
            tracked_entries(&rules)
                .into_iter()
                .filter(|path| is_markdown(path)),
        );
    }
    let docs = root.join(DOCS_DIR);
    if docs.is_dir() {
        let mut found: Vec<PathBuf> = ignore::WalkBuilder::new(&docs)
            .max_depth(Some(MAX_DOCS_DEPTH))
            .hidden(true)
            .build()
            .flatten()
            .map(ignore::DirEntry::into_path)
            .filter(|path| path.is_file() && is_markdown(path))
            .collect();
        found.sort();
        files.extend(found);
    }
    let beyond = files.len().saturating_sub(MAX_FILES);
    files.truncate(MAX_FILES);
    (files, beyond)
}

/// Expand named paths: a directory contributes its Markdown files.
fn named_files(root: &Path, paths: &[PathBuf]) -> (Vec<PathBuf>, usize) {
    let mut files = Vec::new();
    for path in paths {
        let path = if path.is_absolute() {
            path.clone()
        } else {
            root.join(path)
        };
        if path.is_dir() {
            let mut found: Vec<PathBuf> = ignore::WalkBuilder::new(&path)
                .max_depth(Some(MAX_DOCS_DEPTH))
                .build()
                .flatten()
                .map(ignore::DirEntry::into_path)
                .filter(|path| path.is_file() && is_markdown(path))
                .collect();
            found.sort();
            files.extend(found);
        } else {
            files.push(path);
        }
    }
    let beyond = files.len().saturating_sub(MAX_FILES);
    files.truncate(MAX_FILES);
    (files, beyond)
}

fn is_instruction_file(relative: &Path) -> bool {
    let name = relative.to_string_lossy();
    INSTRUCTION_FILES.iter().any(|file| name == *file)
        || relative.starts_with(Path::new(".cursor").join("rules"))
}

/// Read the project's instruction files and docs (or the named `paths`)
/// below `project_root` into facts. Nothing is stored here.
#[must_use]
pub fn import(project_root: &Path, paths: &[PathBuf]) -> ImportResult {
    use crate::tools::admitted_sources::{self, SourceRead};
    let mut result = ImportResult::default();
    let Ok(root) = project_root.canonicalize() else {
        result.refuse(SourceRefusal::Unreadable);
        return result;
    };
    let (files, beyond) = if paths.is_empty() {
        default_files(&root)
    } else {
        named_files(&root, paths)
    };
    result.sources_skipped += beyond;
    let gateway = crate::core::context_admission::carrier::gateway_snapshot();
    let rules = Some(gateway.admission_policy());
    let mut seen = HashSet::new();
    let root_text = root.to_string_lossy().into_owned();
    for file in files {
        let Ok(path) = file.canonicalize() else {
            result.refuse(SourceRefusal::Unreadable);
            continue;
        };
        let Ok(relative) = path.strip_prefix(&root) else {
            result.refuse(SourceRefusal::OutsideRoot);
            continue;
        };
        // Judged by what is read, not by the name it is reached under: a
        // committed `notes.md` linking to `.env` or `.git/config` is no doc.
        if relative.starts_with(".git") || !(is_markdown(&path) || is_instruction_file(relative)) {
            result.refuse(SourceRefusal::Withheld);
            continue;
        }
        let text = match admitted_sources::read_with_admission(
            &root_text,
            &path.to_string_lossy(),
            "ctx_knowledge",
            &gateway,
        ) {
            read @ SourceRead::Admitted(_) => {
                let Some(text) = read.storable() else {
                    result.refuse(SourceRefusal::Withheld);
                    continue;
                };
                text.to_owned()
            }
            SourceRead::Withheld(_) => {
                result.refuse(SourceRefusal::Withheld);
                continue;
            }
            SourceRead::Unavailable(_) => {
                result.refuse(SourceRefusal::Unreadable);
                continue;
            }
        };
        let relative_text = relative.to_string_lossy().replace('\\', "/");
        let session = super::session_label(ImportSource::Docs, &relative_text, rules);
        let instructions = is_instruction_file(relative);
        result.sessions_found += 1;
        // Each fact under a heading is anchored to its section (J2): an
        // edit there makes it *to verify*, a removed section `source_removed`.
        let sections = crate::core::knowledge::anchors::markdown_sections(&text);
        let section_of = |offset: usize| {
            sections
                .iter()
                .filter(|(_, range)| range.contains(&offset))
                .max_by_key(|(_, range)| range.start)
                .map(|(symbol, _)| symbol.as_str())
        };
        let scope = result.repo_scope.get_or_insert_with(Default::default);
        scope.files.push(relative_text.clone());
        for entry in parse(&text, instructions) {
            let anchors = section_of(entry.offset)
                .and_then(|symbol| {
                    crate::core::knowledge::anchors::capture(&root, &relative_text, Some(symbol))
                        .ok()
                })
                .into_iter()
                .collect();
            push_repo_fact(
                &mut result,
                &mut seen,
                ImportSource::Docs,
                &session,
                rules,
                RepoFact {
                    category: entry.category,
                    key: format!("{relative_text}#{}", entry.anchor),
                    value: entry.text,
                    confidence: if instructions { 0.8 } else { 0.6 },
                    validity: instructions.then_some(FactValidity::Convention),
                    valid_from: None,
                    anchors,
                },
            );
        }
    }
    result.files_touched = result.sessions_found;
    result.facts_extracted = result.facts.len();
    if paths.is_empty() {
        result.repo_scope.get_or_insert_with(Default::default).whole = true;
    }
    result
}

#[cfg(test)]
#[path = "markdown_tests.rs"]
mod tests;
