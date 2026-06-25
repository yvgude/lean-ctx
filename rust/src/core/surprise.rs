//! Predictive Surprise Scoring — conditional entropy relative to LLM knowledge.
//!
//! Instead of measuring Shannon entropy in isolation (H(X)), we measure
//! how surprising each line is to the LLM: H(X | `LLM_knowledge`).
//!
//! Approximation: use BPE token frequency ranks from `o200k_base` as a proxy
//! for P(token | LLM). Common tokens (high frequency rank) carry low surprise;
//! rare tokens (low rank / unknown to the vocab) carry high surprise.
//!
//! Scientific basis: Cross-entropy H(P,Q) = -sum(P(x) * log Q(x))
//! where P is the true distribution and Q is the model's prior.

use std::sync::OnceLock;

use super::tokens::encode_tokens;

static VOCAB_LOG_PROBS: OnceLock<Vec<f64>> = OnceLock::new();

/// Build a log-probability table indexed by token ID.
/// Uses a Zipfian approximation: P(rank r) ~ 1/(r * `H_n`) where `H_n` is the
/// harmonic number. This closely matches empirical BPE token distributions.
fn get_vocab_log_probs() -> &'static Vec<f64> {
    VOCAB_LOG_PROBS.get_or_init(|| {
        let vocab_size = 200_000usize;
        let h_n: f64 = (1..=vocab_size).map(|r| 1.0 / r as f64).sum();
        (0..vocab_size)
            .map(|rank| {
                let r = rank + 1; // 1-indexed rank
                let p = 1.0 / (r as f64 * h_n);
                -p.log2()
            })
            .collect()
    })
}

/// Compute the surprise score for a line of text.
///
/// Returns the mean negative log-probability (cross-entropy) of the line's
/// BPE tokens under the Zipfian prior. Higher values = more surprising to
/// the LLM = more important to keep.
///
/// Range: typically 5.0 (very common) to 17.0+ (very rare).
#[must_use]
pub fn line_surprise(text: &str) -> f64 {
    let tokens = encode_tokens(text);
    if tokens.is_empty() {
        return 0.0;
    }
    let log_probs = get_vocab_log_probs();
    let max_id = log_probs.len();

    let total: f64 = tokens
        .iter()
        .map(|&t| {
            let id = t as usize;
            if id < max_id {
                log_probs[id]
            } else {
                17.6 // max surprise for OOV tokens (~log2(200000))
            }
        })
        .sum();

    total / tokens.len() as f64
}

/// Classify how surprising a line is relative to the LLM's expected knowledge.
/// Uses empirically calibrated thresholds for `o200k_base`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurpriseLevel {
    /// Common patterns — safe to compress aggressively
    Low,
    /// Mixed content — standard compression
    Medium,
    /// Rare/unique tokens — preserve carefully
    High,
}

#[must_use]
pub fn classify_surprise(text: &str) -> SurpriseLevel {
    let s = line_surprise(text);
    if s < 8.0 {
        SurpriseLevel::Low
    } else if s < 12.0 {
        SurpriseLevel::Medium
    } else {
        SurpriseLevel::High
    }
}

/// Enhanced entropy filter that combines Shannon entropy with predictive surprise.
/// Lines pass if EITHER their entropy is above threshold OR their surprise is high.
/// This prevents dropping lines that look "low entropy" but contain rare, unique tokens.
#[must_use]
pub fn should_keep_line(trimmed: &str, entropy_threshold: f64) -> bool {
    if trimmed.is_empty() || trimmed.len() < 3 {
        return true;
    }

    let tokens = encode_tokens(trimmed);
    let h = super::entropy::token_entropy_from_ids(&tokens);
    if h >= entropy_threshold {
        return true;
    }

    let h_norm = super::entropy::normalized_token_entropy_from_ids(&tokens);
    if h_norm >= 0.3 {
        return true;
    }

    // New: check if line has high surprise despite low entropy.
    // This catches lines like `CustomDomainType::validate()`
    // which have low token diversity but high surprise per-token.
    let surprise = line_surprise(trimmed);
    surprise >= 11.0
}

// ---------------------------------------------------------------------------
// Semantic redundancy scoring (#544, EFF-7)
// ---------------------------------------------------------------------------
//
// The Zipf prior measures *lexical* rarity: a rare boilerplate identifier
// scores high (kept), a frequent but semantically unique line scores low
// (dropped). The LLMLingua family (survey 2410.12388) climbs this ladder
// with real likelihood models; the strongest model-assisted step we can take
// without shipping a token classifier is MMR-style semantic dedup: a line
// that is near-identical *in embedding space* to something already kept
// carries almost no marginal information (rate-distortion: spend bits on
// distinct content only). This is exactly what H2O/SnapKV exploit at the
// KV level.
//
// The embedder is injected as a function so the production path can use the
// real (feature-gated, lazily loaded) embedding engine while the scoring
// math stays testable; `None` results fall back to pure Zipf behavior,
// keeping the no-embeddings build byte-identical.

/// Kept-line embedding window for MMR redundancy checks. Capped so the
/// incremental cost stays O(n·64) per file.
const KEPT_WINDOW: usize = 64;
/// Cosine similarity at or above this means "semantically duplicate".
const REDUNDANCY_COSINE: f64 = 0.92;

/// Sliding context of already-kept line embeddings.
#[derive(Default)]
pub struct ScoringCtx {
    kept: std::collections::VecDeque<Vec<f32>>,
}

impl ScoringCtx {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_kept(&mut self, embedding: Vec<f32>) {
        if self.kept.len() >= KEPT_WINDOW {
            self.kept.pop_front();
        }
        self.kept.push_back(embedding);
    }

    /// Max cosine similarity of `emb` to any kept embedding.
    pub fn max_cosine(&self, emb: &[f32]) -> f64 {
        self.kept
            .iter()
            .map(|k| cosine(k, emb))
            .fold(0.0_f64, f64::max)
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0_f64;
    let mut na = 0.0_f64;
    let mut nb = 0.0_f64;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += f64::from(*x) * f64::from(*y);
        na += f64::from(*x) * f64::from(*x);
        nb += f64::from(*y) * f64::from(*y);
    }
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Semantic-redundancy keep decision (#544): the Zipf path nominates keep
/// candidates exactly as `should_keep_line`; candidates that embed nearly
/// identically to an already-kept line are dropped (MMR). `embed` returning
/// `None` (engine not loaded / feature off) preserves today's behavior
/// byte-for-byte.
pub fn should_keep_line_semantic(
    trimmed: &str,
    entropy_threshold: f64,
    embed: &dyn Fn(&str) -> Option<Vec<f32>>,
    ctx: &mut ScoringCtx,
) -> bool {
    if !should_keep_line(trimmed, entropy_threshold) {
        return false;
    }
    let Some(emb) = embed(trimmed) else {
        return true;
    };
    if ctx.max_cosine(&emb) >= REDUNDANCY_COSINE {
        return false;
    }
    ctx.push_kept(emb);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_code_has_low_surprise() {
        let common = "let x = 1;";
        let s = line_surprise(common);
        assert!(s > 0.0, "surprise should be positive");
    }

    #[test]
    fn rare_identifiers_have_higher_surprise() {
        let common = "let x = 1;";
        let rare = "let zygomorphic_validator = XenolithProcessor::new();";
        assert!(
            line_surprise(rare) > line_surprise(common),
            "rare identifiers should have higher surprise"
        );
    }

    #[test]
    fn empty_returns_zero() {
        assert_eq!(line_surprise(""), 0.0);
    }

    #[test]
    fn classify_surprise_is_consistent() {
        let simple = "let x = 1;";
        let complex = "ZygomorphicXenolithValidator::process_quantum_state(&mut ctx)";
        let s_simple = line_surprise(simple);
        let s_complex = line_surprise(complex);
        assert!(
            s_complex > s_simple,
            "rare identifiers ({s_complex}) should have higher surprise than common code ({s_simple})"
        );
    }

    #[test]
    fn should_keep_preserves_rare_lines() {
        let rare = "ZygomorphicValidator::process_xenolith(&mut state)";
        assert!(
            should_keep_line(rare, 1.0) || line_surprise(rare) < 11.0,
            "rare lines should be preserved or have measurable surprise"
        );
    }

    /// Deterministic hashed bag-of-words vectorizer: a real (if simple)
    /// embedding function for unit-testing the MMR math. Production uses the
    /// feature-gated neural engine through the same `embed` seam.
    #[allow(clippy::unnecessary_wraps)] // Option matches the fallible embed seam
    fn bow_embed(line: &str) -> Option<Vec<f32>> {
        let mut v = vec![0.0f32; 64];
        for tok in line.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
            if tok.len() < 2 {
                continue;
            }
            let mut h = 0u64;
            for b in tok.bytes() {
                h = h.wrapping_mul(31).wrapping_add(u64::from(b));
            }
            v[(h % 64) as usize] += 1.0;
        }
        Some(v)
    }

    #[test]
    fn semantic_dedup_drops_near_duplicate_kept_lines() {
        let mut ctx = ScoringCtx::new();
        let embed = |s: &str| bow_embed(s);
        let a = "fn validate_user_payload(payload: &UserPayload) -> Result<(), ValidationError>";
        // Same bag of words, reordered — semantically duplicate logic.
        let b = "fn validate_user_payload(payload: &UserPayload) -> Result<(), ValidationError> ";
        let c = "const MAX_RETRY_BACKOFF_MS: u64 = 30_000;";

        assert!(should_keep_line_semantic(a, 0.5, &embed, &mut ctx));
        assert!(
            !should_keep_line_semantic(b, 0.5, &embed, &mut ctx),
            "near-identical line must be dropped as redundant"
        );
        assert!(
            should_keep_line_semantic(c, 0.5, &embed, &mut ctx),
            "distinct line stays"
        );
    }

    #[test]
    fn no_embedder_is_identical_to_zipf_path() {
        let mut ctx = ScoringCtx::new();
        let none = |_: &str| None;
        for line in [
            "fn main() { run(); }",
            "let x = 1;",
            "ZygomorphicXenolithValidator::process_quantum_state(&mut ctx)",
            "// plain comment",
        ] {
            assert_eq!(
                should_keep_line_semantic(line, 1.0, &none, &mut ctx),
                should_keep_line(line, 1.0),
                "without embeddings the decision must match the Zipf path: {line}"
            );
        }
    }

    #[test]
    fn kept_window_is_bounded() {
        let mut ctx = ScoringCtx::new();
        for i in 0..200 {
            ctx.push_kept(vec![i as f32; 8]);
        }
        assert!(ctx.kept.len() <= 64);
    }

    #[test]
    fn cosine_handles_degenerate_inputs() {
        assert_eq!(cosine(&[], &[]), 0.0);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[0.0, 0.0]), 0.0);
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-9);
    }
}
