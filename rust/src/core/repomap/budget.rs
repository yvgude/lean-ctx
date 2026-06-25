//! Token budget fitting for repo map output.
//!
//! Selects the top-ranked symbols that fit within a token budget
//! and formats them as a compact, readable listing.

use crate::core::repomap::ranking::RankedSymbol;
use crate::core::tokens;

/// Format ranked symbols into a repo map string within the token budget.
///
/// Returns a formatted string showing the most important symbols,
/// grouped by file, sorted by descending rank.
#[must_use]
pub fn fit_to_budget(symbols: &[RankedSymbol], max_tokens: usize) -> String {
    if symbols.is_empty() {
        return "No symbols found.".to_string();
    }

    let mut output = String::with_capacity(4096);
    let mut current_tokens = 0;
    let mut current_file = "";
    let mut included_count = 0;
    let mut skipped_count = 0;

    // Header
    let header = "# Repo Map (ranked by structural importance)\n\n";
    let header_tokens = tokens::count_tokens(header);
    if header_tokens >= max_tokens {
        return truncated_output(symbols, 5);
    }
    output.push_str(header);
    current_tokens += header_tokens;

    // Reserve tokens for footer
    let footer_reserve = 30;
    let effective_budget = max_tokens.saturating_sub(footer_reserve);

    for sym in symbols {
        let line = format_symbol_line(sym);
        let line_tokens = tokens::count_tokens(&line);

        if current_tokens + line_tokens > effective_budget {
            skipped_count += 1;
            continue;
        }

        // File header if file changed
        if sym.def.file != current_file {
            let file_header = format!("\n## {}\n", sym.def.file);
            let fh_tokens = tokens::count_tokens(&file_header);
            if current_tokens + fh_tokens + line_tokens > effective_budget {
                skipped_count += 1;
                continue;
            }
            output.push_str(&file_header);
            current_tokens += fh_tokens;
            current_file = &sym.def.file;
        }

        output.push_str(&line);
        current_tokens += line_tokens;
        included_count += 1;
    }

    // Footer
    if skipped_count > 0 {
        let footer = format!(
            "\n---\n{included_count} symbols shown, {skipped_count} omitted (token budget: {max_tokens})\n"
        );
        output.push_str(&footer);
    } else {
        let footer = format!("\n---\n{included_count} symbols shown\n");
        output.push_str(&footer);
    }

    output
}

fn format_symbol_line(sym: &RankedSymbol) -> String {
    let line = sym.def.line;
    let kind = compact_kind(&sym.def.kind);
    format!("  L{line:<5} {kind:<6} {}\n", sym.def.signature)
}

fn compact_kind(kind: &str) -> &str {
    match kind {
        "fn" | "function" | "method" => "fn",
        "struct" => "struct",
        "class" => "class",
        "interface" => "iface",
        "trait" => "trait",
        "enum" => "enum",
        "type" | "TypeAlias" | "type_alias" => "type",
        "export" => "exp",
        "const" | "constant" => "const",
        _ => kind,
    }
}

/// Minimal output when budget is extremely tight.
fn truncated_output(symbols: &[RankedSymbol], limit: usize) -> String {
    let mut out = String::from("Top symbols:\n");
    for sym in symbols.iter().take(limit) {
        out.push_str(&format!(
            "  {}:{} | {} | {}\n",
            sym.def.file, sym.def.line, sym.def.kind, sym.def.name
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::repomap::graph::SymbolDef;

    fn ranked(name: &str, file: &str, score: f64) -> RankedSymbol {
        RankedSymbol {
            def: SymbolDef {
                name: name.into(),
                kind: "fn".into(),
                file: file.into(),
                line: 10,
                end_line: 20,
                is_exported: true,
                signature: format!("fn {name}()"),
            },
            score,
        }
    }

    #[test]
    fn empty_symbols_returns_message() {
        let result = fit_to_budget(&[], 1000);
        assert_eq!(result, "No symbols found.");
    }

    #[test]
    fn includes_symbols_within_budget() {
        let symbols = vec![ranked("foo", "a.rs", 0.5), ranked("bar", "b.rs", 0.3)];
        let result = fit_to_budget(&symbols, 2000);
        assert!(result.contains("foo"));
        assert!(result.contains("bar"));
        assert!(result.contains("2 symbols shown"));
    }

    #[test]
    fn respects_token_budget() {
        let symbols: Vec<RankedSymbol> = (0..100)
            .map(|i| {
                ranked(
                    &format!("sym_{i}"),
                    &format!("file_{i}.rs"),
                    1.0 - i as f64 * 0.01,
                )
            })
            .collect();

        let result = fit_to_budget(&symbols, 200);
        assert!(result.contains("omitted"));
    }

    #[test]
    fn groups_by_file() {
        let symbols = vec![
            ranked("foo", "a.rs", 0.5),
            ranked("bar", "a.rs", 0.4),
            ranked("baz", "b.rs", 0.3),
        ];
        let result = fit_to_budget(&symbols, 2000);
        assert!(result.contains("## a.rs"));
        assert!(result.contains("## b.rs"));
    }

    #[test]
    fn compact_kind_maps_correctly() {
        assert_eq!(compact_kind("fn"), "fn");
        assert_eq!(compact_kind("function"), "fn");
        assert_eq!(compact_kind("method"), "fn");
        assert_eq!(compact_kind("struct"), "struct");
        assert_eq!(compact_kind("interface"), "iface");
        assert_eq!(compact_kind("trait"), "trait");
        assert_eq!(compact_kind("unknown"), "unknown");
    }

    #[test]
    fn very_small_budget_uses_truncated_output() {
        let symbols = vec![ranked("foo", "a.rs", 0.5)];
        let result = fit_to_budget(&symbols, 5);
        assert!(result.contains("Top symbols:"));
    }
}
