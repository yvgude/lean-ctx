use std::path::Path;

use crate::core::graph_provider::SymbolInfo;
use crate::core::property_graph::CodeGraph;
use crate::core::protocol;
use crate::core::tokens::count_tokens;

/// Search for symbols using the FTS5 symbols_fts table.
/// Returns None when FTS5 is unavailable.
fn try_fts_symbol_search(
    name: &str,
    file: Option<&str>,
    kind: Option<&str>,
    project_root: &str,
) -> Option<Vec<SymbolInfo>> {
    use rusqlite::params;

    let graph = CodeGraph::open(project_root).ok()?;
    let conn = graph.connection();

    // Tokenize the search name: split on non-alphanumeric, quote each token
    let tokens: Vec<String> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\""))
        .collect();
    if tokens.is_empty() {
        return None;
    }
    let fts_query = tokens.join(" AND ");

    // Query symbols_fts with JOIN to nodes table for line info and metadata.
    // symbols_fts rowids match the corresponding nodes rowid (both inserted in
    // the same order during mirror), but we LEFT JOIN to handle mismatch.
    let sql = "\
        SELECT s.file_path, s.name, s.label, \
               COALESCE(n.line_start, 0), COALESCE(n.line_end, 0), \
               COALESCE(n.metadata, '{}') \
        FROM symbols_fts s \
        LEFT JOIN nodes n ON n.rowid = s.rowid \
        WHERE symbols_fts MATCH ?1";

    let mut stmt = conn.prepare(sql).ok()?;

    let results: Vec<SymbolInfo> = stmt
        .query_map(params![fts_query], |row| {
            let metadata_str: String = row.get(5)?;
            let is_exported = metadata_str.contains("\"exported\":true");
            Ok(SymbolInfo {
                file: row.get(0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
                start_line: row.get::<_, i64>(3)? as usize,
                end_line: row.get::<_, i64>(4)? as usize,
                is_exported,
            })
        })
        .ok()?
        .filter_map(std::result::Result::ok)
        .filter(|s| {
            if let Some(f) = file
                && !s.file.contains(f)
            {
                return false;
            }
            if let Some(k) = kind
                && s.kind != k
            {
                return false;
            }
            true
        })
        .collect();

    if results.is_empty() {
        None
    } else {
        Some(results)
    }
}

/// Render the body of the single most relevant symbol named `name`.
/// Used by `ctx_compose` to inline the top symbol's definition. Returns
/// `(rendered_with_body, full_file_tokens)` or `None` when not found.
pub fn best_symbol_snippet(name: &str, project_root: &str) -> Option<(String, usize)> {
    let sym = try_fts_symbol_search(name, None, None, project_root)?
        .into_iter()
        .next()?;
    Some(render_single(&sym, project_root))
}

fn render_single(sym: &SymbolInfo, project_root: &str) -> (String, usize) {
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
    let header = format!(
        "{}:{}  {}  {}{}",
        sym.file, sym.start_line, sym.kind, vis, sym.name,
    );

    let savings = protocol::format_savings(full_tokens, snippet_tokens);

    (format!("{header}\n\n{snippet}\n{savings}"), full_tokens)
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
