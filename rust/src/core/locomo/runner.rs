//! Run a `LoCoMo` sample through lean-ctx memory: ingest every turn as a knowledge
//! fact, then for each question recall the top-k memories and score the recalled
//! context against the gold answers (#291).
//!
//! This measures what lean-ctx actually does — *retrieval recall*: did the
//! answer-bearing turn get surfaced, and at what token cost versus dumping the
//! whole transcript. It is deliberately model-free, so results are deterministic.

use std::path::Path;

use crate::core::eval_ab::scorers::{qa_contains, qa_exact_match, qa_f1};
use crate::core::knowledge::ProjectKnowledge;
use crate::core::memory_policy::MemoryPolicy;
use crate::core::tokens::count_tokens;

use super::dataset::LocomoSample;

/// Outcome of scoring a single question.
#[derive(Debug, Clone)]
pub struct QaResult {
    pub category: u8,
    pub f1: f64,
    pub exact_match: bool,
    /// A gold answer is a substring of the recalled context (the key recall signal).
    pub contained: bool,
    pub recall_tokens: usize,
}

/// Outcome of one sample.
#[derive(Debug, Clone)]
pub struct SampleResult {
    pub id: String,
    pub qa: Vec<QaResult>,
    pub transcript_tokens: usize,
}

/// Ingest a sample's turns into an isolated knowledge store rooted at
/// `project_root`, then recall + score each question with `top_k` memories.
///
/// `project_root` must be unique per sample so the knowledge hashes don't collide.
/// The caller is responsible for pointing `LEAN_CTX_DATA_DIR` at a throwaway dir.
#[must_use]
pub fn run_sample(sample: &LocomoSample, project_root: &Path, top_k: usize) -> SampleResult {
    let root = project_root.to_string_lossy().to_string();
    let policy = MemoryPolicy::default();

    // 1. Ingest every turn as a memory under a fresh store.
    let _ = ProjectKnowledge::mutate_locked(&root, |k| {
        for (si, session) in sample.sessions.iter().enumerate() {
            let session_id = if session.session_id.is_empty() {
                format!("s{si}")
            } else {
                session.session_id.clone()
            };
            for (ti, turn) in session.turns.iter().enumerate() {
                let key = format!("{}-{}-{}", sample.id, session_id, ti);
                let value = format!("{}: {}", turn.speaker, turn.text);
                k.remember("conversation", &key, &value, &session_id, 0.9, &policy);
            }
        }
    });

    let transcript_tokens = count_tokens(&sample.transcript());

    // 2. Recall + score each question against the production recall path.
    let mut knowledge = ProjectKnowledge::load_or_create(&root);
    let mut qa = Vec::with_capacity(sample.qa.len());
    for item in &sample.qa {
        let (hits, _) = knowledge.recall_for_output(&item.question, top_k);
        let context = hits
            .iter()
            .map(|f| f.value.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let recall_tokens = count_tokens(&context);

        // Containment is measured over the whole recalled context (did any top-k
        // memory carry the answer = retrieval recall). F1 / EM are measured against
        // the *best single* recalled memory, so a short gold answer isn't penalised
        // for the size of the surrounding context.
        let mut best_f1 = 0.0f64;
        let mut em = false;
        let mut contained = false;
        for gold in &item.answers {
            contained |= qa_contains(&context, gold);
            for hit in &hits {
                best_f1 = best_f1.max(qa_f1(&hit.value, gold));
                em |= qa_exact_match(&hit.value, gold);
            }
        }
        qa.push(QaResult {
            category: item.category,
            f1: best_f1,
            exact_match: em,
            contained,
            recall_tokens,
        });
    }

    SampleResult {
        id: sample.id.clone(),
        qa,
        transcript_tokens,
    }
}

/// Run every sample under per-sample subdirectories of `workspace`.
#[must_use]
pub fn run_suite(samples: &[LocomoSample], workspace: &Path, top_k: usize) -> Vec<SampleResult> {
    samples
        .iter()
        .enumerate()
        .map(|(i, sample)| {
            let proj = workspace.join(format!("sample-{i}"));
            let _ = std::fs::create_dir_all(&proj);
            run_sample(sample, &proj, top_k)
        })
        .collect()
}
