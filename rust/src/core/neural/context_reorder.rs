use super::attention_learned::LearnedAttention;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LineCategory {
    ErrorHandling,
    Import,
    TypeDefinition,
    FunctionSignature,
    Logic,
    ClosingBrace,
    Empty,
}

pub struct CategorizedLine<'a> {
    pub line: &'a str,
    pub category: LineCategory,
    pub original_index: usize,
}

pub fn categorize_line(line: &str) -> LineCategory {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return LineCategory::Empty;
    }

    if is_error_handling(trimmed) {
        return LineCategory::ErrorHandling;
    }

    if is_import(trimmed) {
        return LineCategory::Import;
    }

    if is_type_def(trimmed) {
        return LineCategory::TypeDefinition;
    }

    if is_fn_signature(trimmed) {
        return LineCategory::FunctionSignature;
    }

    if is_closing(trimmed) {
        return LineCategory::ClosingBrace;
    }

    LineCategory::Logic
}

pub fn reorder_for_lcurve(content: &str, task_keywords: &[String]) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= 5 {
        return content.to_string();
    }

    let categorized: Vec<CategorizedLine> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| CategorizedLine {
            line,
            category: categorize_line(line),
            original_index: i,
        })
        .collect();

    let attention = LearnedAttention::with_defaults();
    let kw_lower: Vec<String> = task_keywords.iter().map(|k| k.to_lowercase()).collect();

    let mut scored: Vec<(&CategorizedLine, f64)> = categorized
        .iter()
        .map(|cl| {
            let base = category_priority(cl.category);
            let kw_boost: f64 = if !kw_lower.is_empty() {
                let line_lower = cl.line.to_lowercase();
                kw_lower
                    .iter()
                    .filter(|kw| line_lower.contains(kw.as_str()))
                    .count() as f64
                    * 0.5
            } else {
                0.0
            };
            let n = lines.len().max(1) as f64;
            let orig_pos = cl.original_index as f64 / n;
            let orig_attention = attention.weight(orig_pos);
            let score = base + kw_boost + orig_attention * 0.1;
            (cl, score)
        })
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    scored
        .iter()
        .filter(|(cl, _)| cl.category != LineCategory::Empty || cl.original_index == 0)
        .map(|(cl, _)| cl.line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn category_priority(cat: LineCategory) -> f64 {
    match cat {
        LineCategory::ErrorHandling => 5.0,
        LineCategory::Import => 4.0,
        LineCategory::TypeDefinition => 3.5,
        LineCategory::FunctionSignature => 3.0,
        LineCategory::Logic => 1.0,
        LineCategory::ClosingBrace => 0.2,
        LineCategory::Empty => 0.1,
    }
}

fn is_error_handling(line: &str) -> bool {
    line.starts_with("return Err(")
        || line.starts_with("Err(")
        || line.starts_with("bail!(")
        || line.contains(".map_err(")
        || line.starts_with("raise ")
        || line.starts_with("throw ")
        || line.starts_with("catch ")
        || line.starts_with("except ")
        || line.starts_with("panic!(")
        || line.contains("Error::")
}

fn is_import(line: &str) -> bool {
    line.starts_with("use ")
        || line.starts_with("import ")
        || line.starts_with("from ")
        || line.starts_with("#include")
        || line.starts_with("require(")
        || line.starts_with("const ") && line.contains("require(")
}

fn is_type_def(line: &str) -> bool {
    let starters = [
        "struct ",
        "pub struct ",
        "enum ",
        "pub enum ",
        "trait ",
        "pub trait ",
        "type ",
        "pub type ",
        "interface ",
        "export interface ",
        "class ",
        "export class ",
        "typedef ",
        "data ",
    ];
    starters.iter().any(|s| line.starts_with(s))
}

fn is_fn_signature(line: &str) -> bool {
    let starters = [
        "fn ",
        "pub fn ",
        "async fn ",
        "pub async fn ",
        "function ",
        "export function ",
        "async function ",
        "def ",
        "async def ",
        "func ",
        "pub(crate) fn ",
        "pub(super) fn ",
    ];
    starters.iter().any(|s| line.starts_with(s))
}

fn is_closing(line: &str) -> bool {
    matches!(line, "}" | "};" | ");" | "});" | ")" | "})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categorize_lines_correctly() {
        assert_eq!(categorize_line("use std::io;"), LineCategory::Import);
        assert_eq!(
            categorize_line("pub struct Foo {"),
            LineCategory::TypeDefinition
        );
        assert_eq!(
            categorize_line("fn main() {"),
            LineCategory::FunctionSignature
        );
        assert_eq!(
            categorize_line("return Err(e);"),
            LineCategory::ErrorHandling
        );
        assert_eq!(categorize_line("}"), LineCategory::ClosingBrace);
        assert_eq!(categorize_line("let x = 1;"), LineCategory::Logic);
        assert_eq!(categorize_line(""), LineCategory::Empty);
    }

    #[test]
    fn reorder_puts_errors_and_imports_first() {
        let content = "let x = 1;\nuse std::io;\n}\nreturn Err(e);\npub struct Foo {\nfn main() {";
        let result = reorder_for_lcurve(content, &[]);
        let lines: Vec<&str> = result.lines().collect();
        assert!(
            lines[0].contains("Err") || lines[0].contains("use "),
            "first line should be error handling or import, got: {}",
            lines[0]
        );
    }

    #[test]
    fn task_keywords_boost_relevant_lines() {
        let content = "fn unrelated() {\nlet x = 1;\n}\nfn validate_token() {\nlet y = 2;\n}";
        let result = reorder_for_lcurve(content, &["validate".to_string()]);
        let lines: Vec<&str> = result.lines().collect();
        let validate_pos = lines.iter().position(|l| l.contains("validate"));
        let unrelated_pos = lines.iter().position(|l| l.contains("unrelated"));
        if let (Some(v), Some(u)) = (validate_pos, unrelated_pos) {
            assert!(v < u, "validate should appear before unrelated");
        }
    }
}
