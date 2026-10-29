// SPDX-License-Identifier: Apache-2.0
//! Dedupe of the capture gate (SPEC-ENGINE §3.1.5): an exact key over the
//! normalised statement and a MinHash near-duplicate check, both keyed with
//! the place id key `pk_id`, so two engines of a place reach the same
//! verdict and nobody without the key can confirm a guessed statement.

use unicode_normalization::UnicodeNormalization as _;

use crate::core::knowledge::NodeType;

/// Permutations of the MinHash signature.
const PERMUTATIONS: usize = 64;
/// Constant seeds (`DEDUPE_SEED_V1`): the same on every engine.
const DEDUPE_SEED_V1: u64 = 0x6c65_616e_6374_7831;
/// Estimated Jaccard at or above which two statements are near duplicates.
const NEAR_THRESHOLD: f64 = 0.80;

static PUNCTUATION: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^\p{P}$").expect("static pattern"));

/// norm v1 (C0 2.0 `vectors/norm.vectors.json`): NFKC → full Unicode
/// lowercase → every character of general category P removed outside
/// backtick spans (a backtick toggles a span and is kept; an unclosed span
/// runs to the end) → each run of White_Space replaced by one space → trim.
pub fn norm(s: &str) -> String {
    // `str::to_lowercase` applies the full mapping, final sigma included.
    let lower = s.nfkc().collect::<String>().to_lowercase();
    let mut out = String::new();
    let mut in_code = false;
    let mut buf = [0u8; 4];
    for c in lower.chars() {
        if c == '`' {
            in_code = !in_code;
            out.push(c);
        } else if c.is_whitespace() {
            if !out.ends_with(' ') {
                out.push(' ');
            }
        } else if in_code || !PUNCTUATION.is_match(c.encode_utf8(&mut buf)) {
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// Exact dedupe key `dk = blake3_keyed(pk_id, node_type || 0x00 || norm)`.
pub fn exact_key(pk_id: &[u8; 32], node_type: NodeType, statement: &str) -> [u8; 32] {
    let mut h = blake3::Hasher::new_keyed(pk_id);
    h.update(node_type.as_str().as_bytes());
    h.update(&[0]);
    h.update(norm(statement).as_bytes());
    *h.finalize().as_bytes()
}

/// MinHash signature over keyed word 3-shingles.
pub type Signature = [u64; PERMUTATIONS];

fn mix(mut x: u64) -> u64 {
    // splitmix64 finaliser: a fixed, portable permutation of u64.
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

pub fn signature(pk_id: &[u8; 32], statement: &str) -> Signature {
    let normalized = norm(statement);
    let words: Vec<&str> = normalized.split(' ').filter(|w| !w.is_empty()).collect();
    let shingles: Vec<u64> = if words.len() < 3 {
        vec![shingle_hash(pk_id, &words.join(" "))]
    } else {
        words
            .windows(3)
            .map(|w| shingle_hash(pk_id, &w.join(" ")))
            .collect()
    };
    let mut sig = [u64::MAX; PERMUTATIONS];
    for (i, slot) in sig.iter_mut().enumerate() {
        let seed = mix(DEDUPE_SEED_V1.wrapping_add(i as u64));
        for s in &shingles {
            *slot = (*slot).min(mix(s ^ seed));
        }
    }
    sig
}

fn shingle_hash(pk_id: &[u8; 32], shingle: &str) -> u64 {
    let digest = blake3::keyed_hash(pk_id, shingle.as_bytes());
    u64::from_le_bytes(digest.as_bytes()[..8].try_into().expect("8 bytes"))
}

/// Estimated Jaccard similarity of two signatures.
pub fn similarity(a: &Signature, b: &Signature) -> f64 {
    let same = a.iter().zip(b).filter(|(x, y)| x == y).count();
    same as f64 / PERMUTATIONS as f64
}

pub fn is_near(a: &Signature, b: &Signature) -> bool {
    similarity(a, b) >= NEAR_THRESHOLD
}

/// What a dedupe index knows about one captured node.
#[derive(Debug, Clone)]
pub struct Known {
    pub node_type: NodeType,
    pub signature: Signature,
    /// References the node already carries.
    pub refs: Vec<String>,
}

/// Dedupe verdict for a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    New,
    Duplicate,
    /// Near duplicate of node `index`; the candidate adds these references.
    Merge {
        index: usize,
        refs: Vec<String>,
    },
}

/// Exact keys and signatures of the place plus the current batch.
#[derive(Debug, Default)]
pub struct Index {
    exact: std::collections::HashSet<[u8; 32]>,
    known: Vec<Known>,
}

impl Index {
    pub fn insert(&mut self, key: [u8; 32], known: Known) {
        self.exact.insert(key);
        self.known.push(known);
    }

    pub fn known_mut(&mut self, index: usize) -> Option<&mut Known> {
        self.known.get_mut(index)
    }

    pub fn verdict(&self, key: &[u8; 32], known: &Known) -> Verdict {
        if self.exact.contains(key) {
            return Verdict::Duplicate;
        }
        let near = self.known.iter().position(|k| {
            k.node_type == known.node_type && is_near(&k.signature, &known.signature)
        });
        match near {
            None => Verdict::New,
            Some(index) => {
                let refs: Vec<String> = known
                    .refs
                    .iter()
                    .filter(|r| !self.known[index].refs.contains(r))
                    .cloned()
                    .collect();
                if refs.is_empty() {
                    Verdict::Duplicate
                } else {
                    Verdict::Merge { index, refs }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7; 32];

    #[test]
    fn norm_keeps_code_and_drops_punctuation() {
        assert_eq!(
            norm("  The  Relay, drops `a.b()`  frames!  "),
            "the relay drops `a.b()` frames"
        );
        assert_eq!(norm("Ｆｕｌｌwidth"), "fullwidth");
    }

    #[test]
    fn exact_key_is_case_and_space_insensitive_and_typed() {
        let a = exact_key(&KEY, NodeType::Fact, "The relay drops frames.");
        assert_eq!(
            a,
            exact_key(&KEY, NodeType::Fact, "the  relay drops frames")
        );
        assert_ne!(
            a,
            exact_key(&KEY, NodeType::Gotcha, "The relay drops frames.")
        );
        assert_ne!(
            a,
            exact_key(&[8; 32], NodeType::Fact, "The relay drops frames.")
        );
    }

    #[test]
    fn near_duplicates_are_found_and_others_are_not() {
        let base = "The outbox retries a refused push after the lease is renewed by the scheduler.";
        let one_edit =
            "The outbox retries a refused push after the lease is renewed by the scheduler again.";
        let other =
            "Sealed vaults wrap every content key with the current epoch key before upload.";
        let s = signature(&KEY, base);
        assert!(is_near(&s, &signature(&KEY, one_edit)));
        assert!(!is_near(&s, &signature(&KEY, other)));
        assert_eq!(s, signature(&KEY, base), "deterministic");
    }

    #[test]
    fn near_duplicate_with_a_new_reference_merges() {
        let text = "The outbox retries a refused push after the lease is renewed by the scheduler.";
        let mut index = Index::default();
        let known = |refs: &[&str]| Known {
            node_type: NodeType::Fact,
            signature: signature(&KEY, text),
            refs: refs.iter().map(ToString::to_string).collect(),
        };
        index.insert(exact_key(&KEY, NodeType::Fact, text), known(&["file:a.rs"]));
        let near = format!("{text} Always.");
        let key = exact_key(&KEY, NodeType::Fact, &near);
        assert_eq!(
            index.verdict(&key, &known(&["file:a.rs"])),
            Verdict::Duplicate
        );
        assert_eq!(
            index.verdict(&key, &known(&["file:a.rs", "file:b.rs"])),
            Verdict::Merge {
                index: 0,
                refs: vec!["file:b.rs".into()]
            }
        );
    }
}
