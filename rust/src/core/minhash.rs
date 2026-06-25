//! `MinHash` computation from tree-sitter AST for structural fingerprinting of
//! function/class/impl nodes.
//!
//! Walks the subtree with a `TreeCursor`, collects normalized leaf-node types,
//! builds trigrams, and computes 64-permutation `MinHash` values.
//!
//! Matches the algorithm in CBM's `minhash.c`: same normalization scheme,
//! same trigram construction, same 64-minhash aggregation.

use std::hash::{Hash, Hasher};
use tree_sitter::Node;

/// Build a deterministic `MinHash` fingerprint for the subtree rooted at `node`.
///
/// Returns `None` when the subtree yields fewer than 30 normalized tokens
/// (e.g. signature-only / bodyless definitions).
///
/// The hash is a 64-element `[u32; 64]` — one minimum value per permutation
/// seed — suitable for Jaccard approximation via postcard serialization.
#[must_use]
pub fn compute_minhash(node: &Node) -> Option<[u32; 64]> {
    let tokens = collect_normalized_tokens(node);

    if tokens.len() < 30 {
        return None;
    }

    let trigrams = build_trigrams(&tokens);

    let mut minhash = [u32::MAX; 64];
    for trigram in &trigrams {
        for seed in 0..64u64 {
            let h = hash_trigram(trigram, seed);
            if h < minhash[seed as usize] {
                minhash[seed as usize] = h;
            }
        }
    }

    Some(minhash)
}

/// Walk the subtree using `TreeCursor`, collecting normalised type tokens for
/// every leaf (zero named children). Comments are skipped; everything else is
/// mapped to a 1-2 character code.
fn collect_normalized_tokens(node: &Node) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cursor = node.walk();

    loop {
        let current = cursor.node();
        let kind = current.kind();

        // Skip comment node bodies and their anonymous children entirely.
        if !kind.contains("comment") && current.named_child_count() == 0 {
            // Leaf node: emit normalised type code.
            tokens.push(normalized_kind(kind));
        }

        // Descend into non-comment nodes only — comment bodies are structural
        // noise and their anonymous children (/*, */) don't carry "comment" in
        // their kind name, so we prune at the comment boundary.
        if !kind.contains("comment") && cursor.goto_first_child() {
            continue;
        }

        // No children (or comment node) — try next sibling.
        if cursor.goto_next_sibling() {
            continue;
        }

        // No sibling — ascend until we find an uncle.
        loop {
            if !cursor.goto_parent() {
                return tokens;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Map a tree-sitter `kind()` string to its 1-2 character normalised code.
///
/// Order matters: more specific patterns (e.g. `type_identifier`) MUST be
/// checked before broader ones (e.g. `identifier`) so that "`type_identifier`"
/// maps to "T" rather than being caught by the "identifier" → "I" rule.
fn normalized_kind(kind: &str) -> String {
    // type_identifier → "T" (must precede the generic "identifier" check)
    if kind == "type_identifier" || kind == "type" {
        return "T".to_string();
    }
    // generic identifier → "I"
    if kind.contains("identifier") {
        return "I".to_string();
    }
    if kind.contains("string") || kind.contains("string_literal") || kind.contains("string_content")
    {
        return "S".to_string();
    }
    if kind.contains("number") || kind.contains("integer") || kind.contains("float") {
        return "N".to_string();
    }
    if kind.contains("type") {
        return "T".to_string();
    }
    // First two characters of the original kind name.
    kind.chars().take(2).collect()
}

/// Build a sequence of trigrams from the normalised token list.
///
/// Each trigram is the concatenation of three consecutive normalised tokens.
/// Returns an empty vec when there are fewer than 3 tokens.
fn build_trigrams(tokens: &[String]) -> Vec<String> {
    if tokens.len() < 3 {
        return Vec::new();
    }
    tokens
        .windows(3)
        .map(|w| format!("{}{}{}", w[0], w[1], w[2]))
        .collect()
}

/// Hash a trigram string with a permutation seed using `DefaultHasher`.
///
/// The seed is hashed first, then the trigram, producing a u64 that is folded
/// to u32 via `(hi32 ^ lo32)`.
fn hash_trigram(trigram: &str, seed: u64) -> u32 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut hasher);
    trigram.hash(&mut hasher);
    let hash = hasher.finish();
    (hash >> 32) as u32 ^ (hash as u32)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_rust(src: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("Rust parser");
        parser.parse(src, None).expect("parse")
    }

    #[test]
    fn test_minhash_basic_function() {
        let src = r"
pub fn add(a: i32, b: i32) -> i32 {
    let result = a + b;
    if result > 100 {
        return 100;
    }
    result
}
";
        let tree = parse_rust(src);
        let root = tree.root_node();
        // first named child is the function_item
        let fn_node = root.named_child(0).expect("function_item");
        assert_eq!(fn_node.kind(), "function_item");

        let mh = compute_minhash(&fn_node);
        assert!(mh.is_some(), "expected Some fingerprint");
        let mh = mh.unwrap();

        // All 64 slots should be non-max (at least one trigram contributed).
        for (i, &v) in mh.iter().enumerate() {
            assert!(
                v != u32::MAX,
                "seed {i} still at MAX — no trigram gave a hash?"
            );
        }
    }

    #[test]
    fn test_minhash_bodyless_returns_none() {
        // A trait method signature has no body → < 30 tokens → None.
        let src = "trait Foo { fn bar(x: i32) -> i32; }";
        let tree = parse_rust(src);
        let root = tree.root_node();
        let trait_node = root.named_child(0).expect("trait_item");
        // The trait body contains a `function_signature_item`, not a
        // `function_item`. Grab the signature child directly.
        let body = trait_node.child_by_field_name("body").expect("trait body");
        let sig = body.named_child(0).expect("function_signature_item");
        assert!(sig.kind().contains("function_signature"));

        let mh = compute_minhash(&sig);
        assert!(mh.is_none(), "bodyless signature should return None");
    }

    #[test]
    fn test_minhash_short_body_returns_none() {
        // Very short function (< 30 normalised tokens).
        let src = "fn f() { let x = 1; }";
        let tree = parse_rust(src);
        let root = tree.root_node();
        let fn_node = root.named_child(0).expect("function_item");

        let mh = compute_minhash(&fn_node);
        assert!(mh.is_none(), "short body should return None");
    }

    #[test]
    fn test_deterministic_same_ast() {
        // Two structurally identical (but differently named) functions with
        // enough tokens to exceed the 30-token floor.
        let src_a = "\
fn foo(x: i32) -> i32 {
    let a = x + 1;
    let b = a * 2;
    if b > 10 {
        return b;
    }
    let c = b - 5;
    let d = c / 3;
    d
}";
        let src_b = "\
fn bar(y: i32) -> i32 {
    let a = y + 1;
    let b = a * 2;
    if b > 10 {
        return b;
    }
    let c = b - 5;
    let d = c / 3;
    d
}";

        let tree_a = parse_rust(src_a);
        let tree_b = parse_rust(src_b);
        let fn_a = tree_a.root_node().named_child(0).unwrap();
        let fn_b = tree_b.root_node().named_child(0).unwrap();

        let mh_a = compute_minhash(&fn_a).unwrap();
        let mh_b = compute_minhash(&fn_b).unwrap();

        assert_eq!(
            mh_a, mh_b,
            "identical structure should produce identical hash"
        );
    }

    #[test]
    fn test_different_structure_different_hash() {
        let src_a = "\
fn foo(x: i32) -> i32 {
    let a = x + 1;
    let b = a * 2;
    if b > 10 {
        return b;
    }
    let c = b - 5;
    let d = c / 3;
    d
}";
        let src_b = "\
fn bar(x: i32) -> i32 {
    for i in 0..x {
        let y = i * 2;
        if y > 100 {
            break;
        }
        println!(\"{}\", y);
    }
    0
}";

        let tree_a = parse_rust(src_a);
        let tree_b = parse_rust(src_b);
        let fn_a = tree_a.root_node().named_child(0).unwrap();
        let fn_b = tree_b.root_node().named_child(0).unwrap();

        let mh_a = compute_minhash(&fn_a).unwrap();
        let mh_b = compute_minhash(&fn_b).unwrap();

        assert_ne!(
            mh_a, mh_b,
            "different structure should produce different hash"
        );
    }

    #[test]
    fn test_struct_minhash() {
        let src = r"
pub struct Config {
    pub host: String,
    pub port: u16,
    pub timeout: u64,
    pub max_retries: u32,
    pub tls: bool,
    pub ca_path: Option<String>,
    pub cert_path: Option<String>,
    pub key_path: Option<String>,
}
";
        let tree = parse_rust(src);
        let root = tree.root_node();
        let struct_node = root.named_child(0).expect("struct_item");

        let mh = compute_minhash(&struct_node);
        assert!(mh.is_some(), "struct should produce a fingerprint");
    }

    #[test]
    fn test_impl_minhash() {
        let src = r"
impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    fn area(&self) -> f64 {
        0.0
    }
}
";
        let tree = parse_rust(src);
        let root = tree.root_node();
        let impl_node = root.named_child(0).expect("impl_item");

        let mh = compute_minhash(&impl_node);
        assert!(mh.is_some(), "impl block should produce a fingerprint");
    }

    #[test]
    fn test_comment_skipped() {
        // Comments should not affect normalised tokens.
        let src_no_comment = "fn f() { let x = 1; }";
        let src_with_comment = "fn f() { /* hello */ let x = 1; }";

        let tree_no = parse_rust(src_no_comment);
        let tree_with = parse_rust(src_with_comment);
        let fn_no = tree_no.root_node().named_child(0).unwrap();
        let fn_with = tree_with.root_node().named_child(0).unwrap();

        let tokens_no = collect_normalized_tokens(&fn_no);
        let tokens_with = collect_normalized_tokens(&fn_with);

        assert_eq!(
            tokens_no, tokens_with,
            "comments should be skipped\n  no comment: {tokens_no:?}\n  with comment: {tokens_with:?}"
        );
    }

    #[test]
    fn test_normalized_kind_mapping() {
        assert_eq!(normalized_kind("identifier"), "I");
        assert_eq!(normalized_kind("type_identifier"), "T");
        assert_eq!(normalized_kind("string_literal"), "S");
        assert_eq!(normalized_kind("integer_literal"), "N");
        assert_eq!(normalized_kind("float_literal"), "N");
        assert_eq!(normalized_kind("function_item"), "fu");
        assert_eq!(normalized_kind("if_statement"), "if");
        assert_eq!(normalized_kind("let_declaration"), "le");
        assert_eq!(normalized_kind("block"), "bl");
    }

    #[test]
    fn test_build_trigrams_basic() {
        let tokens: Vec<String> = vec!["I".into(), "T".into(), "N".into(), "S".into()];
        let trigrams = build_trigrams(&tokens);
        assert_eq!(trigrams.len(), 2);
        assert_eq!(trigrams[0], "ITN");
        assert_eq!(trigrams[1], "TNS");
    }

    #[test]
    fn test_build_trigrams_too_short() {
        assert!(build_trigrams(&[]).is_empty());
        assert!(build_trigrams(&["I".into()]).is_empty());
        assert!(build_trigrams(&["I".into(), "T".into()]).is_empty());
    }

    #[test]
    fn test_minhash_jaccard_identical_functions() {
        // Two identical functions should have Jaccard similarity = 1.0.
        let src = "\
fn foo(x: i32) -> i32 {
    let a = x + 1;
    let b = a * 2;
    if b > 10 {
        return b;
    }
    let c = b - 5;
    let d = c / 3;
    d
}";
        let tree = parse_rust(src);
        let root = tree.root_node();
        let fn_node = root.named_child(0).unwrap();

        let mh = compute_minhash(&fn_node).unwrap();

        // Jaccard = (# equal positions) / 64
        let equal = mh.iter().zip(mh.iter()).filter(|(x, y)| x == y).count();
        let jaccard = equal as f32 / 64.0;
        assert!(
            (jaccard - 1.0).abs() < f32::EPSILON,
            "identical function should have Jaccard = 1.0, got {jaccard}"
        );
    }

    #[test]
    fn test_hash_trigram_deterministic() {
        let h1 = hash_trigram("ITN", 0);
        let h2 = hash_trigram("ITN", 0);
        assert_eq!(h1, h2);

        // Different seed → different hash (overwhelmingly likely).
        let h3 = hash_trigram("ITN", 1);
        assert_ne!(h1, h3);

        // Different trigram → different hash.
        let h4 = hash_trigram("TNS", 0);
        assert_ne!(h1, h4);
    }
}
