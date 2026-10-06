// SPDX-License-Identifier: Apache-2.0
use serde_json::{Map, Value};

use crate::server::tool_trait::{ToolContext, get_str, get_str_array};

#[derive(Debug)]
pub struct ResolvedPaths {
    pub roots: Vec<String>,
    pub is_multi: bool,
}

/// Resolve tool paths with multi-root support.
///
/// Priority:
/// 0. `repo` argument (multi-repo alias → specific root)
/// 1. `paths` array argument (explicit multi-root)
/// 2. `path` string argument (single root, pre-resolved by dispatch)
/// 3. Session `extra_roots` (default multi-root from config/MCP)
/// 4. Resolve the active project root (never the process working directory)
///
/// Returns `Err` when an **explicit** `path`/`paths` argument was supplied but
/// could not be resolved (outside the project root, secret-screened, or
/// non-existent). Silently falling back to the project root in that case made
/// `ctx_tree path=/outside/repo` return the whole project tree (#401).
pub fn resolve_tool_paths(
    args: &Map<String, Value>,
    ctx: &ToolContext,
) -> Result<ResolvedPaths, String> {
    if let Some(repo) = get_str(args, "repo")
        && let Some(root) = crate::core::multi_repo::resolve_repo_root(&repo)
    {
        return Ok(ResolvedPaths {
            roots: vec![root],
            is_multi: false,
        });
    }

    if let Some(paths) = get_str_array(args, "paths")
        && !paths.is_empty()
    {
        let resolved = resolve_paths_sync(ctx, &paths);
        if !resolved.is_empty() {
            return Ok(ResolvedPaths {
                is_multi: resolved.len() > 1,
                roots: resolved,
            });
        }
        // The caller explicitly listed paths but none resolved — surface
        // the failure instead of scanning the project root (#401).
        return Err(format!(
            "none of the requested paths could be resolved — they may not exist or are \
                 outside the project root: {}",
            paths.join(", ")
        ));
    }

    // #846: `file_path` is an alias for `path` in ctx_search — agents
    // (especially Claude Code) send it expecting file-level scoping.
    // Without this fallback the parameter is silently ignored and the
    // search runs on the project root.
    for key in &["path", "file_path"] {
        if let Some(p) = ctx.resolved_path(key) {
            return Ok(ResolvedPaths {
                roots: vec![p.to_string()],
                is_multi: false,
            });
        }
        // An explicit path the dispatcher could not resolve lands in
        // `path_errors`. Do NOT fall back to the project root — return the
        // resolution error so the agent learns the path is out of scope
        // rather than silently receiving an unrelated tree (#401).
        if let Some(detail) = ctx.path_error(key) {
            return Err(detail.to_string());
        }
    }

    // Validate before extra roots can return a default scope. An empty or
    // relative primary must never be interpreted against the process CWD.
    if ctx.project_root.trim().is_empty() {
        return Err("active project root is required when no path is supplied".to_string());
    }
    if !std::path::Path::new(&ctx.project_root).is_absolute() {
        return Err("active project root must be absolute when no path is supplied".to_string());
    }

    if let Some(session_lock) = ctx.session.as_ref() {
        let (extra, jail_root) = {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let guard = loop {
                if let Ok(g) = session_lock.clone().try_read_owned() {
                    break Some(g);
                }
                if std::time::Instant::now() >= deadline {
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            };
            match guard {
                Some(session) => {
                    let root = session
                        .project_root
                        .clone()
                        .unwrap_or_else(|| ".".to_string());
                    (session.extra_roots.clone(), root)
                }
                None => (Vec::new(), ".".to_string()),
            }
        };
        if !extra.is_empty() {
            let jail = std::path::Path::new(&jail_root);
            let mut roots = vec![ctx.project_root.clone()];
            for r in &extra {
                let p = std::path::Path::new(r);
                if !p.is_dir() {
                    continue;
                }
                match crate::core::pathjail::jail_path(p, jail) {
                    Ok(_) => roots.push(r.clone()),
                    Err(e) => tracing::warn!("extra_root rejected by PathJail: {e}"),
                }
            }
            if roots.len() > 1 {
                return Ok(ResolvedPaths {
                    is_multi: true,
                    roots,
                });
            }
        }
    }

    Ok(ResolvedPaths {
        roots: vec![ctx.resolve_path_sync(&ctx.project_root)?],
        is_multi: false,
    })
}

fn resolve_paths_sync(ctx: &ToolContext, raw: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(raw.len());
    for p in raw {
        match ctx.resolve_path_sync(p) {
            Ok(resolved) => out.push(resolved),
            Err(e) => {
                tracing::warn!("multi-path resolve failed for {p}: {e}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_ctx() -> ToolContext {
        ToolContext {
            project_root: "/test/project".to_string(),
            extra_roots: Vec::new(),
            minimal: false,
            resolved_paths: std::collections::HashMap::new(),
            crp_mode: crate::tools::CrpMode::Off,
            cache: None,
            session: None,
            tool_calls: None,
            agent_id: None,
            workflow: None,
            ledger: None,
            client_name: None,
            client_role: None,
            shell_access: None,
            pipeline_stats: None,
            call_count: None,
            autonomy: None,
            pressure_snapshot: None,
            path_errors: std::collections::HashMap::new(),
            bm25_cache: None,
            progress_sender: None,
            cancel: None,
        }
    }

    #[test]
    fn fallback_uses_context_project_not_process_cwd() {
        let args = Map::new();
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        assert_ne!(
            root,
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
        let mut ctx = test_ctx();
        ctx.project_root = root.to_string_lossy().into_owned();
        let result = resolve_tool_paths(&args, &ctx).expect("no explicit path → default");
        assert_eq!(result.roots.len(), 1);
        assert_eq!(
            std::fs::canonicalize(&result.roots[0]).expect("resolved project root"),
            std::fs::canonicalize(root).expect("canonical context project root"),
            "fallback must resolve to the context project, independent of path spelling"
        );
        assert!(!result.is_multi);
    }

    #[test]
    fn missing_context_root_does_not_search_process_cwd() {
        let mut ctx = test_ctx();
        ctx.project_root.clear();
        let error = resolve_tool_paths(&Map::new(), &ctx).unwrap_err();
        assert!(error.contains("active project root is required"));
    }

    #[test]
    fn relative_context_root_does_not_search_process_cwd() {
        for root in [".", "relative-project"] {
            let mut ctx = test_ctx();
            ctx.project_root = root.to_string();
            assert!(
                resolve_tool_paths(&Map::new(), &ctx).is_err(),
                "root: {root}"
            );
        }
    }

    #[test]
    fn extra_roots_cannot_bypass_invalid_primary_root() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let extra = tempfile::tempdir().unwrap();
        let extra_root = extra
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut session = crate::core::session::SessionState::new();
        session.project_root = Some(extra_root.clone());
        session.extra_roots = vec![extra_root.clone()];
        let mut ctx = test_ctx();
        ctx.session = Some(std::sync::Arc::new(tokio::sync::RwLock::new(session)));
        for root in ["", ".", "relative-project"] {
            ctx.project_root = root.to_string();
            assert!(
                resolve_tool_paths(&Map::new(), &ctx).is_err(),
                "root: {root}"
            );
        }
        // A valid absolute primary still permits the existing multi-root path.
        ctx.project_root = extra_root;
        let resolved = resolve_tool_paths(&Map::new(), &ctx).unwrap();
        assert!(resolved.is_multi);
        assert!(resolved.roots.iter().all(|root| root == &ctx.project_root));
    }

    #[test]
    fn uses_resolved_path_when_present() {
        let args = Map::new();
        let mut ctx = test_ctx();
        ctx.resolved_paths
            .insert("path".to_string(), "/resolved/dir".to_string());
        let result = resolve_tool_paths(&args, &ctx).expect("resolved path");
        assert_eq!(result.roots, vec!["/resolved/dir"]);
        assert!(!result.is_multi);
    }

    #[test]
    fn empty_paths_array_falls_back() {
        let mut args = Map::new();
        args.insert("paths".to_string(), json!([]));
        let mut ctx = test_ctx();
        ctx.resolved_paths
            .insert("path".to_string(), "/fallback".to_string());
        let result = resolve_tool_paths(&args, &ctx).expect("empty paths → fallback");
        assert_eq!(result.roots, vec!["/fallback"]);
        assert!(!result.is_multi);
    }

    // #401: an explicit `path` the dispatcher could not resolve (out of jail,
    // secret-screened, non-existent) must surface the error — NOT silently
    // fall back to the project root and return an unrelated tree.
    #[test]
    fn explicit_unresolvable_path_errors_instead_of_root_fallback() {
        let mut args = Map::new();
        args.insert(
            "path".to_string(),
            json!("/home/jules/.claude/skills/mpm-config"),
        );
        let mut ctx = test_ctx();
        // Dispatcher could not resolve it → recorded in path_errors, absent
        // from resolved_paths (exactly what the daemon does for out-of-jail).
        ctx.path_errors.insert(
            "path".to_string(),
            "path escapes project root: /home/jules/.claude/skills/mpm-config \
             (root: /test/project)"
                .to_string(),
        );
        let err = resolve_tool_paths(&args, &ctx)
            .expect_err("out-of-jail explicit path must be an error");
        assert!(
            err.contains("escapes project root"),
            "error must explain the path is out of scope: {err}"
        );
    }

    // #401: an explicit `paths` array where nothing resolves must error too.
    //
    // Uses *real* directories so the PathJail decision is deterministic across
    // platforms. Canonicalizing non-existent paths is OS-dependent — the first
    // version of this test fed bogus absolute paths against a non-existent root
    // and passed on macOS while letting them through on Linux CI.
    //
    // Asserts a jail-enforcement invariant, so it only holds when the jail is
    // compiled in. Under `--features no-jail` (pulled in by `--all-features`) the
    // jail is intentionally disabled and out-of-root paths resolve, so skip it.
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn explicit_unresolvable_paths_array_errors() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("project");
        let outside = base.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let mut ctx = test_ctx();
        ctx.project_root = root.to_string_lossy().into_owned();

        let mut args = Map::new();
        args.insert(
            "paths".to_string(),
            json!([outside.to_string_lossy().into_owned()]),
        );
        let err = resolve_tool_paths(&args, &ctx)
            .expect_err("a path outside the project root must be an error");
        assert!(
            err.contains("none of the requested paths"),
            "error must report the unresolved paths: {err}"
        );
    }
}
