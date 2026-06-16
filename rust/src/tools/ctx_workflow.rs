use crate::core::session::SessionState;
use crate::core::workflow::{self, WorkflowRun, WorkflowSpec};
use chrono::Utc;
use serde_json::Value;

pub fn handle_with_session(
    args: Option<&serde_json::Map<String, Value>>,
    session: &mut SessionState,
) -> String {
    handle_with_session_agent(args, session, None)
}

pub fn handle_with_session_agent(
    args: Option<&serde_json::Map<String, Value>>,
    session: &mut SessionState,
    agent_id: Option<&str>,
) -> String {
    let action = get_str(args, "action").unwrap_or_else(|| "status".to_string());

    match action.as_str() {
        "start" => handle_start(args, agent_id),
        "status" => handle_status(session, agent_id),
        "stop" => handle_stop(agent_id),
        "transition" => handle_transition(args, session, agent_id),
        "complete" => handle_complete(args, session, agent_id),
        "evidence_add" => handle_evidence_add(args, session, agent_id),
        "evidence_list" => handle_evidence_list(session, agent_id),
        _ => "Unknown action. Use: start, status, transition, complete, evidence_add, evidence_list, stop".to_string(),
    }
}

fn handle_start(args: Option<&serde_json::Map<String, Value>>, agent_id: Option<&str>) -> String {
    let spec_json = get_str(args, "spec");
    let name_override = get_str(args, "name");

    let mut spec: WorkflowSpec = match spec_json.as_deref() {
        Some(s) if !s.trim().is_empty() => match serde_json::from_str::<WorkflowSpec>(s) {
            Ok(v) => v,
            Err(e) => return format!("Invalid spec JSON: {e}"),
        },
        _ => WorkflowSpec::builtin_plan_code_test(),
    };

    if let Some(name) = name_override
        && !name.trim().is_empty()
    {
        spec.name = name;
    }

    if let Err(e) = workflow::validate_spec(&spec) {
        return format!("Invalid WorkflowSpec: {e}");
    }

    let run = WorkflowRun::new(spec);
    if let Err(e) = workflow::save_active_for_agent(&run, agent_id) {
        return format!("Failed to save workflow: {e}");
    }

    format!(
        "Workflow started: {}\n  State: {}\n  Started: {}",
        run.spec.name, run.current, run.started_at
    )
}

fn handle_status(session: &SessionState, agent_id: Option<&str>) -> String {
    let Ok(active) = workflow::load_active_for_agent(agent_id) else {
        return "Error: failed to load active workflow.".to_string();
    };
    let Some(run) = active else {
        return "No active workflow. Use action=start to begin.".to_string();
    };

    let ledger = crate::core::evidence_ledger::EvidenceLedgerV1::load();

    let elapsed_min = chrono::Utc::now()
        .signed_duration_since(run.updated_at)
        .num_minutes();
    let mut lines = vec![
        format!("Workflow: {}", run.spec.name),
        format!("  State: {}", run.current),
        format!("  Updated: {} ({elapsed_min}m ago)", run.updated_at),
    ];
    if elapsed_min > 20 {
        lines.push("  WARNING: Workflow inactive >20min, will auto-expire at 30min. Use action=stop to exit now.".to_string());
    }

    if let Some(state) = run.spec.state(&run.current)
        && let Some(ref tools) = state.allowed_tools
    {
        let mut tools = tools.clone();
        tools.sort();
        let tools = tools.into_iter().take(30).collect::<Vec<_>>();
        lines.push(format!(
            "  Allowed tools ({} shown): {}",
            tools.len(),
            tools.join(", ")
        ));
    }

    let transitions = workflow::allowed_transitions(&run.spec, &run.current);
    if transitions.is_empty() {
        lines.push("  Transitions: (none)".to_string());
    } else {
        lines.push("  Transitions:".to_string());
        for t in transitions.iter().take(10) {
            let missing = workflow::missing_evidence_for_state(&run.spec, &t.to, |k| {
                run.evidence.iter().any(|e| e.key == k)
                    || session.has_evidence_key(k)
                    || ledger.has_key(k)
            });
            if missing.is_empty() {
                lines.push(format!("    → {} (ok)", t.to));
            } else {
                lines.push(format!("    → {} (missing: {})", t.to, missing.join(", ")));
            }
        }
    }

    lines.join("\n")
}

fn handle_stop(agent_id: Option<&str>) -> String {
    match workflow::clear_active_for_agent(agent_id) {
        Ok(()) => "Workflow stopped (active cleared).".to_string(),
        Err(e) => format!("Error clearing workflow: {e}"),
    }
}

fn handle_transition(
    args: Option<&serde_json::Map<String, Value>>,
    session: &SessionState,
    agent_id: Option<&str>,
) -> String {
    let Some(to) = get_str(args, "to") else {
        return "Error: 'to' is required for transition".to_string();
    };
    let note = get_str(args, "value");

    let Ok(active) = workflow::load_active_for_agent(agent_id) else {
        return "Error: failed to load active workflow.".to_string();
    };
    let Some(mut run) = active else {
        return "No active workflow. Use action=start to begin.".to_string();
    };

    let ledger = crate::core::evidence_ledger::EvidenceLedgerV1::load();
    if let Err(e) = workflow::can_transition(&run.spec, &run.current, &to, |k| {
        run.evidence.iter().any(|e| e.key == k) || session.has_evidence_key(k) || ledger.has_key(k)
    }) {
        return format!("Transition blocked: {e}");
    }

    let from = run.current.clone();
    run.current.clone_from(&to);
    run.updated_at = Utc::now();
    run.transitions
        .push(crate::core::workflow::TransitionRecord {
            from: from.clone(),
            to: to.clone(),
            note: note.clone(),
            timestamp: Utc::now(),
        });

    if let Err(e) = workflow::save_active_for_agent(&run, agent_id) {
        return format!("Failed to save workflow: {e}");
    }

    format!("Transition: {from} → {to}")
}

fn handle_complete(
    args: Option<&serde_json::Map<String, Value>>,
    session: &SessionState,
    agent_id: Option<&str>,
) -> String {
    let Ok(active) = workflow::load_active_for_agent(agent_id) else {
        return "Error: failed to load active workflow.".to_string();
    };
    let Some(mut run) = active else {
        return "No active workflow. Use action=start to begin.".to_string();
    };
    let note = get_str(args, "value");

    let done = "done".to_string();
    if workflow::find_transition(&run.spec, &run.current, &done).is_none() {
        return format!("No transition to 'done' from '{}'", run.current);
    }

    let ledger = crate::core::evidence_ledger::EvidenceLedgerV1::load();
    if let Err(e) = workflow::can_transition(&run.spec, &run.current, &done, |k| {
        run.evidence.iter().any(|e| e.key == k) || session.has_evidence_key(k) || ledger.has_key(k)
    }) {
        return format!("Complete blocked: {e}");
    }

    let from = run.current.clone();
    run.current.clone_from(&done);
    run.updated_at = Utc::now();
    run.transitions
        .push(crate::core::workflow::TransitionRecord {
            from: from.clone(),
            to: done.clone(),
            note,
            timestamp: Utc::now(),
        });

    if let Err(e) = workflow::clear_active_for_agent(agent_id) {
        return format!("Workflow completed but failed to clear: {e}");
    }

    format!("Workflow completed: {from} → done (workflow cleared)")
}

fn handle_evidence_add(
    args: Option<&serde_json::Map<String, Value>>,
    session: &mut SessionState,
    agent_id: Option<&str>,
) -> String {
    let Some(key) = get_str(args, "key") else {
        return "Error: key is required".to_string();
    };
    let value = get_str(args, "value");

    let Ok(active) = workflow::load_active_for_agent(agent_id) else {
        return "Error: failed to load active workflow.".to_string();
    };
    let Some(mut run) = active else {
        return "No active workflow. Use action=start to begin.".to_string();
    };

    run.add_manual_evidence(&key, value.as_deref());
    session.record_manual_evidence(&key, value.as_deref());
    {
        let mut ledger = crate::core::evidence_ledger::EvidenceLedgerV1::load();
        ledger.record_manual(&key, value.as_deref(), chrono::Utc::now());
        let _ = ledger.save();
    }

    if let Err(e) = workflow::save_active_for_agent(&run, agent_id) {
        return format!("Failed to save workflow: {e}");
    }

    format!("Evidence added: {key}")
}

fn handle_evidence_list(session: &SessionState, agent_id: Option<&str>) -> String {
    let Ok(active) = workflow::load_active_for_agent(agent_id) else {
        return "Error: failed to load active workflow.".to_string();
    };
    let Some(run) = active else {
        return "No active workflow.".to_string();
    };

    let ledger = crate::core::evidence_ledger::EvidenceLedgerV1::load();
    let mut lines = vec![format!("Evidence (workflow: {}):", run.spec.name)];
    if run.evidence.is_empty() && session.evidence.is_empty() && ledger.items.is_empty() {
        lines.push("  (none)".to_string());
        return lines.join("\n");
    }

    if !run.evidence.is_empty() {
        lines.push("  Manual (workflow):".to_string());
        for e in run.evidence.iter().rev().take(20) {
            let v = e.value.as_deref().unwrap_or("-");
            lines.push(format!("    {} = {} ({})", e.key, v, e.timestamp));
        }
    }

    if !session.evidence.is_empty() {
        lines.push("  Session receipts (latest):".to_string());
        for e in session.evidence.iter().rev().take(20) {
            lines.push(format!("    {} ({:?})", e.key, e.kind));
        }
    }

    if !ledger.items.is_empty() {
        lines.push("  Ledger (latest):".to_string());
        for e in ledger.items.iter().rev().take(20) {
            lines.push(format!("    {} ({:?})", e.key, e.kind));
        }
    }

    lines.join("\n")
}

fn get_str(args: Option<&serde_json::Map<String, Value>>, key: &str) -> Option<String> {
    args?
        .get(key)?
        .as_str()
        .map(std::string::ToString::to_string)
}
