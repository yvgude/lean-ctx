use crate::core::a2a::message::{MessagePriority, PrivacyLevel};
use crate::core::a2a::task::TaskStore;
use crate::core::agents::{AgentDiary, AgentRegistry, AgentStatus, DiaryEntryType};
use crate::core::evidence_ledger::EvidenceLedgerV1;

#[allow(clippy::too_many_arguments)]
pub fn handle(
    action: &str,
    agent_type: Option<&str>,
    role: Option<&str>,
    project_root: &str,
    current_agent_id: Option<&str>,
    message: Option<&str>,
    category: Option<&str>,
    to_agent: Option<&str>,
    status: Option<&str>,
    privacy: Option<&str>,
    priority: Option<&str>,
    _ttl_hours: Option<u64>,
    format: Option<&str>,
    write: bool,
    filename: Option<&str>,
) -> String {
    match action {
        "register" => {
            let atype = agent_type.unwrap_or("unknown");
            let mut registry = AgentRegistry::load_or_create();
            registry.cleanup_stale(24);
            let agent_id = registry.register(atype, role, project_root);
            match registry.save() {
                Ok(()) => format!(
                    "Agent registered: {agent_id} (type: {atype}, role: {})",
                    role.unwrap_or("none")
                ),
                Err(e) => format!("Registered as {agent_id} but save failed: {e}"),
            }
        }

        "list" => {
            let mut registry = AgentRegistry::load_or_create();
            registry.cleanup_stale(24);
            if let Err(e) = registry.save() {
                tracing::warn!("lean-ctx: failed to persist agent registry: {e}");
            }

            let agents = registry.list_active(Some(project_root));
            if agents.is_empty() {
                return "No active agents for this project.".to_string();
            }

            let mut out = format!("Active agents ({}):\n", agents.len());
            for a in agents {
                let role_str = a.role.as_deref().unwrap_or("-");
                let status_msg = a
                    .status_message
                    .as_deref()
                    .map(|m| format!(" — {m}"))
                    .unwrap_or_default();
                let age = (chrono::Utc::now() - a.last_active).num_minutes();
                out.push_str(&format!(
                    "  {} [{}] role={} status={}{} (last active: {}m ago, pid: {})\n",
                    a.agent_id, a.agent_type, role_str, a.status, status_msg, age, a.pid
                ));
            }
            out
        }

        "post" => {
            let Some(msg) = message else {
                return "Error: message is required for post".to_string();
            };
            let cat = category.unwrap_or("status");
            let from = current_agent_id.unwrap_or("anonymous");
            let msg_privacy = privacy.map_or(PrivacyLevel::Team, PrivacyLevel::parse_str);
            let msg_priority = priority.map_or(MessagePriority::Normal, MessagePriority::parse_str);
            if msg_privacy == PrivacyLevel::Private && to_agent.is_none() {
                return "Error: private messages require to_agent".to_string();
            }
            let mut registry = AgentRegistry::load_or_create();
            let msg_id = registry.post_message_full(
                from,
                to_agent,
                cat,
                msg,
                msg_privacy,
                msg_priority,
                _ttl_hours,
            );
            match registry.save() {
                Ok(()) => {
                    let target = to_agent.unwrap_or("all agents (broadcast)");
                    format!("Posted [{cat}] to {target}: {msg} (id: {msg_id})")
                }
                Err(e) => format!("Posted but save failed: {e}"),
            }
        }

        "read" => {
            let Some(agent_id) = current_agent_id else {
                return "Error: agent must be registered first (use action=register)".to_string();
            };
            let mut registry = AgentRegistry::load_or_create();
            let messages = registry.read_unread(agent_id);

            if messages.is_empty() {
                if let Err(e) = registry.save() {
                    tracing::warn!("lean-ctx: failed to persist agent registry: {e}");
                }
                return "No new messages.".to_string();
            }

            let mut out = format!("New messages ({}):\n", messages.len());
            for m in &messages {
                let age = (chrono::Utc::now() - m.timestamp).num_minutes();
                out.push_str(&format!(
                    "  [{}] from {} ({}m ago): {}\n",
                    m.category, m.from_agent, age, m.message
                ));
            }
            if let Err(e) = registry.save() {
                tracing::warn!(
                    "lean-ctx: failed to persist agent registry (messages may reappear): {e}"
                );
            }
            out
        }

        "status" => {
            let Some(agent_id) = current_agent_id else {
                return "Error: agent must be registered first".to_string();
            };
            let new_status = match status {
                Some("active") => AgentStatus::Active,
                Some("idle") => AgentStatus::Idle,
                Some("finished") => AgentStatus::Finished,
                Some(other) => {
                    return format!("Unknown status: {other}. Use: active, idle, finished");
                }
                None => return "Error: status value is required".to_string(),
            };
            let status_msg = message;

            let mut registry = AgentRegistry::load_or_create();
            registry.set_status(agent_id, new_status.clone(), status_msg);
            match registry.save() {
                Ok(()) => format!(
                    "Status updated: {} → {}{}",
                    agent_id,
                    new_status,
                    status_msg.map(|m| format!(" ({m})")).unwrap_or_default()
                ),
                Err(e) => format!("Status set but save failed: {e}"),
            }
        }

        "info" => {
            let registry = AgentRegistry::load_or_create();
            let total = registry.agents.len();
            let active = registry
                .agents
                .iter()
                .filter(|a| a.status == AgentStatus::Active)
                .count();
            let messages = registry.scratchpad.len();
            format!(
                "Agent Registry: {total} total, {active} active, {messages} scratchpad entries\nLast updated: {}",
                registry.updated_at.format("%Y-%m-%d %H:%M UTC")
            )
        }

        "handoff" => {
            let Some(from) = current_agent_id else {
                return "Error: agent must be registered first".to_string();
            };
            let Some(target) = to_agent else {
                return "Error: to_agent is required for handoff".to_string();
            };
            let summary = message.unwrap_or("(no summary provided)");

            let mut registry = AgentRegistry::load_or_create();

            registry.post_message(
                from,
                Some(target),
                "handoff",
                &format!("HANDOFF from {from}: {summary}"),
            );

            registry.set_status(from, AgentStatus::Finished, Some("handed off"));
            let _ = registry.save();

            // Stigmergy (#540): mark the handed-off work as Done in the field
            // so other agents see it arithmetically, without reading messages.
            crate::core::scent_field::deposit(
                from,
                crate::core::scent_field::ScentKind::Done,
                summary,
                1.0,
            );

            format!("Handoff complete: {from} → {target}\nSummary: {summary}")
        }

        // Stigmergic claim (#540): atomically claim a target (file, task,
        // deploy unit) in the shared scent field. Fails fast when another
        // agent's claim is still active — prevents duplicate work for ~0 tokens.
        "claim" => {
            let Some(target) = message else {
                return "Error: message (the claim target, e.g. a file path or task label) is required for claim".to_string();
            };
            let agent = current_agent_id.map_or_else(
                || crate::core::scent_field::scent_agent_id().to_string(),
                str::to_string,
            );
            let normalized = crate::core::pathutil::normalize_tool_path(target);
            match crate::core::scent_field::claim(&agent, &normalized) {
                Ok(()) => {
                    format!("Claimed: {normalized} (by {agent}, decays in ~10m unless re-claimed)")
                }
                Err(e) => format!("Claim REJECTED: {normalized} — {e}"),
            }
        }

        // Release a stigmergic claim early (done or abandoned).
        "release" => {
            let Some(target) = message else {
                return "Error: message (the claim target) is required for release".to_string();
            };
            let agent = current_agent_id.map_or_else(
                || crate::core::scent_field::scent_agent_id().to_string(),
                str::to_string,
            );
            let normalized = crate::core::pathutil::normalize_tool_path(target);
            crate::core::scent_field::release(&agent, &normalized);
            format!("Released: {normalized}")
        }

        // Sub-agent context contract (GL#450): deterministic briefing pack.
        // `message` is the task; `priority` doubles as the token budget when
        // numeric (default 2000). Same knowledge + task ⇒ byte-identical pack.
        "brief" => {
            let Some(task) = message else {
                return "Error: message (the sub-agent task) is required for brief".to_string();
            };
            let budget = priority
                .and_then(|p| p.parse::<usize>().ok())
                .unwrap_or(2000);
            let Some(knowledge) = crate::core::knowledge::ProjectKnowledge::load(project_root)
            else {
                return "No knowledge stored for this project yet — a briefing pack needs facts. Use ctx_knowledge(action=\"remember\") first.".to_string();
            };
            let pack =
                crate::core::subagent_contract::build_briefing_pack(&knowledge, task, budget);
            match crate::core::subagent_contract::serialize_pack(&pack) {
                Ok(json) => json,
                Err(e) => format!("Error: {e}"),
            }
        }

        // Return synthesis (GL#450): distill a sub-agent's report into
        // recallable parent knowledge. Expects contract-formatted lines
        // ('category/key: value'); rejects are listed, never silently dropped.
        "return" => {
            let Some(report) = message else {
                return "Error: message (the sub-agent report) is required for return".to_string();
            };
            let (facts, rejected) = crate::core::subagent_contract::parse_return_lines(report);
            if facts.is_empty() {
                return format!(
                    "No contract-formatted lines found ({} rejected). Expected 'category/key: value' per line.",
                    rejected.len()
                );
            }

            let session_label = current_agent_id.unwrap_or("subagent");
            let policy = match crate::tools::knowledge_shared::load_policy_or_error() {
                Ok(p) => p,
                Err(e) => return e,
            };
            let count = facts.len();
            let res = crate::core::knowledge::ProjectKnowledge::mutate_locked(
                project_root,
                |knowledge| {
                    for f in &facts {
                        knowledge.remember(
                            &f.category,
                            &f.key,
                            &f.value,
                            session_label,
                            0.8,
                            &policy,
                        );
                    }
                },
            );
            match res {
                Ok(_) => {
                    let mut out = format!(
                        "Return synthesis: {count} fact(s) distilled into parent knowledge"
                    );
                    if !rejected.is_empty() {
                        out.push_str(&format!(
                            "\n{} line(s) rejected (not 'category/key: value'):",
                            rejected.len()
                        ));
                        for r in rejected.iter().take(5) {
                            out.push_str(&format!("\n  ✗ {r}"));
                        }
                    }
                    out
                }
                Err(e) => format!("Error: knowledge store update failed: {e}"),
            }
        }

        "sync" => {
            let registry = AgentRegistry::load_or_create();
            let pending_count = current_agent_id.map_or(0, |id| {
                registry
                    .scratchpad
                    .iter()
                    .filter(|e| {
                        !e.read_by.contains(&id.to_string())
                            && e.from_agent != id
                            && (e.to_agent.is_none() || e.to_agent.as_deref() == Some(id))
                    })
                    .count()
            });
            let agents: Vec<&crate::core::agents::AgentEntry> = registry
                .agents
                .iter()
                .filter(|a| a.status != AgentStatus::Finished && a.project_root == project_root)
                .collect();

            if agents.is_empty() {
                return "No active agents to sync with.".to_string();
            }

            let shared_dir = crate::core::data_dir::lean_ctx_data_dir()
                .unwrap_or_default()
                .join("agents")
                .join("shared");

            let shared_count = if shared_dir.exists() {
                std::fs::read_dir(&shared_dir).map_or(0, std::iter::Iterator::count)
            } else {
                0
            };

            let mut out = "Multi-Agent Sync Status:\n".to_string();
            out.push_str(&format!("  Active agents: {}\n", agents.len()));
            for a in &agents {
                let role = a.role.as_deref().unwrap_or("-");
                let age = (chrono::Utc::now() - a.last_active).num_minutes();
                out.push_str(&format!(
                    "    {} [{}] role={} ({}m ago)\n",
                    a.agent_id, a.agent_type, role, age
                ));
            }
            out.push_str(&format!("  Pending messages: {pending_count}\n"));
            out.push_str(&format!("  Shared contexts: {shared_count}\n"));

            // Stigmergy (#540): arithmetic field view — claims, stuck markers,
            // hot files across all agents, no scratchpad reads needed.
            let scents = crate::core::scent_field::sync_block();
            if !scents.is_empty() {
                out.push_str(&scents);
            }
            out
        }

        "export" => {
            let Some(agent_id) = current_agent_id else {
                return "Error: agent must be registered first (use action=register)".to_string();
            };

            fn privacy_label(p: &PrivacyLevel) -> &'static str {
                match p {
                    PrivacyLevel::Public => "public",
                    PrivacyLevel::Team => "team",
                    PrivacyLevel::Private => "private",
                }
            }

            fn priority_label(p: &MessagePriority) -> &'static str {
                match p {
                    MessagePriority::Low => "low",
                    MessagePriority::Normal => "normal",
                    MessagePriority::High => "high",
                    MessagePriority::Critical => "critical",
                }
            }

            fn maybe_redact(s: &str, should_redact: bool) -> String {
                if should_redact {
                    crate::core::redaction::redact_text(s)
                } else {
                    s.to_string()
                }
            }

            #[derive(serde::Serialize)]
            struct ExportAgentV1 {
                agent_id: String,
                agent_type: String,
                role: Option<String>,
                status: String,
                status_message: Option<String>,
                started_at: String,
                last_active: String,
                pid: u32,
            }

            #[derive(serde::Serialize)]
            struct ExportMessageV1 {
                id: String,
                from_agent: String,
                to_agent: Option<String>,
                category: String,
                privacy: String,
                priority: String,
                message: String,
                metadata: std::collections::BTreeMap<String, String>,
                timestamp: String,
                expires_at: Option<String>,
                read_by_count: usize,
            }

            #[derive(serde::Serialize)]
            struct ExportTaskV1 {
                id: String,
                from_agent: String,
                to_agent: String,
                state: String,
                description: String,
                created_at: String,
                updated_at: String,
                messages: usize,
                artifacts: usize,
                transitions: usize,
            }

            #[derive(serde::Serialize)]
            struct ExportDiaryEntryV1 {
                entry_type: String,
                content: String,
                context: Option<String>,
                timestamp: String,
            }

            #[derive(serde::Serialize)]
            struct ExportDiaryV1 {
                agent_id: String,
                agent_type: String,
                project_root: String,
                updated_at: String,
                entries: Vec<ExportDiaryEntryV1>,
            }

            #[derive(serde::Serialize)]
            struct A2ASnapshotV1 {
                schema_version: u32,
                created_at: String,
                project_root: String,
                agent_id: String,
                agents: Vec<ExportAgentV1>,
                messages: Vec<ExportMessageV1>,
                tasks: Vec<ExportTaskV1>,
                diary: Option<ExportDiaryV1>,
            }

            let privacy_mode = privacy.unwrap_or("redacted");
            let allow_full = privacy_mode == "full"
                && !crate::core::redaction::redaction_enabled_for_active_role();
            let should_redact = !allow_full;

            let now = chrono::Utc::now();
            let mut registry = AgentRegistry::load_or_create();
            registry.cleanup_stale(24);

            let mut agents: Vec<ExportAgentV1> = registry
                .list_active(Some(project_root))
                .into_iter()
                .map(|a| ExportAgentV1 {
                    agent_id: a.agent_id.clone(),
                    agent_type: a.agent_type.clone(),
                    role: a.role.clone(),
                    status: a.status.to_string(),
                    status_message: a.status_message.clone(),
                    started_at: a.started_at.to_rfc3339(),
                    last_active: a.last_active.to_rfc3339(),
                    pid: a.pid,
                })
                .collect();
            agents.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));

            let mut messages: Vec<ExportMessageV1> = registry
                .scratchpad
                .iter()
                .filter(|e| e.to_agent.is_none() || e.to_agent.as_deref() == Some(agent_id))
                .take(200)
                .map(|m| ExportMessageV1 {
                    id: m.id.clone(),
                    from_agent: m.from_agent.clone(),
                    to_agent: m.to_agent.clone(),
                    category: m.category.clone(),
                    privacy: privacy_label(&m.privacy).to_string(),
                    priority: priority_label(&m.priority).to_string(),
                    message: maybe_redact(&m.message, should_redact),
                    metadata: m
                        .metadata
                        .iter()
                        .map(|(k, v)| (k.clone(), maybe_redact(v, should_redact)))
                        .collect(),
                    timestamp: m.timestamp.to_rfc3339(),
                    expires_at: m.expires_at.map(|t| t.to_rfc3339()),
                    read_by_count: m.read_by.len(),
                })
                .collect();
            messages.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then_with(|| a.id.cmp(&b.id)));

            let mut task_store = TaskStore::load();
            task_store.cleanup_old(72);
            let mut tasks: Vec<ExportTaskV1> = task_store
                .tasks_for_agent(agent_id)
                .into_iter()
                .take(200)
                .map(|t| ExportTaskV1 {
                    id: t.id.clone(),
                    from_agent: t.from_agent.clone(),
                    to_agent: t.to_agent.clone(),
                    state: t.state.to_string(),
                    description: maybe_redact(&t.description, should_redact),
                    created_at: t.created_at.to_rfc3339(),
                    updated_at: t.updated_at.to_rfc3339(),
                    messages: t.messages.len(),
                    artifacts: t.artifacts.len(),
                    transitions: t.history.len(),
                })
                .collect();
            tasks.sort_by(|a, b| {
                b.updated_at
                    .cmp(&a.updated_at)
                    .then_with(|| a.id.cmp(&b.id))
            });

            let diary = AgentDiary::load(agent_id).map(|d| ExportDiaryV1 {
                agent_id: d.agent_id,
                agent_type: d.agent_type,
                project_root: d.project_root,
                updated_at: d.updated_at.to_rfc3339(),
                entries: d
                    .entries
                    .iter()
                    .rev()
                    .take(25)
                    .rev()
                    .map(|e| ExportDiaryEntryV1 {
                        entry_type: e.entry_type.to_string(),
                        content: maybe_redact(&e.content, should_redact),
                        context: e.context.as_deref().map(|c| maybe_redact(c, should_redact)),
                        timestamp: e.timestamp.to_rfc3339(),
                    })
                    .collect(),
            });

            let payload = A2ASnapshotV1 {
                schema_version: crate::core::contracts::A2A_SNAPSHOT_V1_SCHEMA_VERSION,
                created_at: now.to_rfc3339(),
                project_root: project_root.to_string(),
                agent_id: agent_id.to_string(),
                agents,
                messages,
                tasks,
                diary,
            };

            let json = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string());

            if write {
                let proofs_dir = std::path::Path::new(project_root)
                    .join(".lean-ctx")
                    .join("proofs");
                if let Err(e) = std::fs::create_dir_all(&proofs_dir) {
                    return format!("Error: create proofs dir: {e}");
                }

                let name = if let Some(f) = filename {
                    let p = std::path::Path::new(f);
                    if p.components().count() != 1 {
                        return "Error: filename must be a plain file name (no directories)"
                            .to_string();
                    }
                    f.to_string()
                } else {
                    format!("a2a-snapshot-v1_{}.json", now.format("%Y%m%d_%H%M%S"))
                };

                let out_path = proofs_dir.join(name);
                if let Err(e) = std::fs::write(&out_path, &json) {
                    return format!("Error: write snapshot: {e}");
                }

                let mut ledger = EvidenceLedgerV1::load();
                if let Err(e) = ledger.record_artifact_file(
                    "proof:a2a-snapshot-v1",
                    &out_path,
                    chrono::Utc::now(),
                ) {
                    return format!("Snapshot written but evidence ledger record failed: {e}");
                }
                if let Err(e) = ledger.save() {
                    return format!("Snapshot written but evidence ledger save failed: {e}");
                }

                return format!(
                    "A2A snapshot exported: {}\n  agents: {}\n  messages: {}\n  tasks: {}",
                    out_path.display(),
                    payload.agents.len(),
                    payload.messages.len(),
                    payload.tasks.len()
                );
            }

            match format.unwrap_or("json") {
                "text" => format!(
                    "A2A snapshot (v1)\n  agents: {}\n  messages: {}\n  tasks: {}",
                    payload.agents.len(),
                    payload.messages.len(),
                    payload.tasks.len()
                ),
                _ => json,
            }
        }

        "diary" => {
            let Some(agent_id) = current_agent_id else {
                return "Error: agent must be registered first".to_string();
            };
            let Some(content) = message else {
                return "Error: message is required for diary entry".to_string();
            };
            let entry_type = match category.unwrap_or("progress") {
                "discovery" | "found" => DiaryEntryType::Discovery,
                "decision" | "decided" => DiaryEntryType::Decision,
                "blocker" | "blocked" => DiaryEntryType::Blocker,
                "progress" | "done" => DiaryEntryType::Progress,
                "insight" => DiaryEntryType::Insight,
                other => {
                    return format!(
                        "Unknown diary type: {other}. Use: discovery, decision, blocker, progress, insight"
                    );
                }
            };
            let atype = agent_type.unwrap_or("unknown");
            let mut diary = AgentDiary::load_or_create(agent_id, atype, project_root);
            let context_str = to_agent;
            diary.add_entry(entry_type.clone(), content, context_str);
            match diary.save() {
                Ok(()) => format!("Diary entry [{entry_type}] added: {content}"),
                Err(e) => format!("Diary entry added but save failed: {e}"),
            }
        }

        "recall_diary" | "diary_recall" => {
            let Some(agent_id) = current_agent_id else {
                let diaries = AgentDiary::list_all();
                if diaries.is_empty() {
                    return "No agent diaries found.".to_string();
                }
                let mut out = format!("Agent Diaries ({}):\n", diaries.len());
                for (id, count, updated) in &diaries {
                    let age = (chrono::Utc::now() - *updated).num_minutes();
                    out.push_str(&format!("  {id}: {count} entries ({age}m ago)\n"));
                }
                return out;
            };
            match AgentDiary::load(agent_id) {
                Some(diary) => diary.format_summary(),
                None => format!("No diary found for agent '{agent_id}'."),
            }
        }

        "diaries" => {
            let diaries = AgentDiary::list_all();
            if diaries.is_empty() {
                return "No agent diaries found.".to_string();
            }
            let mut out = format!("Agent Diaries ({}):\n", diaries.len());
            for (id, count, updated) in &diaries {
                let age = (chrono::Utc::now() - *updated).num_minutes();
                out.push_str(&format!("  {id}: {count} entries ({age}m ago)\n"));
            }
            out
        }

        "share_knowledge" => {
            let cat = category.unwrap_or("general");
            let Some(msg_text) = message else {
                return "Error: message required (format: key1=value1;key2=value2)".to_string();
            };
            let facts: Vec<(String, String)> = msg_text
                .split(';')
                .filter_map(|kv| {
                    let (k, v) = kv.split_once('=')?;
                    Some((k.trim().to_string(), v.trim().to_string()))
                })
                .collect();
            if facts.is_empty() {
                return "Error: no valid key=value pairs found".to_string();
            }
            let from = current_agent_id.unwrap_or("anonymous");
            let mut registry = AgentRegistry::load_or_create();
            registry.share_knowledge(from, cat, &facts);
            match registry.save() {
                Ok(()) => format!("Shared {} facts in category '{}'", facts.len(), cat),
                Err(e) => format!("Share failed: {e}"),
            }
        }

        "receive_knowledge" => {
            let Some(agent_id) = current_agent_id else {
                return "Error: agent must be registered first".to_string();
            };
            let mut registry = AgentRegistry::load_or_create();
            let facts = registry.receive_shared_knowledge(agent_id);
            let _ = registry.save();
            if facts.is_empty() {
                return "No new shared knowledge.".to_string();
            }
            let mut out = format!("Received {} facts:\n", facts.len());
            for f in &facts {
                let age = (chrono::Utc::now() - f.timestamp).num_minutes();
                out.push_str(&format!(
                    "  [{}] {}={} (from {}, {}m ago)\n",
                    f.category, f.key, f.value, f.from_agent, age
                ));
            }
            out
        }

        "poll_events" => {
            let Some(agent_id) = current_agent_id else {
                return "Error: agent must be registered first".to_string();
            };
            let workspace_id = to_agent.unwrap_or(project_root);
            let channel_id = category.unwrap_or("default");
            let since_id: i64 = message.and_then(|s| s.parse().ok()).unwrap_or(0);
            let limit: usize = _ttl_hours.unwrap_or(50) as usize;

            let rt = crate::core::context_os::runtime();
            let events = rt.bus.read(workspace_id, channel_id, since_id, limit);

            let filter = crate::core::context_os::TopicFilter {
                agent_id: Some(agent_id.to_string()),
                kinds: privacy.and_then(|s| {
                    let kinds: Vec<_> = s
                        .split(',')
                        .map(|k| crate::core::context_os::ContextEventKindV1::parse(k.trim()))
                        .collect();
                    if kinds.is_empty() { None } else { Some(kinds) }
                }),
                ..Default::default()
            };

            let filtered: Vec<_> = events.into_iter().filter(|e| filter.matches(e)).collect();
            if filtered.is_empty() {
                return format!("No new events since id={since_id} for {agent_id}.");
            }

            let mut out = format!("Events ({}, since={since_id}):\n", filtered.len());
            for ev in &filtered {
                let actor = ev.actor.as_deref().unwrap_or("-");
                out.push_str(&format!(
                    "  #{} [{}] actor={} cl={} ({})\n",
                    ev.id,
                    ev.kind,
                    actor,
                    ev.consistency_level,
                    ev.timestamp.format("%H:%M:%S")
                ));
            }
            if let Some(last) = filtered.last() {
                out.push_str(&format!("cursor={}", last.id));
            }
            out
        }

        _ => format!(
            "Unknown action: {action}. Use: register, list, post, read, status, info, handoff, sync, poll_events, diary, recall_diary, diaries, share_knowledge, receive_knowledge"
        ),
    }
}
