// SPDX-License-Identifier: Apache-2.0

//! Environment markers as the host that started this process sees them.
//!
//! Some AI clients start MCP servers with an allowlisted environment: Codex
//! passes only `HOME`, `PATH`, `USER`, `LANG` and a few more. A server started
//! that way cannot see `CI`, `DO_NOT_TRACK` or `LEAN_CTX_TELEMETRY`, so a CI
//! job reported as a local machine and an opt-out set in the shell did not
//! reach the server. On Linux the nearest ancestor that sets one of the
//! [`HOST_KEYS`] supplies it when this process has none. Only those keys are
//! kept; the rest of an ancestor's environment is never stored.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Keys looked up in ancestors: consent, CI and runtime-environment markers.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const HOST_KEYS: &[&str] = &[
    "DO_NOT_TRACK",
    "LEAN_CTX_TELEMETRY",
    "LEAN_CTX_TELEMETRY_IN_CI",
    "CI",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "TRAVIS",
    "JENKINS_URL",
    "TF_BUILD",
    "TEAMCITY_VERSION",
    "BITBUCKET_BUILD_NUMBER",
    "CODEBUILD_BUILD_ID",
    "DRONE",
    "APPVEYOR",
    "SEMAPHORE",
    "HEROKU_TEST_RUN_ID",
    "CONTINUOUS_INTEGRATION",
    "CODESPACES",
    "GITPOD_WORKSPACE_ID",
    "REPL_ID",
    "CLAUDE_CODE_REMOTE",
    "KUBERNETES_SERVICE_HOST",
    "container",
];

/// How many ancestors are read: client, its shell, the job runner, its init.
#[cfg(target_os = "linux")]
const MAX_ANCESTORS: usize = 4;

/// `key` from this process, else from the nearest ancestor for [`HOST_KEYS`].
/// A key this process sets, even to an empty value, is never overridden.
#[must_use]
pub(crate) fn var(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(value) => Some(value),
        Err(_) => ancestors().get(key).cloned(),
    }
}

fn ancestors() -> &'static HashMap<&'static str, String> {
    static ANCESTORS: OnceLock<HashMap<&'static str, String>> = OnceLock::new();
    ANCESTORS.get_or_init(read_ancestors)
}

/// Unit tests read their own environment only, so the developer's or the CI
/// runner's shell never changes a result.
#[cfg(any(test, not(target_os = "linux")))]
fn read_ancestors() -> HashMap<&'static str, String> {
    HashMap::new()
}

#[cfg(all(target_os = "linux", not(test)))]
fn read_ancestors() -> HashMap<&'static str, String> {
    let mut found = HashMap::new();
    let mut pid = std::os::unix::process::parent_id();
    for _ in 0..MAX_ANCESTORS {
        if pid <= 1 {
            break;
        }
        if let Ok(environ) = std::fs::read(format!("/proc/{pid}/environ")) {
            collect_host_keys(&environ, &mut found);
        }
        match std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .as_deref()
            .and_then(parent_pid)
        {
            Some(parent) => pid = parent,
            None => break,
        }
    }
    found
}

/// Adds the [`HOST_KEYS`] from a NUL-separated `environ` that `found` does not
/// hold yet, so the nearest ancestor wins.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn collect_host_keys(environ: &[u8], found: &mut HashMap<&'static str, String>) {
    for entry in environ.split(|byte| *byte == 0) {
        let Some(separator) = entry.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let (name, value) = (&entry[..separator], &entry[separator + 1..]);
        if let Some(key) = HOST_KEYS.iter().find(|key| key.as_bytes() == name) {
            found
                .entry(key)
                .or_insert_with(|| String::from_utf8_lossy(value).into_owned());
        }
    }
}

/// The parent PID from `/proc/<pid>/stat`. The command name may contain spaces
/// and parentheses, so the fields are read after its closing parenthesis.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parent_pid(stat: &str) -> Option<u32> {
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_host_keys_are_kept_and_the_nearest_ancestor_wins() {
        let mut found = HashMap::new();
        collect_host_keys(
            b"GITHUB_TOKEN=secret\0CI=true\0DO_NOT_TRACK=1\0container=podman\0NOEQUALS\0",
            &mut found,
        );
        collect_host_keys(b"CI=false\0GITLAB_CI=true\0", &mut found);
        assert_eq!(found.get("CI").map(String::as_str), Some("true"));
        assert_eq!(found.get("DO_NOT_TRACK").map(String::as_str), Some("1"));
        assert_eq!(found.get("container").map(String::as_str), Some("podman"));
        assert_eq!(found.get("GITLAB_CI").map(String::as_str), Some("true"));
        assert_eq!(found.len(), 4, "{found:?}");
    }

    #[test]
    fn parent_pid_survives_spaces_and_parentheses_in_the_command_name() {
        assert_eq!(parent_pid("4242 (codex) S 17 4242 4242 0"), Some(17));
        assert_eq!(parent_pid("4242 (my (odd) cmd) R 99 1 1"), Some(99));
        assert_eq!(parent_pid("garbage"), None);
    }

    #[test]
    fn every_ci_marker_is_a_host_key() {
        for marker in crate::core::telemetry_consent::CI_MARKERS {
            assert!(HOST_KEYS.contains(marker), "{marker}");
        }
    }
}
