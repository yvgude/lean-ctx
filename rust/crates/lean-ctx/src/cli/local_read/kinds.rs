// SPDX-License-Identifier: Apache-2.0
//! The answer of every Contracts kind, for the CLI and the socket alike
//! (G08): each document is built inside a released CLI tool run — the
//! release point, redaction, and exactly one receipt for the bytes the
//! reader gets. The CLI prints it, the local service sends it.

use super::{Answer, Code, Failure, Via};

/// The project a released document belongs to: the one the request names
/// (`--project-root`); on the CLI otherwise the reader's, detected from the
/// working directory; on the socket otherwise none (the service has no
/// working directory of the reader's).
pub(crate) fn release_root(args: &[String], via: Via) -> String {
    match super::flag(args, "--project-root").ok().flatten() {
        Some(root) => root.to_owned(),
        None if via == Via::Cli => super::super::common::detect_project_root(args),
        None => String::new(),
    }
}

/// The release scope (tool) of each kind.
fn tool_of(kind: &str) -> &'static str {
    match kind {
        "session.stats" | "session.cost" => "ctx_session",
        "gain.daily" | "gain.projects" | "spend.projects" => "ctx_metrics",
        "audit.verify" | "ledger.verify" | "evidence.export" => "ctx_proof",
        "providers.list" => "ctx_provider",
        "policies.list" => "ctx_policy",
        "knowledge.remembered" | "knowledge.edited" | "knowledge.related" | "knowledge.removed" => {
            "ctx_knowledge"
        }
        _ => "ctx_agent",
    }
}

/// The document of the Contracts kind `kind` for `args`, for `via`.
pub(crate) fn answer(kind: &str, args: &[String], via: Via) -> Answer {
    let root = release_root(args, via);
    let released = |build: &dyn Fn() -> Result<serde_json::Value, Failure>| {
        super::answer_released(tool_of(kind), &root, kind, via, build)
    };
    match kind {
        "session.stats" => released(&|| super::session::build_stats(args).and_then(to_value)),
        "session.cost" => released(&|| super::session::build_cost(args).and_then(to_value)),
        "gain.daily" => released(&|| super::usage::build_daily(args).and_then(to_value)),
        "gain.projects" => released(&|| super::usage::build_projects(args).and_then(to_value)),
        "spend.projects" => released(&|| super::usage::build_spend(args).and_then(to_value)),
        "bus.list" | "bus.read" | "bus.post" | "bus.leases" | "bus.claim" | "bus.release" => {
            super::bus::bus_answer(&kind["bus.".len()..], args, via)
        }
        "presence.list" => released(&|| super::bus_presence::build(args).and_then(to_value)),
        "hive.presence" => released(&|| super::presence::build(args).and_then(to_value)),
        "knowledge.remembered" | "knowledge.edited" | "knowledge.related" | "knowledge.removed" => {
            super::knowledge_write::answer(kind, args, via)
        }
        "audit.verify" => released(&|| super::evidence::build_audit(args).and_then(to_value)),
        "ledger.verify" => released(&|| super::evidence::build_ledger(args).and_then(to_value)),
        "evidence.export" => released(&|| super::evidence::build_export(args).and_then(to_value)),
        "providers.list" => released(&|| super::providers::build(args).and_then(to_value)),
        "policies.list" => released(&|| super::policies::build(args).and_then(to_value)),
        "agents.list" => released(&|| super::agents::build(args).and_then(to_value)),
        "daemon.status" => released(&|| super::daemon::build(args).and_then(to_value)),
        other => match super::part_of(other) {
            Some(part) => (part.answer)(other, args, via),
            None => Answer::failure(&Failure::new(
                Code::Internal,
                format!("no builder for {other}"),
            )),
        },
    }
}

fn to_value(body: impl serde::Serialize) -> Result<serde_json::Value, Failure> {
    serde_json::to_value(body).map_err(|e| Failure::new(Code::Internal, e.to_string()))
}

/// Print the document of `kind` as the CLI does.
pub(crate) fn run(kind: &str, args: &[String]) {
    super::print(&answer(kind, args, Via::Cli));
}
