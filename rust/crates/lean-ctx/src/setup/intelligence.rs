// SPDX-License-Identifier: Apache-2.0
//! Setup presentation only; package admission and activation have one core authority.

use std::io::Write;

use lean_ctx::core::{
    intelligence_runtime::{COMPANIONS_LATER, DISCLOSURE, SetupRuntime, inspect_setup},
    setup_report::{SetupItem, SetupStepReport},
};

const FALLBACK: &str =
    "Continuing with the public reference runtime; the Intelligence Runtime is unavailable.";

/// What the setup report says about the runtime (owner decision
/// 23.11.2026: installed without a question, with or without a terminal;
/// consent comes from the terms accepted at sign-up).
const NOTE: &str = "Setup installs and enables the runtime from the signed LeanCTX channel, with or without a terminal, unless it was switched off; where no package is available it continues with the public reference runtime, and a failure never stops setup. Switch it off with: lean-ctx setup runtime deactivate.";

/// Said before the download of the runtime itself (owner decision
/// 23.11.2026: setup installs only the runtime and stays fast; the large
/// companion packages load later in the background).
const DOWNLOADING: &str = "Downloading the runtime from the signed LeanCTX channel; its model set loads afterwards in the background.";

/// The setup step for the runtime. `install`: a full setup run (not an
/// update refresh) installs it as well, saying what it does on stderr
/// (stdout may carry `--json`); a failure is a warning, never an error.
pub(super) fn report(install: bool) -> SetupStepReport {
    report_with(install, &mut std::io::stderr(), || {
        inspect_setup().map_err(Into::into)
    })
}

fn report_with(
    install: bool,
    output: &mut impl Write,
    inspect: impl Fn() -> anyhow::Result<SetupRuntime>,
) -> SetupStepReport {
    let mut warnings = Vec::new();
    if install && let Err(error) = install_with(output, inspect()) {
        warnings.push(not_installed_line(&error));
    }
    let status = inspect().map_or("unavailable", |candidate| candidate.status());
    if status == "unavailable" && warnings.is_empty() {
        warnings.push(FALLBACK.into());
    }
    SetupStepReport {
        name: "intelligence_runtime".into(),
        ok: true,
        items: vec![SetupItem {
            name: "Intelligence Runtime".into(),
            status: status.into(),
            path: None,
            note: Some(format!("{DISCLOSURE} {NOTE}")),
        }],
        warnings,
        errors: Vec::new(),
    }
}

/// Why the runtime was not installed, as one plain line; setup goes on
/// without it. A platform the channel has no package for says only that.
pub(crate) fn not_installed_line(error: &anyhow::Error) -> String {
    use lean_ctx::core::intelligence_runtime::InstallError;
    match error.downcast_ref::<InstallError>() {
        Some(InstallError::PlatformUnavailable) => {
            format!("{}.", InstallError::PlatformUnavailable)
        }
        _ => format!("Intelligence Runtime not installed ({error}). {FALLBACK}"),
    }
}

/// Run just the runtime step (installed without a question, with or
/// without a terminal), without changing editor/shell/daemon setup.
pub(crate) fn configure_runtime() -> bool {
    let result = install_with(
        &mut std::io::stdout().lock(),
        inspect_setup().map_err(Into::into),
    );
    if let Err(error) = &result {
        lean_ctx::terminal_ui::print_status_warn(&not_installed_line(error));
    }
    result.is_ok()
}

/// What setup does with the runtime it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    /// Enabled and verified: nothing to do.
    KeepEnabled,
    /// Switched off with `setup runtime deactivate`: it stays off.
    KeepDeactivated,
    /// Installed and verified, not enabled yet: enable it.
    Enable,
    /// Not installed, the signed channel is configured: install it.
    Install,
    /// No package and no channel: the reference runtime.
    Fallback,
}

/// What setup found: `[enabled, deactivated, available, channel]`.
type Found = [bool; 4];

fn plan(candidate: &SetupRuntime) -> Plan {
    plan_for([
        candidate.enabled(),
        candidate.deactivated(),
        candidate.available(),
        candidate.channel_available(),
    ])
}

fn plan_for([enabled, deactivated, available, channel]: Found) -> Plan {
    if enabled {
        Plan::KeepEnabled
    } else if deactivated {
        Plan::KeepDeactivated
    } else if available {
        Plan::Enable
    } else if channel {
        Plan::Install
    } else {
        Plan::Fallback
    }
}

/// Install (no question, no terminal needed) the runtime `inspected`
/// describes, writing plain lines to `output`.
fn install_with(
    output: &mut impl Write,
    inspected: anyhow::Result<SetupRuntime>,
) -> anyhow::Result<()> {
    writeln!(output, "  {DISCLOSURE}")?;
    let candidate = inspected?;
    match plan(&candidate) {
        Plan::KeepEnabled => {
            writeln!(
                output,
                "  Installed and signature verified. Switch it off with: lean-ctx setup runtime deactivate"
            )?;
            // A background load that failed or was left for later: again.
            if candidate.companions_pending() {
                load_companions_later(output, companions_deferral_here(), spawn_load_companions)?;
            }
        }
        Plan::KeepDeactivated => writeln!(
            output,
            "  Switched off with `lean-ctx setup runtime deactivate`; left off. Switch it on again with: lean-ctx setup runtime install"
        )?,
        Plan::Enable => {
            candidate.enable()?;
            writeln!(
                output,
                "  Enabled after signature and compatibility checks."
            )?;
        }
        Plan::Install => {
            writeln!(output, "  {DOWNLOADING}")?;
            output.flush()?;
            let installed = candidate.install_runtime_and_enable()?;
            writeln!(
                output,
                "  Installed and enabled after signature and compatibility checks."
            )?;
            if companions_left_for_later(&installed) {
                load_companions_later(output, companions_deferral_here(), spawn_load_companions)?;
            }
        }
        Plan::Fallback => {
            writeln!(output, "  {FALLBACK}")?;
            writeln!(
                output,
                "  Signed staging package installation: lean-ctx setup runtime install --staging --accept-proprietary (explicit artifact and trust pins required)."
            )?;
        }
    }
    Ok(())
}

/// Whether the install left the companion packages for later.
fn companions_left_for_later(installed: &serde_json::Value) -> bool {
    installed["installation"]["semantic_models_reason"] == COMPANIONS_LATER
}

/// The command that loads the companion packages setup left for later.
const LOAD_COMPANIONS: &str = "lean-ctx setup runtime load-companions";

/// Why setup does not start the companion download here, if it does not:
/// in CI (owner decision 23.11.2026) and in the npm postinstall (a child
/// would hold the package directory open during an upgrade).
fn companions_deferral(ci: bool, in_npm_install: bool) -> Option<&'static str> {
    if ci {
        Some("in CI")
    } else if in_npm_install {
        Some("during the npm install")
    } else {
        None
    }
}

/// CI straight from the job's markers (`CI`, `GITLAB_CI`, `GITHUB_ACTIONS`
/// ...): `LEAN_CTX_TELEMETRY_IN_CI=1` is a telemetry opt-in and must not
/// start a download in a CI job.
fn companions_deferral_here() -> Option<&'static str> {
    companions_deferral(
        lean_ctx::core::telemetry_consent::ci_detected(lean_ctx::core::host_env::var),
        std::env::var_os("npm_lifecycle_event").is_some_and(|event| event == "postinstall"),
    )
}

/// The companion packages setup left for later, in one line. `spawn`
/// starts `lean-ctx setup runtime load-companions` detached, so setup does
/// not wait for the download; when `deferred` names a reason nothing is
/// loaded or started and the line names the command.
fn load_companions_later(
    output: &mut impl Write,
    deferred: Option<&str>,
    spawn: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    if let Some(reason) = deferred {
        return writeln!(
            output,
            "  Its model set was not loaded {reason}; load it with: {LOAD_COMPANIONS}"
        );
    }
    match spawn() {
        Ok(()) => writeln!(
            output,
            "  Its model set is loading in the background (several hundred MB)."
        ),
        Err(error) => writeln!(
            output,
            "  Its model set was not loaded ({error}); load it with: {LOAD_COMPANIONS}"
        ),
    }
}

/// Where the background load writes what went wrong.
const COMPANIONS_LOG: &str = "runtime-companions.log";

/// Start this binary as `setup runtime load-companions`, detached, in the
/// data directory (never the npm package directory), its errors in
/// `runtime-companions.log` there. A later setup starts it again while the
/// set is still missing.
fn spawn_load_companions() -> std::io::Result<()> {
    let binary = std::env::current_exe()?;
    let data = lean_ctx::core::data_dir::lean_ctx_data_dir().map_err(std::io::Error::other)?;
    let log = fresh_log(&data.join(COMPANIONS_LOG))?;
    let mut command = std::process::Command::new(binary);
    command
        .args(["setup", "runtime", "load-companions"])
        .current_dir(&data);
    lean_ctx::ipc::process::spawn_detached(
        &mut command,
        lean_ctx::ipc::process::DetachedStderr::File(log),
    )
    .map(drop)
}

/// A new, empty log only this user can read: the previous one (or a
/// symlink planted in its place) is removed, never followed, and the file
/// is created afresh with mode 0600.
fn fresh_log(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Owner decision 23.11.2026: the runtime is installed without a
    /// question; a deactivated runtime stays off.
    #[test]
    fn setup_runtime_plan_installs_without_asking_and_keeps_deactivated_off() {
        assert_eq!(plan_for([true, false, true, true]), Plan::KeepEnabled);
        assert_eq!(plan_for([false, true, true, true]), Plan::KeepDeactivated);
        assert_eq!(plan_for([false, false, true, true]), Plan::Enable);
        assert_eq!(plan_for([false, false, false, true]), Plan::Install);
        assert_eq!(plan_for([false, false, false, false]), Plan::Fallback);
        // Switched off and the package no longer verifies: stays off.
        assert_eq!(plan_for([false, true, false, true]), Plan::KeepDeactivated);
    }

    /// MR !569 review (note 36031): a runtime switched off with `setup
    /// runtime deactivate` stays off on every entry point that installs,
    /// also when its package no longer verifies (an interrupted update, a
    /// damaged package, a format change). `deactivated` comes from the
    /// configuration alone.
    #[test]
    fn every_setup_entry_point_keeps_a_deactivated_runtime_off() {
        use lean_ctx::core::config::IntelligenceRuntimeConfig;
        let entry_points = [
            // `setup --non-interactive`, `install --repair` and `bootstrap`
            // (via `run_repair_setup`) run the full setup step.
            (
                "setup --non-interactive",
                include_str!("../cli/dispatch/setup_cmd.rs").replace("\r\n", "\n"),
                "run_setup_options(&opts, json)",
            ),
            (
                "install --repair / bootstrap",
                include_str!("../cli/dispatch/setup_cmd.rs").replace("\r\n", "\n"),
                "run_repair_setup(json)",
            ),
            (
                "setup runtime configure",
                include_str!("../cli/dispatch/setup_cmd.rs").replace("\r\n", "\n"),
                "setup::configure_runtime()",
            ),
            (
                "onboard",
                include_str!("onboard.rs").replace("\r\n", "\n"),
                "run_setup_with_options(&opts)",
            ),
            (
                "harden --hard",
                include_str!("../cli/harden.rs").replace("\r\n", "\n"),
                "run_setup_with_options(&opts)",
            ),
            (
                "setup (terminal)",
                include_str!("interactive.rs").replace("\r\n", "\n"),
                "configure_runtime()",
            ),
            (
                "full setup step",
                include_str!("with_options.rs").replace("\r\n", "\n"),
                "intelligence::report(!opts.refresh_only)",
            ),
        ];
        // `cloud connect` decides from the same configuration
        // (`runtime_step` is tested in cloud_connect_cmd.rs).
        let connect = include_str!("../cli/cloud_connect_cmd.rs").replace("\r\n", "\n");
        assert!(
            connect
                .contains("runtime_step(granted, config.runtime_consent, runtime_deactivated())")
        );
        // The decline recorded next to config.toml counts there too.
        let compact: String = connect.split_whitespace().collect();
        assert!(compact.contains(".intelligence_runtime.with_recorded_decline().deactivated()"));
        let declined = IntelligenceRuntimeConfig {
            accept_proprietary: true,
            enabled: false,
            channel_url: "https://example.test/channel".into(),
            manifest_sha256: "a".repeat(64),
            ..Default::default()
        };
        assert!(declined.deactivated());
        for (entry, source, call) in entry_points {
            assert!(source.contains(call), "{entry} no longer reaches `{call}`");
            // Every entry point decides through `install_with` -> `plan`:
            // verified or not, channel or not, it stays off.
            for available in [true, false] {
                for channel in [true, false] {
                    assert_eq!(
                        plan_for([false, declined.deactivated(), available, channel]),
                        Plan::KeepDeactivated,
                        "{entry}: available={available} channel={channel}"
                    );
                }
            }
            // `SetupRuntime` itself with no verified version (MR !569
            // review, note 36077): still off.
            let found = SetupRuntime::not_installed(declined.clone());
            assert!(!found.available() && found.deactivated(), "{entry}");
            assert_eq!(plan(&found), Plan::KeepDeactivated, "{entry}");
        }
        // Enabled but no longer verified: repaired from the channel.
        let enabled = IntelligenceRuntimeConfig {
            enabled: true,
            ..declined.clone()
        };
        assert!(!enabled.deactivated());
        assert_eq!(
            plan_for([false, enabled.deactivated(), false, true]),
            Plan::Install
        );
        // Never accepted: installed without a question.
        assert!(!IntelligenceRuntimeConfig::default().deactivated());
    }

    /// Owner decision 23.11.2026 (MR !569): `setup runtime deactivate` run
    /// before the first install is recorded in the configuration; every
    /// entry point then leaves the runtime off, and only an explicit
    /// `setup runtime install` (an activation) switches it on again.
    #[test]
    fn deactivate_before_the_first_install_keeps_every_entry_point_off() {
        use lean_ctx::core::config::IntelligenceRuntimeConfig;
        // Nothing installed, nothing accepted: only the switch-off.
        let mut config = IntelligenceRuntimeConfig::default();
        assert!(!config.deactivated());
        config.switch_off();
        assert!(config.deactivated());
        assert!(config.is_unset(), "no install state is invented");
        let entry_points = [
            "setup --non-interactive",
            "install --repair / bootstrap",
            "setup runtime configure",
            "onboard",
            "harden --hard",
            "setup (terminal)",
            "full setup step",
        ];
        for entry in entry_points {
            for available in [true, false] {
                for channel in [true, false] {
                    assert_eq!(
                        plan_for([false, config.deactivated(), available, channel]),
                        Plan::KeepDeactivated,
                        "{entry}: available={available} channel={channel}"
                    );
                }
            }
            // As `inspect_setup` finds it: the bundled channel, nothing
            // verified, the decline kept.
            let found = SetupRuntime::not_installed(IntelligenceRuntimeConfig {
                channel_url: "https://example.test/channel".into(),
                ..config.clone()
            });
            assert_eq!(plan(&found), Plan::KeepDeactivated, "{entry}");
        }
        // Recorded next to config.toml (3.12.7 refuses an unknown field in
        // this section), and merged back in when the configuration is read.
        let written = toml::to_string(&config).unwrap();
        assert!(!written.contains("declined"), "{written}");
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(lean_ctx::core::config::DECLINED_MARKER);
        lean_ctx::core::config::record_decline_at(&marker, true).unwrap();
        let read = IntelligenceRuntimeConfig {
            declined: lean_ctx::core::config::decline_recorded_at(&marker),
            ..toml::from_str(&written).unwrap()
        };
        assert!(read.deactivated());
        // `setup runtime install` -> `sync-configured` -> activation: on again.
        let installed =
            IntelligenceRuntimeConfig::activated(read, false, "/r".into(), &"b".repeat(64), "k");
        assert!(installed.enabled && !installed.deactivated());
        assert_eq!(
            plan_for([true, installed.deactivated(), true, true]),
            Plan::KeepEnabled
        );
        assert_eq!(
            lean_ctx::core::intelligence_runtime::plain_install(&["install".to_string()]),
            Some(["sync-configured".into(), "--accept-proprietary".into()])
        );
    }

    /// Owner decision 23.11.2026 (MR !569): in CI setup loads no companion
    /// packages and starts nothing; one line names the command.
    #[test]
    fn setup_in_ci_starts_no_companion_download_and_names_the_command() {
        assert_eq!(companions_deferral(true, false), Some("in CI"));
        assert_eq!(companions_deferral(true, true), Some("in CI"));
        // npm postinstall: no child holding the package directory.
        assert_eq!(
            companions_deferral(false, true),
            Some("during the npm install")
        );
        assert_eq!(companions_deferral(false, false), None);
        let mut out = Vec::new();
        let mut spawned = false;
        load_companions_later(&mut out, companions_deferral(true, false), || {
            spawned = true;
            Ok(())
        })
        .unwrap();
        assert!(!spawned, "no detached start in CI");
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(
            text.contains("lean-ctx setup runtime load-companions"),
            "{text}"
        );
        // Outside CI it starts in the background, also in one line.
        let mut out = Vec::new();
        let mut spawned = false;
        load_companions_later(&mut out, None, || {
            spawned = true;
            Ok(())
        })
        .unwrap();
        assert!(spawned);
        assert_eq!(String::from_utf8(out).unwrap().lines().count(), 1);
        // A failed start says so and names the command, in one line.
        let mut out = Vec::new();
        load_companions_later(&mut out, None, || Err(std::io::Error::other("no exe"))).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.contains("no exe") && text.contains("load-companions"));
        // Both install paths ask before they start anything; a later setup
        // starts it again while the set is missing.
        let source = include_str!("intelligence.rs").replace("\r\n", "\n");
        let code = &source[..source.find("#[cfg(test)]\nmod tests").unwrap()];
        assert_eq!(
            code.matches(
                "load_companions_later(output, companions_deferral_here(), spawn_load_companions)"
            )
            .count(),
            2
        );
        assert!(code.contains("if candidate.companions_pending() {"));
        // CI from the job markers, not the telemetry check (which
        // `LEAN_CTX_TELEMETRY_IN_CI=1` switches off).
        assert!(code.contains("telemetry_consent::ci_detected(lean_ctx::core::host_env::var)"));
        assert!(!code.contains("running_in_ci()"));
        assert!(lean_ctx::core::telemetry_consent::ci_detected(|key| {
            (key == "GITLAB_CI").then(|| "true".to_string())
        }));
        // The child: this binary, the data directory, a log file.
        let spawn = &code[code.find("fn spawn_load_companions()").unwrap()..];
        assert!(spawn.contains("std::env::current_exe()"));
        assert!(spawn.contains(".current_dir(&data)"));
        assert!(spawn.contains("DetachedStderr::File(log)"));
        assert!(spawn.contains("fresh_log(&data.join(COMPANIONS_LOG))"));
        // The log: fresh, 0600, a planted symlink replaced, never followed.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join(COMPANIONS_LOG);
        std::fs::write(&log, "old run").unwrap();
        drop(fresh_log(&log).unwrap());
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let target = dir.path().join("target");
            std::fs::write(&target, "keep").unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
            std::fs::remove_file(&log).unwrap();
            std::os::unix::fs::symlink(&target, &log).unwrap();
            drop(fresh_log(&log).unwrap());
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
            let meta = std::fs::symlink_metadata(&log).unwrap();
            assert!(meta.file_type().is_file());
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }
    }

    /// Owner decision 23.11.2026 (MR !569): setup, with or without a
    /// terminal (CI, npm postinstall `onboard`, `bootstrap`, `harden
    /// --hard`), installs the runtime only; the companion packages are
    /// never downloaded inline and load later in the background.
    #[test]
    fn setup_never_downloads_the_companions_inline() {
        let source = include_str!("intelligence.rs").replace("\r\n", "\n");
        let code = &source[..source.find("#[cfg(test)]\nmod tests").unwrap()];
        assert!(code.contains("candidate.install_runtime_and_enable()"));
        assert!(!code.contains(".install_and_enable()"));
        // `report` (every non-terminal path) and `configure_runtime` both
        // go through `install_with`.
        assert_eq!(code.matches("install_with(").count(), 3);
        let later = serde_json::json!({
            "installation": {"semantic_models": "set_not_present", "semantic_models_reason": COMPANIONS_LATER}
        });
        assert!(companions_left_for_later(&later));
        let inline = serde_json::json!({"installation": {"semantic_models": "installed"}});
        assert!(!companions_left_for_later(&inline));
        assert!(!companions_left_for_later(&serde_json::json!({})));
        assert_eq!(COMPANIONS_LATER, "companions_later");
    }

    /// The setup step without a terminal: a failure is one warning, the
    /// step stays ok, and `configure_runtime` never asks for a terminal.
    #[test]
    fn setup_runtime_report_stays_ok_with_a_warning_and_needs_no_terminal() {
        use lean_ctx::core::intelligence_runtime::InstallError;
        let mut out = Vec::new();
        let report = report_with(true, &mut out, || {
            Err(InstallError::PlatformUnavailable.into())
        });
        assert!(report.ok);
        assert!(report.errors.is_empty());
        assert_eq!(
            report.warnings,
            ["Runtime for this platform is not available yet."]
        );
        assert_eq!(report.items[0].status, "unavailable");
        assert!(String::from_utf8(out).unwrap().contains(DISCLOSURE));
        // An update refresh installs nothing and prints nothing.
        let mut out = Vec::new();
        let refresh = report_with(false, &mut out, || {
            Err(InstallError::PlatformUnavailable.into())
        });
        assert!(refresh.ok && out.is_empty());
        assert_eq!(refresh.warnings, [FALLBACK]);
        // No terminal check in the runtime-only step.
        let source = include_str!("intelligence.rs").replace("\r\n", "\n");
        let start = source.find("fn configure_runtime()").unwrap();
        let body = &source[start..start + source[start..].find("\n}\n").unwrap()];
        assert!(
            !body.contains("terminal()"),
            "configure_runtime checks for a terminal"
        );
        assert!(DOWNLOADING.contains("in the background"));
        assert!(!DISCLOSURE.starts_with("Optional"));
    }

    /// Without a terminal (no stdin at all here) the step still runs and
    /// reports the failure as a plain line instead of refusing.
    #[test]
    fn setup_runtime_runs_without_a_terminal_and_says_why_it_failed() {
        use lean_ctx::core::intelligence_runtime::InstallError;
        let mut out = Vec::new();
        let error = install_with(&mut out, Err(InstallError::PlatformUnavailable.into()))
            .expect_err("the inspection failed");
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains(DISCLOSURE));
        assert!(!printed.contains("interactive terminal"));
        assert_eq!(
            not_installed_line(&error),
            "Runtime for this platform is not available yet."
        );
        assert!(!NOTE.contains("never grants consent"));
        assert!(NOTE.contains("with or without a terminal"));
    }
}
