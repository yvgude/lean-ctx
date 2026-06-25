//! `/api/tree` — a collapsible directory → file → symbol hierarchy built from the
//! real graph provider (`PropertyGraph`). Powers the Explorer tab. No mock data:
//! the tree mirrors indexed files and their extracted symbols.

use crate::dashboard::routes::helpers::detect_project_root_for_dashboard;
use std::collections::BTreeMap;

pub(super) fn get_route(
    path: &str,
    _query_str: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/tree" => Some(tree()),
        _ => None,
    }
}

#[derive(Default)]
struct DirNode {
    dirs: BTreeMap<String, DirNode>,
    files: Vec<FileLeaf>,
}

struct SymLeaf {
    name: String,
    kind: String,
    line: usize,
    exported: bool,
}

struct FileLeaf {
    name: String,
    path: String,
    language: String,
    lines: usize,
    symbols: Vec<SymLeaf>,
}

fn tree() -> (&'static str, &'static str, String) {
    let root = detect_project_root_for_dashboard();
    let project = super::project_basename(&root);
    let provider = match crate::core::graph_coordinator::get_or_start_build(&root) {
        Ok(open) => open.provider,
        Err(progress) => return super::building_response(&progress),
    };

    // Group symbols by their (relative) file path.
    let mut syms_by_file: std::collections::HashMap<String, Vec<SymLeaf>> =
        std::collections::HashMap::new();
    for sym in provider.all_symbols() {
        syms_by_file.entry(sym.file).or_default().push(SymLeaf {
            name: sym.name,
            kind: sym.kind,
            line: sym.start_line,
            exported: sym.is_exported,
        });
    }

    let mut tree_root = DirNode::default();
    let mut file_count = 0usize;
    let mut symbol_count = 0usize;

    for entry in provider.file_entries() {
        file_count += 1;
        let mut symbols: Vec<SymLeaf> = syms_by_file.remove(&entry.path).unwrap_or_default();
        symbols.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.name.cmp(&b.name)));
        symbol_count += symbols.len();

        let parts: Vec<&str> = entry.path.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            continue;
        }
        let (dirs, file_name) = parts.split_at(parts.len() - 1);
        let mut node = &mut tree_root;
        for dir in dirs {
            node = node.dirs.entry((*dir).to_string()).or_default();
        }
        node.files.push(FileLeaf {
            name: file_name[0].to_string(),
            path: entry.path.clone(),
            language: entry.language.clone(),
            lines: entry.line_count,
            symbols,
        });
    }

    let children = serialize_dir(&mut tree_root);
    let val = serde_json::json!({
        "project": project,
        "file_count": file_count,
        "symbol_count": symbol_count,
        "tree": children,
    });
    ("200 OK", "application/json", val.to_string())
}

/// Serialize a directory's children (sub-dirs first, then files), each sorted.
fn serialize_dir(node: &mut DirNode) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    for (name, child) in &mut node.dirs {
        let kids = serialize_dir(child);
        let file_count = count_files(child);
        out.push(serde_json::json!({
            "type": "dir",
            "name": name,
            "files": file_count,
            "children": kids,
        }));
    }
    node.files.sort_by(|a, b| a.name.cmp(&b.name));
    for f in &node.files {
        let syms: Vec<serde_json::Value> = f
            .symbols
            .iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "kind": s.kind,
                    "line": s.line,
                    "exported": s.exported,
                })
            })
            .collect();
        out.push(serde_json::json!({
            "type": "file",
            "name": f.name,
            "path": f.path,
            "language": f.language,
            "lines": f.lines,
            "symbol_count": syms.len(),
            "symbols": syms,
        }));
    }
    out
}

fn count_files(node: &DirNode) -> usize {
    node.files.len() + node.dirs.values().map(count_files).sum::<usize>()
}
