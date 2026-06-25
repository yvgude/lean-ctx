//! Personalized `PageRank` ranking for repo map symbols.
//!
//! Runs `PageRank` on the file-level graph with personalization biased
//! toward session-relevant and user-specified focus files.

use crate::core::pagerank::{self, PageRankInput};
use crate::core::repomap::graph::{RepoGraph, SymbolDef};

/// A symbol with its computed importance score.
#[derive(Debug, Clone)]
pub struct RankedSymbol {
    pub def: SymbolDef,
    pub score: f64,
}

/// Rank all symbols in the repo graph using Personalized `PageRank`.
///
/// - `session_files`: files from the active session (get highest boost)
/// - `focus_files`: user-specified files to emphasize
///
/// Returns symbols sorted by descending score.
#[must_use]
pub fn rank_symbols(
    graph: &RepoGraph,
    session_files: &[String],
    focus_files: &[String],
) -> Vec<RankedSymbol> {
    let input = PageRankInput {
        files: graph.files.clone(),
        forward: graph.forward.clone(),
    };

    let seed_files = build_seed_files(session_files, focus_files, &graph.files);
    let file_ranks = pagerank::compute_personalized(&input, 0.85, 30, &seed_files);

    let mut ranked: Vec<RankedSymbol> = Vec::new();

    for (file, symbols) in &graph.symbols_by_file {
        let file_score = file_ranks.get(file).copied().unwrap_or(0.0);

        for sym in symbols {
            // Exported symbols get a 2x boost within their file
            let export_boost = if sym.is_exported { 2.0 } else { 1.0 };
            let score = file_score * export_boost;

            ranked.push(RankedSymbol {
                def: sym.clone(),
                score,
            });
        }
    }

    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    ranked
}

/// Build the personalization seed from session and focus files.
///
/// Combines both sources into a single seed list (deduplicated).
/// Session files are added first so they appear in the set even
/// if there is overlap with focus files.
fn build_seed_files(
    session_files: &[String],
    focus_files: &[String],
    valid_files: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut seeds: Vec<String> = Vec::new();

    for f in session_files.iter().chain(focus_files.iter()) {
        if valid_files.contains(f) && seen.insert(f.clone()) {
            seeds.push(f.clone());
        }
    }

    seeds
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::repomap::graph::SymbolDef;
    use std::collections::{HashMap, HashSet};

    fn test_graph() -> RepoGraph {
        let mut files = HashSet::new();
        files.insert("a.rs".into());
        files.insert("b.rs".into());
        files.insert("c.rs".into());

        let mut forward = HashMap::new();
        // a imports b, b imports c → c should rank highest
        forward.insert("a.rs".into(), vec!["b.rs".into()]);
        forward.insert("b.rs".into(), vec!["c.rs".into()]);

        let mut symbols_by_file = HashMap::new();
        symbols_by_file.insert("a.rs".into(), vec![test_sym("main", "fn", "a.rs", false)]);
        symbols_by_file.insert("b.rs".into(), vec![test_sym("process", "fn", "b.rs", true)]);
        symbols_by_file.insert(
            "c.rs".into(),
            vec![
                test_sym("Config", "struct", "c.rs", true),
                test_sym("helper", "fn", "c.rs", false),
            ],
        );

        RepoGraph {
            files,
            forward,
            symbols_by_file,
        }
    }

    fn test_sym(name: &str, kind: &str, file: &str, exported: bool) -> SymbolDef {
        SymbolDef {
            name: name.into(),
            kind: kind.into(),
            file: file.into(),
            line: 1,
            end_line: 10,
            is_exported: exported,
            signature: format!("{kind} {name}"),
        }
    }

    #[test]
    fn most_depended_file_ranks_highest() {
        let graph = test_graph();
        let ranked = rank_symbols(&graph, &[], &[]);

        let c_scores: Vec<f64> = ranked
            .iter()
            .filter(|r| r.def.file == "c.rs")
            .map(|r| r.score)
            .collect();
        let a_scores: Vec<f64> = ranked
            .iter()
            .filter(|r| r.def.file == "a.rs")
            .map(|r| r.score)
            .collect();

        let max_c = c_scores.iter().copied().fold(0.0_f64, f64::max);
        let max_a = a_scores.iter().copied().fold(0.0_f64, f64::max);

        assert!(
            max_c > max_a,
            "c.rs (most deps) should rank higher: c={max_c} a={max_a}"
        );
    }

    #[test]
    fn exported_symbols_get_boost() {
        let graph = test_graph();
        let ranked = rank_symbols(&graph, &[], &[]);

        let config = ranked.iter().find(|r| r.def.name == "Config").unwrap();
        let helper = ranked.iter().find(|r| r.def.name == "helper").unwrap();

        assert!(
            config.score > helper.score,
            "exported Config should rank higher than non-exported helper in same file"
        );
    }

    #[test]
    fn session_files_get_boosted() {
        let graph = test_graph();

        let no_seed = rank_symbols(&graph, &[], &[]);
        let with_seed = rank_symbols(&graph, &["a.rs".into()], &[]);

        let a_no_seed = no_seed.iter().find(|r| r.def.name == "main").unwrap().score;
        let a_with_seed = with_seed
            .iter()
            .find(|r| r.def.name == "main")
            .unwrap()
            .score;

        assert!(
            a_with_seed > a_no_seed,
            "session-seeded a.rs should rank higher: {a_with_seed} vs {a_no_seed}"
        );
    }

    #[test]
    fn empty_graph_returns_empty() {
        let graph = RepoGraph {
            files: HashSet::new(),
            forward: HashMap::new(),
            symbols_by_file: HashMap::new(),
        };
        let ranked = rank_symbols(&graph, &[], &[]);
        assert!(ranked.is_empty());
    }

    #[test]
    fn build_seed_filters_invalid_files() {
        let mut valid = HashSet::new();
        valid.insert("a.rs".into());

        let seeds = build_seed_files(
            &["a.rs".into(), "nonexistent.rs".into()],
            &["also_missing.rs".into()],
            &valid,
        );

        assert_eq!(seeds.len(), 1, "only valid a.rs should remain");
        assert_eq!(seeds[0], "a.rs");
    }
}
