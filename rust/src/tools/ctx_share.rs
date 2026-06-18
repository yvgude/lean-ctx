use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
struct SharedContext {
    from_agent: String,
    to_agent: Option<String>,
    files: Vec<SharedFile>,
    message: Option<String>,
    timestamp: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct SharedFile {
    path: String,
    content: String,
    mode: String,
    tokens: usize,
}

fn shared_dir(project_root: &str) -> PathBuf {
    let hash = crate::core::project_hash::hash_project_root(project_root);
    crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("agents")
        .join("shared")
        .join(hash)
}

pub fn handle(
    action: &str,
    from_agent: Option<&str>,
    to_agent: Option<&str>,
    paths: Option<&str>,
    message: Option<&str>,
    cache: &crate::core::cache::SessionCache,
    project_root: &str,
) -> String {
    match action {
        "push" => handle_push(from_agent, to_agent, paths, message, cache, project_root),
        "pull" => handle_pull(from_agent, project_root),
        "list" => handle_list(project_root),
        "clear" => handle_clear(from_agent, project_root),
        _ => format!("Unknown action: {action}. Use: push, pull, list, clear"),
    }
}

fn handle_push(
    from_agent: Option<&str>,
    to_agent: Option<&str>,
    paths: Option<&str>,
    message: Option<&str>,
    cache: &crate::core::cache::SessionCache,
    project_root: &str,
) -> String {
    let Some(from) = from_agent else {
        return "Error: from_agent is required (register first via ctx_agent)".to_string();
    };

    let path_list: Vec<&str> = match paths {
        Some(p) => p.split(',').map(str::trim).collect(),
        None => return "Error: paths is required (comma-separated file paths)".to_string(),
    };

    let mut shared_files = Vec::new();
    let mut not_found = Vec::new();

    for path in &path_list {
        // Revalidate against disk before handing the file to another agent: a
        // stale cached copy would silently pass an outdated handover file to the
        // receiving agent. `current_full_content` re-reads when the cache is
        // behind disk, so the receiver always gets the current content.
        let Some((content, tokens)) = cache.current_full_content(path) else {
            not_found.push(*path);
            continue;
        };
        let canonical = cache
            .get(path)
            .map_or_else(|| (*path).to_string(), |entry| entry.path.clone());
        shared_files.push(SharedFile {
            path: canonical,
            content,
            mode: "full".to_string(),
            tokens,
        });
    }

    if shared_files.is_empty() {
        return format!(
            "No cached files found to share. Files must be read first via ctx_read.\nNot found: {}",
            not_found.join(", ")
        );
    }

    let context = SharedContext {
        from_agent: from.to_string(),
        to_agent: to_agent.map(String::from),
        files: shared_files.clone(),
        message: message.map(String::from),
        timestamp: chrono::Utc::now().to_rfc3339(),
    };

    let dir = shared_dir(project_root);
    let _ = std::fs::create_dir_all(&dir);

    let filename = format!(
        "{}_{}.json",
        from,
        chrono::Utc::now().format("%Y%m%d_%H%M%S")
    );
    let path = dir.join(&filename);

    match serde_json::to_string_pretty(&context) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                return format!("Error writing shared context: {e}");
            }
        }
        Err(e) => return format!("Error serializing shared context: {e}"),
    }

    let total_tokens: usize = shared_files.iter().map(|f| f.tokens).sum();
    let mut result = format!(
        "Shared {} files ({} tokens) from {from}",
        shared_files.len(),
        total_tokens
    );

    if let Some(target) = to_agent {
        result.push_str(&format!(" → {target}"));
    } else {
        result.push_str(" → all agents (broadcast)");
    }

    if !not_found.is_empty() {
        result.push_str(&format!(
            "\nNot in cache (skipped): {}",
            not_found.join(", ")
        ));
    }

    result
}

fn handle_pull(agent_id: Option<&str>, project_root: &str) -> String {
    let dir = shared_dir(project_root);
    if !dir.exists() {
        return "No shared contexts available.".to_string();
    }

    let my_id = agent_id.unwrap_or("anonymous");
    let mut entries: Vec<SharedContext> = Vec::new();

    if let Ok(readdir) = std::fs::read_dir(&dir) {
        for entry in readdir.flatten() {
            if let Ok(content) = std::fs::read_to_string(entry.path())
                && let Ok(ctx) = serde_json::from_str::<SharedContext>(&content)
            {
                let is_for_me = ctx.to_agent.is_none() || ctx.to_agent.as_deref() == Some(my_id);
                let is_not_from_me = ctx.from_agent != my_id;

                if is_for_me && is_not_from_me {
                    entries.push(ctx);
                }
            }
        }
    }

    if entries.is_empty() {
        return "No shared contexts for you.".to_string();
    }

    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    let mut out = format!("Shared contexts available ({}):\n", entries.len());
    for ctx in &entries {
        let file_list: Vec<&str> = ctx.files.iter().map(|f| f.path.as_str()).collect();
        let total_tokens: usize = ctx.files.iter().map(|f| f.tokens).sum();
        out.push_str(&format!(
            "\n  From: {} ({})\n  Files: {} ({} tokens)\n  {}\n",
            ctx.from_agent,
            &ctx.timestamp[..19],
            file_list.join(", "),
            total_tokens,
            ctx.message
                .as_deref()
                .map(|m| format!("Message: {m}"))
                .unwrap_or_default(),
        ));
    }

    let total_files: usize = entries.iter().map(|e| e.files.len()).sum();
    out.push_str(&format!(
        "\nTotal: {} contexts, {} files. Use ctx_read on pulled files to load them into your cache.",
        entries.len(),
        total_files
    ));

    out
}

fn handle_list(project_root: &str) -> String {
    let dir = shared_dir(project_root);
    if !dir.exists() {
        return "No shared contexts.".to_string();
    }

    let mut count = 0;
    let mut total_files = 0;
    let mut out = String::from("Shared context store:\n");

    if let Ok(readdir) = std::fs::read_dir(&dir) {
        for entry in readdir.flatten() {
            if let Ok(content) = std::fs::read_to_string(entry.path())
                && let Ok(ctx) = serde_json::from_str::<SharedContext>(&content)
            {
                count += 1;
                total_files += ctx.files.len();
                let target = ctx.to_agent.as_deref().unwrap_or("broadcast");
                out.push_str(&format!(
                    "  {} → {} ({} files, {})\n",
                    ctx.from_agent,
                    target,
                    ctx.files.len(),
                    &ctx.timestamp[..19]
                ));
            }
        }
    }

    if count == 0 {
        return "No shared contexts.".to_string();
    }

    out.push_str(&format!("\nTotal: {count} shares, {total_files} files"));
    out
}

fn handle_clear(agent_id: Option<&str>, project_root: &str) -> String {
    let dir = shared_dir(project_root);
    if !dir.exists() {
        return "Nothing to clear.".to_string();
    }

    let my_id = agent_id.unwrap_or("anonymous");
    let mut removed = 0;

    if let Ok(readdir) = std::fs::read_dir(&dir) {
        for entry in readdir.flatten() {
            if let Ok(content) = std::fs::read_to_string(entry.path())
                && let Ok(ctx) = serde_json::from_str::<SharedContext>(&content)
                && ctx.from_agent == my_id
            {
                let _ = std::fs::remove_file(entry.path());
                removed += 1;
            }
        }
    }

    format!("Cleared {removed} shared context(s) from {my_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cache::SessionCache;

    /// Concatenated JSON of every shared-context file for `project_root`. Lets a
    /// test assert on exactly what content was *captured into the handover*.
    fn shared_json(project_root: &str) -> String {
        let dir = shared_dir(project_root);
        let mut all = String::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                all.push_str(&std::fs::read_to_string(e.path()).unwrap_or_default());
            }
        }
        all
    }

    #[test]
    fn push_shares_fresh_content_and_pull_lists_it() {
        let _lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());

        let proj = tempfile::tempdir().unwrap();
        let root = proj.path().to_str().unwrap();
        let file = proj.path().join("handover.md");
        std::fs::write(&file, "HANDOVER marker-AAA\n").unwrap();
        let path = file.to_str().unwrap();

        let mut cache = SessionCache::new();
        cache.store(path, "HANDOVER marker-AAA\n");

        let out = handle_push(
            Some("agentA"),
            Some("agentB"),
            Some(path),
            None,
            &cache,
            root,
        );
        assert!(out.contains("Shared 1 files"), "push result: {out}");
        assert!(
            shared_json(root).contains("marker-AAA"),
            "content not captured"
        );

        // The receiver sees the handover listed.
        let pulled = handle_pull(Some("agentB"), root);
        assert!(
            pulled.contains("handover.md"),
            "pull missing file: {pulled}"
        );
    }

    #[test]
    fn push_shares_edited_content_not_stale_diff_mtime() {
        // Carlos handover: a file edited *after* it was cached must be shared as
        // the NEW content — the receiving agent must never get the pre-edit copy.
        let _lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());

        let proj = tempfile::tempdir().unwrap();
        let root = proj.path().to_str().unwrap();
        let file = proj.path().join("handover.md");
        std::fs::write(&file, "V1 marker-AAA\n").unwrap();
        let path = file.to_str().unwrap();

        let mut cache = SessionCache::new();
        cache.store(path, "V1 marker-AAA\n");

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&file, "V2 marker-BBB\n").unwrap();

        let out = handle_push(Some("a"), Some("b"), Some(path), None, &cache, root);
        assert!(out.contains("Shared 1 files"), "push result: {out}");
        let json = shared_json(root);
        assert!(
            json.contains("marker-BBB"),
            "fresh content not shared: {json}"
        );
        assert!(
            !json.contains("marker-AAA"),
            "stale content leaked into handover: {json}"
        );
    }

    #[test]
    fn push_shares_edited_content_same_mtime_same_size() {
        // Hash backstop: identical mtime + identical size, changed content.
        let _lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());

        let proj = tempfile::tempdir().unwrap();
        let root = proj.path().to_str().unwrap();
        let file = proj.path().join("h.md");
        std::fs::write(&file, "AAA\n").unwrap();
        let path = file.to_str().unwrap();
        let mtime = std::fs::metadata(&file).unwrap().modified().unwrap();

        let mut cache = SessionCache::new();
        cache.store(path, "AAA\n");

        // Same length (4 bytes), restore the original mtime → only the content hash differs.
        std::fs::write(&file, "BBB\n").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(mtime)
            .unwrap();

        let out = handle_push(Some("a"), Some("b"), Some(path), None, &cache, root);
        assert!(out.contains("Shared 1 files"), "push result: {out}");
        let json = shared_json(root);
        assert!(
            json.contains("BBB"),
            "hash backstop failed, stale shared: {json}"
        );
        assert!(!json.contains("AAA"), "stale content leaked: {json}");
    }

    #[test]
    fn push_skips_uncached_paths() {
        let _lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());

        let proj = tempfile::tempdir().unwrap();
        let root = proj.path().to_str().unwrap();
        let cache = SessionCache::new(); // empty

        let out = handle_push(
            Some("a"),
            Some("b"),
            Some("/no/such/file.md"),
            None,
            &cache,
            root,
        );
        assert!(
            out.contains("No cached files found to share"),
            "expected skip message: {out}"
        );
    }

    #[test]
    fn push_falls_back_to_last_known_when_file_deleted() {
        // Stale + unreadable (deleted between cache and handover): the last-known
        // cached copy is shared rather than dropping the file silently.
        let _lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());

        let proj = tempfile::tempdir().unwrap();
        // Canonicalize so the cache key stays stable after the file is removed.
        let canon = proj.path().canonicalize().unwrap();
        let root = canon.to_str().unwrap();
        let file = canon.join("gone.md");
        std::fs::write(&file, "LASTKNOWN-AAA\n").unwrap();
        let path = file.to_str().unwrap();

        let mut cache = SessionCache::new();
        cache.store(path, "LASTKNOWN-AAA\n");
        std::fs::remove_file(&file).unwrap();

        let out = handle_push(Some("a"), Some("b"), Some(path), None, &cache, root);
        assert!(out.contains("Shared 1 files"), "push result: {out}");
        assert!(
            shared_json(root).contains("LASTKNOWN-AAA"),
            "last-known content not shared"
        );
    }
}
