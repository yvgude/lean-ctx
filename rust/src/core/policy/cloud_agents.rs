// SPDX-License-Identifier: Apache-2.0
//! The agent, provider and routing members of the Cloud effective policy
//! (ent2-2, ent2-4, N1): `agents.allowed`, `security.allowed_providers` and
//! `security.require_proxy`.
//!
//! - `agents.allowed` (agent ids, `x-merge: intersect`, `"*"` = every
//!   agent): the MCP server refuses every tool call of a client whose agent
//!   id ([`agent_id`], from the `initialize` handshake's `clientInfo.name`)
//!   is not listed. An empty list allows no agent.
//! - `security.allowed_providers` (`x-merge: intersect`): the proxy refuses
//!   a request whose upstream is not allowed. The provider is derived from
//!   the host the request is actually sent to ([`provider_for_host`]), never
//!   from a label or registry id in local config, which a user can name
//!   freely. An entry is either a provider id from the host table
//!   (`anthropic`, `openai`, `google`, …) or an exact host name (an entry
//!   with a `.`, or `localhost`, for example `llm.example.com`). A host that
//!   is not in the table is refused while the member is set, unless the
//!   policy lists that host. A listed host allows any port on it; an entry
//!   (and so a listed host) is at most 64 bytes, the policy schema's limit.
//! - `security.require_proxy` (`x-merge: true_wins`): the MCP server refuses
//!   every tool call of a client whose model traffic is not routed through
//!   the LeanCTX proxy. Whether an agent is routed is answered by the probe
//!   the binary registers ([`set_proxy_routing_probe`]; it reads the agent's
//!   own config, where `lean-ctx proxy enable` writes the proxy URL). An
//!   agent the probe cannot confirm, or any agent when no probe is
//!   registered, counts as not routed (fail closed).
//!
//! Both are read from the installed floor ([`super::cloud_floor`]). With a
//! project the policy of the vault linked to it applies; without one (the
//! proxy cannot tell which checkout a request belongs to) every policy of the
//! floor applies at once, default and per vault, so no vault's narrower list
//! is lost. Fail-closed: a damaged, missing-but-required or older floor, or
//! a data directory that cannot be resolved, refuses instead of dropping the
//! rules.
//!
//! An agent names itself in the handshake, so `agents.allowed` is a policy
//! aid, not a security boundary; like every rule enforced by LeanCTX this
//! covers access through LeanCTX only, a bypass needs host isolation.
//!
//! Monolith: it needs the installed floor, the vault map and the data
//! directory, all engine state (`core::policy::cloud_floor`).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use lean_ctx_protocol::cloud::effective_policy::EffectivePolicyV1;

use super::cloud_floor;

/// The `agents.allowed` entry that allows every agent.
pub const ANY_AGENT: &str = "*";

/// What identifies one version of a floor file on disk: modification time,
/// length and (on Unix) inode, so an atomic rename is seen even within one
/// mtime tick. `None` for a missing file.
type FileStamp = Option<(SystemTime, u64, u64)>;

fn stamp(path: &Path) -> Result<FileStamp, ()> {
    match std::fs::metadata(path) {
        Ok(meta) => {
            #[cfg(unix)]
            let ino = std::os::unix::fs::MetadataExt::ino(&meta);
            #[cfg(not(unix))]
            // No inode outside Unix: mtime and length alone identify a
            // version there, so a same-size rewrite within one mtime tick
            // can be missed until the next change; the installer writes a
            // new file with a fresh mtime, which is seen.
            let ino = 0;
            Ok(Some((meta.modified().map_err(|_| ())?, meta.len(), ino)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

/// The checked floor of one directory, kept while neither file changes.
struct CachedFloor {
    dir: PathBuf,
    stamps: (FileStamp, FileStamp),
    text: Option<String>,
    floor: Result<Option<cloud_floor::Floor>, String>,
}

static FLOOR_CACHE: Mutex<Option<CachedFloor>> = Mutex::new(None);

/// `floor.json` text and its check against `required.json`, re-read only
/// when either file's stamp changed (both are checked on every call).
fn checked_floor(floor_dir: &Path) -> Result<(Option<String>, Option<cloud_floor::Floor>), String> {
    let stamps = (
        stamp(&floor_dir.join(cloud_floor::FLOOR_FILE)),
        stamp(&floor_dir.join(cloud_floor::REQUIRED_FILE)),
    );
    let stamps = match stamps {
        (Ok(a), Ok(b)) => Some((a, b)),
        _ => None,
    };
    let mut cache = FLOOR_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let (Some(stamps), Some(hit)) = (stamps, cache.as_ref())
        && hit.dir == floor_dir
        && hit.stamps == stamps
    {
        return hit.floor.clone().map(|floor| (hit.text.clone(), floor));
    }
    let (text, required) = cloud_floor::read(floor_dir)?;
    let floor = cloud_floor::check(text.as_deref(), required.as_deref());
    if let Some(stamps) = stamps {
        *cache = Some(CachedFloor {
            dir: floor_dir.to_path_buf(),
            stamps,
            text: text.clone(),
            floor: floor.clone(),
        });
    }
    floor.map(|floor| (text, floor))
}

/// The policy of the floor in `floor_dir` that applies to `vault_id` (its
/// own, else the default). `Ok(None)` when no Cloud policy is installed.
pub(super) fn vault_policy_in(
    floor_dir: &Path,
    vault_id: &str,
) -> Result<Option<EffectivePolicyV1>, String> {
    let (_, floor) = checked_floor(floor_dir)?;
    Ok(floor.map(|floor| floor.policy.for_vault(Some(vault_id)).clone()))
}

/// The policies of the floor in `floor_dir` that apply to `project` (the
/// linked vault's, else the default), or to any project when `None`.
/// `Ok(None)` when no Cloud policy is installed.
pub(super) fn policies_in(
    floor_dir: &Path,
    project: Option<&Path>,
) -> Result<Option<Vec<EffectivePolicyV1>>, String> {
    let (text, floor) = checked_floor(floor_dir)?;
    let Some(floor) = floor else {
        return Ok(None);
    };
    let policies = match project {
        Some(project) => {
            let vault = text.as_deref().and_then(|text| {
                let cloud_data = floor_dir.parent()?.parent()?;
                cloud_floor::vault_for(text, cloud_data, project)
            });
            vec![floor.policy.for_vault(vault.as_deref()).clone()]
        }
        None => std::iter::once(floor.policy.effective)
            .chain(floor.policy.vaults.into_iter().map(|v| v.effective))
            .collect(),
    };
    Ok(Some(policies))
}

/// Host table on an MCP client or host name (SPEC-ENGINE §3.6): the agent
/// id a known client stands for.
pub fn known_agent(name: &str) -> Option<&'static str> {
    let name = name.trim().to_ascii_lowercase();
    Some(match name.as_str() {
        "claude" | "claude-code" | "claude code" => "claude-code",
        "codex" | "codex-cli" | "codex-mcp-client" => "codex",
        "cursor" | "cursor-agent" | "cursor-vscode" => "cursor",
        "gemini" | "gemini-cli" => "gemini-cli",
        "copilot" | "gh copilot" | "github-copilot" => "copilot",
        "opencode" => "opencode",
        _ => return None,
    })
}

/// The agent id `agents.allowed` is matched against: a known client's id,
/// else the client name in the policy's name form (lowercase, spaces as
/// `-`, only `a-z 0-9 _ . : -`, at most 64 bytes), `unknown` when nothing
/// is left.
pub fn agent_id(client_name: &str) -> String {
    if let Some(known) = known_agent(client_name) {
        return known.to_string();
    }
    let id: String = client_name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c == ' ' { '-' } else { c })
        .filter(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | ':' | '-')
        })
        .take(64)
        .collect();
    let id = id.trim_start_matches(['_', '.', ':', '-']);
    if id.is_empty() {
        "unknown".to_string()
    } else {
        id.to_string()
    }
}

/// Why the client `client_name` may not use LeanCTX in `project`, or `None`
/// when every applying policy allows its agent (or none sets the member).
pub fn agent_refusal_in(
    floor_dir: &Path,
    project: Option<&Path>,
    client_name: &str,
) -> Option<String> {
    let policies = match policies_in(floor_dir, project) {
        Ok(Some(policies)) => policies,
        Ok(None) => return None,
        Err(error) => {
            return Some(format!(
                "{error}; every agent is refused until it is repaired"
            ));
        }
    };
    let agent = agent_id(client_name);
    let refused = policies.iter().any(|policy| {
        policy
            .agents
            .as_ref()
            .and_then(|a| a.allowed.as_ref())
            .is_some_and(|allowed| !allowed.iter().any(|a| a == ANY_AGENT || *a == agent))
    });
    refused.then(|| format!("agent {agent:?} is not allowed by the team policy (agents.allowed)"))
}

/// Whether the model traffic of the agent with this id ([`agent_id`]) is
/// routed through the LeanCTX proxy right now.
pub type ProxyRoutingProbe = fn(&str) -> bool;

static PROXY_ROUTING_PROBE: OnceLock<ProxyRoutingProbe> = OnceLock::new();

/// Registers the probe `security.require_proxy` asks (once per process; the
/// binary does this at start, the Engine cannot read agent config itself).
pub fn set_proxy_routing_probe(probe: ProxyRoutingProbe) {
    let _ = PROXY_ROUTING_PROBE.set(probe);
}

fn agent_is_routed(agent: &str) -> bool {
    PROXY_ROUTING_PROBE.get().is_some_and(|probe| probe(agent))
}

/// Why the client `client_name` may not use LeanCTX in `project` under
/// `security.require_proxy`, or `None` when no applying policy requires the
/// proxy or `routed` confirms its agent is routed through it.
pub fn proxy_requirement_refusal_in(
    floor_dir: &Path,
    project: Option<&Path>,
    client_name: &str,
    routed: impl Fn(&str) -> bool,
) -> Option<String> {
    let policies = match policies_in(floor_dir, project) {
        Ok(Some(policies)) => policies,
        Ok(None) => return None,
        Err(error) => {
            return Some(format!(
                "{error}; every agent is refused until it is repaired"
            ));
        }
    };
    let required = policies
        .iter()
        .any(|policy| policy.security.require_proxy == Some(true));
    if !required {
        return None;
    }
    let agent = agent_id(client_name);
    (!routed(&agent)).then(|| {
        format!(
            "the team policy requires model traffic through the LeanCTX proxy \
             (security.require_proxy) and agent {agent:?} is not routed through it"
        )
    })
}

/// Fail closed: without a data directory the installed floor cannot be found.
const NO_DATA_DIR: &str =
    "the LeanCTX data directory cannot be resolved, so the team policy cannot be checked";

/// [`agent_refusal_in`] for the floor of this installation.
pub fn agent_refusal(project: Option<&Path>, client_name: &str) -> Option<String> {
    let Some(dir) = cloud_floor::floor_dir() else {
        return Some(format!(
            "{NO_DATA_DIR}; every agent is refused until it is repaired"
        ));
    };
    agent_refusal_in(&dir, project, client_name)
}

/// [`proxy_requirement_refusal_in`] for the floor of this installation and
/// the registered routing probe.
pub fn proxy_requirement_refusal(project: Option<&Path>, client_name: &str) -> Option<String> {
    let Some(dir) = cloud_floor::floor_dir() else {
        return Some(format!(
            "{NO_DATA_DIR}; every agent is refused until it is repaired"
        ));
    };
    proxy_requirement_refusal_in(&dir, project, client_name, agent_is_routed)
}

/// Canonical host table: the provider id a known model API host belongs to.
/// Subdomain rules (`*.openai.azure.com`) match any label in front.
pub fn provider_for_host(host: &str) -> Option<&'static str> {
    const EXACT: &[(&str, &str)] = &[
        ("api.anthropic.com", "anthropic"),
        ("api.openai.com", "openai"),
        ("chatgpt.com", "openai"),
        ("generativelanguage.googleapis.com", "google"),
        ("aiplatform.googleapis.com", "google"),
        ("api.mistral.ai", "mistral"),
        ("api.deepseek.com", "deepseek"),
        ("api.x.ai", "xai"),
        ("api.groq.com", "groq"),
        ("openrouter.ai", "openrouter"),
        ("api.cohere.com", "cohere"),
        ("api.cohere.ai", "cohere"),
        ("api.together.xyz", "together"),
        ("api.fireworks.ai", "fireworks"),
        ("localhost", "local"),
        ("127.0.0.1", "local"),
        ("::1", "local"),
    ];
    const SUFFIX: &[(&str, &str)] = &[
        (".openai.azure.com", "azure"),
        (".services.ai.azure.com", "azure"),
        (".cognitiveservices.azure.com", "azure"),
        ("-aiplatform.googleapis.com", "google"),
    ];
    if let Some((_, provider)) = EXACT.iter().find(|(h, _)| *h == host) {
        return Some(provider);
    }
    if is_bedrock_runtime_host(host) {
        return Some("bedrock");
    }
    SUFFIX
        .iter()
        .find(|(suffix, _)| host.len() > suffix.len() && host.ends_with(suffix))
        .map(|(_, provider)| *provider)
}

/// `bedrock-runtime[-fips].<region>.amazonaws.com`, exactly four labels; any
/// other amazonaws.com host (an S3 bucket, for example) is not Bedrock.
fn is_bedrock_runtime_host(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    matches!(
        labels.as_slice(),
        [service, region, "amazonaws", "com"]
            if matches!(*service, "bedrock-runtime" | "bedrock-runtime-fips")
                && is_aws_region(region)
    )
}

/// An AWS region name: `[a-z]{2}(-[a-z]+)+-\d+` (`us-east-1`, `us-gov-west-1`).
fn is_aws_region(region: &str) -> bool {
    let parts: Vec<&str> = region.split('-').collect();
    let [country, middle @ .., number] = parts.as_slice() else {
        return false;
    };
    country.len() == 2
        && country.bytes().all(|b| b.is_ascii_lowercase())
        && !middle.is_empty()
        && middle
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase()))
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
}

/// The host an upstream base URL sends to, as an HTTP client resolves it:
/// lowercase, without scheme, userinfo, port or trailing dot. `None` when no
/// plain host name or IP literal is left (such an upstream counts as unknown).
pub fn upstream_host(upstream_base: &str) -> Option<String> {
    let rest = upstream_base.trim();
    let rest = rest.split_once("://").map_or(rest, |(_, r)| r);
    // A backslash ends the authority too (WHATWG URL parsing for http/https).
    let authority = rest.split(['/', '\\', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(v6) = host_port.strip_prefix('[') {
        let (inside, _) = v6.split_once(']')?;
        if !inside
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
        {
            return None;
        }
        inside
    } else {
        // A `:` outside IPv6 brackets only separates the port; a second one
        // (or a bare IPv6 literal) is not a plain host.
        let host = host_port.rsplit_once(':').map_or(host_port, |(h, _)| h);
        if host.contains(':') {
            return None;
        }
        host
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let plain = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'));
    plain.then_some(host)
}

/// Whether one `allowed_providers` entry admits `host` (provider id via the
/// host table, else the exact host name).
fn entry_admits(entry: &str, host: &str, provider: Option<&str>) -> bool {
    let entry = entry.trim().to_ascii_lowercase();
    let entry = entry.trim_end_matches('.');
    entry == host || provider.is_some_and(|p| p == entry)
}

/// Why a request sent to `upstream_base` may not leave, or `None` when every
/// policy of the floor allows its host (or none sets the member). `Err` when
/// the floor cannot be trusted (fail closed).
pub fn provider_refusal_in(
    floor_dir: &Path,
    upstream_base: &str,
) -> Result<Option<String>, String> {
    let Some(policies) = policies_in(floor_dir, None)? else {
        return Ok(None);
    };
    let host = upstream_host(upstream_base);
    let provider = host.as_deref().and_then(provider_for_host);
    let refused = policies.iter().any(|policy| {
        policy
            .security
            .allowed_providers
            .as_ref()
            .is_some_and(|allowed| {
                host.as_deref()
                    .is_none_or(|h| !allowed.iter().any(|e| entry_admits(e, h, provider)))
            })
    });
    Ok(refused.then(|| {
        let shown = host.as_deref().unwrap_or("an unparsable upstream");
        match provider {
            Some(p) => format!(
                "provider {p:?} ({shown}) is not allowed by the team policy (security.allowed_providers)"
            ),
            None => format!(
                "host {shown:?} is not a known model provider and is not listed in the team policy (security.allowed_providers)"
            ),
        }
    }))
}

/// [`provider_refusal_in`] for the floor of this installation.
pub fn provider_refusal(upstream_base: &str) -> Result<Option<String>, String> {
    let Some(dir) = cloud_floor::floor_dir() else {
        return Err(NO_DATA_DIR.to_string());
    };
    provider_refusal_in(&dir, upstream_base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Test builders take owned values so call sites read as plain `json!(...)` literals.
    #[allow(clippy::needless_pass_by_value)]
    fn install(floor_dir: &Path, effective: serde_json::Value, vaults: serde_json::Value) {
        std::fs::create_dir_all(floor_dir).unwrap();
        let floor = json!({
            "schema": cloud_floor::FLOOR_SCHEMA, "version": 3,
            "digest": format!("sha256:{}", "a".repeat(64)),
            "not_after": "2099-01-01T00:00:00Z",
            "policy": {"effective": effective, "vaults": vaults}
        });
        std::fs::write(floor_dir.join(cloud_floor::FLOOR_FILE), floor.to_string()).unwrap();
        std::fs::write(
            floor_dir.join(cloud_floor::REQUIRED_FILE),
            r#"{"version":3}"#,
        )
        .unwrap();
    }

    #[allow(clippy::needless_pass_by_value)]
    fn policy(security: serde_json::Value, agents: Option<serde_json::Value>) -> serde_json::Value {
        let mut p = json!({
            "schema": "leanctx.cloud.policy/v1",
            "security": security, "context": {}, "sharing": {}
        });
        if let Some(agents) = agents {
            p["agents"] = agents;
        }
        p
    }

    #[test]
    fn missing_data_dir_refusal_names_the_team_policy() {
        assert!(NO_DATA_DIR.contains("the team policy cannot be checked"));
        assert!(!NO_DATA_DIR.contains("Cloud"));
    }

    #[test]
    fn agent_ids_come_from_the_host_table_or_the_name_form() {
        assert_eq!(agent_id("claude-code"), "claude-code");
        assert_eq!(agent_id(" Claude Code "), "claude-code");
        assert_eq!(agent_id("codex-mcp-client"), "codex");
        assert_eq!(agent_id("Cursor-VSCode"), "cursor");
        assert_eq!(agent_id("Zed Editor"), "zed-editor");
        assert_eq!(agent_id("acp:zed"), "acp:zed");
        assert_eq!(agent_id("-ünï-"), "n-");
        assert_eq!(agent_id(""), "unknown");
        assert_eq!(agent_id("€€"), "unknown");
        assert_eq!(agent_id(&"x".repeat(100)).len(), 64);
    }

    #[test]
    fn allowed_agents_admit_listed_ids_and_the_wildcard() {
        let dir = tempfile::tempdir().unwrap();
        let floor_dir = dir.path().join("cloud").join("policy").join("effective");
        // Nothing installed: no rule.
        assert_eq!(agent_refusal_in(&floor_dir, None, "anything"), None);
        install(
            &floor_dir,
            policy(
                json!({}),
                Some(json!({"allowed": ["claude-code", "codex"]})),
            ),
            json!([]),
        );
        assert_eq!(
            agent_refusal_in(&floor_dir, Some(dir.path()), "claude-code"),
            None
        );
        assert_eq!(agent_refusal_in(&floor_dir, None, "codex-cli"), None);
        let refusal = agent_refusal_in(&floor_dir, None, "Cursor").unwrap();
        assert!(
            refusal.contains("\"cursor\"") && refusal.contains("agents.allowed"),
            "{refusal}"
        );
        install(
            &floor_dir,
            policy(json!({}), Some(json!({"allowed": ["*"]}))),
            json!([]),
        );
        assert_eq!(agent_refusal_in(&floor_dir, None, "Cursor"), None);
        install(
            &floor_dir,
            policy(json!({}), Some(json!({"allowed": []}))),
            json!([]),
        );
        assert!(agent_refusal_in(&floor_dir, None, "claude-code").is_some());
        // A policy without the member: no rule.
        install(
            &floor_dir,
            policy(json!({"pii": "redact"}), None),
            json!([]),
        );
        assert_eq!(agent_refusal_in(&floor_dir, None, "Cursor"), None);
    }

    #[test]
    fn without_a_project_every_vault_policy_applies() {
        let dir = tempfile::tempdir().unwrap();
        let floor_dir = dir.path().join("cloud").join("policy").join("effective");
        install(
            &floor_dir,
            policy(json!({"allowed_providers": ["anthropic", "openai"]}), None),
            json!([{
                "vault_id": "0".repeat(32),
                "effective": policy(json!({"allowed_providers": ["anthropic"]}), Some(json!({"allowed": ["codex"]})))
            }]),
        );
        assert_eq!(
            provider_refusal_in(&floor_dir, "https://api.anthropic.com").unwrap(),
            None
        );
        let refusal = provider_refusal_in(&floor_dir, "https://api.openai.com/v1")
            .unwrap()
            .unwrap();
        assert!(refusal.contains("\"openai\""), "{refusal}");
        assert!(
            provider_refusal_in(&floor_dir, "https://foundry.example.com")
                .unwrap()
                .is_some()
        );
        // An unlinked project gets the default policy: no agent rule there.
        assert_eq!(
            agent_refusal_in(&floor_dir, Some(dir.path()), "cursor"),
            None
        );
        // Without a project the vault's narrower agent list applies too.
        assert!(agent_refusal_in(&floor_dir, None, "cursor").is_some());
    }

    #[test]
    fn a_damaged_floor_refuses_every_agent_and_provider() {
        let dir = tempfile::tempdir().unwrap();
        let floor_dir = dir.path().join("cloud").join("policy").join("effective");
        install(&floor_dir, policy(json!({}), None), json!([]));
        std::fs::write(floor_dir.join(cloud_floor::FLOOR_FILE), "{").unwrap();
        assert!(agent_refusal_in(&floor_dir, None, "claude-code").is_some());
        assert!(provider_refusal_in(&floor_dir, "https://api.anthropic.com").is_err());
        std::fs::remove_file(floor_dir.join(cloud_floor::FLOOR_FILE)).unwrap();
        assert!(agent_refusal_in(&floor_dir, None, "claude-code").is_some());
        assert!(provider_refusal_in(&floor_dir, "https://api.anthropic.com").is_err());
    }

    #[test]
    fn upstream_hosts_parse_as_an_http_client_resolves_them() {
        assert_eq!(
            upstream_host("https://api.openai.com/v1").as_deref(),
            Some("api.openai.com")
        );
        assert_eq!(
            upstream_host("HTTPS://API.Anthropic.com.:443").as_deref(),
            Some("api.anthropic.com")
        );
        assert_eq!(
            upstream_host("http://api.openai.com@evil.host:8080/x").as_deref(),
            Some("evil.host")
        );
        // The authority ends at a backslash: the host is evil.com, not the provider.
        assert_eq!(
            upstream_host("https://evil.com\\@api.openai.com").as_deref(),
            Some("evil.com")
        );
        assert_eq!(upstream_host("http://[::1]:11434").as_deref(), Some("::1"));
        assert_eq!(
            upstream_host("api.mistral.ai").as_deref(),
            Some("api.mistral.ai")
        );
        assert_eq!(upstream_host("https://api.openai.com%2eevil.host"), None);
        assert_eq!(upstream_host("https://"), None);
        // A `:` outside IPv6 brackets is only the port separator.
        assert_eq!(upstream_host("http://::1:11434"), None);
        assert_eq!(upstream_host("http://evil.host:x:443"), None);
        assert_eq!(provider_for_host("api.openai.com"), Some("openai"));
        assert_eq!(provider_for_host("team.openai.azure.com"), Some("azure"));
        assert_eq!(provider_for_host("openai.azure.com"), None);
        assert_eq!(provider_for_host("api.openai.com.evil.host"), None);
        assert_eq!(provider_for_host("evilapi.openai.com"), None);
    }

    #[test]
    fn bedrock_is_only_the_regional_runtime_host() {
        for host in [
            "bedrock-runtime.us-east-1.amazonaws.com",
            "bedrock-runtime-fips.us-east-1.amazonaws.com",
            "bedrock-runtime.eu-central-1.amazonaws.com",
            "bedrock-runtime.us-gov-west-1.amazonaws.com",
        ] {
            assert_eq!(provider_for_host(host), Some("bedrock"), "{host}");
        }
        for host in [
            "bedrock-runtime.mybucket.s3.amazonaws.com",
            "bedrock-runtime.eu-central-1.extra.amazonaws.com",
            "bedrock-runtime..amazonaws.com",
            "bedrock-runtime.amazonaws.com",
            "bedrock-runtime.mybucket.amazonaws.com",
            "bedrock-runtime.us-1.amazonaws.com",
            "bedrock-runtime.us-east-.amazonaws.com",
            "bedrock-runtime.usa-east-1.amazonaws.com",
            "bedrock.us-east-1.amazonaws.com",
        ] {
            assert_eq!(provider_for_host(host), None, "{host}");
        }
    }

    #[test]
    fn the_provider_comes_from_the_upstream_host_not_from_local_names() {
        let dir = tempfile::tempdir().unwrap();
        let floor_dir = dir.path().join("cloud").join("policy").join("effective");
        install(
            &floor_dir,
            policy(
                json!({"allowed_providers": ["anthropic", "llm.example.com"]}),
                None,
            ),
            json!([]),
        );
        // `proxy.openai_upstream = evil.host` (or `anthropic_upstream`): the
        // wire label says openai/anthropic, the host decides.
        let refusal = provider_refusal_in(&floor_dir, "https://evil.host/v1")
            .unwrap()
            .unwrap();
        assert!(refusal.contains("\"evil.host\""), "{refusal}");
        // A registry entry named `anthropic` with any base_url: refused by host.
        assert!(
            provider_refusal_in(&floor_dir, "https://anthropic.proxy.attacker.net")
                .unwrap()
                .is_some()
        );
        assert!(
            provider_refusal_in(&floor_dir, "https://api.anthropic.com.attacker.net")
                .unwrap()
                .is_some()
        );
        // The real host passes; an explicitly listed unknown host passes.
        assert_eq!(
            provider_refusal_in(&floor_dir, "https://api.anthropic.com").unwrap(),
            None
        );
        assert_eq!(
            provider_refusal_in(&floor_dir, "https://LLM.example.com:8443/v1").unwrap(),
            None
        );
        // Loopback is the `local` provider and must be listed like any other.
        assert!(
            provider_refusal_in(&floor_dir, "http://127.0.0.1:11434")
                .unwrap()
                .is_some()
        );
        assert!(
            provider_refusal_in(&floor_dir, "not a url at all")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn require_proxy_refuses_an_agent_that_is_not_routed() {
        let dir = tempfile::tempdir().unwrap();
        let floor_dir = dir.path().join("cloud").join("policy").join("effective");
        install(&floor_dir, policy(json!({}), None), json!([]));
        // Not required: nobody is asked.
        assert_eq!(
            proxy_requirement_refusal_in(&floor_dir, None, "cursor", |_| false),
            None
        );
        install(
            &floor_dir,
            policy(json!({"require_proxy": false}), None),
            json!([{
                "vault_id": "0".repeat(32),
                "effective": policy(json!({"require_proxy": true}), None)
            }]),
        );
        // The probe is asked with the agent id, not the raw client name.
        let routed = |agent: &str| agent == "claude-code";
        assert_eq!(
            proxy_requirement_refusal_in(&floor_dir, None, "Claude Code", routed),
            None
        );
        let refusal =
            proxy_requirement_refusal_in(&floor_dir, None, "codex-mcp-client", routed).unwrap();
        assert!(refusal.contains("security.require_proxy"), "{refusal}");
        assert!(refusal.contains("\"codex\""), "{refusal}");
        // An unlinked project gets the default policy, which does not require it.
        assert_eq!(
            proxy_requirement_refusal_in(&floor_dir, Some(dir.path()), "codex", |_| false),
            None
        );
    }

    #[test]
    fn require_proxy_fails_closed_without_a_floor_or_a_probe() {
        let dir = tempfile::tempdir().unwrap();
        let floor_dir = dir.path().join("cloud").join("policy").join("effective");
        install(&floor_dir, policy(json!({}), None), json!([]));
        std::fs::write(floor_dir.join(cloud_floor::FLOOR_FILE), "{").unwrap();
        assert!(proxy_requirement_refusal_in(&floor_dir, None, "claude-code", |_| true).is_some());
        // No probe is registered in this test binary: nothing is confirmed.
        assert!(PROXY_ROUTING_PROBE.get().is_none());
        assert!(!agent_is_routed("claude-code"));
    }

    #[test]
    fn the_parsed_floor_is_cached_until_a_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let floor_dir = dir.path().join("cloud").join("policy").join("effective");
        install(
            &floor_dir,
            policy(json!({}), Some(json!({"allowed": ["codex"]}))),
            json!([]),
        );
        assert!(agent_refusal_in(&floor_dir, None, "cursor").is_some());
        assert!(agent_refusal_in(&floor_dir, None, "cursor").is_some());
        // A rewrite (new inode via rename, as the installer does) is seen at once.
        let tmp = floor_dir.join("floor.json.tmp");
        let floor = json!({
            "schema": cloud_floor::FLOOR_SCHEMA, "version": 3,
            "digest": format!("sha256:{}", "a".repeat(64)),
            "not_after": "2099-01-01T00:00:00Z",
            "policy": {"effective": policy(json!({}), Some(json!({"allowed": ["cursor"]}))), "vaults": []}
        });
        std::fs::write(&tmp, floor.to_string()).unwrap();
        std::fs::rename(&tmp, floor_dir.join(cloud_floor::FLOOR_FILE)).unwrap();
        assert_eq!(agent_refusal_in(&floor_dir, None, "cursor"), None);
        // Damage after a cached good read still fails closed.
        std::fs::write(floor_dir.join(cloud_floor::FLOOR_FILE), "{").unwrap();
        assert!(agent_refusal_in(&floor_dir, None, "cursor").is_some());
    }
}
