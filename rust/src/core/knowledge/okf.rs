//! Open Knowledge Format (OKF) rendering for lean-ctx project knowledge.
//!
//! OKF (Google Cloud, June 2026) formalises the "LLM-wiki" pattern: a *directory
//! of Markdown files*, one concept per file, each with a small YAML frontmatter
//! block (only `type` is mandatory) and a Markdown body; concepts link to each
//! other with ordinary Markdown links, and reserved `index.md` / `log.md` files
//! provide progressive disclosure and history. It is vendor-neutral, human- and
//! agent-readable, and git-diffable — the portable, no-lock-in counterpart to the
//! signed `ctxpkg` bundle.
//!
//! This module renders a [`KnowledgeSnapshot`] to an OKF bundle and parses a
//! bundle back into facts + relations. Both directions go through that *shared*
//! snapshot, so OKF and ctxpkg can never disagree on what the project's
//! knowledge is.
//!
//! ## Determinism (#498)
//! Every byte of the export is a pure function of the snapshot: frontmatter keys
//! are emitted in a fixed order (reserved OKF keys first, then sorted
//! `leanctx_*`), file slugs are stable, relation lines are sorted, and no
//! `now()` or counter ever reaches the output. Two exports of the same snapshot
//! are byte-identical, which keeps provider prompt-caching effective.
//!
//! ## Lossless round-trip
//! lean-ctx-specific fields ride along as producer-owned `leanctx_*` keys (OKF
//! §reserves only a small set and lets producers add their own), so an
//! export -> import cycle reconstructs the same facts, archetypes, and relations.
//! Foreign bundles that carry only `type` import as plain facts rather than
//! failing.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use walkdir::WalkDir;

use crate::core::knowledge_relations::{KnowledgeEdgeKind, KnowledgeNodeRef, parse_node_ref};
use crate::core::memory_boundary::FactPrivacy;
use crate::core::sensitivity;

use super::snapshot::KnowledgeSnapshot;
use super::types::{KnowledgeArchetype, KnowledgeFact, ProjectPattern};

/// Reserved OKF filenames that are *not* concepts: an overview index and a
/// chronological change log. Skipped on import.
const INDEX_FILE: &str = "index.md";
const LOG_FILE: &str = "log.md";
/// Directory (one level under the bundle root) that holds project patterns.
const PATTERNS_DIR: &str = "patterns";

/// A rendered OKF bundle: relative file path -> file contents. A `BTreeMap` so
/// iteration (and thus writing) is deterministic.
pub type OkfBundle = BTreeMap<String, String>;

/// A relation parsed from a concept's `## Relations` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OkfEdge {
    pub from: KnowledgeNodeRef,
    pub to: KnowledgeNodeRef,
    pub kind: KnowledgeEdgeKind,
}

/// The result of parsing an OKF directory back into lean-ctx structures.
#[derive(Debug, Default)]
pub struct OkfImport {
    pub facts: Vec<KnowledgeFact>,
    pub patterns: Vec<ProjectPattern>,
    pub edges: Vec<OkfEdge>,
}

/// Where a concept lives inside the bundle: `<dir>/<file>.md`.
struct ConceptLoc {
    dir: String,
    file: String,
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Renders a snapshot to a deterministic OKF bundle (path -> contents). Only
/// current facts become concepts; superseded history stays in ctxpkg.
pub fn to_okf_bundle(snapshot: &KnowledgeSnapshot) -> OkfBundle {
    let mut facts = snapshot.current_facts();
    facts.sort_by(|a, b| a.category.cmp(&b.category).then_with(|| a.key.cmp(&b.key)));

    // Pass 1: resolve a stable, unique file location for every fact node so that
    // relation links (pass 2) can point at real files.
    let mut used: HashSet<(String, String)> = HashSet::new();
    let mut paths: HashMap<KnowledgeNodeRef, ConceptLoc> = HashMap::new();
    for f in &facts {
        let node = KnowledgeNodeRef::new(&f.category, &f.key);
        if paths.contains_key(&node) {
            continue;
        }
        let dir = dir_slug(&f.category);
        let file = unique_slug(&dir, &f.key, &mut used);
        paths.insert(node, ConceptLoc { dir, file });
    }

    let mut bundle = OkfBundle::new();

    // Pass 2: render each concept with its outgoing relations.
    for f in &facts {
        let node = KnowledgeNodeRef::new(&f.category, &f.key);
        let Some(loc) = paths.get(&node) else {
            continue;
        };
        let mut rel_lines: Vec<String> = snapshot
            .relations
            .iter()
            .filter(|e| e.from == node && paths.contains_key(&e.to))
            .map(|e| {
                let target = &paths[&e.to];
                let link = relative_link(&loc.dir, target);
                format!("- {}: [{}]({link})", e.kind.as_str(), e.to.id())
            })
            .collect();
        rel_lines.sort();
        rel_lines.dedup();

        bundle.insert(
            format!("{}/{}.md", loc.dir, loc.file),
            render_concept(f, &rel_lines),
        );
    }

    // Patterns: one file each under patterns/.
    let mut patterns = snapshot.patterns.clone();
    patterns.sort_by(|a, b| {
        a.pattern_type
            .cmp(&b.pattern_type)
            .then_with(|| a.description.cmp(&b.description))
    });
    for p in &patterns {
        let file = unique_slug(PATTERNS_DIR, &p.pattern_type, &mut used);
        bundle.insert(format!("{PATTERNS_DIR}/{file}.md"), render_pattern(p));
    }

    // Reserved files: index always, log only when there is history.
    bundle.insert(INDEX_FILE.to_string(), render_index(&facts, &patterns));
    if !snapshot.insights.is_empty() {
        bundle.insert(LOG_FILE.to_string(), render_log(snapshot));
    }

    bundle
}

/// Writes a bundle to `dir`, creating category subdirectories as needed.
pub fn write_okf_bundle(dir: &Path, bundle: &OkfBundle) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    for (rel, contents) in bundle {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, contents).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn render_concept(f: &KnowledgeFact, rel_lines: &[String]) -> String {
    let mut out = emit_frontmatter(&concept_frontmatter(f));
    out.push('\n');
    out.push_str(f.value.trim_end());
    out.push('\n');
    if !rel_lines.is_empty() {
        out.push_str("\n## Relations\n\n");
        for line in rel_lines {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Ordered frontmatter for a fact: reserved OKF keys first (fixed order), then
/// producer-owned `leanctx_*` keys sorted for determinism and lossless import.
/// Emits an `f32` as its shortest round-trippable decimal so frontmatter shows
/// `0.9` rather than the f64-widened `0.8999999761581421`. Keeps git diffs and
/// prompt-cache bytes stable while parsing back to the same `f32` on import.
fn clean_f32(x: f32) -> Value {
    let parsed: f64 = format!("{x}").parse().unwrap_or_else(|_| f64::from(x));
    serde_json::Number::from_f64(parsed).map_or(Value::Null, Value::Number)
}

fn concept_frontmatter(f: &KnowledgeFact) -> Vec<(String, Value)> {
    let mut pairs: Vec<(String, Value)> = Vec::new();
    pairs.push(("type".into(), Value::from(f.archetype.as_type_str())));
    pairs.push(("title".into(), Value::from(f.key.clone())));
    let desc = first_line(&f.value);
    if !desc.is_empty() {
        pairs.push(("description".into(), Value::from(desc)));
    }
    pairs.push(("tags".into(), Value::from(vec![f.category.clone()])));
    pairs.push((
        "timestamp".into(),
        Value::from(f.last_confirmed.to_rfc3339()),
    ));

    let mut extra: BTreeMap<String, Value> = BTreeMap::new();
    extra.insert(
        "leanctx_origin".into(),
        serde_json::to_value(&f.origin).unwrap_or(Value::Null),
    );
    extra.insert("leanctx_archetype".into(), f.archetype.as_type_str().into());
    extra.insert("leanctx_category".into(), f.category.clone().into());
    extra.insert("leanctx_confidence".into(), clean_f32(f.confidence));
    extra.insert(
        "leanctx_confirmation_count".into(),
        f.confirmation_count.into(),
    );
    extra.insert(
        "leanctx_created_at".into(),
        f.created_at.to_rfc3339().into(),
    );
    extra.insert("leanctx_key".into(), f.key.clone().into());
    extra.insert(
        "leanctx_last_confirmed".into(),
        f.last_confirmed.to_rfc3339().into(),
    );
    extra.insert("leanctx_revision_count".into(), f.revision_count.into());
    extra.insert(
        "leanctx_sensitivity".into(),
        serde_json::to_value(f.sensitivity).unwrap_or_else(|_| "public".into()),
    );
    extra.insert(
        "leanctx_source_session".into(),
        f.source_session.clone().into(),
    );
    if let Some(vf) = f.valid_from {
        extra.insert("leanctx_valid_from".into(), vf.to_rfc3339().into());
    }
    if let Some(vu) = f.valid_until {
        extra.insert("leanctx_valid_until".into(), vu.to_rfc3339().into());
    }
    if let Some(s) = &f.supersedes {
        extra.insert("leanctx_supersedes".into(), s.clone().into());
    }
    pairs.extend(extra);
    pairs
}

fn render_pattern(p: &ProjectPattern) -> String {
    let mut pairs: Vec<(String, Value)> = Vec::new();
    pairs.push(("type".into(), Value::from("pattern")));
    pairs.push(("title".into(), Value::from(p.pattern_type.clone())));
    let desc = first_line(&p.description);
    if !desc.is_empty() {
        pairs.push(("description".into(), Value::from(desc)));
    }
    let mut extra: BTreeMap<String, Value> = BTreeMap::new();
    extra.insert(
        "leanctx_created_at".into(),
        p.created_at.to_rfc3339().into(),
    );
    extra.insert("leanctx_examples".into(), Value::from(p.examples.clone()));
    extra.insert("leanctx_kind".into(), "pattern".into());
    extra.insert(
        "leanctx_source_session".into(),
        p.source_session.clone().into(),
    );
    pairs.extend(extra);

    let mut out = emit_frontmatter(&pairs);
    out.push('\n');
    out.push_str(p.description.trim_end());
    out.push('\n');
    if !p.examples.is_empty() {
        out.push_str("\n## Examples\n\n");
        for ex in &p.examples {
            out.push_str("- ");
            out.push_str(ex.trim());
            out.push('\n');
        }
    }
    out
}

fn render_index(facts: &[&KnowledgeFact], patterns: &[ProjectPattern]) -> String {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for f in facts {
        *counts.entry(f.category.clone()).or_insert(0) += 1;
    }
    let mut out = String::from(
        "# Knowledge Index\n\nOpen Knowledge Format (OKF) bundle exported by lean-ctx.\n\n## Concepts by category\n\n",
    );
    if counts.is_empty() {
        out.push_str("_none_\n");
    } else {
        for (cat, n) in &counts {
            out.push_str(&format!("- {} ({})\n", dir_slug(cat), n));
        }
    }
    out.push_str(&format!("\n## Patterns ({})\n", patterns.len()));
    out
}

fn render_log(snapshot: &KnowledgeSnapshot) -> String {
    let mut insights = snapshot.insights.clone();
    insights.sort_by(|a, b| {
        a.timestamp
            .cmp(&b.timestamp)
            .then_with(|| a.summary.cmp(&b.summary))
    });
    let mut out = String::from("# Change Log\n\n");
    for i in &insights {
        out.push_str(&format!("## {}\n\n", i.timestamp.to_rfc3339()));
        out.push_str(i.summary.trim());
        out.push('\n');
        if !i.from_sessions.is_empty() {
            out.push_str(&format!("\n_sessions: {}_\n", i.from_sessions.join(", ")));
        }
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// Parses an OKF directory into facts, patterns, and relations. Lenient by
/// design: files without a `type` (or without frontmatter) are skipped rather
/// than failing the whole import; use [`lint_okf_bundle`] to surface those.
pub fn from_okf_dir(dir: &Path) -> Result<OkfImport, String> {
    if !dir.is_dir() {
        return Err(format!("not a directory: {}", dir.display()));
    }
    let mut imp = OkfImport::default();
    for path in concept_files(dir) {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some((fm_str, body)) = split_frontmatter(&content) else {
            continue;
        };
        let fm = parse_frontmatter_map(&fm_str);
        if !fm.contains_key("type") {
            continue;
        }

        if fm.get("leanctx_kind").and_then(Value::as_str) == Some("pattern") {
            if let Some(p) = build_pattern(&fm, &body) {
                imp.patterns.push(p);
            }
            continue;
        }

        let category = concept_category(&fm);
        let key = concept_key(&fm);
        let from = KnowledgeNodeRef::new(&category, &key);
        imp.facts.push(build_fact(&fm, &body, category, key));
        imp.edges.extend(parse_relations(&body, &from));
    }
    Ok(imp)
}

/// Non-fatal conformance checks. Returns warnings only — OKF's own tooling
/// treats these as advisory, and a partially-malformed bundle should still
/// import what it can.
pub fn lint_okf_bundle(dir: &Path) -> Vec<String> {
    let mut warnings = Vec::new();
    if !dir.is_dir() {
        warnings.push(format!("not a directory: {}", dir.display()));
        return warnings;
    }
    for path in concept_files(dir) {
        let rel = path
            .strip_prefix(dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        let Ok(content) = std::fs::read_to_string(&path) else {
            warnings.push(format!("{rel}: unreadable"));
            continue;
        };
        let Some((fm_str, body)) = split_frontmatter(&content) else {
            warnings.push(format!("{rel}: missing YAML frontmatter"));
            continue;
        };
        let fm = parse_frontmatter_map(&fm_str);
        if !fm.contains_key("type") {
            warnings.push(format!("{rel}: missing required `type` field"));
        }
        if split_body_content(&body).is_empty() {
            warnings.push(format!("{rel}: empty concept body"));
        }
    }
    warnings
}

fn build_fact(fm: &Map<String, Value>, body: &str, category: String, key: String) -> KnowledgeFact {
    let type_str = get_str(fm, "type").unwrap_or_else(|| "fact".to_string());
    let content = split_body_content(body);
    let value = if content.is_empty() {
        get_str(fm, "description").unwrap_or_default()
    } else {
        content
    };
    let archetype = get_str(fm, "leanctx_archetype").map_or_else(
        || KnowledgeArchetype::from_type_str(&type_str),
        |s| KnowledgeArchetype::from_type_str(&s),
    );
    let confidence = fm
        .get("leanctx_confidence")
        .and_then(Value::as_f64)
        .map_or(0.8, |v| v as f32);
    let source_session =
        get_str(fm, "leanctx_source_session").unwrap_or_else(|| "okf-import".to_string());
    let created_at = get_dt(fm, "leanctx_created_at").unwrap_or_else(Utc::now);
    let last_confirmed = get_dt(fm, "leanctx_last_confirmed")
        .or_else(|| get_dt(fm, "timestamp"))
        .unwrap_or(created_at);
    let valid_from = get_dt(fm, "leanctx_valid_from").or(Some(created_at));
    let valid_until = get_dt(fm, "leanctx_valid_until");
    let confirmation_count = fm
        .get("leanctx_confirmation_count")
        .and_then(Value::as_u64)
        .unwrap_or(1) as u32;
    let revision_count = fm
        .get("leanctx_revision_count")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let sensitivity = get_str(fm, "leanctx_sensitivity")
        .and_then(|s| serde_json::from_value(Value::String(s)).ok())
        .unwrap_or_else(|| sensitivity::classify_content(&value));

    KnowledgeFact {
        origin: super::source_view::imported_origin(
            &fm.get("leanctx_origin")
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .unwrap_or_default(),
        ),
        category,
        key,
        value,
        source_session,
        confidence,
        created_at,
        last_confirmed,
        retrieval_count: 0,
        last_retrieved: None,
        valid_from,
        valid_until,
        supersedes: get_str(fm, "leanctx_supersedes"),
        confirmation_count,
        feedback_up: 0,
        feedback_down: 0,
        last_feedback: None,
        privacy: FactPrivacy::default(),
        sensitivity,
        imported_from: Some("okf".to_string()),
        archetype,
        fidelity: None,
        revision_count,
    }
}

fn build_pattern(fm: &Map<String, Value>, body: &str) -> Option<ProjectPattern> {
    let pattern_type = get_str(fm, "title")?;
    let content = split_body_content(body);
    let description = if content.is_empty() {
        get_str(fm, "description").unwrap_or_default()
    } else {
        content
    };
    let examples = fm
        .get("leanctx_examples")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Some(ProjectPattern {
        pattern_type,
        description,
        examples,
        source_session: get_str(fm, "leanctx_source_session")
            .unwrap_or_else(|| "okf-import".to_string()),
        created_at: get_dt(fm, "leanctx_created_at").unwrap_or_else(Utc::now),
    })
}

fn parse_relations(body: &str, from: &KnowledgeNodeRef) -> Vec<OkfEdge> {
    let mut edges = Vec::new();
    let mut in_relations = false;
    for line in body.lines() {
        let t = line.trim();
        if t.eq_ignore_ascii_case("## relations") {
            in_relations = true;
            continue;
        }
        if in_relations && t.starts_with("## ") {
            break;
        }
        if !in_relations {
            continue;
        }
        let Some(rest) = t.strip_prefix("- ") else {
            continue;
        };
        let Some((kind_str, link_part)) = rest.split_once(':') else {
            continue;
        };
        let Some(kind) = KnowledgeEdgeKind::parse(kind_str.trim()) else {
            continue;
        };
        // Extract the `[label]` target id from `[label](path)`.
        let label = link_part.trim().trim_start_matches('[');
        let Some(end) = label.find(']') else {
            continue;
        };
        if let Some(to) = parse_node_ref(&label[..end]) {
            edges.push(OkfEdge {
                from: from.clone(),
                to,
                kind,
            });
        }
    }
    edges
}

// ---------------------------------------------------------------------------
// Frontmatter helpers
// ---------------------------------------------------------------------------

/// Emits YAML frontmatter deterministically. Scalars are rendered via
/// `serde_json` (JSON is a YAML 1.2 subset, so quoted strings / numbers / bools
/// are valid YAML and unambiguous); arrays use block style for readability.
fn emit_frontmatter(pairs: &[(String, Value)]) -> String {
    let mut s = String::from("---\n");
    for (k, v) in pairs {
        match v {
            Value::Array(items) if !items.is_empty() => {
                s.push_str(k);
                s.push_str(":\n");
                for it in items {
                    s.push_str("  - ");
                    s.push_str(&serde_json::to_string(it).unwrap_or_else(|_| "null".into()));
                    s.push('\n');
                }
            }
            Value::Array(_) => {
                s.push_str(k);
                s.push_str(": []\n");
            }
            _ => {
                s.push_str(k);
                s.push_str(": ");
                s.push_str(&serde_json::to_string(v).unwrap_or_else(|_| "null".into()));
                s.push('\n');
            }
        }
    }
    s.push_str("---\n");
    s
}

fn parse_frontmatter_map(fm: &str) -> Map<String, Value> {
    yaml_serde::from_str::<Value>(fm)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Splits a Markdown document into `(frontmatter, body)` if it opens with a
/// `---` fenced YAML block. Returns `None` when there is no frontmatter.
fn split_frontmatter(content: &str) -> Option<(String, String)> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let after = content.strip_prefix("---")?;
    let after = after
        .strip_prefix("\r\n")
        .or_else(|| after.strip_prefix('\n'))?;

    let mut fm = String::new();
    let mut body = String::new();
    let mut closed = false;
    let mut in_body = false;
    for line in after.lines() {
        if in_body {
            body.push_str(line);
            body.push('\n');
        } else if line.trim_end() == "---" {
            in_body = true;
            closed = true;
        } else {
            fm.push_str(line);
            fm.push('\n');
        }
    }
    closed.then_some((fm, body))
}

/// The concept body with any `## Relations` (and later sections) stripped —
/// i.e. the actual knowledge content.
fn split_body_content(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        let t = line.trim();
        if t.eq_ignore_ascii_case("## relations") {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

fn concept_category(fm: &Map<String, Value>) -> String {
    get_str(fm, "leanctx_category")
        .or_else(|| get_str_array_first(fm, "tags"))
        .unwrap_or_else(|| "imported".to_string())
}

fn concept_key(fm: &Map<String, Value>) -> String {
    get_str(fm, "leanctx_key")
        .or_else(|| get_str(fm, "title"))
        .unwrap_or_else(|| "concept".to_string())
}

fn get_str(fm: &Map<String, Value>, key: &str) -> Option<String> {
    fm.get(key).and_then(Value::as_str).map(str::to_string)
}

fn get_str_array_first(fm: &Map<String, Value>, key: &str) -> Option<String> {
    fm.get(key)?
        .as_array()?
        .iter()
        .find_map(|v| v.as_str().map(String::from))
}

fn get_dt(fm: &Map<String, Value>, key: &str) -> Option<DateTime<Utc>> {
    let s = fm.get(key).and_then(Value::as_str)?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

// ---------------------------------------------------------------------------
// Slugs & paths
// ---------------------------------------------------------------------------

fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Deterministic, filesystem-safe slug: lowercase alphanumerics, single dashes
/// for separators, trimmed, capped.
fn slug_like(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if out.len() >= 60 {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn dir_slug(category: &str) -> String {
    let s = slug_like(category);
    if s.is_empty() { "misc".to_string() } else { s }
}

/// A slug unique within `dir`. On collision (two keys slugging alike) a short
/// BLAKE3 digest of the full key is appended — stable regardless of iteration
/// order, so the bundle stays deterministic.
fn unique_slug(dir: &str, key: &str, used: &mut HashSet<(String, String)>) -> String {
    let base = {
        let s = slug_like(key);
        if s.is_empty() { "fact".to_string() } else { s }
    };
    if used.insert((dir.to_string(), base.clone())) {
        return base;
    }
    let suffix = &blake3::hash(key.as_bytes()).to_hex()[..8];
    let cand = format!("{base}-{suffix}");
    used.insert((dir.to_string(), cand.clone()));
    cand
}

fn relative_link(from_dir: &str, to: &ConceptLoc) -> String {
    if from_dir == to.dir {
        format!("{}.md", to.file)
    } else {
        format!("../{}/{}.md", to.dir, to.file)
    }
}

/// All concept `*.md` files in the bundle, sorted, excluding reserved
/// `index.md` / `log.md`.
fn concept_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<std::path::PathBuf> = WalkDir::new(dir)
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name != INDEX_FILE && name != LOG_FILE
        })
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::knowledge_relations::KnowledgeRelationGraph;

    fn fact(
        category: &str,
        key: &str,
        value: &str,
        archetype: KnowledgeArchetype,
    ) -> KnowledgeFact {
        let now = Utc::now();
        KnowledgeFact {
            origin: crate::core::knowledge::FactOrigin::Local,
            category: category.into(),
            key: key.into(),
            value: value.into(),
            source_session: "s1".into(),
            confidence: 0.9,
            created_at: now,
            last_confirmed: now,
            retrieval_count: 0,
            last_retrieved: None,
            valid_from: Some(now),
            valid_until: None,
            supersedes: None,
            confirmation_count: 1,
            feedback_up: 0,
            feedback_down: 0,
            last_feedback: None,
            privacy: FactPrivacy::default(),
            sensitivity: crate::core::sensitivity::SensitivityLevel::default(),
            imported_from: None,
            archetype,
            fidelity: None,
            revision_count: 1,
        }
    }

    fn sample_snapshot() -> KnowledgeSnapshot {
        let facts = vec![
            fact(
                "architecture",
                "auth",
                "Auth uses JWT RS256 tokens verified against Redis sessions.",
                KnowledgeArchetype::Architecture,
            ),
            fact(
                "architecture",
                "db",
                "PostgreSQL 16 with pgvector for the primary datastore.",
                KnowledgeArchetype::Architecture,
            ),
        ];
        let mut graph = KnowledgeRelationGraph::new("hash");
        graph.upsert_edge(
            KnowledgeNodeRef::new("architecture", "auth"),
            KnowledgeNodeRef::new("architecture", "db"),
            KnowledgeEdgeKind::DependsOn,
            "s1",
        );
        KnowledgeSnapshot {
            project_root: "/tmp/proj".into(),
            project_hash: "hash".into(),
            facts,
            patterns: Vec::new(),
            insights: Vec::new(),
            relations: graph.edges,
            task_id: None,
            policy_version: None,
            knowledge_refs: Vec::new(),
            snapshot_hash: None,
        }
    }

    fn key_set(facts: &[KnowledgeFact]) -> HashSet<(String, String, String, String)> {
        facts
            .iter()
            .map(|f| {
                (
                    f.category.clone(),
                    f.key.clone(),
                    f.value.clone(),
                    f.archetype.as_type_str().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn round_trip_preserves_facts_and_relations() {
        let snap = sample_snapshot();
        let bundle = to_okf_bundle(&snap);
        let dir = tempfile::tempdir().unwrap();
        write_okf_bundle(dir.path(), &bundle).unwrap();

        let imported = from_okf_dir(dir.path()).unwrap();
        assert_eq!(
            key_set(&imported.facts),
            key_set(&snap.facts),
            "current facts (category/key/value/archetype) survive the round-trip"
        );

        assert_eq!(imported.edges.len(), 1, "the depends_on relation survives");
        let e = &imported.edges[0];
        assert_eq!(e.from, KnowledgeNodeRef::new("architecture", "auth"));
        assert_eq!(e.to, KnowledgeNodeRef::new("architecture", "db"));
        assert_eq!(e.kind, KnowledgeEdgeKind::DependsOn);
    }

    #[test]
    fn export_is_byte_deterministic() {
        let snap = sample_snapshot();
        assert_eq!(
            to_okf_bundle(&snap),
            to_okf_bundle(&snap),
            "two exports of the same snapshot are byte-identical (#498)"
        );
    }

    #[test]
    fn okf_concepts_match_current_facts() {
        // The shared-core guarantee: the OKF rendering derives exactly the
        // snapshot's current facts — one concept file per current fact.
        let snap = sample_snapshot();
        let bundle = to_okf_bundle(&snap);
        let concept_count = bundle
            .keys()
            .filter(|k| *k != INDEX_FILE && *k != LOG_FILE && !k.starts_with(PATTERNS_DIR))
            .count();
        assert_eq!(concept_count, snap.current_facts().len());
    }

    #[test]
    fn foreign_bundle_needs_only_type() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("note.md"),
            "---\ntype: architecture\n---\n\nWe run everything on Kubernetes.\n",
        )
        .unwrap();

        let imp = from_okf_dir(dir.path()).unwrap();
        assert_eq!(
            imp.facts.len(),
            1,
            "a type-only concept imports as one fact"
        );
        let f = &imp.facts[0];
        assert_eq!(f.archetype, KnowledgeArchetype::Architecture);
        assert_eq!(f.value, "We run everything on Kubernetes.");
        assert_eq!(
            f.key, "concept",
            "no title/leanctx_key falls back to a default"
        );
    }

    #[test]
    fn unknown_frontmatter_keys_survive_a_parse_emit_cycle() {
        // OKF lets producers add arbitrary keys and requires consumers to keep
        // them. Our frontmatter emitter/parser round-trips an unknown key.
        let src = "custom_producer_key: \"keep me\"\ntype: \"fact\"\n";
        let map = parse_frontmatter_map(src);
        let pairs: Vec<(String, Value)> = map.into_iter().collect();
        let emitted = emit_frontmatter(&pairs);
        let reparsed = parse_frontmatter_map(
            emitted
                .trim_start_matches("---\n")
                .trim_end_matches("---\n"),
        );
        assert_eq!(
            reparsed.get("custom_producer_key").and_then(Value::as_str),
            Some("keep me")
        );
    }

    #[test]
    fn lint_reports_warnings_never_panics() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bad.md"), "no frontmatter here\n").unwrap();
        std::fs::write(dir.path().join("ok.md"), "---\ntype: fact\n---\n\nbody\n").unwrap();
        let warnings = lint_okf_bundle(dir.path());
        assert!(
            warnings.iter().any(|w| w.contains("bad.md")),
            "the malformed file is flagged: {warnings:?}"
        );
        // The good file still imports despite the bad one.
        assert_eq!(from_okf_dir(dir.path()).unwrap().facts.len(), 1);
    }

    #[test]
    fn patterns_round_trip_with_examples() {
        let mut snap = sample_snapshot();
        snap.patterns.push(ProjectPattern {
            pattern_type: "naming".into(),
            description: "snake_case for functions".into(),
            examples: vec!["get_user()".into()],
            source_session: "s1".into(),
            created_at: Utc::now(),
        });
        let bundle = to_okf_bundle(&snap);
        let dir = tempfile::tempdir().unwrap();
        write_okf_bundle(dir.path(), &bundle).unwrap();

        let imp = from_okf_dir(dir.path()).unwrap();
        assert_eq!(imp.patterns.len(), 1);
        assert_eq!(imp.patterns[0].pattern_type, "naming");
        assert_eq!(imp.patterns[0].examples, vec!["get_user()".to_string()]);
    }
}
