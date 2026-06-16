//! `lean-ctx agent` — first-class agent identities (GL #433).
//!
//! Subcommands: register, list, show, heartbeat, suspend, resume,
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
            let records = agent_registry::list();
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&records).expect("serializable")
                );
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
        Some("show") => {
            let Some(agent_id) = positional(0) else {
                exit_usage("agent show <agent-id> [--trust-domain org.example]");
            };
            match agent_registry::get(&agent_id) {
                Some(record) => {
                    if as_json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&record).expect("serializable")
                        );
                    } else {
                        println!("{}", serde_json::to_string_pretty(&record).expect("ok"));
                        if let Some(domain) = flag("--trust-domain") {
                            println!("spiffe id: {}", agent_registry::spiffe_id(&record, &domain));
                        }
                    }
                }
                None => exit_err(&format!("agent '{agent_id}' is not registered")),
            }
        }
        Some("heartbeat") => {
            let Some(agent_id) = positional(0) else {
                exit_usage("agent heartbeat <agent-id>");
            };
            match agent_registry::heartbeat(&agent_id) {
                Ok(None) => println!("heartbeat recorded, attestation unchanged"),
                Ok(Some(drift)) => {
                    println!("heartbeat recorded — ATTESTATION DRIFT: {drift}");
                    std::process::exit(3);
                }
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
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).expect("serializable")
                );
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
  lean-ctx agent list [--json]\n\
  lean-ctx agent show <agent-id> [--trust-domain org.example]\n\
  lean-ctx agent heartbeat <agent-id>        liveness + attestation drift check\n\
  lean-ctx agent suspend <agent-id> [--reason <text>]\n\
  lean-ctx agent resume <agent-id>\n\
  lean-ctx agent decommission <agent-id>     final — writes the audit-closing entry\n\
  lean-ctx agent offboard-owner <user@org>   suspend all agents of an owner (SCIM hook)\n\
  lean-ctx agent check <agent-id>            enforce-path identity check (exit 1 = deny)\n\n\
Every identity has a mandatory human owner. Lifecycle transitions write\n\
tamper-evident audit entries. Docs: docs/enterprise/agent-identity.md"
            );
        }
    }
}

fn exit_usage(usage: &str) -> ! {
    eprintln!("usage: lean-ctx {usage}");
    std::process::exit(2);
}

fn exit_err(message: &str) -> ! {
    eprintln!("agent: {message}");
    std::process::exit(1);
}
