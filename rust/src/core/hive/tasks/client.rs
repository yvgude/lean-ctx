// SPDX-License-Identifier: Apache-2.0
//! The task routes of C0 1.9 §1 under `/v1/vaults/{vault_id}/tasks`:
//! requests are validated before they leave, answers before they are used.

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::core::hive::HiveError;
use crate::core::hive::transport::HiveClient;
use lean_ctx_protocol::ValidationError;
use lean_ctx_protocol::hive::tasks::{
    TASK_ACK_SCHEMA, TASK_CLAIM_ACK_SCHEMA, TaskAckV1, TaskClaimAckV1, TaskClaimRenewRequestV1,
    TaskClaimRequestV1, TaskListV1, TaskProposeRequestV1, TaskReleaseRequestV1,
    TaskResultRequestV1, TaskState, TaskUpdateRequestV1, TaskV1, validate_role,
};

/// `GET …/tasks` filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListFilter {
    pub state: Option<TaskState>,
    pub role: Option<String>,
    pub mine: bool,
}

impl ListFilter {
    /// The query string of these filters (`""` without any).
    pub(super) fn query(&self) -> Result<String, HiveError> {
        let mut parts = Vec::new();
        if let Some(state) = self.state {
            let state = serde_json::to_value(state)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .filter(|s| s != "unknown")
                .ok_or_else(|| HiveError::Local("unknown task state".into()))?;
            parts.push(format!("state={state}"));
        }
        if let Some(role) = &self.role {
            validate_role(role).map_err(|e| invalid(&e))?;
            parts.push(format!("role={role}"));
        }
        if self.mine {
            parts.push("mine=true".into());
        }
        Ok(if parts.is_empty() {
            String::new()
        } else {
            format!("?{}", parts.join("&"))
        })
    }
}

fn invalid(error: &ValidationError) -> HiveError {
    HiveError::Local(format!("invalid task request: {error}"))
}

fn decode<T: DeserializeOwned>(value: Value, what: &str) -> Result<T, HiveError> {
    serde_json::from_value(value).map_err(|e| HiveError::Protocol(format!("{what}: {e}")))
}

fn encode<T: serde::Serialize>(request: &T) -> Result<Value, HiveError> {
    serde_json::to_value(request).map_err(|e| HiveError::Local(e.to_string()))
}

/// The task routes of one project.
pub(super) struct TasksApi<'a> {
    pub(super) client: &'a HiveClient,
    pub(super) vault_id: &'a str,
}

impl TasksApi<'_> {
    fn path(&self, tail: &str) -> String {
        format!("/vaults/{}/tasks{tail}", self.vault_id)
    }

    fn ack(&self, answer: Value, number: Option<u64>) -> Result<TaskAckV1, HiveError> {
        let ack: TaskAckV1 = decode(answer, "task ack")?;
        if ack.schema != TASK_ACK_SCHEMA || number.is_some_and(|n| n != ack.number) {
            return Err(HiveError::Protocol(
                "task ack for another task or schema".into(),
            ));
        }
        Ok(ack)
    }

    fn claim_ack(&self, answer: Value, number: u64) -> Result<TaskClaimAckV1, HiveError> {
        let ack: TaskClaimAckV1 = decode(answer, "task claim")?;
        ack.claim
            .validate()
            .map_err(|e| HiveError::Protocol(format!("task claim: {e}")))?;
        if ack.schema != TASK_CLAIM_ACK_SCHEMA || ack.number != number {
            return Err(HiveError::Protocol(
                "task claim for another task or schema".into(),
            ));
        }
        Ok(ack)
    }

    pub(super) fn list(&self, filter: &ListFilter) -> Result<TaskListV1, HiveError> {
        let list: TaskListV1 = decode(self.client.get(&self.path(&filter.query()?))?, "task list")?;
        list.validate()
            .map_err(|e| HiveError::Protocol(format!("task list: {e}")))?;
        if list.vault_id != self.vault_id {
            return Err(HiveError::Protocol("task list of another vault".into()));
        }
        Ok(list)
    }

    pub(super) fn show(&self, number: u64) -> Result<TaskV1, HiveError> {
        let task: TaskV1 = decode(self.client.get(&self.path(&format!("/{number}")))?, "task")?;
        task.validate()
            .map_err(|e| HiveError::Protocol(format!("task: {e}")))?;
        if task.vault_id != self.vault_id || task.number != number {
            return Err(HiveError::Protocol("another task than asked for".into()));
        }
        Ok(task)
    }

    /// `idempotency_key` makes a resent proposal one task.
    pub(super) fn propose(
        &self,
        request: &TaskProposeRequestV1,
        idempotency_key: &str,
    ) -> Result<TaskAckV1, HiveError> {
        request.validate().map_err(|e| invalid(&e))?;
        let answer =
            self.client
                .post_idempotent(&self.path(""), &encode(request)?, idempotency_key)?;
        self.ack(answer, None)
    }

    pub(super) fn claim(
        &self,
        number: u64,
        request: &TaskClaimRequestV1,
    ) -> Result<TaskClaimAckV1, HiveError> {
        request.validate().map_err(|e| invalid(&e))?;
        let answer = self
            .client
            .post(&self.path(&format!("/{number}/claim")), &encode(request)?)?;
        self.claim_ack(answer, number)
    }

    pub(super) fn renew(
        &self,
        number: u64,
        request: &TaskClaimRenewRequestV1,
    ) -> Result<TaskClaimAckV1, HiveError> {
        request.validate().map_err(|e| invalid(&e))?;
        let answer = self.client.post(
            &self.path(&format!("/{number}/claim/renew")),
            &encode(request)?,
        )?;
        self.claim_ack(answer, number)
    }

    pub(super) fn release(
        &self,
        number: u64,
        request: &TaskReleaseRequestV1,
    ) -> Result<TaskAckV1, HiveError> {
        request.validate().map_err(|e| invalid(&e))?;
        let answer = self
            .client
            .post(&self.path(&format!("/{number}/release")), &encode(request)?)?;
        self.ack(answer, Some(number))
    }

    pub(super) fn update(
        &self,
        number: u64,
        request: &TaskUpdateRequestV1,
    ) -> Result<TaskAckV1, HiveError> {
        request.validate().map_err(|e| invalid(&e))?;
        let answer = self
            .client
            .post(&self.path(&format!("/{number}/updates")), &encode(request)?)?;
        self.ack(answer, Some(number))
    }

    pub(super) fn result(
        &self,
        number: u64,
        request: &TaskResultRequestV1,
    ) -> Result<TaskAckV1, HiveError> {
        request.validate().map_err(|e| invalid(&e))?;
        let answer = self
            .client
            .post(&self.path(&format!("/{number}/result")), &encode(request)?)?;
        self.ack(answer, Some(number))
    }
}
