// SPDX-License-Identifier: Apache-2.0
//! Tasks as an agent reads them. Every text people or other agents wrote
//! (titles, goals, criteria, summaries, labels) is fenced as foreign data
//! ([`crate::core::hive::untrusted::wrap`], X5); only the numbers, states
//! and the engine's own words stand outside the fence.
//! Deterministic for the same answer (#498).

use super::TaskView;
use crate::core::hive::untrusted::{UntrustedKind, wrap};
use lean_ctx_protocol::hive::tasks::{
    TaskAckV1, TaskLifecycle, TaskListV1, TaskOriginKind, TaskResultV1, TaskState,
};

fn state(state: TaskState) -> &'static str {
    match state {
        TaskState::Proposed => "proposed",
        TaskState::Open => "open",
        TaskState::Claimed => "claimed",
        TaskState::Submitted => "submitted",
        TaskState::Done => "done",
        TaskState::Rejected => "rejected",
        TaskState::Cancelled => "cancelled",
        TaskState::Failed => "failed",
        TaskState::Unknown => "unknown",
    }
}

/// The state word, plus ` running` when a Hive v5 lifecycle says the
/// holder reported progress (the one thing `state` does not show). A 1.x
/// Hive sends no lifecycle: the state alone, as before.
fn state_with(task_state: TaskState, lifecycle: Option<TaskLifecycle>) -> String {
    let word = state(task_state);
    if task_state == TaskState::Claimed && lifecycle == Some(TaskLifecycle::Running) {
        format!("{word} running")
    } else {
        word.to_owned()
    }
}

/// What carries a result: a 1.x proposal, or (Hive v5, proposal 0) the
/// log seq of its knowledge, or the task alone.
fn result_line(result: &TaskResultV1) -> String {
    let carrier = match (result.proposal, result.seq) {
        (0, Some(seq)) => format!("knowledge at log seq {seq}"),
        (0, None) => "recorded on the task".to_owned(),
        (proposal, _) => format!("proposal #{proposal}"),
    };
    format!("\nResult: {carrier} ({} evidence)", result.evidence.len())
}

/// What a result did, from Hive's answer: a 1.x Hive holds it for a
/// decision (`submitted`), Hive v5 ends the task (`done` or `failed`).
#[must_use]
pub fn render_result_ack(ack: &TaskAckV1) -> String {
    let n = ack.number;
    match ack.state {
        TaskState::Submitted => {
            format!(
                "Result submitted on task #{n}; a person or the review threshold decides on it."
            )
        }
        TaskState::Done => format!("Task #{n} completed with this result."),
        TaskState::Failed => format!("Task #{n} ended as failed with this result."),
        other => format!("Result recorded on task #{n} (now {}).", state(other)),
    }
}

fn origin(origin: TaskOriginKind) -> &'static str {
    match origin {
        TaskOriginKind::Person => "person",
        TaskOriginKind::Agent => "agent",
        TaskOriginKind::Contradiction => "contradiction",
        TaskOriginKind::Question => "question",
        TaskOriginKind::QuarantineTimeout => "quarantine timeout",
        TaskOriginKind::Unknown => "unknown",
    }
}

/// The first `limit` tasks of `list`, one line each.
#[must_use]
pub fn render_list(list: &TaskListV1, limit: usize) -> String {
    if list.tasks.is_empty() {
        return "No Hive tasks match.".into();
    }
    let mut body = String::new();
    for task in list.tasks.iter().take(limit) {
        body.push_str(&format!(
            "#{} {} ({}){}{}: {}\n",
            task.number,
            state_with(task.state, task.lifecycle),
            origin(task.origin),
            task.role
                .as_deref()
                .map(|role| format!(" role={role}"))
                .unwrap_or_default(),
            if task.mine { " mine" } else { "" },
            task.title.as_deref().unwrap_or("(sealed)"),
        ));
    }
    let more = list.tasks.len().saturating_sub(limit);
    let mut out = format!("Hive tasks ({}):\n", list.tasks.len());
    out.push_str(&wrap(
        UntrustedKind::TaskBody,
        "hive tasks",
        body.trim_end(),
    ));
    if more > 0 {
        out.push_str(&format!("\n… {more} more (ctx_tasks action=list)"));
    }
    out
}

/// One task with its updates and result.
#[must_use]
pub fn render_task(view: &TaskView) -> String {
    let task = &view.task;
    let mut out = format!(
        "Hive task #{} — {} ({})",
        task.number,
        state_with(task.state, task.lifecycle),
        origin(task.origin.kind)
    );
    // Outside the fence only a validated role slug: anything else Hive
    // sends there is someone's text and never reaches the header (X5).
    if let Some(role) = task
        .role
        .as_deref()
        .filter(|role| lean_ctx_protocol::hive::tasks::validate_role(role).is_ok())
    {
        out.push_str(&format!(" role={role}"));
    }
    match (&view.held_until, &task.claim) {
        (Some(until), _) => out.push_str(&format!("\nThis machine holds the claim until {until}.")),
        (None, Some(claim)) => out.push_str(&format!(
            "\nClaimed until {}{}.",
            claim.until.as_str(),
            if claim.holder.mine {
                " (by you, elsewhere)"
            } else {
                ""
            }
        )),
        (None, None) => {}
    }
    let (title, goal, acceptance) = match (&view.content, task.sealed.is_some()) {
        (Some(content), _) => (
            Some(content.title.as_str()),
            content.goal.as_deref(),
            Some(content.acceptance.as_str()),
        ),
        (None, true) => {
            out.push_str("\nSealed; this machine holds no key for it yet.");
            (None, None, None)
        }
        (None, false) => (
            task.title.as_deref(),
            task.goal.as_deref(),
            task.acceptance.as_deref(),
        ),
    };
    let mut body = String::new();
    if let Some(title) = title {
        body.push_str(&format!("Title: {title}\n"));
    }
    if let Some(goal) = goal {
        body.push_str(&format!("Goal: {goal}\n"));
    }
    if let Some(acceptance) = acceptance {
        body.push_str(&format!("Done when: {acceptance}\n"));
    }
    body.push_str(&format!("Created by: {}\n", task.created_by.label));
    let source = format!("hive task #{}", task.number);
    out.push('\n');
    out.push_str(&wrap(UntrustedKind::TaskBody, &source, body.trim_end()));
    if !task.updates.is_empty() {
        let mut updates = String::new();
        for (update, summary) in task.updates.iter().zip(&view.update_summaries) {
            updates.push_str(&format!(
                "{} {}: {}\n",
                update.at.as_str(),
                update.by.label,
                summary.as_deref().unwrap_or("(sealed)")
            ));
        }
        out.push_str("\nUpdates:\n");
        out.push_str(&wrap(
            UntrustedKind::TaskUpdate,
            &source,
            updates.trim_end(),
        ));
    }
    if let Some(result) = &task.result {
        out.push_str(&result_line(result));
    }
    out
}
