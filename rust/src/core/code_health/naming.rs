//! Naming-quality heuristic.
//!
//! Cryptic identifiers force agents (and humans) into exhaustive search instead
//! of targeted lookups — the article's `normalize_query` vs `_xfm_q2` example.
//! This check is **deliberately conservative**: only clearly non-descriptive
//! function names are reported, keeping the signal high and false positives near
//! zero. Pure + deterministic so it is safe for read-time annotation (#498).

use serde::Serialize;

/// A function whose name is judged cryptic, with a human-readable reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NamingFinding {
    pub name: String,
    /// 1-based start line of the function.
    pub line: usize,
    pub message: String,
}

/// Report cryptic function names in `source` of the given file `extension`.
/// Returns `None` when tree-sitter is disabled or the extension is unsupported.
pub fn naming_findings(source: &str, extension: &str) -> Option<Vec<NamingFinding>> {
    #[cfg(feature = "tree-sitter")]
    {
        let mut out: Vec<NamingFinding> = Vec::new();
        super::astutil::for_each_function(source, extension, |fn_node, name, _src| {
            if let Some(message) = cryptic_reason(name) {
                let line = fn_node.start_position().row.saturating_add(1);
                out.push(NamingFinding {
                    name: name.to_string(),
                    line,
                    message,
                });
            }
        })?;
        out.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.name.cmp(&b.name)));
        out.dedup();
        Some(out)
    }
    #[cfg(not(feature = "tree-sitter"))]
    {
        let _ = (source, extension);
        None
    }
}

/// Returns a reason string if `name` is cryptic, else `None`. Pure and
/// unit-tested; this is the single source of truth for the heuristic.
pub fn cryptic_reason(name: &str) -> Option<String> {
    let core = name.trim_start_matches('_');
    if core.is_empty() || core == "<anonymous>" {
        return None;
    }
    if is_allowed(core) {
        return None;
    }
    let len = core.chars().count();
    if len <= 2 {
        return Some(format!("name `{name}` is too short to convey intent"));
    }
    if !has_vowel(core) && !is_known_acronym(core) {
        return Some(format!(
            "name `{name}` has no vowels — likely a cryptic abbreviation"
        ));
    }
    None
}

/// Short identifiers that are idiomatic enough to never flag.
fn is_allowed(core: &str) -> bool {
    matches!(
        core.to_ascii_lowercase().as_str(),
        "id" | "ok" | "io" | "db" | "ui" | "os" | "vm" | "fn" | "go" | "rx" | "tx" | "fd" | "ip"
    )
}

/// Common consonant-only acronyms that are clear despite lacking vowels.
fn is_known_acronym(core: &str) -> bool {
    matches!(
        core.to_ascii_lowercase().as_str(),
        "db" | "js"
            | "ts"
            | "css"
            | "html"
            | "http"
            | "https"
            | "url"
            | "uri"
            | "sql"
            | "xml"
            | "json"
            | "jwt"
            | "rpc"
            | "grpc"
            | "tcp"
            | "udp"
            | "ip"
            | "dns"
            | "fs"
            | "os"
            | "vm"
            | "csv"
            | "pdf"
            | "png"
            | "jpg"
            | "svg"
            | "md5"
            | "sha"
            | "crc"
    )
}

fn has_vowel(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u' | 'y'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_vowelless_abbreviation() {
        assert!(cryptic_reason("_xfm_q2").is_some());
        assert!(cryptic_reason("qstr").is_some());
    }

    #[test]
    fn flags_too_short() {
        assert!(cryptic_reason("zq").is_some());
        assert!(cryptic_reason("x").is_some());
    }

    #[test]
    fn accepts_descriptive_names() {
        assert!(cryptic_reason("normalize_query").is_none());
        assert!(cryptic_reason("parse").is_none());
        assert!(cryptic_reason("handleRequest").is_none());
    }

    #[test]
    fn accepts_known_short_and_acronyms() {
        assert!(cryptic_reason("id").is_none());
        assert!(cryptic_reason("db").is_none());
        assert!(cryptic_reason("to_json").is_none());
        assert!(cryptic_reason("http").is_none());
    }

    #[cfg(feature = "tree-sitter")]
    #[test]
    fn finds_cryptic_function_in_source() {
        let src = "fn _xfm_q2(a: i32) -> i32 { a }\nfn normalize_query(b: i32) -> i32 { b }\n";
        let findings = naming_findings(src, "rs").unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].name, "_xfm_q2");
    }

    #[cfg(feature = "tree-sitter")]
    #[test]
    fn deterministic_across_runs() {
        let src = "fn zq() {}\nfn ab() {}\n";
        assert_eq!(naming_findings(src, "rs"), naming_findings(src, "rs"));
    }
}
