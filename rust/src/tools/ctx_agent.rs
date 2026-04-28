use crate::core::agents::{AgentDiary, AgentRegistry, AgentStatus, DiaryEntryType};

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
            let _ = registry.save();

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
            let Some(msg) = message else { return "Error: message is required for post".to_string() };
            let cat = category.unwrap_or("status");
            let from = current_agent_id.unwrap_or("anonymous");
            let mut registry = AgentRegistry::load_or_create();
            let msg_id = registry.post_message(from, to_agent, cat, msg);
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
                    return "Error: agent must be registered first (use action=register)"
                        .to_string()
                };
            let mut registry = AgentRegistry::load_or_create();
            let messages = registry.read_unread(agent_id);

            if messages.is_empty() {
                let _ = registry.save();
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
            let _ = registry.save();
            out
        }

        "status" => {
            let Some(agent_id) = current_agent_id else { return "Error: agent must be registered first".to_string() };
            let new_status = match status {
                Some("active") => AgentStatus::Active,
                Some("idle") => AgentStatus::Idle,
                Some("finished") => AgentStatus::Finished,
                Some(other) => {
                    return format!("Unknown status: {other}. Use: active, idle, finished")
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
            let Some(from) = current_agent_id else { return "Error: agent must be registered first".to_string() };
            let Some(target) = to_agent else { return "Error: to_agent is required for handoff".to_string() };
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

            format!("Handoff complete: {from} → {target}\nSummary: {summary}")
        }

        "sync" => {
            let registry = AgentRegistry::load_or_create();
            let agents: Vec<&crate::core::agents::AgentEntry> = registry
                .agents
                .iter()
                .filter(|a| a.status != AgentStatus::Finished)
                .collect();

            if agents.is_empty() {
                return "No active agents to sync with.".to_string();
            }

            let pending_count = registry
                .scratchpad
                .iter()
                .filter(|e| {
                    if let Some(ref id) = current_agent_id {
                        !e.read_by.contains(&id.to_string()) && e.from_agent != *id
                    } else {
                        false
                    }
                })
                .count();

            let shared_dir = crate::core::data_dir::lean_ctx_data_dir()
                .unwrap_or_default()
                .join("agents")
                .join("shared");

            let shared_count = if shared_dir.exists() {
                std::fs::read_dir(&shared_dir)
                    .map_or(0, std::iter::Iterator::count)
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
            out
        }

        "diary" => {
            let Some(agent_id) = current_agent_id else { return "Error: agent must be registered first".to_string() };
            let Some(content) = message else { return "Error: message is required for diary entry".to_string() };
            let entry_type = match category.unwrap_or("progress") {
                "discovery" | "found" => DiaryEntryType::Discovery,
                "decision" | "decided" => DiaryEntryType::Decision,
                "blocker" | "blocked" => DiaryEntryType::Blocker,
                "progress" | "done" => DiaryEntryType::Progress,
                "insight" => DiaryEntryType::Insight,
                other => return format!("Unknown diary type: {other}. Use: discovery, decision, blocker, progress, insight"),
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
            let Some(msg_text) = message else { return "Error: message required (format: key1=value1;key2=value2)".to_string() };
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
            let Some(agent_id) = current_agent_id else { return "Error: agent must be registered first".to_string() };
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

        _ => format!("Unknown action: {action}. Use: register, list, post, read, status, info, handoff, sync, diary, recall_diary, diaries, share_knowledge, receive_knowledge"),
    }
}
