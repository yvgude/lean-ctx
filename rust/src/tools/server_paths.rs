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
            let root = self.session.read().await.project_root.clone();
            return crate::core::workspace_authority::project_dot(&normalized, root.as_deref());
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
            if session.project_root.is_none()
                && let Some(error) =
                    crate::core::path_resolve::unbound_relative_path(path, &resolved, &jail_root)
            {
                return Err(error);
            }
            (resolved, jail_root, session.extra_roots.clone())
        };

        let jail_root_path = std::path::Path::new(&jail_root);
        let jailed = match crate::core::pathjail::jail_path_with_roots(
            &resolved,
            jail_root_path,
            &extra_roots,
        ) {
            Ok(p) => self
                .maybe_reroot_for_absolute_path(&resolved, jail_root_path, &extra_roots)
                .await?
                .unwrap_or(p),
            Err(e) => {
                // #899: a refused path in a language cache or an agent
                // instruction folder is admitted read-only for the session
                // and resolved again (see `jail_path_auto_admitting`). If it
                // is still refused, report why — "retry" would only repeat
                // the same refusal.
                if crate::core::pathjail::detect_language_cache_root(&resolved).is_some() {
                    match crate::core::pathjail::jail_path_auto_admitting(
                        &resolved,
                        jail_root_path,
                        &extra_roots,
                    ) {
                        Ok(jailed) => jailed,
                        Err(retry) => {
                            crate::core::security_events::record_one(
                                "resolve_path",
                                "unknown",
                                crate::core::security_events::SecurityKind::PathBlocked,
                            );
                            crate::core::hive::metrics_v2::signals::blocked(
                                &crate::core::hive::metrics_v2::signals::path_jail_rule(),
                            );
                            return Err(retry.to_string());
                        }
                    }
                } else if let Some(jailed) = self
                    .maybe_reroot_for_absolute_path(&resolved, jail_root_path, &extra_roots)
                    .await?
                {
                    jailed
                } else {
                    // Resolution runs on the async side, outside the handler's
                    // collector, so the refusal is recorded directly.
                    crate::core::security_events::record_one(
                        "resolve_path",
                        "unknown",
                        crate::core::security_events::SecurityKind::PathBlocked,
                    );
                    crate::core::hive::metrics_v2::signals::blocked(
                        &crate::core::hive::metrics_v2::signals::path_jail_rule(),
                    );
                    return Err(e.to_string());
                }
            }
        };

        crate::core::io_boundary::check_secret_path_for_tool("resolve_path", &jailed)?;

        Ok(crate::core::pathutil::normalize_tool_path(
            &jailed.to_string_lossy().replace('\\', "/"),
        ))
    }

    async fn maybe_reroot_for_absolute_path(
        &self,
        resolved: &std::path::Path,
        jail_root_path: &std::path::Path,
        extra_roots: &[String],
    ) -> Result<Option<std::path::PathBuf>, String> {
        // A shared session is every client's: one client's path never moves
        // it (AP04-G5/G6).
        if !resolved.is_absolute() || self.session_mode == super::SessionMode::Shared {
            return Ok(None);
        }
        // Under a Workspace the session root follows the snapshot only
        // (#2120); a path never moves it.
        if crate::core::workspace_authority::is_configured() {
            return Ok(None);
        }
        // A root pinned with `LEAN_CTX_PROJECT_ROOT` is a boundary the operator
        // set, not a guess to correct: a markerless pinned directory inside a
        // larger checkout must not be widened to that checkout — and every
        // index, policy and receipt moved with it — by the first absolute path.
        if crate::server::roots::pinned_root().is_some_and(|pinned| {
            crate::core::pathutil::safe_canonicalize_or_self(std::path::Path::new(&pinned))
                == crate::core::pathutil::safe_canonicalize_or_self(jail_root_path)
        }) {
            return Ok(None);
        }
        // The outermost marker above a path can be `~` itself (a dotfiles
        // repository) or an agent directory: never a project to move to
        // (AP04-G6).
        let Some(new_root) = maybe_derive_project_root_from_absolute(resolved)
            .filter(|root| !crate::core::pathutil::is_broad_or_unsafe_root(root))
        else {
            return Ok(None);
        };
        let jail_is_real_project =
            has_project_marker(jail_root_path) && !is_suspicious_root(jail_root_path);
        let candidate_under_jail = resolved.starts_with(jail_root_path);
        // #1437: never reroot to a child when jail is a real project. Skip when
        // jail is suspicious (/, home, agent-config) — not a real boundary.
        if jail_is_real_project
            && new_root.starts_with(jail_root_path)
            && new_root != jail_root_path
        {
            return Ok(None);
        }
        // Universal auto-reroot: when the jail root is not a real project
        // (suspicious, None, or markerless), correct to derived project root.
        // No IDE-specific logic — works for all IDEs and daemon.
        let allow_reroot = if candidate_under_jail && jail_is_real_project {
            false
        } else {
            !jail_is_real_project || self.session.read().await.project_root.is_none()
        };

        if !allow_reroot {
            return Ok(None);
        }

        if !self.reroot_to_project(&new_root).await {
            return Ok(None);
        }
        crate::core::pathjail::jail_path_with_roots(resolved, &new_root, extra_roots)
            .map(Some)
            .map_err(|e| e.to_string())
    }

    /// Move the session — and the request that corrected it — to `new_root`
    /// through the same switch as a host's root change: that project's own
    /// session, the stale one saved where it was (AP04 review S4). Refused
    /// under a pinned source view, whose request project cannot follow.
    async fn reroot_to_project(&self, new_root: &std::path::Path) -> bool {
        if crate::core::policy::runtime::inherited_source_view().is_some() {
            return false;
        }
        let new_root_str = new_root.to_string_lossy().to_string();
        let shell_cwd = self
            .startup_shell_cwd
            .as_ref()
            .filter(|cwd| std::path::Path::new(cwd).starts_with(new_root))
            .cloned()
            .or_else(|| Some(new_root_str.clone()));
        let current = self.session.instance();
        let extra_roots = current.read().await.extra_roots.clone();
        let switch = crate::server::SessionSwitch {
            new_root: &new_root_str,
            extra_roots,
            shell_cwd,
            rebind_request: true,
        };
        let Some(saves) = self.switch_session_root(&current, switch).await else {
            return false;
        };
        for save in saves {
            let _ = save.await;
        }
        true
    }

    /// Like `resolve_path`, but returns the original path on failure instead of an error.
    pub async fn resolve_path_or_passthrough(&self, path: &str) -> String {
        self.resolve_path(path)
            .await
            .unwrap_or_else(|_| path.to_string())
    }
}
