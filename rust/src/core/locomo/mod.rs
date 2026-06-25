//! `LoCoMo` memory benchmark (#291).
//!
//! Long-conversation memory, measured the way lean-ctx works: store every turn as
//! a memory, then for each question recall the top-k memories and score the
//! recalled context against the gold answers (token-F1 / exact-match / answer
//! containment), plus the token cost of the recalled context versus dumping the
//! whole transcript.
//!
//! Pipeline: [`dataset`] (load) → [`runner`] (ingest + recall + score) →
//! [`report`] (aggregate to publishable numbers). Run via the `locomo_bench`
//! example: `cargo run --example locomo_bench --features dev-tools`.

pub mod dataset;
pub mod report;
pub mod runner;

use std::path::Path;

use dataset::LocomoSample;
use report::LocomoReport;

/// Run a suite end-to-end and aggregate a report.
///
/// `workspace` should be a fresh temp dir (per-sample subdirs are created under
/// it); the caller must also point `LEAN_CTX_DATA_DIR` at a throwaway dir so the
/// benchmark never touches real project knowledge.
#[must_use]
pub fn run(
    suite_name: &str,
    samples: &[LocomoSample],
    workspace: &Path,
    top_k: usize,
) -> LocomoReport {
    let results = runner::run_suite(samples, workspace, top_k);
    report::aggregate(suite_name, top_k, &results)
}
