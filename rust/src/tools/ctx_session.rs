use crate::core::session::SessionState;

pub fn handle(
    session: &mut SessionState,
    action: &str,
    value: Option<&str>,
    session_id: Option<&str>,
) -> String {
    match action {
        "status" => session.format_compact(),

        "load" => {
            let loaded = if let Some(id) = session_id {
                SessionState::load_by_id(id)
            } else {
                SessionState::load_latest()
            };

            if let Some(prev) = loaded {
                let summary = prev.format_compact();
                *session = prev;
                format!("Session loaded.\n{summary}")
            } else {
                let id_str = session_id.unwrap_or("latest");
                format!("No session found (id: {id_str}). Starting fresh.")
            }
        }

        "save" => {
            match session.save() {
                Ok(()) => format!("Session {} saved (v{}).", session.id, session.version),
                Err(e) => format!("Save failed: {e}"),
            }
        }

        "task" => {
            let desc = value.unwrap_or("(no description)");
            session.set_task(desc, None);
            format!("Task set: {desc}")
        }

        "finding" => {
            let summary = value.unwrap_or("(no summary)");
            let (file, line, text) = parse_finding_value(summary);
            session.add_finding(file.as_deref(), line, text);
            format!("Finding added: {summary}")
        }

        "decision" => {
            let desc = value.unwrap_or("(no description)");
            session.add_decision(desc, None);
            format!("Decision recorded: {desc}")
        }

        "reset" => {
            let _ = session.save();
            let old_id = session.id.clone();
            *session = SessionState::new();
            crate::core::budget_tracker::BudgetTracker::global().reset();
            format!("Session reset. Previous: {old_id}. New: {}", session.id)
        }

        "list" => {
            let sessions = SessionState::list_sessions();
            if sessions.is_empty() {
                return "No sessions found.".to_string();
            }
            let mut lines = vec![format!("Sessions ({}):", sessions.len())];
            for s in sessions.iter().take(10) {
                let task = s.task.as_deref().unwrap_or("(no task)");
                let task_short: String = task.chars().take(40).collect();
                lines.push(format!(
                    "  {} v{} | {} calls | {} tok | {}",
                    s.id, s.version, s.tool_calls, s.tokens_saved, task_short
                ));
            }
            if sessions.len() > 10 {
                lines.push(format!("  ... +{} more", sessions.len() - 10));
            }
            lines.join("\n")
        }

        "cleanup" => {
            let removed = SessionState::cleanup_old_sessions(7);
            format!("Cleaned up {removed} old session(s) (>7 days).")
        }

        "snapshot" => match session.save_compaction_snapshot() {
            Ok(snapshot) => {
                format!(
                    "Compaction snapshot saved ({} bytes).\n{snapshot}",
                    snapshot.len()
                )
            }
            Err(e) => format!("Snapshot failed: {e}"),
        },

        "restore" => {
            let snapshot = if let Some(id) = session_id {
                SessionState::load_compaction_snapshot(id)
            } else {
                SessionState::load_latest_snapshot()
            };
            match snapshot {
                Some(s) => format!("Session restored from compaction snapshot:\n{s}"),
                None => "No compaction snapshot found. Session continues fresh.".to_string(),
            }
        }

        "resume" => session.build_resume_block(),

        "profile" => {
            use crate::core::profiles;
            if let Some(name) = value {
                if let Ok(p) = profiles::set_active_profile(name) {
                    format!(
                        "Profile switched to '{name}'.\n\
                         Read mode: {}, Budget: {} tokens, CRP: {}, Density: {}",
                        p.read.default_mode,
                        p.budget.max_context_tokens,
                        p.compression.crp_mode,
                        p.compression.output_density,
                    )
                } else {
                    let available: Vec<String> =
                        profiles::list_profiles().iter().map(|p| p.name.clone()).collect();
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
                     Read: {}, Budget: {} tok, CRP: {}, Density: {}\n\n\
                     Available profiles:",
                    p.read.default_mode,
                    p.budget.max_context_tokens,
                    p.compression.crp_mode,
                    p.compression.output_density,
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

        "budget" => {
            use crate::core::budget_tracker::BudgetTracker;
            let snap = BudgetTracker::global().check();
            snap.format_compact()
        }

        "role" => {
            use crate::core::roles;
            if let Some(name) = value {
                match roles::set_active_role(name) {
                    Ok(r) => {
                        crate::core::budget_tracker::BudgetTracker::global().reset();
                        format!(
                            "Role switched to '{name}'.\n\
                             Shell: {}, Budget: {} tokens / {} shell / ${:.2}\n\
                             Tools: {}",
                            r.role.shell_policy,
                            r.limits.max_context_tokens,
                            r.limits.max_shell_invocations,
                            r.limits.max_cost_usd,
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
                     Shell policy: {}, Budget: {} tokens / {} shell / ${:.2}\n\n\
                     Available roles:",
                    r.role.description,
                    r.role.shell_policy,
                    r.limits.max_context_tokens,
                    r.limits.max_shell_invocations,
                    r.limits.max_cost_usd,
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

        "diff" => {
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

            let sess_a = SessionState::load_by_id(id_a);
            let sess_b = SessionState::load_by_id(id_b);

            match (sess_a, sess_b) {
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

        "slo" => {
            match value {
                Some("reload") => {
                    crate::core::slo::reload();
                    "SLO definitions reloaded from disk.".to_string()
                }
                Some("history") => {
                    let hist = crate::core::slo::violation_history(20);
                    if hist.is_empty() {
                        "No SLO violations recorded.".to_string()
                    } else {
                        let mut out = format!("SLO violations (last {}):\n", hist.len());
                        for v in &hist {
                            out.push_str(&format!(
                                "  {} {} ({}) {:.2} vs {:.2} → {}\n",
                                v.timestamp, v.slo_name, v.metric, v.actual, v.threshold, v.action
                            ));
                        }
                        out
                    }
                }
                Some("clear") => {
                    crate::core::slo::clear_violations();
                    "SLO violation history cleared.".to_string()
                }
                _ => {
                    let snap = crate::core::slo::evaluate_quiet();
                    snap.format_compact()
                }
            }
        }

        _ => format!("Unknown action: {action}. Use: status, load, save, task, finding, decision, reset, list, cleanup, snapshot, restore, resume, profile, role, budget, slo, diff"),
    }
}

fn parse_finding_value(value: &str) -> (Option<String>, Option<u32>, &str) {
    // Format: "file.rs:42 — summary text" or just "summary text"
    if let Some(dash_pos) = value.find(" \u{2014} ").or_else(|| value.find(" - ")) {
        let location = &value[..dash_pos];
        let sep_len = 3;
        let text = &value[dash_pos + sep_len..];

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
