//! `lean-ctx agent` — first-class agent identities (GL #433).
//!
//! Subcommands: register, list, show, heartbeat, ack-drift, suspend, resume,
//! decommission, offboard-owner, check.

use crate::core::agent_registry::{self, AgentStatus};

pub(crate) fn cmd_agent(args: &[String]) {
    let flag = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|pos| args.get(pos + 1).cloned())
    };
    let positional = |idx: usize| -> Option<String> {
        args.iter()
            .skip(1)
            .filter(|a| !a.starts_with("--"))
            .nth(idx)
            .cloned()
    };
    let as_json = args.iter().any(|a| a == "--json");

    match args.first().map(String::as_str) {
        Some("register") => {
            let (Some(agent_id), Some(role), Some(owner)) =
                (flag("--id"), flag("--role"), flag("--owner"))
            else {
                exit_usage("agent register --id <agent-id> --role <role> --owner <user@org>");
            };
            match agent_registry::register(&agent_id, &role, &owner) {
                Ok(record) => {
                    println!(
                        "registered: {} (role {}, owner {})",
                        record.agent_id, record.role, record.owner
                    );
                    println!("public key: {}", record.public_key);
                    if let Some(att) = &record.attestation {
                        println!(
                            "attested: binary {}…, config {}",
                            &att.binary_sha256[..16.min(att.binary_sha256.len())],
                            if att.config_sha256.is_empty() {
                                "(built-in role)"
                            } else {
                                &att.config_sha256[..16]
                            }
                        );
                    }
                    if let Some(domain) = flag("--trust-domain") {
                        println!("spiffe id: {}", agent_registry::spiffe_id(&record, &domain));
                    }
                }
                Err(e) => exit_err(&e),
            }
        }
        Some("list") => {
            let records = if args.iter().any(|arg| arg == "--all") {
                agent_registry::try_list()
            } else {
                agent_registry::try_list_active()
            }
            .unwrap_or_else(|error| exit_err(&error));
            if as_json {
                print_json_or_exit(&records);
                return;
            }
            if records.is_empty() {
                println!(
                    "no registered agents — start with:\n  lean-ctx agent register --id <agent-id> --role developer --owner you@org"
                );
                return;
            }
            println!(
                "{:<24} {:<12} {:<22} {:<14} heartbeat",
                "AGENT", "ROLE", "OWNER", "STATUS"
            );
            for r in records {
                let status = match r.status {
                    AgentStatus::Active => "active".to_string(),
                    AgentStatus::Suspended => "SUSPENDED".to_string(),
                    AgentStatus::Decommissioned => "decommissioned".to_string(),
                };
                println!(
                    "{:<24} {:<12} {:<22} {:<14} {}",
                    r.agent_id,
                    r.role,
                    r.owner,
                    status,
                    r.last_heartbeat.as_deref().unwrap_or("-")
                );
            }
        }
        Some("presence") => print_local_presence(as_json, args.iter().any(|arg| arg == "--all")),
        Some("show") => {
            let Some(agent_id) = positional(0) else {
                exit_usage("agent show <agent-id> [--trust-domain org.example]");
            };
            match agent_registry::try_get(&agent_id) {
                Ok(Some(record)) => {
                    if as_json {
                        print_json_or_exit(&record);
                    } else {
                        print_json_or_exit(&record);
                        if let Some(domain) = flag("--trust-domain") {
                            println!("spiffe id: {}", agent_registry::spiffe_id(&record, &domain));
                        }
                    }
                }
                Ok(None) => exit_err(&format!("agent '{agent_id}' is not registered")),
                Err(error) => exit_err(&error),
            }
        }
        Some("heartbeat") => {
            let Some(agent_id) = positional(0) else {
                exit_usage("agent heartbeat <agent-id>");
            };
            match agent_registry::heartbeat(&agent_id) {
                Ok(outcome) => {
                    let (message, code) = heartbeat_report(&agent_id, &outcome);
                    println!("{message}");
                    if code != 0 {
                        std::process::exit(code);
                    }
                }
                Err(e) => exit_err(&e),
            }
        }
        Some("ack-drift") => {
            let (Some(agent_id), Some(evidence)) = (positional(0), flag("--evidence")) else {
                exit_usage(
                    "agent ack-drift <agent-id> --evidence \"<evidence exactly as reported>\"",
                );
            };
            match agent_registry::acknowledge_drift(&agent_id, &evidence) {
                Ok(ack) => println!(
                    "drift acknowledged on {}: {} (detected {}, acknowledged {})",
                    ack.agent_id, ack.evidence, ack.detected_at, ack.acknowledged_at
                ),
                Err(e) => exit_err(&e),
            }
        }
        Some("suspend") => {
            let Some(agent_id) = positional(0) else {
                exit_usage("agent suspend <agent-id> [--reason <text>]");
            };
            let reason = flag("--reason").unwrap_or_else(|| "manual suspend".to_string());
            match agent_registry::suspend(&agent_id, &reason) {
                Ok(()) => println!("suspended: {agent_id} ({reason})"),
                Err(e) => exit_err(&e),
            }
        }
        Some("resume") => {
            let Some(agent_id) = positional(0) else {
                exit_usage("agent resume <agent-id>");
            };
            match agent_registry::resume(&agent_id) {
                Ok(()) => println!("resumed: {agent_id}"),
                Err(e) => exit_err(&e),
            }
        }
        Some("decommission") => {
            let Some(agent_id) = positional(0) else {
                exit_usage("agent decommission <agent-id>");
            };
            match agent_registry::decommission(&agent_id) {
                Ok(()) => println!("decommissioned: {agent_id} (final, audit-closed)"),
                Err(e) => exit_err(&e),
            }
        }
        Some("offboard-owner") => {
            let Some(owner) = positional(0) else {
                exit_usage("agent offboard-owner <user@org> [--reason <text>]");
            };
            let reason = flag("--reason").unwrap_or_else(|| "owner offboarded".to_string());
            match agent_registry::suspend_agents_for_owner(&owner, &reason) {
                Ok(ids) if ids.is_empty() => println!("no active agents owned by {owner}"),
                Ok(ids) => println!("suspended {} agent(s): {}", ids.len(), ids.join(", ")),
                Err(e) => exit_err(&e),
            }
        }
        Some("check") => {
            let Some(agent_id) = positional(0) else {
                exit_usage("agent check <agent-id>");
            };
            let result = agent_registry::check(&agent_id);
            if as_json {
                print_json_or_exit(&result);
            } else {
                println!(
                    "{}: {} — {}",
                    result.agent_id,
                    if result.allowed { "ALLOWED" } else { "DENIED" },
                    result.detail
                );
            }
            if !result.allowed {
                std::process::exit(1);
            }
        }
        _ => {
            println!(
                "lean-ctx agent — first-class agent identities (registered, attested, revocable)\n\n\
USAGE:\n\
  lean-ctx agent register --id <agent-id> --role <role> --owner <user@org>\n\
  lean-ctx agent list [--json] [--all]      list active identities (--all: lifecycle history)\n\
  lean-ctx agent presence [--json] [--all]  inspect local MCP agent liveness\n\
  lean-ctx agent show <agent-id> [--trust-domain org.example]\n\
  lean-ctx agent heartbeat <agent-id>        liveness + attestation drift check (exit 3 = unacknowledged drift)\n\
  lean-ctx agent ack-drift <agent-id> --evidence \"<evidence>\"  acknowledge exactly the reported drift\n\
  lean-ctx agent suspend <agent-id> [--reason <text>]\n\
  lean-ctx agent resume <agent-id>           lifecycle only — does NOT acknowledge drift\n\
  lean-ctx agent decommission <agent-id>     final — writes the audit-closing entry\n\
  lean-ctx agent offboard-owner <user@org>   suspend all agents of an owner (SCIM hook)\n\
  lean-ctx agent check <agent-id>            enforce-path identity check (exit 1 = deny)\n\n\
Every identity has a mandatory human owner. Lifecycle transitions write\n\
tamper-evident audit entries. Docs: docs/enterprise/agent-identity.md"
            );
        }
    }
}

/// Runtime presence is intentionally separate from `agent list`: the latter
/// is an identity/authorization registry, while this view answers the
/// operational question "which local agents are actually connected and doing
/// work?". A live but quiet MCP process is `idle`, not falsely `active`.
#[derive(serde::Serialize)]
struct LocalPresenceRow {
    id: String,
    agent_type: String,
    role: Option<String>,
    state: &'static str,
    alive: bool,
    last_active: chrono::DateTime<chrono::Utc>,
    age_seconds: i64,
    pid: u32,
    project_root: String,
    status_message: Option<String>,
}

fn local_presence_snapshot(
    registry: &crate::core::agents::AgentRegistry,
    now: chrono::DateTime<chrono::Utc>,
    include_finished: bool,
    compatibility_identities: &crate::core::agents::ProcessIdentityIndex,
) -> Vec<LocalPresenceRow> {
    const ACTIVE_WINDOW_SECONDS: i64 = 60;

    let mut rows: Vec<_> = registry
        .agents
        .iter()
        .filter_map(|agent| {
            let alive = agent
                .process_identity
                .as_ref()
                .or_else(|| compatibility_identities.get(&agent.agent_id))
                .is_some_and(|identity| crate::ipc::process::matches_identity(agent.pid, identity));
            let age_seconds = (now - agent.last_active).num_seconds().max(0);
            let state = if !alive || agent.status == crate::core::agents::AgentStatus::Finished {
                "finished"
            } else if age_seconds <= ACTIVE_WINDOW_SECONDS {
                "active"
            } else {
                "idle"
            };
            (include_finished || state != "finished").then(|| LocalPresenceRow {
                id: agent.agent_id.clone(),
                agent_type: agent.agent_type.clone(),
                role: agent.role.clone(),
                state,
                alive,
                last_active: agent.last_active,
                age_seconds,
                pid: agent.pid,
                project_root: agent.project_root.clone(),
                status_message: agent.status_message.clone(),
            })
        })
        .collect();
    rows.sort_by(|left, right| {
        left.age_seconds
            .cmp(&right.age_seconds)
            .then_with(|| left.id.cmp(&right.id))
    });
    rows
}

fn print_local_presence(as_json: bool, include_finished: bool) {
    const ACTIVE_WINDOW_SECONDS: i64 = 60;

    let registry = crate::core::agents::AgentRegistry::mutate_locked(|registry| {
        registry.cleanup_stale(
            crate::core::config::Config::load()
                .agents
                .presence_ttl_hours,
        );
    })
    .map(|(registry, ())| registry)
    .unwrap_or_else(|_| crate::core::agents::AgentRegistry::load_or_create());
    let compatibility_identities = crate::core::agents::ProcessIdentityIndex::load();
    let rows = local_presence_snapshot(
        &registry,
        chrono::Utc::now(),
        include_finished,
        &compatibility_identities,
    );

    if as_json {
        print_json_or_exit(&serde_json::json!({
            "active_window_seconds": ACTIVE_WINDOW_SECONDS,
            "presence": rows,
        }));
        return;
    }

    if rows.is_empty() {
        println!("no local MCP agent presence detected");
        return;
    }

    println!(
        "{:<24} {:<14} {:<16} {:<10} {:>8}  PROJECT",
        "AGENT", "TYPE", "ROLE", "STATE", "AGE"
    );
    for row in rows {
        println!(
            "{:<24} {:<14} {:<16} {:<10} {:>7}s  {}",
            row.id,
            row.agent_type,
            row.role.as_deref().unwrap_or("-"),
            row.state,
            row.age_seconds,
            row.project_root,
        );
    }
}

/// Operator-facing rendering of one heartbeat, split from the I/O so the
/// exit-code contract is testable.
///
/// Exit 3 is the documented drift signal and fires on EVERY beat while the
/// drift is unacknowledged — not only on the beat that observed it. A monitor
/// that flipped back to green one minute after the incident, on an identity
/// `agent check` still denies, is the failure this shape rules out. "new" vs
/// "still unacknowledged" keeps the two states distinguishable for an
/// operator reading a log.
fn heartbeat_report(agent_id: &str, outcome: &agent_registry::HeartbeatOutcome) -> (String, i32) {
    let Some(mark) = outcome.drift.as_ref() else {
        return ("heartbeat recorded, attestation unchanged".to_string(), 0);
    };
    let state = if outcome.newly_observed {
        "new".to_string()
    } else {
        format!("still unacknowledged since {}", mark.detected_at)
    };
    (
        format!(
            "heartbeat recorded — ATTESTATION DRIFT ({state}): {}\n\
             acknowledge with: lean-ctx agent ack-drift {agent_id} --evidence \"{}\"",
            mark.evidence, mark.evidence
        ),
        3,
    )
}

fn exit_usage(usage: &str) -> ! {
    eprintln!("usage: lean-ctx {usage}");
    std::process::exit(2);
}

fn print_json_or_exit<T: serde::Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(json) => println!("{json}"),
        Err(error) => exit_err(&format!("cannot serialize JSON output: {error}")),
    }
}

fn exit_err(message: &str) -> ! {
    eprintln!("agent: {message}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::{heartbeat_report, local_presence_snapshot};
    use crate::core::agent_registry::{DriftMark, HeartbeatOutcome};
    use crate::core::agents::{AgentEntry, AgentRegistry, AgentStatus};

    fn mark() -> DriftMark {
        DriftMark {
            detected_at: "2026-09-06T20:00:00Z".to_string(),
            binary: true,
            config: false,
            evidence: "attestation drift dimensions=binary binary=aaaaaaaaaaaa->bbbbbbbbbbbb"
                .to_string(),
        }
    }

    /// B2: the drift signal is the identity's *state*, not the transition.
    /// Beat 2 must still exit 3 while `agent check` still reports drift.
    #[test]
    fn heartbeat_exit_code_stays_3_while_drift_is_unacknowledged() {
        let clean = HeartbeatOutcome {
            drift: None,
            newly_observed: false,
        };
        assert_eq!(heartbeat_report("a1", &clean).1, 0);

        let observing = HeartbeatOutcome {
            drift: Some(mark()),
            newly_observed: true,
        };
        let (new_message, new_code) = heartbeat_report("a1", &observing);
        assert_eq!(new_code, 3);
        assert!(new_message.contains("DRIFT (new)"), "{new_message}");

        let sticky = HeartbeatOutcome {
            drift: Some(mark()),
            newly_observed: false,
        };
        let (sticky_message, sticky_code) = heartbeat_report("a1", &sticky);
        assert_eq!(
            sticky_code, 3,
            "beat 2 must keep signalling: {sticky_message}"
        );
        assert!(
            sticky_message.contains("still unacknowledged since 2026-09-06T20:00:00Z"),
            "{sticky_message}"
        );
        assert_ne!(
            new_message, sticky_message,
            "new and still-unacknowledged must be distinguishable"
        );
        // Both point at the bound acknowledgement, never at plain `resume`.
        for message in [&new_message, &sticky_message] {
            assert!(message.contains("ack-drift a1 --evidence"), "{message}");
            assert!(!message.contains("agent resume"), "{message}");
        }
    }

    #[test]
    fn presence_snapshot_classifies_and_hides_finished_agents() {
        let now = Utc::now();
        let pid = std::process::id();
        let process_identity = crate::ipc::process::identity(pid)
            .expect("current process must have immutable identity");
        let registry = AgentRegistry {
            agents: vec![
                AgentEntry {
                    agent_id: "active".to_string(),
                    agent_type: "mcp".to_string(),
                    role: Some("coder".to_string()),
                    project_root: "/project".to_string(),
                    started_at: now,
                    last_active: now,
                    pid,
                    process_identity: Some(process_identity.clone()),
                    durable_identity_id: None,
                    status: AgentStatus::Active,
                    status_message: None,
                },
                AgentEntry {
                    agent_id: "idle".to_string(),
                    agent_type: "mcp".to_string(),
                    role: None,
                    project_root: "/project".to_string(),
                    started_at: now,
                    last_active: now - Duration::seconds(61),
                    pid,
                    process_identity: Some(process_identity.clone()),
                    durable_identity_id: None,
                    status: AgentStatus::Active,
                    status_message: None,
                },
                AgentEntry {
                    agent_id: "finished".to_string(),
                    agent_type: "mcp".to_string(),
                    role: None,
                    project_root: "/project".to_string(),
                    started_at: now,
                    last_active: now,
                    pid,
                    process_identity: Some(process_identity),
                    durable_identity_id: None,
                    status: AgentStatus::Finished,
                    status_message: None,
                },
            ],
            scratchpad: vec![],
            logical_sessions: vec![],
            logical_session_telemetry_seen: false,
            updated_at: now,
        };

        let visible = local_presence_snapshot(
            &registry,
            now,
            false,
            &crate::core::agents::ProcessIdentityIndex::default(),
        );
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].id, "active");
        assert_eq!(visible[0].state, "active");
        assert_eq!(visible[1].id, "idle");
        assert_eq!(visible[1].state, "idle");

        let all = local_presence_snapshot(
            &registry,
            now,
            true,
            &crate::core::agents::ProcessIdentityIndex::default(),
        );
        assert_eq!(all.len(), 3);
        assert!(all.iter().any(|row| row.state == "finished"));
    }
}
