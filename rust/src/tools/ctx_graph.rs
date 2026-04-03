use std::collections::HashMap;
use std::path::Path;

use crate::core::graph_index::{self, ProjectIndex};
use crate::core::tokens::count_tokens;

pub fn handle(
    action: &str,
    path: Option<&str>,
    root: &str,
    cache: &mut crate::core::cache::SessionCache,
    crp_mode: crate::tools::CrpMode,
) -> String {
    match action {
        "build" => handle_build(root),
        "related" => handle_related(path, root),
        "symbol" => handle_symbol(path, root, cache, crp_mode),
        "impact" => handle_impact(path, root),
        "status" => handle_status(root),
        _ => "Unknown action. Use: build, related, symbol, impact, status".to_string(),
    }
}

fn handle_build(root: &str) -> String {
    let index = graph_index::scan(root);

    let mut by_lang: HashMap<&str, (usize, usize)> = HashMap::new();
    for entry in index.files.values() {
        let e = by_lang.entry(&entry.language).or_insert((0, 0));
        e.0 += 1;
        e.1 += entry.token_count;
    }

    let mut result = Vec::new();
    result.push(format!(
        "Project Graph: {} files, {} symbols, {} edges",
        index.file_count(),
        index.symbol_count(),
        index.edge_count()
    ));

    let mut langs: Vec<_> = by_lang.iter().collect();
    langs.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    result.push("\nLanguages:".to_string());
    for (lang, (count, tokens)) in &langs {
        result.push(format!("  {lang}: {count} files, {tokens} tok"));
    }

    let mut import_counts: HashMap<&str, usize> = HashMap::new();
    for edge in &index.edges {
        if edge.kind == "import" {
            *import_counts.entry(&edge.to).or_insert(0) += 1;
        }
    }
    let mut hotspots: Vec<_> = import_counts.iter().collect();
    hotspots.sort_by(|a, b| b.1.cmp(a.1));

    if !hotspots.is_empty() {
        result.push(format!("\nMost imported ({}):", hotspots.len().min(10)));
        for (module, count) in hotspots.iter().take(10) {
            result.push(format!("  {module}: imported by {count} files"));
        }
    }

    if let Some(dir) = ProjectIndex::index_dir(root) {
        result.push(format!(
            "\nIndex saved: {}",
            crate::core::protocol::shorten_path(&dir.to_string_lossy())
        ));
    }

    let output = result.join("\n");
    let tokens = count_tokens(&output);
    format!("{output}\n[ctx_graph build: {tokens} tok]")
}

fn handle_related(path: Option<&str>, root: &str) -> String {
    let target = match path {
        Some(p) => p,
        None => return "path is required for 'related' action".to_string(),
    };

    let index = match ProjectIndex::load(root) {
        Some(idx) => idx,
        None => {
            return "No graph index found. Run ctx_graph with action='build' first.".to_string()
        }
    };

    let rel_target = target
        .strip_prefix(root)
        .unwrap_or(target)
        .trim_start_matches('/');

    let related = index.get_related(rel_target, 2);
    if related.is_empty() {
        return format!(
            "No related files found for {}",
            crate::core::protocol::shorten_path(target)
        );
    }

    let mut result = format!(
        "Files related to {} ({}):\n",
        crate::core::protocol::shorten_path(target),
        related.len()
    );
    for r in &related {
        result.push_str(&format!("  {}\n", crate::core::protocol::shorten_path(r)));
    }

    let tokens = count_tokens(&result);
    format!("{result}[ctx_graph related: {tokens} tok]")
}

fn handle_symbol(
    path: Option<&str>,
    root: &str,
    cache: &mut crate::core::cache::SessionCache,
    crp_mode: crate::tools::CrpMode,
) -> String {
    let spec = match path {
        Some(p) => p,
        None => {
            return "path is required for 'symbol' action (format: file.rs::function_name)"
                .to_string()
        }
    };

    let (file_part, symbol_name) = match spec.split_once("::") {
        Some((f, s)) => (f, s),
        None => return format!("Invalid symbol spec '{spec}'. Use format: file.rs::function_name"),
    };

    let index = match ProjectIndex::load(root) {
        Some(idx) => idx,
        None => {
            return "No graph index found. Run ctx_graph with action='build' first.".to_string()
        }
    };

    let rel_file = file_part
        .strip_prefix(root)
        .unwrap_or(file_part)
        .trim_start_matches('/');

    let key = format!("{rel_file}::{symbol_name}");
    let symbol = match index.get_symbol(&key) {
        Some(s) => s,
        None => {
            let available: Vec<&str> = index
                .symbols
                .keys()
                .filter(|k| k.starts_with(rel_file))
                .map(|k| k.as_str())
                .take(10)
                .collect();
            if available.is_empty() {
                return format!("Symbol '{symbol_name}' not found in {rel_file}. Run ctx_graph action='build' to update the index.");
            }
            return format!(
                "Symbol '{symbol_name}' not found in {rel_file}.\nAvailable symbols:\n  {}",
                available.join("\n  ")
            );
        }
    };

    let abs_path = if Path::new(file_part).is_absolute() {
        file_part.to_string()
    } else {
        format!("{root}/{rel_file}")
    };

    let content = match std::fs::read_to_string(&abs_path) {
        Ok(c) => c,
        Err(e) => return format!("Cannot read {abs_path}: {e}"),
    };

    let lines: Vec<&str> = content.lines().collect();
    let start = symbol.start_line.saturating_sub(1);
    let end = symbol.end_line.min(lines.len());

    if start >= lines.len() {
        return crate::tools::ctx_read::handle(cache, &abs_path, "full", crp_mode);
    }

    let mut result = format!(
        "{}::{} ({}:{}-{})\n",
        crate::core::protocol::shorten_path(rel_file),
        symbol_name,
        symbol.kind,
        symbol.start_line,
        symbol.end_line
    );

    for (i, line) in lines[start..end].iter().enumerate() {
        result.push_str(&format!("{:>4}|{}\n", start + i + 1, line));
    }

    let tokens = count_tokens(&result);
    let full_tokens = count_tokens(&content);
    let saved = full_tokens.saturating_sub(tokens);
    let pct = if full_tokens > 0 {
        (saved as f64 / full_tokens as f64 * 100.0).round() as usize
    } else {
        0
    };

    format!("{result}[ctx_graph symbol: {tokens} tok (full file: {full_tokens} tok, -{pct}%)]")
}

fn file_path_to_module_prefixes(rel_path: &str, project_root: &str) -> Vec<String> {
    let without_ext = rel_path
        .strip_suffix(".rs")
        .or_else(|| rel_path.strip_suffix(".ts"))
        .or_else(|| rel_path.strip_suffix(".tsx"))
        .or_else(|| rel_path.strip_suffix(".js"))
        .or_else(|| rel_path.strip_suffix(".py"))
        .unwrap_or(rel_path);

    let module_path = without_ext
        .strip_prefix("src/")
        .unwrap_or(without_ext)
        .replace('/', "::");

    let module_path = if module_path.ends_with("::mod") {
        module_path
            .strip_suffix("::mod")
            .unwrap_or(&module_path)
            .to_string()
    } else {
        module_path
    };

    let crate_name = std::fs::read_to_string(Path::new(project_root).join("Cargo.toml"))
        .or_else(|_| std::fs::read_to_string(Path::new(project_root).join("package.json")))
        .ok()
        .and_then(|c| {
            c.lines()
                .find(|l| l.contains("\"name\"") || l.starts_with("name"))
                .and_then(|l| l.split('"').nth(1))
                .map(|n| n.replace('-', "_"))
        })
        .unwrap_or_default();

    let mut prefixes = vec![
        format!("crate::{module_path}"),
        format!("super::{module_path}"),
        module_path.clone(),
    ];
    if !crate_name.is_empty() {
        prefixes.insert(0, format!("{crate_name}::{module_path}"));
    }
    prefixes
}

fn edge_matches_file(edge_to: &str, module_prefixes: &[String]) -> bool {
    module_prefixes.iter().any(|prefix| {
        edge_to == *prefix
            || edge_to.starts_with(&format!("{prefix}::"))
            || edge_to.starts_with(&format!("{prefix},"))
    })
}

fn handle_impact(path: Option<&str>, root: &str) -> String {
    let target = match path {
        Some(p) => p,
        None => return "path is required for 'impact' action".to_string(),
    };

    let index = match ProjectIndex::load(root) {
        Some(idx) => idx,
        None => {
            return "No graph index found. Run ctx_graph with action='build' first.".to_string()
        }
    };

    let rel_target = target
        .strip_prefix(root)
        .unwrap_or(target)
        .trim_start_matches('/');

    let module_prefixes = file_path_to_module_prefixes(rel_target, root);

    let direct: Vec<&str> = index
        .edges
        .iter()
        .filter(|e| e.kind == "import" && edge_matches_file(&e.to, &module_prefixes))
        .map(|e| e.from.as_str())
        .collect();

    let mut all_dependents: Vec<String> = direct.iter().map(|s| s.to_string()).collect();
    for d in &direct {
        for dep in index.get_reverse_deps(d, 1) {
            if !all_dependents.contains(&dep) && dep != rel_target {
                all_dependents.push(dep);
            }
        }
    }

    if all_dependents.is_empty() {
        return format!(
            "No files depend on {}",
            crate::core::protocol::shorten_path(target)
        );
    }

    let mut result = format!(
        "Impact of {} ({} dependents):\n",
        crate::core::protocol::shorten_path(target),
        all_dependents.len()
    );

    if !direct.is_empty() {
        result.push_str(&format!("\nDirect ({}):\n", direct.len()));
        for d in &direct {
            result.push_str(&format!("  {}\n", crate::core::protocol::shorten_path(d)));
        }
    }

    let indirect: Vec<&String> = all_dependents
        .iter()
        .filter(|d| !direct.contains(&d.as_str()))
        .collect();
    if !indirect.is_empty() {
        result.push_str(&format!("\nIndirect ({}):\n", indirect.len()));
        for d in &indirect {
            result.push_str(&format!("  {}\n", crate::core::protocol::shorten_path(d)));
        }
    }

    let tokens = count_tokens(&result);
    format!("{result}[ctx_graph impact: {tokens} tok]")
}

fn handle_status(root: &str) -> String {
    let index = match ProjectIndex::load(root) {
        Some(idx) => idx,
        None => return "No graph index. Run ctx_graph action='build' to create one.".to_string(),
    };

    let mut by_lang: HashMap<&str, usize> = HashMap::new();
    let mut total_tokens = 0usize;
    for entry in index.files.values() {
        *by_lang.entry(&entry.language).or_insert(0) += 1;
        total_tokens += entry.token_count;
    }

    let mut langs: Vec<_> = by_lang.iter().collect();
    langs.sort_by(|a, b| b.1.cmp(a.1));
    let lang_summary: String = langs
        .iter()
        .take(5)
        .map(|(l, c)| format!("{l}:{c}"))
        .collect::<Vec<_>>()
        .join(" ");

    format!(
        "Graph: {} files, {} symbols, {} edges | {} tok total\nLast scan: {}\nLanguages: {lang_summary}\nStored: {}",
        index.file_count(),
        index.symbol_count(),
        index.edge_count(),
        total_tokens,
        index.last_scan,
        ProjectIndex::index_dir(root)
            .map(|d| d.to_string_lossy().to_string())
            .unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edge_matches_file_crate_prefix() {
        let prefixes = vec![
            "lean_ctx::core::cache".to_string(),
            "crate::core::cache".to_string(),
            "super::core::cache".to_string(),
            "core::cache".to_string(),
        ];
        assert!(edge_matches_file(
            "lean_ctx::core::cache::SessionCache",
            &prefixes
        ));
        assert!(edge_matches_file(
            "crate::core::cache::SessionCache",
            &prefixes
        ));
        assert!(edge_matches_file("crate::core::cache", &prefixes));
        assert!(!edge_matches_file(
            "lean_ctx::core::config::Config",
            &prefixes
        ));
        assert!(!edge_matches_file("crate::core::cached_reader", &prefixes));
    }

    #[test]
    fn test_file_path_to_module_prefixes_rust() {
        let prefixes = file_path_to_module_prefixes("src/core/cache.rs", "/nonexistent");
        assert!(prefixes.contains(&"crate::core::cache".to_string()));
        assert!(prefixes.contains(&"core::cache".to_string()));
    }

    #[test]
    fn test_file_path_to_module_prefixes_mod_rs() {
        let prefixes = file_path_to_module_prefixes("src/core/mod.rs", "/nonexistent");
        assert!(prefixes.contains(&"crate::core".to_string()));
        assert!(!prefixes.iter().any(|p| p.contains("mod")));
    }
}
