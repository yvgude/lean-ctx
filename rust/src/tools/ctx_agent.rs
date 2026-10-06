use std::fmt::Write as _;

use crate::core::a2a::message::{MessagePriority, PrivacyLevel};
use crate::core::a2a::task::TaskStore;
use crate::core::agents::{AgentDiary, AgentRegistry, AgentStatus, DiaryEntryType};
use crate::core::evidence_ledger::EvidenceLedgerV1;
use crate::core::ocla::capsule::global_capsule_store;
use crate::core::ocla::registry::OclaRegistry;
use crate::core::ocla::types::{
    AGENT_ENVELOPE_SCHEMA_VERSION, AgentEnvelope, AgentMessageRequest, OclaRequestContext,
};

fn presence_ttl_hours() -> u64 {
    crate::core::config::Config::load()
        .agents
        .presence_ttl_hours
}

fn scratchpad_default_ttl_hours() -> u64 {
    crate::core::config::Config::load()
        .agents
        .scratchpad_default_ttl_hours
}

/// Every action [`handle`] dispatches, one entry each. The MCP schema's
/// `action` enum and the unknown-action reply are both built from this list.
pub(crate) const ACTIONS: &[&str] = &[
    "register",
    "list",
    "post",
    "read",
    "status",
    "info",
    "handoff",
    "sync",
    "claim",
    "release",
    "brief",
    "return",
    "export",
    "diary",
    "recall_diary",
    "diaries",
    "share_knowledge",
    "receive_knowledge",
    "poll_events",
    "control_status",
    "lease_acquire",
    "lease_renew",
    "lease_release",
];

#[allow(clippy::too_many_arguments)]
pub fn handle(
    action: &str,
    agent_type: Option<&str>,
    role: Option<&str>,
    durable_identity_id: Option<&str>,
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
            match AgentRegistry::mutate_locked(|registry| {
                registry.cleanup_stale(presence_ttl_hours());
                registry.register(atype, role, project_root, durable_identity_id)
            })
            .and_then(|(_, agent_id)| agent_id)
            {
                Ok(agent_id) => format!(
                    "Agent registered: {agent_id} (type: {atype}, role: {})",
                    role.unwrap_or("none")
                ),
                Err(e) => format!("Registration failed: {e}"),
            }
        }

        "list" => {
            let registry = match AgentRegistry::mutate_locked(|registry| {
                registry.cleanup_stale(presence_ttl_hours());
            }) {
                Ok((registry, ())) => registry,
                Err(e) => {
                    tracing::warn!("lean-ctx: failed to persist agent registry: {e}");
                    AgentRegistry::load_or_create()
                }
            };

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
            let ttl_hours = _ttl_hours.or(Some(scratchpad_default_ttl_hours()));
            if msg_privacy == PrivacyLevel::Private && to_agent.is_none() {
                return "Error: private messages require to_agent".to_string();
            }
            let request = AgentMessageRequest {
                project_root: project_root.to_string(),
                from_agent_id: from.to_string(),
                to_agent_id: to_agent.map(str::to_string),
                category: cat.to_string(),
                message: msg.to_string(),
                privacy: msg_privacy,
                priority: msg_priority,
                ttl_hours,
            };
            match OclaRegistry::global().agent_gateway.route_message(&request) {
                Ok(msg_id) => {
                    let target = to_agent.unwrap_or("all agents (broadcast)");
                    format!("Posted [{cat}] to {target}: {msg} (id: {msg_id})")
                }
                Err(e) => format!("Post failed: {e}"),
            }
        }

        "read" => {
            let Some(agent_id) = current_agent_id else {
                return "Error: agent must be registered first (use action=register)".to_string();
            };
            let messages = match AgentRegistry::mutate_locked(|registry| {
                registry.update_heartbeat(agent_id)?;
                registry
                    .read_unread_scoped(agent_id, project_root)
                    .map(|messages| messages.into_iter().cloned().collect::<Vec<_>>())
            }) {
                Ok((_, Ok(messages))) => messages,
                Ok((_, Err(error))) => {
                    tracing::warn!("lean-ctx: message read rejected: {error}");
                    return format!("Error: {error}");
                }
                Err(e) => {
                    tracing::warn!(
                        "lean-ctx: failed to persist agent registry (messages may reappear): {e}"
                    );
                    return "Error: failed to read messages".to_string();
                }
            };

            if messages.is_empty() {
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

            match AgentRegistry::mutate_locked(|registry| {
                registry.set_status(agent_id, new_status.clone(), status_msg)
            })
            .and_then(|(_, result)| result)
            {
                Ok(()) => format!(
                    "Status updated: {} → {}{}",
                    agent_id,
                    new_status,
                    status_msg.map(|m| format!(" ({m})")).unwrap_or_default()
                ),
                Err(e) => format!("Status update rejected: {e}"),
            }
        }

        "info" => {
            let registry = AgentRegistry::mutate_locked(|registry| {
                registry.cleanup_stale(presence_ttl_hours());
            })
            .map(|(registry, ())| registry)
            .unwrap_or_else(|_| AgentRegistry::load_or_create());
            let total = registry.agents.len();
            let active = registry.list_active(None).len();
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
            let scratchpad_ttl = scratchpad_default_ttl_hours();

            if let Err(error) = AgentRegistry::mutate_locked(|registry| {
                // The handoff message must be persisted before the sender is
                // marked Finished. A bound presence that finishes first can no
                // longer post, so the delivery would be silently dropped.
                registry.post_message_scoped(
                    Some(project_root),
                    from,
                    Some(target),
                    "handoff",
                    &format!("HANDOFF from {from}: {summary}"),
                    PrivacyLevel::Team,
                    MessagePriority::Normal,
                    Some(scratchpad_ttl),
                )?;
                registry.set_status(from, AgentStatus::Finished, Some("handed off"))?;
                Ok(())
            })
            .and_then(|(_, result)| result)
            {
                return format!("Handoff rejected: {error}");
            }

            // Route through OCLA AgentGateway for cross-process visibility.
            let handoff_message = format!("HANDOFF: {summary}");
            let capsule_ref = global_capsule_store().register(handoff_message.as_bytes());
            let mut envelope = AgentEnvelope {
                schema_version: AGENT_ENVELOPE_SCHEMA_VERSION,
                relay_id: String::new(),
                context: OclaRequestContext::new(
                    format!("handoff:{capsule_ref}"),
                    project_root.to_string(),
                    from.to_string(),
                    capsule_ref.clone(),
                    None,
                    Some(format!("handoff:{from}:{target}")),
                ),
                from_agent_id: from.to_string(),
                to_agent_id: target.to_string(),
                capsule_ref,
                budget_tokens: handoff_message.len().max(1) as u64,
            };
            if let Err(error) = envelope.assign_relay_id() {
                tracing::debug!("OCLA relay setup failed (non-fatal): {error}");
            } else if let Err(error) = OclaRegistry::global().agent_gateway.relay_agent(envelope) {
                tracing::debug!("OCLA relay failed (non-fatal): {error}");
            }

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
            let registry = AgentRegistry::mutate_locked(|registry| {
                registry.cleanup_stale(presence_ttl_hours());
            })
            .map(|(registry, ())| registry)
            .unwrap_or_else(|_| AgentRegistry::load_or_create());
            let pending_count = current_agent_id.map_or(0, |id| {
                registry
                    .scratchpad
                    .iter()
                    .filter(|e| {
                        !e.read_by.contains(&id.to_string())
                            && e.from_agent != id
                            && (e.to_agent.is_none() || e.to_agent.as_deref() == Some(id))
                            && e.project_root.as_deref() == Some(project_root)
                    })
                    .count()
            });
            let agents = registry.list_active(Some(project_root));

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

        // Premium control-plane snapshot: one bounded, read-only view of
        // presence, messages and persisted Work Graph execution state.
        // This keeps agents from guessing whether they should retry, wait or
        // integrate, and gives the operator a stable next action.
        "control_status" => control_status(project_root, current_agent_id),

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
            registry.cleanup_stale(presence_ttl_hours());

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
                .filter(|e| {
                    e.project_root.as_deref() == Some(project_root)
                        && (e.to_agent.is_none() || e.to_agent.as_deref() == Some(agent_id))
                })
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

            let mut task_store = match TaskStore::load() {
                Ok(store) => store,
                Err(error) => {
                    tracing::warn!("lean-ctx: failed to read task store: {error}");
                    return "Error: task storage unavailable".to_string();
                }
            };
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
            match AgentRegistry::mutate_locked(|registry| {
                registry.require_active_for_presence(from, "sharing knowledge")?;
                registry.share_knowledge(from, cat, &facts);
                Ok::<(), String>(())
            }) {
                Ok(_) => format!("Shared {} facts in category '{}'", facts.len(), cat),
                Err(e) => format!("Share failed: {e}"),
            }
        }

        "receive_knowledge" => {
            let Some(agent_id) = current_agent_id else {
                return "Error: agent must be registered first".to_string();
            };
            let facts = AgentRegistry::mutate_locked(|registry| {
                registry.require_active_for_presence(agent_id, "receiving knowledge")?;
                Ok(registry.receive_shared_knowledge(agent_id))
            })
            .and_then(|(_, facts)| facts)
            .unwrap_or_default();
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

        // P11: Acquire an opaque path/symbol lease for mutation planning.
        "lease_acquire" => {
            let Some(agent_id) = current_agent_id else {
                return "Error: agent must be registered first".to_string();
            };
            let Some(target) = message else {
                return "Error: message (path or symbol:<name>) is required".to_string();
            };
            if let Err(error) = AgentRegistry::load_or_create()
                .require_active_for_presence(agent_id, "acquiring a lease")
            {
                return format!("Lease rejected: {error}");
            }
            let (resource_kind, resource_ref) = lease_resource(target);
            let duration_ms = match _ttl_hours.unwrap_or(0) {
                0 => 10 * 60 * 1_000,
                1 => 60 * 60 * 1_000,
                _ => return "Error: ttl_hours for lease must be 0 (10min) or 1 (1h)".to_string(),
            };
            let request = crate::core::agent_lease::AgentLeaseRequestV1 {
                schema_version: crate::core::agent_lease::AGENT_LEASE_SCHEMA_VERSION,
                lease_request_ref: format!(
                    "lease-request:{}",
                    blake3::hash(format!("{agent_id}:{resource_ref}").as_bytes()).to_hex()
                ),
                resource_kind,
                resource_ref: resource_ref.clone(),
                owner_agent_id: agent_id.to_string(),
                duration_ms,
            };
            match crate::core::agent_lease::acquire_shared(request) {
                Ok(crate::core::agent_lease::AgentLeaseAcquireV1::Granted(lease)) => format!(
                    "Lease granted: {} resource={} owner={} expires_at_epoch_ms={}",
                    lease.lease_ref, resource_ref, agent_id, lease.expires_at_epoch_ms
                ),
                Ok(crate::core::agent_lease::AgentLeaseAcquireV1::HeldBy {
                    owner_agent_id,
                    lease_ref,
                    expires_at_epoch_ms,
                }) => format!(
                    "Lease HELD: resource={resource_ref} owner={owner_agent_id} lease_ref={lease_ref} expires={expires_at_epoch_ms}"
                ),
                Err(e) => format!("Lease rejected: {e}"),
            }
        }

        // P11: Release a previously acquired lease.
        "lease_release" => {
            let Some(agent_id) = current_agent_id else {
                return "Error: agent must be registered first".to_string();
            };
            let Some(target) = message else {
                return "Error: message (same path or symbol:<name>) is required".to_string();
            };
            let Some(lease_ref) = category else {
                return "Error: category (lease_ref from lease_acquire) is required".to_string();
            };
            let (resource_kind, resource_ref) = lease_resource(target);
            match crate::core::agent_lease::release_shared(
                resource_kind,
                &resource_ref,
                agent_id,
                lease_ref,
            ) {
                Ok(true) => format!("Lease released: {lease_ref} resource={resource_ref}"),
                Ok(false) => format!("No active lease found for resource={resource_ref}"),
                Err(e) => format!("Release failed: {e}"),
            }
        }

        // P11: Renew an owned lease without rotating its fencing token.
        // A lease that already expired is rejected and must be re-acquired;
        // this prevents a delayed worker from reviving abandoned work.
        "lease_renew" => {
            let Some(agent_id) = current_agent_id else {
                return "Error: agent must be registered first".to_string();
            };
            let Some(target) = message else {
                return "Error: message (same path or symbol:<name>) is required for renew"
                    .to_string();
            };
            let Some(lease_ref) = category else {
                return "Error: category (lease_ref from lease_acquire) is required".to_string();
            };
            if let Err(error) = AgentRegistry::load_or_create()
                .require_active_for_presence(agent_id, "renewing a lease")
            {
                return format!("Lease renewal rejected: {error}");
            }
            let (resource_kind, resource_ref) = lease_resource(target);
            let duration_ms = match _ttl_hours.unwrap_or(0) {
                0 => 10 * 60 * 1_000,
                1 => 60 * 60 * 1_000,
                _ => return "Error: ttl_hours for lease must be 0 (10min) or 1 (1h)".to_string(),
            };
            match crate::core::agent_lease::renew_shared(
                resource_kind,
                &resource_ref,
                agent_id,
                lease_ref,
                duration_ms,
            ) {
                Ok(lease) => format!(
                    "Lease renewed: {} resource={} owner={} expires_at_epoch_ms={}",
                    lease.lease_ref, resource_ref, agent_id, lease.expires_at_epoch_ms
                ),
                Err(e) => format!("Lease renewal rejected: {e}"),
            }
        }

        _ => format!("Unknown action: {action}. Use: {}", ACTIONS.join(", ")),
    }
}

/// Convert a raw path/symbol target into an opaque lease resource.
fn lease_resource(target: &str) -> (crate::core::agent_lease::AgentLeaseResourceKindV1, String) {
    if let Some(symbol) = target.strip_prefix("symbol:") {
        (
            crate::core::agent_lease::AgentLeaseResourceKindV1::Symbol,
            format!("symbolref:{}", blake3::hash(symbol.as_bytes()).to_hex()),
        )
    } else {
        let normalized = crate::core::pathutil::normalize_tool_path(target);
        let mut path_ref = String::from("path:");
        for byte in normalized.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'-' | b'_') {
                path_ref.push(byte as char);
            } else {
                let _ = write!(path_ref, "%{byte:02X}");
            }
        }
        // Keep the bounded opaque-reference contract for unusually long paths.
        // Typical paths retain hierarchy so parent/child claims conflict; the
        // hash fallback remains safe but intentionally cannot infer hierarchy.
        if path_ref.len() > 256 {
            path_ref = format!("pathref:{}", blake3::hash(normalized.as_bytes()).to_hex());
        }
        (
            crate::core::agent_lease::AgentLeaseResourceKindV1::Path,
            path_ref,
        )
    }
}

fn control_status(project_root: &str, current_agent_id: Option<&str>) -> String {
    let registry = AgentRegistry::mutate_locked(|registry| {
        registry.cleanup_stale(presence_ttl_hours());
    })
    .map(|(registry, ())| registry)
    .unwrap_or_else(|_| AgentRegistry::load_or_create());

    let scoped_agents = registry
        .agents
        .iter()
        .filter(|agent| agent.project_root == project_root);
    let mut presence = std::collections::BTreeMap::from([
        ("active", 0usize),
        ("idle", 0usize),
        ("finished", 0usize),
    ]);
    for agent in scoped_agents {
        let key = agent.status.to_string();
        if let Some(count) = presence.get_mut(key.as_str()) {
            *count += 1;
        }
    }

    let pending_messages = current_agent_id.map_or(0, |agent_id| {
        registry
            .scratchpad
            .iter()
            .filter(|entry| {
                !entry.read_by.iter().any(|reader| reader == agent_id)
                    && entry.from_agent != agent_id
                    && (entry.to_agent.is_none() || entry.to_agent.as_deref() == Some(agent_id))
                    && entry.project_root.as_deref() == Some(project_root)
            })
            .count()
    });

    let mut graphs = Vec::new();
    let mut queued_nodes = 0usize;
    let mut active_nodes = 0usize;
    let mut stale_nodes = 0usize;
    match crate::core::work_graph_store::WorkGraphStore::load(project_root) {
        Ok(store) => {
            for graph_id in store.graph_ids().take(32) {
                let Some(graph) = store.graph(graph_id) else {
                    continue;
                };
                let observation = graph.observation();
                let nodes = observation
                    .get("nodes")
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut counts = std::collections::BTreeMap::new();
                for node in &nodes {
                    let state = node
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown");
                    *counts.entry(state.to_string()).or_insert(0usize) += 1;
                }
                queued_nodes += counts.get("pending").copied().unwrap_or(0);
                active_nodes += counts.get("active").copied().unwrap_or(0);
                stale_nodes += counts.get("stale").copied().unwrap_or(0);
                graphs.push(serde_json::json!({
                    "graph_id": graph_id,
                    "revision": store.graph_revision(graph_id).ok(),
                    "node_counts": counts,
                }));
            }
        }
        Err(error) => {
            tracing::debug!("lean-ctx: control snapshot could not load Work Graph: {error}");
        }
    }

    let next_action = if stale_nodes > 0 {
        "reconcile stale Work Graph executions"
    } else if queued_nodes > 0 {
        "claim one pending Work Graph node"
    } else if active_nodes > 0 {
        "continue current work and renew leases"
    } else {
        "lead may enqueue the next independent task"
    };

    serde_json::to_string_pretty(&serde_json::json!({
        "schema_version": 1,
        "project_root": project_root,
        "presence": presence,
        "pending_messages": pending_messages,
        "work_graphs": graphs,
        "queued_nodes": queued_nodes,
        "active_nodes": active_nodes,
        "stale_nodes": stale_nodes,
        "next_action": next_action,
    }))
    .unwrap_or_else(|_| "{\"error\":\"control snapshot serialization failed\"}".to_string())
}

#[cfg(test)]
mod tests {
    use super::handle;

    #[test]
    fn register_forwards_explicit_durable_identity_binding() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::core::agent_registry::register("durable-tool", "coder", "owner")
            .expect("durable identity");
        let result = handle(
            "register",
            Some("mcp"),
            Some("context-engine"),
            Some("durable-tool"),
            "/project",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
        );
        assert!(
            result.starts_with("Agent registered:"),
            "unexpected: {result}"
        );
        let registry = crate::core::agents::AgentRegistry::load().expect("presence registry");
        assert_eq!(
            registry.agents[0].durable_identity_id.as_deref(),
            Some("durable-tool")
        );
    }

    #[test]
    fn bound_handoff_posts_before_finishing_presence() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::core::agent_registry::register("durable-handoff", "coder", "owner")
            .expect("durable identity");
        let registered = handle(
            "register",
            Some("codex"),
            Some("review"),
            Some("durable-handoff"),
            "/project",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
        );
        let agent_id = registered
            .split_whitespace()
            .nth(2)
            .expect("registered agent id")
            .to_string();

        let result = handle(
            "handoff",
            None,
            None,
            None,
            "/project",
            Some(&agent_id),
            Some("verified summary"),
            None,
            Some(&agent_id),
            None,
            None,
            None,
            None,
            None,
            false,
            None,
        );
        assert!(
            result.starts_with("Handoff complete:"),
            "unexpected: {result}"
        );

        let registry = crate::core::agents::AgentRegistry::load().expect("presence registry");
        let agent = registry
            .agents
            .iter()
            .find(|agent| agent.agent_id == agent_id)
            .expect("handed-off presence");
        assert_eq!(agent.status, crate::core::agents::AgentStatus::Finished);
        let message = registry.scratchpad.last().expect("handoff message");
        assert_eq!(message.from_agent, agent_id);
        assert_eq!(message.to_agent.as_deref(), Some(agent_id.as_str()));
        assert_eq!(message.category, "handoff");
        assert!(message.message.contains("verified summary"));
    }
}
// SPDX-License-Identifier: Apache-2.0
