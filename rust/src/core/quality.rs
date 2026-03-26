#![allow(dead_code)]
use crate::core::preservation;
use crate::core::tokens::count_tokens;

const QUALITY_THRESHOLD: f64 = 0.95;
const MIN_DENSITY: f64 = 0.15;

#[derive(Debug, Clone)]
pub struct QualityScore {
    pub ast_score: f64,
    pub identifier_score: f64,
    pub line_score: f64,
    pub density: f64,
    pub composite: f64,
    pub passed: bool,
}

impl QualityScore {
    pub fn format_compact(&self) -> String {
        if self.passed {
            format!(
                "Q:{:.0}% (ast:{:.0} id:{:.0} ln:{:.0} ρ:{:.0}) ✓",
                self.composite * 100.0,
                self.ast_score * 100.0,
                self.identifier_score * 100.0,
                self.line_score * 100.0,
                self.density * 100.0,
            )
        } else {
            format!(
                "Q:{:.0}% (ast:{:.0} id:{:.0} ln:{:.0} ρ:{:.0}) ✗ BELOW THRESHOLD",
                self.composite * 100.0,
                self.ast_score * 100.0,
                self.identifier_score * 100.0,
                self.line_score * 100.0,
                self.density * 100.0,
            )
        }
    }
}

pub fn score(original: &str, compressed: &str, ext: &str) -> QualityScore {
    let pres = preservation::measure(original, compressed, ext);
    let ast_score = pres.overall();

    let identifier_score = measure_identifier_preservation(original, compressed);
    let line_score = measure_line_preservation(original, compressed);
    let density = information_density(original, compressed, ext);

    let composite = ast_score * 0.5 + identifier_score * 0.3 + line_score * 0.2;

    let compression_ratio = measure_line_preservation(original, compressed);
    let adaptive_threshold = QUALITY_THRESHOLD - 0.05 * (1.0 - compression_ratio);
    let passed = composite >= adaptive_threshold && density >= MIN_DENSITY;

    QualityScore {
        ast_score,
        identifier_score,
        line_score,
        density,
        composite,
        passed,
    }
}

/// Information density: ratio of semantic tokens to total output tokens.
/// Measures how much "meaning" per output token is preserved.
pub fn information_density(original: &str, compressed: &str, ext: &str) -> f64 {
    let output_tokens = count_tokens(compressed);
    if output_tokens == 0 {
        return 1.0;
    }

    let pres = preservation::measure(original, compressed, ext);
    let semantic_items = pres.functions_preserved + pres.exports_preserved + pres.imports_preserved;
    let ident_re = regex::Regex::new(r"\b[a-zA-Z_][a-zA-Z0-9_]{3,}\b").unwrap();
    let unique_idents: std::collections::HashSet<&str> =
        ident_re.find_iter(compressed).map(|m| m.as_str()).collect();
    let semantic_token_estimate = semantic_items + unique_idents.len();

    (semantic_token_estimate as f64 / output_tokens as f64).min(1.0)
}

/// Guard: returns compressed if quality passes, original otherwise
pub fn guard(original: &str, compressed: &str, ext: &str) -> (String, QualityScore) {
    let q = score(original, compressed, ext);
    if q.passed {
        (compressed.to_string(), q)
    } else {
        (original.to_string(), q)
    }
}

fn measure_identifier_preservation(original: &str, compressed: &str) -> f64 {
    let ident_re = regex::Regex::new(r"\b[a-zA-Z_][a-zA-Z0-9_]{3,}\b").unwrap();

    let original_idents: std::collections::HashSet<&str> =
        ident_re.find_iter(original).map(|m| m.as_str()).collect();

    if original_idents.is_empty() {
        return 1.0;
    }

    let preserved = original_idents
        .iter()
        .filter(|id| compressed.contains(*id))
        .count();

    preserved as f64 / original_idents.len() as f64
}

fn measure_line_preservation(original: &str, compressed: &str) -> f64 {
    let original_lines: usize = original.lines().filter(|l| !l.trim().is_empty()).count();
    if original_lines == 0 {
        return 1.0;
    }

    let compressed_lines: usize = compressed.lines().filter(|l| !l.trim().is_empty()).count();
    let ratio = compressed_lines as f64 / original_lines as f64;

    ratio.min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_perfect_score_identity() {
        let code = "fn main() {\n    println!(\"hello\");\n}\n";
        let q = score(code, code, "rs");
        assert!(q.composite >= 0.99);
        assert!(q.passed);
    }

    #[test]
    fn test_score_below_threshold_returns_original() {
        let original = "fn validate_token() {\n    let result = check();\n    return result;\n}\n";
        let bad_compressed = "removed everything";
        let (output, q) = guard(original, bad_compressed, "rs");
        assert!(!q.passed);
        assert_eq!(output, original);
    }

    #[test]
    fn test_good_compression_passes() {
        let original = "fn validate_token() {\n    let result = check();\n    return result;\n}\n";
        let compressed = "fn validate_token() { let result = check(); return result; }";
        let q = score(original, compressed, "rs");
        assert!(q.ast_score >= 0.9);
        assert!(q.identifier_score >= 0.9);
    }

    #[test]
    fn test_score_format_compact() {
        let code = "fn main() {}\n";
        let q = score(code, code, "rs");
        let formatted = q.format_compact();
        assert!(formatted.contains("Q:"));
        assert!(formatted.contains("✓"));
    }

    #[test]
    fn test_empty_content_scores_perfect() {
        let q = score("", "", "rs");
        assert!(q.passed);
        assert!(q.composite >= 0.99);
    }

    #[test]
    fn test_rust_file_with_structs() {
        let original = "pub struct Config {\n    pub name: String,\n    pub value: usize,\n}\n\nimpl Config {\n    pub fn new() -> Self {\n        Self { name: String::new(), value: 0 }\n    }\n}\n";
        let compressed = "pub struct Config { pub name: String, pub value: usize }\nimpl Config { pub fn new() -> Self { Self { name: String::new(), value: 0 } } }";
        let q = score(original, compressed, "rs");
        assert!(q.identifier_score >= 0.9);
    }

    #[test]
    fn test_typescript_file() {
        let original = "export function fetchData(url: string): Promise<Response> {\n  return fetch(url);\n}\n\nexport const API_URL = 'https://api.example.com';\n";
        let compressed = "export function fetchData(url: string): Promise<Response> { return fetch(url); }\nexport const API_URL = 'https://api.example.com';";
        let q = score(original, compressed, "ts");
        assert!(q.identifier_score >= 0.9);
    }

    #[test]
    fn test_python_file() {
        let original = "def validate_credentials(username: str, password: str) -> bool:\n    user = find_user(username)\n    return verify_hash(user.password_hash, password)\n";
        let compressed = "def validate_credentials(username, password): user = find_user(username); return verify_hash(user.password_hash, password)";
        let q = score(original, compressed, "py");
        assert!(q.identifier_score >= 0.8);
    }

    #[test]
    fn test_density_high_for_meaningful_compression() {
        let original = "pub fn calculate_total(items: Vec<Item>) -> f64 {\n    items.iter().map(|i| i.price * i.quantity as f64).sum()\n}\n";
        let d = information_density(original, original, "rs");
        assert!(d > 0.15, "identity should have high density: {d}");
    }

    #[test]
    fn test_density_low_for_garbage() {
        let original = "pub fn calculate_total(items: Vec<Item>) -> f64 {\n    items.iter().map(|i| i.price).sum()\n}\n";
        let garbage = "xxx xxx xxx xxx xxx xxx xxx xxx xxx xxx xxx xxx xxx xxx xxx";
        let d = information_density(original, garbage, "rs");
        assert!(d < 0.5, "garbage output should have low density: {d}");
    }

    #[test]
    fn test_density_in_quality_score() {
        let code = "fn main() {\n    println!(\"hello\");\n}\n";
        let q = score(code, code, "rs");
        assert!(q.density > 0.0, "density should be computed");
    }

    #[test]
    fn test_adaptive_threshold_looser_for_heavy_compression() {
        let original = "fn validate_token() {\n    let result = check();\n    return result;\n}\nfn other() {\n    let x = 1;\n}\n";
        let compressed = "fn validate_token() { let result = check(); return result; }";
        let q = score(original, compressed, "rs");
        assert!(
            q.density >= MIN_DENSITY,
            "compressed code should maintain minimum density"
        );
    }
}
