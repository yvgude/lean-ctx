//! Shadow-mode parity harness (#682.3): prove the `PropertyGraph` reproduces
//! everything `graph_index` exposes through the [`GraphProvider`] facade —
//! symbols, edges and structural dependencies — *before* any backend flip
//! (#682.4) relies on PG.
//!
//! The mirror ([`populate_from_project_index`]) sources PG from the very
//! `ProjectIndex` that `graph_index` produces, so equivalence is expected by
//! construction. This harness is the executable proof and the regression gate
//! that it stays so: it asserts PG loses nothing `graph_index` exposed (exact
//! counts + inventory + symbol lookups, and a structural-edge / dependency
//! superset — PG may legitimately expose *more*, never less).

use std::collections::HashSet;

use crate::core::graph_index::ProjectIndex;
use crate::core::graph_provider::GraphProvider;
use crate::core::property_graph::{CodeGraph, populate_from_project_index};

/// Cap on stored human-readable divergences so a pathological index cannot
/// balloon the report; counts remain exact regardless.
const MAX_DIVERGENCES: usize = 20;

/// Quantified comparison of `PropertyGraph` vs `graph_index` facade output.
#[derive(Debug, Default, Clone)]
pub struct ParityReport {
    pub files: usize,
    pub symbol_count_gi: usize,
    pub symbol_count_pg: usize,
    pub edge_count_gi: usize,
    pub edge_count_pg: usize,
    pub file_inventory_equal: bool,
    pub files_checked: usize,
    /// Files where `gi.dependencies ⊆ pg.dependencies` (no loss).
    pub dependencies_lossless: usize,
    /// Files where `gi.dependents ⊆ pg.dependents` (no loss).
    pub dependents_lossless: usize,
    /// Extra dependency edges PG exposes beyond `graph_index` (enrichment, e.g.
    /// re-export / sibling / co-change edges the import-only GI facade omits).
    pub dependencies_extra: usize,
    pub symbols_checked: usize,
    pub symbols_matched: usize,
    /// `gi (from,to)` structural edge pairs are all present in PG.
    pub edge_pairs_lossless: bool,
    pub divergences: Vec<String>,
}

impl ParityReport {
    /// True when PG loses nothing `graph_index` exposed: exact counts, identical
    /// file inventory, no dependency/dependent loss, every sampled symbol
    /// matched, and the structural-edge set is a superset.
    #[must_use]
    pub fn is_lossless(&self) -> bool {
        self.symbol_count_pg == self.symbol_count_gi
            && self.edge_count_pg == self.edge_count_gi
            && self.file_inventory_equal
            && self.dependencies_lossless == self.files_checked
            && self.dependents_lossless == self.files_checked
            && self.symbols_matched == self.symbols_checked
            && self.edge_pairs_lossless
    }

    fn note(&mut self, msg: String) {
        if self.divergences.len() < MAX_DIVERGENCES {
            self.divergences.push(msg);
        }
    }
}

/// Build an in-memory `PropertyGraph` from `index` and compare it, through the
/// shared [`GraphProvider`] facade, against the same index served as
/// `graph_index`. Pure in-memory — no disk, no rescan.
pub fn compare(index: &ProjectIndex) -> anyhow::Result<ParityReport> {
    let pg = CodeGraph::open_in_memory()?;
    populate_from_project_index(&pg, index)?;
    let pgp = GraphProvider::PropertyGraph(pg);
    let gip = GraphProvider::GraphIndex(index.clone());

    let mut r = ParityReport {
        files: index.files.len(),
        symbol_count_gi: gip.symbol_count(),
        symbol_count_pg: pgp.symbol_count(),
        edge_count_gi: gip.edge_count().unwrap_or(0),
        edge_count_pg: pgp.edge_count().unwrap_or(0),
        ..Default::default()
    };

    r.file_inventory_equal = pgp.file_paths() == gip.file_paths();
    if !r.file_inventory_equal {
        r.note("file inventory differs".to_string());
    }
    if r.symbol_count_pg != r.symbol_count_gi {
        r.note(format!(
            "symbol count: gi={} pg={}",
            r.symbol_count_gi, r.symbol_count_pg
        ));
    }
    if r.edge_count_pg != r.edge_count_gi {
        r.note(format!(
            "edge count: gi={} pg={}",
            r.edge_count_gi, r.edge_count_pg
        ));
    }

    for path in gip.file_paths() {
        r.files_checked += 1;

        let gi_dep: HashSet<String> = gip.dependencies(&path).into_iter().collect();
        let pg_dep: HashSet<String> = pgp.dependencies(&path).into_iter().collect();
        if gi_dep.is_subset(&pg_dep) {
            r.dependencies_lossless += 1;
        } else {
            let missing: Vec<_> = gi_dep.difference(&pg_dep).cloned().collect();
            r.note(format!("deps lost for {path}: {missing:?}"));
        }
        r.dependencies_extra += pg_dep.difference(&gi_dep).count();

        let gi_rdep: HashSet<String> = gip.dependents(&path).into_iter().collect();
        let pg_rdep: HashSet<String> = pgp.dependents(&path).into_iter().collect();
        if gi_rdep.is_subset(&pg_rdep) {
            r.dependents_lossless += 1;
        } else {
            let missing: Vec<_> = gi_rdep.difference(&pg_rdep).cloned().collect();
            r.note(format!("dependents lost for {path}: {missing:?}"));
        }
    }

    for (key, sym) in &index.symbols {
        r.symbols_checked += 1;
        match pgp.get_symbol(key) {
            Some(pg_sym)
                if pg_sym.name == sym.name
                    && pg_sym.file == sym.file
                    && pg_sym.start_line == sym.start_line
                    && pg_sym.end_line == sym.end_line =>
            {
                r.symbols_matched += 1;
            }
            _ => r.note(format!("symbol mismatch: {key}")),
        }
    }

    let pg_pairs: HashSet<(String, String)> =
        pgp.edges().into_iter().map(|e| (e.from, e.to)).collect();
    let gi_pairs: HashSet<(String, String)> =
        gip.edges().into_iter().map(|e| (e.from, e.to)).collect();
    r.edge_pairs_lossless = gi_pairs.is_subset(&pg_pairs);
    if !r.edge_pairs_lossless {
        r.note("structural edge (from,to) set is not a superset".to_string());
    }

    Ok(r)
}

/// Render a [`ParityReport`] as a compact, deterministic text block.
#[must_use]
pub fn format_report(r: &ParityReport) -> String {
    let verdict = if r.is_lossless() {
        "LOSSLESS — PropertyGraph reproduces graph_index (safe to flip)"
    } else {
        "DIVERGENT — see divergences below (NOT safe to flip)"
    };
    let mut out = format!(
        "Shadow parity (PropertyGraph vs graph_index)\n\
         Verdict: {verdict}\n\
         Files: {files}\n\
         Symbols: gi={sgi} pg={spg} ({sm}/{sc} matched)\n\
         Edges: gi={egi} pg={epg} (superset={eps})\n\
         Dependencies: {dl}/{fc} lossless (+{dx} enrichment edges)\n\
         Dependents:   {rl}/{fc} lossless",
        verdict = verdict,
        files = r.files,
        sgi = r.symbol_count_gi,
        spg = r.symbol_count_pg,
        sm = r.symbols_matched,
        sc = r.symbols_checked,
        egi = r.edge_count_gi,
        epg = r.edge_count_pg,
        eps = r.edge_pairs_lossless,
        dl = r.dependencies_lossless,
        rl = r.dependents_lossless,
        fc = r.files_checked,
        dx = r.dependencies_extra,
    );
    if !r.divergences.is_empty() {
        out.push_str("\nDivergences:");
        for d in &r.divergences {
            out.push_str(&format!("\n  - {d}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph_index::{FileEntry, IndexEdge, SymbolEntry};

    fn fe(path: &str) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            hash: "h".to_string(),
            language: "rs".to_string(),
            line_count: 1,
            token_count: 1,
            exports: vec![],
            summary: String::new(),
        }
    }

    fn sym(file: &str, name: &str, a: usize, b: usize) -> (String, SymbolEntry) {
        (
            format!("{file}::{name}"),
            SymbolEntry {
                file: file.to_string(),
                name: name.to_string(),
                kind: "function".to_string(),
                start_line: a,
                end_line: b,
                is_exported: true,
                minhash: Vec::new(),
            },
        )
    }

    fn edge(from: &str, to: &str, kind: &str) -> IndexEdge {
        IndexEdge {
            from: from.to_string(),
            to: to.to_string(),
            kind: kind.to_string(),
            weight: 1.0,
        }
    }

    fn index_with(edges: Vec<IndexEdge>) -> ProjectIndex {
        let mut idx = ProjectIndex::new("/t");
        for f in ["a.rs", "b.rs", "c.rs"] {
            idx.files.insert(f.to_string(), fe(f));
        }
        let (k1, s1) = sym("a.rs", "run", 1, 4);
        let (k2, s2) = sym("b.rs", "Helper", 2, 8);
        idx.symbols.insert(k1, s1);
        idx.symbols.insert(k2, s2);
        idx.edges = edges;
        idx
    }

    #[test]
    fn import_only_index_is_lossless() {
        let idx = index_with(vec![
            edge("a.rs", "b.rs", "import"),
            edge("a.rs", "c.rs", "import"),
        ]);
        let r = compare(&idx).unwrap();
        assert!(
            r.is_lossless(),
            "import-only mirror must be lossless: {r:?}"
        );
        assert_eq!(r.symbol_count_pg, 2);
        assert_eq!(r.dependencies_extra, 0, "no enrichment for pure imports");
    }

    #[test]
    fn reexport_and_sibling_are_lossless_with_enrichment() {
        // GI's import-only dependencies() ignores reexport/sibling; PG exposes
        // them as structural edges. That is *more*, never less — still lossless.
        let idx = index_with(vec![
            edge("a.rs", "b.rs", "import"),
            edge("a.rs", "c.rs", "reexport"),
            edge("b.rs", "c.rs", "sibling"),
        ]);
        let r = compare(&idx).unwrap();
        assert!(r.is_lossless(), "superset must still be lossless: {r:?}");
        assert!(
            r.dependencies_extra >= 1,
            "PG exposes the extra structural edges"
        );
        assert!(r.edge_pairs_lossless);
    }

    #[test]
    fn empty_index_is_trivially_lossless() {
        let idx = ProjectIndex::new("/t");
        let r = compare(&idx).unwrap();
        assert!(r.is_lossless());
        assert_eq!(r.files, 0);
    }

    #[test]
    fn trait_impl_symbol_name_with_colons_roundtrips() {
        // Trait-impl symbol names contain `::` (e.g. `std::fmt::Display for T`).
        // The symbol key is `file::name`, so the PG lookup must split on the
        // FIRST `::`, not the last — `rsplitn` put the `::`-tail of the name on
        // the file side and these symbols silently failed to match (#682.3).
        let mut idx = ProjectIndex::new("/t");
        idx.files.insert("a.rs".to_string(), fe("a.rs"));
        let (k, s) = sym("a.rs", "std::fmt::Display for ProfileSource", 10, 20);
        idx.symbols.insert(k, s);
        let r = compare(&idx).unwrap();
        assert_eq!(
            r.symbols_matched, r.symbols_checked,
            "trait-impl symbol name with `::` must round-trip: {r:?}"
        );
        assert!(r.is_lossless(), "{r:?}");
    }
}
