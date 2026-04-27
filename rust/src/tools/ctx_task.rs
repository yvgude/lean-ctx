use crate::core::a2a::task::{TaskPart, TaskState, TaskStore};

pub fn handle(
    action: &str,
    current_agent_id: Option<&str>,
    task_id: Option<&str>,
    to_agent: Option<&str>,
    description: Option<&str>,
    state: Option<&str>,
    message: Option<&str>,
) -> String {
    let agent = match current_agent_id {
        Some(id) => id,
        None if action == "list" || action == "info" => "unknown",
        None => {
            return "Error: agent must be registered first (use ctx_agent action=register)"
                .to_string()
        }
    };

    let mut store = TaskStore::load();
    store.cleanup_old(72);

    let result = match action {
        "create" => handle_create(&mut store, agent, to_agent, description),
        "update" => handle_update(&mut store, agent, task_id, state, message),
        "list" => handle_list(&store, agent),
        "get" => handle_get(&store, task_id),
        "cancel" => handle_cancel(&mut store, agent, task_id, message),
        "message" => handle_message(&mut store, agent, task_id, message),
        "info" => handle_info(&store),
        _ => format!(
            "Unknown action '{action}'. Available: create, update, list, get, cancel, message, info"
        ),
    };

    if matches!(action, "create" | "update" | "cancel" | "message") {
        let _ = store.save();
    }

    result
}

fn handle_create(
    store: &mut TaskStore,
    from: &str,
    to: Option<&str>,
    desc: Option<&str>,
) -> String {
    let Some(to_agent) = to else {
        return "Error: to_agent is required for task creation".to_string();
    };
    let description = desc.unwrap_or("(no description)");
    let id = store.create_task(from, to_agent, description);
    format!("Task created: {id}\n  From: {from}\n  To: {to_agent}\n  Description: {description}")
}

fn handle_update(
    store: &mut TaskStore,
    agent: &str,
    task_id: Option<&str>,
    state: Option<&str>,
    message: Option<&str>,
) -> String {
    let Some(tid) = task_id else {
        return "Error: task_id is required".to_string();
    };
    let new_state = match state {
        Some(s) => match TaskState::parse_str(s) {
            Some(st) => st,
            None => return format!("Error: invalid state '{s}'. Use: working, input-required, completed, failed, canceled"),
        },
        None => return "Error: state is required for update".to_string(),
    };

    let Some(task) = store.get_task_mut(tid) else {
        return format!("Error: task '{tid}' not found");
    };

    if task.to_agent != agent && task.from_agent != agent {
        return format!("Error: agent '{agent}' is not involved in task '{tid}'");
    }

    match task.transition(new_state.clone(), message) {
        Ok(()) => {
            if let Some(msg) = message {
                task.add_message(
                    agent,
                    vec![TaskPart::Text {
                        text: msg.to_string(),
                    }],
                );
            }
            format!(
                "Task {tid} updated → {new_state}\n  History: {} transitions",
                task.history.len()
            )
        }
        Err(e) => format!("Error: {e}"),
    }
}

fn handle_list(store: &TaskStore, agent: &str) -> String {
    let tasks = store.tasks_for_agent(agent);
    if tasks.is_empty() {
        return "No tasks found for this agent.".to_string();
    }

    let mut lines = vec![format!("Tasks ({}):", tasks.len())];
    for task in &tasks {
        let direction = if task.from_agent == agent {
            format!("→ {}", task.to_agent)
        } else {
            format!("← {}", task.from_agent)
        };
        lines.push(format!(
            "  {} [{}] {} — {}",
            task.id, task.state, direction, task.description
        ));
    }

    let pending = store.pending_tasks_for(agent);
    if !pending.is_empty() {
        lines.push(format!(
            "\n{} pending task(s) assigned to you.",
            pending.len()
        ));
    }

    lines.join("\n")
}

fn handle_get(store: &TaskStore, task_id: Option<&str>) -> String {
    let Some(tid) = task_id else {
        return "Error: task_id is required".to_string();
    };
    let Some(task) = store.get_task(tid) else {
        return format!("Error: task '{tid}' not found");
    };

    let mut lines = vec![
        format!("Task: {}", task.id),
        format!("  State: {}", task.state),
        format!("  From: {}", task.from_agent),
        format!("  To: {}", task.to_agent),
        format!("  Description: {}", task.description),
        format!("  Created: {}", task.created_at),
        format!("  Updated: {}", task.updated_at),
        format!("  Messages: {}", task.messages.len()),
        format!("  Artifacts: {}", task.artifacts.len()),
    ];

    if !task.history.is_empty() {
        lines.push("  History:".to_string());
        for t in &task.history {
            lines.push(format!(
                "    {} → {} ({})",
                t.from,
                t.to,
                t.reason.as_deref().unwrap_or("-")
            ));
        }
    }

    lines.join("\n")
}

fn handle_cancel(
    store: &mut TaskStore,
    agent: &str,
    task_id: Option<&str>,
    reason: Option<&str>,
) -> String {
    let Some(tid) = task_id else {
        return "Error: task_id is required".to_string();
    };
    let Some(task) = store.get_task_mut(tid) else {
        return format!("Error: task '{tid}' not found");
    };

    if task.from_agent != agent {
        return format!(
            "Error: only the task creator can cancel (creator: {})",
            task.from_agent
        );
    }

    match task.transition(TaskState::Canceled, reason) {
        Ok(()) => format!("Task {tid} canceled."),
        Err(e) => format!("Error: {e}"),
    }
}

fn handle_message(
    store: &mut TaskStore,
    agent: &str,
    task_id: Option<&str>,
    message: Option<&str>,
) -> String {
    let Some(tid) = task_id else {
        return "Error: task_id is required".to_string();
    };
    let Some(msg) = message else {
        return "Error: message is required".to_string();
    };
    let Some(task) = store.get_task_mut(tid) else {
        return format!("Error: task '{tid}' not found");
    };

    task.add_message(
        agent,
        vec![TaskPart::Text {
            text: msg.to_string(),
        }],
    );
    format!(
        "Message added to task {tid} ({} messages total)",
        task.messages.len()
    )
}

fn handle_info(store: &TaskStore) -> String {
    let total = store.tasks.len();
    let active = store
        .tasks
        .iter()
        .filter(|t| !t.state.is_terminal())
        .count();
    let completed = store
        .tasks
        .iter()
        .filter(|t| t.state == TaskState::Completed)
        .count();
    let failed = store
        .tasks
        .iter()
        .filter(|t| t.state == TaskState::Failed)
        .count();

    format!("Task Store: {total} total, {active} active, {completed} completed, {failed} failed")
}
