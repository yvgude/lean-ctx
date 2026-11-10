// SPDX-License-Identifier: Apache-2.0
//! The activity event: what one agent session did, in host-neutral terms.

use serde::{Deserialize, Serialize};

/// Longest message text kept (user or agent message, permission prompt,
/// error), in bytes; longer text is cut at a char boundary.
pub const MAX_TEXT_BYTES: usize = 2_000;
/// Longest target kept (path, redacted shell command, URL, query), in bytes.
pub const MAX_TARGET_BYTES: usize = 512;
/// Longest label kept (session id, agent, tool name), in bytes.
pub const MAX_LABEL_BYTES: usize = 128;
/// Longest project root kept, in bytes.
pub const MAX_ROOT_BYTES: usize = 1_024;

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The person's message to the agent (`text`).
    UserMessage,
    /// The agent's reply (`text`, bounded).
    AgentMessage,
    /// A tool call that is neither a file read/write nor a shell command.
    ToolCall,
    /// A file read (`target` = path).
    FileRead,
    /// A file write or edit (`target` = path).
    FileWrite,
    /// A shell command (`target` = the redacted command).
    Shell,
    /// A permission prompt (`outcome` asked | allowed | denied).
    Permission,
    /// A turn began (the person submitted a prompt).
    TurnStart,
    /// A turn ended (the agent stopped).
    TurnEnd,
    /// The agent or the host reported an error (`text`).
    Error,
}

impl Kind {
    pub const ALL: [Kind; 10] = [
        Kind::UserMessage,
        Kind::AgentMessage,
        Kind::ToolCall,
        Kind::FileRead,
        Kind::FileWrite,
        Kind::Shell,
        Kind::Permission,
        Kind::TurnStart,
        Kind::TurnEnd,
        Kind::Error,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::UserMessage => "user_message",
            Kind::AgentMessage => "agent_message",
            Kind::ToolCall => "tool_call",
            Kind::FileRead => "file_read",
            Kind::FileWrite => "file_write",
            Kind::Shell => "shell",
            Kind::Permission => "permission",
            Kind::TurnStart => "turn_start",
            Kind::TurnEnd => "turn_end",
            Kind::Error => "error",
        }
    }

    /// The kind named `name` (its contract spelling).
    pub fn parse(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.as_str() == name)
    }
}

/// Who reported the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A host hook (`lean-ctx hook observe`).
    Hook,
    /// A LeanCTX MCP tool call.
    Mcp,
    /// An ACP client callback (Hive Desktop driving the agent).
    Acp,
}

/// How a tool call, shell command or permission prompt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Error,
    Denied,
    Cancelled,
    /// A permission prompt was shown.
    Asked,
    /// A permission prompt was answered with yes.
    Allowed,
}

/// What a producer reports; the store adds `seq` and `at`, redacts and bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub session_id: String,
    pub agent: String,
    pub project_root: String,
    pub kind: Kind,
    pub source: Source,
    pub tool: Option<String>,
    pub target: Option<String>,
    pub outcome: Option<Outcome>,
    pub text: Option<String>,
}

impl Draft {
    pub fn new(
        session_id: impl Into<String>,
        agent: impl Into<String>,
        project_root: impl Into<String>,
        kind: Kind,
        source: Source,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            agent: agent.into(),
            project_root: project_root.into(),
            kind,
            source,
            tool: None,
            target: None,
            outcome: None,
            text: None,
        }
    }

    pub fn tool(mut self, tool: impl Into<String>) -> Self {
        self.tool = Some(tool.into());
        self
    }

    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    pub fn outcome(mut self, outcome: Outcome) -> Self {
        self.outcome = Some(outcome);
        self
    }

    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }
}

/// One stored event: one line of a session's log, and one item of
/// `activity.list` (Local Read v1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// 1-based, increasing within the session (gaps after a trim).
    pub seq: u64,
    /// RFC 3339 UTC with milliseconds.
    pub at: String,
    pub session_id: String,
    pub agent: String,
    pub project_root: String,
    pub kind: Kind,
    pub source: Source,
    pub tool: Option<String>,
    pub target: Option<String>,
    pub outcome: Option<Outcome>,
    pub text: Option<String>,
    /// Whether `text` or `target` was cut to its bound.
    pub truncated: bool,
}

/// `text` cut to `max` bytes at a char boundary; whether it was cut.
pub(crate) fn bound(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_owned(), false);
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}…", &text[..end]), true)
}

/// Empty or whitespace-only text counts as absent.
pub(crate) fn present(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.trim().is_empty())
}
