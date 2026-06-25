//! Aggregate per-question results into publishable `LoCoMo` metrics (#291).

use serde::{Deserialize, Serialize};

use super::runner::SampleResult;

/// Aggregated metrics for one slice of questions (a category, or `category = 0`
/// for the overall row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryMetrics {
    /// `LoCoMo` category, or 0 for "overall".
    pub category: u8,
    pub label: String,
    pub questions: usize,
    /// Fraction of questions whose gold answer was contained in recalled context.
    pub containment_rate: f64,
    pub mean_f1: f64,
    pub exact_match_rate: f64,
    pub mean_recall_tokens: f64,
}

/// A complete, committable benchmark report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocomoReport {
    pub suite: String,
    pub generated_at: String,
    pub top_k: usize,
    pub samples: usize,
    pub questions: usize,
    pub overall: CategoryMetrics,
    pub by_category: Vec<CategoryMetrics>,
    pub mean_transcript_tokens: f64,
    pub mean_recall_tokens: f64,
    /// Token reduction of recalled context vs. dumping the full transcript.
    pub token_reduction_pct: f64,
}

fn category_label(category: u8) -> &'static str {
    match category {
        0 => "overall",
        1 => "single-hop",
        2 => "multi-hop",
        3 => "temporal",
        4 => "open-domain",
        5 => "adversarial",
        _ => "other",
    }
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let mut n = 0usize;
    let mut sum = 0.0;
    for v in values {
        sum += v;
        n += 1;
    }
    if n == 0 { 0.0 } else { sum / n as f64 }
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn metrics_for(category: u8, qa: &[&super::runner::QaResult]) -> CategoryMetrics {
    let questions = qa.len();
    CategoryMetrics {
        category,
        label: category_label(category).to_string(),
        questions,
        containment_rate: round3(mean(qa.iter().map(|q| f64::from(u8::from(q.contained))))),
        mean_f1: round3(mean(qa.iter().map(|q| q.f1))),
        exact_match_rate: round3(mean(qa.iter().map(|q| f64::from(u8::from(q.exact_match))))),
        mean_recall_tokens: round3(mean(qa.iter().map(|q| q.recall_tokens as f64))),
    }
}

/// Aggregate sample results into a report.
#[must_use]
pub fn aggregate(suite: &str, top_k: usize, results: &[SampleResult]) -> LocomoReport {
    let all: Vec<&super::runner::QaResult> = results.iter().flat_map(|r| r.qa.iter()).collect();
    let overall = metrics_for(0, &all);

    let mut categories: Vec<u8> = all.iter().map(|q| q.category).collect();
    categories.sort_unstable();
    categories.dedup();
    let by_category: Vec<CategoryMetrics> = categories
        .into_iter()
        .map(|cat| {
            let slice: Vec<&super::runner::QaResult> =
                all.iter().copied().filter(|q| q.category == cat).collect();
            metrics_for(cat, &slice)
        })
        .collect();

    let mean_transcript_tokens = round3(mean(
        results
            .iter()
            .flat_map(|r| r.qa.iter().map(|_| r.transcript_tokens as f64)),
    ));
    let mean_recall_tokens = overall.mean_recall_tokens;
    let token_reduction_pct = if mean_transcript_tokens > 0.0 {
        round3((1.0 - mean_recall_tokens / mean_transcript_tokens) * 100.0)
    } else {
        0.0
    };

    LocomoReport {
        suite: suite.to_string(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        top_k,
        samples: results.len(),
        questions: all.len(),
        overall,
        by_category,
        mean_transcript_tokens,
        mean_recall_tokens,
        token_reduction_pct,
    }
}

impl LocomoReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Human/publishable Markdown summary.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# LoCoMo Memory Benchmark — lean-ctx\n\n");
        out.push_str(&format!(
            "Suite: `{}` · samples: {} · questions: {} · top_k: {}\n\n",
            self.suite, self.samples, self.questions, self.top_k
        ));
        out.push_str("Retrieval-recall benchmark: each conversation turn is stored as a memory, then for every question the top-k memories are recalled and scored against the gold answers. Model-free and deterministic.\n\n");
        out.push_str("## Overall\n\n");
        out.push_str("| metric | value |\n|---|---|\n");
        out.push_str(&format!(
            "| answer containment (recall@{}) | {:.1}% |\n",
            self.top_k,
            self.overall.containment_rate * 100.0
        ));
        out.push_str(&format!(
            "| mean best-memory token-F1 | {:.3} |\n",
            self.overall.mean_f1
        ));
        out.push_str(&format!(
            "| exact-match rate | {:.1}% |\n",
            self.overall.exact_match_rate * 100.0
        ));
        out.push_str(&format!(
            "| mean recalled-context tokens | {:.0} |\n",
            self.mean_recall_tokens
        ));
        out.push_str(&format!(
            "| mean full-transcript tokens | {:.0} |\n",
            self.mean_transcript_tokens
        ));
        out.push_str(&format!(
            "| token reduction vs. full transcript | {:.1}% |\n\n",
            self.token_reduction_pct
        ));

        out.push_str("## By category\n\n");
        out.push_str("| category | questions | containment | mean F1 | recall tokens |\n");
        out.push_str("|---|---|---|---|---|\n");
        for c in &self.by_category {
            out.push_str(&format!(
                "| {} | {} | {:.1}% | {:.3} | {:.0} |\n",
                c.label,
                c.questions,
                c.containment_rate * 100.0,
                c.mean_f1,
                c.mean_recall_tokens
            ));
        }
        out.push('\n');
        out.push_str(&format!("_Generated {}._\n", self.generated_at));
        out
    }
}
