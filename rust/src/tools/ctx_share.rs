use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
struct SharedContext {
    from_agent: String,
    to_agent: Option<String>,
    files: Vec<SharedFile>,
    message: Option<String>,
    timestamp: String,
    /// The project the share was pushed in. The directory is a hash that
    /// also covers the project's identity; this root proves the project when
    /// that identity changed. Shares written before it carry none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_root: Option<String>,
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

/// Copy shares this project pushed under an earlier identity (remote,
/// package name), proven by their recorded root, into its current
/// directory: never moved or overwritten, journaled once per file, bounded.
fn adopt_drifted_shares(project_root: &str) {
    const SCAN_LIMIT: usize = 2048;
    let current = shared_dir(project_root);
    let Some(base) = current.parent().map(std::path::Path::to_path_buf) else {
        return;
    };
    let Ok(partitions) = std::fs::read_dir(&base) else {
        return;
    };
    let canonical = |root: &str| {
        crate::core::pathutil::safe_canonicalize_bounded(std::path::Path::new(root), 2000)
    };
    let wanted = canonical(project_root);
    let partition = crate::core::project_hash::hash_project_root(project_root);
    let journal = crate::core::legacy_adoption::read_journal();
    let mut scanned = 0usize;
    for dir in partitions.flatten().map(|entry| entry.path()) {
        if dir == current || !dir.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for file in files.flatten().map(|entry| entry.path()) {
            scanned += 1;
            if scanned > SCAN_LIMIT {
                return;
            }
            let Some(share) = std::fs::read_to_string(&file)
                .ok()
                .and_then(|content| serde_json::from_str::<SharedContext>(&content).ok())
            else {
                continue;
            };
            if share
                .project_root
                .as_deref()
                .is_none_or(|root| canonical(root) != wanted)
            {
                continue;
            }
            let Some(name) = file.file_name() else {
                continue;
            };
            let item = format!(
                "ctx-share:{}/{}",
                dir.file_name().unwrap_or_default().to_string_lossy(),
                name.to_string_lossy()
            );
            let target = current.join(name);
            if target.exists() || crate::core::legacy_adoption::adopted(&journal, &item, &partition)
            {
                continue;
            }
            let ok =
                std::fs::create_dir_all(&current).is_ok() && std::fs::copy(&file, &target).is_ok();
            let _ = crate::core::legacy_adoption::record("ctx-share", &item, &partition, ok);
        }
    }
}

/// Filesystem-safe slug of an agent id for the share filename. Agent ids may
/// carry characters that are reserved on NTFS — `team:alice` (the documented
/// org format, enterprise#28) contains `:`, which Windows silently interprets
/// as an Alternate Data Stream: the write "succeeds" but `read_dir` never
/// lists a file, so the share is unpullable. Keep `[A-Za-z0-9._-]`, map
/// everything else to `-`. The real agent id lives inside the JSON payload;
/// the filename is only a directory-entry label.
fn sanitize_for_filename(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Reads `path` (absolute or relative to `project_root`) only if it resolves
/// inside the project root after symlink resolution — the jail that keeps a
/// share from carrying files outside the workspace (enterprise#28).
///
/// The read itself goes through the common rooted admitted reader (G01): a
/// handover is a derived store, so it takes the admitted text of a source
/// whose ORIGINAL may be kept (complete, not restricted). `None`: not a file
/// inside the root. `Some(Err(reason))`: withheld, with the content-free
/// reason.
fn read_within_root(path: &str, project_root: &str) -> Option<Result<(String, String), String>> {
    let root =
        crate::core::pathutil::canonicalize_secure(std::path::Path::new(project_root)).ok()?;
    let candidate = std::path::Path::new(path);
    let absolute = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    let resolved = crate::core::pathutil::canonicalize_secure(&absolute).ok()?;
    if !resolved.starts_with(&root) {
        return None;
    }
    let resolved_str = resolved.to_string_lossy().to_string();
    let mut remaining = crate::core::limits::max_read_bytes();
    let read = crate::tools::admitted_sources::read(
        &resolved_str,
        &root.to_string_lossy(),
        "ctx_share",
        &mut remaining,
    );
    match read {
        crate::tools::admitted_sources::SourceRead::Withheld(reason) => Some(Err(reason)),
        crate::tools::admitted_sources::SourceRead::Unavailable(_) => None,
        crate::tools::admitted_sources::SourceRead::Admitted(read) => {
            if read.storable && read.complete {
                Some(Ok((resolved_str, read.content)))
            } else {
                Some(Err(crate::core::context_admission::carrier::Withheld::new(
                    &["store.restricted_content"],
                )
                .message("source content")))
            }
        }
    }
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
    let mut withheld = Vec::new();
    // A handover is persisted for another agent: a derived store. Only
    // admitted text enters it, never withheld or restricted content (G5, E3).
    let admission = crate::core::context_admission::stores::StoreAdmission::current();

    for path in &path_list {
        // Revalidate against disk before handing the file to another agent: a
        // stale cached copy would silently pass an outdated handover file to the
        // receiving agent. `current_full_content` re-reads when the cache is
        // behind disk, so the receiver always gets the current content.
        let found = match cache.current_full_content(path) {
            Some(Ok((content, _))) => {
                let canonical = cache
                    .get(path)
                    .map_or_else(|| (*path).to_string(), |entry| entry.path.clone());
                Some((canonical, content))
            }
            // The typed, content-free reason is shown with the path (a source
            // that is gone, restricted, or withheld by the current policy).
            Some(Err(reason)) => {
                withheld.push(format!("{path} — {reason}"));
                continue;
            }
            // Not in this instance's cache — org flows (team server,
            // enterprise#28) run each call on a fresh instance, so fall back to
            // a direct read, jailed to the project root: a share must never
            // exfiltrate files outside the workspace.
            None => match read_within_root(path, project_root) {
                Some(Ok(found)) => Some(found),
                Some(Err(reason)) => {
                    withheld.push(format!("{path} — {reason}"));
                    continue;
                }
                None => None,
            },
        };
        let Some((canonical, content)) = found else {
            not_found.push(*path);
            continue;
        };
        let Some(content) = admission.admit(&content, std::path::Path::new(&canonical)) else {
            withheld.push((*path).to_string());
            continue;
        };
        let tokens = crate::core::tokens::count_tokens(&content);
        shared_files.push(SharedFile {
            path: canonical,
            content,
            mode: "full".to_string(),
            tokens,
        });
    }

    if shared_files.is_empty() {
        let mut out = String::from(
            "No shareable files found (not cached, and not readable inside the project root).",
        );
        if !not_found.is_empty() {
            out.push_str(&format!("\nNot found: {}", not_found.join(", ")));
        }
        if !withheld.is_empty() {
            out.push_str(&format!(
                "\nWithheld by the context gateway: {}",
                withheld.join(", ")
            ));
        }
        return out;
    }

    // The note travels with the handover, so it is admitted like the files.
    let admitted_message = message.map(|m| admission.admit_text(m));
    let message_withheld = matches!(admitted_message, Some(None));
    let context = SharedContext {
        from_agent: from.to_string(),
        to_agent: to_agent.map(String::from),
        files: shared_files.clone(),
        message: admitted_message.flatten(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        project_root: Some(project_root.to_string()),
    };

    let dir = shared_dir(project_root);
    let _ = std::fs::create_dir_all(&dir);

    let filename = format!(
        "{}_{}.json",
        sanitize_for_filename(from),
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
    if !withheld.is_empty() {
        result.push_str(&format!(
            "\nWithheld by the context gateway (skipped): {}",
            withheld.join(", ")
        ));
    }
    if message_withheld {
        result.push_str("\nMessage withheld by the context gateway (not shared).");
    }

    result
}

fn handle_pull(agent_id: Option<&str>, project_root: &str) -> String {
    adopt_drifted_shares(project_root);
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
            // The note is stored caller text without a source binding: it is
            // served under the originless rule, re-checked under the policy
            // current NOW (G04), not printed as it was stored.
            ctx.message
                .as_deref()
                .map(|m| {
                    match crate::tools::admitted_sources::serve_originless(m, "shared message") {
                        Ok(served) => format!("Message: {served}"),
                        Err(reason) => format!("Message: [withheld: {reason}]"),
                    }
                })
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
    adopt_drifted_shares(project_root);
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
            out.contains("No shareable files found"),
            "expected skip message: {out}"
        );
    }

    #[test]
    fn push_falls_back_to_disk_inside_root_without_cache() {
        // Org flow (enterprise#28): the team server runs every call on a fresh
        // instance — an empty cache must not block sharing a workspace file.
        let _lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());

        let proj = tempfile::tempdir().unwrap();
        let root = proj.path().to_str().unwrap();
        std::fs::write(proj.path().join("notes.md"), "ORG-SHARE marker-CCC\n").unwrap();

        let cache = SessionCache::new(); // fresh instance, nothing cached
        let out = handle_push(
            Some("team:alice"),
            None,
            Some("notes.md"),
            Some("handover"),
            &cache,
            root,
        );
        assert!(out.contains("Shared 1 files"), "push result: {out}");
        assert!(
            shared_json(root).contains("marker-CCC"),
            "disk fallback content not captured"
        );

        // A different token (agent) pulls it — org-wide sharing.
        let pulled = handle_pull(Some("team:bob"), root);
        assert!(pulled.contains("notes.md"), "receiver pull: {pulled}");
        // The sender does not see their own share on pull.
        let own = handle_pull(Some("team:alice"), root);
        assert!(own.contains("No shared contexts for you"), "own: {own}");
    }

    #[test]
    fn push_disk_fallback_is_jailed_to_project_root() {
        // A path outside the workspace root must never enter a share.
        let _lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());

        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "TOP-SECRET\n").unwrap();

        let proj = tempfile::tempdir().unwrap();
        let root = proj.path().to_str().unwrap();
        let cache = SessionCache::new();

        for evil in [
            secret.to_str().unwrap().to_string(),
            format!("../{}", secret.display()),
            "../../etc/hosts".to_string(),
        ] {
            let out = handle_push(Some("a"), None, Some(&evil), None, &cache, root);
            assert!(
                out.contains("No shareable files found"),
                "jail escape via {evil}: {out}"
            );
        }
        assert!(
            !shared_json(root).contains("TOP-SECRET"),
            "outside content leaked into share store"
        );
    }

    #[test]
    fn share_store_is_isolated_per_workspace_root() {
        // Two workspaces on the same host (team server) must not see each
        // other's shares — the store is keyed by the workspace root.
        let _lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());

        let ws1 = tempfile::tempdir().unwrap();
        let ws2 = tempfile::tempdir().unwrap();
        let root1 = ws1.path().to_str().unwrap();
        let root2 = ws2.path().to_str().unwrap();
        std::fs::write(ws1.path().join("a.md"), "WS1-ONLY\n").unwrap();

        let cache = SessionCache::new();
        let out = handle_push(Some("team:t1"), None, Some("a.md"), None, &cache, root1);
        assert!(out.contains("Shared 1 files"), "push: {out}");

        // Same host, other workspace: nothing visible.
        let other = handle_pull(Some("team:t2"), root2);
        assert!(
            other.contains("No shared contexts"),
            "workspace isolation broken: {other}"
        );
        // Same workspace: visible.
        let same = handle_pull(Some("team:t2"), root1);
        assert!(same.contains("a.md"), "same-workspace pull: {same}");
    }

    #[test]
    fn share_filename_is_ntfs_safe_for_org_agent_ids() {
        // `team:alice` in the filename made NTFS treat `:` as an Alternate
        // Data Stream — the share file never appeared in read_dir and the
        // handover was silently unpullable on Windows. The slug keeps the
        // filename portable; the true agent id lives in the JSON payload.
        assert_eq!(sanitize_for_filename("team:alice"), "team-alice");
        assert_eq!(
            sanitize_for_filename("a/b\\c*d?e\"f<g>h|i"),
            "a-b-c-d-e-f-g-h-i"
        );
        assert_eq!(sanitize_for_filename("agent_A.1-x"), "agent_A.1-x");
    }

    #[test]
    fn push_does_not_share_the_last_known_copy_of_a_deleted_file() {
        // G04 (deliberate behaviour change; this test used to assert that the
        // last-known cached copy of a deleted file is still shared): a cached
        // copy is not its own source, and a handover to another agent is
        // egress. Once the file is gone nothing is shared from the cache; the
        // typed reason is shown, the content is not. A readable, permitted
        // file in the same push is still shared byte-identically.
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

        let kept = canon.join("kept.md");
        std::fs::write(&kept, "STILL-HERE-BBB\n").unwrap();
        let kept_path = kept.to_str().unwrap();

        let mut cache = SessionCache::new();
        cache.store(path, "LASTKNOWN-AAA\n");
        cache.store(kept_path, "STILL-HERE-BBB\n");
        std::fs::remove_file(&file).unwrap();

        // The deleted file alone: nothing is shared, the reason is typed.
        let out = handle_push(Some("a"), Some("b"), Some(path), None, &cache, root);
        assert!(
            out.contains("No shareable files found"),
            "push result: {out}"
        );
        assert!(
            out.contains("store.source_unavailable"),
            "push result: {out}"
        );
        assert!(!out.contains("LASTKNOWN-AAA"), "push result: {out}");

        // Together with a readable, permitted file: that one is shared.
        let both = format!("{path},{kept_path}");
        let out = handle_push(Some("a"), Some("b"), Some(&both), None, &cache, root);
        assert!(out.contains("Shared 1 files"), "push result: {out}");
        assert!(
            out.contains("store.source_unavailable"),
            "push result: {out}"
        );
        assert!(!out.contains("LASTKNOWN-AAA"), "push result: {out}");
        let shared = shared_json(root);
        assert!(
            shared.contains("STILL-HERE-BBB\\n"),
            "the permitted file is shared unchanged: {shared}"
        );
        assert!(
            !shared.contains("LASTKNOWN-AAA"),
            "no byte of the deleted source may be shared: {shared}"
        );
    }

    /// G04: `pull` printed a stored handover note exactly as it was stored. A
    /// note has no source binding, so it is served under the originless rule:
    /// re-admitted under today's content rules in Community developer mode
    /// (a value the gateway masks is masked), withheld once a project policy
    /// is active.
    #[test]
    fn pull_serves_the_stored_note_under_the_originless_rule() {
        const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
        let _data = crate::core::data_dir::isolated_data_dir();
        let proj = tempfile::tempdir().unwrap();
        let root = proj.path().to_str().unwrap();
        // A record as an earlier version (or another process) stored it.
        let stored = SharedContext {
            from_agent: "agentA".to_string(),
            to_agent: Some("agentB".to_string()),
            files: Vec::new(),
            message: Some(format!("handover notes, deploy key {AWS_KEY}")),
            timestamp: "2026-10-06T07:00:00+00:00".to_string(),
            project_root: None,
        };
        let dir = shared_dir(root);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agentA_20261006_070000.json"),
            serde_json::to_string_pretty(&stored).unwrap(),
        )
        .unwrap();
        {
            let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
            let pulled = handle_pull(Some("agentB"), root);
            assert!(pulled.contains("Message: handover notes"), "{pulled}");
            assert!(!pulled.contains(AWS_KEY), "{pulled}");
        }
        let policy = crate::core::policy::load(
            "name = \"ctx-share-pull-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n",
        )
        .unwrap();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        let pulled = handle_pull(Some("agentB"), root);
        assert!(pulled.contains("From: agentA"), "{pulled}");
        assert!(pulled.contains("Message: [withheld: "), "{pulled}");
        assert!(!pulled.contains("handover notes"), "{pulled}");
    }

    /// G01: the disk fallback read the file with the plain reader and let only
    /// the gateway's store admission judge it — a source the PROJECT policy
    /// blocks was written into the handover. It is now read through the
    /// rooted admitted reader; a clean file is shared unchanged.
    #[test]
    fn the_disk_fallback_shares_admitted_sources_only() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let policy = crate::core::policy::load(
            "name = \"ctx-share-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
             [filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )
        .unwrap();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        let proj = tempfile::tempdir().unwrap();
        let root = proj.path().to_str().unwrap();
        std::fs::write(proj.path().join("open.md"), "OPEN-NOTES-CCC\n").unwrap();
        std::fs::write(proj.path().join("plan.md"), "CONFIDENTIAL\nROADMAP-DDD\n").unwrap();
        let cache = SessionCache::new();
        let out = handle_push(
            Some("a"),
            Some("b"),
            Some("open.md,plan.md"),
            None,
            &cache,
            root,
        );
        assert!(out.contains("Shared 1 files"), "push result: {out}");
        assert!(out.contains("plan.md — "), "push result: {out}");
        assert!(!out.contains("ROADMAP-DDD"), "push result: {out}");
        let shared = shared_json(root);
        assert!(shared.contains("OPEN-NOTES-CCC\\n"), "{shared}");
        assert!(!shared.contains("ROADMAP-DDD"), "{shared}");
    }
}
