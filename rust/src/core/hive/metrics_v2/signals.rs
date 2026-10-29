// SPDX-License-Identifier: Apache-2.0
//! K10, K12 and K13: counts the stores do not keep, recorded where they
//! happen while engine metrics are on (the same switch as [`super::secrets`]).
//!
//! - K10 `refetch[]`: per filter (a read mode, `shell_output`,
//!   `search_output`), outputs served truncated — a compressed `ctx_read`
//!   view, a firewalled tool output with a `ctx_expand` pointer — and the
//!   ones the agent fetched back in full (the compressed read's full
//!   re-read, a `ctx_expand` of the archive);
//! - K12 `policy_blocks[]`: per rule, refusals in force (`blocked`) and the
//!   ones the same rule in monitor/warn mode only evaluated (`would_block`):
//!   the shell allowlist (`gateway.security_violation`), the path jail
//!   (`gateway.path_jail`), secret-like paths and secret filters
//!   (`security.block_secrets`), destination and egress admission
//!   (`gateway.destination`, `gateway.egress`);
//! - K13 `unregulated[]`: what no *enforcing* rule covers at decision time
//!   (monitor mode does not regulate): programs a shell command ran
//!   (`shell_security = off`, or no allowlist), by known-program name; tool
//!   calls of a host family without an intercepting hook (anything but
//!   replace mode with the native-tool deny hook), by host family; calls
//!   the MCP gateway relayed to a downstream server, named only when the
//!   workspace's signed Hive policy names that server, else `other`.
//!
//! Each record is `{ts, kind, name, project}` in `signals.jsonl` (0600,
//! bounded); `name` is a closed wire name (`FilterName`, `RuleId`,
//! `UnregulatedName`) made here, never a path, a command line or a mode
//! argument. The collector reads the log per hour and prunes it.

use std::io::Write as _;
use std::path::Path;

use lean_ctx_protocol::hive::metrics_v2::{
    FilterName, HOST_FAMILIES, KNOWN_PROGRAMS, OTHER, ReadMode, RuleId, UnregulatedKind,
    UnregulatedName, host_family,
};
use serde::{Deserialize, Serialize};

use super::buffer::dir_in;
use crate::core::hive::state::write_private;

pub(crate) const SIGNALS_FILE: &str = "signals.jsonl";
/// The log stops growing here; the collector prunes what it has read.
const MAX_SIGNALS_BYTES: u64 = 4 * 1024 * 1024;

/// What one record counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Truncated,
    Refetched,
    Blocked,
    WouldBlock,
    UnregulatedProgram,
    UnregulatedHost,
    UnregulatedMcpServer,
}

/// One record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Signal {
    pub(crate) ts: String,
    pub(crate) kind: Kind,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) project: Option<String>,
}

impl Signal {
    /// The name as the wire type of its kind, `None` when it is not one
    /// (a record is never trusted for more than it can be).
    pub(crate) fn filter(&self) -> Option<FilterName> {
        matches!(self.kind, Kind::Truncated | Kind::Refetched)
            .then(|| FilterName::new(&self.name))
            .flatten()
    }

    pub(crate) fn rule(&self) -> Option<RuleId> {
        matches!(self.kind, Kind::Blocked | Kind::WouldBlock)
            .then(|| RuleId::new(&self.name))
            .flatten()
    }

    /// K13 of any kind: the wire kind and a name that kind allows.
    pub(crate) fn unregulated(&self) -> Option<(UnregulatedKind, UnregulatedName)> {
        let name = self.name.as_str();
        let (kind, fits) = match self.kind {
            Kind::UnregulatedProgram => (UnregulatedKind::Program, KNOWN_PROGRAMS.contains(&name)),
            Kind::UnregulatedHost => (UnregulatedKind::Host, HOST_FAMILIES.contains(&name)),
            // The shape rule of [`UnregulatedName`] (no path, no token).
            Kind::UnregulatedMcpServer => (UnregulatedKind::McpServer, true),
            _ => return None,
        };
        fits.then(|| UnregulatedName::new(name))
            .flatten()
            .map(|n| (kind, n))
    }
}

/// The filter of a read mode (`lines:10-20` is `lines`).
pub(crate) fn read_mode_filter(mode: &str) -> FilterName {
    let mode = ReadMode::of(mode);
    let wire = serde_json::to_value(mode)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| OTHER.to_string());
    FilterName::or_other(&wire)
}

/// The filter of a firewalled tool output.
pub(crate) fn output_filter(tool: &str) -> FilterName {
    FilterName::or_other(match tool {
        "ctx_shell" | "ctx_execute" => "shell_output",
        "ctx_search" => "search_output",
        _ => OTHER,
    })
}

/// A program as the known-program table names it, else `other`.
pub(crate) fn program_name(base: &str) -> UnregulatedName {
    let base = base.rsplit(['/', '\\']).next().unwrap_or(base);
    let base = base.strip_suffix(".exe").unwrap_or(base);
    UnregulatedName::new(base)
        .filter(|_| KNOWN_PROGRAMS.contains(&base))
        .unwrap_or_else(UnregulatedName::other)
}

/// Whether anything is recorded now (engine metrics on), so a caller can
/// skip work whose only use is a record.
pub(crate) fn recording() -> bool {
    super::secrets::sink().is_some()
}

fn record(kind: Kind, names: &[&str]) {
    if names.is_empty() {
        return;
    }
    let Some(dir) = super::secrets::sink() else {
        return;
    };
    let path = dir.join(SIGNALS_FILE);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_SIGNALS_BYTES) {
        return;
    }
    let project = crate::core::savings_ledger::project_scope::current_project_key();
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut lines = String::new();
    for name in names {
        let signal = Signal {
            ts: ts.clone(),
            kind,
            name: (*name).to_string(),
            project: project.clone(),
        };
        if let Ok(line) = serde_json::to_string(&signal) {
            lines.push_str(&line);
            lines.push('\n');
        }
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path);
    if let Ok(mut file) = file {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        let _ = file.write_all(lines.as_bytes());
    }
}

/// K10: an output served truncated, by filter.
pub(crate) fn truncated(filter: &FilterName) {
    record(Kind::Truncated, &[filter.as_str()]);
}

/// K10: a truncated output fetched back in full, by filter.
pub(crate) fn refetched(filter: &FilterName) {
    record(Kind::Refetched, &[filter.as_str()]);
}

/// K12: a refusal in force.
pub(crate) fn blocked(rule: &RuleId) {
    record(Kind::Blocked, &[rule.as_str()]);
}

/// K12: a refusal a rule in monitor mode would have made.
pub(crate) fn would_block(rule: &RuleId) {
    record(Kind::WouldBlock, &[rule.as_str()]);
}

/// K13: programs a command ran that no rule covers.
pub(crate) fn unregulated_programs(programs: &[UnregulatedName]) {
    let names: Vec<&str> = programs.iter().map(UnregulatedName::as_str).collect();
    record(Kind::UnregulatedProgram, &names);
}

/// K12: a refusal by `rule`, in force (`enforcing`) or in monitor mode.
pub(crate) fn refused(rule: &RuleId, enforcing: bool) {
    if enforcing {
        blocked(rule);
    } else {
        would_block(rule);
    }
}

/// The rule the path jail decides under.
pub(crate) fn path_jail_rule() -> RuleId {
    RuleId::or_other("gateway.path_jail")
}

/// The rule secret-like paths and secret filters decide under.
pub(crate) fn secrets_rule() -> RuleId {
    RuleId::or_other("security.block_secrets")
}

/// The rules a withholding admission decision refused under, from its
/// closed reason codes (never the destination or the content).
pub(crate) fn admission_rules<'a>(reason_codes: impl IntoIterator<Item = &'a str>) -> Vec<RuleId> {
    let mut rules: Vec<RuleId> = Vec::new();
    for code in reason_codes {
        let rule = match code {
            "secret.blocked" => "security.block_secrets",
            "pii.blocked" => "security.pii",
            "classification.blocked" => "security.classification",
            "destination.constraint_satisfied" | "destination.remote_allowed_once" => continue,
            c if c.starts_with("destination.") => "gateway.destination",
            c if c.starts_with("egress.") => "gateway.egress",
            _ => continue,
        };
        let rule = RuleId::or_other(rule);
        if !rules.contains(&rule) {
            rules.push(rule);
        }
    }
    rules
}

/// K12: an admission decision that withheld content, by the rules it names.
pub(crate) fn admission_refused<'a>(reason_codes: impl IntoIterator<Item = &'a str>) {
    if !recording() {
        return;
    }
    for rule in admission_rules(reason_codes) {
        blocked(&rule);
    }
}

/// Whether a host family's tool calls are regulated: the native-tool deny
/// hook is enforcing (replace mode) for a host that supports it. Hybrid,
/// MCP-only and off leave native tools outside the gateway.
pub(crate) fn host_regulated(family: &str) -> bool {
    crate::core::host_coverage::tool_integration(family)
        == crate::core::host_coverage::ToolIntegration::DenyNativeTools
        && crate::hooks::replace_mode_chosen()
}

/// K13: one tool call of the host `client` (its MCP client id), counted by
/// host family when no enforcing rule regulates that host.
pub(crate) fn host_call(client: &str) {
    if !recording() {
        return;
    }
    let family = host_family(client);
    if !host_regulated(family) {
        record(Kind::UnregulatedHost, &[family]);
    }
}

/// The wire name of a downstream MCP server: its id when the workspace's
/// signed policy names it (`server` or `server::tool` in its allowed or
/// denied tools), else `other`. Never a hash of the id.
pub(crate) fn mcp_server_name(server: &str, policy_names: &[String]) -> UnregulatedName {
    let named = policy_names.iter().any(|entry| {
        entry == server
            || entry
                .strip_prefix(server)
                .is_some_and(|rest| rest.starts_with("::"))
    });
    let probe = Signal {
        ts: String::new(),
        kind: Kind::UnregulatedMcpServer,
        name: server.to_string(),
        project: None,
    };
    named
        .then(|| probe.unregulated().map(|(_, n)| n))
        .flatten()
        .unwrap_or_else(UnregulatedName::other)
}

/// Tool entries of the installed, verified Hive policy (default and every
/// vault), the only source that may name an MCP server on the wire.
fn signed_policy_tool_entries() -> Vec<String> {
    use crate::core::hive::effective_floor as floor;
    let Some(dir) = floor::floor_dir() else {
        return Vec::new();
    };
    let Ok((Some(text), required)) = floor::read(&dir) else {
        return Vec::new();
    };
    let Ok(Some(installed)) = floor::check(Some(&text), required.as_deref()) else {
        return Vec::new();
    };
    std::iter::once(&installed.policy.effective)
        .chain(installed.policy.vaults.iter().map(|v| &v.effective))
        .flat_map(|p| {
            p.context
                .allowed_tools
                .iter()
                .flatten()
                .chain(p.context.denied_tools.iter().flatten())
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect()
}

/// K13: one call the MCP gateway relays to `handle` (`server::tool`). The
/// engine enforces tool rules on its own tools only, so no rule regulates a
/// downstream server's tools: every relayed call is unregulated.
pub(crate) fn mcp_relay_call(handle: &str) {
    if !recording() {
        return;
    }
    let server = handle.split_once("::").map_or(handle, |(s, _)| s);
    let name = mcp_server_name(server, &signed_policy_tool_entries());
    record(Kind::UnregulatedMcpServer, &[name.as_str()]);
}

/// The rule the shell allowlist decides under.
pub(crate) fn shell_rule() -> RuleId {
    RuleId::or_other("gateway.security_violation")
}

/// Every record under `hive_data`.
pub(crate) fn signals(hive_data: &Path) -> Vec<(chrono::DateTime<chrono::Utc>, Signal)> {
    let Ok(text) = std::fs::read_to_string(dir_in(hive_data).join(SIGNALS_FILE)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<Signal>(line).ok())
        .filter_map(|signal| {
            let at = chrono::DateTime::parse_from_rfc3339(&signal.ts)
                .ok()?
                .with_timezone(&chrono::Utc);
            Some((at, signal))
        })
        .collect()
}

/// Drop records older than `before` (closed hours are built once).
pub(crate) fn prune(hive_data: &Path, before: chrono::DateTime<chrono::Utc>) {
    let path = dir_in(hive_data).join(SIGNALS_FILE);
    if !path.is_file() {
        return;
    }
    let kept: String = signals(hive_data)
        .into_iter()
        .filter(|(at, _)| *at >= before)
        .filter_map(|(_, signal)| serde_json::to_string(&signal).ok())
        .map(|line| line + "\n")
        .collect();
    if kept.is_empty() {
        let _ = std::fs::remove_file(path);
    } else {
        let _ = write_private(&dir_in(hive_data), SIGNALS_FILE, kept.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_closed_wire_names_or_other() {
        assert_eq!(read_mode_filter("lines:10-20").as_str(), "lines");
        assert_eq!(read_mode_filter("signatures").as_str(), "signatures");
        assert_eq!(read_mode_filter("/etc/passwd").as_str(), OTHER);
        assert_eq!(output_filter("ctx_shell").as_str(), "shell_output");
        assert_eq!(output_filter("ctx_search").as_str(), "search_output");
        assert_eq!(output_filter("ctx_tree").as_str(), OTHER);
        assert_eq!(program_name("/usr/bin/git").as_str(), "git");
        assert_eq!(program_name("terraform").as_str(), "terraform");
        for content in ["./deploy-acme.sh", "acme-internal-cli", "rm -rf /", "Git"] {
            assert_eq!(program_name(content).as_str(), OTHER, "{content}");
        }
        assert_eq!(shell_rule().as_str(), "gateway.security_violation");
        assert_eq!(path_jail_rule().as_str(), "gateway.path_jail");
        assert_eq!(secrets_rule().as_str(), "security.block_secrets");
    }

    #[test]
    fn admission_reasons_map_to_closed_rules() {
        let rules = |codes: &[&str]| -> Vec<String> {
            admission_rules(codes.iter().copied())
                .iter()
                .map(|r| r.as_str().to_string())
                .collect()
        };
        assert_eq!(rules(&["secret.blocked"]), ["security.block_secrets"]);
        assert_eq!(
            rules(&[
                "destination.remote_restricted",
                "destination.constraint_unsatisfied"
            ]),
            ["gateway.destination"]
        );
        assert_eq!(rules(&["egress.restricted_content"]), ["gateway.egress"]);
        assert!(rules(&["destination.constraint_satisfied", "secret.redacted"]).is_empty());
    }

    #[test]
    fn an_mcp_server_is_named_only_by_the_signed_policy() {
        let policy = vec!["github::create_issue".to_string(), "linear".to_string()];
        assert_eq!(mcp_server_name("github", &policy).as_str(), "github");
        assert_eq!(mcp_server_name("linear", &policy).as_str(), "linear");
        assert_eq!(mcp_server_name("acme-internal", &policy).as_str(), OTHER);
        assert_eq!(
            mcp_server_name("git", &policy).as_str(),
            OTHER,
            "prefix only"
        );
        assert_eq!(mcp_server_name("github", &[]).as_str(), OTHER);
    }

    #[test]
    fn host_families_without_a_deny_hook_are_unregulated() {
        assert!(!host_regulated(OTHER));
        assert!(!host_regulated("opencode") || crate::hooks::replace_mode_chosen());
    }

    #[test]
    fn a_record_is_only_what_its_kind_allows() {
        let signal = |kind, name: &str| Signal {
            ts: "2026-10-26T16:00:00Z".into(),
            kind,
            name: name.into(),
            project: None,
        };
        assert!(signal(Kind::Truncated, "map").filter().is_some());
        assert!(
            signal(Kind::Truncated, "gateway.path_jail")
                .filter()
                .is_none()
        );
        assert!(
            signal(Kind::WouldBlock, "gateway.path_jail")
                .rule()
                .is_some()
        );
        assert!(signal(Kind::WouldBlock, "map").rule().is_none());
        assert!(
            signal(Kind::UnregulatedProgram, "git")
                .unregulated()
                .is_some()
        );
        assert_eq!(
            signal(Kind::UnregulatedHost, "cursor").unregulated(),
            Some((
                UnregulatedKind::Host,
                UnregulatedName::new("cursor").unwrap()
            ))
        );
        assert!(
            signal(Kind::UnregulatedHost, "claude-code")
                .unregulated()
                .is_none()
        );
        assert!(
            signal(Kind::UnregulatedHost, "terraform")
                .unregulated()
                .is_none()
        );
        assert!(
            signal(Kind::UnregulatedMcpServer, "github")
                .unregulated()
                .is_some()
        );
        assert!(
            signal(Kind::UnregulatedMcpServer, "other")
                .unregulated()
                .is_some()
        );
        assert!(
            signal(Kind::UnregulatedMcpServer, &"a".repeat(32))
                .unregulated()
                .is_none()
        );
        assert!(signal(Kind::Blocked, "git").unregulated().is_none());
        // An MCP-server-shaped name is not a program.
        assert!(
            signal(Kind::UnregulatedProgram, "github")
                .unregulated()
                .is_none()
        );
        let json = serde_json::to_value(signal(Kind::Refetched, "full")).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 4, "{json}");
    }
}
