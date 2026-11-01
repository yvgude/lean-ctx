use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};
use std::time::Duration;

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_bool, get_str};
use crate::tool_defs::tool_def;

/// #1570 P1: `compact` records a directive (summary in `value`); the proxy
/// replaces the pre-cut span on every request until `restore` clears it —
/// lossless by construction since the client resends full history each turn.
fn handle_compact_action(
    action: &str,
    value: Option<&str>,
    args: &Map<String, Value>,
) -> ToolOutput {
    let text = if action == "restore" {
        if crate::core::compact_directive::clear() {
            "Compaction cleared — the next request carries the full original history again."
                .to_string()
        } else {
            "No active compaction to restore.".to_string()
        }
    } else {
        let keep = args
            .get("keep_recent_turns")
            .and_then(serde_json::Value::as_u64)
            .map_or(6, |n| n as usize)
            .max(4);
        match value {
            None | Some("") => "compact requires value = the summary that replaces the \
                 compacted span. Write it exhaustive-but-lean: every file path, decision, \
                 constraint and finding that keeps context integrity; quote short user \
                 requests verbatim; drop dead-end exploration."
                .to_string(),
            Some(summary) => {
                match crate::core::compact_directive::create(keep, summary.to_string()) {
                    Ok(()) => format!(
                        "Compaction armed (keeps last {keep} user turns verbatim). It applies \
                         to your NEXT request onward, only outside the provider-cached \
                         prefix, at tool-pair-safe boundaries. Undo any time: \
                         ctx_session action=\"restore\"."
                    ),
                    Err(reason) => reason,
                }
            }
        }
    };
    ToolOutput {
        text,
        original_tokens: 0,
        saved_tokens: 0,
        mode: Some(action.to_string()),
        path: None,
        changed: false,
        shell_outcome: None,
        content_blocks: None,
    }
}

pub struct CtxSessionTool;

/// Session management is useful, but it must never make the entire MCP server
/// wait behind a long-lived writer. Explicitly retryable operations fail fast.
const SESSION_LOCK_BUDGET: Duration = Duration::from_millis(250);

fn session_matches_snapshot(
    live: &crate::core::session::SessionState,
    snapshot: &crate::core::session::SessionState,
) -> bool {
    live.id == snapshot.id
        && live.version == snapshot.version
        && live.project_root == snapshot.project_root
        && live.shell_cwd == snapshot.shell_cwd
        && live.extra_roots == snapshot.extra_roots
}

impl McpTool for CtxSessionTool {
    fn name(&self) -> &'static str {
        "ctx_session"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_session",
            "Session memory. save at session end, load at start, status = snapshot;\n\
             task|finding|decision record progress (value=text).\n\
             ANTIPATTERN: permanent project knowledge → ctx_knowledge.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "status|handoff|load|save|task|finding|decision|compact|restore|list|… (invalid action lists all)"
                    },
                    "value": { "type": "string" },
                    "tag": { "type": "string", "enum": ["note", "dead_end"], "description": "finding: dead_end = tried and abandoned" },
                    "keep_recent_turns": { "type": "integer", "description": "compact: trailing user turns kept verbatim (default 6)" },
                    "session_id": { "type": "string", "description": "Omit for your own session" }
                },
                "required": ["action"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action")
            .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
        let value = get_str(args, "value");
        let sid = get_str(args, "session_id");
        let format = get_str(args, "format");
        let path = get_str(args, "path");
        let write = get_bool(args, "write").unwrap_or(false);
        let privacy = get_str(args, "privacy");
        let terse = get_bool(args, "terse");
        let dead_end = match get_str(args, "tag").as_deref() {
            None | Some("note") => false,
            Some("dead_end") => true,
            Some(_) => {
                return Err(ErrorData::invalid_params(
                    "tag must be note or dead_end",
                    None,
                ));
            }
        };

        // Restore is content-free. Compact summaries are admitted against a
        // detached, trusted session scope before the directive reaches disk.
        if action == "restore" {
            return Ok(handle_compact_action(&action, value.as_deref(), args));
        }
        if action == "compact" {
            let Some(summary) = value.as_deref().filter(|summary| !summary.is_empty()) else {
                return Ok(handle_compact_action(&action, value.as_deref(), args));
            };
            let session_handle = ctx
                .session
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
            let Some(snapshot) = crate::server::bounded_lock::read_for(
                session_handle,
                "ctx_session compact admission snapshot",
                SESSION_LOCK_BUDGET,
            )
            .map(|session| session.clone()) else {
                return Err(ErrorData::internal_error(
                    "session is busy; retry the operation",
                    None,
                ));
            };
            let admitted =
                crate::core::session::admission::admit_storable_session_text(&snapshot, summary)
                    .map_err(|_| {
                        ErrorData::invalid_params(
                            "compact summary withheld by current policy",
                            None,
                        )
                    })?;
            return Ok(handle_compact_action(&action, Some(&admitted), args));
        }

        let tool_calls_handle = ctx
            .tool_calls
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("tool_calls not available", None))?;
        let call_durations: Vec<(String, u64)> = crate::server::bounded_lock::read_for(
            tool_calls_handle,
            "ctx_session tool-call snapshot",
            SESSION_LOCK_BUDGET,
        )
        .map_or_else(Vec::new, |tc| {
            tc.iter().map(|c| (c.tool.clone(), c.duration_ms)).collect()
        });
        let agent_id = ctx
            .agent_id
            .as_ref()
            .and_then(|agent_id| agent_id.try_read().ok().and_then(|id| id.clone()));

        let result = if let Some(result) = crate::tools::ctx_session::handle_without_session(
            &action,
            value.as_deref(),
            sid.as_deref(),
        ) {
            result
        } else {
            let session_handle = ctx
                .session
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("session not available", None))?;

            if action == "save" {
                let Some(snapshot) = crate::server::bounded_lock::read_for(
                    session_handle,
                    "ctx_session save snapshot",
                    SESSION_LOCK_BUDGET,
                )
                .map(|session| session.clone()) else {
                    return Err(ErrorData::internal_error(
                        "session is busy; retry the operation",
                        None,
                    ));
                };
                let result =
                    match crate::core::session::SessionState::save_shared_sync(session_handle) {
                        Err(error) => format!("Save failed: {error}"),
                        Ok(receipt) if receipt.current => {
                            format!("Session {} saved (v{}).", receipt.id, receipt.version)
                        }
                        Ok(receipt) => format!(
                            "Session {} snapshot v{} saved; newer session changes remain pending.",
                            receipt.id, receipt.version
                        ),
                    };
                crate::core::session::admission::admit_tool_value(&snapshot, &result)
                    .unwrap_or_else(|_| {
                        "ERROR: session save result withheld by current policy.".into()
                    })
            } else if action == "reset" {
                let Some(snapshot) = crate::server::bounded_lock::read_for(
                    session_handle,
                    "ctx_session reset snapshot",
                    SESSION_LOCK_BUDGET,
                )
                .map(|session| session.clone()) else {
                    return Err(ErrorData::internal_error(
                        "session is busy; retry the operation",
                        None,
                    ));
                };
                let output =
                    match crate::core::session::SessionState::save_shared_sync(session_handle) {
                        Err(error) => {
                            format!("Reset refused: current session was not saved: {error}")
                        }
                        Ok(receipt)
                            if !receipt.current
                                || receipt.id != snapshot.id
                                || receipt.version != snapshot.version =>
                        {
                            "Reset refused: session changed while it was being saved; retry.".into()
                        }
                        Ok(_) => {
                            let Some(mut live) = crate::server::bounded_lock::write_for(
                                session_handle,
                                "ctx_session reset commit",
                                SESSION_LOCK_BUDGET,
                            ) else {
                                return Err(ErrorData::internal_error(
                                    "session is busy; retry the operation",
                                    None,
                                ));
                            };
                            if !session_matches_snapshot(&live, &snapshot)
                                || !std::sync::Arc::ptr_eq(&live.save_gate, &snapshot.save_gate)
                            {
                                return Err(ErrorData::internal_error(
                                    "session changed during reset; retry the operation",
                                    None,
                                ));
                            }
                            crate::tools::ctx_session::reset_after_save(&mut live)
                        }
                    };
                crate::core::session::admission::admit_tool_value(&snapshot, &output)
                    .unwrap_or_else(|_| "ERROR: reset result withheld by current policy.".into())
            } else if action == "load" {
                let Some(snapshot) = crate::server::bounded_lock::read_for(
                    session_handle,
                    "ctx_session load snapshot",
                    SESSION_LOCK_BUDGET,
                )
                .map(|session| session.clone()) else {
                    return Err(ErrorData::internal_error(
                        "session is busy; retry the operation",
                        None,
                    ));
                };
                let (candidate, output) =
                    crate::tools::ctx_session::prepare_load(&snapshot, sid.as_deref()).map_err(
                        |_| {
                            ErrorData::invalid_params(
                                "session load denied by current project scope or content policy",
                                None,
                            )
                        },
                    )?;
                let Some(mut live) = crate::server::bounded_lock::write_for(
                    session_handle,
                    "ctx_session load commit",
                    SESSION_LOCK_BUDGET,
                ) else {
                    return Err(ErrorData::internal_error(
                        "session is busy; retry the operation",
                        None,
                    ));
                };
                if !session_matches_snapshot(&live, &snapshot) {
                    return Err(ErrorData::internal_error(
                        "session changed during load; retry the operation",
                        None,
                    ));
                }
                *live = candidate;
                output
            } else if matches!(action.as_str(), "export" | "resume" | "snapshot") {
                let Some(snapshot) = crate::server::bounded_lock::read_for(
                    session_handle,
                    "ctx_session snapshot",
                    SESSION_LOCK_BUDGET,
                )
                .map(|session| session.clone()) else {
                    return Err(ErrorData::internal_error(
                        "session is busy; retry the operation",
                        None,
                    ));
                };
                crate::tools::ctx_session::handle_snapshot_read_action(
                    &snapshot,
                    &action,
                    value.as_deref(),
                    crate::tools::ctx_session::SessionToolOptions {
                        format: format.as_deref(),
                        path: path.as_deref(),
                        write,
                        privacy: privacy.as_deref(),
                        terse,
                        agent_id: agent_id.as_deref(),
                    },
                )
                .expect("snapshot action is explicitly handled")
            } else if matches!(action.as_str(), "status" | "show" | "handoff") {
                let Some(snapshot) = crate::server::bounded_lock::read_for(
                    session_handle,
                    "ctx_session read",
                    SESSION_LOCK_BUDGET,
                )
                .map(|session| session.clone()) else {
                    return Err(ErrorData::internal_error(
                        "session is busy; retry the operation",
                        None,
                    ));
                };
                crate::tools::ctx_session::handle_read_only(&snapshot, &action)
                    .expect("read-only action is explicitly handled")
            } else if matches!(action.as_str(), "task" | "finding" | "decision" | "list") {
                let Some(snapshot) = crate::server::bounded_lock::read_for(
                    session_handle,
                    "ctx_session admitted action snapshot",
                    SESSION_LOCK_BUDGET,
                )
                .map(|session| session.clone()) else {
                    return Err(ErrorData::internal_error(
                        "session is busy; retry the operation",
                        None,
                    ));
                };
                let prepared = crate::tools::ctx_session::prepare_admitted_action(
                    &snapshot,
                    &call_durations,
                    &action,
                    value.as_deref(),
                    sid.as_deref(),
                    crate::tools::ctx_session::SessionToolOptions {
                        format: format.as_deref(),
                        path: path.as_deref(),
                        write,
                        privacy: privacy.as_deref(),
                        terse,
                        agent_id: agent_id.as_deref(),
                    },
                )
                .map_err(|_| {
                    ErrorData::invalid_params(
                        "session action withheld by current project or content policy",
                        None,
                    )
                })?;
                if matches!(action.as_str(), "task" | "finding" | "decision") {
                    let candidate = prepared.candidate;
                    let output = prepared.output;
                    let Some(mut live) = crate::server::bounded_lock::write_for(
                        session_handle,
                        "ctx_session admitted action commit",
                        SESSION_LOCK_BUDGET,
                    ) else {
                        return Err(ErrorData::internal_error(
                            "session is busy; retry the operation",
                            None,
                        ));
                    };
                    if !session_matches_snapshot(&live, &snapshot) {
                        return Err(ErrorData::internal_error(
                            "session changed during content admission; retry the operation",
                            None,
                        ));
                    }
                    if !prepared.store_admission.is_current() {
                        return Err(ErrorData::internal_error(
                            "session policy changed during content admission; retry the operation",
                            None,
                        ));
                    }
                    *live = candidate.clone();
                    drop(live);
                    // The finding as the session keeps it (admitted, redacted)
                    // becomes one of the session episode's dead ends.
                    if dead_end
                        && action == "finding"
                        && let Some(finding) = candidate.findings.last()
                    {
                        crate::core::hive::capture::episode::note_dead_end(
                            &candidate.id,
                            &finding.summary,
                        );
                    }
                    crate::core::session::admission::admit_tool_value(&candidate, &output)
                        .unwrap_or_else(|_| "[lean-ctx gateway: session content withheld]".into())
                } else {
                    if !prepared.store_admission.is_current() {
                        return Err(ErrorData::internal_error(
                            "session policy changed during content admission; retry the operation",
                            None,
                        ));
                    }
                    prepared.output
                }
            } else {
                let Some(mut session) = crate::server::bounded_lock::write_for(
                    session_handle,
                    "ctx_session write",
                    SESSION_LOCK_BUDGET,
                ) else {
                    return Err(ErrorData::internal_error(
                        "session is busy; retry the operation",
                        None,
                    ));
                };
                crate::tools::ctx_session::handle(
                    &mut session,
                    &call_durations,
                    &action,
                    value.as_deref(),
                    sid.as_deref(),
                    crate::tools::ctx_session::SessionToolOptions {
                        format: format.as_deref(),
                        path: path.as_deref(),
                        write,
                        privacy: privacy.as_deref(),
                        terse,
                        agent_id: agent_id.as_deref(),
                    },
                )
            }
        };

        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some(action),
            path: None,
            changed: false,
            shell_outcome: None,
            content_blocks: None,
        })
    }

    // No action declares `publishes_every_source` (the trait default applies).
    // `status` and `show` print a counters line and `Root: <project>`: that
    // is session-derived context, which is neither published into the call's
    // source capture nor marks it incomplete, so their reply cannot claim a
    // complete source set. A session reply is therefore never registered in
    // the delivered-lineage register. The delivery calls stay
    // (`SessionState::format_compact_delivered`, `finding_served`): a finding
    // bound to a source that no longer admits is left out of the reply.
}
