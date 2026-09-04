#[allow(clippy::wildcard_imports)]
use super::super::*;
use super::roots_list_failure_is_permanent;

impl LeanCtxServer {
    pub(super) fn guard_role_and_policy(name: &str) -> Option<CallToolResult> {
        let role_check = role_guard::check_tool_access(name);
        if let Some(denied) = role_guard::into_call_tool_result(&role_check) {
            tracing::warn!(
                tool = name,
                role = %role_check.role_name,
                "Tool blocked by role policy"
            );
            return Some(denied);
        }

        // #673 — context-policy-pack tool gating. Additive to the role guard:
        // a pack's `allow_tools`/`deny_tools` are enforced here. No-op (allow)
        // when no policy pack is active, so existing behavior is unchanged.
        let policy_check = policy_guard::check_tool_access(name);
        if let Some(denied) = policy_guard::into_call_tool_result(&policy_check) {
            tracing::warn!(
                tool = name,
                policy = ?policy_check.policy_name,
                "Tool blocked by context policy pack"
            );
            return Some(denied);
        }
        None
    }

    pub(super) fn guard_egress(
        guard_name: &str,
        guard_args: Option<&serde_json::Map<String, serde_json::Value>>,
    ) -> Option<CallToolResult> {
        // #676 — egress / output DLP on agent writes & actions. Inspect the
        // payload of write/action tools BEFORE dispatch so a forbidden write
        // never touches disk and a forbidden command never runs. Only the
        // agent's tool-driven egress is governed here (a human's own editor
        // writes never pass through this path). No-op unless the active pack has
        // an `[egress]` section. Payload mapping (incl. all ctx_patch bodies)
        // lives in `core::egress::write_payload` — shared with `policy enforce`.
        if let Some(active) = crate::core::policy::runtime::active()
            && active.egress.is_active()
        {
            let target = crate::core::egress::write_payload(guard_name, guard_args);
            if let Some((payload, kind)) = target {
                if let Some(reason) = active.egress.check_content(&payload, &active.redaction) {
                    tracing::warn!(tool = guard_name, %reason, "agent egress blocked by policy");
                    policy_guard::audit_egress(guard_name, &reason);
                    return Some(CallToolResult::success(vec![ContentBlock::text(format!(
                        "[POLICY BLOCKED] {kind} blocked by context policy pack egress rule \
                         ({reason}). Review the effective local or organization \
                         egress policy."
                    ))]));
                }
                if let Some(max) = active.egress.max_writes_per_min
                    && !crate::core::egress::check_rate(max)
                {
                    tracing::warn!(tool = guard_name, max, "agent egress rate limit exceeded");
                    policy_guard::audit_egress(guard_name, "rate-limit");
                    return Some(CallToolResult::success(vec![ContentBlock::text(format!(
                        "[POLICY BLOCKED] {kind} rate limit exceeded ({max}/min) by context \
                         policy pack. Slow agent writes/actions or adjust .lean-ctx/policy.toml."
                    ))]));
                }
            }
        }
        None
    }

    pub(super) async fn guard_workflow(&self, name: &str) -> Option<CallToolResult> {
        if name != "ctx_workflow" {
            let active = self.workflow.read().await.clone();
            if let Some(run) = active {
                if run.current == "done" || is_workflow_stale(&run) {
                    let mut wf = self.workflow.write().await;
                    *wf = None;
                    let _ = crate::core::workflow::clear_active();
                } else if !WORKFLOW_PASSTHROUGH_TOOLS.contains(&name)
                    && let Some(state) = run.spec.state(&run.current)
                    && let Some(allowed) = &state.allowed_tools
                {
                    let allowed_ok = allowed.iter().any(|t| t == name);
                    if !allowed_ok {
                        let mut shown = allowed.clone();
                        shown.sort();
                        shown.truncate(30);
                        return Some(CallToolResult::success(vec![ContentBlock::text(format!(
                            "Tool '{name}' blocked by workflow '{}' (state: {}). Allowed: {}. Use ctx_workflow(action=\"stop\") to exit.",
                            run.spec.name,
                            run.current,
                            shown.join(", ")
                        ))]));
                    }
                }
            }
        }
        None
    }

    /// Resolve project root from MCP client roots (once per session).
    /// Called on the first tool call. If the client supports `roots/list`,
    /// we query it and pick the best root with project markers.
    ///
    /// Roots is SEP-2577-deprecated in rmcp 2.0 but still fully functional; we
    /// keep it for client-driven project-root auto-detection until MCP removes it.
    #[expect(deprecated)]
    pub(super) async fn resolve_roots_once(&self) {
        use std::sync::atomic::Ordering;
        if !self.has_client_roots.load(Ordering::Relaxed) {
            return;
        }
        if self.roots_resolved.swap(true, Ordering::Relaxed) {
            return;
        }
        let peer_guard = self.peer.read().await;
        let Some(peer) = peer_guard.as_ref() else {
            return;
        };
        let list_result = match peer.list_roots().await {
            Ok(r) => r,
            Err(e) => {
                let permanent = roots_list_failure_is_permanent(&e);
                const MAX_ATTEMPTS: u32 = 3;
                let attempts = self.roots_list_attempts.fetch_add(1, Ordering::Relaxed) + 1;
                if !permanent && attempts < MAX_ATTEMPTS {
                    self.roots_resolved.store(false, Ordering::Relaxed);
                }
                tracing::warn!(
                    "roots/list failed (attempt {attempts}, {}): {e}",
                    if permanent {
                        "client does not implement it — giving up"
                    } else if attempts < MAX_ATTEMPTS {
                        "will retry on a later tool call"
                    } else {
                        "retry budget exhausted"
                    }
                );
                return;
            }
        };
        drop(peer_guard);

        let uris: Vec<String> = list_result.roots.iter().map(|r| r.uri.clone()).collect();
        let validated_paths = roots::valid_dir_paths_from_uris(&uris);
        let Some(new_root) = roots::best_root_from_uris(&uris) else {
            return;
        };
        // Defense-in-depth: never adopt a broad/unsafe root (HOME, `/`, agent
        // sandbox dirs) even if the client reports it — it would pollute the
        // session and resolve relative paths against the wrong tree.
        if crate::core::pathutil::is_broad_or_unsafe_root(std::path::Path::new(&new_root)) {
            tracing::warn!("MCP roots: ignoring unsafe project root {new_root}");
            return;
        }

        let mut session = self.session.write().await;
        let old_root = session.project_root.clone();

        let other_roots: Vec<String> = validated_paths
            .iter()
            .filter(|p| p.as_str() != new_root)
            .cloned()
            .collect();
        if !other_roots.is_empty() {
            session.extra_roots = other_roots;
            tracing::info!(
                "MCP roots: {} extra root(s) registered",
                session.extra_roots.len()
            );
        }

        if old_root.as_deref() == Some(&new_root) {
            let _ = session.save();
            return;
        }
        tracing::info!(
            "MCP roots: switching project root from {:?} to {new_root}",
            old_root
        );
        if let Some(existing) =
            crate::core::session::SessionState::load_latest_for_project_root(&new_root)
        {
            *session = existing;
            session.extra_roots = validated_paths
                .iter()
                .filter(|p| p.as_str() != new_root)
                .cloned()
                .collect();
        }
        session.project_root = Some(new_root);
        let _ = session.save();
        drop(session);
        // Indices warm lazily on first use of a tool that needs them (#152) —
        // the dispatch path for this very call handles it via
        // `index_orchestrator::ensure_warm_for_tool`, so no eager scan here.
    }
}
