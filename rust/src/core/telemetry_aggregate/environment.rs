// SPDX-License-Identifier: Apache-2.0

//! Setup dimensions read from the build and the host at send time.
//!
//! Every function here reduces what it looks at to a closed enum before it
//! leaves this module: paths, environment values and dates never reach a batch.

use std::path::Path;

use super::super::telemetry_v2::{
    ActiveDays, ClientFamily, DistributionChannel, EmbeddingsState, InstallAge, IntegrationMode,
    RuntimeEnvironment, SetupProfileMetrics,
};

pub(super) fn distribution_channel() -> DistributionChannel {
    build_channel().unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .map_or(DistributionChannel::Unknown, |exe| channel_from_path(&exe))
    })
}

/// A packager may pin the channel at build time; nothing in the release
/// pipeline does today, so the executable's location decides.
fn build_channel() -> Option<DistributionChannel> {
    Some(match option_env!("LEAN_CTX_DISTRIBUTION_CHANNEL")? {
        "cargo" => DistributionChannel::Cargo,
        "homebrew" => DistributionChannel::Homebrew,
        "npm" => DistributionChannel::Npm,
        "docker" => DistributionChannel::Docker,
        "source" => DistributionChannel::Source,
        "aur" => DistributionChannel::Aur,
        "pypi" => DistributionChannel::Pypi,
        "binary" => DistributionChannel::Binary,
        _ => return None,
    })
}

/// Classifies where the running executable lives. Only the enum is sent.
pub(super) fn channel_from_path(exe: &Path) -> DistributionChannel {
    let path = exe
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    if path.contains("/node_modules/") {
        DistributionChannel::Npm
    } else if path.contains("/cellar/")
        || path.contains("/homebrew/")
        || path.contains("/linuxbrew/")
    {
        DistributionChannel::Homebrew
    } else if path.contains("/.cargo/bin/") {
        DistributionChannel::Cargo
    } else if path.contains("/site-packages/")
        || path.contains("/dist-packages/")
        || path.contains("/pipx/")
    {
        DistributionChannel::Pypi
    } else if path.contains("/target/debug/") || path.contains("/target/release/") {
        DistributionChannel::Source
    } else if path == "/usr/bin/lean-ctx" && Path::new("/var/lib/pacman").is_dir() {
        DistributionChannel::Aur
    } else if path.ends_with("/.local/bin/lean-ctx")
        || path.ends_with("/usr/local/bin/lean-ctx")
        || path.ends_with("/lean-ctx.exe")
    {
        DistributionChannel::Binary
    } else {
        DistributionChannel::Unknown
    }
}

pub(super) fn install_age(now: std::time::SystemTime) -> Option<InstallAge> {
    let created = crate::core::installation_id::identity_created_at()?;
    let age = now.duration_since(created).unwrap_or_default();
    Some(InstallAge::from_seconds(age.as_secs()))
}

/// Distinct UTC days with a successful send for this installation in the
/// trailing 30 days, counting `today` (the send in progress).
pub(super) fn active_days(installation_id: &str, today: &str) -> ActiveDays {
    let cutoff = chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d")
        .map(|day| {
            (day - chrono::Duration::days(29))
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_default();
    let records = crate::core::telemetry_ledger::read_all();
    let mut days: Vec<&str> = records
        .iter()
        .filter(|record| record.status == "success" && record.installation_id == installation_id)
        .filter_map(|record| record.timestamp.get(..10))
        .filter(|day| *day >= cutoff.as_str() && *day <= today)
        .collect();
    days.push(today);
    days.sort_unstable();
    days.dedup();
    ActiveDays::from_count(days.len())
}

pub(super) fn runtime_environment() -> RuntimeEnvironment {
    // `host_env`: a client that hides the job's environment from its MCP
    // servers must not turn a CI job or a container into a local machine.
    runtime_environment_from(
        |key| crate::core::host_env::var(key).filter(|value| !value.is_empty()),
        in_container(),
    )
}

/// Only documented, verifiable markers; anything else is a local machine.
pub(super) fn runtime_environment_from(
    lookup: impl Fn(&str) -> Option<String>,
    container: bool,
) -> RuntimeEnvironment {
    // Telemetry runs in CI only after an explicit LEAN_CTX_TELEMETRY_IN_CI opt-in.
    if crate::core::telemetry_consent::ci_detected(&lookup) {
        RuntimeEnvironment::Ci
    } else if lookup("CODESPACES").is_some() {
        RuntimeEnvironment::Codespaces
    } else if lookup("GITPOD_WORKSPACE_ID").is_some() {
        RuntimeEnvironment::Gitpod
    } else if lookup("REPL_ID").is_some() {
        RuntimeEnvironment::Replit
    } else if lookup("CLAUDE_CODE_REMOTE").is_some() {
        RuntimeEnvironment::CloudAgent
    } else if container {
        RuntimeEnvironment::Container
    } else {
        RuntimeEnvironment::Local
    }
}

fn in_container() -> bool {
    if Path::new("/.dockerenv").exists()
        || Path::new("/run/.containerenv").exists()
        || Path::new("/var/run/secrets/kubernetes.io").is_dir()
        || crate::core::host_env::var("KUBERNETES_SERVICE_HOST").is_some()
        || crate::core::host_env::var("container").is_some_and(|value| !value.is_empty())
    {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        // cgroup v1 names the runtime; under cgroup v2 a container sees only
        // `0::/`, so its overlay root filesystem is the remaining marker.
        std::fs::read_to_string("/proc/1/cgroup").is_ok_and(|cgroup| {
            ["docker", "kubepods", "containerd", "libpod"]
                .iter()
                .any(|marker| cgroup.contains(marker))
        }) || std::fs::read_to_string("/proc/self/mountinfo")
            .is_ok_and(|mountinfo| root_is_overlay(&mountinfo))
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Whether `/` is a container image's union filesystem, from
/// `/proc/self/mountinfo` (`id parent dev root mountpoint … - fstype …`).
/// The last mount on `/` is the one in effect.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) fn root_is_overlay(mountinfo: &str) -> bool {
    mountinfo
        .lines()
        .filter_map(|line| {
            let (mount, filesystem) = line.split_once(" - ")?;
            (mount.split_whitespace().nth(4)? == "/")
                .then(|| filesystem.split_whitespace().next())
                .flatten()
        })
        .next_back()
        .is_some_and(|fstype| matches!(fstype, "overlay" | "fuse-overlayfs" | "aufs"))
}

pub(super) fn setup_profile() -> SetupProfileMetrics {
    let integration_mode = match crate::core::config::Config::load().hook_mode_override() {
        None => IntegrationMode::Default,
        Some(crate::hooks::HookMode::Mcp) => IntegrationMode::Mcp,
        Some(crate::hooks::HookMode::Hybrid) => IntegrationMode::Hybrid,
        Some(crate::hooks::HookMode::Replace) => IntegrationMode::Replace,
    };
    SetupProfileMetrics {
        integration_mode,
        embeddings: embeddings_state(),
    }
}

#[cfg(feature = "embeddings")]
fn embeddings_state() -> EmbeddingsState {
    if crate::core::embeddings::EmbeddingEngine::is_available() {
        EmbeddingsState::Installed
    } else if crate::tools::ctx_knowledge::embeddings_auto_download_allowed() {
        EmbeddingsState::NotInstalled
    } else {
        EmbeddingsState::Disabled
    }
}

#[cfg(not(feature = "embeddings"))]
fn embeddings_state() -> EmbeddingsState {
    EmbeddingsState::Unsupported
}

/// Client that drives this installation: the MCP handshake of this process,
/// then the last handshake persisted by any process (the daemon and CLI never
/// see one themselves), then the host's environment variables. `None` means no
/// MCP client was seen at all (CLI and shell hooks only); `Other` is an MCP
/// client LeanCTX does not recognise.
pub(super) fn client_family() -> ClientFamily {
    const PERSISTED_MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;
    let handshake = Some(crate::core::client_capabilities::current().client_id)
        .filter(|id| id != "unknown")
        .or_else(|| {
            crate::core::client_capabilities::load_persisted(PERSISTED_MAX_AGE_SECS)
                .map(|caps| caps.client_id)
        });
    if let Some(family) = handshake.as_deref().and_then(ClientFamily::from_client_id) {
        return family;
    }
    if std::env::var_os("CLAUDECODE").is_some() {
        ClientFamily::Claude
    } else if std::env::var_os("CODEX_HOME").is_some() {
        ClientFamily::Codex
    } else if std::env::var_os("CURSOR_TRACE_ID").is_some() {
        ClientFamily::Cursor
    } else if std::env::var_os("GEMINI_CLI").is_some() {
        ClientFamily::Gemini
    } else if handshake.is_some()
        || crate::core::client_capabilities::handshake_seen(PERSISTED_MAX_AGE_SECS)
    {
        ClientFamily::Other
    } else {
        ClientFamily::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_follows_the_install_location() {
        let cases = [
            (
                "/Users/a/.npm-global/lib/node_modules/lean-ctx-bin/bin/lean-ctx",
                DistributionChannel::Npm,
            ),
            (
                "C:\\Users\\a\\AppData\\Roaming\\npm\\node_modules\\lean-ctx-bin\\bin\\lean-ctx.exe",
                DistributionChannel::Npm,
            ),
            ("/opt/homebrew/bin/lean-ctx", DistributionChannel::Homebrew),
            (
                "/usr/local/Cellar/lean-ctx/3.11.1/bin/lean-ctx",
                DistributionChannel::Homebrew,
            ),
            (
                "/home/linuxbrew/.linuxbrew/bin/lean-ctx",
                DistributionChannel::Homebrew,
            ),
            ("/home/a/.cargo/bin/lean-ctx", DistributionChannel::Cargo),
            (
                "/home/a/.venv/lib/python3.12/site-packages/leanctx_engine/bin/lean-ctx",
                DistributionChannel::Pypi,
            ),
            (
                "/home/a/src/lean-ctx/rust/target/release/lean-ctx",
                DistributionChannel::Source,
            ),
            ("/home/a/.local/bin/lean-ctx", DistributionChannel::Binary),
            ("/usr/local/bin/lean-ctx", DistributionChannel::Binary),
            (
                "C:\\Program Files\\LeanCTX\\lean-ctx.exe",
                DistributionChannel::Binary,
            ),
            ("/srv/tools/lean-ctx", DistributionChannel::Unknown),
        ];
        for (path, expected) in cases {
            assert_eq!(channel_from_path(Path::new(path)), expected, "{path}");
        }
    }

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn environment_uses_documented_markers_only() {
        assert_eq!(
            runtime_environment_from(env(&[]), false),
            RuntimeEnvironment::Local
        );
        assert_eq!(
            runtime_environment_from(env(&[]), true),
            RuntimeEnvironment::Container
        );
        assert_eq!(
            runtime_environment_from(env(&[("CODESPACES", "true")]), true),
            RuntimeEnvironment::Codespaces
        );
        assert_eq!(
            runtime_environment_from(env(&[("GITPOD_WORKSPACE_ID", "x")]), false),
            RuntimeEnvironment::Gitpod
        );
        assert_eq!(
            runtime_environment_from(env(&[("REPL_ID", "x")]), false),
            RuntimeEnvironment::Replit
        );
        assert_eq!(
            runtime_environment_from(env(&[("CLAUDE_CODE_REMOTE", "true")]), true),
            RuntimeEnvironment::CloudAgent
        );
        // Telemetry only runs in CI after an explicit opt-in; it is then labelled.
        assert_eq!(
            runtime_environment_from(
                env(&[("GITHUB_ACTIONS", "true"), ("CODESPACES", "true")]),
                true
            ),
            RuntimeEnvironment::Ci
        );
        assert_eq!(
            runtime_environment_from(env(&[("CI", "false")]), false),
            RuntimeEnvironment::Local
        );
    }

    #[test]
    fn an_overlay_root_marks_a_cgroup_v2_container() {
        let container = "\
1270 1101 0:310 / / rw,relatime master:487 - overlay overlay rw,lowerdir=/var/lib/x
1271 1270 0:313 / /proc rw,nosuid - proc proc rw
1272 1270 0:314 / /dev rw,nosuid - tmpfs tmpfs rw";
        let host = "\
22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
24 22 0:5 / /proc rw - proc proc rw
90 22 0:44 / /var/lib/docker/overlay2/abc/merged rw - overlay overlay rw";
        // A later mount on `/` replaces the earlier one.
        let remounted = "\
1 0 0:1 / / rw - overlay overlay rw
2 1 259:2 / / rw - btrfs /dev/sda2 rw";
        assert!(root_is_overlay(container));
        assert!(!root_is_overlay(host), "an overlay below / is not the root");
        assert!(!root_is_overlay(remounted));
        assert!(!root_is_overlay(""));
    }

    #[test]
    #[serial_test::serial]
    fn active_days_count_distinct_successful_days_for_this_installation() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let record = |timestamp: &str, installation: &str, status: &str, hash: char| {
            crate::core::telemetry_ledger::HeartbeatRecord {
                timestamp: timestamp.into(),
                installation_id: installation.into(),
                version: "3.11.1".into(),
                os: String::new(),
                arch: String::new(),
                schema_version: 2,
                event_names: vec![],
                payload_hash: hash.to_string().repeat(64),
                endpoint: String::new(),
                status: status.into(),
            }
        };
        for entry in [
            record("2026-10-01T08:00:00Z", "me", "success", 'a'),
            record("2026-10-01T17:00:00Z", "me", "success", 'b'),
            record("2026-10-04T09:00:00Z", "me", "success", 'c'),
            record("2026-10-05T09:00:00Z", "me", "failed", 'd'),
            record("2026-10-06T09:00:00Z", "someone-else", "success", 'e'),
            record("2026-09-01T09:00:00Z", "me", "success", 'f'),
        ] {
            crate::core::telemetry_ledger::append(&entry).unwrap();
        }
        // Oct 1, Oct 4 and today; failed, foreign and out-of-window days do not count.
        assert_eq!(active_days("me", "2026-10-08"), ActiveDays::D2To3);
        assert_eq!(active_days("nobody", "2026-10-08"), ActiveDays::D1);
    }
}
