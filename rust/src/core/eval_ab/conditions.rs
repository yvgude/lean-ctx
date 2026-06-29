//! A/B context conditions (#235): the two context layers a task is run under.
//!
//! Both conditions are handed the **same token budget**, so any quality difference is
//! attributable to *what* lean-ctx put in the window, not *how much*:
//!
//! * [`Condition::Baseline`] — "without lean-ctx": raw files in deterministic path order,
//!   packed until the budget is full (the naive "dump the repo" approach).
//! * [`Condition::LeanCtx`] — "with lean-ctx": BM25 relevance-ranks files against the task
//!   query, then packs them through [`aggressive_compress`] so more *relevant* signal fits in
//!   the same budget.
//!
//! Every assembled context carries a `digest` (hex SHA-256 of the exact bytes) so the report
//! can prove which window each answer was produced from.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::core::bm25_index::BM25Index;
use crate::core::compressor::aggressive_compress;
use crate::core::tokens::count_tokens;

use super::sha256_hex;

/// Default budget both conditions respect. Small enough to force selection pressure.
pub const DEFAULT_BUDGET_TOKENS: usize = 4000;

/// Largest single file (bytes) considered for context — skips vendored blobs / binaries.
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// Which context layer the model receives for a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    /// Baseline — "without lean-ctx".
    Baseline,
    /// Treatment — "with lean-ctx".
    LeanCtx,
    /// Treatment variant that routes JSON/JSONL through the deduplicating
    /// [`crate::core::json_crush`] core (lossless array crush) instead of the
    /// whitespace-only compaction the generic [`aggressive_compress`] applies to
    /// structured data. Used to measure json_crush's token savings and its
    /// answer-preservation floor in isolation (#942).
    JsonCrush,
    /// Treatment variant that routes CSV/TSV through the columnar
    /// [`crate::core::tabular_crush`] core (lossless constant-column hoisting)
    /// instead of the line-based compaction the generic [`aggressive_compress`]
    /// applies to delimited data. Measures tabular_crush's token savings and its
    /// answer-preservation floor in isolation (#982).
    TabularCrush,
    /// Treatment variant that routes YAML through the [`crate::core::yaml_crush`]
    /// core (YAML → compact JSON + lossless array factoring) instead of the
    /// line-based compaction the generic [`aggressive_compress`] applies to YAML.
    /// Measures yaml_crush's token savings and its answer-preservation floor in
    /// isolation (#985).
    YamlCrush,
}

impl Condition {
    /// Stable label used in reports + the determinism digest.
    pub fn label(self) -> &'static str {
        match self {
            Condition::Baseline => "baseline",
            Condition::LeanCtx => "lean_ctx",
            Condition::JsonCrush => "json_crush",
            Condition::TabularCrush => "tabular_crush",
            Condition::YamlCrush => "yaml_crush",
        }
    }
}

/// The assembled context for one (task, condition) pair.
#[derive(Debug, Clone)]
pub struct AssembledContext {
    /// The exact context string placed before the task prompt.
    pub text: String,
    /// Token count of `text` (≤ budget).
    pub tokens: usize,
    /// Number of files that contributed.
    pub files: usize,
    /// Hex SHA-256 of `text` — the auditable context fingerprint.
    pub digest: String,
}

/// Assembles the context for `condition` from `workspace`, honouring `budget` tokens.
pub fn assemble(
    condition: Condition,
    workspace: &Path,
    query: &str,
    budget: usize,
) -> Result<AssembledContext> {
    let entries = match condition {
        Condition::Baseline => baseline_entries(workspace),
        Condition::LeanCtx => lean_ctx_entries(workspace, query),
        Condition::JsonCrush => json_crush_entries(workspace, query),
        Condition::TabularCrush => tabular_crush_entries(workspace, query),
        Condition::YamlCrush => yaml_crush_entries(workspace, query),
    };
    Ok(pack(&entries, budget))
}

/// `(relpath, rendered_content)` in baseline order: every text file, path-sorted, raw.
fn baseline_entries(workspace: &Path) -> Vec<(String, String)> {
    let mut files = gather_text_files(workspace);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

/// `(relpath, rendered_content)` in lean-ctx order: BM25-ranked by `query`, then compressed.
fn lean_ctx_entries(workspace: &Path, query: &str) -> Vec<(String, String)> {
    ranked_entries(workspace, query, |content, ext| {
        aggressive_compress(content, ext)
    })
}

/// Like [`lean_ctx_entries`], but JSON/JSONL files go through the deduplicating
/// `json_crush` core (lossless) when it pays, instead of whitespace-only
/// compaction. Every other file uses the same `aggressive_compress` path.
fn json_crush_entries(workspace: &Path, query: &str) -> Vec<(String, String)> {
    ranked_entries(workspace, query, |content, ext| match ext {
        Some("json" | "jsonl") => crate::core::json_crush::crush_text_if_beneficial(content)
            .unwrap_or_else(|| aggressive_compress(content, ext)),
        _ => aggressive_compress(content, ext),
    })
}

/// Like [`lean_ctx_entries`], but CSV/TSV files go through the columnar
/// `tabular_crush` core (lossless) when it pays, instead of line-based
/// compaction. Every other file uses the same `aggressive_compress` path.
fn tabular_crush_entries(workspace: &Path, query: &str) -> Vec<(String, String)> {
    ranked_entries(
        workspace,
        query,
        |content, ext| match crate::core::compressor::tabular_delimiter(ext) {
            Some(delim) => crate::core::tabular_crush::crush_text_if_beneficial(content, delim)
                .unwrap_or_else(|| aggressive_compress(content, ext)),
            None => aggressive_compress(content, ext),
        },
    )
}

/// Like [`lean_ctx_entries`], but YAML files go through the [`yaml_crush`] core
/// (YAML → compact JSON + lossless array factoring) when it pays, instead of
/// line-based compaction. Every other file uses the same `aggressive_compress`
/// path.
///
/// [`yaml_crush`]: crate::core::yaml_crush
fn yaml_crush_entries(workspace: &Path, query: &str) -> Vec<(String, String)> {
    ranked_entries(workspace, query, |content, ext| {
        if crate::core::compressor::is_yaml_ext(ext) {
            crate::core::yaml_crush::crush_text_if_beneficial(content)
                .unwrap_or_else(|| aggressive_compress(content, ext))
        } else {
            aggressive_compress(content, ext)
        }
    })
}

/// Shared BM25-ranked assembly: rank `workspace` files by `query`, render each
/// with `render(content, ext)`, dedup by path. Falls back to baseline ordering
/// when retrieval is empty so a treatment condition is never accidentally empty.
fn ranked_entries(
    workspace: &Path,
    query: &str,
    render: impl Fn(&str, Option<&str>) -> String,
) -> Vec<(String, String)> {
    let index = BM25Index::build_from_directory(workspace);
    let ranked = index.search(query, 256);
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for result in ranked {
        if !seen.insert(result.file_path.clone()) {
            continue;
        }
        let path = resolve(workspace, &result.file_path);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let ext = path.extension().and_then(|e| e.to_str());
        out.push((rel_label(workspace, &path), render(&content, ext)));
    }
    if out.is_empty() {
        return baseline_entries(workspace);
    }
    out
}

/// Resolves a BM25 `file_path` (relative or absolute) against the workspace root.
fn resolve(root: &Path, file_path: &str) -> PathBuf {
    let p = Path::new(file_path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    }
}

/// Label for a file inside the workspace (relative when possible, else the file name).
///
/// Separators are normalized to `/` so the assembled context — and therefore
/// every `RecordedRunner` replay key derived from it — is byte-identical on
/// Windows and Unix (#498). Without this, a nested fixture such as
/// `config/seed-data.json` would label as `config\seed-data.json` on Windows and
/// never match the committed Unix-generated recording.
fn rel_label(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Walks `root` (respecting .gitignore + hidden filters) and returns every readable UTF-8 file
/// under [`MAX_FILE_BYTES`] as `(relpath, content)`.
fn gather_text_files(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .filter_entry(crate::core::walk_filter::keep_entry)
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if entry.metadata().map_or(u64::MAX, |m| m.len()) > MAX_FILE_BYTES {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(path) {
            out.push((rel_label(root, path), content));
        }
    }
    out
}

/// Packs entries into one context string, greedily filling the budget then hard-capping it so
/// the result is always ≤ `budget` tokens.
fn pack(entries: &[(String, String)], budget: usize) -> AssembledContext {
    let mut text = String::new();
    let mut running = 0usize;
    let mut files = 0usize;
    for (label, content) in entries {
        let block = format!("// file: {label}\n{content}\n\n");
        let cost = count_tokens(&block);
        if running > 0 && running + cost > budget {
            continue;
        }
        text.push_str(&block);
        running += cost;
        files += 1;
        if running >= budget {
            break;
        }
    }
    let capped = truncate_to_tokens(&text, budget);
    let tokens = count_tokens(&capped);
    let digest = sha256_hex(capped.as_bytes());
    AssembledContext {
        text: capped,
        tokens,
        files,
        digest,
    }
}

/// Returns the longest char-prefix of `text` that stays within `budget` tokens.
fn truncate_to_tokens(text: &str, budget: usize) -> String {
    if count_tokens(text) <= budget {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        let candidate: String = chars[..mid].iter().collect();
        if count_tokens(&candidate) <= budget {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("relevant.md"),
            "The consolidation pipeline persists to bm25, graph, knowledge and session stores.",
        )
        .unwrap();
        fs::write(
            dir.path().join("noise.md"),
            "Lorem ipsum dolor sit amet, totally unrelated filler content about cats and weather.",
        )
        .unwrap();
        dir
    }

    #[test]
    fn conditions_produce_distinct_digests() {
        let ws = workspace();
        let a = assemble(Condition::Baseline, ws.path(), "consolidation stores", 4000).unwrap();
        let b = assemble(Condition::LeanCtx, ws.path(), "consolidation stores", 4000).unwrap();
        assert!(a.tokens > 0 && b.tokens > 0);
        assert_ne!(a.digest, b.digest, "raw vs compressed must differ");
    }

    #[test]
    fn assembly_is_deterministic() {
        let ws = workspace();
        let first = assemble(Condition::LeanCtx, ws.path(), "consolidation", 4000).unwrap();
        let second = assemble(Condition::LeanCtx, ws.path(), "consolidation", 4000).unwrap();
        assert_eq!(first.digest, second.digest);
    }

    #[test]
    fn json_crush_condition_beats_baseline_and_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let rows: Vec<String> = (0..30)
            .map(|i| {
                format!(
                    "{{\"id\":{i},\"role\":\"operator\",\"status\":\"active\",\"region\":\"emea\"}}"
                )
            })
            .collect();
        fs::write(
            dir.path().join("roster.json"),
            format!("[{}]", rows.join(",")),
        )
        .unwrap();

        let crushed = assemble(Condition::JsonCrush, dir.path(), "roster operator", 4000).unwrap();
        let baseline = assemble(Condition::Baseline, dir.path(), "roster operator", 4000).unwrap();
        assert!(
            crushed.tokens < baseline.tokens,
            "crush {} must beat baseline {}",
            crushed.tokens,
            baseline.tokens
        );

        let again = assemble(Condition::JsonCrush, dir.path(), "roster operator", 4000).unwrap();
        assert_eq!(
            crushed.digest, again.digest,
            "json_crush assembly is deterministic"
        );
    }

    #[test]
    fn tabular_crush_condition_beats_baseline_and_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let mut csv = String::from("id,name,status,region,tier\n");
        for i in 0..40 {
            csv.push_str(&format!("{i},user{i},active,eu-central-1,standard\n"));
        }
        fs::write(dir.path().join("roster.csv"), csv).unwrap();

        let crushed = assemble(Condition::TabularCrush, dir.path(), "roster status", 4000).unwrap();
        let baseline = assemble(Condition::Baseline, dir.path(), "roster status", 4000).unwrap();
        assert!(
            crushed.tokens < baseline.tokens,
            "tabular crush {} must beat baseline {}",
            crushed.tokens,
            baseline.tokens
        );

        let again = assemble(Condition::TabularCrush, dir.path(), "roster status", 4000).unwrap();
        assert_eq!(
            crushed.digest, again.digest,
            "tabular_crush assembly is deterministic"
        );
    }

    #[test]
    fn yaml_crush_condition_beats_baseline_and_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let mut yaml = String::from("items:\n");
        for i in 0..40 {
            yaml.push_str(&format!(
                "  - apiVersion: v1\n    kind: Pod\n    namespace: prod\n    name: pod-{i}\n"
            ));
        }
        fs::write(dir.path().join("pods.yaml"), yaml).unwrap();

        let crushed = assemble(Condition::YamlCrush, dir.path(), "pod namespace", 4000).unwrap();
        let baseline = assemble(Condition::Baseline, dir.path(), "pod namespace", 4000).unwrap();
        assert!(
            crushed.tokens < baseline.tokens,
            "yaml crush {} must beat baseline {}",
            crushed.tokens,
            baseline.tokens
        );

        let again = assemble(Condition::YamlCrush, dir.path(), "pod namespace", 4000).unwrap();
        assert_eq!(
            crushed.digest, again.digest,
            "yaml_crush assembly is deterministic"
        );
    }

    #[test]
    fn budget_is_respected() {
        let ws = workspace();
        let ctx = assemble(Condition::Baseline, ws.path(), "x", 12).unwrap();
        assert!(ctx.tokens <= 12, "got {} tokens", ctx.tokens);
    }

    #[test]
    fn truncate_caps_tokens() {
        let long = "word ".repeat(5000);
        let capped = truncate_to_tokens(&long, 50);
        assert!(count_tokens(&capped) <= 50);
    }
}
