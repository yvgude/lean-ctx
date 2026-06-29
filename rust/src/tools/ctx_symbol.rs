use std::path::Path;

use crate::core::graph_provider::{self, FileInfo, GraphProvider, SymbolInfo};
use crate::core::protocol;
use crate::core::tokens::count_tokens;

pub fn handle(
    name: &str,
    file: Option<&str>,
    kind: Option<&str>,
    project_root: &str,
) -> (String, usize) {
    let Some(open) = graph_provider::open_or_build(project_root) else {
        return (
            format!(
                "Symbol '{name}' not found (no graph available). \
                 Try ctx_search(pattern=\"{name}\") for a broader search.",
            ),
            0,
        );
    };
    let gp = &open.provider;

    let matches = gp.find_symbols(name, file, kind);

    if matches.is_empty() {
        return (
            format!(
                "Symbol '{name}' not found in index ({} symbols indexed). \
                 Try ctx_search(pattern=\"{name}\") for a broader search.",
                gp.symbol_count()
            ),
            0,
        );
    }

    if matches.len() == 1 {
        return render_single(&matches[0], gp, project_root);
    }

    if matches.len() <= 5 {
        return render_multiple(&matches, gp, project_root);
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

/// Render the body of the single most relevant symbol named `name`.
/// Used by `ctx_compose` to inline the top symbol's definition. Returns
/// `(rendered_with_body, full_file_tokens)` or `None` when no graph/symbol.
pub fn best_symbol_snippet(name: &str, project_root: &str) -> Option<(String, usize)> {
    let open = graph_provider::open_or_build(project_root)?;
    let gp = &open.provider;
    let sym = gp.find_symbols(name, None, None).into_iter().next()?;
    Some(render_single(&sym, gp, project_root))
}

fn render_single(sym: &SymbolInfo, gp: &GraphProvider, project_root: &str) -> (String, usize) {
    let abs_path = resolve_file_path(&sym.file, project_root);

    if let Err(e) = crate::core::pathjail::jail_path(
        std::path::Path::new(&abs_path),
        std::path::Path::new(project_root),
    ) {
        return (
            format!("Symbol '{}': path blocked by jail: {e}", sym.name),
            0,
        );
    }

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
    let cc_note = symbol_cc_note(&content, &sym.file, &sym.name, sym.start_line);
    let header = format!(
        "{}::{} ({} {}, L{}-{}){cc_note}",
        sym.file, sym.name, vis, sym.kind, sym.start_line, sym.end_line
    );

    let file_info: Option<FileInfo> = gp.get_file_entry(&sym.file);
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
    symbols: &[SymbolInfo],
    gp: &GraphProvider,
    project_root: &str,
) -> (String, usize) {
    let mut out = String::new();
    let mut total_original = 0usize;

    for (i, sym) in symbols.iter().enumerate() {
        if i > 0 {
            out.push_str("\n---\n\n");
        }
        let (rendered, orig) = render_single(sym, gp, project_root);
        out.push_str(&rendered);
        total_original = total_original.max(orig);
    }

    (out, total_original)
}

/// Optional ` · cc=NN` suffix for a symbol header — the code-health complexity
/// of the function being shown (#1084). Computed fresh from the already-read
/// file content, so it's exact for *any* symbol. Over-threshold functions are
/// flagged `(over)`. Honors the `code_health.annotate_reads` opt-out and is
/// empty for non-functions / when tree-sitter is off.
fn symbol_cc_note(content: &str, file: &str, name: &str, start_line: usize) -> String {
    let cfg = crate::core::config::Config::load();
    if !cfg.code_health.annotate_reads {
        return String::new();
    }
    let ext = Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match crate::core::code_health::annotate::cognitive_for_symbol(content, ext, name, start_line) {
        Some(cc) if cc > cfg.code_health.cognitive_threshold => format!(" · cc={cc} (over)"),
        Some(cc) => format!(" · cc={cc}"),
        None => String::new(),
    }
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

    fn test_provider() -> GraphProvider {
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
        GraphProvider::GraphIndex(index)
    }

    #[test]
    fn find_exact_match() {
        let gp = test_provider();
        let results = gp.find_symbols("main", None, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "main");
    }

    #[test]
    fn find_with_kind_filter() {
        let gp = test_provider();
        let results = gp.find_symbols("Config", None, Some("struct"));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "struct");
    }

    #[test]
    fn find_with_file_filter() {
        let gp = test_provider();
        let results = gp.find_symbols("Config", Some("lib.rs"), None);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn no_match_returns_empty() {
        let gp = test_provider();
        let results = gp.find_symbols("nonexistent", None, None);
        assert!(results.is_empty());
    }
}
