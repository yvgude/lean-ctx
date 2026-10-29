// SPDX-License-Identifier: Apache-2.0

//! Engine metrics v2 (C0 2.0 `schemas/metrics.schema.json`, telemetry SPEC).
//!
//! `POST /v1/metrics/hours` [`HoursRequestV2`] → [`HoursAckV2`]: hourly
//! counters of one engine, split by Hive vault, signed by the instance key
//! (Ed25519 over [`ENGINE_METRICS_DOMAIN`] ‖ canonical_json(body with
//! `key_id`, without `signature`), as `worker_register`). Bucket key (instance
//! from the bearer token, `hour`, `vault`); a higher `rev` replaces the
//! hour, a lower or equal one is acked `unchanged`. `GET /v1/metrics/sharing`
//! and every ack carry the [`SharingStateV2`].
//!
//! Aggregates only, enforced by the types: every string on this wire is a
//! closed-pattern newtype ([`ToolName`], [`RuleId`], [`WireName`],
//! [`FilterName`], [`DetectorName`], [`UnregulatedName`], [`Fingerprint`],
//! [`HourStamp`], [`VaultId`], [`EngineVersion`], [`KeyId`],
//! [`SignatureHex`], [`TimeStamp`]) or a closed enum ([`ReadMode`],
//! [`EngineOs`], the schema constants). None of them can hold a path, a
//! command line, a prompt, a secret or other free text: constructing one
//! from such a value fails (or maps to `other`), and deserialising one
//! refuses it. Every struct refuses unknown fields.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ValidationError;

pub const ENGINE_METRICS_SCHEMA: &str = "leanctx.hive.engine-metrics/v2";
pub const ENGINE_METRICS_ACK_SCHEMA: &str = "leanctx.hive.engine-metrics-ack/v2";
/// The policy member that gates engine metrics (`x-merge: false_wins`,
/// `x-enforcement: server`; absent at every level = the plan default).
pub const SHARE_ENGINE_METRICS_MEMBER: &str = "sharing.share_engine_metrics";
/// Domain of the batch signature (instance key).
pub const ENGINE_METRICS_DOMAIN: &str = "leanctx.hive.engine-metrics.v2\n";
/// HKDF info of the per-workspace secret fingerprint key.
pub const SECRET_FINGERPRINT_INFO: &str = "leanctx.hive.secret-fingerprint.v1";
/// `POST /v1/metrics/hours`, relative to the engine API base.
pub const HOURS_ROUTE: &str = "/metrics/hours";
/// `GET /v1/metrics/sharing`.
pub const SHARING_ROUTE: &str = "/metrics/sharing";
/// `POST /v1/metrics/sharing/confirm`.
pub const SHARING_CONFIRM_ROUTE: &str = "/metrics/sharing/confirm";

pub const MAX_BUCKETS: usize = 168;
pub const MAX_SLICES: usize = 33;
pub const MAX_TOOLS: usize = 48;
pub const MAX_POLICY_BLOCKS: usize = 32;
pub const MAX_REFETCH: usize = 32;
pub const MAX_UNREGULATED: usize = 32;
pub const MAX_SECRET_HITS: usize = 32;
pub const MAX_HOSTS: usize = 16;
pub const MAX_SPEND: usize = 32;
/// Largest request body Hive accepts.
pub const MAX_HOURS_BODY_BYTES: usize = 512 * 1024;
/// Largest count a JSON number carries exactly (2^53).
pub const MAX_COUNT: u64 = 1 << 53;
/// Name of the row that absorbs rows beyond a limit or names outside a rule.
pub const OTHER: &str = "other";

/// K2 (M1): the LeanCTX tool names the engine may send as `tools[].tool`;
/// any other name (a proxied or custom MCP tool) is `other`. The engine's
/// tool registry must be a subset (engine test).
pub const KNOWN_TOOLS: &[&str] = &[
    "cli_deps",
    "cli_diff",
    "cli_find",
    "cli_full",
    "cli_glob",
    "cli_grep",
    "cli_ls",
    "cli_read",
    "cli_read_dedup",
    "cli_shell",
    "cli_signatures",
    "ctx_agent",
    "ctx_analyze",
    "ctx_architecture",
    "ctx_artifacts",
    "ctx_benchmark",
    "ctx_call",
    "ctx_callgraph",
    "ctx_checkpoint",
    "ctx_compose",
    "ctx_compress",
    "ctx_compress_memory",
    "ctx_compute",
    "ctx_context",
    "ctx_control",
    "ctx_cost",
    "ctx_dedup",
    "ctx_delta",
    "ctx_discover",
    "ctx_discover_tools",
    "ctx_edit",
    "ctx_execute",
    "ctx_expand",
    "ctx_feedback",
    "ctx_gain",
    "ctx_git_read",
    "ctx_glob",
    "ctx_graph",
    "ctx_handoff",
    "ctx_heatmap",
    "ctx_impact",
    "ctx_index",
    "ctx_intent",
    "ctx_knowledge",
    "ctx_load_tools",
    "ctx_memory",
    "ctx_metrics",
    "ctx_multi_read",
    "ctx_multi_repo",
    "ctx_outline",
    "ctx_overview",
    "ctx_pack",
    "ctx_package",
    "ctx_patch",
    "ctx_prefetch",
    "ctx_preload",
    "ctx_proof",
    "ctx_provider",
    "ctx_radar",
    "ctx_read",
    "ctx_review",
    "ctx_search",
    "ctx_semantic_search",
    "ctx_session",
    "ctx_shape",
    "ctx_share",
    "ctx_shell",
    "ctx_smart_read",
    "ctx_symbol",
    "ctx_tasks",
    "ctx_tools",
    "ctx_transcript_compact",
    "ctx_tree",
    "ctx_url_read",
    "ctx_verify",
    "ctx_work_graph",
    "ctx_wrapped",
    "ocla_savings",
    "shell",
    OTHER,
];

/// Policy member paths and gateway rule ids (`policy_blocks[].rule`).
pub const KNOWN_RULES: &[&str] = &[
    "context.allowed_sources",
    "context.allowed_tools",
    "context.auto_extraction",
    "context.denied_tools",
    "context.extraction_requires_source_anchor",
    "context.injection_filter",
    "context.max_context_tokens",
    "context.max_cost_cents_per_person_per_day",
    "context.max_cost_cents_per_project_per_month",
    "context.max_requests_per_minute_per_person",
    "context.max_source_age_days",
    "context.max_writes_per_min",
    "context.ranking_profile",
    "context.read_mode",
    "security.allowed_models",
    "security.block_secrets",
    "security.blocked_labels",
    "security.classification",
    "security.confidential_out_of_sharing",
    "security.excluded_paths",
    "security.external_model_excluded_classes",
    "security.pii",
    "sharing.allowed_vaults",
    "sharing.auto_merge_results",
    "sharing.consent_required_for",
    "sharing.cross_team_sharing",
    "sharing.personal_workspace",
    "sharing.share_engine_metrics",
    "sharing.task_claim_min_role",
    "gateway.budget_exceeded",
    "gateway.cross_project",
    "gateway.path_jail",
    "gateway.rate_limited",
    "gateway.security_violation",
    "gateway.tool_denied",
    "gateway.destination",
    "gateway.egress",
    OTHER,
];

/// Re-fetch filters: a read mode or an output class (`refetch[].filter`).
pub const KNOWN_FILTERS: &[&str] = &[
    "auto",
    "full",
    "map",
    "signatures",
    "diff",
    "task",
    "reference",
    "aggressive",
    "entropy",
    "lines",
    "shell_output",
    "search_output",
    OTHER,
];

/// Secret detectors (`secret_hits[].detector`).
pub const KNOWN_DETECTORS: &[&str] = &[
    "aws_access_key",
    "aws_secret_key",
    "gcp_key",
    "azure_key",
    "github_token",
    "gitlab_token",
    "slack_token",
    "stripe_key",
    "openai_key",
    "anthropic_key",
    "private_key",
    "jwt",
    "password_assignment",
    "connection_string",
    "generic_api_key",
    OTHER,
];

/// Known programs (`unregulated[]` of kind `program`); never a command line.
pub const KNOWN_PROGRAMS: &[&str] = &[
    "git",
    "cargo",
    "rustc",
    "npm",
    "npx",
    "pnpm",
    "yarn",
    "node",
    "deno",
    "bun",
    "python",
    "python3",
    "pip",
    "uv",
    "go",
    "make",
    "cmake",
    "docker",
    "kubectl",
    "helm",
    "terraform",
    "aws",
    "gcloud",
    "az",
    "gh",
    "curl",
    "wget",
    "ssh",
    "scp",
    "rsync",
    "psql",
    "mysql",
    "redis-cli",
    "java",
    "mvn",
    "gradle",
    "dotnet",
    "ruby",
    "bundle",
    "php",
    "composer",
    "swift",
    "xcodebuild",
    "bash",
    "sh",
    "zsh",
    OTHER,
];

/// K2 (H2): MCP hosts by their client id; any other `clientInfo.name` is `other`.
pub const KNOWN_HOSTS: &[&str] = &[
    "amp",
    "antigravity",
    "augment",
    "chatgpt",
    "claude-code",
    "claude-desktop",
    "cline",
    "codebuddy",
    "codewhale",
    "codex",
    "continue",
    "copilot-cli",
    "crush",
    "cursor",
    "emacs",
    "factory",
    "gemini-cli",
    "goose",
    "jetbrains",
    "kilo-code",
    "kiro",
    "lm-studio",
    "neovim",
    "oh-my-pi",
    "opencode",
    "pi",
    "qwen-code",
    "roo-code",
    "trae",
    "visual-studio",
    "vscode-copilot",
    "warp",
    "windsurf",
    "zed",
    "unknown",
    OTHER,
];

/// K13: agent host families (`unregulated[]` of kind `host`), coarser than
/// [`KNOWN_HOSTS`]: one family per vendor integration; anything else is `other`.
pub const HOST_FAMILIES: &[&str] = &[
    "amp",
    "claude",
    "cline",
    "codex",
    "continue",
    "copilot",
    "cursor",
    "gemini",
    "goose",
    "jetbrains",
    "kilo",
    "kiro",
    "opencode",
    "pi",
    "qwen",
    "roo",
    "windsurf",
    "zed",
    OTHER,
];

/// The host family of an MCP client id (a [`KNOWN_HOSTS`] entry or what the
/// client announced); never the announced text itself.
#[must_use]
pub fn host_family(client: &str) -> &'static str {
    let client = client.to_ascii_lowercase();
    let family = match client.as_str() {
        "claude" | "claude-code" | "claude-desktop" | "claude-ai" => "claude",
        "codex" | "codex-cli" | "codex-mcp-client" => "codex",
        "copilot" | "copilot-cli" | "vscode-copilot" | "vscode" => "copilot",
        "gemini" | "gemini-cli" => "gemini",
        "pi" | "oh-my-pi" | "omp" => "pi",
        "roo" | "roo-code" => "roo",
        "kilo" | "kilo-code" => "kilo",
        "qwen" | "qwen-code" => "qwen",
        other => other,
    };
    HOST_FAMILIES
        .iter()
        .copied()
        .find(|f| *f == family)
        .unwrap_or(OTHER)
}

/// Providers of `spend[]`.
pub const KNOWN_PROVIDERS: &[&str] = &[
    "anthropic",
    "openai",
    "google",
    "microsoft",
    "deepseek",
    "meta",
    "unknown",
    OTHER,
];

/// K2 (H3): models of `spend[]`: the engine's embedded price table keys
/// (and the contract fixture names); any other model id (fine-tunes, local
/// or org-named models) is `other`. The engine's price table must be a
/// subset (engine test).
pub const KNOWN_MODELS: &[&str] = &[
    "claude-fable-5",
    "claude-opus-4.5",
    "claude-sonnet-4.5",
    "claude-haiku-4.5",
    "claude-sonnet-4-5",
    "claude-3.5-sonnet",
    "claude-3-opus",
    "claude-3-haiku",
    "gpt-5",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.4-nano",
    "gemini-2.5-pro",
    "gemini-2.5-flash",
    "gemini-2.5-flash-lite",
    "phi-4",
    "phi-4-mini",
    "deepseek-v3.2",
    "deepseek-v3",
    "llama-3.3-70b",
    "llama-4-maverick",
    "fallback-blended",
    OTHER,
];

fn invalid(what: &str) -> ValidationError {
    ValidationError::new(format!("engine metrics: invalid {what}"))
}

// ── closed-pattern strings ───────────────────────────────────────────────────

macro_rules! closed_string {
    ($(#[$doc:meta])* $name:ident, $check:path, $what:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// `value` when it matches the rule, else `None`.
            pub fn new(value: &str) -> Option<Self> {
                $check(value).then(|| Self(value.to_string()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                Self::new(&raw).ok_or_else(|| serde::de::Error::custom(concat!("invalid ", $what)))
            }
        }
    };
}
pub(crate) use closed_string;

/// K2 H3: a closed table, not a shape. A custom or proxied MCP tool name
/// that looks like an identifier (`acme_secret_project`) is refused, so the
/// engine maps it to `other` ([`ToolName::or_other`]) before it is counted.
fn is_tool(value: &str) -> bool {
    KNOWN_TOOLS.contains(&value)
}

fn is_rule(value: &str) -> bool {
    KNOWN_RULES.contains(&value)
}

fn is_filter(value: &str) -> bool {
    KNOWN_FILTERS.contains(&value)
}

fn is_detector(value: &str) -> bool {
    KNOWN_DETECTORS.contains(&value)
}

/// An MCP server name as the contract allows it: `^[a-z0-9][a-z0-9_-]{0,47}$`
/// and never a run of 24+ letters or digits (token-like).
pub fn is_mcp_server_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    let shape = bytes
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && value.len() <= 48
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'));
    let mut run = 0usize;
    let token_like = value.bytes().any(|b| {
        run = if b.is_ascii_alphanumeric() {
            run + 1
        } else {
            0
        };
        run >= 24
    });
    shape && !token_like
}

/// v1 `name` rule (`[A-Za-z0-9._:/-]{1,64}`, never a path) and, since the
/// K2 review, a closed table: a known host, provider or model, or `other`.
/// A relative path, a host name, a fine-tune id or a key never passes.
fn is_wire_name(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'/' | b'-'))
        && !value.starts_with('/')
        && !value.contains("..")
        && !value.contains("//")
        && (value == OTHER
            || KNOWN_HOSTS.contains(&value)
            || KNOWN_PROVIDERS.contains(&value)
            || KNOWN_MODELS.contains(&value))
}

/// A known program, a host family or an MCP server name (the kind decides
/// which one applies, [`SliceV2::validate`]).
fn is_unregulated_name(value: &str) -> bool {
    KNOWN_PROGRAMS.contains(&value) || HOST_FAMILIES.contains(&value) || is_mcp_server_name(value)
}

fn is_hour(value: &str) -> bool {
    is_time(value) && value.ends_with(":00:00Z")
}

/// `YYYY-MM-DDTHH:MM:SSZ`.
fn is_time(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'Z'
        && [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18]
            .iter()
            .all(|&i| b[i].is_ascii_digit())
        && chrono::DateTime::parse_from_rfc3339(value).is_ok()
}

fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_hex16(value: &str) -> bool {
    is_hex(value, 16)
}

fn is_hex32(value: &str) -> bool {
    is_hex(value, 32)
}

fn is_hex128(value: &str) -> bool {
    is_hex(value, 128)
}

fn is_engine_version(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
}

closed_string!(
    /// A registered LeanCTX tool name ([`KNOWN_TOOLS`], `other` included).
    ToolName, is_tool, "tool name"
);
closed_string!(
    /// A policy member path or gateway rule id (`security.excluded_paths`).
    RuleId, is_rule, "rule id"
);
closed_string!(
    /// A model, provider or host name (v1 `name` rule, never a path).
    WireName, is_wire_name, "name"
);
closed_string!(
    /// A compression filter: a read mode or a pattern name (tool rule).
    FilterName, is_filter, "filter name"
);
closed_string!(
    /// A secret detector name (tool rule).
    DetectorName, is_detector, "detector name"
);
closed_string!(
    /// A known program, host or MCP server name (never a command line).
    UnregulatedName, is_unregulated_name, "unregulated name"
);
closed_string!(
    /// First 16 hex of HMAC-SHA256(workspace fingerprint key, secret).
    Fingerprint, is_hex16, "fingerprint"
);
closed_string!(
    /// A UTC hour start, `YYYY-MM-DDTHH:00:00Z`.
    HourStamp, is_hour, "hour"
);
closed_string!(
    /// A UTC time, second precision.
    TimeStamp, is_time, "time"
);
closed_string!(
    /// A Hive vault id (32 lowercase hex characters).
    VaultId, is_hex32, "vault id"
);
closed_string!(
    /// `CARGO_PKG_VERSION` as the contract allows it.
    EngineVersion, is_engine_version, "engine version"
);
closed_string!(
    /// `hex(blake3(instance public key))[..16]`.
    KeyId, is_hex16, "key id"
);
closed_string!(
    /// An Ed25519 signature, lowercase hex.
    SignatureHex, is_hex128, "signature"
);

impl ToolName {
    /// `value` when it is a registered LeanCTX tool ([`KNOWN_TOOLS`]), else
    /// `other`: a command line, a path, a proxied or custom MCP tool name
    /// never becomes a tool name.
    pub fn or_other(value: &str) -> Self {
        if KNOWN_TOOLS.contains(&value) {
            Self(value.to_string())
        } else {
            Self::other()
        }
    }

    pub fn other() -> Self {
        Self(OTHER.into())
    }
}

impl FilterName {
    pub fn or_other(value: &str) -> Self {
        Self::new(value).unwrap_or_else(|| Self(OTHER.into()))
    }
}

impl UnregulatedName {
    pub fn other() -> Self {
        Self(OTHER.into())
    }
}

impl WireName {
    pub fn other() -> Self {
        Self(OTHER.into())
    }
}

impl DetectorName {
    pub fn or_other(value: &str) -> Self {
        Self::new(value).unwrap_or_else(|| Self(OTHER.into()))
    }
}

impl RuleId {
    pub fn or_other(value: &str) -> Self {
        Self::new(value).unwrap_or_else(Self::other)
    }

    pub fn other() -> Self {
        Self(OTHER.into())
    }
}

impl HourStamp {
    /// The hour that holds `time` (UTC).
    pub fn of(time: chrono::DateTime<chrono::Utc>) -> Self {
        Self(time.format("%Y-%m-%dT%H:00:00Z").to_string())
    }

    pub fn start(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(&self.0)
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or_default()
    }
}

impl TimeStamp {
    pub fn at(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(&self.0)
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or_default()
    }
}

/// A schema id that serialises as exactly `ID` and accepts nothing else.
macro_rules! schema_const {
    ($name:ident, $id:expr) => {
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        pub struct $name;

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str($id)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                (raw == $id)
                    .then_some(Self)
                    .ok_or_else(|| serde::de::Error::custom("wrong schema"))
            }
        }
    };
}
pub(crate) use schema_const;

schema_const!(HoursSchema, ENGINE_METRICS_SCHEMA);
schema_const!(HoursAckSchema, ENGINE_METRICS_ACK_SCHEMA);

// ── closed enums ─────────────────────────────────────────────────────────────

/// `ctx_read` read modes (closed set; unknown modes count as `other`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadMode {
    Auto,
    Full,
    Map,
    Signatures,
    Diff,
    Task,
    Reference,
    Aggressive,
    Entropy,
    Lines,
    Other,
}

impl ReadMode {
    /// The read mode a recorded mode name counts as: `lines:10-20` is
    /// `lines`, anything outside the set is `other`.
    pub fn of(mode: &str) -> Self {
        let head = mode.split(':').next().unwrap_or(mode).trim();
        match head {
            "auto" => Self::Auto,
            "full" => Self::Full,
            "map" => Self::Map,
            "signatures" => Self::Signatures,
            "diff" => Self::Diff,
            "task" => Self::Task,
            "reference" => Self::Reference,
            "aggressive" => Self::Aggressive,
            "entropy" => Self::Entropy,
            "lines" => Self::Lines,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineOs {
    Macos,
    Linux,
    Windows,
}

/// Policy level that set the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyLevel {
    Org,
    Workspace,
    Vault,
    Engine,
}

impl PolicyLevel {
    /// How people read the level (Company › Team › Vault › Engine).
    pub fn label(self) -> &'static str {
        match self {
            Self::Org => "Company",
            Self::Workspace => "Team",
            Self::Vault => "Vault",
            Self::Engine => "Engine",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "org" => Some(Self::Org),
            "workspace" => Some(Self::Workspace),
            "vault" => Some(Self::Vault),
            "engine" => Some(Self::Engine),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    TooOld,
    Future,
    BeforeSharing,
    PolicyDenied,
    VaultNotAllowed,
    VaultUnknown,
    UnattributedNotPersonal,
}

/// `unregulated[].kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnregulatedKind {
    Program,
    Host,
    McpServer,
}

/// `sharing_state.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharingValue {
    On,
    Off,
    NeedsConfirmation,
}

/// `sharing_state.plan_default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanDefault {
    On,
    Off,
}

// ── wire structs ─────────────────────────────────────────────────────────────

/// The batch as signed: `hours_request` without `key_id` and `signature`.
/// `hive metrics preview --json` prints this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoursBodyV2 {
    pub schema: HoursSchema,
    pub engine: EngineV2,
    pub buckets: Vec<BucketV2>,
}

/// `POST /v1/metrics/hours` body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoursRequestV2 {
    pub schema: HoursSchema,
    pub engine: EngineV2,
    pub buckets: Vec<BucketV2>,
    pub key_id: KeyId,
    pub signature: SignatureHex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineV2 {
    pub engine_version: EngineVersion,
    pub os: EngineOs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BucketV2 {
    pub hour: HourStamp,
    pub rev: u32,
    pub slices: Vec<SliceV2>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SliceV2 {
    pub vault: Option<VaultId>,
    pub tokens: TokensV2,
    pub savings: SavingsV2,
    pub calls: CallsV2,
    pub read_modes: BTreeMap<ReadMode, u64>,
    pub tools: Vec<ToolRowV2>,
    pub compression: CompressionV2,
    pub ledger: LedgerV2,
    pub guards: GuardsV2,
    pub policy_blocks: Vec<BlockRowV2>,
    pub refetch: Vec<RefetchRowV2>,
    pub unregulated: Vec<UnregulatedRowV2>,
    pub secret_hits: Vec<SecretRowV2>,
    pub sessions: SessionsV2,
    pub hosts: Vec<HostRowV2>,
    pub spend: Vec<SpendRowV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hive: Option<HiveHourV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gauges: Option<GaugesV2>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokensV2 {
    pub input: u64,
    pub output: u64,
    pub saved: u64,
}

/// Saved tokens by trust label (K7) and LeanCTX's own overhead (K8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavingsV2 {
    pub measured: u64,
    pub estimated: u64,
    pub estimated_low: u64,
    pub estimated_high: u64,
    pub evidenced: u64,
    pub overhead: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallsV2 {
    pub tool_calls: u64,
    pub cache_reads: u64,
    pub cache_hits: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRowV2 {
    pub tool: ToolName,
    pub calls: u64,
    pub saved: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompressionV2 {
    pub original: u64,
    pub compressed: u64,
}

/// Saved tokens by ledger kind (the ROI view).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerV2 {
    pub reduction: u64,
    pub cache_discount: u64,
    pub correction: u64,
    pub edit: u64,
    pub other: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardsV2 {
    pub secrets_redacted: u64,
    pub pii_redacted: u64,
    pub injection_flags: u64,
    pub content_withheld: u64,
    pub paths_blocked: u64,
    pub shell_blocked: u64,
    pub partial_inspections: u64,
}

/// K12: refusals in force and refusals a monitor-mode rule would make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockRowV2 {
    pub rule: RuleId,
    pub blocked: u64,
    pub would_block: u64,
}

/// K10: outputs truncated and fetched back in full.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefetchRowV2 {
    pub filter: FilterName,
    pub truncated: u64,
    pub refetched: u64,
}

/// K13: activity no rule covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnregulatedRowV2 {
    pub kind: UnregulatedKind,
    pub name: UnregulatedName,
    pub calls: u64,
}

/// K21: a redacted secret as its workspace fingerprint, never its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretRowV2 {
    pub detector: DetectorName,
    pub count: u64,
}

/// X4 Hive help in one hour (moved from the retired `POST /v1/metrics`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HiveHourV2 {
    pub knowledge_hits_referenced: u64,
    pub first_context_sessions: u64,
    pub first_context_ms_p50: Option<u64>,
    pub task_usage: Vec<TaskUsageV2>,
}

/// Ledger usage while a session held a Hive task (by number, never a title).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskUsageV2 {
    pub number: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub list_usd_micros: u64,
}

pub const MAX_TASK_USAGE: usize = 64;
pub const MAX_FIRST_CONTEXT_MS: u64 = 3_600_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionsV2 {
    pub started: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostRowV2 {
    pub host: WireName,
    pub sessions: u64,
    pub tool_calls: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpendRowV2 {
    pub model: WireName,
    pub provider: WireName,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub usd_micros: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GaugesV2 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts_current: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoursAckV2 {
    pub schema: HoursAckSchema,
    pub accepted: u32,
    pub unchanged: u32,
    pub refused: Vec<RefusalV2>,
    pub sharing: SharingStateV2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefusalV2 {
    pub hour: HourStamp,
    pub vault: Option<VaultId>,
    pub reason: RefusalReason,
    pub level: Option<PolicyLevel>,
}

/// The policy part of a sharing state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharingPolicyValueV2 {
    pub value: bool,
    pub level: PolicyLevel,
}

/// `sharing_state`: `GET /v1/metrics/sharing`, the confirm answer, and the
/// `sharing` of every ack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharingStateV2 {
    pub state: SharingValue,
    pub plan_default: PlanDefault,
    pub policy: Option<SharingPolicyValueV2>,
    pub confirmed_at: Option<TimeStamp>,
    pub since: Option<TimeStamp>,
}

// ── checks ───────────────────────────────────────────────────────────────────

fn counts_ok(values: impl IntoIterator<Item = u64>) -> bool {
    values.into_iter().all(|n| n <= MAX_COUNT)
}

fn unique<'a>(names: impl IntoIterator<Item = &'a str>) -> bool {
    let mut seen = BTreeSet::new();
    names.into_iter().all(|n| seen.insert(n))
}

impl SliceV2 {
    /// Every counter of the slice, for the 2^53 bound and activity checks.
    pub fn counters(&self) -> Vec<u64> {
        let t = &self.tokens;
        let s = &self.savings;
        let c = &self.calls;
        let l = &self.ledger;
        let g = &self.guards;
        let mut all = vec![
            t.input,
            t.output,
            t.saved,
            s.measured,
            s.estimated,
            s.estimated_low,
            s.estimated_high,
            s.evidenced,
            s.overhead,
            c.tool_calls,
            c.cache_reads,
            c.cache_hits,
            self.compression.original,
            self.compression.compressed,
            l.reduction,
            l.cache_discount,
            l.correction,
            l.edit,
            l.other,
            g.secrets_redacted,
            g.pii_redacted,
            g.injection_flags,
            g.content_withheld,
            g.paths_blocked,
            g.shell_blocked,
            g.partial_inspections,
            self.sessions.started,
        ];
        all.extend(self.read_modes.values().copied());
        all.extend(self.tools.iter().flat_map(|r| [r.calls, r.saved]));
        all.extend(
            self.policy_blocks
                .iter()
                .flat_map(|r| [r.blocked, r.would_block]),
        );
        all.extend(self.refetch.iter().flat_map(|r| [r.truncated, r.refetched]));
        all.extend(self.unregulated.iter().map(|r| r.calls));
        if let Some(a) = &self.hive {
            all.extend([a.knowledge_hits_referenced, a.first_context_sessions]);
            all.extend(a.first_context_ms_p50);
            all.extend(
                a.task_usage
                    .iter()
                    .flat_map(|t| [t.number, t.input_tokens, t.output_tokens, t.list_usd_micros]),
            );
        }
        all.extend(self.secret_hits.iter().map(|r| r.count));
        all.extend(self.hosts.iter().flat_map(|r| [r.sessions, r.tool_calls]));
        all.extend(self.spend.iter().flat_map(|r| {
            [
                r.requests,
                r.input_tokens,
                r.output_tokens,
                r.cached_tokens,
                r.usd_micros,
            ]
        }));
        all
    }

    /// Whether anything happened (the gauge is a level, not activity; the
    /// overhead alone is not activity either).
    pub fn has_activity(&self) -> bool {
        let mut counters = self.counters();
        counters.remove(8);
        counters.into_iter().any(|n| n > 0)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.tools.len() > MAX_TOOLS
            || self.policy_blocks.len() > MAX_POLICY_BLOCKS
            || self.refetch.len() > MAX_REFETCH
            || self.unregulated.len() > MAX_UNREGULATED
            || self.secret_hits.len() > MAX_SECRET_HITS
            || self.hosts.len() > MAX_HOSTS
            || self.spend.len() > MAX_SPEND
        {
            return Err(invalid("slice: too many rows"));
        }
        if !unique(self.tools.iter().map(|r| r.tool.as_str()))
            || !unique(self.policy_blocks.iter().map(|r| r.rule.as_str()))
            || !unique(self.refetch.iter().map(|r| r.filter.as_str()))
            || !unique(self.hosts.iter().map(|r| r.host.as_str()))
        {
            return Err(invalid("slice: duplicate row name"));
        }
        let mut pairs = BTreeSet::new();
        if !self
            .spend
            .iter()
            .all(|r| pairs.insert((r.model.as_str(), r.provider.as_str())))
        {
            return Err(invalid("slice: duplicate spend row"));
        }
        let mut unregulated = BTreeSet::new();
        if !self
            .unregulated
            .iter()
            .all(|r| unregulated.insert((r.kind, r.name.as_str())))
        {
            return Err(invalid("slice: duplicate unregulated row"));
        }
        if !unique(self.secret_hits.iter().map(|r| r.detector.as_str())) {
            return Err(invalid("slice: duplicate secret row"));
        }
        for row in &self.unregulated {
            let name = row.name.as_str();
            let fits = match row.kind {
                UnregulatedKind::Program => KNOWN_PROGRAMS.contains(&name),
                UnregulatedKind::Host => HOST_FAMILIES.contains(&name),
                UnregulatedKind::McpServer => is_mcp_server_name(name),
            };
            if !fits {
                return Err(invalid("slice: unregulated name of another kind"));
            }
        }
        if let Some(a) = &self.hive {
            let sorted = a.task_usage.windows(2).all(|w| w[0].number < w[1].number);
            if a.task_usage.len() > MAX_TASK_USAGE
                || !sorted
                || a.task_usage.iter().any(|t| t.number == 0)
                || a.first_context_ms_p50
                    .is_some_and(|ms| ms > MAX_FIRST_CONTEXT_MS)
            {
                return Err(invalid("slice: hive"));
            }
        }
        let gauge = self.gauges.and_then(|g| g.facts_current).unwrap_or(0);
        if !counts_ok(self.counters()) || gauge > MAX_COUNT {
            return Err(invalid("slice: count beyond 2^53"));
        }
        Ok(())
    }
}

impl BucketV2 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.rev == 0 {
            return Err(invalid("bucket rev"));
        }
        if self.slices.is_empty() || self.slices.len() > MAX_SLICES {
            return Err(invalid("bucket: 1-33 slices"));
        }
        let mut vaults = BTreeSet::new();
        for slice in &self.slices {
            if !vaults.insert(slice.vault.as_ref().map(VaultId::as_str)) {
                return Err(invalid("bucket: duplicate vault"));
            }
            slice.validate()?;
        }
        Ok(())
    }
}

impl HoursBodyV2 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.buckets.is_empty() || self.buckets.len() > MAX_BUCKETS {
            return Err(invalid("request: 1-168 buckets"));
        }
        if !unique(self.buckets.iter().map(|b| b.hour.as_str())) {
            return Err(invalid("request: duplicate hour"));
        }
        self.buckets.iter().try_for_each(BucketV2::validate)
    }

    /// The bytes the signature covers: the request with `key_id` and
    /// without `signature` (the `worker_register` scheme).
    pub fn signing_input(&self, key_id: &KeyId) -> Result<Vec<u8>, ValidationError> {
        let mut value = serde_json::to_value(self)
            .map_err(|e| ValidationError::new(format!("engine metrics: encode: {e}")))?;
        value["key_id"] = serde_json::Value::String(key_id.as_str().to_string());
        super::signing_input(ENGINE_METRICS_DOMAIN, &value)
    }

    pub fn signed(self, key_id: KeyId, signature: SignatureHex) -> HoursRequestV2 {
        HoursRequestV2 {
            schema: self.schema,
            engine: self.engine,
            buckets: self.buckets,
            key_id,
            signature,
        }
    }
}

impl HoursRequestV2 {
    /// The signed part.
    pub fn body(&self) -> HoursBodyV2 {
        HoursBodyV2 {
            schema: self.schema,
            engine: self.engine.clone(),
            buckets: self.buckets.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        self.body().validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_their_closed_rules() {
        assert!(ToolName::new("ctx_read").is_some());
        for bad in ["rm -rf /", "/etc/passwd", "Ctx_Read", "ctx-read", "", "a b"] {
            assert!(ToolName::new(bad).is_none(), "{bad}");
            assert_eq!(ToolName::or_other(bad).as_str(), OTHER);
        }
        for unknown in ["acme_secret_project", "a", "ctx_reader"] {
            assert!(ToolName::new(unknown).is_none(), "{unknown}");
            assert_eq!(ToolName::or_other(unknown).as_str(), OTHER);
        }
        assert!(ToolName::new(OTHER).is_some());
        assert!(RuleId::new("sharing.share_engine_metrics").is_some());
        assert!(RuleId::new("gateway.tool_denied").is_some());
        assert!(RuleId::new("gateway.destination").is_some());
        assert!(RuleId::new("gateway.egress").is_some());
        assert_eq!(host_family("claude-code"), "claude");
        assert_eq!(host_family("Cursor"), "cursor");
        assert_eq!(host_family("oh-my-pi"), "pi");
        assert_eq!(host_family("acme-internal-agent"), OTHER);
        assert!(HOST_FAMILIES.iter().all(|f| is_unregulated_name(f)));
        for bad in ["blocked because of /x", "a.b.c", "Security.pii", ".x", "x."] {
            assert!(RuleId::new(bad).is_none(), "{bad}");
        }
        assert!(WireName::new("claude-sonnet-4-5").is_some());
        assert!(WireName::new("anthropic").is_some());
        assert!(WireName::new("claude-code").is_some());
        for bad in [
            "/Users/me",
            "a//b",
            "../x",
            "with space",
            "",
            "anthropic/claude",
            "ada-macbook.local",
        ] {
            assert!(WireName::new(bad).is_none(), "{bad}");
        }
        assert_eq!(ToolName::or_other("ctx_read").as_str(), "ctx_read");
        assert_eq!(ToolName::or_other("acme_internal_db").as_str(), OTHER);
        assert!(UnregulatedName::new("terraform").is_some());
        assert!(
            UnregulatedName::new("github").is_some(),
            "an MCP server name"
        );
        assert!(!is_mcp_server_name(&"a".repeat(30)), "token-like");
        assert_eq!(RuleId::or_other("custom.rule").as_str(), OTHER);
        assert_eq!(FilterName::or_other("cargo_test").as_str(), OTHER);
        assert_eq!(DetectorName::or_other("Generic").as_str(), OTHER);
        for bad in [
            "terraform apply -auto-approve",
            "/usr/bin/terraform",
            "a..b",
            "Git",
        ] {
            assert!(UnregulatedName::new(bad).is_none(), "{bad}");
        }
        assert!(Fingerprint::new("9c1e4b7a02d3f658").is_some());
        assert!(Fingerprint::new("[REDACTED:AWS key]").is_none());
        assert!(HourStamp::new("2026-10-26T17:00:00Z").is_some());
        for bad in [
            "2026-10-26T17:05:00Z",
            "2026-10-26T17:00:00+01:00",
            "2026-13-26T17:00:00Z",
        ] {
            assert!(HourStamp::new(bad).is_none(), "{bad}");
        }
        assert!(TimeStamp::new("2026-10-26T15:12:40Z").is_some());
        assert_eq!(ReadMode::of("lines:10-20"), ReadMode::Lines);
        assert_eq!(ReadMode::of("signatures"), ReadMode::Signatures);
        assert_eq!(ReadMode::of("something new"), ReadMode::Other);
    }

    #[test]
    fn free_text_cannot_be_deserialised_into_any_field() {
        let tool = serde_json::from_str::<ToolRowV2>(
            r#"{"tool":"cat ~/.ssh/id_rsa","calls":1,"saved":0}"#,
        );
        assert!(tool.is_err());
        let extra =
            serde_json::from_str::<TokensV2>(r#"{"input":1,"output":1,"saved":0,"prompt":"x"}"#);
        assert!(extra.is_err());
        let schema = serde_json::from_str::<HoursSchema>(r#""leanctx.hive.engine-metrics/v3""#);
        assert!(schema.is_err());
    }
}
