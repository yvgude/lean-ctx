//! Token-optimal encoding based on empirical lab results.
//!
//! Uses a lookup table (concept -> optimal representation) derived from
//! Experiment C's cross-tokenizer analysis. Falls back to identity when
//! no optimizations are known.

use std::collections::HashMap;
use std::path::Path;

pub struct TokenOptimizer {
    replacements: HashMap<String, String>,
}

// Lab Experiment C (2026-04-02): Unicode symbols (λ, →, §, ∂, ⊕) INCREASE token count
// on GPT-4/GPT-4o tokenizers. English keywords already encode as 1 token each.
// Only use ASCII abbreviations that tokenizers handle well.
const DEFAULT_OPTIMIZATIONS: &[(&str, &str)] = &[
    ("function ", "fn "),
    ("boolean", "bool"),
    ("string", "str"),
    ("number", "num"),
    ("undefined", "undef"),
    ("console.log", "log"),
    ("export function ", "fn "),
    ("    ", "  "),
    ("Result<T, E>", "Result"),
    ("Result<T,E>", "Result"),
    ("Option<T>", "Option"),
    ("Vec<String>", "Vec"),
    ("Vec<&str>", "Vec"),
    ("Vec<u8>", "Vec"),
    ("HashMap<String, String>", "HashMap"),
    ("HashMap<K, V>", "HashMap"),
    ("HashMap<K,V>", "HashMap"),
    ("BTreeMap<K, V>", "BTreeMap"),
    ("HashSet<String>", "HashSet"),
    ("Box<dyn Error>", "Box<Error>"),
    ("Arc<Mutex<", "Arc<Mutex<"),
    ("std::collections::HashMap", "HashMap"),
    ("std::collections::HashSet", "HashSet"),
    ("std::collections::BTreeMap", "BTreeMap"),
    ("std::path::PathBuf", "PathBuf"),
    ("std::path::Path", "Path"),
    ("std::sync::Arc", "Arc"),
    ("std::sync::Mutex", "Mutex"),
    ("std::io::Result", "io::Result"),
    ("std::fmt::Display", "Display"),
    ("std::fmt::Debug", "Debug"),
];

impl TokenOptimizer {
    pub fn load_or_default(model_dir: &Path) -> Self {
        let config_path = model_dir.join("token_optimizer.json");
        if config_path.exists() {
            match Self::load_from_file(&config_path) {
                Ok(opt) => {
                    tracing::info!(
                        "Token optimizer loaded ({} rules) from {:?}",
                        opt.replacements.len(),
                        config_path,
                    );
                    return opt;
                }
                Err(e) => {
                    tracing::warn!("Failed to load token optimizer: {e}. Using defaults.");
                }
            }
        }

        Self::with_defaults()
    }

    pub fn with_defaults() -> Self {
        let replacements: HashMap<String, String> = DEFAULT_OPTIMIZATIONS
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        Self { replacements }
    }

    fn load_from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let data: HashMap<String, String> = serde_json::from_str(&content)?;
        Ok(Self { replacements: data })
    }

    pub fn optimize<'a>(&'a self, _concept: &str, representation: &'a str) -> &'a str {
        representation
    }

    pub fn optimize_line(&self, line: &str) -> String {
        let mut result = line.to_string();
        for (from, to) in &self.replacements {
            result = result.replace(from.as_str(), to.as_str());
        }
        result = elide_lifetimes(&result);
        result
    }

    pub fn optimize_block(&self, content: &str) -> String {
        let optimized: Vec<String> = content
            .lines()
            .map(|line| self.optimize_line(line))
            .collect();
        let collapsed = collapse_closing_braces(&optimized);
        collapsed.join("\n")
    }

    pub fn replacement_count(&self) -> usize {
        self.replacements.len()
    }
}

fn elide_lifetimes(line: &str) -> String {
    let mut result = line.to_string();
    let patterns = ["'a ", "'b ", "'c ", "'static "];
    for pat in &patterns {
        if *pat == "'static " {
            continue;
        }
        let with_ref = format!("&{pat}");
        let with_mut = format!("&{pat}mut ");
        result = result.replace(&with_mut, "&mut ");
        result = result.replace(&with_ref, "&");
    }
    result
}

fn collapse_closing_braces(lines: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::with_capacity(lines.len());
    let mut brace_run = 0u32;

    for line in lines {
        let trimmed = line.trim();
        if matches!(trimmed, "}" | "};" | ");" | "});" | ")") {
            brace_run += 1;
            if brace_run <= 2 {
                result.push(trimmed.to_string());
            } else if brace_run == 3 {
                if let Some(last) = result.last_mut() {
                    last.push_str(trimmed);
                }
            }
            continue;
        }
        brace_run = 0;
        result.push(line.clone());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_optimizations_apply() {
        let opt = TokenOptimizer::with_defaults();
        assert_eq!(opt.optimize_line("function hello() {"), "fn hello() {");
        assert_eq!(opt.optimize_line("boolean flag"), "bool flag");
    }

    #[test]
    fn indentation_compresses() {
        let opt = TokenOptimizer::with_defaults();
        let input = "    let x = 1;";
        let output = opt.optimize_line(input);
        assert_eq!(output, "  let x = 1;");
    }

    #[test]
    fn generic_types_simplify() {
        let opt = TokenOptimizer::with_defaults();
        assert_eq!(
            opt.optimize_line("fn foo() -> Result<T, E>"),
            "fn foo() -> Result"
        );
        assert_eq!(
            opt.optimize_line("fn bar() -> Option<T>"),
            "fn bar() -> Option"
        );
        assert_eq!(
            opt.optimize_line("let v: Vec<String> = vec![]"),
            "let v: Vec = vec![]"
        );
        assert_eq!(
            opt.optimize_line("use std::collections::HashMap;"),
            "use HashMap;"
        );
    }

    #[test]
    fn multiline_optimization() {
        let opt = TokenOptimizer::with_defaults();
        let input = "function hello() {\n    return 42;\n}";
        let output = opt.optimize_block(input);
        assert_eq!(output, "fn hello() {\n  return 42;\n}");
    }

    #[test]
    fn lifetime_elision() {
        let opt = TokenOptimizer::with_defaults();
        assert_eq!(
            opt.optimize_line("fn foo(&'a str) -> &'a str"),
            "fn foo(&str) -> &str"
        );
        assert_eq!(opt.optimize_line("fn bar(&'a mut Vec)"), "fn bar(&mut Vec)");
        assert_eq!(
            opt.optimize_line("fn baz(&'static str)"),
            "fn baz(&'static str)",
            "'static must not be elided"
        );
    }

    #[test]
    fn closing_brace_collapsing() {
        let opt = TokenOptimizer::with_defaults();
        let input = "fn main() {\n  inner() {\n    x\n  }\n}\n}\n}\n}\nfn next() {}";
        let output = opt.optimize_block(input);
        assert!(output.contains("fn next()"), "code after braces preserved");
        let brace_only_lines: Vec<_> = output.lines().filter(|l| l.trim() == "}").collect();
        assert!(
            brace_only_lines.len() <= 2,
            "should collapse 4+ closing braces"
        );
    }

    #[test]
    fn std_path_shortening() {
        let opt = TokenOptimizer::with_defaults();
        assert_eq!(opt.optimize_line("use std::path::PathBuf;"), "use PathBuf;");
        assert_eq!(opt.optimize_line("use std::sync::Arc;"), "use Arc;");
    }
}
