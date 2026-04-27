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

fn shared_dir() -> PathBuf {
    crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("agents")
        .join("shared")
}

pub fn handle(
    action: &str,
    from_agent: Option<&str>,
    to_agent: Option<&str>,
    paths: Option<&str>,
    message: Option<&str>,
    cache: &crate::core::cache::SessionCache,
) -> String {
    match action {
        "push" => handle_push(from_agent, to_agent, paths, message, cache),
        "pull" => handle_pull(from_agent),
        "list" => handle_list(),
        "clear" => handle_clear(from_agent),
        _ => format!("Unknown action: {action}. Use: push, pull, list, clear"),
    }
}

fn handle_push(
    from_agent: Option<&str>,
    to_agent: Option<&str>,
    paths: Option<&str>,
    message: Option<&str>,
    cache: &crate::core::cache::SessionCache,
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
        if let Some(entry) = cache.get(path) {
            shared_files.push(SharedFile {
                path: entry.path.clone(),
                content: entry.content.clone(),
                mode: "full".to_string(),
                tokens: entry.original_tokens,
            });
        } else {
            not_found.push(*path);
        }
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

    let dir = shared_dir();
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

fn handle_pull(agent_id: Option<&str>) -> String {
    let dir = shared_dir();
    if !dir.exists() {
        return "No shared contexts available.".to_string();
    }

    let my_id = agent_id.unwrap_or("anonymous");
    let mut entries: Vec<SharedContext> = Vec::new();

    if let Ok(readdir) = std::fs::read_dir(&dir) {
        for entry in readdir.flatten() {
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                if let Ok(ctx) = serde_json::from_str::<SharedContext>(&content) {
                    let is_for_me =
                        ctx.to_agent.is_none() || ctx.to_agent.as_deref() == Some(my_id);
                    let is_not_from_me = ctx.from_agent != my_id;

                    if is_for_me && is_not_from_me {
                        entries.push(ctx);
                    }
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

fn handle_list() -> String {
    let dir = shared_dir();
    if !dir.exists() {
        return "No shared contexts.".to_string();
    }

    let mut count = 0;
    let mut total_files = 0;
    let mut out = String::from("Shared context store:\n");

    if let Ok(readdir) = std::fs::read_dir(&dir) {
        for entry in readdir.flatten() {
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                if let Ok(ctx) = serde_json::from_str::<SharedContext>(&content) {
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
    }

    if count == 0 {
        return "No shared contexts.".to_string();
    }

    out.push_str(&format!("\nTotal: {count} shares, {total_files} files"));
    out
}

fn handle_clear(agent_id: Option<&str>) -> String {
    let dir = shared_dir();
    if !dir.exists() {
        return "Nothing to clear.".to_string();
    }

    let my_id = agent_id.unwrap_or("anonymous");
    let mut removed = 0;

    if let Ok(readdir) = std::fs::read_dir(&dir) {
        for entry in readdir.flatten() {
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                if let Ok(ctx) = serde_json::from_str::<SharedContext>(&content) {
                    if ctx.from_agent == my_id {
                        let _ = std::fs::remove_file(entry.path());
                        removed += 1;
                    }
                }
            }
        }
    }

    format!("Cleared {removed} shared context(s) from {my_id}")
}
