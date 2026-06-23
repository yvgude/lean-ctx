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
}

impl Condition {
    /// Stable label used in reports + the determinism digest.
    pub fn label(self) -> &'static str {
        match self {
            Condition::Baseline => "baseline",
            Condition::LeanCtx => "lean_ctx",
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
    let index = crate::core::index_orchestrator::load_or_build_bm25(workspace);
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
        let compressed = aggressive_compress(&content, ext);
        out.push((rel_label(workspace, &path), compressed));
    }
    // If retrieval found nothing (e.g. empty index), fall back to the baseline ordering so the
    // treatment condition is never empty by accident.
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
fn rel_label(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
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
