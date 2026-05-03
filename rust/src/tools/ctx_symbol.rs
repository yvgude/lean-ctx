use std::path::Path;

use crate::core::graph_index::{self, ProjectIndex, SymbolEntry};
use crate::core::protocol;
use crate::core::tokens::count_tokens;

pub fn handle(
    name: &str,
    file: Option<&str>,
    kind: Option<&str>,
    project_root: &str,
) -> (String, usize) {
    let index = graph_index::load_or_build(project_root);

    let matches = find_symbols(&index, name, file, kind);

    if matches.is_empty() {
        return (
            format!(
                "Symbol '{name}' not found in index ({} symbols indexed). \
                 Try ctx_search(pattern=\"{name}\") for a broader search.",
                index.symbol_count()
            ),
            0,
        );
    }

    if matches.len() == 1 {
        return render_single(matches[0], &index, project_root);
    }

    if matches.len() <= 5 {
        return render_multiple(&matches, &index, project_root);
    }

    let mut out = format!(
        "{} matches for '{name}'. Narrow with file= or kind=:\n",
        matches.len()
    );
    for m in matches.iter().take(20) {
        out.push_str(&format!(
            "  {}::{} ({}:L{}-{})\n",
            m.file, m.name, m.kind, m.start_line, m.end_line
        ));
    }
    if matches.len() > 20 {
        out.push_str(&format!("  ... and {} more\n", matches.len() - 20));
    }
    (out, 0)
}

fn find_symbols<'a>(
    index: &'a ProjectIndex,
    name: &str,
    file_filter: Option<&str>,
    kind_filter: Option<&str>,
) -> Vec<&'a SymbolEntry> {
    let name_lower = name.to_lowercase();
    let mut results: Vec<&SymbolEntry> = index
        .symbols
        .values()
        .filter(|s| {
            let s_lower = s.name.to_lowercase();
            let name_match = s_lower == name_lower
                || s_lower.ends_with(&name_lower)
                || s_lower.starts_with(&format!("{name_lower}::"))
                || s_lower.contains(&format!("::{name_lower}"));

            let file_match = file_filter.is_none_or(|f| s.file.contains(f));

            let kind_match = kind_filter.is_none_or(|k| s.kind.to_lowercase() == k.to_lowercase());

            name_match && file_match && kind_match
        })
        .collect();

    results.sort_by(|a, b| {
        let a_exact = a.name.to_lowercase() == name_lower;
        let b_exact = b.name.to_lowercase() == name_lower;
        b_exact.cmp(&a_exact).then_with(|| a.file.cmp(&b.file))
    });

    results
}

fn render_single(sym: &SymbolEntry, index: &ProjectIndex, project_root: &str) -> (String, usize) {
    let abs_path = resolve_file_path(&sym.file, project_root);

    let Ok(content) = std::fs::read_to_string(&abs_path) else {
        return (
            format!(
                "Symbol '{}' found at {}:L{}-{} but file unreadable",
                sym.name, sym.file, sym.start_line, sym.end_line
            ),
            0,
        );
    };

    let lines: Vec<&str> = content.lines().collect();
    let start = sym.start_line.saturating_sub(1);
    let end = sym.end_line.min(lines.len());
    let snippet: String = lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>4}|{}", start + i + 1, line))
        .collect::<Vec<_>>()
        .join("\n");

    let full_tokens = count_tokens(&content);
    let snippet_tokens = count_tokens(&snippet);

    let vis = if sym.is_exported { "+" } else { "-" };
    let header = format!(
        "{}::{} ({} {}, L{}-{})",
        sym.file, sym.name, vis, sym.kind, sym.start_line, sym.end_line
    );

    let file_info = index.files.get(&sym.file);
    let ctx = if let Some(f) = file_info {
        format!(
            "File: {} ({} lines, {} tokens)",
            sym.file, f.line_count, f.token_count
        )
    } else {
        format!("File: {}", sym.file)
    };

    let savings = protocol::format_savings(full_tokens, snippet_tokens);

    (
        format!("{header}\n{ctx}\n\n{snippet}\n{savings}"),
        full_tokens,
    )
}

fn render_multiple(
    symbols: &[&SymbolEntry],
    index: &ProjectIndex,
    project_root: &str,
) -> (String, usize) {
    let mut out = String::new();
    let mut total_original = 0usize;

    for (i, sym) in symbols.iter().enumerate() {
        if i > 0 {
            out.push_str("\n---\n\n");
        }
        let (rendered, orig) = render_single(sym, index, project_root);
        out.push_str(&rendered);
        total_original = total_original.max(orig);
    }

    (out, total_original)
}

fn resolve_file_path(relative: &str, project_root: &str) -> String {
    let p = Path::new(relative);
    if p.is_absolute() && p.exists() {
        return relative.to_string();
    }
    let joined = Path::new(project_root).join(relative);
    if joined.exists() {
        return joined.to_string_lossy().to_string();
    }
    relative.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph_index::{ProjectIndex, SymbolEntry};

    fn test_index() -> ProjectIndex {
        let mut index = ProjectIndex::new("/tmp/test");
        index.symbols.insert(
            "src/main.rs::main".to_string(),
            SymbolEntry {
                file: "src/main.rs".to_string(),
                name: "main".to_string(),
                kind: "fn".to_string(),
                start_line: 1,
                end_line: 10,
                is_exported: false,
            },
        );
        index.symbols.insert(
            "src/lib.rs::Config".to_string(),
            SymbolEntry {
                file: "src/lib.rs".to_string(),
                name: "Config".to_string(),
                kind: "struct".to_string(),
                start_line: 5,
                end_line: 20,
                is_exported: true,
            },
        );
        index.symbols.insert(
            "src/lib.rs::Config::load".to_string(),
            SymbolEntry {
                file: "src/lib.rs".to_string(),
                name: "Config::load".to_string(),
                kind: "method".to_string(),
                start_line: 22,
                end_line: 35,
                is_exported: true,
            },
        );
        index
    }

    #[test]
    fn find_exact_match() {
        let index = test_index();
        let results = find_symbols(&index, "main", None, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "main");
    }

    #[test]
    fn find_with_kind_filter() {
        let index = test_index();
        let results = find_symbols(&index, "Config", None, Some("struct"));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "struct");
    }

    #[test]
    fn find_with_file_filter() {
        let index = test_index();
        let results = find_symbols(&index, "Config", Some("lib.rs"), None);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn no_match_returns_empty() {
        let index = test_index();
        let results = find_symbols(&index, "nonexistent", None, None);
        assert!(results.is_empty());
    }
}
