use super::server::LeanCtxServer;
use super::startup::{
    has_project_marker, is_suspicious_root, maybe_derive_project_root_from_absolute,
};

impl LeanCtxServer {
    pub fn checkpoint_interval_effective() -> usize {
        if let Ok(v) = std::env::var("LEAN_CTX_CHECKPOINT_INTERVAL")
            && let Ok(parsed) = v.trim().parse::<usize>()
        {
            return parsed;
        }
        let profile_interval = crate::core::profiles::active_profile()
            .autonomy
            .checkpoint_interval_effective();
        if profile_interval > 0 {
            return profile_interval as usize;
        }
        crate::core::config::Config::load().checkpoint_interval as usize
    }

    /// Resolves a (possibly relative) tool path against the session's project_root.
    /// Absolute paths and "." are returned as-is. Relative paths like "src/main.rs"
    /// are joined with project_root so tools work regardless of the server's cwd.
    pub async fn resolve_path(&self, path: &str) -> Result<String, String> {
        let normalized = crate::core::pathutil::normalize_tool_path(path);
        if normalized.is_empty() || normalized == "." {
            return Ok(normalized);
        }
        let p = std::path::Path::new(&normalized);

        let (resolved, jail_root, extra_roots) = {
            let session = self.session.read().await;
            let jail_root = session
                .project_root
                .as_deref()
                .or(session.shell_cwd.as_deref())
                .unwrap_or(".")
                .to_string();

            // #707: a shell_cwd tracking a mid-session worktree switch
            // (different git checkout) outranks the stale project_root — same
            // precedence as core::path_resolve. Checked BEFORE the `p.exists()`
            // probe below: that probe runs against the *process* CWD, which
            // IDEs routinely set to the original project root, so it would
            // short-circuit every relative path back to the stale checkout and
            // the divergence rule could never apply.
            let worktree_cwd = if p.is_absolute() {
                None
            } else {
                session
                    .project_root
                    .as_deref()
                    .zip(session.shell_cwd.as_deref())
                    .filter(|(root, cwd)| {
                        crate::core::path_resolve::shell_cwd_is_divergent_checkout(root, cwd)
                    })
                    .map(|(_, cwd)| std::path::Path::new(cwd).join(&normalized))
            };

            let resolved = if let Some(overridden) = worktree_cwd {
                overridden
            } else if p.is_absolute() || p.exists() {
                std::path::PathBuf::from(&normalized)
            } else if let Some(ref root) = session.project_root {
                let joined = std::path::Path::new(root).join(&normalized);
                if joined.exists() {
                    joined
                } else if let Some(ref cwd) = session.shell_cwd {
                    std::path::Path::new(cwd).join(&normalized)
                } else {
                    std::path::Path::new(&jail_root).join(&normalized)
                }
            } else if let Some(ref cwd) = session.shell_cwd {
                std::path::Path::new(cwd).join(&normalized)
            } else {
                std::path::Path::new(&jail_root).join(&normalized)
            };

            // Session-scoped trusted roots (MCP roots/list, config extra_roots,
            // git worktrees) must widen the jail for an explicit path (#403).
            (resolved, jail_root, session.extra_roots.clone())
        };

        let jail_root_path = std::path::Path::new(&jail_root);
        let jailed = match crate::core::pathjail::jail_path_with_roots(
            &resolved,
            jail_root_path,
            &extra_roots,
        ) {
            Ok(p) => p,
            Err(e) => {
                // #899: the rejected path is dependency source in a language
                // cache (Go module cache, cargo registry, site-packages,
                // node_modules, …). Register its root as a session-scoped
                // read-only root and ask the agent to retry — the next resolve
                // sees it in the read allow-list. Stays fail-closed: this first
                // call still errors, and writes into the cache remain denied.
                if let Some((label, cache_root)) =
                    crate::core::pathjail::detect_language_cache_root(&resolved)
                    && crate::core::pathjail::register_session_read_only_root(&cache_root)
                {
                    return Err(format!(
                        "Auto-detected {label} at {} — added as a read-only root for this \
                         session. Retry the read.",
                        cache_root.display()
                    ));
                }
                if p.is_absolute() {
                    if let Some(new_root) = maybe_derive_project_root_from_absolute(&resolved) {
                        let cfg_allow = std::env::var("LEAN_CTX_ALLOW_REROOT").map_or_else(
                            |_| crate::core::config::Config::load().allow_auto_reroot,
                            |v| v == "1" || v == "true",
                        );
                        let candidate_under_jail = resolved.starts_with(jail_root_path);
                        // #580/#649: when the MCP server was launched from an
                        // agent/IDE config dir (e.g. ~/.copilot) or a markerless
                        // client cwd (e.g. WSL VS Code starting in /mnt/c/Users),
                        // that jail is not a real project boundary. The derived
                        // root already carries a project marker, so correcting to
                        // it is a root fix, not a jail weakening. Real project
                        // roots and trusted startup roots still keep the
                        // conservative gate.
                        let allow_reroot = if candidate_under_jail {
                            false
                        } else if is_suspicious_root(jail_root_path)
                            || (self.startup_project_root.is_none()
                                && !has_project_marker(jail_root_path))
                        {
                            true
                        } else if !cfg_allow {
                            false
                        } else if let Some(ref trusted_root) = self.startup_project_root {
                            std::path::Path::new(trusted_root) == new_root.as_path()
                        } else {
                            !has_project_marker(jail_root_path)
                        };

                        if allow_reroot {
                            let mut session = self.session.write().await;
                            let new_root_str = new_root.to_string_lossy().to_string();
                            session.project_root = Some(new_root_str.clone());
                            session.shell_cwd = self
                                .startup_shell_cwd
                                .as_ref()
                                .filter(|cwd| std::path::Path::new(cwd).starts_with(&new_root))
                                .cloned()
                                .or_else(|| Some(new_root_str.clone()));
                            let _ = session.save();

                            crate::core::pathjail::jail_path_with_roots(
                                &resolved,
                                &new_root,
                                &extra_roots,
                            )
                            .map_err(|e| e.to_string())?
                        } else {
                            return Err(e.to_string());
                        }
                    } else {
                        return Err(e.to_string());
                    }
                } else {
                    return Err(e.to_string());
                }
            }
        };

        crate::core::io_boundary::check_secret_path_for_tool("resolve_path", &jailed)?;

        Ok(crate::core::pathutil::normalize_tool_path(
            &jailed.to_string_lossy().replace('\\', "/"),
        ))
    }

    /// Like `resolve_path`, but returns the original path on failure instead of an error.
    pub async fn resolve_path_or_passthrough(&self, path: &str) -> String {
        self.resolve_path(path)
            .await
            .unwrap_or_else(|_| path.to_string())
    }
}
