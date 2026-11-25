use lean_ctx_time::UtcDateTime;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::core::a2a::message::{MessagePriority, PrivacyLevel};
use crate::ipc::process::ProcessIdentity;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRegistry {
    pub agents: Vec<AgentEntry>,
    pub scratchpad: Vec<ScratchpadEntry>,
    #[serde(default)]
    pub logical_sessions: Vec<LogicalSessionPresence>,
    #[serde(default)]
    pub logical_session_telemetry_seen: bool,
    pub updated_at: UtcDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LogicalSessionPresence {
    pub source: String,
    pub workspace: String,
    pub session_id: String,
    pub opened_at: UtcDateTime,
    pub last_heartbeat: UtcDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    pub agent_id: String,
    pub agent_type: String,
    pub role: Option<String>,
    pub project_root: String,
    pub started_at: UtcDateTime,
    pub last_active: UtcDateTime,
    pub pid: u32,
    /// PID alone is not an identity: operating systems reuse it. The immutable
    /// process start marker prevents a retired MCP record from becoming "live"
    /// again when an unrelated process receives the same PID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_identity: Option<ProcessIdentity>,
    /// Optional explicit binding to the durable identity registry. Generated
    /// presence IDs remain independent and unchanged for legacy callers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_identity_id: Option<String>,
    pub status: AgentStatus,
    pub status_message: Option<String>,
    /// The status line the agent set itself (`ctx_agent action=status`,
    /// [`AgentRegistry::set_status_by_agent`]). LeanCTX's own lifecycle
    /// notes ("connection closed", lease and transport states) are written
    /// to `status_message` only, so [`AgentEntry::agent_status_line`] tells
    /// the two apart without matching their text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_status_message: Option<String>,
    /// Present only on the presence an MCP server registers for its own
    /// connection. A transport stays listed while its process lives; worker
    /// leases (`ctx_agent`) expire independently of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<TransportClient>,
    /// Whether `role` was stated by the agent or its environment, or filled
    /// in by LeanCTX (placeholder, client-name heuristic, `coder` default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_source: Option<RoleSource>,
    /// The presence this one was registered next to inside the same MCP
    /// process (a subagent shares its parent's connection): it has its own
    /// id, so its reads never consume the parent's directed messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The launching app's session id (`LEAN_CTX_SESSION_ID`) of the
    /// process that registered this presence, so the app maps presences to
    /// its sessions without walking the process tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_session_id: Option<String>,
    /// The name people see for this presence: `ctx_agent register`'s
    /// `name`, else the launching app's session name
    /// (`LEAN_CTX_SESSION_NAME`). Display only, never an address or a role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

impl AgentEntry {
    /// Whether this is only a LeanCTX MCP server's own presence: the
    /// `context-engine` placeholder no agent stated. It is local plumbing,
    /// never a team agent or a Cloud worker.
    pub fn is_engine_placeholder(&self) -> bool {
        self.role.as_deref() == Some(ENGINE_PLACEHOLDER_ROLE)
            && self.role_source != Some(RoleSource::Declared)
    }

    /// The current status line when the agent wrote it itself, else none:
    /// a later lifecycle note from LeanCTX (or a cleared status) replaces
    /// `status_message` without touching `agent_status_message`, so the
    /// two differ and nothing is returned.
    pub fn agent_status_line(&self) -> Option<&str> {
        match (&self.status_message, &self.agent_status_message) {
            (Some(current), Some(own)) if current == own => Some(current.as_str()),
            _ => None,
        }
    }
}

/// The host behind an MCP transport, from the `initialize` handshake.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransportClient {
    /// Identified host (`claude-code`, `codex`, `vscode`, …; `unknown` until
    /// the handshake arrives or when the name is not recognised).
    pub host: String,
    /// Raw `clientInfo.name`, bounded and stripped of control characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Raw `clientInfo.version`, bounded and stripped of control characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl TransportClient {
    pub(crate) const UNKNOWN_HOST: &'static str = "unknown";

    /// The transport of a process that has not seen its handshake yet.
    pub(crate) fn pending() -> Self {
        Self {
            host: Self::UNKNOWN_HOST.to_string(),
            name: None,
            version: None,
        }
    }

    /// The transport for this process: its recorded MCP handshake, if any.
    pub fn current() -> Self {
        crate::core::client_capabilities::handshake().map_or_else(Self::pending, |handshake| Self {
            host: handshake.host,
            name: Some(handshake.name),
            version: handshake.version,
        })
    }

    pub(crate) fn is_identified(&self) -> bool {
        self.host != Self::UNKNOWN_HOST || self.name.is_some()
    }
}

/// The role an MCP server registers before any agent states one.
pub const ENGINE_PLACEHOLDER_ROLE: &str = "context-engine";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RoleSource {
    /// Stated by the agent (`ctx_agent register`) or `LEAN_CTX_ROLE`.
    Declared,
    /// Filled in by LeanCTX: placeholder, client-name heuristic or default.
    Assumed,
}

impl RoleSource {
    pub fn as_str(self) -> &'static str {
        match self {
            RoleSource::Declared => "declared",
            RoleSource::Assumed => "assumed",
        }
    }
}

/// The session role stated through the environment (`LEAN_CTX_ROLE`, then
/// `LEAN_CTX_AGENT_ROLE`). Anything else an MCP session gets is assumed.
pub fn declared_session_role() -> Option<String> {
    stated_roles(
        std::env::var("LEAN_CTX_ROLE").ok().as_deref(),
        std::env::var("LEAN_CTX_AGENT_ROLE").ok().as_deref(),
    )
    .session
}

/// The permission role stated through the environment: `LEAN_CTX_ROLE`
/// only. `LEAN_CTX_AGENT_ROLE` names the agent on the bus (a host such as
/// LeanCTX Code sets it per worker) and never chooses what tools may do.
pub fn declared_permission_role() -> Option<String> {
    stated_roles(std::env::var("LEAN_CTX_ROLE").ok().as_deref(), None).permission
}

/// What `LEAN_CTX_ROLE` and `LEAN_CTX_AGENT_ROLE` state, blank values unset.
#[derive(Debug, PartialEq, Eq)]
pub struct StatedRoles {
    /// The role on the bus and in presence.
    pub session: Option<String>,
    /// The role whose permissions apply.
    pub permission: Option<String>,
}

pub fn stated_roles(role: Option<&str>, agent_role: Option<&str>) -> StatedRoles {
    let clean = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let permission = clean(role);
    StatedRoles {
        session: permission.clone().or_else(|| clean(agent_role)),
        permission,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AgentStatus {
    Active,
    Idle,
    Finished,
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentStatus::Active => write!(f, "active"),
            AgentStatus::Idle => write!(f, "idle"),
            AgentStatus::Finished => write!(f, "finished"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScratchpadEntry {
    pub id: String,
    pub from_agent: String,
    pub to_agent: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    pub category: String,
    #[serde(default)]
    pub priority: MessagePriority,
    #[serde(default)]
    pub privacy: PrivacyLevel,
    pub message: String,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    #[serde(default)]
    pub project_root: Option<String>,
    pub timestamp: UtcDateTime,
    pub read_by: Vec<String>,
    #[serde(default)]
    pub expires_at: Option<UtcDateTime>,
}

/// Metadata key under which a bus message carries its admission binding. The
/// on-disk shape of [`ScratchpadEntry`] is unchanged: `metadata` already is a
/// string map, and an entry written before bindings existed simply has no
/// such key (the legacy rule of `context_admission::stored` applies to it).
pub const BINDING_METADATA_KEY: &str = "lean_ctx.binding";

/// Metadata keys under which a post made with an idempotency key carries that
/// key and the digest of the request it was first posted with (AP07-G9). Like
/// the binding, they live in `metadata`, so the entry keeps its shape and
/// entries without a key are unaffected.
pub(crate) const REQUEST_ID_METADATA_KEY: &str = "lean_ctx.request_id";
pub(crate) const REQUEST_DIGEST_METADATA_KEY: &str = "lean_ctx.request_digest";

const MAX_REQUEST_ID_BYTES: usize = 128;

/// An idempotency key as a caller may send it: 1-128 bytes, no control
/// characters. Shared by every store that replays a keyed write.
pub(crate) fn check_request_id(key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > MAX_REQUEST_ID_BYTES || key.chars().any(char::is_control) {
        return Err(format!(
            "request_id must be 1-{MAX_REQUEST_ID_BYTES} bytes without control characters"
        ));
    }
    Ok(())
}

/// The digest that binds an idempotency key to the request it was first
/// used with: `domain` separates the stores, `payload` is the request.
pub(crate) fn request_digest(domain: &str, payload: &serde_json::Value) -> String {
    crate::core::hasher::hash_hex(format!("{domain}\0{payload}").as_bytes())
}

/// The refusal of a key reused with another request; content-free.
pub fn request_id_conflict() -> String {
    "request_id was already used with a different request (idempotency conflict)".to_string()
}

/// Idempotency-key prefix of bus entries another machine's worker delivered
/// through Cloud (the relay writes them with it).
pub const CLOUD_INBOUND_KEY_PREFIX: &str = "cloud:";

impl ScratchpadEntry {
    /// True for an entry another machine's worker delivered through Cloud: it
    /// is shown fenced as foreign data and never published again.
    pub fn is_cloud_inbound(&self) -> bool {
        self.metadata
            .get(REQUEST_ID_METADATA_KEY)
            .is_some_and(|key| key.starts_with(CLOUD_INBOUND_KEY_PREFIX))
    }

    /// The Cloud bus address an inbound entry came from (its relay key is
    /// `cloud:<address>:<seq>`, or `cloud:<address>:<tag>.<seq>` once the
    /// bus is bound to a place and epoch, each maybe `@<agent>`; only the
    /// address may hold ':'); `None` for local entries.
    pub fn cloud_inbound_address(&self) -> Option<&str> {
        self.metadata
            .get(REQUEST_ID_METADATA_KEY)?
            .strip_prefix(CLOUD_INBOUND_KEY_PREFIX)?
            .rsplit_once(':')
            .map(|(address, _)| address)
            .filter(|address| !address.is_empty())
    }

    /// Admit `message` for the bus and return the text to store with its
    /// binding. A bus message is typed by an agent: its lineage is
    /// "caller-supplied, no source" — governed by content rules and the
    /// project authority of its writer, not by a source that could be
    /// re-authorized later.
    pub(crate) fn admit_message(
        from_agent: &str,
        project_root: Option<&str>,
        message: &str,
    ) -> Result<(String, HashMap<String, String>), String> {
        use crate::core::context_admission::stored::{Lineage, admit_and_bind};
        let (admitted, binding) = admit_and_bind(
            message,
            &format!("agent:{from_agent}"),
            project_root,
            Lineage::CallerSupplied,
        )
        .map_err(|withheld| withheld.message("agent message"))?;
        let mut metadata = HashMap::new();
        if let Ok(binding) = serde_json::to_string(&binding) {
            metadata.insert(BINDING_METADATA_KEY.to_owned(), binding);
        }
        Ok((admitted, metadata))
    }

    /// The message as a new delivery to a reader working in `reader_project`,
    /// re-authorized under the CURRENT policy: a message stored before the
    /// policy tightened is redacted or withheld now. A withheld message is
    /// rendered as the typed, content-free gateway notice.
    ///
    /// A served message publishes its lineage into the call's source capture
    /// (`stored::publish_served`): the files of a source-bound message, or —
    /// for a message typed by an agent or stored without a binding, which is
    /// every message the bus writes today — "unknown". A withheld message
    /// publishes nothing: the notice is fixed text.
    #[must_use]
    pub fn served_message(&self, reader_project: Option<&str>) -> String {
        use crate::core::context_admission::stored;
        let binding: Option<stored::StoredBinding> = self
            .metadata
            .get(BINDING_METADATA_KEY)
            .and_then(|binding| serde_json::from_str(binding).ok());
        let served = match stored::serve_stored(&self.message, binding.as_ref(), reader_project) {
            Ok(served) => {
                stored::publish_served(binding.as_ref());
                served
            }
            Err(withheld) => return withheld.notice(),
        };
        // X5: a message another instance delivered through Cloud is data,
        // its sender and kind included (they came with it, M6).
        if self.is_cloud_inbound() {
            use crate::core::untrusted::{UntrustedKind, wrap};
            return wrap(
                UntrustedKind::BusMessage,
                "cloud bus",
                &format!(
                    "from: {}\nkind: {}\n\n{served}",
                    self.from_agent, self.category
                ),
            );
        }
        served
    }
}

#[cfg(test)]
mod lineage_tests;

#[cfg(test)]
mod stated_role_tests {
    use super::{StatedRoles, stated_roles};

    #[test]
    fn a_bus_name_is_a_session_role_but_never_a_permission_role() {
        assert_eq!(
            stated_roles(None, Some("reviewer")),
            StatedRoles {
                session: Some("reviewer".into()),
                permission: None,
            }
        );
        assert_eq!(
            stated_roles(Some(" coder "), Some("reviewer")),
            StatedRoles {
                session: Some("coder".into()),
                permission: Some("coder".into()),
            }
        );
        assert_eq!(
            stated_roles(Some("  "), Some("")),
            StatedRoles {
                session: None,
                permission: None,
            }
        );
    }
}
