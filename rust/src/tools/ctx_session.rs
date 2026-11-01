// SPDX-License-Identifier: Apache-2.0

use crate::core::session::SessionState;

#[derive(Clone, Copy, Debug)]
pub struct SessionToolOptions<'a> {
    pub format: Option<&'a str>,
    pub path: Option<&'a str>,
    pub write: bool,
    pub privacy: Option<&'a str>,
    /// For `action=configure`: set terse output mode when `Some`.
    pub terse: Option<bool>,
    pub agent_id: Option<&'a str>,
}

pub fn handle(
    session: &mut SessionState,
    tool_calls: &[(String, u64)],
    action: &str,
    value: Option<&str>,
    session_id: Option<&str>,
    opts: SessionToolOptions<'_>,
) -> String {
    match prepare_admitted_action(session, tool_calls, action, value, session_id, opts) {
        Err(_) => "ERROR: session action withheld by current project or content policy.".into(),
        Ok(prepared) => {
            if !prepared.store_admission.is_current() {
                return "ERROR: session policy changed; retry the operation.".into();
            }
            let PreparedSessionAction {
                candidate,
                mut output,
                completed_task,
                ..
            } = prepared;
            *session = candidate;
            if let Some(note) = record_completed_task_after_commit(
                session,
                completed_task,
                tool_calls,
                opts.agent_id,
            ) {
                output.push_str(&note);
            }
            crate::core::session::admission::admit_tool_value(session, &output).unwrap_or_else(
                |error| {
                    // Two different refusals. The gateway withheld the reply's
                    // content: its notice (`[lean-ctx …]`). Or no policy scope
                    // could be established for this session at all (no usable
                    // project root): that is a failed action, reported in the
                    // tool's own `ERROR:` form like every other scope denial —
                    // never the action's unadmitted output.
                    if error.starts_with("[lean-ctx") {
                        "[lean-ctx gateway: session content withheld]".into()
                    } else {
                        "ERROR: session action withheld by current project or content policy."
                            .into()
                    }
                },
            )
        }
    }
}

pub(crate) struct PreparedSessionAction {
    pub(crate) candidate: SessionState,
    pub(crate) output: String,
    pub(crate) completed_task: bool,
    pub(crate) store_admission: crate::core::context_admission::stores::StoreAdmission,
}

/// Prepare content-bearing actions from a detached snapshot. Callers with a
/// live session lock commit candidate only after checking snapshot identity.
pub(crate) fn prepare_admitted_action(
    session: &SessionState,
    tool_calls: &[(String, u64)],
    action: &str,
    value: Option<&str>,
    session_id: Option<&str>,
    opts: SessionToolOptions<'_>,
) -> Result<PreparedSessionAction, String> {
    let store_admission = crate::core::context_admission::stores::StoreAdmission::current();
    let mut candidate = session.clone();
    let (output, completed_task) = match action {
        "task" | "finding" | "decision" => {
            let fallback = match action {
                "finding" => "(no summary)",
                _ => "(no description)",
            };
            let admitted = crate::core::session::admission::admit_storable_session_text(
                session,
                value.unwrap_or(fallback),
            )?;
            let completed = action == "task" && task_is_complete(&admitted);
            let output = match action {
                "task" => handle_task(&mut candidate, tool_calls, Some(&admitted), opts),
                "finding" => handle_finding(&mut candidate, Some(&admitted)),
                _ => handle_decision(&mut candidate, Some(&admitted)),
            };
            (output, completed)
        }
        "list" => (handle_scoped_list(session)?, false),
        "restore" => (handle_scoped_restore(session, session_id)?, false),
        "episodes" => (
            handle_episodes_scoped(session, tool_calls, value, opts)?,
            false,
        ),
        "procedures" => (handle_procedures_scoped(session, value)?, false),
        _ => (
            handle_legacy(&mut candidate, tool_calls, action, value, session_id, opts),
            false,
        ),
    };
    if !store_admission.is_current() {
        return Err("session admission policy changed; retry".into());
    }
    Ok(PreparedSessionAction {
        candidate,
        output,
        completed_task,
        store_admission,
    })
}

/// Run completion learning only after the caller commits or saves the updated
/// session. Storage failures never roll back the successful session update.
pub(crate) fn record_completed_task_after_commit(
    session: &SessionState,
    completed_task: bool,
    tool_calls: &[(String, u64)],
    agent_id: Option<&str>,
) -> Option<String> {
    if !completed_task {
        return None;
    }
    auto_record_episode(session, tool_calls, agent_id)
        .ok()
        .flatten()
        .map(|id| format!("\nEpisode auto-recorded: {id}"))
}

fn task_is_complete(description: &str) -> bool {
    let lower = description.to_lowercase();
    description.contains("[100%]") || lower.contains("[done]") || lower.contains("[complete]")
}

fn handle_legacy(
    session: &mut SessionState,
    tool_calls: &[(String, u64)],
    action: &str,
    value: Option<&str>,
    session_id: Option<&str>,
    opts: SessionToolOptions<'_>,
) -> String {
    if action == "handoff" {
        return handle_handoff(session);
    }

    match action {
        "status" | "show" => handle_status(session),
        "load" => handle_load(session, session_id),
        "save" => handle_save(session),
        "export" => handle_export(session, value, opts),
        "import" => handle_import(session, value, opts),
        "task" => handle_task(session, tool_calls, value, opts),
        "finding" => handle_finding(session, value),
        "decision" => handle_decision(session, value),
        "reset" => handle_reset(session),
        "list" => handle_list(session),
        "cleanup" => handle_cleanup(),
        "configure" => handle_configure(session, opts),
        "snapshot" => handle_snapshot(session),
        "restore" => handle_restore(session, session_id),
        "resume" => handle_resume(session),
        "profile" => handle_profile(value),
        "role" => handle_role(value),
        "diff" => handle_diff(value),
        "output_stats" => handle_output_stats(),
        "verify" => handle_verify(),
        "episodes" => handle_episodes(session, tool_calls, value, opts),
        "procedures" => handle_procedures(session, value),
        _ => handle_unknown(action),
    }
}

/// Handle actions that do not access live-session state. Keeping them outside
/// the live session lock prevents an explicit `list`/`cleanup` request from
/// stalling unrelated reads and shells behind an O(session-store) operation.
/// Some actions intentionally update independent process-global configuration.
pub fn handle_without_session(
    action: &str,
    value: Option<&str>,
    _session_id: Option<&str>,
) -> Option<String> {
    match action {
        "cleanup" => Some(handle_cleanup()),
        "profile" => Some(handle_profile(value)),
        "role" => Some(handle_role(value)),
        "diff" => Some(handle_diff(value)),
        "output_stats" => Some(handle_output_stats()),
        "verify" => Some(handle_verify()),
        _ => None,
    }
}

/// Handle session reads after rechecking all recalled content under the
/// current project and Gateway policies.
pub fn handle_read_only(session: &SessionState, action: &str) -> Option<String> {
    let Ok(projected) = crate::core::session::admission::for_delivery(session) else {
        return Some("[lean-ctx gateway: session content withheld]".into());
    };
    let text = match action {
        "status" | "show" => handle_status(&projected),
        "handoff" => handle_handoff(&projected),
        _ => return None,
    };
    Some(
        crate::core::session::admission::admit_tool_value(&projected, &text)
            .unwrap_or_else(|_| "[lean-ctx gateway: session content withheld]".into()),
    )
}

/// Run snapshot/export work after the caller cloned live state and released its
/// lock. Persisted projections are admitted before they reach an archive writer.
pub fn handle_snapshot_read_action(
    session: &SessionState,
    action: &str,
    value: Option<&str>,
    opts: SessionToolOptions<'_>,
) -> Option<String> {
    match action {
        "export" => {
            let Ok(projected) = crate::core::session::admission::for_storage(session) else {
                return Some("ERROR: session export withheld by current policy.".into());
            };
            let output = handle_export(&projected, value, opts);
            // The default export is structural JSON from the already-admitted
            // typed projection; masking that serialization would corrupt it.
            let structural_json =
                opts.format
                    .unwrap_or(if opts.write { "summary" } else { "json" })
                    != "summary";
            Some(if structural_json {
                output
            } else {
                crate::core::session::admission::admit_tool_value(&projected, &output)
                    .unwrap_or_else(|_| "ERROR: session export withheld by current policy.".into())
            })
        }
        "resume" => {
            let Ok(projected) = crate::core::session::admission::for_delivery(session) else {
                return Some("[lean-ctx gateway: session content withheld]".into());
            };
            let output = projected.build_resume_block();
            Some(
                crate::core::session::admission::admit_tool_value(&projected, &output)
                    .unwrap_or_else(|_| "[lean-ctx gateway: session content withheld]".into()),
            )
        }
        "snapshot" => {
            let Ok(projected) = crate::core::session::admission::for_storage(session) else {
                return Some("ERROR: session snapshot withheld by current policy.".into());
            };
            let output = projected.save_compaction_snapshot().map_or_else(
                |error| format!("Snapshot failed: {error}"),
                |snapshot| {
                    format!(
                        "Compaction snapshot saved ({} bytes).\n{snapshot}",
                        snapshot.len()
                    )
                },
            );
            Some(
                crate::core::session::admission::admit_tool_value(&projected, &output)
                    .unwrap_or_else(|_| {
                        "ERROR: session snapshot withheld by current policy.".into()
                    }),
            )
        }
        _ => None,
    }
}

/// The session summary as a delivery: findings publish their sources or mark
/// the call's capture, and any other session text marks it incomplete
/// (`SessionState::format_compact_delivered`).
///
/// When the session's root was chosen among several workspace roots, a line
/// names the choice, the other roots and how to change it (AP04-G7).
fn handle_status(session: &SessionState) -> String {
    let summary = session.format_compact_delivered();
    // Under a launcher Workspace its binding is the root line (#2120).
    match crate::core::workspace_authority::current() {
        crate::core::workspace_authority::Authority::Legacy => {}
        crate::core::workspace_authority::Authority::Workspace(snapshot) => {
            return format!("{summary}\n{}", snapshot.status_line());
        }
        crate::core::workspace_authority::Authority::Unavailable(reason) => {
            return format!(
                "{summary}\n{}",
                crate::core::workspace_authority::unavailable_reason(&reason)
            );
        }
    }
    match session
        .project_root
        .as_deref()
        .and_then(crate::core::root_selection::recorded_for)
    {
        Some(selection) => format!("{summary}\n{}", selection.notice()),
        None => summary,
    }
}

fn handle_handoff(session: &SessionState) -> String {
    // What this session changed in the knowledge store, from its journal:
    // the successor reads the changeset instead of diffing the store.
    let changeset = session
        .project_root
        .as_deref()
        .map(|root| crate::tools::ctx_knowledge::session_change_count(root, &session.id))
        .filter(|count| *count > 0)
        .map(|count| {
            format!(
                "\nKnowledge written by this session: {count} change(s) — ctx_knowledge action=changes query=session:{}",
                session.id
            )
        })
        .unwrap_or_default();
    if session.handoff_context.is_empty() {
        return format!("Handoff context: no recent cross-session decisions.{changeset}");
    }

    let mut output = format!(
        "Handoff context ({} recent cross-session decisions):",
        session.handoff_context.len()
    );
    for fact in &session.handoff_context {
        output.push_str(&format!(
            "\n  [{}] {} (confidence: {:.0}%)",
            fact.category,
            fact.value,
            fact.confidence * 100.0
        ));
    }
    output.push_str(&changeset);
    output
}

fn handle_load(session: &mut SessionState, session_id: Option<&str>) -> String {
    match prepare_load(session, session_id) {
        Ok((loaded, output)) => {
            *session = loaded;
            output
        }
        Err(error) => error,
    }
}

/// Load and admit a detached candidate. The caller may commit it only after
/// confirming that the live session identity and version still match.
pub(crate) fn prepare_load(
    session: &SessionState,
    session_id: Option<&str>,
) -> Result<(SessionState, String), String> {
    const SCOPE_DENIED: &str = "ERROR: session load denied: scoped session unavailable or mismatched; live session unchanged.";

    let receiving_root = session.strict_project_root().ok_or(SCOPE_DENIED)?;
    // Without an id: the caller's own saved session — never the project's
    // latest, which may be another agent's (I07/S03). Another session is
    // loaded only by naming it, e.g. the id a handoff gave.
    let id = session_id.unwrap_or(session.id.as_str());
    let loaded =
        SessionState::load_by_id_for_project_root(id, &receiving_root).map_err(|_| SCOPE_DENIED)?;

    let Some(mut loaded) = loaded else {
        return Err(SCOPE_DENIED.into());
    };
    if loaded.strict_project_root().as_deref() != Some(receiving_root.as_str()) {
        return Err(SCOPE_DENIED.into());
    }

    // These fields come from live MCP roots/shell and must not be replaced by
    // persisted state. Validate root scope before this policy lookup.
    loaded.extra_roots.clone_from(&session.extra_roots);
    loaded.shell_cwd.clone_from(&session.shell_cwd);
    let admitted = crate::core::session::admission::for_storage(&loaded)
        .map_err(|_| "ERROR: session load withheld by current policy; live session unchanged.")?;
    let summary = handle_status(&admitted);
    let output = crate::core::session::admission::admit_tool_value(
        &admitted,
        &format!("Session loaded.\n{summary}"),
    )
    .map_err(|_| "ERROR: session load withheld by current policy; live session unchanged.")?;
    Ok((admitted, output))
}

fn handle_save(session: &mut SessionState) -> String {
    let result = save_session(session);
    if result.is_ok()
        && let Some(root) = session.project_root.as_deref()
    {
        // Queue learnings for Hive, debounced per project (local only).
        crate::core::hive::knowledge::draft::on_session_event(
            root,
            &session.id,
            crate::core::hive::knowledge::draft::Trigger::Save,
        );
    }
    result.unwrap_or_else(|error| format!("Save failed: {error}"))
}

pub(crate) fn save_session(session: &mut SessionState) -> Result<String, String> {
    match session.save() {
        Ok(()) => Ok(format!(
            "Session {} saved (v{}).",
            session.id, session.version
        )),
        Err(error) => Err(error),
    }
}

fn handle_export(
    session: &SessionState,
    value: Option<&str>,
    opts: SessionToolOptions<'_>,
) -> String {
    if session.canonical_checkpoint.is_some() {
        return "ERROR: a canonical checkpoint requires a versioned carrier; legacy bundle export would discard its current lineage.".into();
    }
    {
        let requested_privacy =
            crate::core::ccp_session_bundle::BundlePrivacyV1::parse(opts.privacy);
        if requested_privacy == crate::core::ccp_session_bundle::BundlePrivacyV1::Full
            && crate::core::roles::active_role_name() != "admin"
        {
            return "ERROR: privacy=full requires role 'admin'.".to_string();
        }

        let bundle = crate::core::ccp_session_bundle::build_bundle_v1(session, requested_privacy);
        let json = match crate::core::ccp_session_bundle::serialize_bundle_v1_pretty(&bundle) {
            Ok(s) => s,
            Err(e) => return e,
        };

        let format = opts
            .format
            .unwrap_or(if opts.write { "summary" } else { "json" });
        let root = session.project_root.clone().unwrap_or_else(|| {
            std::env::current_dir()
                .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string())
        });
        let root_path = std::path::PathBuf::from(&root);

        let mut written: Option<String> = None;
        if opts.write || opts.path.is_some() {
            let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
            let candidate = if let Some(p) = opts.path.or(value) {
                let p = std::path::PathBuf::from(p);
                if p.is_absolute() {
                    p
                } else {
                    root_path.join(p)
                }
            } else {
                root_path
                    .join(".lean-ctx")
                    .join("session_bundles")
                    .join(format!(
                        "ccp-session-bundle-v1_{}_{}.json",
                        bundle.session.id, ts
                    ))
            };

            let jailed = match crate::core::io_boundary::jail_and_check_path(
                "ctx_session.export",
                candidate.as_path(),
                root_path.as_path(),
            ) {
                Ok((p, _warning)) => p,
                Err(e) => return e,
            };

            // Read-only-roots choke point (#475): export must not write a
            // bundle into a read-only root even when the jail allows reads.
            if let Err(e) = crate::core::pathjail::enforce_writable(&jailed) {
                return format!("Export write failed: {e}");
            }
            if let Err(e) = crate::core::ccp_session_bundle::write_bundle_v1(&jailed, &json) {
                return format!("Export write failed: {e}");
            }
            written = Some(jailed.to_string_lossy().to_string());
        }

        match format {
            "summary" => {
                let mut out = format!(
                    "CCP session bundle exported (v{}).\n\
schema_version: {}\n\
session_id: {}\n\
bytes: {}\n",
                    bundle.session.version,
                    bundle.schema_version,
                    bundle.session.id,
                    json.len()
                );
                if let Some(p) = written {
                    out.push_str(&format!("path: {p}\n"));
                }
                if let Some(h) = bundle.project.project_root_hash {
                    out.push_str(&format!("project_root_hash: {h}\n"));
                }
                if let Some(h) = bundle.project.project_identity_hash {
                    out.push_str(&format!("project_identity_hash: {h}\n"));
                }
                out
            }
            _ => {
                if let Some(p) = written {
                    format!("{json}\n\npath: {p}")
                } else {
                    json
                }
            }
        }
    }
}

fn handle_import(
    session: &mut SessionState,
    value: Option<&str>,
    opts: SessionToolOptions<'_>,
) -> String {
    {
        let root = session.project_root.clone().unwrap_or_else(|| {
            std::env::current_dir()
                .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string())
        });
        let root_path = std::path::PathBuf::from(&root);

        let Some(p) = opts.path.or(value) else {
            return "ERROR: path is required for action=import".to_string();
        };

        let candidate = {
            let p = std::path::PathBuf::from(p);
            if p.is_absolute() {
                p
            } else {
                root_path.join(p)
            }
        };
        let jailed = match crate::core::io_boundary::jail_and_check_path(
            "ctx_session.import",
            candidate.as_path(),
            root_path.as_path(),
        ) {
            Ok((p, _warning)) => p,
            Err(e) => return e,
        };

        let bundle = match crate::core::ccp_session_bundle::read_bundle_v1(&jailed) {
            Ok(b) => b,
            Err(e) => return format!("Import failed: {e}"),
        };

        // A legacy identity hash is a scope check, not signer admission. Never
        // overwrite a foreign project's session after only displaying a warning.
        let current_root_hash = crate::core::project_hash::hash_project_root(&root);
        let current_identity_hash = crate::core::project_hash::project_identity(&root)
            .as_deref()
            .map(crate::core::hasher::hash_str);

        let matching_scope = match (
            bundle.project.project_identity_hash.as_ref(),
            current_identity_hash.as_ref(),
        ) {
            (Some(exported), Some(current)) => exported == current,
            _ => bundle.project.project_root_hash.as_ref() == Some(&current_root_hash),
        };
        if !matching_scope {
            return "Import failed: project scope mismatch; live session unchanged.".to_string();
        }

        let mut candidate = session.clone();
        let report = crate::core::ccp_session_bundle::import_bundle_v1_into_session(
            &mut candidate,
            &bundle,
            Some(&root),
        );
        // Import into a new identity, preserving both the current session and
        // any existing archive bearing the untrusted source identity.
        if let Err(error) = candidate.save_new() {
            return format!(
                "Import failed: {error}; live session unchanged; recovery candidate: {}.",
                candidate.id
            );
        }
        *session = candidate;

        format!(
            "CCP session bundle imported.\n\
session_id: {}\n\
source_session_id: {}\n\
version: {}\n\
files_touched: {}\n\
stale_files: {}\n",
            report.session_id,
            report.source_session_id,
            report.version,
            report.files_touched,
            report.stale_files
        )
    }
}

fn handle_task(
    session: &mut SessionState,
    _tool_calls: &[(String, u64)],
    value: Option<&str>,
    _opts: SessionToolOptions<'_>,
) -> String {
    {
        let desc = value.unwrap_or("(no description)");
        session.set_task(desc, None);
        format!("Task set: {desc}")
    }
}

fn handle_finding(session: &mut SessionState, value: Option<&str>) -> String {
    {
        let summary = value.unwrap_or("(no summary)");
        let (file, line, text) = parse_finding_value(summary);
        session.add_finding_for_file(file.as_deref(), line, text);
        format!("Finding added: {summary}")
    }
}

fn handle_decision(session: &mut SessionState, value: Option<&str>) -> String {
    {
        let desc = value.unwrap_or("(no description)");
        session.add_decision(desc, None);
        format!("Decision recorded: {desc}")
    }
}

fn handle_reset(session: &mut SessionState) -> String {
    if let Err(error) = session.save() {
        return format!("Reset refused: current session was not saved: {error}");
    }
    reset_after_save(session)
}

pub(crate) fn reset_after_save(session: &mut SessionState) -> String {
    let old_id = session.id.clone();
    *session = SessionState::new();
    // Clear the persistent context ledger so pressure resets to 0%
    let mut ledger = crate::core::context_ledger::ContextLedger::load();
    ledger.reset();
    ledger.save();
    if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
        let radar_path = data_dir.join("context_radar.jsonl");
        let prev = data_dir.join("context_radar.prev.jsonl");
        let _ = std::fs::rename(&radar_path, &prev);
    }
    format!(
        "Session reset. Previous: {old_id}. New: {}. Ledger cleared (0% pressure).",
        session.id
    )
}

fn handle_list(session: &SessionState) -> String {
    handle_scoped_list(session).unwrap_or_else(|_| {
        "ERROR: session list withheld by project scope or content policy.".into()
    })
}

fn handle_scoped_list(session: &SessionState) -> Result<String, String> {
    let text = crate::core::session::admission::with_store_admission(session, |admission| {
        let root = session
            .strict_project_root()
            .ok_or_else(|| "session project scope unavailable".to_string())?;
        let mut rows = Vec::new();
        for summary in SessionState::list_sessions() {
            let Ok(Some(owner)) = SessionState::load_by_id_for_project_root(&summary.id, &root)
            else {
                continue;
            };
            let task = owner
                .task
                .as_ref()
                .map(|task| task.description.as_str())
                .unwrap_or("(no task)");
            let Some(admitted_task) =
                crate::core::session::admission::admit_store_text(admission, task)
            else {
                continue;
            };
            rows.push((summary, admitted_task));
        }
        if rows.is_empty() {
            return Ok("No sessions found.".into());
        }
        let count = rows.len();
        let mut lines = vec![format!("Sessions ({count}):")];
        for (summary, task) in rows.iter().take(10) {
            let task_short: String = task.chars().take(40).collect();
            lines.push(format!(
                "  {} v{} | {} calls | {} tok | {}",
                summary.id, summary.version, summary.tool_calls, summary.tokens_saved, task_short
            ));
        }
        if count > 10 {
            lines.push(format!("  ... +{} more", count - 10));
        }
        Ok(lines.join("\n"))
    })?;
    crate::core::session::admission::admit_tool_value(session, &text)
}

fn handle_cleanup() -> String {
    let days = crate::core::config::Config::load().session_retention_days_effective();
    let removed = SessionState::cleanup_old_sessions(i64::from(days));
    format!("Cleaned up {removed} old session(s) (>{days} days).")
}

fn handle_configure(session: &mut SessionState, opts: SessionToolOptions<'_>) -> String {
    match opts.terse {
        Some(enabled) => {
            session.terse_mode = enabled;
            session.increment();
            format!("Session configured: terse_mode={enabled}")
        }
        None => format!("Session config: terse_mode={}", session.terse_mode),
    }
}

fn handle_snapshot(session: &mut SessionState) -> String {
    match session.save_compaction_snapshot() {
        Ok(snapshot) => {
            format!(
                "Compaction snapshot saved ({} bytes).\n{snapshot}",
                snapshot.len()
            )
        }
        Err(e) => format!("Snapshot failed: {e}"),
    }
}

fn handle_restore(session: &SessionState, session_id: Option<&str>) -> String {
    handle_scoped_restore(session, session_id).unwrap_or_else(|_| {
        "ERROR: session restore withheld by project scope or content policy.".into()
    })
}

fn handle_scoped_restore(
    session: &SessionState,
    session_id: Option<&str>,
) -> Result<String, String> {
    let root = session
        .strict_project_root()
        .ok_or_else(|| "session project scope unavailable".to_string())?;
    // Without an id: the caller's own snapshot, never the project's latest
    // session's (I07/S03).
    let id = session_id.unwrap_or(session.id.as_str());
    let owner = SessionState::load_by_id_for_project_root(id, &root)
        .map_err(|_| "session project scope mismatch".to_string())?;
    let Some(owner) = owner else {
        return Ok("No compaction snapshot found. Session continues fresh.".into());
    };
    let Some(snapshot) = SessionState::load_compaction_snapshot(&owner.id) else {
        return Ok("No compaction snapshot found. Session continues fresh.".into());
    };
    let output = format!("Session restored from compaction snapshot:\n{snapshot}");
    crate::core::session::admission::admit_tool_value(session, &output)
}

fn handle_resume(session: &mut SessionState) -> String {
    session.build_resume_block()
}

fn handle_profile(value: Option<&str>) -> String {
    {
        use crate::core::profiles;
        if let Some(name) = value {
            if let Ok(p) = profiles::set_active_profile(name) {
                format!(
                    "Profile switched to '{name}'.\n\
                         Read mode: {}, CRP: {}",
                    p.read.default_mode_effective(),
                    p.compression.crp_mode_effective(),
                )
            } else {
                let available: Vec<String> = profiles::list_profiles()
                    .iter()
                    .map(|p| p.name.clone())
                    .collect();
                format!(
                    "Profile '{name}' not found. Available: {}",
                    available.join(", ")
                )
            }
        } else {
            let name = profiles::active_profile_name();
            let p = profiles::active_profile();
            let list = profiles::list_profiles();
            let mut out = format!(
                "Active profile: {name}\n\
                     Read: {}, CRP: {}\n\n\
                     Available profiles:",
                p.read.default_mode_effective(),
                p.compression.crp_mode_effective(),
            );
            for info in &list {
                let marker = if info.name == name { " *" } else { "  " };
                out.push_str(&format!(
                    "\n{marker} {:<14} ({}) {}",
                    info.name, info.source, info.description
                ));
            }
            out.push_str("\n\nSwitch: ctx_session action=profile value=<name>");
            out
        }
    }
}

fn handle_role(value: Option<&str>) -> String {
    {
        use crate::core::roles;
        if let Some(name) = value {
            match roles::set_active_role(name) {
                Ok(r) => {
                    format!(
                        "Role switched to '{name}'.\n\
                             Shell: {}\n\
                             Tools: {}",
                        r.role.shell_policy,
                        if r.tools.allowed.iter().any(|a| a == "*") {
                            let denied = if r.tools.denied.is_empty() {
                                "none".to_string()
                            } else {
                                format!("denied: {}", r.tools.denied.join(", "))
                            };
                            format!("* (all), {denied}")
                        } else {
                            r.tools.allowed.join(", ")
                        }
                    )
                }
                Err(e) => {
                    let available: Vec<String> =
                        roles::list_roles().iter().map(|r| r.name.clone()).collect();
                    format!("{e}. Available: {}", available.join(", "))
                }
            }
        } else {
            let name = roles::active_role_name();
            let r = roles::active_role();
            let list = roles::list_roles();
            let mut out = format!(
                "Active role: {name}\n\
                     Description: {}\n\
                     Shell policy: {}\n\n\
                     Available roles:",
                r.role.description, r.role.shell_policy,
            );
            for info in &list {
                let marker = if info.is_active { " *" } else { "  " };
                out.push_str(&format!(
                    "\n{marker} {:<14} ({}) {}",
                    info.name, info.source, info.description
                ));
            }
            out.push_str("\n\nSwitch: ctx_session action=role value=<name>");
            out
        }
    }
}

fn handle_diff(value: Option<&str>) -> String {
    {
        let parts: Vec<&str> = value.unwrap_or("").split_whitespace().collect();
        if parts.len() < 2 {
            return "Usage: ctx_session diff <session_id_a> <session_id_b> [format]\n\
                        Formats: summary (default), json\n\
                        Example: ctx_session diff abc123 def456 json"
                .to_string();
        }
        let id_a = parts[0];
        let id_b = parts[1];
        let format = parts.get(2).copied().unwrap_or("summary");

        // Only sessions of the request's own project are compared; a foreign
        // session reads as absent rather than confirming it exists (AP04-G8).
        let Some(project) = crate::core::policy::runtime::REQUEST_PROJECT
            .try_with(|slot| slot.borrow().clone())
            .ok()
            .flatten()
        else {
            return "ERROR: project_unresolved — session diff needs the request's project."
                .to_string();
        };
        let project = project.to_string_lossy();
        let load = |id: &str| {
            SessionState::load_by_id_for_project_root(id, &project)
                .ok()
                .flatten()
        };

        match (load(id_a), load(id_b)) {
            (Some(a), Some(b)) => {
                let d = crate::core::session_diff::diff_sessions(&a, &b);
                match format {
                    "json" => d.format_json(),
                    _ => d.format_summary(),
                }
            }
            (None, _) => format!("Session not found: {id_a}"),
            (_, None) => format!("Session not found: {id_b}"),
        }
    }
}

fn handle_output_stats() -> String {
    {
        let snap = crate::core::output_verification::stats_snapshot();
        let mut out = snap.format_compact();
        // Agent-output echo summary (#501).
        let echo = crate::core::output_echo::load_stats();
        if !echo.reports.is_empty() {
            out.push_str(&format!(
                "\nOutput echo: {:.0}% avg over last {} replies ({} analyzed total)",
                echo.avg_ratio(50) * 100.0,
                echo.reports.len(),
                echo.total_analyzed
            ));
        }
        out
    }
}

fn handle_verify() -> String {
    {
        let snap = crate::core::output_verification::stats_snapshot();
        format!(
            "DEPRECATION: action=\"verify\" is renamed to action=\"output_stats\" (ctx_verify is the full observability stack).\n{}",
            snap.format_compact()
        )
    }
}

fn handle_episodes(
    session: &mut SessionState,
    tool_calls: &[(String, u64)],
    value: Option<&str>,
    opts: SessionToolOptions<'_>,
) -> String {
    handle_episodes_scoped(session, tool_calls, value, opts).unwrap_or_else(|_| {
        "ERROR: episodic operation withheld by project scope or content policy.".into()
    })
}

fn handle_episodes_scoped(
    session: &SessionState,
    tool_calls: &[(String, u64)],
    value: Option<&str>,
    opts: SessionToolOptions<'_>,
) -> Result<String, String> {
    let (_, hash) = trusted_memory_scope(session)?;
    let policy = crate::core::config::Config::load()
        .memory_policy_effective()
        .map_err(|error| format!("invalid memory policy: {error}"))?;

    match value {
        Some("record") => {
            let id = crate::core::episodic_memory::record_session_episode(
                &hash,
                session,
                tool_calls,
                opts.agent_id,
                &policy.episodic,
                false,
            )?
            .ok_or_else(|| "episode not recorded".to_string())?;
            crate::core::events::emit(crate::core::events::EventKind::KnowledgeUpdate {
                category: "episodic".to_string(),
                key: id.clone(),
                action: "record".to_string(),
            });
            let learned = auto_detect_admitted_procedures(session, &hash, &policy.procedural).ok();
            let output = match learned {
                Some(count) if count > 0 => format!(
                    "Episode recorded: {id} (procedures auto-updated: {count} known workflows)"
                ),
                _ => format!("Episode recorded: {id}"),
            };
            crate::core::session::admission::admit_tool_value(session, &output)
        }
        Some(v) if v.starts_with("search ") => {
            let query = v.trim_start_matches("search ").trim();
            let query =
                crate::core::session::admission::admit_storable_session_text(session, query)?;
            let output =
                crate::core::session::admission::with_store_admission(session, |admission| {
                    let store = crate::core::episodic_memory::EpisodicStore::load_admitted(
                        &hash, admission,
                    )?;
                    let hits = store.search(&query);
                    if hits.is_empty() {
                        return Ok("No episodes matched.".to_string());
                    }
                    let mut out = format!("Episodes matched ({}):", hits.len());
                    for episode in hits.into_iter().take(10) {
                        let task: String = episode.task_description.chars().take(50).collect();
                        out.push_str(&format!(
                            "\n  {} | {} | {} | {}",
                            episode.id,
                            episode.timestamp,
                            episode.outcome.label(),
                            task
                        ));
                    }
                    Ok(out)
                })?;
            crate::core::session::admission::admit_tool_value(session, &output)
        }
        Some(v) if v.starts_with("file ") => {
            let file = v.trim_start_matches("file ").trim();
            let file = crate::core::session::admission::admit_storable_session_text(session, file)?;
            let output =
                crate::core::session::admission::with_store_admission(session, |admission| {
                    let store = crate::core::episodic_memory::EpisodicStore::load_admitted(
                        &hash, admission,
                    )?;
                    let hits = store.by_file(&file);
                    let mut out = format!("Episodes for file match '{file}' ({}):", hits.len());
                    for episode in hits.into_iter().take(10) {
                        let task: String = episode.task_description.chars().take(50).collect();
                        out.push_str(&format!(
                            "\n  {} | {} | {} | {}",
                            episode.id,
                            episode.timestamp,
                            episode.outcome.label(),
                            task
                        ));
                    }
                    Ok(out)
                })?;
            crate::core::session::admission::admit_tool_value(session, &output)
        }
        Some(v) if v.starts_with("outcome ") => {
            let label = v.trim_start_matches("outcome ").trim();
            if !matches!(label, "success" | "failure" | "partial" | "unknown") {
                return Err("unsupported episode outcome".into());
            }
            let output =
                crate::core::session::admission::with_store_admission(session, |admission| {
                    let store = crate::core::episodic_memory::EpisodicStore::load_admitted(
                        &hash, admission,
                    )?;
                    let hits = store.by_outcome(label);
                    let mut out = format!("Episodes outcome '{label}' ({}):", hits.len());
                    for episode in hits.into_iter().take(10) {
                        let task: String = episode.task_description.chars().take(50).collect();
                        out.push_str(&format!(
                            "\n  {} | {} | {}",
                            episode.id, episode.timestamp, task
                        ));
                    }
                    Ok(out)
                })?;
            crate::core::session::admission::admit_tool_value(session, &output)
        }
        _ => {
            let output = crate::core::session::admission::with_store_admission(
                session,
                |admission| {
                    let store = crate::core::episodic_memory::EpisodicStore::load_admitted(
                        &hash, admission,
                    )?;
                    let stats = store.stats();
                    let recent = store.recent(10);
                    let mut out = format!(
                        "Episodic memory: {} episodes, success_rate={:.0}%, tokens_total={}\n\nRecent:",
                        stats.total_episodes,
                        stats.success_rate * 100.0,
                        stats.total_tokens
                    );
                    for episode in recent {
                        let task: String = episode.task_description.chars().take(60).collect();
                        out.push_str(&format!(
                            "\n  {} | {} | {} | {}",
                            episode.id,
                            episode.timestamp,
                            episode.outcome.label(),
                            task
                        ));
                    }
                    out.push_str("\n\nActions: ctx_session action=episodes value=record|\"search <q>\"|\"file <path>\"|\"outcome success|failure|partial|unknown\"");
                    Ok(out)
                },
            )?;
            crate::core::session::admission::admit_tool_value(session, &output)
        }
    }
}

fn trusted_memory_scope(session: &SessionState) -> Result<(String, String), String> {
    let root = session
        .strict_project_root()
        .ok_or_else(|| "session project scope unavailable".to_string())?;
    let hash = crate::core::project_hash::hash_project_root(&root);
    Ok((root, hash))
}

fn auto_detect_admitted_procedures(
    session: &SessionState,
    project_hash: &str,
    policy: &crate::core::memory_policy::ProceduralPolicy,
) -> Result<usize, String> {
    crate::core::session::admission::with_store_admission(session, |admission| {
        let episodes =
            crate::core::episodic_memory::EpisodicStore::load_admitted(project_hash, admission)?;
        if episodes.episodes.is_empty() {
            return Ok(0);
        }
        let (procedures, before) =
            crate::core::procedural_memory::ProceduralStore::mutate_locked_admitted_with_authority(
                project_hash,
                admission,
                |procedures, authority| {
                    let before = procedures.procedures.len();
                    procedures.detect_patterns_admitted(
                        &episodes.episodes,
                        policy,
                        admission,
                        authority,
                    )?;
                    Ok(before)
                },
            )?;
        let learned = procedures.procedures.len().saturating_sub(before);
        if learned > 0 {
            crate::core::events::emit(crate::core::events::EventKind::KnowledgeUpdate {
                category: "procedural".to_string(),
                key: format!("{learned} new"),
                action: "auto_learn".to_string(),
            });
        }
        Ok(learned)
    })
}

fn handle_procedures(session: &mut SessionState, value: Option<&str>) -> String {
    handle_procedures_scoped(session, value).unwrap_or_else(|_| {
        "ERROR: procedural operation withheld by project scope or content policy.".into()
    })
}

fn handle_procedures_scoped(session: &SessionState, value: Option<&str>) -> Result<String, String> {
    let (_, hash) = trusted_memory_scope(session)?;
    let policy = crate::core::config::Config::load()
        .memory_policy_effective()
        .map_err(|error| format!("invalid memory policy: {error}"))?;

    if value == Some("detect") {
        let (procedure_count, episode_count) =
            crate::core::session::admission::with_store_admission(session, |admission| {
                let episodes =
                    crate::core::episodic_memory::EpisodicStore::load_admitted(&hash, admission)?;
                let (procedures, ()) =
                    crate::core::procedural_memory::ProceduralStore::mutate_locked_admitted_with_authority(
                        &hash,
                        admission,
                        |procedures, authority| {
                            procedures.detect_patterns_admitted(
                                &episodes.episodes,
                                &policy.procedural,
                                admission,
                                authority,
                            )
                        },
                    )?;
                Ok((procedures.procedures.len(), episodes.episodes.len()))
            })?;
        crate::core::events::emit(crate::core::events::EventKind::KnowledgeUpdate {
            category: "procedural".to_string(),
            key: hash,
            action: "detect".to_string(),
        });
        let output = format!(
            "Procedures updated. Total procedures: {procedure_count} (episodes: {episode_count})."
        );
        return crate::core::session::admission::admit_tool_value(session, &output);
    }

    if let Some(value) = value.filter(|value| value.starts_with("suggest ")) {
        let task = value.trim_start_matches("suggest ").trim();
        let task = crate::core::session::admission::admit_storable_session_text(session, task)?;
        let output = crate::core::session::admission::with_store_admission(session, |admission| {
            let procedures =
                crate::core::procedural_memory::ProceduralStore::load_admitted(&hash, admission)?;
            let hits = procedures.suggest(&task);
            if hits.is_empty() {
                return Ok("No procedures matched.".to_string());
            }
            let mut out = format!("Procedures suggested ({}):", hits.len());
            for procedure in hits.into_iter().take(10) {
                out.push_str(&format!(
                    "\n  {} | conf={:.0}% | success={:.0}% | steps={}",
                    procedure.name,
                    procedure.confidence * 100.0,
                    procedure.success_rate() * 100.0,
                    procedure.steps.len()
                ));
            }
            Ok(out)
        })?;
        return crate::core::session::admission::admit_tool_value(session, &output);
    }

    let output = crate::core::session::admission::with_store_admission(session, |admission| {
        let projected = crate::core::session::admission::for_storage(session)?;
        let episodes =
            crate::core::episodic_memory::EpisodicStore::load_admitted(&hash, admission)?;
        let procedures =
            crate::core::procedural_memory::ProceduralStore::load_admitted(&hash, admission)?;
        let task = projected
            .task
            .as_ref()
            .map(|task| task.description.clone())
            .unwrap_or_default();
        let suggestions = if task.is_empty() {
            Vec::new()
        } else {
            procedures.suggest(&task)
        };
        let mut out = format!(
            "Procedural memory: {} procedures (episodes: {})",
            procedures.procedures.len(),
            episodes.episodes.len()
        );
        if !task.is_empty() {
            out.push_str(&format!(
                "\nTask: {}",
                task.chars().take(80).collect::<String>()
            ));
            if !suggestions.is_empty() {
                out.push_str("\n\nSuggested:");
                for procedure in suggestions.into_iter().take(5) {
                    out.push_str(&format!(
                        "\n  {} | conf={:.0}% | success={:.0}% | steps={}",
                        procedure.name,
                        procedure.confidence * 100.0,
                        procedure.success_rate() * 100.0,
                        procedure.steps.len()
                    ));
                }
            }
        }
        out.push_str("\n\nActions: ctx_session action=procedures value=detect|\"suggest <task>\"");
        Ok(out)
    })?;
    crate::core::session::admission::admit_tool_value(session, &output)
}

fn handle_unknown(action: &str) -> String {
    format!(
        "Unknown action: {action}. Use: status, handoff, load, save, task, finding, decision, reset, list, cleanup, snapshot, restore, resume, configure, profile, role, diff, output_stats, verify, export, import, episodes, procedures"
    )
}

/// Records an episode only after a completed task has been committed.
fn auto_record_episode(
    session: &SessionState,
    tool_calls: &[(String, u64)],
    agent_id: Option<&str>,
) -> Result<Option<String>, String> {
    let (_, hash) = trusted_memory_scope(session)?;
    let policy = crate::core::config::Config::load()
        .memory_policy_effective()
        .map_err(|error| format!("invalid memory policy: {error}"))?;
    let Some(id) = crate::core::episodic_memory::record_session_episode(
        &hash,
        session,
        tool_calls,
        agent_id,
        &policy.episodic,
        true,
    )?
    else {
        return Ok(None);
    };
    crate::core::events::emit(crate::core::events::EventKind::KnowledgeUpdate {
        category: "episodic".to_string(),
        key: id.clone(),
        action: "auto_record".to_string(),
    });
    let _ = auto_detect_admitted_procedures(session, &hash, &policy.procedural);
    Ok(Some(id))
}

fn parse_finding_value(value: &str) -> (Option<String>, Option<u32>, &str) {
    const EM_DASH_SEP: &str = " \u{2014} ";
    const ASCII_SEP: &str = " - ";

    let (dash_pos, sep) = if let Some(p) = value.find(EM_DASH_SEP) {
        (Some(p), EM_DASH_SEP)
    } else {
        (value.find(ASCII_SEP), ASCII_SEP)
    };

    if let Some(pos) = dash_pos {
        let location = &value[..pos];
        let text = &value[pos + sep.len()..];

        if let Some(colon_pos) = location.rfind(':') {
            let file = &location[..colon_pos];
            if let Ok(line) = location[colon_pos + 1..].parse::<u32>() {
                return (Some(file.to_string()), Some(line), text);
            }
        }
        return (Some(location.to_string()), None, text);
    }
    (None, None, value)
}

#[cfg(test)]
mod tests;
