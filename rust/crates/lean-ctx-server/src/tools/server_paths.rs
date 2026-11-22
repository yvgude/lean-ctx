// SPDX-License-Identifier: Apache-2.0
use super::server::LeanCtxServer;
use super::startup::{
    has_project_marker, is_suspicious_root, maybe_derive_project_root_from_absolute,
};

/// Bound on the filesystem probes that judge an adoption: a hung network
/// mount refuses the adoption instead of stalling the call (same bound as
/// the jail's own canonicalization).
const ADOPTION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Adoption probes still running on the blocking pool, timed out ones
/// included. A probe stuck on a hung mount keeps its thread; past this bound
/// no new probe starts and the adoption fails closed, so hung probes never
/// pile up threads.
const MAX_ADOPTION_PROBES: usize = 2;
static ADOPTION_PROBES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// One in-flight probe; released when the probe's thread finishes.
struct ProbeSlot;

impl ProbeSlot {
    fn take() -> Option<Self> {
        use std::sync::atomic::Ordering;
        ADOPTION_PROBES
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_ADOPTION_PROBES).then_some(n + 1)
            })
            .ok()
            .map(|_| Self)
    }
}

impl Drop for ProbeSlot {
    fn drop(&mut self) {
        ADOPTION_PROBES.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

#[cfg(test)]
tokio::task_local! {
    /// Racing adoptions in a test meet at this barrier before the lock, once
    /// per task.
    pub(crate) static ADOPTION_BARRIER:
        std::cell::RefCell<Option<std::sync::Arc<tokio::sync::Barrier>>>;
}

impl LeanCtxServer {
    pub fn checkpoint_interval_effective() -> usize {
        if let Ok(v) = std::env::var("LEAN_CTX_CHECKPOINT_INTERVAL")
            && let Ok(parsed) = v.trim().parse::<usize>()
        {
            return parsed;
        }
        let profile_interval = lean_ctx::core::profiles::active_profile()
            .autonomy
            .checkpoint_interval_effective();
        if profile_interval > 0 {
            return profile_interval as usize;
        }
        lean_ctx::core::config::Config::load().checkpoint_interval as usize
    }

    /// Resolves a (possibly relative) tool path against the session's project_root.
    /// Absolute paths and "." are returned as-is. Relative paths like "src/main.rs"
    /// are joined with project_root so tools work regardless of the server's cwd.
    pub async fn resolve_path(&self, path: &str) -> Result<String, String> {
        self.resolve_path_refusing(path)
            .await
            .map_err(|refusal| refusal.message)
    }

    /// Records a classified boundary refusal once, with the calling `tool`:
    /// its class only, never the path.
    pub(crate) async fn record_path_refusal(
        &self,
        tool: &str,
        refusal: &lean_ctx::core::path_resolve::PathRefusal,
    ) {
        let Some(class) = refusal.class else {
            return;
        };
        let agent_id = self
            .agent_id
            .read()
            .await
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        lean_ctx::core::security_events::record_refusals(tool, &agent_id, class, 1);
    }

    /// [`Self::resolve_path`], saying which boundary refused the path. The
    /// dispatch layer records a classified refusal once, with the calling
    /// tool (the same path may be resolved again for planning).
    pub(crate) async fn resolve_path_refusing(
        &self,
        path: &str,
    ) -> Result<String, lean_ctx::core::path_resolve::PathRefusal> {
        // A handler resolving a path dispatch did not pre-resolve (an array
        // or non-string argument) does not know its tool here: an adoption
        // it triggers is audited as `unknown`, never under a plausible name.
        self.resolve_path_for_tool(path, "unknown").await
    }

    /// [`Self::resolve_path_refusing`] on behalf of the dispatching `tool`,
    /// which a project adoption records in the audit trail.
    pub(crate) async fn resolve_path_for_tool(
        &self,
        path: &str,
        tool: &str,
    ) -> Result<String, lean_ctx::core::path_resolve::PathRefusal> {
        use lean_ctx::core::path_resolve::PathRefusal;
        let normalized = lean_ctx::core::pathutil::normalize_tool_path(path);
        if normalized.is_empty() || normalized == "." {
            let root = self.session.read().await.project_root.clone();
            return Ok(lean_ctx::core::workspace_authority::project_dot(
                &normalized,
                root.as_deref(),
            )?);
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
                        lean_ctx::core::path_resolve::shell_cwd_is_divergent_checkout(root, cwd)
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
                    lean_ctx::core::path_resolve::unbound_relative_path(path, &resolved, &jail_root)
            {
                return Err(error.into());
            }
            (resolved, jail_root, session.extra_roots.clone())
        };

        let jail_root_path = std::path::Path::new(&jail_root);
        let jailed = match lean_ctx::core::pathjail::jail_path_with_roots(
            &resolved,
            jail_root_path,
            &extra_roots,
        ) {
            Ok(p) => self
                .maybe_reroot_for_absolute_path(&resolved, jail_root_path, &extra_roots, tool)
                .await?
                .unwrap_or(p),
            Err(e) => {
                // #899: a refused path in a language cache or an agent
                // instruction folder is admitted read-only for the session
                // and resolved again (see `jail_path_auto_admitting`). If it
                // is still refused, report why — "retry" would only repeat
                // the same refusal.
                if lean_ctx::core::pathjail::detect_language_cache_root(&resolved).is_some() {
                    match lean_ctx::core::pathjail::jail_path_auto_admitting(
                        &resolved,
                        jail_root_path,
                        &extra_roots,
                    ) {
                        Ok(jailed) => jailed,
                        Err(retry) => {
                            lean_ctx_cloud::metrics_v2::signals::blocked(
                                &lean_ctx_cloud::metrics_v2::signals::path_jail_rule(),
                            );
                            return Err(PathRefusal::jail(&retry));
                        }
                    }
                } else if let Some(jailed) = self
                    .maybe_reroot_for_absolute_path(&resolved, jail_root_path, &extra_roots, tool)
                    .await?
                {
                    jailed
                } else {
                    lean_ctx_cloud::metrics_v2::signals::blocked(
                        &lean_ctx_cloud::metrics_v2::signals::path_jail_rule(),
                    );
                    return Err(PathRefusal::jail(&e));
                }
            }
        };

        lean_ctx::core::io_boundary::check_secret_path_for_tool("resolve_path", &jailed)
            .map_err(PathRefusal::secret_path)?;

        Ok(lean_ctx::core::pathutil::normalize_tool_path(
            &jailed.to_string_lossy().replace('\\', "/"),
        ))
    }

    async fn maybe_reroot_for_absolute_path(
        &self,
        resolved: &std::path::Path,
        jail_root_path: &std::path::Path,
        extra_roots: &[String],
        tool: &str,
    ) -> Result<Option<std::path::PathBuf>, String> {
        // A shared session is every client's: one client's path never moves
        // it (AP04-G5/G6).
        if !resolved.is_absolute() || self.session_mode == super::SessionMode::Shared {
            return Ok(None);
        }
        // Under a Workspace the session root follows the snapshot only
        // (#2120); a path never moves it.
        if lean_ctx::core::workspace_authority::is_configured() {
            return Ok(None);
        }
        // A root pinned with `LEAN_CTX_PROJECT_ROOT` is a boundary the operator
        // set, not a guess to correct: a markerless pinned directory inside a
        // larger checkout must not be widened to that checkout — and every
        // index, policy and receipt moved with it — by the first absolute path.
        if crate::server::roots::pinned_root().is_some_and(|pinned| {
            lean_ctx::core::pathutil::safe_canonicalize_or_self(std::path::Path::new(&pinned))
                == lean_ctx::core::pathutil::safe_canonicalize_or_self(jail_root_path)
        }) {
            return Ok(None);
        }
        // The session moved while this path was resolved (another call
        // adopted a project, or the host changed roots): judge the path by the
        // root it has now, never by the stale one it was jailed to.
        let bound = self.session.read().await.project_root.clone();
        if bound
            .as_deref()
            .is_some_and(|root| std::path::Path::new(root) != jail_root_path)
            && let Some(rejailed) =
                Self::rejail_in_bound_root(resolved, bound.as_deref(), extra_roots)
        {
            return rejailed.map(Some);
        }
        // No project bound (none, or `~`, `/`, an agent directory): the
        // first absolute path inside a project binds it through the hardened
        // adoption, once per session. Without a usable home (`HOME=/`) the
        // previous re-root below stays in charge.
        if self.session_is_unbound().await
            && lean_ctx::core::pathjail_autobind::hardened_adoption_available()
        {
            return self.adopt_project_root(resolved, extra_roots, tool).await;
        }
        self.reroot_to_derived_project(resolved, jail_root_path, extra_roots)
            .await
    }

    /// The re-root of a markerless session root (a client cwd such as
    /// `/mnt/c/Users`, #649) — and of an unbound session when the hardened
    /// adoption is unavailable — unchanged from before activation #3.
    async fn reroot_to_derived_project(
        &self,
        resolved: &std::path::Path,
        jail_root_path: &std::path::Path,
        extra_roots: &[String],
    ) -> Result<Option<std::path::PathBuf>, String> {
        // The outermost marker above a path can be `~` itself (a dotfiles
        // repository) or an agent directory: never a project to move to
        // (AP04-G6).
        let Some(new_root) = maybe_derive_project_root_from_absolute(resolved)
            .filter(|root| !lean_ctx::core::pathutil::is_broad_or_unsafe_root(root))
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
        lean_ctx::core::pathjail::jail_path_with_roots(resolved, &new_root, extra_roots)
            .map(Some)
            .map_err(|e| e.to_string())
    }

    /// The session has no real project: no root, or a root no project can
    /// be (`~`, `/`, an agent or IDE config directory).
    async fn session_is_unbound(&self) -> bool {
        self.session
            .read()
            .await
            .project_root
            .as_deref()
            .is_none_or(|root| is_suspicious_root(std::path::Path::new(root)))
    }

    /// Adopt the project containing `resolved` as the unbound session's
    /// writable project (`lean_ctx::core::pathjail_autobind::adoptable_root`),
    /// once per session, audited with the dispatching `tool`. `None` keeps
    /// today's refusal.
    ///
    /// Adoptions are serialized: a call that queued behind another one
    /// re-checks the session; when the winner bound a project the path is
    /// resolved against that root, or refused when it lies outside it.
    ///
    /// The adoption is committed and its audit write started without an await
    /// in between: `switch_to_project` installs the new session after its
    /// last await and returns synchronously, and the flag, the spawned audit
    /// write and the log line follow before the next await. A request dropped
    /// at any await point (client cancel, shutdown) therefore either left the
    /// session unbound or left it adopted with the audit write under way on
    /// the blocking pool, never adopted silently.
    async fn adopt_project_root(
        &self,
        resolved: &std::path::Path,
        extra_roots: &[String],
        tool: &str,
    ) -> Result<Option<std::path::PathBuf>, String> {
        // The filesystem probes (canonicalize, markers, ownership) run on the
        // blocking pool, bounded, before the lock: a hung mount neither parks
        // a runtime worker nor every adoption queued behind this one. The
        // verdict depends on the path alone, so judging it before the lock
        // changes nothing; the session is re-checked under the lock.
        // A session that already adopted never probes again (the lock below
        // refuses anyway); too many hung probes in flight adopt nothing.
        let already = self.autobind.try_lock().is_ok_and(|adopted| *adopted);
        let slot = (!already).then(ProbeSlot::take).flatten();
        let adoptable = match slot {
            None => None,
            Some(slot) => {
                let candidate = resolved.to_path_buf();
                let judged = tokio::time::timeout(
                    ADOPTION_PROBE_TIMEOUT,
                    tokio::task::spawn_blocking(move || {
                        let _slot = slot;
                        lean_ctx::core::pathjail_autobind::adoptable_root(&candidate)
                            .filter(|root| !lean_ctx::core::pathutil::is_broad_or_unsafe_root(root))
                    }),
                )
                .await;
                // A probe that timed out or failed adopts nothing (fail closed).
                judged.ok().and_then(Result::ok).flatten()
            }
        };
        let agent_id = self
            .agent_id
            .read()
            .await
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        // Tests force racing calls to meet here, past the unbound check and
        // before the lock, so the lock alone decides the race.
        #[cfg(test)]
        if let Some(barrier) = ADOPTION_BARRIER
            .try_with(|barrier| barrier.borrow_mut().take())
            .ok()
            .flatten()
        {
            barrier.wait().await;
        }
        let mut adopted = self.autobind.lock().await;
        let bound = self.session.read().await.project_root.clone();
        let still_open = !*adopted
            && bound
                .as_deref()
                .is_none_or(|root| is_suspicious_root(std::path::Path::new(root)));
        if !still_open {
            drop(adopted);
            return Self::rejail_in_bound_root(resolved, bound.as_deref(), extra_roots).transpose();
        }
        let Some(root) = adoptable else {
            return Ok(None);
        };
        let Some(saves) = self.switch_to_project(&root).await else {
            return Ok(None);
        };
        // The session is switched. No await until the adoption is marked and
        // its audit write is spawned (B1). The write takes the trail lock
        // (bounded, up to 750 ms), so it runs on the blocking pool, not on a
        // runtime worker; a spawned blocking task runs to completion even when
        // this request is dropped. The lock is held until it finished, so the
        // event is on disk before any later call sees the adoption settled.
        // The local audit trail names the root; the log line does not.
        *adopted = true;
        lean_ctx::core::pathjail_autobind::mark_adopted(&root);
        let audit = {
            let (tool, agent_id, root) = (tool.to_string(), agent_id, root.clone());
            tokio::task::spawn_blocking(move || {
                lean_ctx::core::pathjail_autobind::record_autobind(&tool, &agent_id, &root);
            })
        };
        tracing::warn!(
            "[SECURITY] PathJail: unbound session adopted the project of its first path (tool {tool})"
        );
        let _ = audit.await;
        drop(adopted);
        // Queued calls re-resolve against the adopted root while the stale
        // session is saved.
        for save in saves {
            let _ = save.await;
        }
        lean_ctx::core::pathjail::jail_path_with_roots(resolved, &root, extra_roots)
            .map(Some)
            .map_err(|e| e.to_string())
    }

    /// `resolved` jailed to the real project the session is bound to now.
    /// `None` when the session has no real project (none, or a suspicious
    /// root); `Some(Err)` when the path lies outside the bound project — a
    /// refusal, never a fallback to the root the path was first jailed to.
    fn rejail_in_bound_root(
        resolved: &std::path::Path,
        bound: Option<&str>,
        extra_roots: &[String],
    ) -> Option<Result<std::path::PathBuf, String>> {
        let root = std::path::Path::new(bound?);
        if is_suspicious_root(root) {
            return None;
        }
        Some(
            lean_ctx::core::pathjail::jail_path_with_roots(resolved, root, extra_roots)
                .map_err(|e| e.to_string()),
        )
    }

    /// Move the session — and the request that corrected it — to `new_root`
    /// through the same switch as a host's root change: that project's own
    /// session, the stale one saved where it was (AP04 review S4). Refused
    /// under a pinned source view, whose request project cannot follow.
    async fn reroot_to_project(&self, new_root: &std::path::Path) -> bool {
        let Some(saves) = self.switch_to_project(new_root).await else {
            return false;
        };
        for save in saves {
            let _ = save.await;
        }
        true
    }

    /// [`Self::reroot_to_project`] without awaiting the saves of the stale
    /// session: the session is switched when this returns `Some`.
    async fn switch_to_project(
        &self,
        new_root: &std::path::Path,
    ) -> Option<Vec<tokio::task::JoinHandle<()>>> {
        if lean_ctx::core::policy::runtime::inherited_source_view().is_some() {
            return None;
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
        self.switch_session_root(&current, switch).await
    }

    /// Like `resolve_path`, but returns the original path on failure instead of an error.
    pub async fn resolve_path_or_passthrough(&self, path: &str) -> String {
        self.resolve_path(path)
            .await
            .unwrap_or_else(|_| path.to_string())
    }
}
