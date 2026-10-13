// SPDX-License-Identifier: Apache-2.0
//! Docstring → code retrieval eval for embedding models (CodeSearchNet style).
//!
//! Every documented Rust function under `LEAN_CTX_EMBED_EVAL_ROOT` becomes a
//! corpus chunk — its signature and body, *without* the doc comment so the
//! answer is not in the text — and the doc comment's first sentence becomes
//! the query that must find it. Reports recall@1, recall@10, MRR, index build
//! time and query latency per model. Ignored by default (downloads models):
//!
//! ```sh
//! LEAN_CTX_EMBED_EVAL_ROOT=/path/to/repo/rust/src \
//!   cargo test --release --lib -- --ignored embedding_retrieval_eval --nocapture
//! ```

use std::path::Path;
use std::time::Instant;

use super::EmbeddingEngine;
use super::model_registry::EmbeddingModel;

/// Body lines kept per function: enough to identify it, short like an index chunk.
const BODY_LINES: usize = 40;
/// Queries need some substance; one-word docs ("Getter.") prove nothing.
const MIN_QUERY_WORDS: usize = 6;

struct Case {
    query: String,
    chunk: String,
}

fn collect_cases(root: &Path) -> Vec<Case> {
    let mut files: Vec<_> = ignore::WalkBuilder::new(root)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
        .map(|e| e.path().to_path_buf())
        .collect();
    files.sort();
    let mut cases = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        let mut doc: Vec<&str> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("///") {
                doc.push(rest.trim());
                continue;
            }
            if trimmed.starts_with("#[") {
                continue;
            }
            let is_fn = [
                "fn ",
                "pub fn ",
                "pub(crate) fn ",
                "pub(super) fn ",
                "async fn ",
                "pub async fn ",
            ]
            .iter()
            .any(|p| trimmed.starts_with(p));
            if is_fn && !doc.is_empty() {
                let joined = doc.join(" ");
                let sentence = joined
                    .split(". ")
                    .next()
                    .unwrap_or(&joined)
                    .trim()
                    .to_string();
                if sentence.split_whitespace().count() >= MIN_QUERY_WORDS {
                    let chunk = lines[i..lines.len().min(i + BODY_LINES)]
                        .iter()
                        .filter(|l| !l.trim_start().starts_with("//"))
                        .copied()
                        .collect::<Vec<_>>()
                        .join("\n");
                    cases.push(Case {
                        query: sentence,
                        chunk,
                    });
                }
            }
            doc.clear();
        }
    }
    cases
}

struct Score {
    recall_at_1: f64,
    recall_at_10: f64,
    mrr: f64,
    index_ms: u128,
    query_us_per: f64,
}

fn score(engine: &EmbeddingEngine, cases: &[Case]) -> Score {
    let chunks: Vec<&str> = cases.iter().map(|c| c.chunk.as_str()).collect();
    let t = Instant::now();
    let corpus = engine.embed_batch(&chunks).expect("embed corpus");
    let index_ms = t.elapsed().as_millis();

    let t = Instant::now();
    let queries: Vec<Vec<f32>> = cases
        .iter()
        .map(|c| engine.embed_query(&c.query).expect("embed query"))
        .collect();
    let query_us_per = t.elapsed().as_micros() as f64 / cases.len() as f64;

    let (mut hit1, mut hit10, mut rr) = (0usize, 0usize, 0.0f64);
    for (target, q) in queries.iter().enumerate() {
        let own = super::cosine_similarity(q, &corpus[target]);
        // Rank of the target = 1 + chunks scoring strictly higher.
        let rank = 1 + corpus
            .iter()
            .enumerate()
            .filter(|(j, v)| *j != target && super::cosine_similarity(q, v) > own)
            .count();
        hit1 += usize::from(rank == 1);
        hit10 += usize::from(rank <= 10);
        rr += 1.0 / rank as f64;
    }
    let n = cases.len() as f64;
    Score {
        recall_at_1: hit1 as f64 / n,
        recall_at_10: hit10 as f64 / n,
        mrr: rr / n,
        index_ms,
        query_us_per,
    }
}

#[test]
#[ignore = "downloads embedding models and walks a source tree"]
fn embedding_retrieval_eval() {
    let Ok(root) = std::env::var("LEAN_CTX_EMBED_EVAL_ROOT") else {
        eprintln!("set LEAN_CTX_EMBED_EVAL_ROOT to a source tree");
        return;
    };
    let cases = collect_cases(Path::new(&root));
    assert!(
        cases.len() >= 100,
        "only {} documented functions",
        cases.len()
    );
    eprintln!("cases: {} documented functions under {root}", cases.len());
    let base = EmbeddingEngine::model_directory();
    let models = std::env::var("LEAN_CTX_EMBED_EVAL_MODELS")
        .unwrap_or_else(|_| "potion-code-16m,minilm".to_string());
    for name in models.split(',') {
        let model = EmbeddingModel::from_str_name(name).expect("known model");
        let t = Instant::now();
        let engine = match EmbeddingEngine::load_model(&base, model.clone()) {
            Ok(engine) => engine,
            Err(e) => {
                eprintln!("{model}: not loadable here ({e})");
                continue;
            }
        };
        let load_ms = t.elapsed().as_millis();
        let s = score(&engine, &cases);
        eprintln!(
            "{model:>22}  dims {:>4}  R@1 {:.3}  R@10 {:.3}  MRR {:.3}  load {load_ms} ms  \
             index {} ms ({:.0} chunks/s)  query {:.0} µs",
            engine.dimensions(),
            s.recall_at_1,
            s.recall_at_10,
            s.mrr,
            s.index_ms,
            cases.len() as f64 / (s.index_ms.max(1) as f64 / 1000.0),
            s.query_us_per,
        );
    }
}
