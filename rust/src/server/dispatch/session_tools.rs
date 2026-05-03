use rmcp::ErrorData;
use serde_json::Value;

use crate::server::helpers::{get_bool, get_str, get_str_array};
use crate::tools::LeanCtxServer;

impl LeanCtxServer {
    pub(crate) async fn dispatch_session_tools(
        &self,
        name: &str,
        args: Option<&serde_json::Map<String, Value>>,
        _minimal: bool,
    ) -> Result<String, ErrorData> {
        Ok(match name {
            "ctx_session" => {
                let action = get_str(args, "action")
                    .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
                let value = get_str(args, "value");
                let sid = get_str(args, "session_id");
                let tool_calls = self.tool_calls.read().await.clone();
                let call_durations: Vec<(String, u64)> = tool_calls
                    .iter()
                    .map(|c| (c.tool.clone(), c.duration_ms))
                    .collect();
                let mut session = self.session.write().await;
                let result = crate::tools::ctx_session::handle(
                    &mut session,
                    &call_durations,
                    &action,
                    value.as_deref(),
                    sid.as_deref(),
                );
                drop(session);
                self.record_call("ctx_session", 0, 0, Some(action)).await;
                result
            }
            "ctx_knowledge" => {
                let action = get_str(args, "action")
                    .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
                let category = get_str(args, "category");
                let key = get_str(args, "key");
                let value = get_str(args, "value");
                let query = get_str(args, "query");
                let mode = get_str(args, "mode");
                let pattern_type = get_str(args, "pattern_type");
                let examples = get_str_array(args, "examples");
                let confidence: Option<f32> = args
                    .as_ref()
                    .and_then(|a| a.get("confidence"))
                    .and_then(serde_json::Value::as_f64)
                    .map(|v| v as f32);

                let session = self.session.read().await;
                let session_id = session.id.clone();
                let project_root = session.project_root.clone().unwrap_or_else(|| {
                    std::env::current_dir().map_or_else(
                        |_| "unknown".to_string(),
                        |p| p.to_string_lossy().to_string(),
                    )
                });
                drop(session);

                if action == "gotcha" {
                    let trigger = get_str(args, "trigger").unwrap_or_default();
                    let resolution = get_str(args, "resolution").unwrap_or_default();
                    let severity = get_str(args, "severity").unwrap_or_default();
                    let cat = category.as_deref().unwrap_or("convention");

                    if trigger.is_empty() || resolution.is_empty() {
                        self.record_call("ctx_knowledge", 0, 0, Some(action)).await;
                        return Ok(
                            "ERROR: trigger and resolution are required for gotcha action"
                                .to_string(),
                        );
                    }

                    let mut store = crate::core::gotcha_tracker::GotchaStore::load(&project_root);
                    let msg = match store.report_gotcha(
                        &trigger,
                        &resolution,
                        cat,
                        &severity,
                        &session_id,
                    ) {
                        Some(gotcha) => {
                            let conf = (gotcha.confidence * 100.0) as u32;
                            let label = gotcha.category.short_label();
                            format!("Gotcha recorded: [{label}] {trigger} (confidence: {conf}%)")
                        }
                        None => format!(
                            "Gotcha noted: {trigger} (evicted by higher-confidence entries)"
                        ),
                    };
                    let _ = store.save(&project_root);
                    self.record_call("ctx_knowledge", 0, 0, Some(action)).await;
                    return Ok(msg);
                }

                let result = crate::tools::ctx_knowledge::handle(
                    &project_root,
                    &action,
                    category.as_deref(),
                    key.as_deref(),
                    value.as_deref(),
                    query.as_deref(),
                    &session_id,
                    pattern_type.as_deref(),
                    examples,
                    confidence,
                    mode.as_deref(),
                );
                self.record_call("ctx_knowledge", 0, 0, Some(action)).await;
                result
            }
            "ctx_agent" => {
                let action = get_str(args, "action")
                    .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
                let agent_type = get_str(args, "agent_type");
                let role = get_str(args, "role");
                let message = get_str(args, "message");
                let category = get_str(args, "category");
                let to_agent = get_str(args, "to_agent");
                let status = get_str(args, "status");

                let session = self.session.read().await;
                let project_root = session.project_root.clone().unwrap_or_else(|| {
                    std::env::current_dir().map_or_else(
                        |_| "unknown".to_string(),
                        |p| p.to_string_lossy().to_string(),
                    )
                });
                drop(session);

                let current_agent_id = self.agent_id.read().await.clone();
                let result = crate::tools::ctx_agent::handle(
                    &action,
                    agent_type.as_deref(),
                    role.as_deref(),
                    &project_root,
                    current_agent_id.as_deref(),
                    message.as_deref(),
                    category.as_deref(),
                    to_agent.as_deref(),
                    status.as_deref(),
                );

                if action == "register" {
                    if let Some(id) = result.split(':').nth(1) {
                        let id = id.split_whitespace().next().unwrap_or("").to_string();
                        if !id.is_empty() {
                            *self.agent_id.write().await = Some(id);
                        }
                    }

                    let agent_role = crate::core::agents::AgentRole::from_str_loose(
                        role.as_deref().unwrap_or("coder"),
                    );
                    let depth = crate::core::agents::ContextDepthConfig::for_role(agent_role);
                    let depth_hint = format!(
                        "\n[context] role={:?} preferred_mode={} max_full={} max_sig={} budget_ratio={:.0}%",
                        agent_role,
                        depth.preferred_mode,
                        depth.max_files_full,
                        depth.max_files_signatures,
                        depth.context_budget_ratio * 100.0,
                    );
                    self.record_call("ctx_agent", 0, 0, Some(action)).await;
                    return Ok(format!("{result}{depth_hint}"));
                }

                self.record_call("ctx_agent", 0, 0, Some(action)).await;
                result
            }
            "ctx_share" => {
                let action = get_str(args, "action")
                    .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
                let to_agent = get_str(args, "to_agent");
                let paths = get_str(args, "paths");
                let message = get_str(args, "message");

                let from_agent = self.agent_id.read().await.clone();
                let cache = self.cache.read().await;
                let result = crate::tools::ctx_share::handle(
                    &action,
                    from_agent.as_deref(),
                    to_agent.as_deref(),
                    paths.as_deref(),
                    message.as_deref(),
                    &cache,
                );
                drop(cache);

                self.record_call("ctx_share", 0, 0, Some(action)).await;
                result
            }
            "ctx_task" => {
                let action = get_str(args, "action").unwrap_or_else(|| "list".to_string());
                let current_agent_id = { self.agent_id.read().await.clone() };
                let task_id = get_str(args, "task_id");
                let to_agent = get_str(args, "to_agent");
                let description = get_str(args, "description");
                let state = get_str(args, "state");
                let message = get_str(args, "message");
                let result = crate::tools::ctx_task::handle(
                    &action,
                    current_agent_id.as_deref(),
                    task_id.as_deref(),
                    to_agent.as_deref(),
                    description.as_deref(),
                    state.as_deref(),
                    message.as_deref(),
                );
                self.record_call("ctx_task", 0, 0, Some(action)).await;
                result
            }
            "ctx_handoff" => {
                let action = get_str(args, "action").unwrap_or_else(|| "list".to_string());
                match action.as_str() {
                    "list" => {
                        let items = crate::core::handoff_ledger::list_ledgers();
                        let result = crate::tools::ctx_handoff::format_list(&items);
                        self.record_call("ctx_handoff", 0, 0, Some(action)).await;
                        result
                    }
                    "clear" => {
                        let removed =
                            crate::core::handoff_ledger::clear_ledgers().unwrap_or_default();
                        let result = crate::tools::ctx_handoff::format_clear(removed);
                        self.record_call("ctx_handoff", 0, 0, Some(action)).await;
                        result
                    }
                    "show" => {
                        let path = get_str(args, "path").ok_or_else(|| {
                            ErrorData::invalid_params("path is required for action=show", None)
                        })?;
                        let path = self
                            .resolve_path(&path)
                            .await
                            .map_err(|e| ErrorData::invalid_params(e, None))?;
                        let ledger =
                            crate::core::handoff_ledger::load_ledger(std::path::Path::new(&path))
                                .map_err(|e| {
                                ErrorData::internal_error(format!("load ledger: {e}"), None)
                            })?;
                        let result = crate::tools::ctx_handoff::format_show(
                            std::path::Path::new(&path),
                            &ledger,
                        );
                        self.record_call("ctx_handoff", 0, 0, Some(action)).await;
                        result
                    }
                    "create" => {
                        let curated_paths = get_str_array(args, "paths").unwrap_or_default();
                        let mut curated_refs: Vec<(String, String)> = Vec::new();
                        if !curated_paths.is_empty() {
                            let mut cache = self.cache.write().await;
                            for p in curated_paths.into_iter().take(20) {
                                let abs = self
                                    .resolve_path(&p)
                                    .await
                                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                                let text = crate::tools::ctx_read::handle_with_task(
                                    &mut cache,
                                    &abs,
                                    "signatures",
                                    crate::tools::CrpMode::effective(),
                                    None,
                                );
                                curated_refs.push((abs, text));
                            }
                        }

                        let session = { self.session.read().await.clone() };
                        let active_intent = session.active_structured_intent.clone();
                        let tool_calls = { self.tool_calls.read().await.clone() };
                        let workflow = { self.workflow.read().await.clone() };
                        let agent_id = { self.agent_id.read().await.clone() };
                        let client_name = { self.client_name.read().await.clone() };
                        let project_root = session.project_root.clone();

                        let (ledger, path) = crate::core::handoff_ledger::create_ledger(
                            crate::core::handoff_ledger::CreateLedgerInput {
                                agent_id,
                                client_name: Some(client_name),
                                project_root,
                                session,
                                tool_calls,
                                workflow,
                                curated_refs,
                            },
                        )
                        .map_err(|e| {
                            ErrorData::internal_error(format!("create ledger: {e}"), None)
                        })?;

                        let ctx_ledger = self.ledger.read().await;
                        let package = crate::core::handoff_ledger::HandoffPackage::build(
                            ledger.clone(),
                            active_intent.as_ref(),
                            if ctx_ledger.entries.is_empty() {
                                None
                            } else {
                                Some(&*ctx_ledger)
                            },
                        );
                        drop(ctx_ledger);

                        let mut output = crate::tools::ctx_handoff::format_created(&path, &ledger);
                        let compact = package.format_compact();
                        if !compact.is_empty() {
                            output.push_str("\n\n");
                            output.push_str(&compact);
                        }

                        self.record_call("ctx_handoff", 0, 0, Some(action)).await;
                        output
                    }
                    "pull" => {
                        let path = get_str(args, "path").ok_or_else(|| {
                            ErrorData::invalid_params("path is required for action=pull", None)
                        })?;
                        let path = self
                            .resolve_path(&path)
                            .await
                            .map_err(|e| ErrorData::invalid_params(e, None))?;
                        let ledger =
                            crate::core::handoff_ledger::load_ledger(std::path::Path::new(&path))
                                .map_err(|e| {
                                ErrorData::internal_error(format!("load ledger: {e}"), None)
                            })?;

                        let apply_workflow = get_bool(args, "apply_workflow").unwrap_or(true);
                        let apply_session = get_bool(args, "apply_session").unwrap_or(true);
                        let apply_knowledge = get_bool(args, "apply_knowledge").unwrap_or(true);

                        if apply_workflow {
                            let mut wf = self.workflow.write().await;
                            wf.clone_from(&ledger.workflow);
                        }

                        if apply_session {
                            let mut session = self.session.write().await;
                            if let Some(t) = ledger.session.task.as_deref() {
                                session.set_task(t, None);
                            }
                            for d in &ledger.session.decisions {
                                session.add_decision(d, None);
                            }
                            for f in &ledger.session.findings {
                                session.add_finding(None, None, f);
                            }
                            session.next_steps.clone_from(&ledger.session.next_steps);
                            let _ = session.save();
                        }

                        let mut knowledge_imported = 0u32;
                        let mut contradictions = 0u32;
                        if apply_knowledge {
                            let root = if let Some(r) = ledger.project_root.as_deref() {
                                r.to_string()
                            } else {
                                let session = self.session.read().await;
                                session
                                    .project_root
                                    .clone()
                                    .unwrap_or_else(|| ".".to_string())
                            };
                            let session_id = {
                                let s = self.session.read().await;
                                s.id.clone()
                            };
                            let policy = match crate::core::config::Config::load()
                                .memory_policy_effective()
                            {
                                Ok(p) => p,
                                Err(e) => {
                                    let path = crate::core::config::Config::path().map_or_else(
                                        || "~/.lean-ctx/config.toml".to_string(),
                                        |p| p.display().to_string(),
                                    );
                                    return Ok(format!(
                                        "Error: invalid memory policy: {e}\nFix: edit {path}"
                                    ));
                                }
                            };
                            let mut knowledge =
                                crate::core::knowledge::ProjectKnowledge::load_or_create(&root);
                            for fact in &ledger.knowledge.facts {
                                let c = knowledge.remember(
                                    &fact.category,
                                    &fact.key,
                                    &fact.value,
                                    &session_id,
                                    fact.confidence,
                                    &policy,
                                );
                                if c.is_some() {
                                    contradictions += 1;
                                }
                                knowledge_imported += 1;
                            }
                            let _ = knowledge.run_memory_lifecycle(&policy);
                            let _ = knowledge.save();
                        }

                        let lines = [
                            "ctx_handoff pull".to_string(),
                            format!(" path: {path}"),
                            format!(" md5: {}", ledger.content_md5),
                            format!(" applied_workflow: {apply_workflow}"),
                            format!(" applied_session: {apply_session}"),
                            format!(" imported_knowledge: {knowledge_imported}"),
                            format!(" contradictions: {contradictions}"),
                        ];
                        let result = lines.join("\n");
                        self.record_call("ctx_handoff", 0, 0, Some(action)).await;
                        result
                    }
                    _ => {
                        let result =
                            "Unknown action. Use: create, show, list, pull, clear".to_string();
                        self.record_call("ctx_handoff", 0, 0, Some(action)).await;
                        result
                    }
                }
            }
            "ctx_workflow" => {
                let action = get_str(args, "action").unwrap_or_else(|| "status".to_string());
                let result = {
                    let mut session = self.session.write().await;
                    crate::tools::ctx_workflow::handle_with_session(args, &mut session)
                };
                *self.workflow.write().await = crate::core::workflow::load_active().ok().flatten();
                self.record_call("ctx_workflow", 0, 0, Some(action)).await;
                result
            }
            _ => unreachable!("dispatch_session_tools called with unknown tool: {name}"),
        })
    }
}
