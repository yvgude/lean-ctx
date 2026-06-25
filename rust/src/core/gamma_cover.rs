//! Minimal TF-IDF chunk subset covering peers within γ bits of residual information.

use std::collections::{HashMap, HashSet};

fn entropy_bits(tokens: &[String]) -> f64 {
    if tokens.is_empty() {
        return 0.0;
    }
    let mut freq: HashMap<&str, usize> = HashMap::new();
    for t in tokens {
        *freq.entry(t.as_str()).or_default() += 1;
    }
    let n = tokens.len() as f64;
    freq.values().fold(0.0_f64, |acc, &c| {
        let p = c as f64 / n;
        acc - p * p.log2()
    })
}

fn tf_idf_vectors(chunks: &[(String, Vec<String>)]) -> Vec<HashMap<usize, f64>> {
    let n_docs = chunks.len();
    let mut df: HashMap<&str, usize> = HashMap::new();

    for (_, tokens) in chunks {
        let mut seen = HashSet::new();
        for tok in tokens {
            let prev = seen.insert(tok.as_str());
            if prev {
                *df.entry(tok.as_str()).or_default() += 1;
            }
        }
    }

    let mut vocab: HashMap<&str, usize> = HashMap::new();
    let mut next_id = 0usize;
    for (_, tokens) in chunks {
        for tok in tokens {
            vocab.entry(tok.as_str()).or_insert_with(|| {
                let id = next_id;
                next_id += 1;
                id
            });
        }
    }

    chunks
        .iter()
        .map(|(_, tokens)| {
            if tokens.is_empty() {
                return HashMap::new();
            }
            let mut tf: HashMap<&str, usize> = HashMap::new();
            let mut max_tf = 1usize;
            for tok in tokens {
                let e = tf.entry(tok.as_str()).or_default();
                *e += 1;
                max_tf = max_tf.max(*e);
            }
            let mut v = HashMap::new();
            for (term, &c) in &tf {
                let Some(&tid) = vocab.get(term) else {
                    continue;
                };
                let tf_norm = c as f64 / max_tf as f64;
                let dfi = df.get(term).copied().unwrap_or(1).max(1);
                let idf = ((n_docs as f64 + 1.0) / (dfi as f64 + 1.0)).ln() + 1.0;
                v.insert(tid, tf_norm * idf);
            }
            v
        })
        .collect()
}

fn cosine_sparse(a: &HashMap<usize, f64>, b: &HashMap<usize, f64>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0_f64;
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    for (&k, &va) in small {
        if let Some(&vb) = large.get(&k) {
            dot += va * vb;
        }
    }
    let na: f64 = a.values().map(|x| x * x).sum::<f64>().sqrt();
    let nb: f64 = b.values().map(|x| x * x).sum::<f64>().sqrt();
    if na <= f64::EPSILON || nb <= f64::EPSILON {
        return 0.0;
    }
    (dot / (na * nb)).clamp(0.0, 1.0)
}

fn covers(vecs: &[HashMap<usize, f64>], entropy: &[f64], i: usize, j: usize, gamma: f64) -> bool {
    if i == j {
        return true;
    }
    let h = entropy[j];
    if h <= f64::EPSILON {
        return true;
    }
    let sim = cosine_sparse(&vecs[i], &vecs[j]);
    let residual = (1.0 - sim) * h;
    residual <= gamma + 1e-12
}

/// Greedy set cover: repeatedly pick the chunk that newly covers the most still-uncovered chunks.
/// Chunk `i` covers `j` when information not explained by similarity to `i` is at most `gamma` bits.
#[must_use]
pub fn compute_cover(chunks: &[(String, Vec<String>)], gamma: f64) -> Vec<usize> {
    let n = chunks.len();
    if n == 0 {
        return Vec::new();
    }

    let vecs = tf_idf_vectors(chunks);
    let entropy: Vec<f64> = chunks.iter().map(|(_, t)| entropy_bits(t)).collect();

    let mut picked = Vec::new();
    let mut covered = vec![false; n];

    loop {
        if covered.iter().all(|&c| c) {
            break;
        }

        let mut best_i = usize::MAX;
        let mut best_gain = 0usize;

        for i in 0..n {
            let gain = (0..n)
                .filter(|&j| !covered[j] && covers(&vecs, &entropy, i, j, gamma))
                .count();
            if gain > best_gain {
                best_gain = gain;
                best_i = i;
            } else if gain == best_gain && gain > 0 && i < best_i {
                best_i = i;
            }
        }

        if best_gain == 0 {
            // fallback: cover leftovers individually (singleton summaries still close under γ large enough)
            if let Some(j) = (0..n).find(|&j| !covered[j]) {
                picked.push(j);
                covered[j] = true;
            } else {
                break;
            }
            continue;
        }

        picked.push(best_i);
        for (j, cov) in covered.iter_mut().enumerate().take(n) {
            if covers(&vecs, &entropy, best_i, j, gamma) {
                *cov = true;
            }
        }
    }

    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(content: &str, tokens: &[&str]) -> (String, Vec<String>) {
        (content.into(), tokens.iter().map(|s| (*s).into()).collect())
    }

    #[test]
    fn empty_input() {
        assert!(compute_cover(&[], 1.0).is_empty());
    }

    #[test]
    fn duplicate_chunks_one_covers_other() {
        let c = vec![
            chunk(
                "alpha beta gamma delta",
                &["alpha", "beta", "gamma", "delta"],
            ),
            chunk(
                "alpha beta gamma delta",
                &["alpha", "beta", "gamma", "delta"],
            ),
        ];
        let cov = compute_cover(&c, 0.01);
        assert_eq!(cov.len(), 1);
    }

    #[test]
    fn hub_chunk_covers_spokes() {
        let hub_toks = vec!["fn", "parse", "emit", "error", "ok", "ctx"];
        let mut chunks = vec![chunk("hub impl", &hub_toks)];
        for i in 0..4 {
            let mut toks = hub_toks.clone();
            toks.push(["extra_a", "extra_b", "noise_z"][i % 3]);
            chunks.push(chunk("spoke", &toks));
        }
        let cov = compute_cover(&chunks, 2.5);
        assert!(cov.len() <= 3);
        assert!(cov.contains(&0));
    }

    #[test]
    fn orthogonal_chunks_need_multiple_picks() {
        let c = vec![
            chunk("u", &["u1", "u2", "u3"]),
            chunk("v", &["v1", "v2", "v3"]),
            chunk("w", &["w1", "w2", "w3"]),
        ];
        let cov = compute_cover(&c, 0.01);
        assert!(cov.len() >= 2);
    }
}
