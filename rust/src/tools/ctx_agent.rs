use crate::core::a2a::message::{MessagePriority, PrivacyLevel};
use crate::core::a2a::task::TaskStore;
use crate::core::agents::{
    AgentDiary, AgentRegistry, AgentStatus, DiaryEntryType, ScratchpadEntry,
};
use crate::core::ocla::capsule::global_capsule_store;
use crate::core::ocla::registry::OclaRegistry;
use crate::core::ocla::types::{
    AGENT_ENVELOPE_SCHEMA_VERSION, AgentEnvelope, AgentMessageRequest, OclaRequestContext,
};

#[path = "ctx_agent_control.rs"]
mod control;
use control::{control_status, lease_resource};

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
    "wait",
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
    "send",
    "reply",
    "inbox",
];

/// Arguments of the Hive message actions (`send`, `reply`, `inbox`).
#[derive(Debug, Clone, Default)]
pub struct MessageArgs {
    pub message_type: Option<String>,
    pub text: Option<String>,
    pub to: Option<String>,
    pub to_instance: Option<String>,
    pub reply_to: Option<String>,
    pub deadline_hours: Option<u64>,
    pub references: Vec<String>,
}

impl MessageArgs {
    fn untyped(text: Option<&str>, category: Option<&str>, to: Option<&str>) -> Self {
        Self {
            message_type: category.map(str::to_string),
            text: text.map(str::to_string),
            to: to.map(str::to_string),
            ..Self::default()
        }
    }
}

/// `send`: a typed message to the project's Hive channel. `reply`: answers a received message in its own network and
/// thread (type defaults to `answer`). `inbox`: unread Hive messages and
/// task events, every body wrapped as foreign data.
pub fn handle_message(
    action: &str,
    args: &MessageArgs,
    project_root: &str,
    current_agent_id: Option<&str>,
) -> String {
    use crate::core::hive::bus::messages::{
        InboxFilter, SendRequest, TYPES, inbox_for_root, send_from_root,
    };
    let Some(agent) = current_agent_id else {
        return "Error: agent must be registered first (use action=register)".to_string();
    };
    if action == "inbox" {
        let filter = InboxFilter {
            message_type: args.message_type.as_deref(),
        };
        return inbox_for_root(project_root, agent, filter)
            .unwrap_or_else(|e| format!("Error: {e}"));
    }
    let Some(text) = args.text.as_deref() else {
        return format!("Error: message is required for {action}");
    };
    let reply_to = args.reply_to.as_deref();
    if action == "reply" && reply_to.is_none() {
        return "Error: reply_to (the gid of the message answered) is required".to_string();
    }
    let message_type = args
        .message_type
        .as_deref()
        .or((action == "reply").then_some("answer"));
    let request = SendRequest {
        message_type,
        text,
        to: args.to.as_deref(),
        to_instance: args.to_instance.as_deref(),
        reply_to,
        deadline_hours: args.deadline_hours,
        swarm_id: None,
        references: &args.references,
    };
    match send_from_root(project_root, agent, &request) {
        Ok(id) => format!(
            "Queued [{}] {id}; the Hive relay sends it (types: {TYPES})",
            message_type.unwrap_or("?")
        ),
        Err(error) => format!("Error: {error}"),
    }
}

/// `action=read` with a typed outcome: `Ok` is the reply, `Err` is the text
/// of a failed read — the same text [`handle`] answers. `read` is the one
/// action of this tool that declares every source
/// (`CtxAgentTool::publishes_every_source`), so its failure must reach the
/// registered tool as a failure and never as a reply that could be recorded
/// as a delivery of the sources captured before it failed.
pub fn handle_read(
    message: Option<&str>,
    current_agent_id: Option<&str>,
    project_root: &str,
) -> Result<String, String> {
    // Same admission as `handle`: unadmitted caller text refuses the call.
    if let Some(text) = message
        && let Err(withheld) = crate::core::context_admission::carrier::admit_for_store(text)
    {
        return Err(format!("Error: {}", withheld.message("agent message")));
    }
    read_messages(current_agent_id, project_root)
}

/// The unread messages of `current_agent_id` in `project_root`.
fn read_messages(current_agent_id: Option<&str>, project_root: &str) -> Result<String, String> {
    let Some(agent_id) = current_agent_id else {
        return Err("Error: agent must be registered first (use action=register)".to_string());
    };
    let mut text = read_messages_as(agent_id, project_root, true)?;
    // What other agents declared in the linked emspace space, beside the bus.
    if let Some(sessions) = crate::core::providers::emspace::admitted_sessions_block(project_root) {
        text.push_str(&sessions);
    }
    Ok(text)
}

/// The line `read`/`wait` print once after an expired registration was renewed.
fn renewed_line(agent_id: &str) -> String {
    format!(
        "Note: the registration of {agent_id} had expired and was renewed; retry a claim refused meanwhile.\n"
    )
}

/// One `read` answers at most this much message text (at least one
/// message); the rest stays unread and the answer says how many. Hosts
/// archive or cut longer tool answers, and the cut messages were lost.
pub(crate) const READ_PAGE_BYTES: usize = 6_000;

/// One unread message as `read` shows it.
fn render_message(m: &ScratchpadEntry, project_root: &str) -> String {
    let age = (chrono::Utc::now() - m.timestamp).num_minutes();
    // A new delivery: re-authorized under the current policy. One from
    // another instance carries its sender and kind inside the fence.
    if crate::core::hive::bus::relay::is_inbound(m) {
        return format!(
            "  [hive] ({age}m ago):\n{}\n",
            m.served_message(Some(project_root))
        );
    }
    format!(
        "  [{}] from {} ({}m ago): {}\n",
        m.category,
        m.from_agent,
        age,
        m.served_message(Some(project_root))
    )
}

/// The unread messages of `agent_id`, oldest first and at most
/// [`READ_PAGE_BYTES`] per call; only the ones shown are marked read.
/// `own_process`: the call comes from the agent's own session, which renews
/// its lease (and revives an expired one); `lean-ctx agent wait` reads for
/// it from another process.
pub(crate) fn read_messages_as(
    agent_id: &str,
    project_root: &str,
    own_process: bool,
) -> Result<String, String> {
    let (shown, left) = match AgentRegistry::mutate_locked(|registry| {
        if own_process {
            registry.renew_worker(agent_id)?;
        }
        registry.read_unread_page(agent_id, project_root, READ_PAGE_BYTES, |m| {
            render_message(m, project_root)
        })
    }) {
        Ok((_, Ok(page))) => page,
        Ok((_, Err(error))) => {
            tracing::warn!("lean-ctx: message read rejected: {error}");
            return Err(format!("Error: {error}"));
        }
        Err(e) => {
            tracing::warn!(
                "lean-ctx: failed to persist agent registry (messages may reappear): {e}"
            );
            return Err("Error: failed to read messages".to_string());
        }
    };

    let mut renewed = if crate::core::agents::take_renewed(agent_id) {
        renewed_line(agent_id)
    } else {
        String::new()
    };
    // An MCP server older than the installed binary misses its fixes.
    if own_process && let Some(note) = crate::core::binary_identity::replaced_note() {
        renewed.insert_str(0, &note);
    }
    // Typed Hive messages wait in their own inbox (I1); point at it.
    let hive = crate::core::hive::bus::messages::waiting_hint(project_root, agent_id)
        .map(|hint| format!("{hint}\n"))
        .unwrap_or_default();
    if shown.is_empty() {
        return Ok(format!("{renewed}{hive}No new messages."));
    }

    let mut out = format!("{renewed}{hive}New messages ({}):\n", shown.len());
    for line in &shown {
        out.push_str(line);
    }
    if left > 0 {
        out.push_str(&format!(
            "{left} more unread (still unread): read again for the next ones.\n"
        ));
    }
    Ok(out)
}

/// `wait` without `timeout_secs`, and its ceiling.
pub(crate) const WAIT_DEFAULT_SECS: u64 = 600;
pub(crate) const WAIT_MAX_SECS: u64 = 1800;

/// The stable reply of a `wait` that saw no message.
pub(crate) fn wait_timeout_line(secs: u64) -> String {
    format!("No new messages within {secs}s.")
}

/// `action=wait`: block until a message for `agent_id` (to it or broadcast,
/// not its own) is waiting in `project_root`, then answer exactly like
/// `read`; `Ok(None)` after `timeout_secs` (default 600, at most 1800).
/// `own_process` as in [`read_messages_as`]: the agent's own session keeps
/// its lease alive while it waits.
pub(crate) fn wait_messages(
    agent_id: &str,
    project_root: &str,
    timeout_secs: Option<u64>,
    own_process: bool,
) -> Result<Option<String>, String> {
    let secs = timeout_secs.unwrap_or(WAIT_DEFAULT_SECS).min(WAIT_MAX_SECS);
    if own_process {
        // Fail fast on an unknown or finished agent instead of waiting.
        AgentRegistry::renew_worker_persistent(agent_id).map_err(|e| format!("Error: {e}"))?;
    }
    let arrived = AgentRegistry::wait_for_unread(
        agent_id,
        project_root,
        std::time::Duration::from_secs(secs),
        || {
            if own_process && let Err(e) = AgentRegistry::renew_worker_persistent(agent_id) {
                tracing::debug!("lean-ctx: wait keepalive for {agent_id}: {e}");
            }
        },
    );
    if arrived {
        return read_messages_as(agent_id, project_root, own_process).map(Some);
    }
    crate::core::agents::friction::record_for(
        crate::core::agents::friction::WAIT_TIMEOUT,
        Some(agent_id),
        Some(project_root),
    );
    Ok(None)
}

/// The text of `wait` for the MCP tool: the messages, or the timeout line
/// (after the renewal note, when there is one).
pub fn handle_wait(
    current_agent_id: Option<&str>,
    project_root: &str,
    timeout_secs: Option<u64>,
) -> Result<String, String> {
    let Some(agent_id) = current_agent_id else {
        return Err("Error: agent must be registered first (use action=register)".to_string());
    };
    let secs = timeout_secs.unwrap_or(WAIT_DEFAULT_SECS).min(WAIT_MAX_SECS);
    Ok(
        match wait_messages(agent_id, project_root, timeout_secs, true)? {
            Some(text) => text,
            None if crate::core::agents::take_renewed(agent_id) => {
                format!("{}{}", renewed_line(agent_id), wait_timeout_line(secs))
            }
            None => wait_timeout_line(secs),
        },
    )
}

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
    request_id: Option<&str>,
) -> String {
    // The caller's text is admitted once, before any action stores, routes,
    // derives from or echoes it (post, handoff, status, diary, knowledge,
    // brief). Unadmitted input is never echoed back: a refusal names the typed
    // reason only. Allowed text is unchanged byte for byte.
    let admitted_message = match message
        .map(crate::core::context_admission::carrier::admit_for_store)
        .transpose()
    {
        Ok(admitted) => admitted,
        Err(withheld) => return format!("Error: {}", withheld.message("agent message")),
    };
    let message = admitted_message.as_deref();
    match action {
        "register" => {
            let atype = agent_type.unwrap_or("unknown");
            match AgentRegistry::mutate_locked(|registry| {
                registry.cleanup_stale(presence_ttl_hours());
                registry.register(atype, role, project_root, durable_identity_id)
            })
            .and_then(|(_, agent_id)| agent_id)
            {
                Ok(agent_id) => {
                    // The relay announces the agent (and its role) now, not
                    // on its next tick: a claim right after register finds it.
                    crate::core::hive::bus::presence::request_announce();
                    // The note goes last: callers take the id from the
                    // text after the first ':'.
                    format!(
                        "Agent registered: {agent_id} (type: {atype}, role: {}){}",
                        role.unwrap_or("none"),
                        crate::core::binary_identity::replaced_note()
                            .map(|note| format!("\n{}", note.trim_end()))
                            .unwrap_or_default()
                    )
                }
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
            let sessions = crate::core::providers::emspace::admitted_sessions_block(project_root)
                .unwrap_or_default();
            if agents.is_empty() {
                return format!("No active agents for this project.{sessions}");
            }

            // V5.3: sessions are listed under the durable worker they attach
            // to; without an enrolled instance workers have no `wkr_` id.
            let store = crate::core::hive::HiveStore::open().ok();
            let instance = store
                .as_ref()
                .and_then(|s| s.instance().ok().flatten())
                .map(|record| record.instance_id);
            let book = match (&store, &instance) {
                (Some(store), Some(id)) => {
                    crate::core::hive::workers::attach::list(store, id).unwrap_or_default()
                }
                _ => Vec::new(),
            };
            let mut roster = crate::core::hive::workers::roster::render(
                &agents,
                instance.as_deref().map(|id| (id, book.as_slice())),
                chrono::Utc::now(),
            );
            roster.push_str(&sessions);
            roster
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
                request_id: request_id.map(str::to_string),
            };
            match OclaRegistry::global().agent_gateway.route_message(&request) {
                Ok(msg_id) => {
                    let target = to_agent.unwrap_or("all agents (broadcast)");
                    format!("Posted [{cat}] to {target}: {msg} (id: {msg_id})")
                }
                Err(e) => format!("Post failed: {e}"),
            }
        }

        // The text of either outcome, for callers of this untyped entry
        // point; the registered tool takes the typed one (`handle_read`).
        "read" => match read_messages(current_agent_id, project_root) {
            Ok(text) | Err(text) => text,
        },

        // This entry point has no `timeout_secs`: the default applies; the
        // registered tool passes it (`handle_wait`).
        "wait" => match handle_wait(current_agent_id, project_root, None) {
            Ok(text) | Err(text) => text,
        },

        // Hive agent messages (I1). This entry point has no `type`/`reply_to`
        // arguments: `category` carries the type; the registered tool passes all.
        "send" => handle_message(
            "send",
            &MessageArgs::untyped(message, category, to_agent),
            project_root,
            current_agent_id,
        ),
        "reply" => handle_message(
            "reply",
            &MessageArgs::untyped(message, category, to_agent),
            project_root,
            current_agent_id,
        ),
        "inbox" => handle_message(
            "inbox",
            &MessageArgs::untyped(message, category, to_agent),
            project_root,
            current_agent_id,
        ),

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
            // Hive bus control (C5b): a vetoed handoff is refused.
            if let Some(refusal) = crate::core::hive::bus::control::veto_refusal(
                Some(from),
                &[&format!("handoff:{from}:{target}")],
            ) {
                return format!("Handoff rejected: {refusal}");
            }
            let summary = message.unwrap_or("(no summary provided)");
            let scratchpad_ttl = scratchpad_default_ttl_hours();
            // C5c: a target Hive reports on another machine gets an offer
            // through Hive; the sender finishes only on its ack.
            let remote = match crate::core::hive::bus::handoff::remote_target(
                std::path::Path::new(project_root),
                target,
            ) {
                Ok(remote) => remote,
                Err(ambiguous) => return format!("Handoff rejected: {ambiguous}"),
            };
            let waiting = format!("handing off to {target} (awaiting the Hive ack)");

            let entry_id = match AgentRegistry::mutate_locked(|registry| {
                // The handoff message must be persisted before the sender is
                // marked Finished. A bound presence that finishes first can no
                // longer post, so the delivery would be silently dropped.
                let entry_id = registry.post_message_scoped(
                    Some(project_root),
                    from,
                    Some(target),
                    "handoff",
                    &format!("HANDOFF from {from}: {summary}"),
                    PrivacyLevel::Team,
                    MessagePriority::Normal,
                    Some(scratchpad_ttl),
                )?;
                if remote.is_some() {
                    registry.set_status(from, AgentStatus::Idle, Some(&waiting))?;
                } else {
                    registry.set_status(from, AgentStatus::Finished, Some("handed off"))?;
                }
                Ok(entry_id)
            })
            .and_then(|(_, result)| result)
            {
                Ok(entry_id) => entry_id,
                Err(error) => return format!("Handoff rejected: {error}"),
            };
            let offered = remote.map(|r| {
                crate::core::hive::bus::handoff::offer(
                    &r.data_dir,
                    &r.org_id,
                    &r.vault_id,
                    from,
                    target,
                    &entry_id,
                )
            });

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
            crate::core::scent_field::deposit_for(
                project_root,
                from,
                crate::core::scent_field::ScentKind::Done,
                summary,
                1.0,
            );

            match offered {
                Some(Ok(handoff_id)) => format!(
                    "Handoff offered (hive): {from} → {target} handoff_id={handoff_id}; \
                     {from} finishes when {target}'s machine acks\nSummary: {summary}"
                ),
                Some(Err(error)) => {
                    let _ = AgentRegistry::mutate_locked(|registry| {
                        registry.set_status(from, AgentStatus::Active, Some("handoff not sent"))
                    });
                    format!("Handoff rejected: the Hive offer could not be queued ({error})")
                }
                None => format!("Handoff complete: {from} → {target}\nSummary: {summary}"),
            }
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
            match crate::core::scent_field::claim(project_root, &agent, &normalized) {
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
            crate::core::scent_field::release(project_root, &agent, &normalized);
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
            // The pack builder leaves out facts that may not be delivered now.
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
                            && e.project_root.as_deref().is_some_and(|own| {
                                crate::core::pathutil::same_project(own, project_root)
                            })
                    })
                    .count()
            });
            let agents = registry.list_active(Some(project_root));

            if agents.is_empty() {
                return "No active agents to sync with.".to_string();
            }

            // Only this project's shares (`ctx_share` partitions by project).
            let shared_dir = crate::core::data_dir::lean_ctx_data_dir()
                .unwrap_or_default()
                .join("agents")
                .join("shared")
                .join(crate::core::project_hash::hash_project_root(project_root));

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
            // hot files across this project's agents, no scratchpad reads needed.
            let scents = crate::core::scent_field::sync_block(project_root);
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
                    e.project_root
                        .as_deref()
                        .is_some_and(|own| crate::core::pathutil::same_project(own, project_root))
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
                    message: maybe_redact(&m.served_message(Some(project_root)), should_redact),
                    // The admission binding is store-internal provenance.
                    metadata: m
                        .metadata
                        .iter()
                        .filter(|(k, _)| k.as_str() != crate::core::agents::BINDING_METADATA_KEY)
                        .map(|(k, v)| (k.clone(), maybe_redact(v, should_redact)))
                        .collect(),
                    timestamp: m.timestamp.to_rfc3339(),
                    expires_at: m.expires_at.map(|t| t.to_rfc3339()),
                    read_by_count: m.read_by.len(),
                })
                .collect();
            messages.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then_with(|| a.id.cmp(&b.id)));

            // Prune and read under the store lock: a concurrent task mutation
            // is either fully before or fully after this one, never lost.
            // Export runs once per request, so it waits for a busy store
            // instead of failing after the default lock timeout.
            const EXPORT_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
            let tasks = TaskStore::default_path().and_then(|path| {
                TaskStore::mutate_locked_within(&path, EXPORT_LOCK_WAIT, |task_store| {
                    task_store.cleanup_old(72);
                    Ok::<_, std::io::Error>(
                        task_store
                            .tasks_for_agent(agent_id)
                            .into_iter()
                            .take(200)
                            .map(|t| ExportTaskV1 {
                                id: t.id.clone(),
                                from_agent: t.from_agent.clone(),
                                to_agent: t.to_agent.clone(),
                                state: t.state.to_string(),
                                description: maybe_redact(&t.served_description(), should_redact),
                                created_at: t.created_at.to_rfc3339(),
                                updated_at: t.updated_at.to_rfc3339(),
                                messages: t.messages.len(),
                                artifacts: t.artifacts.len(),
                                transitions: t.history.len(),
                            })
                            .collect::<Vec<ExportTaskV1>>(),
                    )
                })
            });
            let mut tasks = match tasks {
                Ok(tasks) => tasks,
                Err(error) => {
                    tracing::warn!("lean-ctx: failed to read task store: {error}");
                    return "Error: task storage unavailable".to_string();
                }
            };
            tasks.sort_by(|a, b| {
                b.updated_at
                    .cmp(&a.updated_at)
                    .then_with(|| a.id.cmp(&b.id))
            });

            let diary =
                AgentDiary::load_for_project(agent_id, project_root).map(|d| ExportDiaryV1 {
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
                let diaries = AgentDiary::list_for_project(project_root);
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
            match AgentDiary::load_for_project(agent_id, project_root) {
                Some(diary) => diary.format_summary(),
                None => format!("No diary found for agent '{agent_id}'."),
            }
        }

        "diaries" => {
            let diaries = AgentDiary::list_for_project(project_root);
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
            let Some(from) = current_agent_id else {
                return "Error: agent must be registered first".to_string();
            };
            // Shared knowledge reaches only the agents of this project.
            match AgentRegistry::mutate_locked(|registry| {
                registry.require_active_for_presence(from, "sharing knowledge")?;
                registry.share_knowledge(project_root, from, cat, &facts)
            })
            .and_then(|(_, shared)| shared)
            {
                Ok(count) => format!("Shared {count} facts in category '{cat}' with this project"),
                Err(e) => format!("Share failed: {e}"),
            }
        }

        "receive_knowledge" => {
            let Some(agent_id) = current_agent_id else {
                return "Error: agent must be registered first".to_string();
            };
            let facts = match AgentRegistry::mutate_locked(|registry| {
                registry.require_active_for_presence(agent_id, "receiving knowledge")?;
                registry.receive_shared_knowledge(project_root, agent_id)
            })
            .and_then(|(_, facts)| facts)
            {
                Ok(facts) => facts,
                Err(e) => return format!("Receive failed: {e}"),
            };
            if facts.is_empty() {
                return "No new shared knowledge.".to_string();
            }
            let mut out = format!("Received {} facts:\n", facts.len());
            for f in &facts {
                let age = (chrono::Utc::now() - f.timestamp).num_minutes();
                out.push_str(&format!(
                    "  [{}] {} (from {}, {}m ago)\n",
                    f.category, f.text, f.from_agent, age
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
            let (resource_kind, resource_ref) = match lease_resource(project_root, target) {
                Ok(resource) => resource,
                Err(error) => return format!("Lease rejected: {error}"),
            };
            // Hive bus control (C5b): a vetoed resource is refused.
            if let Some(refusal) =
                crate::core::hive::bus::control::veto_refusal(Some(agent_id), &[&resource_ref])
            {
                return format!("Lease rejected: {refusal}");
            }
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
                Ok(crate::core::agent_lease::AgentLeaseAcquireV1::Granted(lease)) => {
                    // C5c: across machines through Hive (bounded, see bus::leases).
                    use crate::core::hive::bus::leases::{self, Outcome};
                    let hive = leases::acquire(
                        std::path::Path::new(project_root),
                        agent_id,
                        target,
                        duration_ms,
                        &lease.lease_ref,
                    );
                    let release_local = || {
                        let _ = crate::core::agent_lease::release_shared(
                            resource_kind,
                            &resource_ref,
                            agent_id,
                            &lease.lease_ref,
                        );
                    };
                    match hive {
                        Outcome::Held {
                            agent_id: holder,
                            instance_id,
                            expires_at,
                        } => {
                            release_local();
                            format!(
                                "Lease HELD (hive): resource={resource_ref} owner={holder} \
                                 instance={instance_id} expires={expires_at}"
                            )
                        }
                        Outcome::Vetoed => {
                            release_local();
                            "Lease rejected: a workspace veto bars this agent from it \
                             until the veto is lifted"
                                .to_string()
                        }
                        Outcome::Granted { ref expires_at, .. } => format!(
                            "Lease granted: {} resource={} owner={} expires_at_epoch_ms={} {} hive_expires_at={expires_at}",
                            lease.lease_ref,
                            resource_ref,
                            agent_id,
                            lease.expires_at_epoch_ms,
                            leases::scope_line(&hive)
                        ),
                        other => format!(
                            "Lease granted: {} resource={} owner={} expires_at_epoch_ms={} {}",
                            lease.lease_ref,
                            resource_ref,
                            agent_id,
                            lease.expires_at_epoch_ms,
                            leases::scope_line(&match other {
                                Outcome::Superseded => Outcome::Local("Hive lost the lease".into()),
                                other => other,
                            })
                        ),
                    }
                }
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
            let (resource_kind, resource_ref) = match lease_resource(project_root, target) {
                Ok(resource) => resource,
                Err(error) => return format!("Release failed: {error}"),
            };
            // C5c: the Hive side is released by the relay (queued, no network here).
            crate::core::hive::bus::leases::queue_release(lease_ref);
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
            let (resource_kind, resource_ref) = match lease_resource(project_root, target) {
                Ok(resource) => resource,
                Err(error) => return format!("Lease renewal rejected: {error}"),
            };
            if let Some(refusal) = crate::core::hive::bus::control::veto_refusal(
                Some(agent_id),
                &[lease_ref, &resource_ref],
            ) {
                return format!("Lease renewal rejected: {refusal}");
            }
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
                Ok(lease) => {
                    use crate::core::hive::bus::leases::{self, Outcome};
                    match leases::renew(std::path::Path::new(project_root), lease_ref, duration_ms)
                    {
                        outcome @ (Outcome::Superseded | Outcome::Vetoed) => {
                            let _ = crate::core::agent_lease::release_shared(
                                resource_kind,
                                &resource_ref,
                                agent_id,
                                lease_ref,
                            );
                            if outcome == Outcome::Vetoed {
                                "Lease renewal rejected: a workspace veto ended it".to_string()
                            } else {
                                "Lease renewal rejected: Hive no longer holds it for you \
                                 (expired on the service clock or superseded); acquire it again"
                                    .to_string()
                            }
                        }
                        hive => format!(
                            "Lease renewed: {} resource={} owner={} expires_at_epoch_ms={} {}",
                            lease.lease_ref,
                            resource_ref,
                            agent_id,
                            lease.expires_at_epoch_ms,
                            leases::scope_line(&hive)
                        ),
                    }
                }
                Err(e) => format!("Lease renewal rejected: {e}"),
            }
        }

        _ => format!("Unknown action: {action}. Use: {}", ACTIONS.join(", ")),
    }
}

#[cfg(test)]
#[path = "ctx_agent_tests.rs"]
mod tests;
// SPDX-License-Identifier: Apache-2.0
