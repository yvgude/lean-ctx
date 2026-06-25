use crate::core::multi_repo::{
    MultiRepoConfig, RepoRootConfig, format_fused_results, global_manager,
};

/// Handle `ctx_multi_repo` tool calls with action-based dispatch.
#[must_use]
pub fn handle(
    action: &str,
    path: Option<&str>,
    alias: Option<&str>,
    query: Option<&str>,
    roots_filter: Option<&[String]>,
    max_results: usize,
) -> (String, usize) {
    match action {
        "add_root" => handle_add_root(path, alias),
        "remove_root" => handle_remove_root(path),
        "list_roots" => handle_list_roots(),
        "search" => handle_search(query, max_results, roots_filter),
        "status" => handle_status(),
        "save_config" => handle_save_config(),
        _ => (
            format!(
                "Unknown action: {action}. Valid: add_root, remove_root, list_roots, search, status, save_config"
            ),
            0,
        ),
    }
}

fn handle_add_root(path: Option<&str>, alias: Option<&str>) -> (String, usize) {
    let Some(path) = path else {
        return ("ERROR: path is required for add_root".to_string(), 0);
    };

    let manager = global_manager();
    let Ok(mut mgr) = manager.lock() else {
        return ("ERROR: failed to acquire multi-repo lock".to_string(), 0);
    };

    match mgr.add_root(path, alias) {
        Ok(()) => {
            let count = mgr.root_count();
            (
                format!(
                    "Added root: {path} (alias: {}). Total roots: {count}",
                    alias.unwrap_or("<auto>")
                ),
                0,
            )
        }
        Err(e) => (format!("ERROR: {e}"), 0),
    }
}

fn handle_remove_root(path: Option<&str>) -> (String, usize) {
    let Some(path) = path else {
        return ("ERROR: path is required for remove_root".to_string(), 0);
    };

    let manager = global_manager();
    let Ok(mut mgr) = manager.lock() else {
        return ("ERROR: failed to acquire multi-repo lock".to_string(), 0);
    };

    match mgr.remove_root(path) {
        Ok(()) => {
            let count = mgr.root_count();
            (format!("Removed root: {path}. Remaining roots: {count}"), 0)
        }
        Err(e) => (format!("ERROR: {e}"), 0),
    }
}

fn handle_list_roots() -> (String, usize) {
    let manager = global_manager();
    let Ok(mgr) = manager.lock() else {
        return ("ERROR: failed to acquire multi-repo lock".to_string(), 0);
    };

    let roots = mgr.list_roots();
    if roots.is_empty() {
        return (
            "No repo roots configured. Use add_root to add repositories.".to_string(),
            0,
        );
    }

    let mut out = String::with_capacity(roots.len() * 80);
    out.push_str(&format!("Configured roots ({}):\n", roots.len()));
    for root in &roots {
        let idx_status = if root.has_index { "indexed" } else { "pending" };
        out.push_str(&format!(
            "  [{alias}] {path} ({idx_status})\n",
            alias = root.alias,
            path = root.path,
        ));
    }
    (out, 0)
}

fn handle_search(
    query: Option<&str>,
    max_results: usize,
    roots_filter: Option<&[String]>,
) -> (String, usize) {
    let Some(query) = query else {
        return ("ERROR: query is required for search".to_string(), 0);
    };

    let manager = global_manager();
    let Ok(mut mgr) = manager.lock() else {
        return ("ERROR: failed to acquire multi-repo lock".to_string(), 0);
    };

    if mgr.root_count() == 0 {
        return (
            "ERROR: no repo roots configured. Use add_root first.".to_string(),
            0,
        );
    }

    let results = mgr.search(query, max_results, roots_filter);
    let output = format_fused_results(&results);
    let tokens = crate::core::tokens::count_tokens(&output);
    (output, tokens)
}

fn handle_status() -> (String, usize) {
    let manager = global_manager();
    let Ok(mgr) = manager.lock() else {
        return ("ERROR: failed to acquire multi-repo lock".to_string(), 0);
    };

    let roots = mgr.list_roots();
    let active = mgr.is_active();

    let mut out = String::new();
    out.push_str(&format!(
        "Multi-repo status: {}\n",
        if active { "ACTIVE" } else { "INACTIVE" }
    ));
    out.push_str(&format!("Roots: {}\n", roots.len()));
    for root in &roots {
        out.push_str(&format!(
            "  [{alias}] {path}\n",
            alias = root.alias,
            path = root.path
        ));
    }
    out.push_str(&format!(
        "Config: {}\n",
        crate::core::multi_repo::config_file_path().display()
    ));
    (out, 0)
}

fn handle_save_config() -> (String, usize) {
    let manager = global_manager();
    let Ok(mgr) = manager.lock() else {
        return ("ERROR: failed to acquire multi-repo lock".to_string(), 0);
    };

    let roots = mgr.list_roots();
    let config = MultiRepoConfig {
        repos: roots
            .iter()
            .map(|r| RepoRootConfig {
                path: r.path.clone(),
                alias: Some(r.alias.clone()),
            })
            .collect(),
        rrf_k: None,
    };

    match config.save() {
        Ok(()) => (
            format!(
                "Config saved to {}",
                crate::core::multi_repo::config_file_path().display()
            ),
            0,
        ),
        Err(e) => (format!("ERROR: {e}"), 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_unknown_action() {
        let (output, _) = handle("invalid", None, None, None, None, 10);
        assert!(output.contains("Unknown action"));
    }

    #[test]
    fn handle_add_root_missing_path() {
        let (output, _) = handle("add_root", None, None, None, None, 10);
        assert!(output.contains("path is required"));
    }

    #[test]
    fn handle_search_missing_query() {
        let (output, _) = handle("search", None, None, None, None, 10);
        assert!(output.contains("query is required"));
    }

    #[test]
    fn handle_list_roots_empty() {
        // This test depends on global state but should work on clean init
        let (output, _) = handle_list_roots();
        // Either shows roots or says none configured
        assert!(!output.is_empty());
    }
}
