use std::sync::OnceLock;

use regex::Regex;

use crate::core::config::{Config, SecretDetectionConfig};

macro_rules! static_regex {
    ($pattern:expr_2021) => {{
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new($pattern).expect(concat!("invalid regex: ", $pattern)))
    }};
}

#[derive(Debug, Clone)]
pub struct SecretMatch {
    pub pattern_name: &'static str,
    pub line_number: usize,
    pub redacted_preview: String,
}

fn aws_key_re() -> &'static Regex {
    static_regex!(r"AKIA[0-9A-Z]{16}")
}

fn private_key_re() -> &'static Regex {
    static_regex!(r"-----BEGIN (?:RSA |EC |DSA |OPENSSH )?PRIVATE KEY-----")
}

fn github_token_re() -> &'static Regex {
    static_regex!(r"gh[ps]_[A-Za-z0-9_]{36,}")
}

fn anthropic_key_re() -> &'static Regex {
    static_regex!(r"sk-ant-[A-Za-z0-9_\-]{20,}")
}

fn openai_key_re() -> &'static Regex {
    static_regex!(r"sk-[A-Za-z0-9]{20,}")
}

// #718 word boundaries for the generic key/value patterns: the keyword must
// not be a camelCase subword (`superuserPassword`), while SNAKE_CASE env
// names (`MY_TOKEN=…`) keep matching — `_` stays a permitted predecessor.
// Group 1 = predecessor + key prefix (kept on redaction), group 2 = value
// (guarded: identifier references and placeholders are benign).
// #1830: blanks only around the separator — a valueless `token:` line must not
// take the next line for its value.
fn generic_api_key_re() -> &'static Regex {
    static_regex!(
        r#"(?im)((?:^|[^a-z0-9])(?:api[_-]?key|secret[_-]?key|token|password|passwd|access[_-]?token|client[_-]?secret)[ \t]*[=:][ \t]*)(['"]?[a-zA-Z0-9_\-]{20,})"#
    )
}

fn high_entropy_b64_re() -> &'static Regex {
    static_regex!(
        r#"(?im)((?:^|[^a-z0-9])(?:key|token|secret|password|credential|auth)[ \t]*[=:][ \t]*)(['"]?[A-Za-z0-9+/=\-_]{40,})"#
    )
}

fn gitlab_pat_re() -> &'static Regex {
    static_regex!(r"glpat-[A-Za-z0-9_\-]{20,}")
}

fn jwt_re() -> &'static Regex {
    static_regex!(r"eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}")
}

fn slack_token_re() -> &'static Regex {
    static_regex!(r"xox[bpas]-[0-9a-zA-Z\-]{10,}")
}

fn stripe_key_re() -> &'static Regex {
    static_regex!(r"[sr]k_live_[0-9a-zA-Z]{10,}")
}

fn db_url_re() -> &'static Regex {
    static_regex!(r"(?:postgres|mysql|mongodb|redis)://[^\s]+:[^\s]+@")
}

fn npm_token_re() -> &'static Regex {
    static_regex!(r"npm_[A-Za-z0-9]{10,}")
}

fn github_fine_grained_re() -> &'static Regex {
    static_regex!(r"github_pat_[A-Za-z0-9_]{20,}")
}

const BUILTIN_PATTERNS: &[(&str, fn() -> &'static Regex)] = &[
    ("aws_key", aws_key_re),
    ("private_key", private_key_re),
    ("github_token", github_token_re),
    ("github_fine_grained", github_fine_grained_re),
    ("anthropic_key", anthropic_key_re),
    ("openai_key", openai_key_re),
    ("gitlab_pat", gitlab_pat_re),
    ("jwt", jwt_re),
    ("slack_token", slack_token_re),
    ("stripe_key", stripe_key_re),
    ("db_url", db_url_re),
    ("npm_token", npm_token_re),
];

/// Key/value patterns with a kept prefix (group 1) and a guarded value
/// (group 2): identifier references, placeholders and language literals are
/// benign (#718) and neither reported nor redacted.
const GUARDED_PATTERNS: &[(&str, fn() -> &'static Regex)] = &[
    ("generic_api_key", generic_api_key_re),
    ("high_entropy_secret", high_entropy_b64_re),
];

/// True when a guarded-pattern match should be skipped: the value is benign
/// (#718) — shares the benign-value heuristics with `core::redaction` so the
/// two layers cannot drift.
fn guarded_match_is_benign(caps: &regex::Captures) -> bool {
    caps.get(2)
        .is_some_and(|value| crate::core::redaction::is_benign_secret_value(value.as_str()))
}

/// Compile the subtractive `exclude_patterns` from config (#718). Invalid
/// regexes are skipped — a broken exclude must never disable detection.
fn compile_excludes(patterns: &[String]) -> Vec<Regex> {
    patterns.iter().filter_map(|p| Regex::new(p).ok()).collect()
}

fn excluded(excludes: &[Regex], matched: &str) -> bool {
    excludes.iter().any(|ex| ex.is_match(matched))
}

fn make_redacted_preview(matched: &str) -> String {
    let chars: Vec<char> = matched.chars().collect();
    if chars.len() <= 6 {
        return "***".to_string();
    }
    let prefix: String = chars[..4].iter().collect();
    let suffix: String = chars[chars.len() - 2..].iter().collect();
    format!("{prefix}***{suffix}")
}

fn collect_matches(
    content: &str,
    custom_patterns: &[String],
    excludes: &[Regex],
) -> Vec<SecretMatch> {
    let mut matches = Vec::new();

    let line_offsets: Vec<usize> = std::iter::once(0)
        .chain(content.match_indices('\n').map(|(i, _)| i + 1))
        .collect();

    let offset_to_line = |byte_offset: usize| -> usize {
        match line_offsets.binary_search(&byte_offset) {
            Ok(i) => i + 1,
            Err(i) => i,
        }
    };

    for &(name, regex_fn) in BUILTIN_PATTERNS {
        let re = regex_fn();
        for m in re.find_iter(content) {
            if excluded(excludes, m.as_str()) {
                continue;
            }
            matches.push(SecretMatch {
                pattern_name: name,
                line_number: offset_to_line(m.start()),
                redacted_preview: make_redacted_preview(m.as_str()),
            });
        }
    }

    for &(name, regex_fn) in GUARDED_PATTERNS {
        let re = regex_fn();
        for caps in re.captures_iter(content) {
            let whole = caps.get(0).map_or("", |m| m.as_str());
            if guarded_match_is_benign(&caps) || excluded(excludes, whole) {
                continue;
            }
            let start = caps.get(0).map_or(0, |m| m.start());
            matches.push(SecretMatch {
                pattern_name: name,
                line_number: offset_to_line(start),
                redacted_preview: make_redacted_preview(whole),
            });
        }
    }

    for pattern_str in custom_patterns {
        if let Ok(re) = Regex::new(pattern_str) {
            for m in re.find_iter(content) {
                if excluded(excludes, m.as_str()) {
                    continue;
                }
                matches.push(SecretMatch {
                    pattern_name: "custom_pattern",
                    line_number: offset_to_line(m.start()),
                    redacted_preview: make_redacted_preview(m.as_str()),
                });
            }
        }
    }

    matches
}

pub fn detect_secrets(content: &str) -> Vec<SecretMatch> {
    collect_matches(content, &[], &[])
}

pub fn detect_secrets_with_custom(content: &str, custom_patterns: &[String]) -> Vec<SecretMatch> {
    collect_matches(content, custom_patterns, &[])
}

pub fn scan_and_redact(
    content: &str,
    config: &SecretDetectionConfig,
) -> (String, Vec<SecretMatch>) {
    if !config.enabled {
        return (content.to_string(), Vec::new());
    }

    let excludes = compile_excludes(&config.exclude_patterns);
    let matches = collect_matches(content, &config.custom_patterns, &excludes);

    if matches.is_empty() || !config.redact {
        return (content.to_string(), matches);
    }

    let mut redacted = content.to_string();
    for &(name, regex_fn) in BUILTIN_PATTERNS {
        let re = regex_fn();
        redacted = re
            .replace_all(&redacted, |caps: &regex::Captures| {
                let whole = caps.get(0).map_or("", |m| m.as_str());
                if excluded(&excludes, whole) {
                    return whole.to_string();
                }
                format!("[REDACTED:{name}]")
            })
            .to_string();
    }

    for &(name, regex_fn) in GUARDED_PATTERNS {
        let re = regex_fn();
        redacted = re
            .replace_all(&redacted, |caps: &regex::Captures| {
                let whole = caps.get(0).map_or("", |m| m.as_str());
                if guarded_match_is_benign(caps) || excluded(&excludes, whole) {
                    return whole.to_string();
                }
                let prefix = caps.get(1).map_or("", |m| m.as_str());
                format!("{prefix}[REDACTED:{name}]")
            })
            .to_string();
    }

    for pattern_str in &config.custom_patterns {
        if let Ok(re) = Regex::new(pattern_str) {
            redacted = re
                .replace_all(&redacted, |caps: &regex::Captures| {
                    let whole = caps.get(0).map_or("", |m| m.as_str());
                    if excluded(&excludes, whole) {
                        return whole.to_string();
                    }
                    "[REDACTED:custom_pattern]".to_string()
                })
                .to_string();
        }
    }

    (redacted, matches)
}

pub fn scan_and_redact_from_config(content: &str) -> (String, Vec<SecretMatch>) {
    let cfg = Config::load();
    scan_and_redact(content, &cfg.secret_detection)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn detects_aws_key() {
        let input = concat!("aws_key = AK", "IAIOSFODNN7EXAMPLE");
        let matches = detect_secrets(input);
        assert!(matches.iter().any(|m| m.pattern_name == "aws_key"));
    }

    #[test]
    fn detects_private_key_header() {
        let input = concat!("-----BEGIN RSA PRIVATE", " KEY-----\nMIIEpAIB...");
        let matches = detect_secrets(input);
        assert!(matches.iter().any(|m| m.pattern_name == "private_key"));
    }

    #[test]
    fn detects_github_token() {
        let input = concat!(
            "export GITHUB_TOKEN=gh",
            "p_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijk"
        );
        let matches = detect_secrets(input);
        assert!(matches.iter().any(|m| m.pattern_name == "github_token"));
    }

    #[test]
    fn detects_anthropic_key() {
        let input = concat!("ANTHROPIC_API_KEY=sk-ant-api03-", "abcdef1234567890ABCD");
        let matches = detect_secrets(input);
        assert!(matches.iter().any(|m| m.pattern_name == "anthropic_key"));
    }

    #[test]
    fn detects_openai_key() {
        let input = concat!("OPENAI_API_KEY=sk-", "abcdefghijklmnopqrstuvwx");
        let matches = detect_secrets(input);
        assert!(matches.iter().any(|m| m.pattern_name == "openai_key"));
    }

    #[test]
    fn detects_gitlab_pat() {
        let input = concat!("token = glpat-", "xxxxxxxxxxxxxxxxxxxx");
        let matches = detect_secrets(input);
        assert!(matches.iter().any(|m| m.pattern_name == "gitlab_pat"));
    }

    #[test]
    fn detects_generic_api_key() {
        let input = concat!("api_key = abcdefghijklmnop", "qrstuvwxyz1234567890");
        let matches = detect_secrets(input);
        assert!(matches.iter().any(
            |m| m.pattern_name == "generic_api_key" || m.pattern_name == "high_entropy_secret"
        ));
    }

    #[test]
    fn clean_content_returns_empty() {
        let input = "fn main() { println!(\"hello world\"); }";
        let matches = detect_secrets(input);
        assert!(matches.is_empty());
    }

    #[test]
    fn detects_jwt() {
        let input = concat!("token = eyJhbGciOiJIU", "zI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkw");
        let matches = detect_secrets(input);
        assert!(matches.iter().any(|m| m.pattern_name == "jwt"));
    }

    #[test]
    fn detects_slack_token() {
        let input = concat!("SLACK_TOKEN=xoxb-12345", "67890-abcdefghij");
        let matches = detect_secrets(input);
        assert!(matches.iter().any(|m| m.pattern_name == "slack_token"));
    }

    #[test]
    fn detects_stripe_key() {
        let input = concat!("stripe_key = sk_", "live_abcdefghij1234567890");
        let matches = detect_secrets(input);
        assert!(matches.iter().any(|m| m.pattern_name == "stripe_key"));
    }

    #[test]
    fn detects_db_url() {
        let input = concat!(
            "DATABASE_URL=postgres://user:pass",
            "word@localhost:5432/db"
        );
        let matches = detect_secrets(input);
        assert!(matches.iter().any(|m| m.pattern_name == "db_url"));
    }

    #[test]
    fn detects_npm_token() {
        let input = concat!("NPM_TOKEN=npm_abcd", "efghij1234567890");
        let matches = detect_secrets(input);
        assert!(matches.iter().any(|m| m.pattern_name == "npm_token"));
    }

    #[test]
    fn detects_github_fine_grained() {
        let input = concat!("token = github_pat_ABC", "DEFGHIJKLMNOPQRSTuv");
        let matches = detect_secrets(input);
        assert!(
            matches
                .iter()
                .any(|m| m.pattern_name == "github_fine_grained")
        );
    }

    #[test]
    fn redacted_preview_format() {
        let preview = make_redacted_preview(concat!("AK", "IAIOSFODNN7EXAMPLE"));
        assert!(preview.starts_with("AKIA"));
        assert!(preview.ends_with("LE"));
        assert!(preview.contains("***"));
    }

    #[test]
    fn redacted_preview_short_string() {
        let preview = make_redacted_preview("short");
        assert_eq!(preview, "***");
    }

    #[test]
    fn scan_and_redact_replaces_secrets() {
        let cfg = SecretDetectionConfig {
            enabled: true,
            redact: true,
            ..Default::default()
        };
        let input = concat!("key = AK", "IAIOSFODNN7EXAMPLE");
        let (redacted, matches) = scan_and_redact(input, &cfg);
        assert!(!matches.is_empty());
        assert!(redacted.contains("[REDACTED:aws_key]"));
        assert!(!redacted.contains(concat!("AK", "IAIOSFODNN7EXAMPLE")));
    }

    #[test]
    fn scan_without_redact_preserves_content() {
        let cfg = SecretDetectionConfig {
            enabled: true,
            redact: false,
            ..Default::default()
        };
        let input = concat!("key = AK", "IAIOSFODNN7EXAMPLE");
        let (output, matches) = scan_and_redact(input, &cfg);
        assert!(!matches.is_empty());
        assert_eq!(output, input);
    }

    #[test]
    fn disabled_detection_returns_unchanged() {
        let cfg = SecretDetectionConfig {
            enabled: false,
            redact: true,
            ..Default::default()
        };
        let input = concat!("key = AK", "IAIOSFODNN7EXAMPLE");
        let (output, matches) = scan_and_redact(input, &cfg);
        assert!(matches.is_empty());
        assert_eq!(output, input);
    }

    #[test]
    fn custom_pattern_detected() {
        let cfg = SecretDetectionConfig {
            enabled: true,
            redact: true,
            custom_patterns: vec![r"MYCORP_[A-Z]{10,}".to_string()],
            ..Default::default()
        };
        let input = "value is MYCORP_ABCDEFGHIJKLMNO here";
        let (redacted, matches) = scan_and_redact(input, &cfg);
        assert!(matches.iter().any(|m| m.pattern_name == "custom_pattern"));
        assert!(redacted.contains("[REDACTED:custom_pattern]"));
    }

    #[test]
    fn line_numbers_are_correct() {
        let input = concat!("line1\nline2\nAK", "IAIOSFODNN7EXAMPLE\nline4");
        let matches = detect_secrets(input);
        assert!(!matches.is_empty());
        assert_eq!(matches[0].line_number, 3);
    }

    #[test]
    fn multiple_secrets_on_different_lines() {
        let input = concat!(
            "AK",
            "IAIOSFODNN7EXAMPLE\nclean\nsk-abcdef",
            "ghijklmnopqrstuvwx"
        );
        let matches = detect_secrets(input);
        assert!(matches.len() >= 2);
        let aws = matches
            .iter()
            .find(|m| m.pattern_name == "aws_key")
            .unwrap();
        assert_eq!(aws.line_number, 1);
        let oai = matches
            .iter()
            .find(|m| m.pattern_name == "openai_key")
            .unwrap();
        assert_eq!(oai.line_number, 3);
    }

    #[test]
    fn ec_private_key_detected() {
        let input = concat!("-----BEGIN EC PRIVATE", " KEY-----");
        let matches = detect_secrets(input);
        assert!(matches.iter().any(|m| m.pattern_name == "private_key"));
    }

    #[test]
    fn openssh_private_key_detected() {
        let input = concat!("-----BEGIN OPENSSH PRIVATE", " KEY-----");
        let matches = detect_secrets(input);
        assert!(matches.iter().any(|m| m.pattern_name == "private_key"));
    }
}
