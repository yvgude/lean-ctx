// SPDX-License-Identifier: Apache-2.0
//! The task routes of C0 1.9 §1 under `/v1/vaults/{vault_id}/tasks`:
//! requests are validated before they leave, answers before they are used.

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::CloudError;
use crate::transport::CloudClient;
use lean_ctx_protocol::ValidationError;
use lean_ctx_protocol::cloud::tasks::{
    TASK_ACK_SCHEMA, TASK_CLAIM_ACK_SCHEMA, TaskAckV1, TaskClaimAckV1, TaskClaimRenewRequestV1,
    TaskClaimRequestV1, TaskFieldsRequestV1, TaskListV1, TaskPriority, TaskProposeRequestV1,
    TaskReleaseRequestV1, TaskResultRequestV1, TaskState, TaskUpdateRequestV1, TaskV1,
    TaskVerifyRequestV1, validate_role,
};

/// `GET …/tasks` filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListFilter {
    pub state: Option<TaskState>,
    pub role: Option<String>,
    pub mine: bool,
    /// Cloud's `priority=` filter: only the tasks of this priority.
    pub priority: Option<TaskPriority>,
}

impl ListFilter {
    /// The query string of these filters (`""` without any).
    pub(super) fn query(&self) -> Result<String, CloudError> {
        let mut parts = Vec::new();
        if let Some(state) = self.state {
            let state = serde_json::to_value(state)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .filter(|s| s != "unknown")
                .ok_or_else(|| CloudError::Local("unknown task state".into()))?;
            parts.push(format!("state={state}"));
        }
        if let Some(role) = &self.role {
            validate_role(role).map_err(|e| invalid(&e))?;
            parts.push(format!("role={role}"));
        }
        if self.mine {
            parts.push("mine=true".into());
        }
        if let Some(priority) = self.priority {
            let word = super::fields::priority_word(Some(priority));
            parts.push(format!("priority={word}"));
        }
        Ok(if parts.is_empty() {
            String::new()
        } else {
            format!("?{}", parts.join("&"))
        })
    }
}

fn invalid(error: &ValidationError) -> CloudError {
    CloudError::Local(format!("invalid task request: {error}"))
}

fn decode<T: DeserializeOwned>(value: Value, what: &str) -> Result<T, CloudError> {
    serde_json::from_value(value).map_err(|e| CloudError::Protocol(format!("{what}: {e}")))
}

fn encode<T: serde::Serialize>(request: &T) -> Result<Value, CloudError> {
    serde_json::to_value(request).map_err(|e| CloudError::Local(e.to_string()))
}

/// The task routes of one project.
pub(super) struct TasksApi<'a> {
    pub(super) client: &'a CloudClient,
    pub(super) vault_id: &'a str,
}

impl TasksApi<'_> {
    fn path(&self, tail: &str) -> String {
        format!("/vaults/{}/tasks{tail}", self.vault_id)
    }

    fn ack(&self, answer: Value, number: Option<u64>) -> Result<TaskAckV1, CloudError> {
        let ack: TaskAckV1 = decode(answer, "task ack")?;
        if ack.schema != TASK_ACK_SCHEMA || number.is_some_and(|n| n != ack.number) {
            return Err(CloudError::Protocol(
                "task ack for another task or schema".into(),
            ));
        }
        Ok(ack)
    }

    fn claim_ack(&self, answer: Value, number: u64) -> Result<TaskClaimAckV1, CloudError> {
        let ack: TaskClaimAckV1 = decode(answer, "task claim")?;
        ack.claim
            .validate()
            .map_err(|e| CloudError::Protocol(format!("task claim: {e}")))?;
        if ack.schema != TASK_CLAIM_ACK_SCHEMA || ack.number != number {
            return Err(CloudError::Protocol(
                "task claim for another task or schema".into(),
            ));
        }
        Ok(ack)
    }

    pub(super) fn list(&self, filter: &ListFilter) -> Result<TaskListV1, CloudError> {
        let list: TaskListV1 = decode(self.client.get(&self.path(&filter.query()?))?, "task list")?;
        list.validate()
            .map_err(|e| CloudError::Protocol(format!("task list: {e}")))?;
        if list.vault_id != self.vault_id {
            return Err(CloudError::Protocol("task list of another vault".into()));
        }
        Ok(list)
    }

    pub(super) fn show(&self, number: u64) -> Result<TaskV1, CloudError> {
        let task: TaskV1 = decode(self.client.get(&self.path(&format!("/{number}")))?, "task")?;
        task.validate()
            .map_err(|e| CloudError::Protocol(format!("task: {e}")))?;
        if task.vault_id != self.vault_id || task.number != number {
            return Err(CloudError::Protocol("another task than asked for".into()));
        }
        Ok(task)
    }

    /// `idempotency_key` makes a resent proposal one task.
    pub(super) fn propose(
        &self,
        request: &TaskProposeRequestV1,
        idempotency_key: &str,
    ) -> Result<TaskAckV1, CloudError> {
        request.validate().map_err(|e| invalid(&e))?;
        let answer =
            self.client
                .post_idempotent(&self.path(""), &encode(request)?, idempotency_key)?;
        self.ack(answer, None)
    }

    /// `idempotency_key` is the one of the logical claim: every resend of
    /// it carries the same key, so a claim whose answer was lost is not
    /// granted twice.
    pub(super) fn claim(
        &self,
        number: u64,
        request: &TaskClaimRequestV1,
        idempotency_key: &str,
    ) -> Result<TaskClaimAckV1, CloudError> {
        request.validate().map_err(|e| invalid(&e))?;
        let answer = self.client.post_idempotent(
            &self.path(&format!("/{number}/claim")),
            &encode(request)?,
            idempotency_key,
        )?;
        self.claim_ack(answer, number)
    }

    pub(super) fn renew(
        &self,
        number: u64,
        request: &TaskClaimRenewRequestV1,
    ) -> Result<TaskClaimAckV1, CloudError> {
        request.validate().map_err(|e| invalid(&e))?;
        let answer = self.client.post(
            &self.path(&format!("/{number}/claim/renew")),
            &encode(request)?,
        )?;
        self.claim_ack(answer, number)
    }

    /// `idempotency_key`: one per logical release, see [`Self::claim`].
    pub(super) fn release(
        &self,
        number: u64,
        request: &TaskReleaseRequestV1,
        idempotency_key: &str,
    ) -> Result<TaskAckV1, CloudError> {
        request.validate().map_err(|e| invalid(&e))?;
        let answer = self.client.post_idempotent(
            &self.path(&format!("/{number}/release")),
            &encode(request)?,
            idempotency_key,
        )?;
        self.ack(answer, Some(number))
    }

    /// `idempotency_key`: one per logical update, so a resent summary is
    /// one entry of the task's history.
    pub(super) fn update(
        &self,
        number: u64,
        request: &TaskUpdateRequestV1,
        idempotency_key: &str,
    ) -> Result<TaskAckV1, CloudError> {
        request.validate().map_err(|e| invalid(&e))?;
        let answer = self.client.post_idempotent(
            &self.path(&format!("/{number}/updates")),
            &encode(request)?,
            idempotency_key,
        )?;
        self.ack(answer, Some(number))
    }

    /// `idempotency_key`: one per logical submission, see [`Self::claim`].
    pub(super) fn result(
        &self,
        number: u64,
        request: &TaskResultRequestV1,
        idempotency_key: &str,
    ) -> Result<TaskAckV1, CloudError> {
        request.validate().map_err(|e| invalid(&e))?;
        let answer = self.client.post_idempotent(
            &self.path(&format!("/{number}/result")),
            &encode(request)?,
            idempotency_key,
        )?;
        self.ack(answer, Some(number))
    }

    /// C0 2.2 E48; `idempotency_key` makes a resent verdict one verdict.
    pub(super) fn verify(
        &self,
        number: u64,
        request: &TaskVerifyRequestV1,
        idempotency_key: &str,
    ) -> Result<TaskAckV1, CloudError> {
        request.validate().map_err(|e| invalid(&e))?;
        let answer = self.client.post_idempotent(
            &self.path(&format!("/{number}/verify")),
            &encode(request)?,
            idempotency_key,
        )?;
        self.ack(answer, Some(number))
    }

    /// C0 2.2 E47 `PATCH …/{number}`: priority and labels at
    /// `base_version` (`409 stale_base` when another write came first).
    pub(super) fn fields(
        &self,
        number: u64,
        request: &TaskFieldsRequestV1,
    ) -> Result<TaskAckV1, CloudError> {
        request.validate().map_err(|e| invalid(&e))?;
        let answer = self
            .client
            .send(
                "PATCH",
                &self.path(&format!("/{number}")),
                Some(&encode(request)?),
                &[],
                lean_ctx_protocol::cloud::MAX_BODY_BYTES,
            )?
            .body;
        self.ack(answer, Some(number))
    }
}
