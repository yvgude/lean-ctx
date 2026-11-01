// SPDX-License-Identifier: Apache-2.0
//! `rmcp::ServerHandler` trait implementation for [`LeanCtxServer`].
//!
//! Split out of `server/mod.rs`; `use super::*` re-imports the parent module’s
//! aliases and sibling submodules. Methods attach to `LeanCtxServer` regardless
//! of which module the impl block lives in.

#[allow(clippy::wildcard_imports)]
use super::*;

/// rmcp moves wire `_meta` into RequestContext before calling the handler.
/// Retain the existing guarded dispatcher and progress-token readers as owners.
fn restore_tool_meta(request: &mut CallToolRequestParams, transport_meta: &rmcp::model::Meta) {
    if !transport_meta.is_empty() {
        request
            .meta
            .get_or_insert_with(rmcp::model::Meta::new)
            .extend(transport_meta.clone());
    }
}

/// Builds the advertised MCP server capabilities.
///
/// `tools` is always enabled **and** always declares `listChanged`: lean-ctx
/// emits `notifications/tools/list_changed` whenever a tool call mutates the
/// dynamic tool set (see `dispatch::send_tools_list_changed`). The MCP spec only
/// permits sending that notification when the matching capability was advertised
/// — otherwise a strict client (e.g. Claude Code) treats it as a protocol
/// violation and drops the entire tool set ("connected, but no tools"). The
/// `resources`/`prompts` surfaces stay client-gated so we never advertise a
/// surface the connected client cannot use.
fn server_capabilities(resources: bool, prompts: bool) -> ServerCapabilities {
    match (resources, prompts) {
        (true, true) => ServerCapabilities::builder()
            .enable_tools()
            .enable_tool_list_changed()
            .enable_resources()
            .enable_resources_subscribe()
            .enable_prompts()
            .build(),
        (true, false) => ServerCapabilities::builder()
            .enable_tools()
            .enable_tool_list_changed()
            .enable_resources()
            .enable_resources_subscribe()
            .build(),
        (false, true) => ServerCapabilities::builder()
            .enable_tools()
            .enable_tool_list_changed()
            .enable_prompts()
            .build(),
        (false, false) => ServerCapabilities::builder()
            .enable_tools()
            .enable_tool_list_changed()
            .build(),
    }
}

fn suppress_initialize_instructions(client_id: &str) -> bool {
    let cfg = crate::core::config::Config::load();

    cfg.declines_rule_steering() || matches!(client_id, "antigravity" | "gemini-cli")
}

fn engine_server_info() -> Implementation {
    Implementation::new("lean-ctx", env!("CARGO_PKG_VERSION"))
        .with_title("LeanCTX Engine")
        .with_description("Context Gateway for AI Systems: task-relevant context, supported controls, local knowledge and observable evidence.")
        .with_website_url("https://leanctx.com")
}

impl ServerHandler for LeanCtxServer {
    fn get_info(&self) -> ServerInfo {
        let capabilities = server_capabilities(true, true);

        let instructions = crate::instructions::build_instructions(CrpMode::effective());

        InitializeResult::new(capabilities)
            .with_server_info(engine_server_info())
            .with_instructions(instructions)
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        let name = request.client_info.name.clone();
        tracing::info!("MCP client connected: {:?}", name);
        *self.client_name.write().await = name.clone();
        *self.peer.write().await = Some(context.peer.clone());
        // Attribute this connection's bus presence to its host, recorded
        // before any registration below reads it. A shared server serves
        // several clients from one process, so it names no single host.
        if self.session_mode != crate::tools::SessionMode::Shared {
            crate::core::client_capabilities::record_handshake(&name, &request.client_info.version);
        }
        // Per-agent savings attribution: a personal server serves exactly one
        // client, so its ledger events can carry that client's identity. The
        // shared (multi-client) mode keeps the process-level fallback.
        if self.session_mode != crate::tools::SessionMode::Shared {
            let bus_id = self.presence_agent_id.read().await.clone();
            crate::core::agent_identity::set_session_attribution(&name, bus_id.as_deref());
        }

        if self.session_mode != crate::tools::SessionMode::Shared {
            if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
                let radar = data_dir.join("context_radar.jsonl");
                if radar.exists() {
                    let prev = data_dir.join("context_radar.prev.jsonl");
                    let _ = std::fs::rename(&radar, &prev);
                }
            }
        }

        let has_roots = request.capabilities.roots.is_some();
        self.has_client_roots
            .store(has_roots, std::sync::atomic::Ordering::Relaxed);
        if has_roots {
            tracing::info!("Client supports MCP roots/list — will resolve on first tool call");
        }

        let explicit_env_root = roots::root_from_env();
        let workspace_selection = explicit_env_root
            .is_none()
            .then(roots::workspace_selection_from_env)
            .flatten();
        let env_root = explicit_env_root.or_else(|| {
            workspace_selection
                .as_ref()
                .map(|selection| selection.chosen_root.clone())
        });
        let derived_root = derive_project_root_from_cwd();
        let effective_root = roots::session_root(env_root, derived_root);
        // AP04-G7: the first of several workspace folders stays the choice
        // (#699); recording it lets `ctx_session status` and `/api/scope`
        // name it and how to change it. Only when it is the session's root.
        if self.session_mode != crate::tools::SessionMode::Shared
            && !crate::core::workspace_authority::is_configured()
            && let Some(selection) = workspace_selection
                .filter(|selection| effective_root.as_ref() == Some(&selection.chosen_root))
        {
            crate::core::root_selection::record(&selection);
        }

        let cwd_str = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        // A shared session keeps the root it was created for: the serving
        // process's own env and working directory say nothing about the
        // client, and every client of that session would move with it.
        let process_roots_apply = self.session_mode != crate::tools::SessionMode::Shared;
        {
            let mut session = self.session.write().await;
            if process_roots_apply && !cwd_str.is_empty() {
                session.shell_cwd = Some(cwd_str.clone());
            }
            if let Some(root) = effective_root.as_ref().filter(|_| process_roots_apply) {
                session.project_root = Some(root.clone());
                tracing::info!("Project root set to: {root}");
                // Cursor multi-root: register sibling workspace folders as extra
                // trusted roots so explicit cross-folder paths are not rejected
                // by the path jail (#699).
                for other in roots::workspace_roots_from_env() {
                    if &other != root && !session.extra_roots.contains(&other) {
                        session.extra_roots.push(other);
                    }
                }
            } else if process_roots_apply && let Some(ref root) = session.project_root {
                // A previously persisted session may carry a contaminated root
                // (e.g. HOME from an older build or a client that reported HOME
                // as its workspace). Drop it unless it is a real, safe project
                // dir — otherwise PROJECT MEMORY leaks across projects.
                let root_path = std::path::Path::new(root);
                let root_has_marker = has_project_marker(root_path);
                let root_str = root_path.to_string_lossy();
                let root_suspicious = crate::core::pathutil::is_broad_or_unsafe_root(root_path)
                    || root_str.contains("/var/folders/")
                    || root_str.contains("/tmp/")
                    || root_str.contains("/.lmstudio")
                    || root_str.contains("\\AppData\\Local\\Temp")
                    || root_str.contains("\\Temp\\")
                    || root_str.contains("\\.lmstudio");
                if root_suspicious && !root_has_marker {
                    tracing::info!("Dropping suspicious persisted project root: {root}");
                    session.project_root = None;
                }
            }
            let cfg_extra = crate::core::config::Config::load().extra_roots;
            if !cfg_extra.is_empty() {
                let existing: std::collections::HashSet<_> =
                    session.extra_roots.iter().cloned().collect();
                for r in cfg_extra {
                    if !existing.contains(&r) {
                        session.extra_roots.push(r);
                    }
                }
            }
            // A launcher-bound Workspace replaces every root found above
            // (#2120): env/cwd roots, workspace folders and configured extra
            // roots grant nothing; an unusable Workspace leaves no root.
            match crate::core::workspace_authority::load_current() {
                crate::core::workspace_authority::Authority::Legacy => {}
                crate::core::workspace_authority::Authority::Workspace(snapshot) => {
                    let primary = snapshot.primary.to_string_lossy().into_owned();
                    tracing::info!(
                        "Workspace {} rev {} bound: project root {primary}",
                        snapshot.id,
                        snapshot.revision
                    );
                    session.extra_roots = snapshot.additional_root_strings();
                    session.shell_cwd = Some(primary.clone());
                    session.project_root = Some(primary);
                }
                crate::core::workspace_authority::Authority::Unavailable(reason) => {
                    tracing::error!(
                        "{}",
                        crate::core::workspace_authority::unavailable_reason(&reason)
                    );
                    session.extra_roots.clear();
                    session.project_root = None;
                    session.shell_cwd = None;
                }
            }
            session.refresh_handoff_context();

            if self.session_mode == crate::tools::SessionMode::Shared {
                if let Some(ref root) = session.project_root
                    && let Some(ref rt) = self.context_os
                {
                    rt.shared_sessions.persist_best_effort(
                        root,
                        &self.workspace_id,
                        &self.channel_id,
                        &session,
                    );
                    rt.metrics.record_session_persisted();
                }
            } else if let Err(e) = session.save() {
                // Synchronous on purpose: this runs once per connection, before
                // any tool can be called, and the session must be on disk before
                // it is. The wait is bounded by `PreparedSave`'s lock deadline,
                // so it cannot become the wedge #1783 describes.
                tracing::warn!("lean-ctx: failed to persist session state: {e}");
            }
        }

        // Indices are warmed lazily on first use of a tool that needs them
        // (issue #152), not eagerly here — a session that only uses
        // ctx_read/ctx_shell/ctx_tree must not pay a full graph + BM25 scan.
        // See `index_orchestrator::ensure_warm_for_tool`, driven from dispatch.

        let agent_name = name.clone();
        // A shared session's clients register on its own root, not the
        // serving process's (AP04-G5).
        let agent_root = if process_roots_apply {
            effective_root.clone().unwrap_or_default()
        } else {
            self.session
                .read()
                .await
                .project_root
                .clone()
                .unwrap_or_default()
        };
        let agent_id_handle = self.agent_id.clone();
        let presence_agent_id_handle = self.presence_agent_id.clone();
        let presence_role_handle = self.presence_role.clone();
        tokio::task::spawn_blocking(move || {
            if std::env::var("LEAN_CTX_HEADLESS").is_ok() {
                return;
            }

            // The startup presence predates the handshake; name its host now
            // rather than after the maintenance below (or never, without a root).
            if let Err(error) =
                crate::core::agents::AgentRegistry::record_transport_client_persistent(
                    &crate::core::agents::TransportClient::current(),
                )
            {
                tracing::warn!("lean-ctx: failed to attribute MCP presence to its host: {error}");
            }

            // Avoid startup stampedes when multiple agent sessions initialize at once.
            // These are best-effort maintenance tasks; it's fine to skip if another
            // lean-ctx instance is already doing them.
            let maintenance = crate::core::startup_guard::try_acquire_lock(
                "startup-maintenance",
                std::time::Duration::from_secs(2),
                std::time::Duration::from_mins(2),
            );
            if maintenance.is_some() {
                if let Some(home) = dirs::home_dir() {
                    startup_rule_maintenance_at_home(&home);
                    // The on-demand SKILL.md belongs to the same steering surface
                    // as the rules block: the session-start heal writes rules for
                    // every detected client, so a fresh machine that never ran
                    // `lean-ctx setup` otherwise carries a permanent doctor
                    // warning ("SKILL.md not installed"). Idempotent + gated on
                    // the same opt-outs as setup (rules_injection=off inside,
                    // auto_inject_skills=Some(false) here).
                    if crate::core::config::Config::load()
                        .setup
                        .should_inject_skills()
                    {
                        let _ = crate::rules_inject::install_all_skills(&home);
                    }
                }
                crate::hooks::refresh_installed_hooks();
                crate::shell_hook::refresh_installed_shell_hooks();
                crate::core::version_check::check_background();
                // Enforce the on-disk budget: prune accumulated quarantined BM25
                // indexes and cap the archive FTS DB (#2364). Silent (tracing
                // only) so it never corrupts the MCP stdio protocol.
                let _ = crate::core::storage_maintenance::run_quiet();
            }
            drop(maintenance);

            if !agent_root.is_empty() {
                let (effective_role, role_source) =
                    resolve_session_role(&agent_name, crate::core::agents::declared_session_role());

                // The active role is process-wide: a client of a shared server
                // must not set it for every other client. Its own role is
                // still remembered for its presence below.
                if process_roots_apply {
                    let _ = crate::core::roles::set_active_role_with_source(&effective_role, true);
                }
                // #1766: remember the resolved role so the fail-closed presence
                // retry re-registers with it, not the `context-engine` placeholder.
                if let Ok(mut guard) = presence_role_handle.try_write() {
                    *guard = Some(effective_role.clone());
                }

                let registration = crate::core::agents::AgentRegistry::register_mcp_process_as(
                    &agent_root,
                    &effective_role,
                    role_source,
                    None,
                );
                let id = match registration {
                    Ok(id) => Some(id),
                    Err(error) => {
                        tracing::warn!("lean-ctx: agent registration failed: {error}");
                        None
                    }
                };
                if let (Some(id), Ok(mut guard)) = (id.as_ref(), agent_id_handle.try_write()) {
                    *guard = Some(id.clone());
                }
                if let (Some(id), Ok(mut guard)) = (id, presence_agent_id_handle.try_write()) {
                    *guard = Some(id);
                }
            }
        });

        let client_caps = crate::core::client_capabilities::ClientMcpCapabilities::detect(&name);
        tracing::info!("Client capabilities: {}", client_caps.format_summary());

        {
            let cfg = crate::core::config::Config::load();
            let cats = cfg.default_tool_categories_effective();
            dynamic_tools::init_from_config(&cats);
        }

        if let Some(max) = client_caps.max_tools
            && let Ok(mut dt) = dynamic_tools::global().lock()
        {
            dt.set_supports_list_changed(true);
            if max < 100 {
                dt.unload_category(dynamic_tools::ToolCategory::Debug);
                dt.unload_category(dynamic_tools::ToolCategory::Memory);
            }
        } else if client_caps.dynamic_tools
            && let Ok(mut dt) = dynamic_tools::global().lock()
        {
            dt.set_supports_list_changed(true);
        }

        crate::core::client_capabilities::set_detected(&client_caps);

        let session = self.session.read().await.clone();

        let capabilities = server_capabilities(client_caps.resources, client_caps.prompts);

        let result = InitializeResult::new(capabilities).with_server_info(engine_server_info());

        // GH #1447: Antigravity/Gemini CLI disconnect when the initialize
        // response contains `instructions`. The MCP spec marks it optional,
        // but these clients fail on any unexpected field in the JSON envelope.
        //
        // GH #1599/#1758: both `rules_injection = "off"` and an explicit
        // `setup.auto_inject_rules = false` decline lean-ctx rule steering.
        // The initialize `instructions` field is that same steering block
        // delivered over a different channel, so the central policy predicate
        // must govern this surface too.
        if suppress_initialize_instructions(client_caps.client_id.as_str()) {
            Ok(result)
        } else {
            let instructions = crate::instructions::build_instructions_with_client_and_session(
                CrpMode::effective(),
                &name,
                &session,
            );
            Ok(result.with_instructions(instructions))
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        use crate::server::tool_visibility::CandidateSet;
        // Panic guard (mirrors call_tool): a panic while filtering the registry /
        // touching the dynamic-tools mutex must not kill the rmcp request task.
        use std::panic::AssertUnwindSafe;
        let computed = AssertUnwindSafe(async {
            let cfg = crate::core::config::Config::load();
            let disabled = cfg.disabled_tools_effective();
            let raw_profile = cfg.tool_profile_effective();
            let tool_profile = crate::server::tool_visibility::resolve_auto_profile(&raw_profile);
            crate::server::tool_visibility::record_auto_turn();
            // A profile is "explicit" when the user opted into one (config field,
            // env var, or a custom tools list). Without an explicit choice we keep
            // the token-lean lazy core set as the default. With one, the profile is
            // authoritative and resolves against the full registry, so e.g.
            // `standard` advertises its full balanced set instead of the accidental
            // `core ∩ standard` intersection.
            let explicit_profile = crate::server::tool_visibility::explicit_profile(&cfg);

            let client = self.client_name.read().await.clone();
            let hook_covered = is_client_hook_covered(&client);

            let candidate = crate::server::tool_visibility::candidate_set(
                &crate::server::tool_visibility::CandidateInputs {
                    full_mode: crate::tool_defs::is_full_mode(),
                    unified_env: std::env::var("LEAN_CTX_UNIFIED").is_ok(),
                    explicit_profile,
                    hook_covered,
                },
            );
            let all_tools = match candidate {
                CandidateSet::Full | CandidateSet::ProfileAuthoritative => {
                    if let Some(ref reg) = self.registry {
                        reg.tool_defs()
                    } else {
                        // Unreachable in production: every constructor sets a registry
                        // (locked by `production_server_always_has_registry`). If it
                        // ever fires, the advertised static defs can drift from what
                        // dispatch (which needs the registry) can execute — make it loud.
                        tracing::error!(
                            "list_tools served WITHOUT a tool registry (full mode) — advertising \
                             static granular defs that dispatch cannot run; tools may drift from handlers."
                        );
                        crate::tool_defs::granular_tool_defs()
                    }
                }
                CandidateSet::Unified => crate::tool_defs::unified_tool_defs(),
                // GH #1474: ShadowOnly is never returned anymore; treat as LazyCore.
                CandidateSet::ShadowOnly |
                CandidateSet::LazyCore => {
                    if let Some(ref reg) = self.registry {
                        let core_names = crate::tool_defs::core_tool_names();
                        reg.tool_defs()
                            .into_iter()
                            .filter(|t| core_names.contains(&t.name.as_ref()))
                            .collect()
                    } else {
                        // Unreachable in production (see above); loud if it ever fires.
                        tracing::error!(
                            "list_tools served WITHOUT a tool registry (lazy mode) — advertising \
                             static lazy defs that dispatch cannot run; tools may drift from handlers."
                        );
                        crate::tool_defs::lazy_tool_defs()
                    }
                }
            };
            let quirks = crate::server::tool_visibility::ClientQuirks::resolve(&client, candidate);

            let active_role = crate::core::roles::active_role();
            let tools: Vec<_> = all_tools
                .into_iter()
                .filter(|t| {
                    let name = t.name.as_ref();
                    crate::server::tool_visibility::is_tool_visible(
                        name,
                        &tool_profile,
                        &disabled,
                        quirks,
                        active_role.is_tool_allowed(name),
                    )
                })
                .collect();

            // Guarantee the universal invoker is advertised in non-full mode. Lazy
            // and profile filtering hide most tools; without ctx_call a static-list
            // client (one that only calls advertised tools) could not reach them.
            // ctx_call enforces the same role/workflow gates on the inner tool.
            let tools = {
                use crate::server::tool_visibility::INVOKER;
                let mut tools = tools;
                let already = tools.iter().any(|t| t.name.as_ref() == INVOKER);
                if crate::server::tool_visibility::needs_invoker(
                    crate::tool_defs::is_full_mode(),
                    already,
                    active_role.is_tool_allowed(INVOKER),
                    &disabled,
                ) && let Some(def) = self.registry.as_ref().and_then(|reg| {
                    reg.tool_defs()
                        .into_iter()
                        .find(|t| t.name.as_ref() == INVOKER)
                }) {
                    tools.push(def);
                }
                tools
            };

            let tools = {
                let Ok(dyn_state) = dynamic_tools::global().lock() else {
                    tracing::warn!(
                        "dynamic_tools mutex poisoned in list_tools; returning unfiltered"
                    );
                    return Ok(ListToolsResult {
                        tools,
                        ..Default::default()
                    });
                };
                // The lazy category gate (load tools on demand for dynamic_tools
                // clients) only applies to the *default* lean-core surface. When the
                // user opted into an explicit profile, that profile IS the
                // authoritative surface — gating it by category would silently drop
                // profile-enabled tools like Standard's ctx_architecture /
                // ctx_semantic_search for Codex et al. (#358), so the advertised set
                // would no longer match `lean-ctx tools show`.
                if crate::server::tool_visibility::category_gate_applies(
                    dyn_state.supports_list_changed(),
                    explicit_profile,
                ) {
                    tools
                        .into_iter()
                        .filter(|t| dyn_state.is_tool_active(t.name.as_ref()))
                        .collect()
                } else {
                    tools
                }
            };

            let tools = {
                let active = self.workflow.read().await.clone();
                if let Some(run) = active {
                    if run.current == "done" || is_workflow_stale(&run) {
                        let mut wf = self.workflow.write().await;
                        *wf = None;
                        let _ = crate::core::workflow::clear_active();
                    } else if let Some(state) = run.spec.state(&run.current)
                        && state.allowed_tools.is_some()
                    {
                        return Ok(ListToolsResult {
                            tools: tools
                                .into_iter()
                                .filter(|t| workflow_lists(&run, t.name.as_ref()))
                                .collect(),
                            ..Default::default()
                        });
                    }
                }
                tools
            };

            let tools = {
                let cfg = crate::core::config::Config::load();
                let level = crate::core::config::CompressionLevel::effective(&cfg);
                let mode =
                    crate::core::terse::mcp_compress::DescriptionMode::from_compression_level(
                        &level,
                    );
                if mode == crate::core::terse::mcp_compress::DescriptionMode::Full {
                    tools
                } else {
                    tools
                        .into_iter()
                        .map(|mut t| {
                            let compressed = crate::core::terse::mcp_compress::compress_description(
                                t.name.as_ref(),
                                t.description.as_deref().unwrap_or(""),
                                mode,
                            );
                            t.description = Some(compressed.into());
                            t
                        })
                        .collect()
                }
            };


            // R28: Kernel schema optimization — budget-aware description compression.
            let tools = crate::server::schema_hook::optimize_tools(tools, &client);
            // #1008: When ctx_patch is hidden for this client, scrub references
            // from other tools' descriptions so the LLM never sees the name and
            // won't attempt to call it. Replace with ctx_edit (visible alternative).
            let tools = if quirks.hide_ctx_patch {
                tools
                    .into_iter()
                    .map(|mut t| {
                        if let Some(ref desc) = t.description
                            && desc.contains("ctx_patch")
                        {
                            t.description =
                                Some(desc.replace("ctx_patch", "ctx_edit").into());
                        }
                        t
                    })
                    .collect()
            } else {
                tools
            };

            // Plan mode: restrict advertised tools to the curated read-only set.
            let tools = {
                use crate::tools::InteractionMode;
                let mode = InteractionMode::from_u8(
                    self.interaction_mode
                        .load(std::sync::atomic::Ordering::Relaxed),
                );
                if mode == InteractionMode::Plan {
                    let plan_set =
                        crate::core::editor_registry::plan_mode::plan_mode_tools();
                    tools
                        .into_iter()
                        .filter(|t| {
                            let n = t.name.as_ref();
                            plan_set.contains(&n) || n == "ctx_call"
                        })
                        .collect()
                } else {
                    tools
                }
            };

            Ok(ListToolsResult {
                tools,
                ..Default::default()
            })
        })
        .catch_unwind()
        .await;
        computed.unwrap_or_else(|_| {
            // A panic here must NOT leave the agent tool-less — that is
            // indistinguishable from "MCP totally failed" and gives the user no
            // recovery path. Fall back to the static lazy-core defs (a pure,
            // panic-free function) so ctx_read/ctx_shell/ctx_call stay available
            // even if the dynamic/registry path blew up.
            tracing::error!(
                "list_tools panicked; serving the static lazy-core tool set as a fallback"
            );
            Ok(ListToolsResult {
                tools: crate::tool_defs::lazy_tool_defs(),
                ..Default::default()
            })
        })
    }

    fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<rmcp::model::ListPromptsResult, ErrorData>> {
        std::future::ready(Ok(rmcp::model::ListPromptsResult::with_all_items(
            prompts::list_prompts(),
        )))
    }

    async fn get_prompt(
        &self,
        request: rmcp::model::GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::GetPromptResult, ErrorData> {
        let ledger = self.ledger.read().await;
        match prompts::get_prompt(&request, &ledger) {
            Some(result) => Ok(result),
            None => Err(ErrorData::invalid_params(
                format!("Unknown prompt: {}", request.name),
                None,
            )),
        }
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<rmcp::model::ListResourcesResult, rmcp::ErrorData>> {
        std::future::ready(Ok(rmcp::model::ListResourcesResult::with_all_items(
            resources::list_resources(),
        )))
    }

    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResult, rmcp::ErrorData> {
        let ledger = self.ledger.read().await;
        if let Some(contents) = resources::read_resource(&request.uri, &ledger) {
            return Ok(rmcp::model::ReadResourceResult::new(contents));
        }

        // GH #1418: agents sometimes call resources/read with file:// URIs
        // instead of ctx_read — serve the file directly rather than erroring.
        let session = self.session.read().await;
        let root = session.project_root.as_deref();
        match file_resource::read_file_resource(&request.uri, root, &session.extra_roots) {
            Ok(contents) => Ok(rmcp::model::ReadResourceResult::new(contents)),
            Err(file_resource::FileResourceError::NotAFilePath) => {
                Err(rmcp::ErrorData::resource_not_found(
                    resources::unknown_resource_message(&request.uri),
                    None,
                ))
            }
            Err(e) => Err(rmcp::ErrorData::resource_not_found(e.to_string(), None)),
        }
    }

    async fn call_tool(
        &self,
        mut request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        use std::panic::AssertUnwindSafe;

        // In flight until this call returns — the idle policy never releases
        // or exits under a running call.
        let _activity = crate::server::mcp_idle::begin_call();
        restore_tool_meta(&mut request, &context.meta);

        let progress_token = request
            .meta
            .as_ref()
            .and_then(rmcp::model::Meta::get_progress_token);
        if let Some(ref token) = progress_token {
            let sender =
                crate::server::progress::ProgressSender::new(context.peer.clone(), token.clone());
            *self
                .progress_sender
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
        }

        let tool_name_for_panic = request.name.as_ref().to_string();
        let args_fp_for_panic = request
            .arguments
            .as_ref()
            .map(|a| {
                crate::core::loop_detection::LoopDetector::fingerprint(&serde_json::Value::Object(
                    a.clone(),
                ))
            })
            .unwrap_or_default();

        let loop_detector = self.loop_detector.clone();
        let ct = context.ct.clone();
        // The host's own id of this call, if it names one: noted with the
        // delivery so the proxy can tell which call these bytes answered.
        let host_call_id = crate::server::call_identity::host_call_id(request.meta.as_ref());

        // #1781: scope the request's cancellation token for the whole dispatch
        // so a tool that waits on a child process can observe an abandoned call
        // instead of holding its blocking-pool slot until its own soft cap.
        let Ok(result) = AssertUnwindSafe(crate::server::tool_trait::REQUEST_CT.scope(
            ct.clone(),
            Box::pin(crate::server::call_identity::scoped(
                host_call_id,
                self.call_tool_guarded(request),
            )),
        ))
        .catch_unwind()
        .await
        else {
            // A panic message can quote the content it failed on (a slice
            // boundary inside a string, a parse error): the log line names
            // the event only (AP10-R4).
            tracing::error!("call_tool panicked (tool_panic)");

            if let Ok(mut detector) =
                tokio::time::timeout(std::time::Duration::from_secs(1), loop_detector.write()).await
            {
                detector.record_error_outcome(&tool_name_for_panic, &args_fp_for_panic);
            }

            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "ERROR: lean-ctx internal error. The MCP server is still running. \
                 Please retry or use a different approach."
                    .to_string(),
            )]));
        };
        // #1265: If the client cancelled this request while the tool was
        // executing, drop the result instead of replying to a stale ID.
        if ct.is_cancelled() {
            tracing::warn!(
                "tool '{tool_name_for_panic}' completed after client cancellation — dropping result"
            );
            return Err(ErrorData::internal_error(
                "request was cancelled by client",
                None,
            ));
        }
        result
    }

    async fn on_roots_list_changed(
        &self,
        _context: rmcp::service::NotificationContext<RoleServer>,
    ) {
        tracing::info!("Received roots/list_changed — will re-resolve on next tool call");
        self.roots_resolved
            .store(false, std::sync::atomic::Ordering::Relaxed);
        // Fresh client signal — restore the transient-failure retry budget.
        self.roots_list_attempts
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

fn startup_rule_maintenance_at_home(home: &std::path::Path) {
    let cfg = crate::core::config::Config::load();
    if cfg.declines_rule_steering() {
        return;
    }
    let _ = crate::rules_inject::inject_all_rules(home);
}

/// Whether the tool listing shows `tool` under the active `run` whose current
/// state gates tools: exactly when the call gate (`workflow::tool_gate`) lets
/// it through. For a real run that is its allowed tools plus the
/// pass-through tools, as before. For the stand-in of a withheld workflow the
/// pass-through tools are NOT added — every call to them is refused — so the
/// listing is the workflow control tool and the wrappers that reach it.
fn workflow_lists(run: &crate::core::workflow::WorkflowRun, tool: &str) -> bool {
    crate::core::workflow::tool_gate(run, tool, WORKFLOW_PASSTHROUGH_TOOLS)
        == crate::core::workflow::ToolGate::Pass
}

/// The session role and its provenance: a role stated through the
/// environment is declared; the client-name heuristic and the `coder`
/// fallback are assumptions and are reported as such.
fn resolve_session_role(
    client_name: &str,
    declared: Option<String>,
) -> (String, crate::core::agents::RoleSource) {
    if let Some(role) = declared {
        return (role, crate::core::agents::RoleSource::Declared);
    }
    let heuristic_role = match client_name.to_lowercase().as_str() {
        n if n.contains("cursor") => "coder",
        n if n.contains("claude") => "coder",
        n if n.contains("codebuddy") => "coder",
        n if n.contains("codex") => "coder",
        n if n.contains("antigravity") || n.contains("gemini") => "coder",
        n if n.contains("review") => "reviewer",
        n if n.contains("test") => "debugger",
        _ => "coder",
    };
    (
        heuristic_role.to_string(),
        crate::core::agents::RoleSource::Assumed,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_role_reports_whether_it_was_assumed() {
        use crate::core::agents::RoleSource;
        assert_eq!(
            resolve_session_role("claude-code", None),
            ("coder".to_string(), RoleSource::Assumed)
        );
        assert_eq!(
            resolve_session_role("unknown-host", None),
            ("coder".to_string(), RoleSource::Assumed)
        );
        assert_eq!(
            resolve_session_role("code-review-bot", None),
            ("reviewer".to_string(), RoleSource::Assumed)
        );
        assert_eq!(
            resolve_session_role("claude-code", Some("reviewer".to_string())),
            ("reviewer".to_string(), RoleSource::Declared)
        );
    }

    /// The listing under a workflow is what the call gate lets through: the
    /// stand-in for a withheld workflow lists no pass-through tool, a real
    /// run lists its allowed tools and the pass-through tools as before.
    #[test]
    fn the_workflow_listing_matches_the_call_gate() {
        use crate::core::context_admission::stored::lineage_support::{governed, project, scoped};
        use crate::core::workflow::gate::tests::review;
        use crate::core::workflow::{is_withheld_sentinel, load_active, save_active};
        let _data = governed();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let (_other_directory, other) = project();
        scoped(&root, || save_active(&review())).expect("saved in its project");
        let closed = scoped(&other, load_active)
            .expect("load")
            .expect("a withheld workflow is still a workflow");
        assert!(is_withheld_sentinel(&closed));
        let listed = |run: &crate::core::workflow::WorkflowRun| -> Vec<&'static str> {
            WORKFLOW_PASSTHROUGH_TOOLS
                .iter()
                .chain(["ctx_call", "ctx_overview", "ctx_shell"].iter())
                .copied()
                .filter(|tool| workflow_lists(run, tool))
                .collect()
        };
        assert_eq!(
            listed(&closed),
            ["ctx", "ctx_workflow", "ctx_call"],
            "no pass-through tool is listed under the stand-in"
        );
        for refused in ["ctx_read", "ctx_search", "ctx_session", "ctx_ledger"] {
            assert!(WORKFLOW_PASSTHROUGH_TOOLS.contains(&refused));
            assert!(!workflow_lists(&closed, refused), "{refused}");
        }

        let real = scoped(&root, load_active).expect("load").expect("active");
        let mut expected = WORKFLOW_PASSTHROUGH_TOOLS.to_vec();
        expected.push("ctx_overview");
        assert_eq!(listed(&real), expected, "a real run lists as before");
    }

    #[test]
    fn initialize_instructions_respect_explicit_auto_inject_false() {
        let _guard = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("config");

        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            "[setup]\nauto_inject_rules = false\n",
        )
        .unwrap();

        let _config_guard = crate::test_env::EnvVarGuard::set(
            "LEAN_CTX_CONFIG_DIR",
            config_dir.to_string_lossy().as_ref(),
        );
        let _injection_guard =
            crate::test_env::EnvVarGuard::set("LEAN_CTX_RULES_INJECTION", "shared");

        assert!(
            suppress_initialize_instructions("cursor"),
            "explicit auto_inject_rules=false declines rule steering and must suppress MCP initialize instructions"
        );
    }

    #[test]
    fn initialize_instructions_preserve_default_auto_behavior() {
        let _guard = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("config");

        std::fs::create_dir_all(&config_dir).unwrap();

        let _config_guard = crate::test_env::EnvVarGuard::set(
            "LEAN_CTX_CONFIG_DIR",
            config_dir.to_string_lossy().as_ref(),
        );
        let _injection_guard =
            crate::test_env::EnvVarGuard::set("LEAN_CTX_RULES_INJECTION", "shared");

        assert!(
            !suppress_initialize_instructions("cursor"),
            "default auto_inject_rules=None must preserve initialize instructions"
        );
    }

    #[test]
    fn initialize_instructions_preserve_explicit_true_behavior() {
        let _guard = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("config");

        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            "[setup]\nauto_inject_rules = true\n",
        )
        .unwrap();

        let _config_guard = crate::test_env::EnvVarGuard::set(
            "LEAN_CTX_CONFIG_DIR",
            config_dir.to_string_lossy().as_ref(),
        );
        let _injection_guard =
            crate::test_env::EnvVarGuard::set("LEAN_CTX_RULES_INJECTION", "shared");

        assert!(
            !suppress_initialize_instructions("cursor"),
            "explicit auto_inject_rules=true must preserve initialize instructions"
        );
    }

    #[test]
    fn initialize_instructions_respect_rules_injection_off() {
        let _guard = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("config");

        std::fs::create_dir_all(&config_dir).unwrap();

        let _config_guard = crate::test_env::EnvVarGuard::set(
            "LEAN_CTX_CONFIG_DIR",
            config_dir.to_string_lossy().as_ref(),
        );
        let _injection_guard = crate::test_env::EnvVarGuard::set("LEAN_CTX_RULES_INJECTION", "off");

        assert!(
            suppress_initialize_instructions("cursor"),
            "rules_injection=off must continue suppressing initialize instructions"
        );
    }

    #[test]
    fn initialize_instructions_keep_client_compatibility_exceptions() {
        let _guard = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("config");

        std::fs::create_dir_all(&config_dir).unwrap();

        let _config_guard = crate::test_env::EnvVarGuard::set(
            "LEAN_CTX_CONFIG_DIR",
            config_dir.to_string_lossy().as_ref(),
        );
        let _injection_guard =
            crate::test_env::EnvVarGuard::set("LEAN_CTX_RULES_INJECTION", "shared");

        assert!(suppress_initialize_instructions("antigravity"));
        assert!(suppress_initialize_instructions("gemini-cli"));
    }
    #[test]
    fn startup_maintenance_respects_explicit_auto_inject_false() {
        let _guard = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let config_dir = tmp.path().join("config");

        std::fs::create_dir_all(home.join(".cursor")).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();

        std::fs::write(
            home.join(".cursor/mcp.json"),
            r#"{"mcpServers":{"lean-ctx":{"command":"lean-ctx","args":["mcp"]}}}"#,
        )
        .unwrap();

        std::fs::write(
            config_dir.join("config.toml"),
            "[setup]\nauto_inject_rules = false\n",
        )
        .unwrap();

        let _config_guard = crate::test_env::EnvVarGuard::set(
            "LEAN_CTX_CONFIG_DIR",
            config_dir.to_string_lossy().as_ref(),
        );
        let _injection_guard =
            crate::test_env::EnvVarGuard::set("LEAN_CTX_RULES_INJECTION", "shared");
        let _scope_guard = crate::test_env::EnvVarGuard::set("LEAN_CTX_RULES_SCOPE", "global");

        startup_rule_maintenance_at_home(&home);

        let rules = home.join(".cursor/rules/lean-ctx.mdc");

        assert!(
            !rules.exists(),
            "startup maintenance must not write Cursor rules when setup.auto_inject_rules=false"
        );
    }

    #[test]
    fn automatic_rule_maintenance_allows_default_startup_write() {
        let _guard = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let config_dir = tmp.path().join("config");

        std::fs::create_dir_all(home.join(".cursor")).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();

        std::fs::write(
            home.join(".cursor/mcp.json"),
            r#"{"mcpServers":{"lean-ctx":{"command":"lean-ctx","args":["mcp"]}}}"#,
        )
        .unwrap();

        let _config_guard = crate::test_env::EnvVarGuard::set(
            "LEAN_CTX_CONFIG_DIR",
            config_dir.to_string_lossy().as_ref(),
        );
        let _injection_guard =
            crate::test_env::EnvVarGuard::set("LEAN_CTX_RULES_INJECTION", "shared");
        let _scope_guard = crate::test_env::EnvVarGuard::set("LEAN_CTX_RULES_SCOPE", "global");

        startup_rule_maintenance_at_home(&home);

        assert!(
            home.join(".cursor/rules/lean-ctx.mdc").exists(),
            "default auto_inject_rules=None must preserve zero-config startup rule maintenance"
        );
    }

    #[test]
    fn automatic_rule_maintenance_allows_explicit_true_startup_write() {
        let _guard = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let config_dir = tmp.path().join("config");

        std::fs::create_dir_all(home.join(".cursor")).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();

        std::fs::write(
            home.join(".cursor/mcp.json"),
            r#"{"mcpServers":{"lean-ctx":{"command":"lean-ctx","args":["mcp"]}}}"#,
        )
        .unwrap();

        std::fs::write(
            config_dir.join("config.toml"),
            "[setup]\nauto_inject_rules = true\n",
        )
        .unwrap();

        let _config_guard = crate::test_env::EnvVarGuard::set(
            "LEAN_CTX_CONFIG_DIR",
            config_dir.to_string_lossy().as_ref(),
        );
        let _injection_guard =
            crate::test_env::EnvVarGuard::set("LEAN_CTX_RULES_INJECTION", "shared");
        let _scope_guard = crate::test_env::EnvVarGuard::set("LEAN_CTX_RULES_SCOPE", "global");

        startup_rule_maintenance_at_home(&home);

        assert!(
            home.join(".cursor/rules/lean-ctx.mdc").exists(),
            "explicit auto_inject_rules=true must preserve startup rule maintenance"
        );
    }

    #[test]
    fn host_task_scope_retains_wire_metadata_for_receipt_replay() {
        use rmcp::model::{CallToolRequest, Meta};
        let mut wire: CallToolRequest = serde_json::from_value(serde_json::json!({
            "method":"tools/call", "params":{"name":"ctx_read", "arguments":{},
                "_meta":{"requestId":"wire-retry", "progressToken":19}}
        }))
        .unwrap();
        assert!(wire.params.meta.is_none());
        let transported = wire.extensions.get::<Meta>().unwrap().clone();
        restore_tool_meta(&mut wire.params, &transported);
        assert_eq!(
            wire.params.meta.as_ref().unwrap().0["requestId"],
            "wire-retry"
        );
        assert_eq!(wire.params.meta.as_ref().unwrap().0["progressToken"], 19);
        assert!(
            wire.params
                .meta
                .as_ref()
                .unwrap()
                .get_progress_token()
                .is_some()
        );
        wire.params
            .meta
            .as_mut()
            .unwrap()
            .0
            .insert("requestId".into(), "local-conflict".into());
        wire.params
            .meta
            .as_mut()
            .unwrap()
            .0
            .insert("local-only".into(), true.into());
        restore_tool_meta(&mut wire.params, &transported);
        assert_eq!(
            wire.params.meta.as_ref().unwrap().0["requestId"],
            "wire-retry"
        );
        assert_eq!(wire.params.meta.as_ref().unwrap().0["local-only"], true);
        let before = wire.params.meta.clone();
        restore_tool_meta(&mut wire.params, &Meta::new());
        assert_eq!(wire.params.meta, before);
    }

    /// lean-ctx emits `notifications/tools/list_changed` whenever a tool call
    /// mutates the dynamic tool set. The capability MUST be advertised on every
    /// client surface (resources/prompts on or off) — otherwise a strict client
    /// such as Claude Code rejects the undeclared notification and drops the whole
    /// tool set ("connected, but tools not registered"). Regression guard for #688.
    #[test]
    fn server_capabilities_always_declare_tool_list_changed() {
        for (resources, prompts) in [(true, true), (true, false), (false, true), (false, false)] {
            let caps = server_capabilities(resources, prompts);
            let tools = caps.tools.expect("tools capability must be advertised");
            assert_eq!(
                tools.list_changed,
                Some(true),
                "listChanged must be Some(true) for (resources={resources}, prompts={prompts})"
            );
        }
    }

    /// The `list_tools` panic guard serves `lazy_tool_defs()`; it must contain the
    /// essentials so an internal panic never leaves the agent tool-less (which is
    /// indistinguishable from "MCP totally failed"). Regression guard for #688.
    #[test]
    fn lazy_core_fallback_is_never_empty() {
        let _guard = crate::core::data_dir::isolated_data_dir();
        let defs = crate::tool_defs::lazy_tool_defs();
        assert!(!defs.is_empty(), "lazy-core fallback must not be empty");
        for essential in ["ctx_read", "ctx_shell", "ctx_call"] {
            assert!(
                defs.iter().any(|t| t.name.as_ref() == essential),
                "lazy-core fallback must include {essential}"
            );
        }
    }
}
