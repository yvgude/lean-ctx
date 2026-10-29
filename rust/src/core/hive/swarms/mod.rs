// SPDX-License-Identifier: Apache-2.0
//! Private swarms, engine side (SPEC-ENGINE §3.13, §3.14; CONTRACT §3.20;
//! C0 2.0 `routes/swarm_*`, `routes/pledge*`, `enums/pledge-limits.json`,
//! `enums/goal-node-states.json`; RESOLUTIONS R107).
//!
//! A swarm belongs to a workspace; its participants are members and invited
//! users. Their agents work as ordinary host sessions: every swarm route
//! carries the instance token plus the participant's swarm `worker_id`.
//! The 1.x subtask client stays in [`super::swarm`].

pub mod client;
pub mod local;
pub mod participate;
pub mod pledge;
pub mod pledge_record;
pub mod render;
pub mod result;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One node of the goal tree (E22).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalNode {
    pub id: String,
    pub kind: GoalKind,
    pub parent: Option<String>,
    /// The swarm-log node behind it (`n_…`).
    pub node: String,
    pub state: GoalState,
    pub title: String,
    pub assignee: Option<String>,
    #[serde(default)]
    pub attempts: u32,
    pub lock_until: Option<u64>,
    #[serde(default)]
    pub results: Vec<String>,
    #[serde(default)]
    pub review: Option<Value>,
    #[serde(default)]
    pub criterion_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalKind {
    Root,
    Subgoal,
    Task,
    Synthesis,
}

impl GoalKind {
    /// `claimable_kinds` of `enums/goal-node-states.json`.
    pub fn claimable(self) -> bool {
        matches!(self, GoalKind::Task | GoalKind::Synthesis)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            GoalKind::Root => "root",
            GoalKind::Subgoal => "subgoal",
            GoalKind::Task => "task",
            GoalKind::Synthesis => "synthesis",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalState {
    Open,
    Claimed,
    Working,
    InReview,
    Done,
    Blocked,
    Dropped,
}

impl GoalState {
    pub fn as_str(self) -> &'static str {
        match self {
            GoalState::Open => "open",
            GoalState::Claimed => "claimed",
            GoalState::Working => "working",
            GoalState::InReview => "in_review",
            GoalState::Done => "done",
            GoalState::Blocked => "blocked",
            GoalState::Dropped => "dropped",
        }
    }
}

/// `enums/swarm-states.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmState {
    Open,
    Running,
    Paused,
    Done,
    Archived,
}

impl SwarmState {
    pub fn as_str(self) -> &'static str {
        match self {
            SwarmState::Open => "open",
            SwarmState::Running => "running",
            SwarmState::Paused => "paused",
            SwarmState::Done => "done",
            SwarmState::Archived => "archived",
        }
    }
}

/// The swarm as E22 returns it. Fields the engine does not read stay out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmInfo {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub goal: String,
    pub state: SwarmState,
    pub coordinator: Option<String>,
    #[serde(default)]
    pub conductor_workers: Vec<String>,
    #[serde(default)]
    pub delegates: Vec<String>,
    pub participant_task_cap: u32,
    pub max_workers_effective: u32,
}

/// E22 `GET /v1/swarms/{id}/goal`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    pub swarm: SwarmInfo,
    pub nodes: Vec<GoalNode>,
    pub epoch: u64,
}

impl Goal {
    pub fn node(&self, id: &str) -> Option<&GoalNode> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

/// E25 claim answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimGrant {
    pub claim_id: String,
    pub fencing_token: u64,
    pub expires_at: u64,
}

/// `g_` + 26 base32: a goal node id.
pub fn is_goal_id(value: &str) -> bool {
    prefixed_id(value, "g_")
}

/// `cl_` + 26 base32: a claim id.
pub fn is_claim_id(value: &str) -> bool {
    prefixed_id(value, "cl_")
}

fn prefixed_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|rest| {
        rest.len() == 26
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    })
}

/// A swarm id is a UUID (lowercase hex).
pub fn is_swarm_id(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    parts.len() == 5
        && parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12])
        && parts.iter().all(|p| {
            p.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

/// Windows on ARM is refused for swarm work (RESOLUTIONS R106).
pub fn platform_supported() -> bool {
    !cfg!(all(windows, target_arch = "aarch64")) && !emulated_on_windows_arm()
}

#[cfg(windows)]
fn emulated_on_windows_arm() -> bool {
    // An x64 build under emulation reports the native machine through
    // PROCESSOR_ARCHITEW6432 / PROCESSOR_ARCHITECTURE.
    ["PROCESSOR_ARCHITEW6432", "PROCESSOR_ARCHITECTURE"]
        .iter()
        .any(|k| std::env::var(k).is_ok_and(|v| v.eq_ignore_ascii_case("ARM64")))
}

#[cfg(not(windows))]
fn emulated_on_windows_arm() -> bool {
    false
}

#[cfg(test)]
mod tests;
