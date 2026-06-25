use tree_sitter::{Node, Parser, QueryCursor, StreamingIterator};

use super::extract::find_capture_index;
use super::queries::get_language;
use super::query_cache::get_cached_sig_query;

#[must_use]
pub fn ast_prune(content: &str, file_ext: &str) -> Option<String> {
    let language = get_language(file_ext)?;

    thread_local! {
        static PARSER: std::cell::RefCell<Parser> = std::cell::RefCell::new(Parser::new());
    }

    let tree = PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        let _ = parser.set_language(&language);
        parser.parse(content, None)
    })?;
    let query = get_cached_sig_query(file_ext)?;

    let def_idx = find_capture_index(query, "def")?;
    let source = content.as_bytes();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);

    let lines: Vec<&str> = content.lines().collect();
    let mut keep = vec![false; lines.len()];

    while let Some(m) = matches.next() {
        for cap in m.captures {
            if cap.index == def_idx {
                let node = cap.node;
                let sig_start = node.start_position().row;

                if let Some(body) = find_body_node(&node) {
                    let body_start = body.start_position().row;
                    // `saturating_sub` guards against a malformed AST where the body row
                    // precedes the signature row, which would otherwise underflow `usize`
                    // → panic (debug) or an enormous `.take()` → OOM (release).
                    for flag in keep
                        .iter_mut()
                        .skip(sig_start)
                        .take(body_start.min(sig_start + 3).saturating_sub(sig_start) + 1)
                    {
                        *flag = true;
                    }
                    let body_end = body.end_position().row;
                    if body_end < lines.len() {
                        keep[body_end] = true;
                    }
                } else {
                    let end = node.end_position().row;
                    for flag in keep
                        .iter_mut()
                        .skip(sig_start)
                        .take(end.min(sig_start + 2).saturating_sub(sig_start) + 1)
                    {
                        *flag = true;
                    }
                }
            }
        }
    }

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() && i > 0 && i + 1 < lines.len() && keep.get(i + 1) == Some(&true) {
            keep[i] = true;
        }
        if is_import_line(trimmed, file_ext) {
            keep[i] = true;
        }
    }

    let mut result = Vec::new();
    let mut prev_kept = true;
    for (i, line) in lines.iter().enumerate() {
        if keep[i] {
            if !prev_kept {
                result.push("  // ...".to_string());
            }
            result.push(line.to_string());
            prev_kept = true;
        } else {
            prev_kept = false;
        }
    }

    Some(result.join("\n"))
}

fn find_body_node<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    if let Some(body) = node.child_by_field_name("body") {
        return Some(body);
    }
    if let Some(block) = node.child_by_field_name("block") {
        return Some(block);
    }
    let mut cursor = node.walk();

    node.children(&mut cursor).find(|c| {
        matches!(
            c.kind(),
            "block"
                | "compound_statement"
                | "function_body"
                | "class_body"
                | "declaration_list"
                | "enum_body"
                | "statement_block"
        )
    })
}

fn is_import_line(trimmed: &str, ext: &str) -> bool {
    match ext {
        "rs" => trimmed.starts_with("use ") || trimmed.starts_with("mod "),
        "ts" | "tsx" | "js" | "jsx" => {
            trimmed.starts_with("import ") || trimmed.starts_with("export {")
        }
        "py" => trimmed.starts_with("import ") || trimmed.starts_with("from "),
        "go" => trimmed.starts_with("import ") || trimmed == "import (",
        "java" | "kt" | "kts" => trimmed.starts_with("import ") || trimmed.starts_with("package "),
        "c" | "h" | "cpp" | "hpp" => trimmed.starts_with("#include"),
        "cs" => trimmed.starts_with("using ") || trimmed.starts_with("namespace "),
        "rb" => trimmed.starts_with("require ") || trimmed.starts_with("require_relative "),
        "swift" => trimmed.starts_with("import "),
        "php" => trimmed.starts_with("use ") || trimmed.starts_with("namespace "),
        _ => false,
    }
}
