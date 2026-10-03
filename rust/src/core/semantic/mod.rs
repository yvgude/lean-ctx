//! Semantic code intelligence on top of the tree-sitter structure.
//!
//! Tree-sitter is the always-available baseline: it extracts symbols, imports
//! and call sites, and resolves callees within the caller's scope. Where that
//! is not enough — an ambiguous name, or a name only unique project-wide —
//! a local semantic backend (language server or live JetBrains IDE, via
//! [`crate::lsp::router`]) is asked where the call actually goes. Its answer
//! becomes [`EdgeEvidence`] on the property-graph edge, which ranking and
//! impact analysis weight by grade.
//!
//! Guarantees: local only (no network, no source leaves the machine); no
//! automatic installs; `semantic_mode = "auto"` never starts a server in the
//! background; every target is confined to the indexed project; without any
//! backend the graph is exactly the structural graph.

pub mod coverage;
pub mod enrich;
pub mod evidence;
pub mod implementations;
pub mod resolve;

#[cfg(test)]
mod e2e_tests;

pub use enrich::{Escalation, EscalationBudget, EscalationStats, SemanticVerdict, escalate_calls};
pub use evidence::{EdgeEvidence, EvidenceGrade, EvidenceOrigin};

use crate::core::config::SemanticMode;
use crate::core::property_graph::{CodeGraph, EdgeKind};

/// One-line evidence summary of the file-level graph, e.g.
/// `Semantic: mode=auto | calls 120 verified · 340 resolved · 45 heuristic | implements 12`.
/// Edges without typed evidence (older graphs, other producers) are counted
/// as `unannotated` only when present.
pub fn status_line(graph: &CodeGraph, mode: SemanticMode, project_root: &str) -> String {
    let mode = match mode {
        SemanticMode::Off => "off",
        SemanticMode::Auto => "auto",
        SemanticMode::Eager => "eager",
    };
    let coverage = coverage::coverage_by_language(graph);
    let mut all = coverage::GradeCounts::default();
    for c in coverage.values() {
        all.verified += c.verified;
        all.resolved += c.resolved;
        all.heuristic += c.heuristic;
        all.unannotated += c.unannotated;
    }
    let implements = graph
        .file_edges_of_kind(&EdgeKind::Implements)
        .map_or(0, |e| e.len());
    let extra = if all.unannotated > 0 {
        format!(" · {} unannotated", all.unannotated)
    } else {
        String::new()
    };
    let mut out = format!(
        "Semantic: mode={mode} | calls {} verified · {} resolved · {} heuristic{extra} | implements {implements}",
        all.verified, all.resolved, all.heuristic
    );
    for line in coverage::coverage_lines(&coverage, project_root) {
        out.push('\n');
        out.push_str(&line);
    }
    out
}
