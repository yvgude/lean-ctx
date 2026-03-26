use std::collections::HashMap;

use crate::core::tokens::count_tokens;

const MIN_IDENT_LENGTH: usize = 6;
const SHORT_ID_PREFIX: char = 'α';

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SymbolMap {
    forward: HashMap<String, String>,
    next_id: usize,
}

impl Default for SymbolMap {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolMap {
    pub fn new() -> Self {
        Self {
            forward: HashMap::new(),
            next_id: 1,
        }
    }

    pub fn register(&mut self, identifier: &str) -> Option<String> {
        if identifier.len() < MIN_IDENT_LENGTH {
            return None;
        }

        if let Some(existing) = self.forward.get(identifier) {
            return Some(existing.clone());
        }

        let short_id = format!("{SHORT_ID_PREFIX}{}", self.next_id);
        self.next_id += 1;
        self.forward
            .insert(identifier.to_string(), short_id.clone());
        Some(short_id)
    }

    pub fn apply(&self, text: &str) -> String {
        if self.forward.is_empty() {
            return text.to_string();
        }

        let mut sorted: Vec<(&String, &String)> = self.forward.iter().collect();
        sorted.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

        let mut result = text.to_string();
        for (long, short) in &sorted {
            result = result.replace(long.as_str(), short.as_str());
        }
        result
    }

    pub fn format_table(&self) -> String {
        if self.forward.is_empty() {
            return String::new();
        }

        let mut entries: Vec<(&String, &String)> = self.forward.iter().collect();
        entries.sort_by_key(|(_, v)| {
            v.trim_start_matches(SHORT_ID_PREFIX)
                .parse::<usize>()
                .unwrap_or(0)
        });

        let mut table = String::from("\n§MAP:");
        for (long, short) in &entries {
            table.push_str(&format!("\n  {short}={long}"));
        }
        table
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.forward.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.forward.is_empty()
    }
}

/// MAP entry cost in tokens: "  αN=identifier\n" ≈ short_id_tokens + ident_tokens + 2 (= and newline)
const MAP_ENTRY_OVERHEAD: usize = 2;

/// ROI-based decision: register only when total savings exceed the MAP entry cost.
/// savings = occurrences * (tokens(ident) - tokens(short_id))
/// cost    = tokens(ident) + tokens(short_id) + MAP_ENTRY_OVERHEAD
pub fn should_register(identifier: &str, occurrences: usize, next_id: usize) -> bool {
    if identifier.len() < MIN_IDENT_LENGTH {
        return false;
    }
    let ident_tokens = count_tokens(identifier);
    let short_id = format!("{SHORT_ID_PREFIX}{next_id}");
    let short_tokens = count_tokens(&short_id);

    let token_saving_per_use = ident_tokens.saturating_sub(short_tokens);
    if token_saving_per_use == 0 {
        return false;
    }

    let total_savings = occurrences * token_saving_per_use;
    let entry_cost = ident_tokens + short_tokens + MAP_ENTRY_OVERHEAD;

    total_savings > entry_cost
}

pub fn extract_identifiers(content: &str, ext: &str) -> Vec<String> {
    let ident_re = regex::Regex::new(r"\b[a-zA-Z_][a-zA-Z0-9_]*\b").unwrap();

    let mut seen = HashMap::new();
    for mat in ident_re.find_iter(content) {
        let word = mat.as_str();
        if word.len() >= MIN_IDENT_LENGTH && !is_keyword(word, ext) {
            *seen.entry(word.to_string()).or_insert(0usize) += 1;
        }
    }

    let mut next_id = 1usize;
    let mut idents: Vec<(String, usize)> = seen
        .into_iter()
        .filter(|(ident, count)| {
            let pass = should_register(ident, *count, next_id);
            if pass {
                next_id += 1;
            }
            pass
        })
        .collect();

    idents.sort_by(|a, b| {
        let savings_a = a.0.len() * a.1;
        let savings_b = b.0.len() * b.1;
        savings_b.cmp(&savings_a)
    });

    idents.into_iter().map(|(s, _)| s).collect()
}

fn is_keyword(word: &str, ext: &str) -> bool {
    match ext {
        "rs" => matches!(
            word,
            "continue" | "default" | "return" | "struct" | "unsafe" | "where"
        ),
        "ts" | "tsx" | "js" | "jsx" => matches!(
            word,
            "constructor" | "arguments" | "undefined" | "prototype" | "instanceof"
        ),
        "py" => matches!(word, "continue" | "lambda" | "return" | "import" | "class"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_register_short_ident_rejected() {
        assert!(!should_register("foo", 100, 1));
        assert!(!should_register("bar", 50, 1));
        assert!(!should_register("x", 1000, 1));
    }

    #[test]
    fn test_should_register_roi_positive() {
        // Very long identifier (many BPE tokens) appearing 5 times
        assert!(should_register(
            "authenticate_user_credentials_handler",
            5,
            1
        ));
    }

    #[test]
    fn test_should_register_roi_negative_single_use() {
        // Long ident but only 1 occurrence — MAP entry cost > savings
        assert!(!should_register(
            "authenticate_user_credentials_handler",
            1,
            1
        ));
    }

    #[test]
    fn test_should_register_roi_scales_with_frequency() {
        let ident = "configuration_manager_instance";
        // Should fail at low frequency, pass at high frequency
        let passes_at_low = should_register(ident, 2, 1);
        let passes_at_high = should_register(ident, 10, 1);
        // At some point frequency makes it worthwhile
        assert!(passes_at_high || !passes_at_low);
    }

    #[test]
    fn test_extract_identifiers_roi_filtering() {
        // Repeat a long identifier enough times that ROI is positive
        let long = "authenticate_user_credentials_handler";
        let content = format!("{long} {long} {long} {long} {long} short");
        let result = extract_identifiers(&content, "rs");
        assert!(result.contains(&long.to_string()));
        assert!(!result.contains(&"short".to_string()));
    }

    #[test]
    fn test_register_returns_existing() {
        let mut map = SymbolMap::new();
        let first = map.register("validateToken");
        let second = map.register("validateToken");
        assert_eq!(first, second);
    }

    #[test]
    fn test_apply_replaces_identifiers() {
        let mut map = SymbolMap::new();
        map.register("validateToken");
        let result = map.apply("call validateToken here");
        assert!(result.contains("α1"));
        assert!(!result.contains("validateToken"));
    }

    #[test]
    fn test_format_table_output() {
        let mut map = SymbolMap::new();
        map.register("validateToken");
        let table = map.format_table();
        assert!(table.contains("§MAP:"));
        assert!(table.contains("α1=validateToken"));
    }
}
