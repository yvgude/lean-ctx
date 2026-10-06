// SPDX-License-Identifier: Apache-2.0

//! Disclosure, consent and environment rules for default-on telemetry.
//!
//! Telemetry is on by default, so every place that can tell a person about it
//! (setup, `telemetry on|status`, the one-time notice) prints the same list of
//! what is sent. The list mirrors the v2 event set in `telemetry_v2`; extend it
//! together with any new event.

use std::io::IsTerminal;

/// What a batch can contain, in plain words.
pub const DISCLOSURE: &[&str] = &[
    "random installation ID (not derived from your machine or account)",
    "LeanCTX version, OS, CPU architecture, install channel (cargo/npm/homebrew/…)",
    "AI client family (Claude, Cursor, Codex, …) and setup profile / integrations",
    "daily counts per built-in tool: calls, failures, latency buckets",
    "session counts and uptime, error categories, version upgrades",
    "aggregate autopilot, sync and plan events (counts only)",
];

/// What a batch never contains.
pub const NEVER_SENT: &str =
    "No prompts, code, file names, paths, commands, secrets or IP-derived data.";

/// Bump when [`DISCLOSURE`] gains a category, so existing installations see
/// the notice again.
const NOTICE_VERSION: u32 = 1;

/// Environment variables that mark a CI or build job. Each job usually starts
/// from a fresh home and would report as a brand-new installation, so CI never
/// collects or sends telemetry.
const CI_MARKERS: &[&str] = &[
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
];

/// Pure CI detection over an environment lookup. `CI` counts unless it is
/// explicitly false; the vendor markers count whenever they are non-empty.
pub fn ci_detected(lookup: impl Fn(&str) -> Option<String>) -> bool {
    let set = |key: &str| lookup(key).is_some_and(|value| !value.trim().is_empty());
    let ci_flag = lookup("CI").is_some_and(|value| {
        let value = value.trim().to_ascii_lowercase();
        !value.is_empty() && value != "false" && value != "0"
    });
    ci_flag || CI_MARKERS.iter().any(|key| set(key))
}

/// Whether this process runs inside a CI job. `LEAN_CTX_TELEMETRY_IN_CI=1`
/// opts a machine that sets a CI marker for other reasons back in.
pub fn running_in_ci() -> bool {
    // Unit tests run inside CI themselves; detection is covered by `ci_detected`.
    if cfg!(test) {
        return false;
    }
    let opted_in = std::env::var("LEAN_CTX_TELEMETRY_IN_CI").is_ok_and(|value| value.trim() == "1");
    !opted_in && ci_detected(|key| std::env::var(key).ok())
}

/// Lines describing what is collected, for setup and `telemetry on`.
pub fn disclosure_lines() -> Vec<String> {
    let mut lines: Vec<String> = DISCLOSURE.iter().map(|item| format!("• {item}")).collect();
    lines.push(NEVER_SENT.to_string());
    lines
}

/// Persist an explicit choice. Both keys are written in one atomic update, so
/// a declined prompt can never be read back as default-on.
pub fn persist_choice(enabled: bool) -> Result<(), String> {
    crate::core::config::setter::set_many_by_key(&consent_updates(enabled))
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Config writes for an explicit choice. Every key must exist in the schema.
pub fn consent_updates(enabled: bool) -> [(&'static str, &'static str); 2] {
    let (value, preference) = if enabled {
        ("true", "explicitly_enabled")
    } else {
        ("false", "explicitly_disabled")
    };
    [
        ("telemetry.enabled", value),
        ("telemetry.preference", preference),
    ]
}

fn notice_path() -> Result<std::path::PathBuf, String> {
    crate::core::paths::state_dir().map(|dir| dir.join("telemetry_notice_version"))
}

fn notice_seen() -> bool {
    notice_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| text.trim().parse::<u32>().ok())
        .is_some_and(|seen| seen >= NOTICE_VERSION)
}

/// Record that the current disclosure was shown (setup, `telemetry on|off`).
pub fn mark_notice_seen() {
    let Ok(path) = notice_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, format!("{NOTICE_VERSION}\n"));
}

fn notice_text() -> String {
    let mut text =
        String::from("\x1b[1mlean-ctx sends anonymous usage telemetry (on by default).\x1b[0m\n");
    for line in disclosure_lines() {
        text.push_str("  ");
        text.push_str(&line);
        text.push('\n');
    }
    text.push_str(
        "  \x1b[2mSee the exact payload: lean-ctx telemetry show · Turn off: lean-ctx telemetry off · Details: https://leanctx.com/privacy\x1b[0m\n",
    );
    text
}

/// Show the disclosure once per installation (and again when it grows) on the
/// first interactive command. Never prints for MCP, hooks or piped use, never
/// when telemetry is already off or blocked, and never touches stdout.
pub fn maybe_show_notice() {
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return;
    }
    if notice_seen() || !telemetry_would_send() {
        return;
    }
    eprint!("{}", notice_text());
    eprintln!();
    mark_notice_seen();
}

fn telemetry_would_send() -> bool {
    let Ok(config) = crate::core::config::Config::try_load_global() else {
        return false;
    };
    config.telemetry.send_eligible(
        std::env::var("DO_NOT_TRACK").ok().as_deref(),
        std::env::var("LEAN_CTX_TELEMETRY").ok().as_deref(),
    ) && !running_in_ci()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + 'static {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn plain_workstation_is_not_ci() {
        assert!(!ci_detected(env(&[])));
        assert!(!ci_detected(env(&[("CI", "false")])));
        assert!(!ci_detected(env(&[("CI", "0")])));
        assert!(!ci_detected(env(&[("CI", "")])));
        assert!(!ci_detected(env(&[("GITHUB_ACTIONS", " ")])));
    }

    #[test]
    fn common_ci_markers_are_detected() {
        assert!(ci_detected(env(&[("CI", "true")])));
        assert!(ci_detected(env(&[("CI", "1")])));
        for marker in CI_MARKERS {
            assert!(ci_detected(env(&[(marker, "x")])), "{marker}");
        }
    }

    #[test]
    fn consent_updates_only_write_schema_keys() {
        let schema = crate::core::config::schema::ConfigSchema::generate();
        for enabled in [true, false] {
            for (key, _) in consent_updates(enabled) {
                assert!(schema.lookup(key).is_some(), "unknown config key {key}");
            }
        }
    }

    #[test]
    fn disclosure_names_every_sent_category_and_the_exclusions() {
        let text = notice_text();
        for item in DISCLOSURE {
            assert!(text.contains(item));
        }
        assert!(text.contains(NEVER_SENT));
        assert!(text.contains("lean-ctx telemetry off"));
        assert!(text.contains("lean-ctx telemetry show"));
    }
}
