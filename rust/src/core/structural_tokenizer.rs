//! Structural tokenizer treating idiomatic multi-token spans as single motifs.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    Identifier,
    Operator,
    Literal,
    Pattern,
    Noise,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructuralToken {
    pub kind: TokenKind,
    pub text: String,
    pub weight: f64,
}

const W_PATTERN: f64 = 3.0;
const W_KEYWORD: f64 = 2.0;
const W_LITERAL: f64 = 1.5;
const W_IDENTIFIER: f64 = 1.0;
const W_OPERATOR: f64 = 0.8;
const W_NOISE: f64 = 0.15;

fn for_in_rust_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"for\s+[a-zA-Z_][a-zA-Z0-9_]*\s+in\s+").expect("for-in regex"))
}

fn keywords_for(lang: &str) -> &'static HashSet<&'static str> {
    static RUST: OnceLock<HashSet<&str>> = OnceLock::new();
    static GO: OnceLock<HashSet<&str>> = OnceLock::new();
    static GENERIC: OnceLock<HashSet<&str>> = OnceLock::new();

    match lang {
        "rust" | "rs" => RUST.get_or_init(|| {
            HashSet::from([
                "pub", "fn", "let", "mut", "struct", "enum", "impl", "trait", "use", "mod",
                "crate", "super", "self", "where", "type", "const", "static", "async", "await",
                "match", "if", "else", "for", "while", "loop", "break", "continue", "return",
                "unsafe", "move", "ref", "dyn", "extern", "in", "as",
            ])
        }),
        "go" => GO.get_or_init(|| {
            HashSet::from([
                "func",
                "package",
                "import",
                "var",
                "const",
                "type",
                "struct",
                "interface",
                "map",
                "chan",
                "defer",
                "go",
                "select",
                "switch",
                "case",
                "default",
                "if",
                "else",
                "for",
                "range",
                "return",
                "break",
                "continue",
                "fallthrough",
                "nil",
                "make",
                "new",
                "len",
                "cap",
            ])
        }),
        _ => GENERIC.get_or_init(|| {
            HashSet::from([
                "if", "else", "for", "while", "return", "fn", "func", "let", "var", "const", "pub",
                "import", "class", "def",
            ])
        }),
    }
}

fn try_pattern(rest: &str, lang: &str) -> Option<(usize, String)> {
    let ascii_patterns: &[(&str, &[&str])] = &[
        ("if err != nil", &["go"]),
        ("pub async fn", &["rust", "rs"]),
        ("async fn", &["rust", "rs"]),
        ("pub fn", &["rust", "rs"]),
        ("fn main()", &["rust", "rs", "generic", ""]),
        ("match ", &["rust", "rs"]),
    ];

    for (pat, langs) in ascii_patterns {
        if !langs.iter().any(|&l| l == lang || l.is_empty()) {
            continue;
        }
        if rest.starts_with(pat) {
            return Some((pat.len(), (*pat).to_string()));
        }
    }

    if (lang == "rust" || lang == "rs")
        && let Some(m) = for_in_rust_re().find(rest)
        && m.start() == 0
    {
        return Some((m.end(), m.as_str().to_string()));
    }

    None
}

fn skip_line_comment(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

fn skip_block_comment(bytes: &[u8], mut i: usize) -> Option<usize> {
    if i + 1 >= bytes.len() || bytes[i] != b'/' || bytes[i + 1] != b'*' {
        return None;
    }
    i += 2;
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            return Some(i + 2);
        }
        i += 1;
    }
    Some(bytes.len())
}

fn scan_string(bytes: &[u8], quote: u8, mut i: usize) -> usize {
    i += 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if b == quote {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

fn scan_raw_string(bytes: &[u8], i: usize) -> usize {
    if i + 1 >= bytes.len() || bytes[i] != b'r' {
        return i;
    }
    let mut j = i + 1;
    let mut hashes = 0usize;
    while j < bytes.len() && bytes[j] == b'#' {
        hashes += 1;
        j += 1;
    }
    if j >= bytes.len() || bytes[j] != b'"' {
        return i;
    }
    j += 1;
    while j < bytes.len() {
        if bytes[j] == b'"' {
            let mut k = j + 1;
            let mut ok = true;
            for _ in 0..hashes {
                if k >= bytes.len() || bytes[k] != b'#' {
                    ok = false;
                    break;
                }
                k += 1;
            }
            if ok && hashes == 0 {
                return k;
            }
            if ok {
                return k;
            }
        }
        j += 1;
    }
    bytes.len()
}

fn scan_number(bytes: &[u8], mut i: usize) -> usize {
    let start = i;
    if bytes.get(i) == Some(&b'0') && bytes.get(i + 1).is_some_and(|b| *b == b'x' || *b == b'X') {
        i += 2;
        while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
            i += 1;
        }
        return i.max(start + 1);
    }
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_' || bytes[i] == b'.') {
        i += 1;
    }
    if bytes.get(i) == Some(&b'e') || bytes.get(i) == Some(&b'E') {
        i += 1;
        if bytes.get(i) == Some(&b'+') || bytes.get(i) == Some(&b'-') {
            i += 1;
        }
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    i.max(start + 1)
}

fn scan_identifier(bytes: &[u8], mut i: usize) -> usize {
    let start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    i.max(start + 1)
}

fn push_op(out: &mut Vec<StructuralToken>, text: &str) {
    out.push(StructuralToken {
        kind: TokenKind::Operator,
        text: text.to_string(),
        weight: W_OPERATOR,
    });
}

/// Tokenize source into weighted structural tokens (motifs, keywords, literals, …).
#[must_use]
pub fn structural_tokenize(code: &str, lang: &str) -> Vec<StructuralToken> {
    let lang_lower = lang.to_lowercase();
    let lang_k = match lang_lower.as_str() {
        "rust" | "rs" => "rust",
        "go" | "golang" => "go",
        _ => "generic",
    };

    let kw = keywords_for(lang_k);
    let bytes = code.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::new();

    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if start != i {
                out.push(StructuralToken {
                    kind: TokenKind::Noise,
                    text: code[start..i].to_string(),
                    weight: W_NOISE,
                });
            }
            continue;
        }

        let rest = &code[i..];
        if let Some((len, text)) = try_pattern(rest, lang_k) {
            out.push(StructuralToken {
                kind: TokenKind::Pattern,
                text,
                weight: W_PATTERN,
            });
            i += len;
            continue;
        }

        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
            let start = i;
            i = skip_line_comment(bytes, i);
            out.push(StructuralToken {
                kind: TokenKind::Noise,
                text: code[start..i].to_string(),
                weight: W_NOISE,
            });
            continue;
        }

        if let Some(next) = skip_block_comment(bytes, i) {
            let start = i;
            i = next;
            out.push(StructuralToken {
                kind: TokenKind::Noise,
                text: code[start..i].to_string(),
                weight: W_NOISE,
            });
            continue;
        }

        if lang_k == "rust"
            && bytes[i] == b'r'
            && (bytes.get(i + 1) == Some(&b'#') || bytes.get(i + 1) == Some(&b'"'))
        {
            let start = i;
            i = scan_raw_string(bytes, i);
            out.push(StructuralToken {
                kind: TokenKind::Literal,
                text: code[start..i].to_string(),
                weight: W_LITERAL,
            });
            continue;
        }

        if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            let start = i;
            i = scan_string(bytes, quote, i);
            out.push(StructuralToken {
                kind: TokenKind::Literal,
                text: code[start..i].to_string(),
                weight: W_LITERAL,
            });
            continue;
        }

        if bytes[i].is_ascii_digit() {
            let start = i;
            i = scan_number(bytes, i);
            out.push(StructuralToken {
                kind: TokenKind::Literal,
                text: code[start..i].to_string(),
                weight: W_LITERAL,
            });
            continue;
        }

        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            i = scan_identifier(bytes, i);
            let word = &code[start..i];
            let kind = if kw.contains(word) {
                TokenKind::Keyword
            } else {
                TokenKind::Identifier
            };
            let weight = if kind == TokenKind::Keyword {
                W_KEYWORD
            } else {
                W_IDENTIFIER
            };
            out.push(StructuralToken {
                kind,
                text: word.to_string(),
                weight,
            });
            continue;
        }

        let two = i + 1 < bytes.len();
        if two {
            let pair = [bytes[i], bytes[i + 1]];
            let s = std::str::from_utf8(&pair).unwrap_or("??");
            match pair {
                [b'!' | b'=' | b'<' | b'>' | b'+' | b'-', b'=']
                | [b'-' | b'=', b'>']
                | [b':', b':']
                | [b'&', b'&']
                | [b'|', b'|'] => {
                    push_op(&mut out, s);
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }

        let ch = bytes[i] as char;
        push_op(&mut out, &ch.to_string());
        i += 1;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_pub_fn_pattern() {
        let toks = structural_tokenize("pub fn foo() {}", "rust");
        assert_eq!(toks[0].kind, TokenKind::Pattern);
        assert_eq!(toks[0].text, "pub fn");
        assert_eq!(toks[0].weight, W_PATTERN);
    }

    #[test]
    fn rust_async_fn_pattern() {
        let toks = structural_tokenize("pub async fn bar() {}", "rust");
        assert!(
            toks.iter()
                .any(|t| t.kind == TokenKind::Pattern && t.text.starts_with("pub async fn")),
            "{toks:?}"
        );
    }

    #[test]
    fn rust_match_pattern_prefix() {
        let toks = structural_tokenize("match x {", "rust");
        assert_eq!(toks[0].kind, TokenKind::Pattern);
        assert_eq!(toks[0].text, "match ");
    }

    #[test]
    fn rust_for_in_loop_pattern() {
        let src = "for item in items.iter() {";
        let toks = structural_tokenize(src, "rust");
        assert!(
            toks.iter()
                .any(|t| t.kind == TokenKind::Pattern && t.text.starts_with("for "))
        );
    }

    #[test]
    fn go_err_nil_pattern() {
        let toks = structural_tokenize("if err != nil { return err }", "go");
        assert!(
            toks.iter()
                .any(|t| t.kind == TokenKind::Pattern && t.text.contains("err"))
        );
        let pat = toks
            .iter()
            .find(|t| t.kind == TokenKind::Pattern)
            .expect("pattern");
        assert_eq!(pat.text, "if err != nil");
        assert_eq!(pat.weight, W_PATTERN);
    }

    #[test]
    fn weights_pattern_above_identifier() {
        let toks = structural_tokenize("pub fn main() {}", "rust");
        let p = toks.iter().find(|t| t.kind == TokenKind::Pattern).unwrap();
        let id = toks
            .iter()
            .find(|t| t.kind == TokenKind::Identifier && t.text == "main")
            .unwrap();
        assert!(p.weight > id.weight);
        assert!(p.weight > W_KEYWORD);
    }

    #[test]
    fn comment_is_noise() {
        let toks = structural_tokenize("// hello\nlet x = 1;", "rust");
        assert!(
            toks.iter()
                .any(|t| t.kind == TokenKind::Noise && t.text.starts_with("//"))
        );
        assert!(
            toks.iter()
                .any(|t| t.kind == TokenKind::Keyword && t.text == "let")
        );
    }

    #[test]
    fn string_literal_kind() {
        let toks = structural_tokenize(r#"let s = "ab";"#, "rust");
        let lit = toks
            .iter()
            .find(|t| t.kind == TokenKind::Literal && t.text.starts_with('"'));
        assert!(lit.is_some());
        assert_eq!(lit.unwrap().weight, W_LITERAL);
    }
}
