//! Per-language semantic coverage: how many of a language's file-level call
//! edges are verified, and whether a language server for it can run here.
//! Shared by `ctx_graph status`, `lean-ctx doctor` and the dashboard legend.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::core::property_graph::{CodeGraph, EdgeKind};

use super::evidence::{EdgeEvidence, EvidenceGrade};

/// Evidence counts of one language's file-level `calls` edges.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct GradeCounts {
    pub verified: usize,
    pub resolved: usize,
    pub heuristic: usize,
    /// Edges without typed evidence (older graphs, other producers).
    pub unannotated: usize,
}

impl GradeCounts {
    pub fn total(&self) -> usize {
        self.verified + self.resolved + self.heuristic + self.unannotated
    }

    /// Verified share in whole percent (0 when there are no edges).
    pub fn verified_percent(&self) -> usize {
        (self.verified * 100).checked_div(self.total()).unwrap_or(0)
    }
}

/// `calls` edge evidence grouped by the caller file's language (the
/// `LanguageId` spelling: `rust`, `typescript`, …), sorted by language.
pub fn coverage_by_language(graph: &CodeGraph) -> BTreeMap<&'static str, GradeCounts> {
    let mut out: BTreeMap<&'static str, GradeCounts> = BTreeMap::new();
    for (source, _, metadata) in graph
        .file_edges_of_kind(&EdgeKind::Calls)
        .unwrap_or_default()
    {
        let Some(lang) = crate::core::language_capabilities::language_for_path(&source) else {
            continue;
        };
        let counts = out.entry(lang.id_str()).or_default();
        match EdgeEvidence::from_metadata(metadata.as_deref()).map(|e| e.grade) {
            Some(EvidenceGrade::VerifiedSemantic) => counts.verified += 1,
            Some(EvidenceGrade::ResolvedStructural) => counts.resolved += 1,
            Some(EvidenceGrade::HeuristicStructural) => counts.heuristic += 1,
            None => counts.unannotated += 1,
        }
    }
    out
}

/// Whether lean-ctx can start a standalone language server for a language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServerStatus {
    /// Server binary lean-ctx would start (`rust-analyzer`, `gopls`, …).
    pub binary: String,
    /// The binary exists *and* can run (a rustup proxy without the installed
    /// component does not count).
    pub runnable: bool,
    /// How to install it, when it cannot run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_hint: Option<&'static str>,
}

/// Standalone-server status for a language (`LanguageId` spelling) in
/// `project_root` — the server the router would start there (a project's own
/// TypeScript counts); `None` when lean-ctx knows no standalone server for it
/// (a JetBrains IDE may still serve it).
pub fn server_status(language: &str, project_root: &str) -> Option<ServerStatus> {
    let default = crate::lsp::config::default_servers()
        .remove(language)?
        .command;
    let resolved = resolved_binary_cached(language, project_root);
    // JavaScript is served by the TypeScript server.
    let hint_lang = if language == "javascript" {
        "typescript"
    } else {
        language
    };
    Some(ServerStatus {
        install_hint: resolved
            .is_none()
            .then(|| crate::lsp::config::install_hint_for_language(hint_lang)),
        runnable: resolved.is_some(),
        binary: resolved.unwrap_or(default),
    })
}

/// The server binary for `language` in `project_root`, if one can run.
/// Resolving may spawn `<proxy> --version`; status surfaces (dashboard
/// polling) ask repeatedly, so answers are kept for a minute.
fn resolved_binary_cached(language: &str, project_root: &str) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex, PoisonError};
    use std::time::{Duration, Instant};
    type Key = (String, String);
    static CACHE: LazyLock<Mutex<HashMap<Key, (Option<String>, Instant)>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    const TTL: Duration = Duration::from_mins(1);

    let key = (language.to_string(), project_root.to_string());
    if let Some((binary, at)) = CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
        && at.elapsed() < TTL
    {
        return binary.clone();
    }
    // Exactly what the router would start (a configured `[lsp]` path counts).
    let binary = crate::lsp::router::standalone_server(language, project_root)
        .ok()
        .map(|s| s.binary_name());
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    cache.retain(|_, (_, at)| at.elapsed() < TTL);
    cache.insert(key, (binary.clone(), Instant::now()));
    binary
}

/// One line per language for terminal output, e.g.
/// `  rust        96/120 verified (80%) · rust-analyzer ✓`, from
/// [`coverage_by_language`]. Sorted by language; deterministic.
pub fn coverage_lines(
    coverage: &BTreeMap<&'static str, GradeCounts>,
    project_root: &str,
) -> Vec<String> {
    coverage
        .iter()
        .map(|(lang, c)| {
            let server = match server_status(lang, project_root) {
                Some(s) if s.runnable => format!("{} ✓", s.binary),
                Some(s) => format!(
                    "{} not installed ({})",
                    s.binary,
                    s.install_hint.unwrap_or("see docs")
                ),
                None => "no standalone server (JetBrains IDE only)".to_string(),
            };
            format!(
                "  {lang:<11} {}/{} verified ({}%) · {server}",
                c.verified,
                c.total(),
                c.verified_percent()
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property_graph::Node;
    use crate::core::semantic::EvidenceOrigin;

    #[test]
    #[allow(clippy::many_single_char_names)] // graph test nodes
    fn calls_are_counted_per_caller_language_and_grade() {
        let g = CodeGraph::open_in_memory().unwrap();
        let id = |p: &str| g.upsert_node(&Node::file(p)).unwrap();
        let (a, b, t, u) = (
            id("src/a.rs"),
            id("src/b.rs"),
            id("web/t.ts"),
            id("web/u.ts"),
        );
        let ev = |grade| EdgeEvidence::new(grade, EvidenceOrigin::Enrichment, None, 1);
        g.upsert_edge_with_evidence(a, b, &EdgeKind::Calls, &ev(EvidenceGrade::VerifiedSemantic))
            .unwrap();
        g.upsert_edge_with_evidence(
            b,
            a,
            &EdgeKind::Calls,
            &ev(EvidenceGrade::HeuristicStructural),
        )
        .unwrap();
        g.upsert_edge_with_evidence(
            t,
            u,
            &EdgeKind::Calls,
            &ev(EvidenceGrade::ResolvedStructural),
        )
        .unwrap();

        let cov = coverage_by_language(&g);
        let rust = cov["rust"];
        assert_eq!((rust.verified, rust.heuristic, rust.total()), (1, 1, 2));
        assert_eq!(rust.verified_percent(), 50);
        assert_eq!(cov["typescript"].resolved, 1);
        assert_eq!(
            GradeCounts::default().verified_percent(),
            0,
            "no division by zero"
        );
    }
}
