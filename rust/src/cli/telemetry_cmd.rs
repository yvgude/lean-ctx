//! CLI commands for the anonymous telemetry heartbeat.
//!
//! `lean-ctx telemetry [status|on|off|reset-id|show]`

use crate::core::config;
use crate::core::installation_id;

pub(super) fn cmd_telemetry(args: &[String]) {
    let sub = args.first().map(String::as_str).unwrap_or("status");

    match sub {
        "status" => show_status(),
        "on" | "enable" => set_enabled(true),
        "off" | "disable" => set_enabled(false),
        "reset-id" => reset_id(),
        "show" | "pending" => show_payload(),
        "history" | "log" => show_history(),
        "purge-local" => purge_local(),
        "delete-remote" => delete_remote(),
        "--help" | "-h" => print_help(),
        other => {
            eprintln!("telemetry: unknown subcommand '{other}'");
            print_help();
            std::process::exit(1);
        }
    }
}

/// Why the effective send path is inactive despite the persisted preference.
///
/// The verdict itself always comes from [`config::TelemetryConfig::send_eligible`];
/// this only explains a `false` from that same authority, reusing its own public
/// predicates instead of re-interpreting the opt-out rules a second time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendBlocker {
    /// Persisted opt-out: `telemetry off`, or a legacy `enabled = false`.
    Preference,
    /// `DO_NOT_TRACK=1`, or `LEAN_CTX_TELEMETRY=off|false|0|no`.
    Environment,
    /// A CI marker (`CI`, `GITHUB_ACTIONS`, …): CI jobs never collect or send.
    Ci,
    /// The authority refuses for a reason this display does not model yet.
    Policy,
}

impl SendBlocker {
    fn describe(self) -> &'static str {
        match self {
            Self::Preference => "off by your saved preference",
            Self::Environment => "blocked by the environment (DO_NOT_TRACK / LEAN_CTX_TELEMETRY)",
            Self::Ci => "not sent from CI (set LEAN_CTX_TELEMETRY_IN_CI=1 if this is no CI job)",
            Self::Policy => "blocked by telemetry policy",
        }
    }
}

/// Classifies a non-eligible state. `None` means the authority itself says the
/// installation is send-eligible, so the order below never decides eligibility —
/// it only picks the reason to show, most-persistent cause first.
fn send_blocker(
    telemetry: &config::TelemetryConfig,
    do_not_track: Option<&str>,
    env_override: Option<&str>,
) -> Option<SendBlocker> {
    if telemetry.send_eligible(do_not_track, env_override) {
        return None;
    }
    if telemetry.explicitly_disabled() {
        Some(SendBlocker::Preference)
    } else if config::TelemetryConfig::environment_disables(do_not_track, env_override) {
        Some(SendBlocker::Environment)
    } else {
        Some(SendBlocker::Policy)
    }
}

fn show_status() {
    // Global-only and fail-closed, exactly like every path that actually
    // sends: an unreadable config never counts as default-on.
    let cfg = match config::Config::try_load_global() {
        Ok(cfg) => cfg,
        Err(error) => {
            println!("  Sending:    \x1b[2minactive — config unreadable ({error})\x1b[0m");
            return;
        }
    };
    let enabled = !cfg.telemetry.explicitly_disabled();
    let blocker = send_blocker(
        &cfg.telemetry,
        std::env::var("DO_NOT_TRACK").ok().as_deref(),
        std::env::var("LEAN_CTX_TELEMETRY").ok().as_deref(),
    )
    .or_else(|| crate::core::telemetry_consent::running_in_ci().then_some(SendBlocker::Ci));
    // The aggregate records every acknowledged send; the config field only
    // tracks the daily background pass and lags intraday sends.
    let last = crate::core::telemetry_aggregate::last_sent_bucket()
        .or_else(|| cfg.telemetry.last_heartbeat.clone());
    let last = last.as_deref().unwrap_or("never");

    println!(
        "  Preference: {}",
        if enabled {
            "\x1b[32menabled\x1b[0m"
        } else {
            "\x1b[2mdisabled\x1b[0m"
        }
    );
    match blocker {
        None => println!("  Sending:    \x1b[32mactive\x1b[0m"),
        Some(reason) => println!(
            "  Sending:    \x1b[2minactive — {}\x1b[0m",
            reason.describe()
        ),
    }

    if let Ok(id) = installation_id::get_or_create() {
        println!("  Install ID: {}", installation_id::masked(&id));
    }
    println!("  Last sent:  {last}");
    println!();

    if enabled {
        println!("  \x1b[2mDisable: lean-ctx telemetry off\x1b[0m");
    } else {
        println!("  \x1b[2mEnable:  lean-ctx telemetry on\x1b[0m");
    }
    println!("  \x1b[2mInspect: lean-ctx telemetry show\x1b[0m");
}

/// `telemetry on|off` writes both consent keys atomically through the shared
/// consent rules. Legacy `cloud.contribute_enabled` is migrated by the loader.
fn set_enabled(enabled: bool) {
    match crate::core::telemetry_consent::persist_choice(enabled) {
        Ok(()) => {
            crate::core::telemetry_consent::mark_notice_seen();
            if enabled {
                println!("Telemetry enabled — thank you for helping improve lean-ctx!");
                println!("Sent as cumulative daily totals, several times a day:");
                for line in crate::core::telemetry_consent::disclosure_lines() {
                    println!("  {line}");
                }
                println!("\x1b[2mDisable anytime: lean-ctx telemetry off\x1b[0m");
            } else {
                println!("Telemetry disabled. No data will be sent.");
                println!("\x1b[2mRe-enable: lean-ctx telemetry on\x1b[0m");
            }
        }
        Err(e) => {
            eprintln!("Failed to update config: {e}");
            std::process::exit(1);
        }
    }
}

fn reset_id() {
    let (current_id, deletion_token) = match installation_id::get_or_create_identity() {
        Ok(identity) => identity,
        Err(error) => {
            eprintln!("Failed to read telemetry identity: {error}");
            std::process::exit(1);
        }
    };
    match crate::cloud_client::delete_remote_telemetry(&current_id, &deletion_token) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!(
                "Installation ID was not reset because its remote telemetry could not be deleted."
            );
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("Installation ID was not reset: {error}");
            std::process::exit(1);
        }
    }
    match crate::core::telemetry_aggregate::rotate_identity_state_then(installation_id::reset) {
        Ok(new_id) => {
            println!(
                "Installation ID regenerated: {}",
                installation_id::masked(&new_id)
            );
            println!("\x1b[2mThe old ID is gone — the server cannot correlate old and new.\x1b[0m");
        }
        Err(e) => {
            eprintln!("Failed to reset installation ID: {e}");
            std::process::exit(1);
        }
    }
}

fn show_payload() {
    let Ok(cfg) = config::Config::try_load_global() else {
        println!("No telemetry payload is eligible: the config is unreadable.");
        return;
    };
    let do_not_track = std::env::var("DO_NOT_TRACK").ok();
    let telemetry_override = std::env::var("LEAN_CTX_TELEMETRY").ok();
    if !cfg
        .telemetry
        .send_eligible(do_not_track.as_deref(), telemetry_override.as_deref())
    {
        println!("No telemetry payload is currently eligible for sending.");
        return;
    }
    if crate::core::telemetry_consent::running_in_ci() {
        println!("No telemetry payload is sent from CI.");
        return;
    }
    let payload = match crate::core::telemetry_aggregate::pending_daily_batch() {
        Ok(payload) => payload,
        Err(error) => {
            eprintln!("Unable to build telemetry payload: {error}");
            return;
        }
    };

    println!("This is the exact JSON that would be sent to api.leanctx.com:");
    println!();
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).unwrap_or_default()
    );
    println!();
    println!(
        "\x1b[2mEndpoint: POST {}/api/telemetry/v2/batch\x1b[0m",
        api_url()
    );
    println!(
        "\x1b[2mFrequency: cumulative daily totals, up to {} times per UTC day\x1b[0m",
        crate::core::telemetry_aggregate::DAILY_SEND_CAP
    );
    println!("\x1b[2mAuthentication: none\x1b[0m");
}

fn api_url() -> String {
    std::env::var("LEAN_CTX_API_URL").unwrap_or_else(|_| "https://api.leanctx.com".to_string())
}

fn show_history() {
    let records = crate::core::telemetry_ledger::read_all();
    if records.is_empty() {
        println!("No heartbeats sent yet.");
        println!("\x1b[2mEnable with: lean-ctx telemetry on\x1b[0m");
        return;
    }
    let header = format!(
        "  \x1b[1m{:<28} {:<12} {:<10} {}\x1b[0m",
        "Timestamp", "Version", "OS", "Arch"
    );
    println!("{header}");
    println!("  {}", "\u{2500}".repeat(65));
    for record in records.iter().rev().take(50) {
        println!(
            "  {:<28} {:<12} {:<10} {}",
            record.timestamp, record.version, record.os, record.arch,
        );
        if record.schema_version > 0 {
            println!(
                "    schema={} status={} events={} hash={} endpoint={}",
                record.schema_version,
                record.status,
                record.event_names.join(","),
                record.payload_hash,
                record.endpoint
            );
        }
    }
    println!();
    println!(
        "  \x1b[2m{} total heartbeats recorded\x1b[0m",
        records.len()
    );
}

fn purge_local() {
    match purge_local_history() {
        Ok(()) => println!("Local telemetry history purged."),
        Err(error) => {
            eprintln!("Failed to purge local telemetry history: {error}");
            std::process::exit(1);
        }
    }
}

fn purge_local_history() -> Result<(), String> {
    crate::core::telemetry_aggregate::purge_local_state_then(
        crate::core::telemetry_ledger::purge_local,
    )
}

fn delete_remote() {
    let (installation_id, deletion_token) = match installation_id::get_or_create_identity() {
        Ok(identity) => identity,
        Err(error) => {
            eprintln!("Failed to read telemetry identity: {error}");
            std::process::exit(1);
        }
    };
    match crate::cloud_client::delete_remote_telemetry(&installation_id, &deletion_token) {
        Ok(true) => {
            match crate::core::telemetry_aggregate::rotate_identity_state_then(
                installation_id::reset,
            ) {
                Ok(_) => {
                    println!("Remote telemetry was deleted and the local identity was rotated.");
                }
                Err(error) => {
                    eprintln!(
                        "Remote telemetry deleted, but local identity rotation failed: {error}"
                    );
                    std::process::exit(1);
                }
            }
        }
        Ok(false) => {
            eprintln!(
                "Remote telemetry was not deleted; send one current v2 batch first to register the deletion credential."
            );
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("Failed to delete remote telemetry: {error}");
            std::process::exit(1);
        }
    }
}

fn print_help() {
    println!("Usage: lean-ctx telemetry [subcommand]");
    println!();
    println!("Manage privacy-safe telemetry (default-on, fully disableable, no PII).");
    println!();
    println!("Subcommands:");
    println!("  status     Show current telemetry status (default)");
    println!("  on         Enable anonymous product telemetry");
    println!("  off        Disable anonymous product telemetry");
    println!("  show       Display the exact payload that would be sent");
    println!("  pending    Display the exact typed batch currently eligible for sending");
    println!("  reset-id   Regenerate the anonymous installation ID");
    println!("  history    Show log of all sent batches");
    println!("  purge-local Delete the local telemetry history");
    println!("  delete-remote Delete server-side telemetry for this installation");
    println!();
    println!("Sends cumulative daily totals: version, OS/arch, a random install UUID,");
    println!("client family, setup profile and per-tool call counts.");
    println!("No code, filenames, prompts, commands or personal data — ever.");
    println!("Opt out: lean-ctx telemetry off, DO_NOT_TRACK=1 or LEAN_CTX_TELEMETRY=off.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purge_keeps_history_during_send_then_removes_all_local_telemetry() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::core::telemetry_aggregate::record_current_version().unwrap();
        let lease = crate::core::telemetry_aggregate::begin_daily_send().unwrap();
        let pending = lease.batch().clone();
        let state_dir = crate::core::paths::state_dir().unwrap();
        let aggregate = state_dir.join("telemetry_v2_aggregate.json");
        let one_shots = state_dir.join("telemetry_v2_one_shots.json");
        assert!(aggregate.exists());
        assert!(one_shots.exists());
        let ledger = state_dir.join("telemetry_heartbeats.jsonl");
        std::fs::write(&ledger, b"existing history\n").unwrap();
        assert!(purge_local_history().unwrap_err().contains("timed out"));
        assert_eq!(std::fs::read(&ledger).unwrap(), b"existing history\n");
        drop(lease);
        assert_eq!(
            crate::core::telemetry_aggregate::preview_daily_batch().unwrap(),
            pending
        );
        purge_local_history().unwrap();
        assert!(!ledger.exists());
        assert!(!aggregate.exists());
        assert!(!one_shots.exists());
    }

    /// Eligible state, and each reason the status line must be able to name.
    #[test]
    fn status_names_every_reason_sending_is_inactive() {
        // Default-on sends right away; the disclosure is a notice, not a gate.
        let cfg = config::TelemetryConfig::default();
        assert_eq!(send_blocker(&cfg, None, None), None);

        assert_eq!(
            send_blocker(&cfg, Some("1"), None),
            Some(SendBlocker::Environment)
        );
        for value in ["off", "false", "0", "no", " OFF "] {
            assert_eq!(
                send_blocker(&cfg, None, Some(value)),
                Some(SendBlocker::Environment),
                "LEAN_CTX_TELEMETRY={value} must block sending"
            );
        }
        // Only the exact opt-out spellings block; nothing else is invented here.
        assert_eq!(send_blocker(&cfg, Some("0"), None), None);
        assert_eq!(send_blocker(&cfg, None, Some("on")), None);

        let disabled = config::TelemetryConfig {
            preference: config::TelemetryPreference::ExplicitlyDisabled,
            ..cfg.clone()
        };
        assert_eq!(
            send_blocker(&disabled, None, None),
            Some(SendBlocker::Preference)
        );
        // A persisted opt-out is reported as such even when the environment
        // would independently block the send.
        assert_eq!(
            send_blocker(&disabled, Some("1"), Some("off")),
            Some(SendBlocker::Preference)
        );

        let legacy = config::TelemetryConfig {
            enabled: false,
            ..cfg.clone()
        };
        assert_eq!(
            send_blocker(&legacy, None, None),
            Some(SendBlocker::Preference)
        );
    }

    /// The display must never disagree with the eligibility authority: across
    /// every reachable combination, "no blocker" is exactly `send_eligible`.
    #[test]
    fn status_never_contradicts_the_eligibility_authority() {
        let preferences = [
            config::TelemetryPreference::DefaultOn,
            config::TelemetryPreference::ExplicitlyEnabled,
            config::TelemetryPreference::ExplicitlyDisabled,
        ];
        let environments = [None, Some("0"), Some("1"), Some("off"), Some("no")];
        for enabled in [true, false] {
            for preference in preferences {
                for do_not_track in environments {
                    for env_override in environments {
                        let cfg = config::TelemetryConfig {
                            enabled,
                            preference,
                            last_heartbeat: None,
                        };
                        assert_eq!(
                            send_blocker(&cfg, do_not_track, env_override).is_none(),
                            cfg.send_eligible(do_not_track, env_override),
                            "disagreement for enabled={enabled} preference={preference:?} \
                             DO_NOT_TRACK={do_not_track:?} LEAN_CTX_TELEMETRY={env_override:?}"
                        );
                    }
                }
            }
        }
    }

    /// Reporting status is read-only: it never rewrites the persisted choice.
    #[test]
    fn status_leaves_the_persisted_choice_untouched() {
        let cfg = config::TelemetryConfig {
            enabled: true,
            preference: config::TelemetryPreference::ExplicitlyEnabled,
            last_heartbeat: Some("2026-09-20".to_string()),
        };
        let _ = send_blocker(&cfg, Some("1"), Some("off"));
        assert!(cfg.enabled);
        assert_eq!(
            cfg.preference,
            config::TelemetryPreference::ExplicitlyEnabled
        );
        assert_eq!(cfg.last_heartbeat.as_deref(), Some("2026-09-20"));
    }
}
