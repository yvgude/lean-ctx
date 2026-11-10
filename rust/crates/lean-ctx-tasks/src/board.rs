// SPDX-License-Identifier: Apache-2.0
//! The project's tasks in memory and every transition on them. Pure: the
//! time comes in, nothing is read or written here ([`crate::store`] does).

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use lean_ctx_protocol::UtcTimestamp;
use lean_ctx_protocol::hive::tasks::{
    AmendCaller, ClaimRefV1, EvidenceV1, EvidenceViewV1, MAX_VIEW_UPDATES, TASK_AMEND_SCHEMA,
    TASK_PROPOSE_SCHEMA, TASK_RELEASE_SCHEMA, TASK_RESULT_SCHEMA, TASK_SCHEMA, TASK_UPDATE_SCHEMA,
    TASK_VERIFY_SCHEMA, TaskAmendRequestV1, TaskClaimV1, TaskLifecycle, TaskMergeV1,
    TaskOriginKind, TaskOriginV1, TaskOutcome, TaskPriority, TaskProposeRequestV1,
    TaskReleaseRequestV1, TaskResultRequestV1, TaskResultV1, TaskState, TaskUpdateKind,
    TaskUpdateRequestV1, TaskUpdateV1, TaskV1, TaskVerdict, TaskVerificationV1,
    TaskVerifyRequestV1, UsageV1, amend_rule, may_amend, merge_evidence, validate_labels,
};

use crate::{
    Actor, Change, DEFAULT_CLAIM_MINUTES, LOCAL_VAULT, MAX_CLAIM_MINUTES, MIN_CLAIM_MINUTES,
    TaskError, TaskEvent, timestamp,
};

pub const BOARD_SCHEMA: &str = "leanctx.local-tasks/v1";
/// Tasks a project keeps; the oldest closed ones drop first.
pub const MAX_TASKS: usize = 2000;
/// Transitions a task keeps, newest last.
pub const MAX_TRANSITIONS: usize = 50;

/// One transition: who moved the task from which state to which, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    pub seq: u64,
    pub number: u64,
    pub event: TaskEvent,
    pub from: TaskState,
    pub to: TaskState,
    pub by: lean_ctx_protocol::hive::tasks::TaskActorV1,
    pub at: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The worker of a submitted result (its notice is not sent to it), so
    /// a notice re-sent from the journal reads as the first one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
}

impl From<&Transition> for Change {
    fn from(t: &Transition) -> Self {
        Self {
            seq: t.seq,
            number: t.number,
            event: t.event,
            state: t.to,
            worker: t.worker.clone(),
        }
    }
}

/// Where a local task was brought into a Hive vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedRef {
    /// The vault address `ws/vault`.
    pub vault: String,
    /// The task's number in that vault.
    pub number: u64,
    pub at: UtcTimestamp,
}

/// A local task: Hive's task view plus its transitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalTask {
    pub task: TaskV1,
    /// Newest last, at most [`MAX_TRANSITIONS`].
    #[serde(default)]
    pub transitions: Vec<Transition>,
    /// The length of the current claim (its renewals keep it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_minutes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<SharedRef>,
}

/// All tasks of one project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    pub schema: String,
    /// The number the next task gets.
    pub next_number: u64,
    /// The last journal sequence written.
    pub seq: u64,
    #[serde(default)]
    pub tasks: BTreeMap<u64, LocalTask>,
    /// Transitions of the write in progress (journaled by the store).
    #[serde(skip)]
    pub(crate) pending: Vec<Transition>,
    /// Their changes, for the caller's notices.
    #[serde(skip)]
    pub(crate) changes: Vec<Change>,
}

impl Default for Board {
    fn default() -> Self {
        Self {
            schema: BOARD_SCHEMA.to_owned(),
            next_number: 1,
            seq: 0,
            tasks: BTreeMap::new(),
            pending: Vec::new(),
            changes: Vec::new(),
        }
    }
}

/// What a proposal says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Proposal {
    pub title: String,
    pub goal: Option<String>,
    pub acceptance: String,
    pub role: Option<String>,
}

/// What a result says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    pub summary: String,
    pub evidence: Vec<EvidenceV1>,
    pub usage: Option<UsageV1>,
    pub outcome: TaskOutcome,
}

/// What an amendment of a result says: on a submitted task the whole new
/// evidence list and a new summary or none (the summary stays); on a done
/// task only `merge_commit` (the merge record).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Amendment {
    pub summary: Option<String>,
    pub evidence: Vec<EvidenceV1>,
    pub merge_commit: Option<String>,
    pub reason: Option<String>,
    /// The caller checked that this process runs for a registered Queen
    /// session (`lean_ctx_hive::queens::this_session_is_queen`): it may
    /// record a merge like a person.
    pub queen: bool,
}

fn closed(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Done
            | TaskState::Rejected
            | TaskState::Cancelled
            | TaskState::Failed
            | TaskState::Unknown
    )
}

/// The claim reference the Hive request checks need (local claims have no
/// lease; the holder is the claim).
fn local_claim() -> ClaimRefV1 {
    ClaimRefV1 {
        lease_id: "local".to_owned(),
        fencing_token: 1,
    }
}

fn invalid(error: impl std::fmt::Display) -> TaskError {
    TaskError::Invalid(error.to_string())
}

fn check_reason(reason: Option<&str>) -> Result<Option<String>, TaskError> {
    let reason = reason.map(str::trim).filter(|r| !r.is_empty());
    TaskReleaseRequestV1 {
        schema: TASK_RELEASE_SCHEMA.to_owned(),
        claim: local_claim(),
        reason: reason.map(str::to_owned),
    }
    .validate()
    .map_err(invalid)?;
    Ok(reason.map(str::to_owned))
}

fn lifecycle(task: &TaskV1) -> Option<TaskLifecycle> {
    Some(match task.state {
        TaskState::Open => TaskLifecycle::Queued,
        TaskState::Claimed => {
            let since = task.claim.as_ref().map(|c| c.since.as_str());
            let reported = task.updates.iter().any(|u| {
                u.kind != Some(TaskUpdateKind::Verification) && Some(u.at.as_str()) >= since
            });
            if reported {
                TaskLifecycle::Running
            } else {
                TaskLifecycle::Claimed
            }
        }
        TaskState::Submitted => TaskLifecycle::Submitted,
        TaskState::Done => TaskLifecycle::Completed,
        TaskState::Failed => TaskLifecycle::Failed,
        TaskState::Cancelled => TaskLifecycle::Cancelled,
        TaskState::Proposed | TaskState::Rejected | TaskState::Unknown => return None,
    })
}

fn push_update(task: &mut TaskV1, update: TaskUpdateV1) {
    task.updates.push(update);
    let excess = task.updates.len().saturating_sub(MAX_VIEW_UPDATES);
    task.updates.drain(..excess);
}

impl Board {
    fn task_mut(&mut self, number: u64) -> Result<&mut LocalTask, TaskError> {
        self.tasks
            .get_mut(&number)
            .ok_or(TaskError::NotFound(number))
    }

    /// Record a transition of `number` (already applied to the task).
    fn record(
        &mut self,
        number: u64,
        event: TaskEvent,
        from: TaskState,
        by: &Actor,
        at: DateTime<Utc>,
        reason: Option<String>,
    ) -> Change {
        self.seq += 1;
        let seq = self.seq;
        let transition = Transition {
            seq,
            number,
            event,
            from,
            to: TaskState::Unknown,
            by: by.view(),
            at: timestamp(at),
            reason,
            worker: None,
        };
        let mut change = Change {
            seq,
            number,
            event,
            state: TaskState::Unknown,
            worker: None,
        };
        if let Some(local) = self.tasks.get_mut(&number) {
            local.task.lifecycle = lifecycle(&local.task);
            let mut transition = transition;
            transition.to = local.task.state;
            change.state = local.task.state;
            if event == TaskEvent::Submitted {
                change.worker = local.task.result.as_ref().and_then(|r| r.by.agent.clone());
                transition.worker.clone_from(&change.worker);
            }
            local.transitions.push(transition.clone());
            let excess = local.transitions.len().saturating_sub(MAX_TRANSITIONS);
            local.transitions.drain(..excess);
            self.pending.push(transition);
            self.changes.push(change.clone());
        }
        change
    }

    /// Open claims whose time ran out open their task again.
    pub fn expire(&mut self, now: DateTime<Utc>) -> Vec<Change> {
        let now_text = timestamp(now);
        let due: Vec<(u64, String)> = self
            .tasks
            .iter()
            .filter_map(|(n, local)| {
                let claim = local.task.claim.as_ref()?;
                (local.task.state == TaskState::Claimed && claim.until.as_str() < now_text.as_str())
                    .then(|| (*n, claim.holder.agent.clone().unwrap_or_default()))
            })
            .collect();
        due.into_iter()
            .map(|(number, holder)| {
                if let Some(local) = self.tasks.get_mut(&number) {
                    local.task.state = TaskState::Open;
                    local.task.claim = None;
                    local.claim_minutes = None;
                }
                self.record(
                    number,
                    TaskEvent::Released,
                    TaskState::Claimed,
                    &Actor::agent(holder),
                    now,
                    Some("claim expired".to_owned()),
                )
            })
            .collect()
    }

    /// Whether a claim ran out by `now` (a read shows it open).
    pub fn has_expired(&self, now: DateTime<Utc>) -> bool {
        let now = timestamp(now);
        self.tasks.values().any(|local| {
            local.task.state == TaskState::Claimed
                && local
                    .task
                    .claim
                    .as_ref()
                    .is_some_and(|c| c.until.as_str() < now.as_str())
        })
    }

    /// Propose a task (state `proposed`); anyone may.
    pub fn propose(
        &mut self,
        by: &Actor,
        proposal: Proposal,
        now: DateTime<Utc>,
    ) -> Result<Change, TaskError> {
        let clean = |s: Option<String>| s.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
        let request = TaskProposeRequestV1 {
            schema: TASK_PROPOSE_SCHEMA.to_owned(),
            title: Some(proposal.title.trim().to_owned()),
            goal: clean(proposal.goal),
            acceptance: Some(proposal.acceptance.trim().to_owned()),
            role: clean(proposal.role),
            origin: TaskOriginV1 {
                kind: TaskOriginKind::Agent,
                facts: Vec::new(),
                message: None,
            },
            session_ref: "local".to_owned(),
        };
        request.validate().map_err(invalid)?;
        self.make_room()?;
        let number = self.next_number;
        self.next_number += 1;
        let origin_kind = match by {
            Actor::Agent { .. } => TaskOriginKind::Agent,
            Actor::Person { .. } => TaskOriginKind::Person,
        };
        let task = TaskV1 {
            schema: TASK_SCHEMA.to_owned(),
            vault_id: LOCAL_VAULT.to_owned(),
            number,
            state: TaskState::Proposed,
            lifecycle: None,
            title: request.title,
            goal: request.goal,
            acceptance: request.acceptance,
            role: request.role,
            origin: TaskOriginV1 {
                kind: origin_kind,
                facts: Vec::new(),
                message: None,
            },
            created_by: by.view(),
            created_at: timestamp(now),
            approved_by: None,
            claim: None,
            updates: Vec::new(),
            result: None,
            decision: None,
            verified_by: None,
            verified_at: None,
            verification: None,
            priority: None,
            labels: Vec::new(),
            fields_version: None,
            process: None,
        };
        self.tasks.insert(
            number,
            LocalTask {
                task,
                transitions: Vec::new(),
                claim_minutes: None,
                shared: None,
            },
        );
        Ok(self.record(
            number,
            TaskEvent::Proposed,
            TaskState::Proposed,
            by,
            now,
            None,
        ))
    }

    /// Keep at most [`MAX_TASKS`]: drop the oldest closed tasks, refuse
    /// when every task is still unfinished.
    fn make_room(&mut self) -> Result<(), TaskError> {
        while self.tasks.len() >= MAX_TASKS {
            let oldest = self
                .tasks
                .iter()
                .find(|(_, local)| closed(local.task.state))
                .map(|(n, _)| *n);
            match oldest {
                Some(n) => {
                    self.tasks.remove(&n);
                }
                None => {
                    return Err(TaskError::InvalidState {
                        number: self.next_number,
                        reason: "too_many_tasks",
                    });
                }
            }
        }
        Ok(())
    }

    /// Accept (`open`) or decline (`rejected`) a proposal; anyone may.
    pub fn decide(
        &mut self,
        number: u64,
        by: &Actor,
        accept: bool,
        reason: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Change, TaskError> {
        let reason = check_reason(reason)?;
        let local = self.task_mut(number)?;
        if local.task.state != TaskState::Proposed {
            return Err(TaskError::InvalidState {
                number,
                reason: if closed(local.task.state) {
                    "task_closed"
                } else {
                    "not_proposed"
                },
            });
        }
        let event = if accept {
            local.task.state = TaskState::Open;
            local.task.approved_by = Some(by.view());
            TaskEvent::Accepted
        } else {
            local.task.state = TaskState::Rejected;
            TaskEvent::Declined
        };
        Ok(self.record(number, event, TaskState::Proposed, by, now, reason))
    }

    /// Claim an open task, or renew the caller's own claim. Only agents
    /// claim. `None` when the call only renewed (no transition).
    pub fn claim(
        &mut self,
        number: u64,
        by: &Actor,
        minutes: Option<u32>,
        now: DateTime<Utc>,
    ) -> Result<Option<Change>, TaskError> {
        if by.agent_id().is_none() {
            return Err(TaskError::Forbidden {
                number,
                reason: "agents_claim",
            });
        }
        let minutes = minutes.unwrap_or(DEFAULT_CLAIM_MINUTES);
        if !(MIN_CLAIM_MINUTES..=MAX_CLAIM_MINUTES).contains(&minutes) {
            return Err(TaskError::Invalid(format!(
                "minutes must be {MIN_CLAIM_MINUTES}-{MAX_CLAIM_MINUTES}"
            )));
        }
        let local = self.task_mut(number)?;
        let until = timestamp(now + Duration::minutes(i64::from(minutes)));
        match local.task.state {
            TaskState::Open => {}
            TaskState::Claimed => {
                let claim = local.task.claim.as_mut().ok_or(TaskError::InvalidState {
                    number,
                    reason: "not_claimed",
                })?;
                if !by.is(&claim.holder) {
                    return Err(TaskError::InvalidState {
                        number,
                        reason: "task_claimed",
                    });
                }
                claim.until = until;
                local.claim_minutes = Some(minutes);
                return Ok(None);
            }
            state if closed(state) => {
                return Err(TaskError::InvalidState {
                    number,
                    reason: "task_closed",
                });
            }
            _ => {
                return Err(TaskError::InvalidState {
                    number,
                    reason: "task_not_open",
                });
            }
        }
        local.task.state = TaskState::Claimed;
        local.task.claim = Some(TaskClaimV1 {
            holder: by.view(),
            since: timestamp(now),
            until,
        });
        local.claim_minutes = Some(minutes);
        Ok(Some(self.record(
            number,
            TaskEvent::Claimed,
            TaskState::Open,
            by,
            now,
            None,
        )))
    }

    /// The task, claimed by `by`.
    fn held(&mut self, number: u64, by: &Actor) -> Result<&mut LocalTask, TaskError> {
        let local = self.task_mut(number)?;
        match local.task.state {
            TaskState::Claimed => {}
            state if closed(state) => {
                return Err(TaskError::InvalidState {
                    number,
                    reason: "task_closed",
                });
            }
            _ => {
                return Err(TaskError::InvalidState {
                    number,
                    reason: "not_claimed",
                });
            }
        }
        let holder = local.task.claim.as_ref().map(|c| &c.holder);
        if by.agent_id().is_none() || !holder.is_some_and(|h| by.is(h)) {
            return Err(TaskError::Forbidden {
                number,
                reason: "not_holder",
            });
        }
        Ok(local)
    }

    /// The holder reports one line of progress; renews the claim.
    pub fn update(
        &mut self,
        number: u64,
        by: &Actor,
        summary: &str,
        usage: Option<UsageV1>,
        now: DateTime<Utc>,
    ) -> Result<Change, TaskError> {
        TaskUpdateRequestV1 {
            schema: TASK_UPDATE_SCHEMA.to_owned(),
            claim: local_claim(),
            summary: Some(summary.trim().to_owned()),
            usage,
        }
        .validate()
        .map_err(invalid)?;
        let local = self.held(number, by)?;
        let minutes = local.claim_minutes.unwrap_or(DEFAULT_CLAIM_MINUTES);
        if let Some(claim) = local.task.claim.as_mut() {
            claim.until = timestamp(now + Duration::minutes(i64::from(minutes)));
        }
        push_update(
            &mut local.task,
            TaskUpdateV1 {
                by: by.view(),
                at: timestamp(now),
                summary: Some(summary.trim().to_owned()),
                usage,
                kind: Some(TaskUpdateKind::Progress),
                verdict: None,
                amended_from: Vec::new(),
                amended_to: Vec::new(),
            },
        );
        Ok(self.record(
            number,
            TaskEvent::Updated,
            TaskState::Claimed,
            by,
            now,
            None,
        ))
    }

    /// The holder's result: `submitted` (waits for a check), or `failed`.
    pub fn submit(
        &mut self,
        number: u64,
        by: &Actor,
        submission: Submission,
        now: DateTime<Utc>,
    ) -> Result<Change, TaskError> {
        let summary = submission.summary.trim().to_owned();
        TaskResultRequestV1 {
            schema: TASK_RESULT_SCHEMA.to_owned(),
            claim: local_claim(),
            summary: Some(summary.clone()),
            evidence: submission.evidence.clone(),
            draft: None,
            usage: submission.usage,
            outcome: Some(submission.outcome),
            gate_record: None,
        }
        .validate()
        .map_err(invalid)?;
        let local = self.held(number, by)?;
        local.task.claim = None;
        local.claim_minutes = None;
        local.task.result = Some(TaskResultV1 {
            by: by.view(),
            at: timestamp(now),
            proposal: 0,
            seq: None,
            summary: Some(summary),
            evidence: submission
                .evidence
                .into_iter()
                .map(|e| EvidenceViewV1 {
                    kind: e.kind,
                    reference: e.reference,
                    anchor: e.anchor,
                    verified: false,
                })
                .collect(),
            usage: submission.usage,
            merge: None,
        });
        let event = match submission.outcome {
            TaskOutcome::Completed => {
                local.task.state = TaskState::Submitted;
                TaskEvent::Submitted
            }
            TaskOutcome::Failed => {
                local.task.state = TaskState::Failed;
                TaskEvent::Failed
            }
        };
        Ok(self.record(number, event, TaskState::Claimed, by, now, None))
    }

    /// A result is corrected without a claim (a submit ended it), by
    /// [`amend_rule`] and [`may_amend`]: on `submitted` the submitter
    /// replaces summary and evidence; on `done` the submitter, a person or a
    /// Queen session (never the reviewer)
    /// records the merge once. The update `kind: amendment` keeps old → new
    /// (`amended_from`, `amended_to`). Refused before anything changes.
    pub fn amend(
        &mut self,
        number: u64,
        by: &Actor,
        amendment: Amendment,
        now: DateTime<Utc>,
    ) -> Result<Change, TaskError> {
        let reason = check_reason(amendment.reason.as_deref())?;
        let summary = amendment
            .summary
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        let request = TaskAmendRequestV1 {
            schema: TASK_AMEND_SCHEMA.to_owned(),
            worker_id: by.agent_id().unwrap_or("person").to_owned(),
            summary: summary.clone(),
            evidence: amendment.evidence,
            merge_commit: amendment.merge_commit,
            reason: reason.clone(),
        };
        request.validate().map_err(invalid)?;
        let local = self.task_mut(number)?;
        let caller = AmendCaller {
            // Only an agent submits; a person is never the submitter.
            submitter: by.agent_id().is_some()
                && local.task.result.as_ref().is_some_and(|r| by.is(&r.by)),
            reviewer: local
                .task
                .verified_by
                .as_ref()
                .is_some_and(|reviewer| by.is_exactly(reviewer)),
            person: by.agent_id().is_none(),
            queen: amendment.queen,
        };
        may_amend(&request, caller).map_err(|refusal| TaskError::Forbidden {
            number,
            reason: refusal.code(),
        })?;
        amend_rule(&local.task, &request).map_err(|refusal| TaskError::InvalidState {
            number,
            reason: refusal.code(),
        })?;
        let state = local.task.state;
        let Some(result) = local.task.result.as_mut() else {
            return Err(TaskError::InvalidState {
                number,
                reason: "not_amendable",
            });
        };
        let view = |e: EvidenceV1| EvidenceViewV1 {
            kind: e.kind,
            reference: e.reference,
            anchor: e.anchor,
            verified: false,
        };
        let (before, after) = if let Some(commit) = request.merge_commit {
            result.merge = Some(TaskMergeV1 {
                commit: commit.clone(),
                by: by.view(),
                at: timestamp(now),
            });
            (result.evidence.clone(), vec![view(merge_evidence(&commit))])
        } else {
            // An item kept from before keeps its view (and `verified`).
            let after: Vec<EvidenceViewV1> = request
                .evidence
                .into_iter()
                .map(|e| {
                    result
                        .evidence
                        .iter()
                        .find(|v| {
                            v.kind == e.kind && v.reference == e.reference && v.anchor == e.anchor
                        })
                        .cloned()
                        .unwrap_or_else(|| view(e))
                })
                .collect();
            if summary.is_some() {
                result.summary.clone_from(&summary);
            }
            (
                std::mem::replace(&mut result.evidence, after.clone()),
                after,
            )
        };
        push_update(
            &mut local.task,
            TaskUpdateV1 {
                by: by.view(),
                at: timestamp(now),
                summary: summary.or(reason.clone()),
                usage: None,
                kind: Some(TaskUpdateKind::Amendment),
                verdict: None,
                amended_from: before,
                amended_to: after,
            },
        );
        Ok(self.record(number, TaskEvent::Amended, state, by, now, reason))
    }

    /// Give a claimed task back (`open`): its holder, or a person.
    pub fn release(
        &mut self,
        number: u64,
        by: &Actor,
        reason: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Change, TaskError> {
        let reason = check_reason(reason)?;
        let local = if by.agent_id().is_none() {
            let local = self.task_mut(number)?;
            if local.task.state != TaskState::Claimed {
                return Err(TaskError::InvalidState {
                    number,
                    reason: if closed(local.task.state) {
                        "task_closed"
                    } else {
                        "not_claimed"
                    },
                });
            }
            local
        } else {
            self.held(number, by)?
        };
        local.task.state = TaskState::Open;
        local.task.claim = None;
        local.claim_minutes = None;
        Ok(self.record(
            number,
            TaskEvent::Released,
            TaskState::Claimed,
            by,
            now,
            reason,
        ))
    }

    /// A check of a submitted result: `accept` → `done`, `reject` (with a
    /// reason) → `open`. Never by the agent that submitted it.
    pub fn verify(
        &mut self,
        number: u64,
        by: &Actor,
        verdict: TaskVerdict,
        reason: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Change, TaskError> {
        let reason = reason.map(str::trim).filter(|r| !r.is_empty());
        TaskVerifyRequestV1 {
            schema: TASK_VERIFY_SCHEMA.to_owned(),
            worker_id: by.agent_id().unwrap_or("person").to_owned(),
            verdict,
            reason: reason.map(str::to_owned),
        }
        .validate()
        .map_err(invalid)?;
        let local = self.task_mut(number)?;
        if local.task.state != TaskState::Submitted {
            return Err(TaskError::InvalidState {
                number,
                reason: if closed(local.task.state) {
                    "task_closed"
                } else {
                    "not_submitted"
                },
            });
        }
        let worker = local.task.result.as_ref().and_then(|r| r.by.agent.clone());
        if let Some(agent) = by.agent_id()
            && worker.as_deref() == Some(agent)
        {
            return Err(TaskError::Forbidden {
                number,
                reason: "self_verify",
            });
        }
        let at = timestamp(now);
        local.task.verification = Some(TaskVerificationV1 {
            verdict,
            by: by.view(),
            at: at.clone(),
            reason: reason.map(str::to_owned),
        });
        push_update(
            &mut local.task,
            TaskUpdateV1 {
                by: by.view(),
                at: at.clone(),
                summary: reason.map(str::to_owned),
                usage: None,
                kind: Some(TaskUpdateKind::Verification),
                verdict: Some(verdict),
                amended_from: Vec::new(),
                amended_to: Vec::new(),
            },
        );
        let event = if verdict == TaskVerdict::Accept {
            local.task.state = TaskState::Done;
            local.task.verified_by = Some(by.view());
            local.task.verified_at = Some(at);
            TaskEvent::Verified
        } else {
            local.task.state = TaskState::Open;
            TaskEvent::Rejected
        };
        Ok(self.record(
            number,
            event,
            TaskState::Submitted,
            by,
            now,
            reason.map(str::to_owned),
        ))
    }

    /// Stop a proposed, open or claimed task, with a reason: a person only
    /// (an agent is refused `agents_stop`). The task ends in the
    /// `cancelled` state (Hive's state name).
    pub fn stop(
        &mut self,
        number: u64,
        by: &Actor,
        reason: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Change, TaskError> {
        let reason = check_reason(reason)?;
        if reason.is_none() {
            return Err(TaskError::Invalid(
                "stopping a task needs a reason".to_owned(),
            ));
        }
        if by.agent_id().is_some() {
            return Err(TaskError::Forbidden {
                number,
                reason: "agents_stop",
            });
        }
        let local = self.task_mut(number)?;
        let from = local.task.state;
        if !matches!(
            from,
            TaskState::Proposed | TaskState::Open | TaskState::Claimed
        ) {
            return Err(TaskError::InvalidState {
                number,
                reason: if closed(from) {
                    "task_closed"
                } else {
                    "task_submitted"
                },
            });
        }
        local.task.state = TaskState::Cancelled;
        local.task.claim = None;
        local.claim_minutes = None;
        Ok(self.record(number, TaskEvent::Stopped, from, by, now, reason))
    }

    /// Mark a task as brought into a Hive vault (journaled; no notice).
    pub fn mark_shared(
        &mut self,
        number: u64,
        by: &Actor,
        vault: &str,
        vault_number: u64,
        now: DateTime<Utc>,
    ) -> Result<Change, TaskError> {
        if vault.trim().is_empty() || vault_number == 0 {
            return Err(TaskError::Invalid(
                "a vault address and its task number are required".to_owned(),
            ));
        }
        let local = self.task_mut(number)?;
        if local.shared.is_some() {
            return Err(TaskError::InvalidState {
                number,
                reason: "already_shared",
            });
        }
        local.shared = Some(SharedRef {
            vault: vault.trim().to_owned(),
            number: vault_number,
            at: timestamp(now),
        });
        let state = local.task.state;
        Ok(self.record(
            number,
            TaskEvent::Shared,
            state,
            by,
            now,
            Some(format!("{}#{vault_number}", vault.trim())),
        ))
    }

    /// Set a task's priority and/or labels (C0 2.2 E47 on this machine):
    /// `fields_version` counts every such write. Who may do this is the
    /// caller's rule (a person or a registered Queen session); a closed
    /// task keeps its fields (`task_closed`).
    pub fn set_fields(
        &mut self,
        number: u64,
        by: &Actor,
        priority: Option<TaskPriority>,
        labels: Option<Vec<String>>,
        now: DateTime<Utc>,
    ) -> Result<Change, TaskError> {
        if priority.is_none() && labels.is_none() {
            return Err(TaskError::Invalid(
                "set a priority, labels or both".to_owned(),
            ));
        }
        if let Some(labels) = &labels {
            validate_labels(labels).map_err(|e| TaskError::Invalid(format!("labels: {e}")))?;
        }
        let local = self.task_mut(number)?;
        let state = local.task.state;
        if closed(state) {
            return Err(TaskError::InvalidState {
                number,
                reason: "task_closed",
            });
        }
        if let Some(priority) = priority {
            local.task.priority = Some(priority);
        }
        if let Some(labels) = labels {
            local.task.labels = labels;
        }
        local.task.fields_version = Some(local.task.fields_version.unwrap_or(0) + 1);
        let reason = format!(
            "priority {}, labels {}",
            match local.task.priority.unwrap_or_default() {
                TaskPriority::High => "high",
                TaskPriority::Normal => "normal",
            },
            if local.task.labels.is_empty() {
                "none".to_owned()
            } else {
                local.task.labels.join(",")
            }
        );
        Ok(self.record(number, TaskEvent::Fields, state, by, now, Some(reason)))
    }
}
