// SPDX-License-Identifier: Apache-2.0
//! A task's priority and labels (C0 2.2 E47): the order of work on a board.
//!
//! - Priority is `high` or `normal` (absent on the wire means `normal`);
//!   labels are at most 16 unique `[a-z0-9-]{1,32}`. A wrong value is
//!   refused here, before any request.
//! - A write is a `PATCH` at the task's `fields_version`. When another write
//!   came first (`409 stale_base`) the task is read again and the write is
//!   sent once more at the new version; a second conflict is shown.
//! - Lists show `high` before `normal`, then by number.

use std::path::Path;

use lean_ctx_protocol::hive::lease::HiveRight;
use lean_ctx_protocol::hive::tasks::{
    TASK_FIELDS_SCHEMA, TaskAckV1, TaskFieldsRequestV1, TaskPriority, TaskSummaryV1,
    validate_labels,
};

use super::{Link, connected};
use crate::{HiveError, TaskRefusal};

/// What a priority or labels write changes; `None` keeps the field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldsChange {
    pub priority: Option<TaskPriority>,
    /// The whole new list (`Some(vec![])` clears it).
    pub labels: Option<Vec<String>>,
}

impl FieldsChange {
    /// Refuses a change that changes nothing or carries bad labels.
    pub fn validate(&self) -> Result<(), HiveError> {
        if self.priority.is_none() && self.labels.is_none() {
            return Err(HiveError::Config("set a priority, labels or both".into()));
        }
        if let Some(labels) = &self.labels {
            validate_labels(labels).map_err(|e| HiveError::Config(format!("labels: {e}")))?;
        }
        Ok(())
    }
}

/// `high` or `normal`, exactly; anything else is refused with what is allowed.
pub fn parse_priority(value: &str) -> Result<TaskPriority, HiveError> {
    match value.trim() {
        "high" => Ok(TaskPriority::High),
        "normal" => Ok(TaskPriority::Normal),
        other => Err(HiveError::Config(format!(
            "priority is high or normal (got {:?})",
            other.chars().take(32).collect::<String>()
        ))),
    }
}

/// The wire word of a priority (`normal` when absent).
#[must_use]
pub fn priority_word(priority: Option<TaskPriority>) -> &'static str {
    match priority.unwrap_or_default() {
        TaskPriority::High => "high",
        TaskPriority::Normal => "normal",
    }
}

/// Labels from `a,b,c` (spaces around a label are dropped, an empty value
/// is no labels), validated as Hive validates them.
pub fn parse_labels(value: &str) -> Result<Vec<String>, HiveError> {
    let labels: Vec<String> = value
        .split(',')
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_owned)
        .collect();
    validate_labels(&labels).map_err(|e| HiveError::Config(format!("labels: {e}")))?;
    Ok(labels)
}

/// The board order: `high` first, then by number.
pub fn sort_by_priority(tasks: &mut [TaskSummaryV1]) {
    tasks.sort_by_key(|task| (rank(task.priority), task.number));
}

/// `0` for `high`, `1` for `normal` (and absent).
#[must_use]
pub fn rank(priority: Option<TaskPriority>) -> u8 {
    match priority.unwrap_or_default() {
        TaskPriority::High => 0,
        TaskPriority::Normal => 1,
    }
}

/// Set task `number`'s priority and/or labels in the project's vault.
pub fn set_fields(
    project_root: &Path,
    number: u64,
    change: &FieldsChange,
) -> Result<TaskAckV1, HiveError> {
    change.validate()?;
    let c = connected(project_root, HiveRight::TasksPropose)?;
    set_fields_via(&c.link(), number, change)
}

pub(super) fn set_fields_via(
    link: &Link<'_>,
    number: u64,
    change: &FieldsChange,
) -> Result<TaskAckV1, HiveError> {
    change.validate()?;
    let send = || {
        let base_version = link
            .call(|api| api.show(number))?
            .fields_version
            .unwrap_or(0);
        let request = TaskFieldsRequestV1 {
            schema: TASK_FIELDS_SCHEMA.into(),
            base_version,
            priority: change.priority,
            labels: change.labels.clone(),
        };
        link.call(|api| api.fields(number, &request))
    };
    match send() {
        Err(HiveError::Task {
            refusal: TaskRefusal::StaleBase,
            ..
        }) => send(),
        other => other,
    }
}
