use crate::core::archive;

pub fn handle(args: &serde_json::Value) -> String {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("retrieve");

    match action {
        "list" => handle_list(args),
        _ => handle_retrieve(args),
    }
}

fn handle_retrieve(args: &serde_json::Value) -> String {
    let id = match args.get("id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return "ERROR: 'id' parameter is required. Use ctx_expand(action=\"list\") to see available archives.".to_string(),
    };

    if let Some(pattern) = args.get("search").and_then(|v| v.as_str()) {
        return match archive::retrieve_with_search(id, pattern) {
            Some(result) => result,
            None => format!("Archive '{id}' not found or expired. Use ctx_expand(action=\"list\") to see available archives."),
        };
    }

    let start = args
        .get("start_line")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let end = args
        .get("end_line")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);

    if let (Some(s), Some(e)) = (start, end) {
        return match archive::retrieve_with_range(id, s, e) {
            Some(result) => {
                format!("Archive {id} lines {s}-{e}:\n{result}")
            }
            None => format!("Archive '{id}' not found or expired."),
        };
    }

    match archive::retrieve(id) {
        Some(content) => {
            let lines = content.lines().count();
            let chars = content.len();
            format!("Archive {id} ({chars} chars, {lines} lines):\n{content}")
        }
        None => format!(
            "Archive '{id}' not found or expired. Use ctx_expand(action=\"list\") to see available archives."
        ),
    }
}

fn handle_list(args: &serde_json::Value) -> String {
    let session_id = args.get("session_id").and_then(|v| v.as_str());
    let entries = archive::list_entries(session_id);

    if entries.is_empty() {
        return "No archives found.".to_string();
    }

    let mut out = format!("{} archive(s):\n", entries.len());
    for e in &entries {
        out.push_str(&format!(
            "  {} | {} | {} | {} chars ({} tok) | {}\n",
            e.id,
            e.tool,
            e.command,
            e.size_chars,
            e.size_tokens,
            e.created_at.format("%H:%M:%S")
        ));
    }
    out.push_str("\nRetrieve: ctx_expand(id=\"<id>\")");
    out.push_str("\nSearch: ctx_expand(id=\"<id>\", search=\"ERROR\")");
    out.push_str("\nRange: ctx_expand(id=\"<id>\", start_line=10, end_line=50)");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn handle_missing_id_returns_error() {
        let result = handle(&json!({}));
        assert!(result.contains("ERROR"));
        assert!(result.contains("id"));
    }

    #[test]
    fn handle_nonexistent_returns_not_found() {
        let result = handle(&json!({"id": "nonexistent_xyz"}));
        assert!(result.contains("not found"));
    }

    #[test]
    fn handle_list_empty() {
        let result = handle(&json!({"action": "list"}));
        assert!(
            result.contains("No archives") || result.contains("archive(s)"),
            "unexpected: {result}"
        );
    }
}
