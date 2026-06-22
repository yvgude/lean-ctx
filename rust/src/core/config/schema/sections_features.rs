//! Feature config sections (autonomy, providers, loop-detection, updates,
//! boundary-policy, secret-detection, cloud, gain).
//! Split out of `schema/mod.rs`; `use super::*` re-imports helpers + `SectionSchema`.

#[allow(clippy::wildcard_imports)]
use super::*;
use std::collections::BTreeMap;

pub(super) fn build(sections: &mut BTreeMap<String, SectionSchema>) {
    let cfg = crate::core::config::Config::default();
    let mut autonomy = BTreeMap::new();
    autonomy.insert(
        "enabled".into(),
        key(
            "bool",
            serde_json::json!(cfg.autonomy.enabled),
            "Enable autonomous background behaviors",
        ),
    );
    autonomy.insert(
        "auto_preload".into(),
        key(
            "bool",
            serde_json::json!(cfg.autonomy.auto_preload),
            "Auto-preload related files on first read",
        ),
    );
    autonomy.insert(
        "auto_dedup".into(),
        key(
            "bool",
            serde_json::json!(cfg.autonomy.auto_dedup),
            "Auto-deduplicate repeated reads",
        ),
    );
    autonomy.insert(
        "auto_related".into(),
        key(
            "bool",
            serde_json::json!(cfg.autonomy.auto_related),
            "Auto-load graph-related files",
        ),
    );
    autonomy.insert(
        "auto_consolidate".into(),
        key(
            "bool",
            serde_json::json!(cfg.autonomy.auto_consolidate),
            "Auto-consolidate knowledge periodically",
        ),
    );
    autonomy.insert(
        "silent_preload".into(),
        key(
            "bool",
            serde_json::json!(cfg.autonomy.silent_preload),
            "Suppress preload notifications in output",
        ),
    );
    autonomy.insert(
        "dedup_threshold".into(),
        key(
            "usize",
            serde_json::json!(cfg.autonomy.dedup_threshold),
            "Number of repeated reads before dedup triggers",
        ),
    );
    autonomy.insert(
        "consolidate_every_calls".into(),
        key(
            "u32",
            serde_json::json!(cfg.autonomy.consolidate_every_calls),
            "Consolidate knowledge every N tool calls",
        ),
    );
    autonomy.insert(
        "consolidate_cooldown_secs".into(),
        key(
            "u64",
            serde_json::json!(cfg.autonomy.consolidate_cooldown_secs),
            "Minimum seconds between consolidation runs",
        ),
    );
    autonomy.insert(
        "cognition_loop_enabled".into(),
        key_with_env(
            "bool",
            serde_json::json!(cfg.autonomy.cognition_loop_enabled),
            "Enable the background cognition loop (periodic knowledge consolidation)",
            "LEAN_CTX_COGNITION_LOOP_ENABLED",
        ),
    );
    autonomy.insert(
        "cognition_loop_interval_secs".into(),
        key_with_env(
            "u64",
            serde_json::json!(cfg.autonomy.cognition_loop_interval_secs),
            "Seconds between cognition loop iterations",
            "LEAN_CTX_COGNITION_LOOP_INTERVAL_SECS",
        ),
    );
    autonomy.insert(
        "cognition_loop_max_steps".into(),
        key_with_env(
            "u8",
            serde_json::json!(cfg.autonomy.cognition_loop_max_steps),
            "Maximum steps per cognition loop iteration (>= 9 enables observation synthesis)",
            "LEAN_CTX_COGNITION_LOOP_MAX_STEPS",
        ),
    );
    autonomy.insert(
        "cognition_synthesis_min_cluster".into(),
        key_with_env(
            "usize",
            serde_json::json!(cfg.autonomy.cognition_synthesis_min_cluster),
            "Minimum facts per entity before observation synthesis writes a summary (needs cognition_loop_max_steps >= 9)",
            "LEAN_CTX_COGNITION_SYNTHESIS_MIN_CLUSTER",
        ),
    );
    sections.insert(
        "autonomy".into(),
        SectionSchema {
            description: "Controls autonomous background behaviors (preload, dedup, consolidation)"
                .into(),
            keys: autonomy,
        },
    );

    let mut providers = BTreeMap::new();
    providers.insert(
        "enabled".into(),
        key(
            "bool",
            serde_json::json!(cfg.providers.enabled),
            "Master switch for the provider subsystem (GitHub, GitLab, etc.)",
        ),
    );
    providers.insert(
        "auto_index".into(),
        key(
            "bool",
            serde_json::json!(cfg.providers.auto_index),
            "Auto-ingest provider results into BM25/embedding indexes",
        ),
    );
    providers.insert(
        "cache_ttl_secs".into(),
        key(
            "u64",
            serde_json::json!(cfg.providers.cache_ttl_secs),
            "Default cache TTL for provider results (seconds)",
        ),
    );
    providers.insert(
        "github.enabled".into(),
        key(
            "bool",
            serde_json::json!(cfg.providers.github.enabled),
            "Enable/disable GitHub provider",
        ),
    );
    providers.insert(
        "github.api_url".into(),
        key(
            "string",
            serde_json::json!(cfg.providers.github.api_url),
            "GitHub API base URL (for GitHub Enterprise)",
        ),
    );
    providers.insert(
        "gitlab.enabled".into(),
        key(
            "bool",
            serde_json::json!(cfg.providers.gitlab.enabled),
            "Enable/disable GitLab provider",
        ),
    );
    providers.insert(
        "gitlab.api_url".into(),
        key(
            "string",
            serde_json::json!(cfg.providers.gitlab.api_url),
            "GitLab API base URL (for self-hosted instances)",
        ),
    );
    providers.insert(
        "mcp_bridges.<name>.url".into(),
        key(
            "string",
            serde_json::json!(null),
            "HTTP/SSE URL for a remote MCP server",
        ),
    );
    providers.insert(
        "mcp_bridges.<name>.command".into(),
        key(
            "string",
            serde_json::json!(null),
            "Command to spawn a local MCP server (stdio transport)",
        ),
    );
    providers.insert(
        "mcp_bridges.<name>.args".into(),
        key(
            "array",
            serde_json::json!([]),
            "Arguments for the MCP server command",
        ),
    );
    providers.insert(
        "mcp_bridges.<name>.auth_env".into(),
        key(
            "string",
            serde_json::json!(null),
            "Environment variable name containing auth token for MCP server",
        ),
    );
    sections.insert(
            "providers".into(),
            SectionSchema {
                description:
                    "External context providers (GitHub, GitLab, Jira, MCP bridges, etc.). Set tokens via env vars (GITHUB_TOKEN, GITLAB_TOKEN). MCP bridges connect external MCP servers as context sources."
                        .into(),
                keys: providers,
            },
        );

    let mut loop_det = BTreeMap::new();
    loop_det.insert(
        "normal_threshold".into(),
        key(
            "u32",
            serde_json::json!(cfg.loop_detection.normal_threshold),
            "Repetitions before reducing output",
        ),
    );
    loop_det.insert(
        "reduced_threshold".into(),
        key(
            "u32",
            serde_json::json!(cfg.loop_detection.reduced_threshold),
            "Repetitions before further reducing output",
        ),
    );
    loop_det.insert(
        "blocked_threshold".into(),
        key(
            "u32",
            serde_json::json!(cfg.loop_detection.blocked_threshold),
            "Repetitions before blocking. 0 = disabled",
        ),
    );
    loop_det.insert(
        "window_secs".into(),
        key(
            "u64",
            serde_json::json!(cfg.loop_detection.window_secs),
            "Time window in seconds for loop detection",
        ),
    );
    loop_det.insert(
        "search_group_limit".into(),
        key(
            "u32",
            serde_json::json!(cfg.loop_detection.search_group_limit),
            "Maximum unique searches within a loop window",
        ),
    );
    loop_det.insert(
            "tool_total_limits".into(),
            key(
                "table",
                serde_json::json!({"ctx_read": 100, "ctx_search": 80, "ctx_shell": 50, "ctx_semantic_search": 60}),
                "Per-tool total call limits within a session. Keys are tool names, values are max calls",
            ),
        );
    sections.insert(
        "loop_detection".into(),
        SectionSchema {
            description: "Loop detection settings for preventing repeated identical tool calls"
                .into(),
            keys: loop_det,
        },
    );

    let mut updates = BTreeMap::new();
    updates.insert(
        "auto_update".into(),
        key(
            "bool",
            serde_json::json!(cfg.updates.auto_update),
            "Enable automatic updates (requires explicit opt-in)",
        ),
    );
    updates.insert(
        "check_interval_hours".into(),
        key(
            "u64",
            serde_json::json!(cfg.updates.check_interval_hours),
            "How often to check for updates (hours)",
        ),
    );
    updates.insert(
        "notify_only".into(),
        key(
            "bool",
            serde_json::json!(cfg.updates.notify_only),
            "Only notify about updates, don't install automatically",
        ),
    );
    sections.insert(
        "updates".into(),
        SectionSchema {
            description: "Automatic update configuration".into(),
            keys: updates,
        },
    );

    let mut boundary = BTreeMap::new();
    boundary.insert(
        "cross_project_search".into(),
        key(
            "bool",
            serde_json::json!(cfg.boundary_policy.cross_project_search),
            "Allow searching across project boundaries",
        ),
    );
    boundary.insert(
        "cross_project_import".into(),
        key(
            "bool",
            serde_json::json!(cfg.boundary_policy.cross_project_import),
            "Allow importing knowledge from other projects",
        ),
    );
    boundary.insert(
        "audit_cross_access".into(),
        key(
            "bool",
            serde_json::json!(cfg.boundary_policy.audit_cross_access),
            "Log audit events when cross-project access occurs",
        ),
    );
    boundary.insert(
        "universal_gotchas_enabled".into(),
        key(
            "bool",
            serde_json::json!(cfg.boundary_policy.universal_gotchas_enabled),
            "Load universal (cross-project) gotchas",
        ),
    );
    sections.insert(
        "boundary_policy".into(),
        SectionSchema {
            description: "Cross-project boundary and access control policies".into(),
            keys: boundary,
        },
    );

    let mut secret_det = BTreeMap::new();
    secret_det.insert(
        "enabled".into(),
        key(
            "bool",
            serde_json::json!(cfg.secret_detection.enabled),
            "Enable secret/credential detection in tool outputs",
        ),
    );
    secret_det.insert(
        "redact".into(),
        key(
            "bool",
            serde_json::json!(cfg.secret_detection.redact),
            "Redact detected secrets from output",
        ),
    );
    secret_det.insert(
        "custom_patterns".into(),
        key(
            "array",
            serde_json::json!(cfg.secret_detection.custom_patterns),
            "Additional regex patterns to detect as secrets",
        ),
    );
    sections.insert(
        "secret_detection".into(),
        SectionSchema {
            description: "Secret/credential detection and redaction settings".into(),
            keys: secret_det,
        },
    );

    let mut sensitivity = BTreeMap::new();
    sensitivity.insert(
        "enabled".into(),
        key(
            "bool",
            serde_json::json!(cfg.sensitivity.enabled),
            "Enable the per-item sensitivity policy floor (no-op when false)",
        ),
    );
    sensitivity.insert(
        "policy_floor".into(),
        key(
            "string",
            serde_json::json!(cfg.sensitivity.policy_floor.as_str()),
            "Block items at/above this level: public|internal|confidential|secret",
        ),
    );
    sensitivity.insert(
        "action".into(),
        key(
            "string",
            serde_json::json!(cfg.sensitivity.action.as_str()),
            "How to enforce the floor: redact (mask spans) or drop (withhold item)",
        ),
    );
    sections.insert(
        "sensitivity".into(),
        SectionSchema {
            description: "Per-item sensitivity model with a uniform policy floor (#212)".into(),
            keys: sensitivity,
        },
    );

    let mut gateway = BTreeMap::new();
    gateway.insert(
        "enabled".into(),
        key(
            "bool",
            serde_json::json!(cfg.gateway.enabled),
            "Enable the MCP Tool-Catalog Gateway (no-op when false)",
        ),
    );
    gateway.insert(
        "top_n".into(),
        key(
            "integer",
            serde_json::json!(cfg.gateway.top_n),
            "How many tools `ctx_tools find` returns per query (clamped 1..=50)",
        ),
    );
    gateway.insert(
        "cache_ttl_secs".into(),
        key(
            "integer",
            serde_json::json!(cfg.gateway.cache_ttl_secs),
            "Aggregated-catalog cache lifetime in seconds",
        ),
    );
    gateway.insert(
        "call_timeout_secs".into(),
        key(
            "integer",
            serde_json::json!(cfg.gateway.call_timeout_secs),
            "Per-operation timeout for downstream connect/list/call (seconds)",
        ),
    );
    sections.insert(
        "gateway".into(),
        SectionSchema {
            description: "MCP Tool-Catalog Gateway: aggregate + query-route downstream MCP servers (#210). Global-only.".into(),
            keys: gateway,
        },
    );

    let mut gateway_servers = BTreeMap::new();
    gateway_servers.insert(
        "name".into(),
        key(
            "string",
            serde_json::json!(""),
            "Stable server id; becomes the catalog namespace (`name::tool`)",
        ),
    );
    gateway_servers.insert(
        "transport".into(),
        key(
            "string",
            serde_json::json!("stdio"),
            "Transport: stdio (spawn command) or http (connect to url)",
        ),
    );
    gateway_servers.insert(
        "enabled".into(),
        key(
            "bool",
            serde_json::json!(true),
            "Per-server switch (default true)",
        ),
    );
    gateway_servers.insert(
        "command".into(),
        key(
            "string",
            serde_json::json!(""),
            "Executable to spawn (stdio transport)",
        ),
    );
    gateway_servers.insert(
        "args".into(),
        key(
            "array",
            serde_json::json!([]),
            "Arguments for the spawned command (stdio transport)",
        ),
    );
    gateway_servers.insert(
        "env".into(),
        key(
            "table",
            serde_json::json!({}),
            "Extra environment variables for the child process (stdio transport)",
        ),
    );
    gateway_servers.insert(
        "url".into(),
        key(
            "string",
            serde_json::json!(""),
            "Streamable-HTTP endpoint (http transport)",
        ),
    );
    gateway_servers.insert(
        "headers".into(),
        key(
            "table",
            serde_json::json!({}),
            "Extra request headers, e.g. Authorization (http transport)",
        ),
    );
    sections.insert(
        "gateway.servers".into(),
        SectionSchema {
            description: "Downstream MCP servers (array of tables: `[[gateway.servers]]`)".into(),
            keys: gateway_servers,
        },
    );

    let mut cloud = BTreeMap::new();
    cloud.insert(
        "contribute_enabled".into(),
        key(
            "bool",
            serde_json::json!(cfg.cloud.contribute_enabled),
            "Enable contributing anonymized stats to lean-ctx cloud",
        ),
    );
    cloud.insert(
        "auto_sync".into(),
        key(
            "bool",
            serde_json::json!(cfg.cloud.auto_sync),
            "Push the Personal Cloud (knowledge, commands, CEP, gotchas, buddy, feedback) silently once per day at session end (Pro; toggle: `lean-ctx cloud autosync on|off`)",
        ),
    );
    sections.insert(
        "cloud".into(),
        SectionSchema {
            description: "Cloud feature settings".into(),
            keys: cloud,
        },
    );

    let mut gain = BTreeMap::new();
    gain.insert(
            "auto_publish".into(),
            key(
                "bool",
                serde_json::json!(cfg.gain.auto_publish),
                "Automatically (re)publish your Wrapped recap when you run `lean-ctx gain` (opt-in, off by default; throttled and sends only an aggregate payload)",
            ),
        );
    gain.insert(
        "leaderboard".into(),
        key(
            "bool",
            serde_json::json!(cfg.gain.leaderboard),
            "When auto-publishing, also list the card on the public opt-in leaderboard",
        ),
    );
    gain.insert(
        "display_name".into(),
        key(
            "string?",
            serde_json::json!(cfg.gain.display_name),
            "Optional display name shown on your published card / leaderboard entry",
        ),
    );
    gain.insert(
        "auto_publish_interval_hours".into(),
        key(
            "u64",
            serde_json::json!(cfg.gain.auto_publish_interval_hours),
            "Minimum hours between automatic publishes (throttle; default 24)",
        ),
    );
    gain.insert(
        "last_auto_publish".into(),
        key(
            "string?",
            serde_json::json!(null),
            "Timestamp of the last automatic publish (written by lean-ctx for throttling — not meant to be edited)",
        ),
    );
    sections.insert(
        "gain".into(),
        SectionSchema {
            description: "Token-savings recap publishing (gain --publish / auto-publish)".into(),
            keys: gain,
        },
    );
}
