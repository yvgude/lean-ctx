//! `lean-ctx policy enforce <tool>` — server-free policy enforcement evaluator.
//!
//! Runs the **same guard sequence** as the live MCP agent path
//! ([`crate::server::call_tool`]'s `call_tool_guarded`) using the **same guard
//! functions** ([`crate::server::role_guard`], [`crate::server::policy_guard`],
//! [`crate::core::egress`], [`crate::core::input_filters`]) and records the
//! **same audit-trail entries** — but **without starting the MCP server**.
//!
//! Why this exists:
//! * CISO policy authoring/testing: prove what a pack denies, redacts and blocks
//!   before rolling it out, from the CLI or CI.
//! * Auditable enforcement evidence: every gate decision is appended to the
//!   tamper-evident audit trail, exported by `lean-ctx audit evidence`.
//!
//! It honors the policy resolved by [`crate::core::policy::runtime`] — the local
//! project pack (`.lean-ctx/policy.toml`) folded under a trusted, signed org
//! policy floor (GL #674) — so the verdict reflects exactly what the agent would
//! experience on this endpoint.
//!
//! The order below mirrors the server chokepoint and MUST stay in sync with it;
//! the security *rules* live once in the guard modules (no duplication here, only
//! the orchestration), and `tests` pins the deny/egress/filter behavior.

use serde_json::{Map, Value};

use crate::core::policy::runtime;
use crate::server::{policy_guard, role_guard};

/// Structured result of a single guarded evaluation.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EnforceOutcome {
    /// Blocked by the active role policy (recorded as `ToolDenied`).
    DeniedRole(String),
    /// Blocked by the active context policy pack's allow/deny lists (`ToolDenied`).
    DeniedPolicy(String),
    /// Write/action blocked by the pack's `[egress]` DLP (`ToolDenied`).
    BlockedEgress(String),
    /// Tool output withheld by an input `[filters]` `block` action (`ToolDenied`).
    BlockedFilter(String),
    /// Allowed; output passed through redaction + filters.
    Allowed {
        redactions: usize,
        filtered: Vec<(String, usize)>,
    },
    /// The tool dispatched but its handler failed (not a policy decision).
    DispatchError(String),
}

impl EnforceOutcome {
    /// Single-line status used by both the text renderer and `--json`.
    fn status(&self) -> &'static str {
        match self {
            EnforceOutcome::DeniedRole(_) => "denied-role",
            EnforceOutcome::DeniedPolicy(_) => "denied-policy",
            EnforceOutcome::BlockedEgress(_) => "blocked-egress",
            EnforceOutcome::BlockedFilter(_) => "blocked-filter",
            EnforceOutcome::Allowed { .. } => "allowed",
            EnforceOutcome::DispatchError(_) => "dispatch-error",
        }
    }
}

/// A failed call whose message is the policy's own refusal: the read path applies
/// the same block-before-mask decision as step 5 before any text exists, so that
/// refusal is the policy's verdict (`blocked-filter`), not a failed handler.
fn dispatch_failure(message: String) -> EnforceOutcome {
    if message.contains("withheld by policy") {
        EnforceOutcome::BlockedFilter(message)
    } else {
        EnforceOutcome::DispatchError(message)
    }
}

struct EnforceArgs {
    tool: String,
    project_root: String,
    json: String,
    as_json: bool,
}

pub(crate) fn cmd_enforce(args: &[String]) {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("policy enforce: {msg}\n");
            print_help();
            std::process::exit(2);
        }
    };

    let map = match parse_json_object(&parsed.json) {
        Ok(m) => m,
        Err(msg) => {
            eprintln!("policy enforce: {msg}");
            std::process::exit(2);
        }
    };

    let outcome = run_enforce(&parsed.tool, &parsed.project_root, &parsed.json, &map);
    // The verdict is the ONE output of this command. The evaluated tool's
    // text is never written (it is no delivery, see `run_enforce`); the
    // verdict can quote a guard message or the text of a failed operation,
    // so it is released as human-facing output, receipted for exactly the
    // bytes written, and handed over by the one observed writer (G08). It is
    // released and receipted under the project it evaluated — the explicit
    // `--project-root`, normalized as every explicit root is — not under the
    // directory the command happens to be called from.
    let root = super::common::detect_project_root(&[
        "--project-root".to_owned(),
        parsed.project_root.clone(),
    ]);
    let run = super::common::CliToolRun::new("ctx_proof", &root, None).human_facing();
    if parsed.as_json {
        match run.deliver_json(|| render_json(&parsed.tool, &outcome)) {
            Ok(document) => super::common::print_released(&document),
            Err(reason) => super::common::exit_withheld(&reason),
        }
    } else {
        super::common::print_released(&run.deliver_text(|| render_text(&parsed.tool, &outcome)));
    }
}

fn print_help() {
    println!(
        "lean-ctx policy enforce — evaluate a tool call against the active policy\n\n\
USAGE:\n  \
lean-ctx policy enforce <tool> --project-root <path> [--json '<args>'] [--as-json]\n\n\
Applies the exact agent-pipeline guards (role + context policy deny/allow,\n\
egress DLP on writes/actions, output redaction + input filters) WITHOUT\n\
starting the MCP server, and records the same audit-trail entries. The active\n\
policy is the project pack folded under any installed, trusted org-policy floor.\n\n\
EXAMPLES:\n  \
lean-ctx policy enforce ctx_url_read --project-root .\n  \
lean-ctx policy enforce ctx_shell --project-root . --json '{{\"command\":\"echo hi\"}}'\n  \
lean-ctx policy enforce ctx_read  --project-root . --json '{{\"path\":\"file.txt\"}}' --as-json"
    );
}

fn parse_args(args: &[String]) -> Result<EnforceArgs, String> {
    let mut tool: Option<String> = None;
    let mut project_root: Option<String> = None;
    let mut json: Option<String> = None;
    let mut as_json = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "--project-root" => {
                i += 1;
                project_root = Some(args.get(i).ok_or("--project-root needs a value")?.clone());
            }
            "--json" => {
                i += 1;
                json = Some(args.get(i).ok_or("--json needs a value")?.clone());
            }
            "--as-json" => as_json = true,
            other if other.starts_with("--") => {
                return Err(format!("unknown flag '{other}'"));
            }
            _ => {
                if tool.is_none() {
                    tool = Some(args[i].clone());
                } else {
                    return Err(format!("unexpected argument '{}'", args[i]));
                }
            }
        }
        i += 1;
    }

    Ok(EnforceArgs {
        tool: tool.ok_or("missing <tool>")?,
        project_root: project_root.ok_or("missing --project-root")?,
        json: json.unwrap_or_else(|| "{}".to_string()),
        as_json,
    })
}

fn parse_json_object(raw: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str::<Value>(raw).map_err(|e| format!("invalid --json: {e}"))? {
        Value::Object(m) => Ok(m),
        _ => Err("invalid --json: expected a JSON object".to_string()),
    }
}

/// The guarded evaluation. Mirrors `server::call_tool::call_tool_guarded`'s gate
/// order, reusing the identical guard functions so a single source of truth
/// governs the *rules* (only this thin orchestration is CLI-local).
pub(crate) fn run_enforce(
    tool: &str,
    project_root: &str,
    args_json: &str,
    map: &Map<String, Value>,
) -> EnforceOutcome {
    // 1. Role guard (records ToolDenied on block).
    let role = role_guard::check_tool_access(tool);
    if role.blocked {
        return EnforceOutcome::DeniedRole(role.message.unwrap_or_else(|| "role denied".into()));
    }

    // 2. Context-policy-pack tool gating (records ToolDenied on block).
    let policy = policy_guard::check_tool_access(tool);
    if policy.blocked {
        return EnforceOutcome::DeniedPolicy(
            policy.message.unwrap_or_else(|| "policy denied".into()),
        );
    }

    // 3. Egress / output DLP on write & action payloads, BEFORE dispatch — so a
    //    forbidden write never touches disk and a forbidden command never runs.
    //    Payload mapping (ctx_edit/ctx_patch/ctx_shell/ctx_execute) is shared
    //    with the server gate via `core::egress::write_payload`.
    if let Some(active) = runtime::active()
        && active.egress.is_active()
    {
        let payload = crate::core::egress::write_payload(tool, Some(map)).map(|(p, _)| p);
        if let Some(payload) = payload {
            if let Some(reason) = active.egress.check_content(&payload, &active.redaction) {
                policy_guard::audit_egress(tool, &reason);
                return EnforceOutcome::BlockedEgress(reason);
            }
            if let Some(max) = active.egress.max_writes_per_min
                && !crate::core::egress::check_rate(max)
            {
                policy_guard::audit_egress(tool, "rate-limit");
                return EnforceOutcome::BlockedEgress("rate-limit".to_string());
            }
        }
    }

    // 4. Dispatch the real tool (the guard is applied here, around the same
    //    handler the server invokes). The tool's text is evaluated below and
    //    never written, so the call is no delivery: nothing is released or
    //    receipted for it (`run_call_undelivered`). What this command writes
    //    — the verdict — is released where it is printed (`cmd_enforce`).
    let call_args = vec![
        tool.to_string(),
        "--project-root".to_string(),
        project_root.to_string(),
        "--json".to_string(),
        args_json.to_string(),
    ];
    let text = match super::call_cmd::run_call_undelivered(&call_args) {
        Ok(t) => t,
        Err(e) => return dispatch_failure(e.to_string()),
    };

    // 5. Use the same block-before-mask decision as model/storage boundaries.
    let mut redactions = 0;
    let mut filtered = Vec::new();
    if let Some(active) = runtime::active() {
        let outcome = crate::core::policy::content::evaluate_text(&text, &active);
        if outcome.blocked {
            policy_guard::audit_filter(tool, &outcome.audit, true);
            return EnforceOutcome::BlockedFilter(
                outcome.block_reason.unwrap_or_else(|| "policy".into()),
            );
        }
        if !outcome.audit.is_empty() {
            policy_guard::audit_filter(tool, &outcome.audit, false);
            redactions = outcome
                .audit
                .iter()
                .filter(|(label, _)| label.starts_with("redaction:"))
                .map(|(_, hits)| hits)
                .sum();
            filtered = outcome.audit;
        }
    }

    EnforceOutcome::Allowed {
        redactions,
        filtered,
    }
}

/// The verdict as text, without the final newline the writer adds.
fn render_text(tool: &str, outcome: &EnforceOutcome) -> String {
    match outcome {
        EnforceOutcome::DeniedRole(msg) | EnforceOutcome::DeniedPolicy(msg) => {
            format!("DENIED  {tool}\n{msg}")
        }
        EnforceOutcome::BlockedEgress(reason) => {
            format!("BLOCKED {tool}  egress DLP ({reason}) — write/action withheld; audited")
        }
        EnforceOutcome::BlockedFilter(reason) => {
            format!("BLOCKED {tool}  input filter ({reason}) — output withheld; audited")
        }
        EnforceOutcome::Allowed {
            redactions,
            filtered,
        } => {
            let filt = if filtered.is_empty() {
                "none".to_string()
            } else {
                filtered
                    .iter()
                    .map(|(c, n)| format!("{c}×{n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            format!("ALLOWED {tool}  redactions={redactions}  filters=[{filt}]")
        }
        EnforceOutcome::DispatchError(e) => {
            format!("ERROR   {tool}  tool handler failed: {e}")
        }
    }
}

/// The verdict as a `--as-json` document, without the final newline.
fn render_json(tool: &str, outcome: &EnforceOutcome) -> String {
    let detail = match outcome {
        EnforceOutcome::DeniedRole(m)
        | EnforceOutcome::DeniedPolicy(m)
        | EnforceOutcome::BlockedEgress(m)
        | EnforceOutcome::BlockedFilter(m)
        | EnforceOutcome::DispatchError(m) => Value::String(m.clone()),
        EnforceOutcome::Allowed {
            redactions,
            filtered,
        } => serde_json::json!({
            "redactions": redactions,
            "filters": filtered
                .iter()
                .map(|(c, n)| serde_json::json!({ "class": c, "count": n }))
                .collect::<Vec<_>>(),
        }),
    };
    let v = serde_json::json!({
        "tool": tool,
        "status": outcome.status(),
        "detail": detail,
    });
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_read_the_policy_withholds_is_blocked_and_any_other_failure_is_a_dispatch_error() {
        let blocked = super::dispatch_failure(
            "error: -32602: Cannot read file: /p/notes.md: source content withheld by policy"
                .into(),
        );
        assert_eq!(blocked.status(), "blocked-filter");
        let failed =
            super::dispatch_failure("error: -32602: Cannot read file: /p/x: not found".into());
        assert_eq!(failed.status(), "dispatch-error");
    }

    use super::*;
    use serde_json::Map;

    #[test]
    fn parse_requires_tool_and_root() {
        assert!(parse_args(&["ctx_read".into()]).is_err());
        let ok =
            parse_args(&["ctx_read".into(), "--project-root".into(), ".".into()]).expect("valid");
        assert_eq!(ok.tool, "ctx_read");
        assert_eq!(ok.json, "{}");
        assert!(!ok.as_json);
    }

    #[test]
    fn json_must_be_object() {
        assert!(parse_json_object("[]").is_err());
        assert!(parse_json_object("{\"a\":1}").is_ok());
    }

    #[test]
    fn status_labels_are_stable() {
        assert_eq!(
            EnforceOutcome::DeniedRole(String::new()).status(),
            "denied-role"
        );
        assert_eq!(
            EnforceOutcome::BlockedEgress(String::new()).status(),
            "blocked-egress"
        );
        assert_eq!(
            EnforceOutcome::Allowed {
                redactions: 0,
                filtered: vec![]
            }
            .status(),
            "allowed"
        );
    }

    #[test]
    fn exempt_meta_tool_is_never_policy_denied() {
        // No active pack in the unit-test environment ⇒ exempt + ordinary tools
        // both pass the gates; this asserts the orchestration wiring, not rules.
        let map = Map::new();
        let outcome = run_enforce("ctx_session", ".", "{}", &map);
        assert_ne!(outcome.status(), "denied-policy");
    }
}
