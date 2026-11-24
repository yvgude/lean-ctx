// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx cloud connect`: the single entry to Cloud (CHECKLIST 2.1.1).
//!
//! 1. Browser login through the device flow. The person picks the
//!    workspace on the approval page. Skipped when this machine is
//!    already connected.
//! 2. One confirmation of what this machine shares with that workspace,
//!    asked once per workspace (`core::cloud::connect`).
//! 3. The current project is mapped to its vault. A first share into a
//!    vault waits for the person's yes (`core::cloud::mapping`), asked
//!    here at a terminal.
//!
//! Running it again is safe and asks nothing that was answered. Later
//! sessions never prompt.

use std::io::{BufRead as _, Write as _};
use std::path::Path;

use lean_ctx_time::UtcDateTime;
use serde_json::{Value, json};

use super::cloud_cmd::{flag_value, plan_name};
use lean_ctx_cloud::CloudConfigExt as _;
use lean_ctx_cloud::auth::{
    EnrollmentToken, Session, SetupAnnouncement, announce_setup, enroll_with_token,
    finish_device_flow, start_device_flow_for,
};
use lean_ctx_cloud::connect::{self as steps, ProjectStep};
use lean_ctx_cloud::consent::ConsentError;
use lean_ctx_cloud::mapping::MapError;
use lean_ctx_cloud::state::InstanceRecord;
use lean_ctx_cloud::status::report;
use lean_ctx_cloud::transport::CloudClient;
use lean_ctx_cloud::vault::VaultMap;
use lean_ctx_cloud::{CloudConfig, CloudError, CloudStore};
use lean_ctx_protocol::cloud::enroll::{DeviceCodeV1, MAX_INSTANCE_LABEL_CHARS, TrialPlan};
use lean_ctx_protocol::cloud::lease::CloudLeaseV1;

/// The label this machine gets when `--label` names none: the computer
/// name people see (macOS: `scutil --get ComputerName`), else the short
/// host name. Sent with the device code, so the approval page names it.
fn default_label() -> String {
    #[cfg(target_os = "macos")]
    let computer = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned());
    #[cfg(not(target_os = "macos"))]
    let computer: Option<String> = None;
    label_from(
        computer.as_deref(),
        &gethostname::gethostname().to_string_lossy(),
    )
}

/// The computer name when there is one, else the host name up to its
/// first dot; control characters out, at most the protocol's length.
fn label_from(computer: Option<&str>, host: &str) -> String {
    let clean = |name: &str| -> String {
        name.chars()
            .filter(|c| !c.is_control())
            .take(MAX_INSTANCE_LABEL_CHARS)
            .collect::<String>()
            .trim()
            .to_string()
    };
    let short = host.trim().split('.').next().unwrap_or_default();
    [computer.map(clean), Some(clean(short))]
        .into_iter()
        .flatten()
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "lean-ctx".into())
}

/// `url` as a terminal hyperlink (OSC 8) when stdout is a terminal, plain
/// otherwise. A URL with control characters is never wrapped.
fn link(url: &str, tty: bool) -> String {
    if tty && !url.chars().any(char::is_control) {
        format!("\x1b]8;;{url}\x1b\\{url}\x1b]8;;\x1b\\")
    } else {
        url.to_string()
    }
}

fn stdout_tty() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdout())
}

/// A key set another root signed (Cloud task lean-ctx #2): say which root
/// this engine checked against and how to use the right one; what follows
/// the refusal (the removed key, task #1) stays.
fn name_the_root(
    error: CloudError,
    trust: &lean_ctx_cloud::root_cert::ConnectTrust,
    origin: &str,
) -> CloudError {
    use lean_ctx_cloud::lease::ROOT_MISMATCH;
    let CloudError::Protocol(message) = error else {
        return error;
    };
    let Some(rest) = message.strip_prefix(ROOT_MISMATCH) else {
        return CloudError::Protocol(message);
    };
    let which = if trust.unvouched {
        format!(
            "the development root key from `[cloud] root_key` in your lean-ctx config. For {origin}, remove `root_key` from `[cloud]` so lean-ctx uses the production root it carries, or set it to the root key of that LeanCTX Cloud server"
        )
    } else if trust.certificate.is_some() {
        format!(
            "the root the certificate of {origin} names. Ask the operator of that LeanCTX Cloud server to renew its root certificate"
        )
    } else {
        format!(
            "the production root key built into lean-ctx {}. Update lean-ctx; for a self-hosted LeanCTX Cloud server, set `[cloud] root_key` to its root key",
            env!("CARGO_PKG_VERSION")
        )
    };
    CloudError::Protocol(format!(
        "{origin} answered with a key set not signed by {which}{rest}"
    ))
}

/// The environment variable a managed launcher (LeanCTX Code under MDM)
/// passes a pre-issued enrollment token in.
pub(super) const ENROLLMENT_TOKEN_ENV: &str = "LEAN_CTX_CLOUD_ENROLLMENT_TOKEN";
/// The flag that reads the enrollment token from one line of stdin.
const ENROLLMENT_TOKEN_STDIN: &str = "--enrollment-token-stdin";
/// The longest stdin line read for a token (it is 68 characters).
const ENROLLMENT_TOKEN_MAX_LINE: u64 = 1024;

/// The enrollment token this connect uses, if any: one line of `stdin`
/// with `--enrollment-token-stdin` (an explicit flag wins), else
/// `env` (`LEAN_CTX_CLOUD_ENROLLMENT_TOKEN`; empty counts as unset). Never
/// from argv, where other users of the machine could read it in `ps`.
/// The format is checked here, before anything is sent.
fn enrollment_token(
    args: &[String],
    stdin: &mut dyn std::io::BufRead,
    env: Option<std::ffi::OsString>,
) -> Result<Option<EnrollmentToken>, CloudError> {
    if args
        .iter()
        .any(|a| a == "--enrollment-token" || a.starts_with("--enrollment-token="))
    {
        return Err(CloudError::Config(format!(
            "an enrollment token is never passed on the command line, where other users can read it: pipe it to `{ENROLLMENT_TOKEN_STDIN}` or set {ENROLLMENT_TOKEN_ENV}"
        )));
    }
    if args.iter().any(|a| a == ENROLLMENT_TOKEN_STDIN) {
        let mut line = zeroize::Zeroizing::new(String::new());
        std::io::Read::take(stdin, ENROLLMENT_TOKEN_MAX_LINE)
            .read_line(&mut line)
            .map_err(|e| {
                CloudError::Local(format!(
                    "read the enrollment token from stdin: {}",
                    e.kind()
                ))
            })?;
        if line.trim().is_empty() {
            return Err(CloudError::Config(format!(
                "{ENROLLMENT_TOKEN_STDIN}: stdin held no enrollment token"
            )));
        }
        return EnrollmentToken::parse(&line).map(Some);
    }
    let Some(raw) = env.filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let raw = zeroize::Zeroizing::new(raw.into_string().map_err(|_| {
        CloudError::Config(format!(
            "{ENROLLMENT_TOKEN_ENV}: the enrollment token is malformed (not UTF-8)"
        ))
    })?);
    EnrollmentToken::parse(&raw).map(Some)
}

/// `--trial [--plan personal|team]`: the plan whose one-step trial the
/// browser starts (`--plan` defaults to personal and needs `--trial`).
fn trial_choice(args: &[String]) -> Result<Option<TrialPlan>, CloudError> {
    let plan = flag_value(args, "--plan")?;
    match (args.iter().any(|a| a == "--trial"), plan) {
        (false, None) => Ok(None),
        (false, Some(_)) => Err(CloudError::Config(
            "--plan is only valid with --trial".into(),
        )),
        (true, None) => Ok(Some(TrialPlan::Personal)),
        (true, Some(plan)) => TrialPlan::parse(plan).map(Some).ok_or_else(|| {
            let shown: String = plan.chars().filter(|c| !c.is_control()).take(32).collect();
            CloudError::Config(format!("--plan {shown}: choose personal or team"))
        }),
    }
}

/// How a device flow ended, worded for a `--trial` connect: a Cloud that
/// refuses a used trial only in the browser lets the code expire, so the
/// hint names the plans page ([`CloudError::for_trial`]).
fn trial_aware(error: CloudError, trial: Option<TrialPlan>) -> CloudError {
    if trial.is_some() {
        error.for_trial()
    } else {
        error
    }
}

/// `cloud connect --json`, event `code`: what the person opens and checks.
fn code_event(code: &DeviceCodeV1) -> Value {
    json!({
        "event": "code",
        "verification_uri": code.verification_uri,
        "verification_uri_complete": code.verification_uri_complete,
        "user_code": code.user_code,
        "expires_in": code.expires_in,
    })
}

/// `cloud connect --json`, event `connected`: the plan's wire name and
/// when a running trial ends (`null` without one). `runtime` (only when
/// this connect installed the runtime the plan includes) says how that
/// ended; a failed install never turns `connected` into `error`.
fn connected_event(lease: &CloudLeaseV1, runtime: Option<Value>) -> Value {
    let mut event = json!({
        "event": "connected",
        "plan": lease.plan,
        "trial_ends_at": lean_ctx_cloud::status::trial_ends_at(lease),
    });
    if let Some(runtime) = runtime {
        event["runtime"] = runtime;
    }
    event
}

/// `cloud connect --json`, event `error` (the command exits 1).
fn error_event(error: &CloudError) -> Value {
    json!({
        "event": "error",
        "code": error.event_code(),
        "message": error.to_string(),
    })
}

/// One event per line on stdout, flushed at once for the reader.
fn emit(event: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{event}");
    let _ = out.flush();
}

/// The enrollment token of this run, removed from the environment.
fn take_enrollment_token(args: &[String]) -> Result<Option<EnrollmentToken>, CloudError> {
    // Read and checked before anything else, so a malformed token never
    // reaches the network.
    let env = std::env::var_os(ENROLLMENT_TOKEN_ENV);
    let had_env = env.is_some();
    let token = enrollment_token(args, &mut std::io::stdin().lock(), env);
    if had_env {
        // SAFETY: removed on the CLI's main thread before this command
        // spawns anything, so the daemon, the runtime installer and other
        // children never inherit the token.
        unsafe { std::env::remove_var(ENROLLMENT_TOKEN_ENV) };
    }
    token
}

/// `--url`, else the configured Cloud.
fn origin_for(config: &CloudConfig, args: &[String]) -> Result<String, CloudError> {
    match flag_value(args, "--url")? {
        Some(url) => lean_ctx_cloud::config::normalize_origin(url),
        None => config.origin(),
    }
}

/// `lean-ctx cloud connect [--url <origin>] [--label <name>] [--no-browser] [--yes]
/// [--enrollment-token-stdin] [--trial [--plan personal|team]] [--json]`.
///
/// Prints the three steps the web shows too (approve this machine,
/// connected, share this project) and ends with "Done." and where to look.
/// With an enrollment token (stdin or `LEAN_CTX_CLOUD_ENROLLMENT_TOKEN`)
/// step 1 enrolls without a browser. `--json`: see [`run_json`].
pub(super) fn run(config: &CloudConfig, args: &[String]) -> Result<(), CloudError> {
    if args.iter().any(|a| a == "--json") {
        if let Err(error) = run_json(config, args) {
            emit(&error_event(&error));
            std::process::exit(1);
        }
        return Ok(());
    }
    let token = take_enrollment_token(args)?;
    let trial = trial_choice(args)?;
    let store = CloudStore::open()?;
    let origin = origin_for(config, args)?;
    if let Some(existing) = store.instance()? {
        let current = report(&store, config, UtcDateTime::now());
        if current.connected() {
            println!(
                "Already connected to {} as {} ({}). Run `lean-ctx cloud disconnect` first to switch.",
                existing.url, existing.label, existing.org_name
            );
            let session = lean_ctx_cloud::root_cert::trust_root(&store, config, &existing.url)
                .and_then(|root| Session::load(store, root));
            return match session {
                Ok(session) => {
                    let outcome = after_login(config, &session, &existing, args)?;
                    print_done(&existing, &outcome);
                    Ok(())
                }
                // `status` explains a broken session; connect stays a no-op.
                Err(error) => {
                    tracing::info!("cloud connect: {error}");
                    Ok(())
                }
            };
        }
        // A lapsed or broken enrollment is replaced; its keys are reused.
    }
    let chosen: Option<String> = flag_value(args, "--label")?
        .map(|label| label.chars().take(MAX_INSTANCE_LABEL_CHARS).collect());
    let label = chosen.unwrap_or_else(default_label);
    let client = CloudClient::new(&origin)?;
    println!("LeanCTX Cloud — connect this machine");
    // A self-hosted Cloud is trusted when LeanCTX vouched for its root.
    let trust = lean_ctx_cloud::root_cert::for_connect(&client, config, &origin)
        .map_err(|error| unreachable(error, &origin))?;
    if trust.unvouched {
        println!(
            "(Development LeanCTX Cloud server: its root key comes from `[cloud] root_key` in your config.)"
        );
    }
    let root = trust.root;
    let here = project_here();
    let (early, named, announced, enrolled) = if let Some(token) = &token {
        // A pre-issued enrollment token (MDM, unattended hosts): no device
        // code, no browser; the team is the one the token was issued for.
        println!("\nEnroll this machine with your team's enrollment token");
        print!("  Enrolling…");
        let _ = std::io::stdout().flush();
        let enrolled = enroll_with_token(&store, &client, token, &label, &root)
            .map_err(|error| unreachable(error, &origin));
        (None, None, false, enrolled)
    } else {
        let code = start_device_flow_for(&client, Some(label.as_str()), trial)
            .map_err(|error| unreachable(error, &origin))?;
        // C0 2.1: the approval page also asks "Share this project" (one code).
        // The folder name and git remote reach Cloud only after the person's
        // yes here (or `--yes`); without one the page names no project.
        let early = here
            .as_ref()
            .filter(|_| code.offers_device_setup())
            .and_then(|_| share_before_approval(config, args));
        let mut announce_config = config.clone();
        if early == Some(false) {
            steps::without_sharing(&mut announce_config);
        }
        let named = here.as_deref().filter(|_| early == Some(true));
        let announced =
            announce_setup(&client, &code, &setup_announcement(&announce_config, named));
        println!("\nApprove this machine in your browser");
        if args.iter().any(|a| a == "--no-browser")
            || std::env::var_os("LEAN_CTX_NO_BROWSER").is_some()
        {
            println!(
                "  Open this link on any device: {}",
                link(&code.verification_uri_complete, stdout_tty())
            );
        } else {
            let _ = open_browser(&code.verification_uri_complete);
            println!(
                "  We opened: {}",
                link(&code.verification_uri_complete, stdout_tty())
            );
        }
        println!(
            "  Check the code  {}  in the browser: it must be the same (valid {} minutes).",
            code.user_code,
            code.expires_in.div_ceil(60).max(1)
        );
        if let Some(plan) = trial {
            println!(
                "  There you sign in and start your {} trial for this machine.",
                trial_plan_name(plan)
            );
        } else if announced {
            println!(
                "  There you sign in, choose the workspace and confirm what this machine shares."
            );
        } else {
            println!("  There you sign in and choose the workspace for this machine.");
        }
        print!("  Waiting for you to approve…");
        let _ = std::io::stdout().flush();
        let enrolled =
            finish_device_flow(&store, &client, &code, &label, &root, &std::thread::sleep)
                .map_err(|error| trial_aware(error, trial));
        (early, named, announced, enrolled)
    };
    let (instance, sharing) = enrolled
        .map_err(|error| name_the_root(error, &trust, &origin))
        .map_err(|error| {
            println!();
            // A cooling-down Personal slot: `cloud status` keeps saying until when
            // and where the web recovery is (P7).
            if matches!(&error, CloudError::QuotaExceeded(q) if q.cooling_slot()) {
                println!("Instances:    {origin} (Settings › Connected instances)");
            }
            lean_ctx_cloud::throttle::note(
                &store,
                lean_ctx_cloud::throttle::Scope::Connect,
                error,
                UtcDateTime::now(),
            )
        })?;
    println!(
        "{}",
        if token.is_some() {
            " enrolled."
        } else {
            " approved."
        }
    );
    if let Some(certificate) = &trust.certificate {
        lean_ctx_cloud::root_cert::store_certificate(&store, certificate)?;
    }
    // An admin allowed this new enrolment: an earlier revocation is over.
    lean_ctx_cloud::revoked::clear(&store)?;
    lean_ctx_cloud::throttle::clear_connect(&store);
    let session = Session::load(store, root)?;
    // Prove the instance key works before reporting success.
    session.client()?;
    println!(
        "\nConnected {} to {} ({} plan).",
        instance.label,
        instance.workspace_label(),
        plan_name(&session.lease.lease.plan)
    );
    if let Some(trial) = lean_ctx_cloud::status::trial_line(&session.lease.lease) {
        println!("  {trial}.");
    }
    // Sync runs in the daemon's background: keep it running by itself, and
    // restart it so its scheduler starts for this connection (a test or an
    // isolated instance never installs a user-wide service).
    if let Some(line) = lean_ctx::daemon_autostart::supervision_line(
        lean_ctx::daemon_autostart::ensure_supervised(true),
    ) {
        println!("{line}");
    }
    announce_and_pull(&session);
    // The page answered "Share this project": recorded like the terminal's
    // answer, so no session asks again; a no switches every upload off.
    // E2E 27.10. E3: outside a project the page offers no share choice, so
    // whatever it sent is no answer and nothing is recorded or switched off.
    // The page asks only about a project it was named; otherwise the
    // answer given here before the approval stands.
    let sharing = sharing.filter(|_| project_here().is_some() && named.is_some());
    let answer = match sharing {
        Some(Some(share)) => Some((share, Some(steps::SOURCE_BROWSER))),
        _ => early.map(|share| (share, None)),
    };
    let mut device = false;
    if let Some((share, source)) = answer {
        if !share {
            lean_ctx::core::config::Config::update_global(|c| steps::without_sharing(&mut c.cloud))
                .map_err(|e| CloudError::Local(format!("record the answer: {e}")))?;
        }
        steps::record_from(
            &session.store,
            &instance.org_id,
            share,
            UtcDateTime::now(),
            source,
        )?;
        device = share && source.is_some();
    }
    let config = if answer.is_some_and(|(share, _)| !share) {
        let mut off = config.clone();
        steps::without_sharing(&mut off);
        off
    } else {
        config.clone()
    };
    let outcome = after_login_with(&config, &session, &instance, args, device, announced)?;
    if announced {
        report_setup(&session, &outcome);
    }
    print_done(&instance, &outcome);
    engine_metrics_notice(&config, &session, &instance);
    // Owner decision 23.11.2026: installed without a question (consent comes
    // from the terms accepted at sign-up); a failure never fails the connect.
    if let Some(runtime) = included_runtime(&config, &session) {
        println!("{}", runtime_line(&runtime));
    }
    Ok(())
}

/// What connect did with the runtime the plan includes.
#[derive(Debug)]
enum IncludedRuntime {
    /// Switched off with `setup runtime deactivate` (also before the first
    /// install): left off.
    KeptOff,
    /// Installed through the signed channel, or why not.
    Installed(Result<String, lean_ctx::core::intelligence_runtime::InstallError>),
}

/// What connect does with the runtime the plan includes: nothing when the
/// lease does not grant it or this machine installed it already; left off
/// when it was switched off (only `setup runtime install` turns it on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeStep {
    Install,
    KeepOff,
}

fn runtime_step(granted: bool, installed_before: bool, deactivated: bool) -> Option<RuntimeStep> {
    if !granted || installed_before {
        None
    } else if deactivated {
        Some(RuntimeStep::KeepOff)
    } else {
        Some(RuntimeStep::Install)
    }
}

/// The runtime the plan includes, installed through the signed channel
/// when the lease grants it, this machine has not installed it yet and it
/// was not switched off. Prints nothing (`--json` writes events only).
fn included_runtime(config: &CloudConfig, session: &Session) -> Option<IncludedRuntime> {
    let granted = session
        .lease
        .lease
        .grants(lean_ctx_protocol::cloud::lease::CloudRight::Runtime);
    match runtime_step(granted, config.runtime_consent, runtime_deactivated())? {
        RuntimeStep::KeepOff => Some(IncludedRuntime::KeptOff),
        RuntimeStep::Install => Some(IncludedRuntime::Installed(install_included_runtime())),
    }
}

/// `SetupRuntime::deactivated()` from the configuration, the same as every
/// setup entry point. An unreadable configuration installs nothing either.
fn runtime_deactivated() -> bool {
    lean_ctx::core::config::Config::try_load_global().map_or(true, |config| {
        config
            .intelligence_runtime
            .with_recorded_decline()
            .deactivated()
    })
}

/// Said when connect leaves a switched-off runtime off.
const RUNTIME_KEPT_OFF: &str = "Runtime switched off on this computer; left off. Switch it on with: lean-ctx setup runtime install";

/// Install through the signed channel; recorded only once it worked, so
/// the next connect tries again after a failure.
fn install_included_runtime() -> Result<String, lean_ctx::core::intelligence_runtime::InstallError>
{
    let installed = lean_ctx::core::intelligence_runtime::run(&[
        "sync-configured".to_string(),
        "--accept-proprietary".to_string(),
    ])?;
    if let Err(error) =
        lean_ctx::core::config::Config::update_global(|config| config.cloud.runtime_consent = true)
    {
        tracing::info!("record the runtime install: {error}");
    }
    Ok(installed)
}

/// One plain line for the terminal.
fn runtime_line(runtime: &IncludedRuntime) -> String {
    use lean_ctx::core::intelligence_runtime::InstallError;
    let installed = match runtime {
        IncludedRuntime::KeptOff => return RUNTIME_KEPT_OFF.into(),
        IncludedRuntime::Installed(installed) => installed,
    };
    match installed {
        Ok(_) => "LeanCTX Intelligence runtime installed.".into(),
        Err(InstallError::PlatformUnavailable) => {
            format!("{}.", InstallError::PlatformUnavailable)
        }
        Err(error) => format!(
            "Runtime not installed: {error}. This computer is connected; retry with `lean-ctx cloud runtime install`."
        ),
    }
}

/// The `runtime` member of the `connected` event.
fn runtime_member(runtime: &IncludedRuntime) -> Value {
    use lean_ctx::core::intelligence_runtime::InstallError;
    let installed = match runtime {
        IncludedRuntime::KeptOff => {
            return json!({ "state": "switched_off", "message": RUNTIME_KEPT_OFF });
        }
        IncludedRuntime::Installed(installed) => installed,
    };
    match installed {
        Ok(_) => json!({ "state": "installed" }),
        Err(error @ InstallError::PlatformUnavailable) => {
            json!({ "state": "unavailable", "message": error.to_string() })
        }
        Err(error) => json!({ "state": "failed", "message": error.to_string() }),
    }
}

/// E44 at connect (SPEC-ENGINE §3.6) and the effective policy: neither
/// ever fails the connect.
fn announce_and_pull(session: &Session) {
    // A Cloud before C0 2.0 has no instance card.
    if let Err(error) = lean_ctx_cloud::workers::register::announce_instance(session) {
        tracing::info!("cloud instance card: {error}");
    }
    // Unified Control (2.x): the effective policy is in force and acked from
    // the connect on; the scheduler checks it again every interval. A 1.x
    // Cloud keeps its scheduled pull only.
    if lean_ctx_cloud::bus::relay_v2::speaks_v2(&session.store)
        && session
            .lease
            .lease
            .grants(lean_ctx_protocol::cloud::lease::CloudRight::PolicyPull)
        && let Err(error) = lean_ctx_cloud::policy::pull(session)
    {
        tracing::info!("cloud policy at connect: {error}");
    }
}

fn trial_plan_name(plan: TrialPlan) -> &'static str {
    match plan {
        TrialPlan::Personal => "Personal",
        TrialPlan::Team => "Team",
    }
}

/// `cloud connect --json` (LeanCTX Code and other launchers): asks
/// nothing and prints only line-delimited JSON events on stdout:
/// `code` (open `verification_uri_complete`, check `user_code`), then
/// `connected`, or `error` (printed by [`run`], exit 1). An already
/// connected machine answers `connected` for its connection at once.
/// Sharing is not asked: it follows the `[cloud]` settings until a person
/// answers `lean-ctx cloud connect` in a terminal.
fn run_json(config: &CloudConfig, args: &[String]) -> Result<(), CloudError> {
    let token = take_enrollment_token(args)?;
    let trial = trial_choice(args)?;
    let store = CloudStore::open()?;
    let origin = origin_for(config, args)?;
    if store.instance()?.is_some() {
        let current = report(&store, config, UtcDateTime::now());
        if let (true, Some(lease)) = (current.connected(), &current.lease) {
            emit(&connected_event(lease, None));
            return Ok(());
        }
    }
    let chosen: Option<String> = flag_value(args, "--label")?
        .map(|label| label.chars().take(MAX_INSTANCE_LABEL_CHARS).collect());
    let label = chosen.unwrap_or_else(default_label);
    let client = CloudClient::new(&origin)?;
    let trust = lean_ctx_cloud::root_cert::for_connect(&client, config, &origin)?;
    let root = trust.root;
    let enrolled = if let Some(token) = &token {
        enroll_with_token(&store, &client, token, &label, &root)
    } else {
        let code = start_device_flow_for(&client, Some(label.as_str()), trial)?;
        emit(&code_event(&code));
        if !args.iter().any(|a| a == "--no-browser") {
            let _ = open_browser(&code.verification_uri_complete);
        }
        finish_device_flow(&store, &client, &code, &label, &root, &std::thread::sleep)
            .map_err(|error| trial_aware(error, trial))
    };
    let (instance, _) = enrolled
        .map_err(|error| name_the_root(error, &trust, &origin))
        .map_err(|error| {
            lean_ctx_cloud::throttle::note(
                &store,
                lean_ctx_cloud::throttle::Scope::Connect,
                error,
                UtcDateTime::now(),
            )
        })?;
    if let Some(certificate) = &trust.certificate {
        lean_ctx_cloud::root_cert::store_certificate(&store, certificate)?;
    }
    lean_ctx_cloud::revoked::clear(&store)?;
    lean_ctx_cloud::throttle::clear_connect(&store);
    let session = Session::load(store, root)?;
    session.client()?;
    // The supervision line is for people; stdout carries events only.
    let _ = lean_ctx::daemon_autostart::ensure_supervised(true);
    announce_and_pull(&session);
    // Default on, as in the terminal; the outcome is not an event.
    let _ = confirm_metrics(config, &session, &instance);
    let runtime = included_runtime(config, &session).map(|runtime| runtime_member(&runtime));
    emit(&connected_event(&session.lease.lease, runtime));
    Ok(())
}

/// An unreachable Cloud before anything was sent, in plain words.
fn unreachable(error: CloudError, origin: &str) -> CloudError {
    match error {
        CloudError::Offline(_) => CloudError::Refused(format!(
            "can't reach LeanCTX Cloud at {origin}. Check your connection and try again; LeanCTX keeps working locally meanwhile."
        )),
        other => other,
    }
}

/// How step 3 ended for the project here.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    /// Not inside a project with a stable identity (no git origin, id file
    /// or manifest).
    NoProject,
    /// The project syncs with the vault `ns/name`.
    Shared { ns: String, name: String },
    /// Nothing leaves this machine, and why.
    KeptLocal(String),
    /// The project waits for the person's yes.
    Waiting,
    /// The project has no vault yet; the background sync retries.
    Retry(String),
}

/// Usage metrics at connect (telemetry SPEC §4, K4; owner decision
/// 23.11.2026: no onboarding question, default on, off in Settings). Only
/// the unset plan default is confirmed; one plain line says how to switch
/// them off whenever they are shared or connect switched them on.
fn engine_metrics_notice(config: &CloudConfig, session: &Session, instance: &InstanceRecord) {
    let workspace = instance.workspace_label();
    match confirm_metrics(config, session, instance) {
        None => {}
        Some(Ok(metrics)) => {
            if let Some(line) = metrics_line(&workspace, metrics) {
                println!("\n{line}");
            }
        }
        Some(Err(error)) => eprintln!("cloud: usage metrics not switched on: {error}"),
    }
}

/// What connect knows about usage metrics afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Metrics {
    /// The workspace said they are shared.
    Shared,
    /// The workspace could not be asked; the last known state says shared.
    LastKnownShared,
    /// Connect confirmed the plan default; the workspace does not share
    /// them yet (for example while its policy is still being set up).
    SwitchedOn,
    /// Off in Settings, by an admin, the lease or policy: left off.
    Off,
}

/// Whether connect may confirm usage metrics on this machine: released,
/// `cloud.metrics_upload` on, and no `DO_NOT_TRACK=1` or
/// `LEAN_CTX_TELEMETRY=off` in the environment. The environment only keeps
/// connect from switching sharing on; the upload gate is the workspace's
/// state and `cloud.metrics_upload`, so a workspace that already shares is
/// still named as shared.
fn metrics_allowed(
    released: bool,
    metrics_upload: bool,
    do_not_track: Option<&str>,
    telemetry: Option<&str>,
) -> bool {
    released
        && metrics_upload
        && !lean_ctx::core::config::TelemetryConfig::environment_disables(do_not_track, telemetry)
}

/// Switch usage metrics on where the workspace still waits for a first
/// confirmation. `None` before the release or when this machine (config
/// or environment) switched them off. Prints nothing (`--json` writes
/// events only).
fn confirm_metrics(
    config: &CloudConfig,
    session: &Session,
    instance: &InstanceRecord,
) -> Option<Result<Metrics, CloudError>> {
    use lean_ctx::core::host_env::var;
    use lean_ctx_cloud::metrics_v2::{gate, upload};
    let lease = &session.lease.lease;
    let released = upload::released(&instance.url);
    settle_metrics(
        released && config.metrics_upload,
        metrics_allowed(
            released,
            config.metrics_upload,
            var("DO_NOT_TRACK").as_deref(),
            var("LEAN_CTX_TELEMETRY").as_deref(),
        ),
        || upload::refresh_sharing(session).map(|_| ()),
        || gate::decide(&session.store, config, instance, lease, UtcDateTime::now()),
        || upload::confirm(session).map(|_| ()),
    )
}

/// The connect decision about usage metrics, apart from the network:
/// `fetch` asks the workspace for its current state, `decide` is the gate
/// on what is known, `confirm` switches the plan default on.
///
/// - Off on this machine (or not released): `None`, nothing is sent.
/// - `may_confirm` false (`DO_NOT_TRACK=1`, `LEAN_CTX_TELEMETRY=off`):
///   nothing is confirmed; a workspace that already shares is said to.
/// - Not confirmed yet (the unset default, as the workspace just said):
///   confirmed; shared, or switched on and shared once the workspace
///   allows it.
/// - Any other off (Settings, an admin, the lease, policy): stays off, and
///   no line says it is shared.
/// - The workspace could not be asked: nothing is confirmed.
fn settle_metrics(
    allowed: bool,
    may_confirm: bool,
    fetch: impl FnOnce() -> Result<(), CloudError>,
    decide: impl Fn() -> lean_ctx_cloud::metrics_v2::gate::Sharing,
    confirm: impl FnOnce() -> Result<(), CloudError>,
) -> Option<Result<Metrics, CloudError>> {
    use lean_ctx_cloud::metrics_v2::gate::{OffReason, Sharing};
    if !allowed {
        return None;
    }
    if let Err(error) = fetch() {
        return Some(if decide().is_on() {
            Ok(Metrics::LastKnownShared)
        } else {
            Err(error)
        });
    }
    match decide() {
        Sharing::On { .. } => Some(Ok(Metrics::Shared)),
        Sharing::Off {
            reason: OffReason::NotConfirmed,
            ..
        } if !may_confirm => Some(Ok(Metrics::Off)),
        Sharing::Off {
            reason: OffReason::NotConfirmed,
            ..
        } => Some(confirm().map(|()| {
            if decide().is_on() {
                Metrics::Shared
            } else {
                Metrics::SwitchedOn
            }
        })),
        Sharing::Off { .. } => Some(Ok(Metrics::Off)),
    }
}

/// The one line connect prints about usage metrics; none when they are
/// off.
fn metrics_line(workspace: &str, metrics: Metrics) -> Option<String> {
    let what = "pseudonymous counts (including full re-fetches) and the names of unregulated programs; never file content, prompts or secrets. Switch them off in Settings or with `lean-ctx config set cloud.metrics_upload false`.";
    let head = match metrics {
        Metrics::Shared => format!("Usage metrics are shared with {workspace}"),
        Metrics::LastKnownShared => format!(
            "Usage metrics were last known to be shared with {workspace} (the workspace could not be asked just now)"
        ),
        Metrics::SwitchedOn => format!(
            "Usage metrics are switched on for {workspace} and are shared once the workspace allows it"
        ),
        Metrics::Off => return None,
    };
    Some(format!("{head}: {what}"))
}

/// Steps 2 and 3 for a connected machine.
fn after_login(
    config: &CloudConfig,
    session: &Session,
    instance: &InstanceRecord,
    args: &[String],
) -> Result<Outcome, CloudError> {
    after_login_with(config, session, instance, args, false, false)
}

/// The checkout here, when LeanCTX can identify it (git origin, id file or
/// manifest): what connect shares and the page names.
fn project_here() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    lean_ctx_cloud::vault::project_identity(Path::new(&root))?;
    Some(root)
}

/// What the approval page shows (C0 2.1): the folder name (as the first
/// share's consent names it) and the remote, plus the upload channels.
fn setup_announcement(config: &CloudConfig, root: Option<&str>) -> SetupAnnouncement {
    let project = root.map(|root| {
        let root = Path::new(root);
        let name: String = project_name(root)
            .chars()
            .filter(|c| !c.is_control())
            .take(64)
            .collect();
        let remote = lean_ctx_cloud::identity_v2::origin_remote(root)
            .filter(|r| (1..=500).contains(&r.chars().count()));
        (name, remote)
    });
    SetupAnnouncement {
        project: project.filter(|(name, _)| !name.trim().is_empty()),
        shares: steps::share_codes(config),
    }
}

/// Tell the approval page how the project step ended (best effort).
fn report_setup(session: &Session, outcome: &Outcome) {
    use lean_ctx_protocol::cloud::enroll::{DEVICE_SETUP_RESULT_SCHEMA, DeviceSetupResultV1};
    let (state, vault) = match outcome {
        Outcome::Shared { ns, name } => ("shared", Some(format!("{ns}/{name}"))),
        Outcome::NoProject => ("no_project", None),
        Outcome::KeptLocal(_) => ("kept_local", None),
        Outcome::Waiting | Outcome::Retry(_) => ("failed", None),
    };
    let body = DeviceSetupResultV1 {
        schema: DEVICE_SETUP_RESULT_SCHEMA.into(),
        state: state.into(),
        vault,
    };
    let sent = serde_json::to_value(&body)
        .map_err(|e| CloudError::Local(e.to_string()))
        .and_then(|body| session.with_client(|client| client.post("/device/setup/result", &body)));
    if let Err(error) = sent {
        tracing::info!("cloud device setup result: {error}");
    }
}

/// Steps 2 and 3; `device`: the approval page just confirmed sharing this
/// project, so its first share is answered there (C0 2.1), not here;
/// `announced`: this Cloud asks about sharing on the page, so `--yes`
/// does not answer it.
fn after_login_with(
    config: &CloudConfig,
    session: &Session,
    instance: &InstanceRecord,
    args: &[String],
    device: bool,
    announced: bool,
) -> Result<Outcome, CloudError> {
    // Sharing is asked per project: a folder LeanCTX cannot identify (no
    // git remote) has nothing to share, whatever was answered (E3).
    if project_here().is_none() {
        return Ok(Outcome::NoProject);
    }
    let Some(shared) = confirm_sharing(config, &session.store, instance, args, announced)? else {
        return Ok(Outcome::Waiting);
    };
    if !shared {
        return Ok(Outcome::KeptLocal(format!(
            "nothing from this machine is shared with {}",
            instance.org_name
        )));
    }
    let cwd = std::env::current_dir().map_err(|e| CloudError::Local(e.to_string()))?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    // A yes given again in this run turned the channels back on.
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    Ok(map_project(&config, session, Path::new(&root), device))
}

/// The project's name as people know it: its folder.
fn project_name(root: &Path) -> String {
    root.file_name().map_or_else(
        || root.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The closing block: what happened, where to look, what comes next.
fn print_done(instance: &InstanceRecord, outcome: &Outcome) {
    let web = instance
        .web_url
        .clone()
        .unwrap_or_else(|| instance.url.clone());
    let cwd = std::env::current_dir().unwrap_or_default();
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let project = project_name(Path::new(&root));
    println!();
    for line in done_lines(outcome, &instance.org_name, &web, &project, stdout_tty()) {
        println!("{line}");
    }
}

/// The lines of the closing block.
fn done_lines(outcome: &Outcome, org: &str, web: &str, project: &str, tty: bool) -> Vec<String> {
    let web = web.trim_end_matches('/');
    let app = format!("  Open:  {}", link(&format!("{web}/app/"), tty));
    match outcome {
        Outcome::Shared { ns, name } => vec![
            format!(
                "Done. Your agents now share what they learn about {project} with your team in {org}."
            ),
            format!("  Open:  {}", link(&format!("{web}/o/{ns}/v/{name}/"), tty)),
            "  Next:  Keep working as usual; LeanCTX syncs in the background.".into(),
            "         Another project? Run `lean-ctx cloud connect` there.".into(),
        ],
        Outcome::NoProject => vec![format!("Done. This computer is connected to {org}."), app],
        Outcome::KeptLocal(why) => vec![
            format!("Done. {project} stays on this machine: {why}."),
            format!("  Your agents still use {org}'s knowledge and policies."),
            app,
            "  Changed your mind? Run `lean-ctx cloud connect` here again in your own terminal: it asks again.".into(),
        ],
        Outcome::Waiting => vec![
            format!(
                "Almost done. {project} stays on this machine until you confirm it yourself: run `lean-ctx cloud connect` here, in your own terminal (agents cannot confirm it)."
            ),
            app,
        ],
        Outcome::Retry(why) => vec![
            format!(
                "Connected, but {project} is not set up yet: {why}. LeanCTX tries again in the background; nothing leaves this machine before that."
            ),
            app,
        ],
    }
}

/// Step 3's line for a recorded answer; one from the approval page says so.
fn decided_line(decided: &steps::SharingDecision, org: &str) -> String {
    match (decided.source.as_deref(), decided.shared) {
        (Some(steps::SOURCE_BROWSER), true) => {
            format!("answered in the browser: shared with {org}.")
        }
        (Some(steps::SOURCE_BROWSER), false) => {
            "answered in the browser: not shared. Nothing from this machine is shared.".into()
        }
        (_, true) => format!("Sharing with {org} is on: you confirmed it."),
        (_, false) => format!("Sharing with {org} is off: you said no."),
    }
}

/// A recorded "no" is asked again when a person can answer (a terminal);
/// a recorded yes stands. `--yes` never turns a no into a yes.
fn asks_again(decided: &steps::SharingDecision, terminal: bool) -> bool {
    !decided.shared && terminal
}

/// `--yes` answers the sharing question only where the terminal asks it:
/// a Cloud that took the setup (C0 2.1) asks on the approval page.
fn yes_answers_sharing(args: &[String], announced: bool) -> bool {
    !announced && super::prompt::wants_yes(args)
}

/// Against a Cloud that offers the device setup (C0 2.1): ask before the
/// approval, because only a yes may send the project's folder name and git
/// remote to Cloud (`/device/setup`). `None` when nobody can answer (no
/// terminal, no `--yes`): the project is then not named.
fn share_before_approval(config: &CloudConfig, args: &[String]) -> Option<bool> {
    let assume_yes = super::prompt::wants_yes(args);
    if !assume_yes && !lean_ctx::core::host_env::stdin_is_terminal() {
        return None;
    }
    println!("\nBefore you approve · Share this project");
    println!("  With the workspace you choose, this machine would share:");
    for item in steps::shared_items(config) {
        println!("  - {item}");
    }
    Some(super::prompt::confirm(
        "Share this project? A yes sends its folder name and git remote to LeanCTX Cloud now, so the approval page can show it",
        assume_yes,
    ))
}

/// The sharing answer for the instance's workspace: the recorded one, or
/// asked now. `None` when no person could answer (no terminal, no
/// `--yes`); the `[cloud]` settings then apply unchanged, as before.
fn confirm_sharing(
    config: &CloudConfig,
    store: &CloudStore,
    instance: &InstanceRecord,
    args: &[String],
    announced: bool,
) -> Result<Option<bool>, CloudError> {
    println!("\nShare this project");
    if let Some(decided) = steps::decision(store, &instance.org_id) {
        println!("  {}", decided_line(&decided, &instance.org_name));
        let terminal = lean_ctx::core::host_env::stdin_is_terminal();
        if !asks_again(&decided, terminal) {
            return Ok(Some(decided.shared));
        }
        // E2E 27.10. E2: a recorded "no" is asked again here, in a terminal.
        if !super::prompt::confirm("Share it with the workspace now?", false) {
            return Ok(Some(false));
        }
        lean_ctx::core::config::Config::update_global(|c| steps::with_sharing(&mut c.cloud))
            .map_err(|e| CloudError::Local(format!("record the answer: {e}")))?;
        steps::record(store, &instance.org_id, true, UtcDateTime::now())?;
        return Ok(Some(true));
    }
    let items = steps::shared_items(config);
    if items.is_empty() {
        println!(
            "  This machine shares nothing with {}: every upload is switched off in your `[cloud]` settings.",
            instance.org_name
        );
        // Not recorded: turning a channel on later asks at the next connect.
        return Ok(Some(false));
    }
    let assume_yes = yes_answers_sharing(args, announced);
    if announced && super::prompt::wants_yes(args) {
        println!(
            "  --yes does not answer this: LeanCTX Cloud asks about sharing on the approval page."
        );
    }
    if !assume_yes && !lean_ctx::core::host_env::stdin_is_terminal() {
        println!(
            "  Nobody could answer here (no terminal), so sharing with {} follows your `[cloud]` settings until you do.",
            instance.org_name
        );
        return Ok(None);
    }
    println!(
        "  Your agents share what they learn with your team in {}. From this machine that is:",
        instance.org_name
    );
    for item in &items {
        println!("  - {item}");
    }
    println!(
        "  Either way your agents get the workspace's knowledge and policies, and local tools never depend on LeanCTX Cloud."
    );
    let shared = super::prompt::confirm("Share this with the workspace?", assume_yes);
    if !shared {
        lean_ctx::core::config::Config::update_global(|c| steps::without_sharing(&mut c.cloud))
            .map_err(|e| CloudError::Local(format!("record the answer: {e}")))?;
        println!(
            "  Nothing is shared. Turn single channels on later with `lean-ctx config set cloud.<key> true`."
        );
    }
    steps::record(store, &instance.org_id, shared, UtcDateTime::now())?;
    Ok(Some(shared))
}

/// Step 3: name the vault of the project at `root`. Never fails the
/// connect: the background sync retries what did not work here.
fn map_project(config: &CloudConfig, session: &Session, root: &Path, device: bool) -> Outcome {
    // Dogfood 5.11.: a recorded vault the link file or the identity no
    // longer backs is dropped here and resolved again, never reused.
    match lean_ctx_cloud::resolution::forget_stale_at(session.store.data_dir(), root) {
        Ok(Some(stale)) => println!("  Note: {}; resolving it again.", stale.describe()),
        Ok(None) => {}
        Err(error) => return Outcome::Retry(error.to_string()),
    }
    let outcome = map_project_step(config, session, root, device);
    if let Outcome::Shared { ns, name } = &outcome {
        let ours = lean_ctx_protocol::cloud::vault::VaultRefV1 {
            ns: ns.clone(),
            name: name.clone(),
        };
        if let Some(other) = lean_ctx_cloud::mapping::bound_elsewhere(session, root, &ours) {
            println!(
                "  Warning: LeanCTX Cloud binds this repository to the vault {}/{} (another Instance of {} resolved it there), but this machine syncs it with {ns}/{name}. One repository in two vaults splits its knowledge: run `lean-ctx cloud link {}/{}` here, or move it in LeanCTX Cloud.",
                other.ns, other.name, session.instance.org_name, other.ns, other.name
            );
        }
    }
    outcome
}

fn map_project_step(config: &CloudConfig, session: &Session, root: &Path, device: bool) -> Outcome {
    let project = project_name(root);
    let org = &session.instance.org_name;
    match steps::project_step(&VaultMap::load(session.store.data_dir()), root) {
        ProjectStep::None => Outcome::NoProject,
        ProjectStep::Known(vault) => {
            println!("  This project syncs with the vault {vault}.");
            let (ns, name) = vault.split_once('/').unwrap_or(("", vault.as_str()));
            Outcome::Shared {
                ns: ns.to_string(),
                name: name.to_string(),
            }
        }
        ProjectStep::Resolve => {
            let resolve =
                || lean_ctx_cloud::knowledge::pull::resolve_project(session, config, root);
            let mut result = resolve();
            // C0 2.1: the approval page confirmed this share a moment ago.
            if device
                && let Err(MapError::Consent(ConsentError::Required { consent_id, .. })) = &result
            {
                match answer_on_device_page(config, session, consent_id) {
                    Ok(()) => result = resolve(),
                    Err(error) => println!("  cloud: {error}"),
                }
            }
            // 2.1.3: the first share waits for the person; at a terminal
            // they answer here, through the same channel as `cloud consent`.
            if let Err(MapError::Consent(ConsentError::Required { consent_id, .. })) = &result
                && lean_ctx::core::host_env::stdin_is_terminal()
            {
                println!(
                    "  {project} has not shared anything with {org} yet. Confirm it once: what your agents learn here then goes into this project's vault in {org} (the one it already has there, or a new one)."
                );
                let approve = ["approve".to_string(), consent_id.clone()];
                match super::cloud_consent_cmd::run(config, &approve) {
                    Ok(()) => result = resolve(),
                    Err(error) => println!("  cloud: {error}"),
                }
            }
            match result {
                Ok(record) => {
                    println!(
                        "  This project syncs with the vault {}/{}.",
                        record.ns, record.name
                    );
                    Outcome::Shared {
                        ns: record.ns,
                        name: record.name,
                    }
                }
                Err(MapError::Consent(ConsentError::Denied)) => {
                    Outcome::KeptLocal("you said no to sharing it".into())
                }
                Err(MapError::Consent(_)) => Outcome::Waiting,
                Err(MapError::Cloud(CloudError::Refused(why)))
                    if why.starts_with("vault_mismatch") =>
                {
                    Outcome::KeptLocal(why)
                }
                Err(error) => Outcome::Retry(format!(
                    "it has no vault yet ({error}); `lean-ctx cloud link <workspace>/<vault>` names one"
                )),
            }
        }
    }
}

/// The first share, answered on the device approval page (C0 2.1).
fn answer_on_device_page(
    config: &CloudConfig,
    session: &Session,
    consent_id: &str,
) -> Result<(), CloudError> {
    use lean_ctx_cloud::consent::broker::{Broker, Trust};
    use lean_ctx_cloud::consent::web::WebRun;
    let store = &session.store;
    let broker = Broker::open(store, Trust::load(store, config));
    let now = || u64::try_from(UtcDateTime::now().timestamp_millis()).unwrap_or_default();
    let run = WebRun {
        broker: &broker,
        data_dir: store.data_dir(),
        now: &now,
        sleep: &std::thread::sleep,
        open: &|_| {},
    };
    let record = session.with_client(|client| {
        Ok(run.run_device(client, consent_id, &mut || {
            println!("  One more question is waiting for you on the browser page.");
        }))
    })??;
    if record.state == lean_ctx_cloud::consent::State::Approved {
        Ok(())
    } else {
        Err(ConsentError::Denied.into())
    }
}

/// `cloud runtime install`: install through the signed runtime channel
/// without a question (owner decision 23.11.2026: consent comes from the
/// terms accepted at sign-up). Asked for by name, so a failure fails it.
pub(super) fn install_runtime() -> Result<(), CloudError> {
    let result = install_included_runtime()
        .map_err(|e| CloudError::Local(format!("runtime install: {e}")))?;
    println!("{result}");
    Ok(())
}

/// Open `url` in the default browser (best effort). `LEAN_CTX_NO_BROWSER`
/// (any value) keeps every browser closed, like `--no-browser`.
pub(super) fn open_browser(url: &str) -> std::io::Result<()> {
    if std::env::var_os("LEAN_CTX_NO_BROWSER").is_some() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(windows)]
    let mut command = {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let mut command = std::process::Command::new("xdg-open");
    command
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decided(shared: bool, source: Option<&str>) -> steps::SharingDecision {
        steps::SharingDecision {
            schema: steps::SHARING_SCHEMA.into(),
            org_id: "org_a".into(),
            shared,
            decided_at: "2026-10-26T09:00:00Z".into(),
            source: source.map(str::to_string),
        }
    }

    #[test]
    fn step_three_says_when_the_browser_answered() {
        assert_eq!(
            decided_line(&decided(true, Some("browser")), "Acme"),
            "answered in the browser: shared with Acme."
        );
        assert!(
            decided_line(&decided(false, Some("browser")), "Acme")
                .starts_with("answered in the browser: not shared")
        );
        assert_eq!(
            decided_line(&decided(true, None), "Acme"),
            "Sharing with Acme is on: you confirmed it."
        );
    }

    /// E2E 27.10. E2: "Changed your mind? Run connect again" must ask again.
    #[test]
    fn a_recorded_no_is_asked_again_in_a_terminal_only() {
        assert!(asks_again(&decided(false, Some("browser")), true));
        assert!(asks_again(&decided(false, None), true));
        assert!(!asks_again(&decided(false, None), false), "no terminal");
        assert!(!asks_again(&decided(true, None), true), "a yes stands");
    }

    const TOKEN: &str = "het_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const OTHER: &str = "het_fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    fn token_from(args: &[&str], stdin: &str, env: Option<&str>) -> Result<Option<String>, String> {
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        enrollment_token(
            &args,
            &mut std::io::Cursor::new(stdin.as_bytes()),
            env.map(std::ffi::OsString::from),
        )
        .map(|t| t.map(|t| format!("{t:?}")))
        .map_err(|e| e.to_string())
    }

    #[test]
    fn the_enrollment_token_comes_from_stdin_first_then_the_environment() {
        assert_eq!(token_from(&[], "", None), Ok(None), "no token: device flow");
        assert_eq!(
            token_from(&[], TOKEN, Some("")),
            Ok(None),
            "empty env is unset"
        );
        let some = Ok(Some("EnrollmentToken(het_…)".to_string()));
        assert_eq!(token_from(&[], "", Some(TOKEN)), some);
        assert_eq!(
            token_from(&["--enrollment-token-stdin"], &format!("{TOKEN}\n"), None),
            some
        );
        // The explicit flag wins over the ambient variable: a malformed
        // stdin fails even with a good token in the environment.
        let stdin_wins = token_from(&["--enrollment-token-stdin"], "nope\n", Some(OTHER));
        assert!(stdin_wins.is_err_and(|e| e.contains("malformed")));
        assert_eq!(
            token_from(
                &["--enrollment-token-stdin"],
                &format!("{TOKEN}\n"),
                Some("bad")
            ),
            some
        );
        // Only the first line counts.
        assert_eq!(
            token_from(
                &["--enrollment-token-stdin"],
                &format!("{TOKEN}\n{OTHER}\n"),
                None
            ),
            some
        );
        assert!(
            token_from(&["--enrollment-token-stdin"], "\n", Some(TOKEN))
                .is_err_and(|e| e.contains("stdin held no enrollment token"))
        );
    }

    #[test]
    fn the_enrollment_token_is_never_taken_from_argv_nor_echoed() {
        let inline = format!("--enrollment-token={TOKEN}");
        for args in [vec!["--enrollment-token", TOKEN], vec![inline.as_str()]] {
            let error = token_from(&args, "", None).expect_err("argv refused");
            assert!(
                error.contains("never passed on the command line"),
                "{error}"
            );
            assert!(!error.contains(&TOKEN[4..]), "{error}");
        }
        let bad = format!("{}x", &TOKEN[..TOKEN.len() - 1]);
        for error in [
            token_from(&[], "", Some(&bad)).unwrap_err(),
            token_from(&["--enrollment-token-stdin"], &bad, None).unwrap_err(),
        ] {
            assert!(error.contains("malformed"), "{error}");
            assert!(!error.contains(&bad[4..20]), "{error}");
        }
        let long = "a".repeat(4096);
        let error = token_from(&["--enrollment-token-stdin"], &long, None).unwrap_err();
        assert!(!error.contains(&long[..64]), "{error}");
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_string()).collect()
    }

    #[test]
    fn trial_flags_default_to_personal_and_plan_needs_trial() {
        assert_eq!(trial_choice(&strings(&[])), Ok(None));
        assert_eq!(
            trial_choice(&strings(&["--trial", "--yes"])),
            Ok(Some(TrialPlan::Personal))
        );
        assert_eq!(
            trial_choice(&strings(&["--trial", "--plan", "team"])),
            Ok(Some(TrialPlan::Team))
        );
        let alone = trial_choice(&strings(&["--plan", "team"])).unwrap_err();
        assert_eq!(alone.event_code(), "config");
        assert!(alone.to_string().contains("only valid with --trial"));
        let other = trial_choice(&strings(&["--trial", "--plan", "enterprise"])).unwrap_err();
        assert!(other.to_string().contains("choose personal or team"));
        assert!(trial_choice(&strings(&["--trial", "--plan"])).is_err());
    }

    fn lease(trial_ends_at: Option<&str>) -> CloudLeaseV1 {
        let envelope: Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../lean-ctx-protocol/tests/fixtures/cloud/lease_envelope.response.json"
            ))
            .unwrap(),
        )
        .unwrap();
        let mut lease = envelope["lease"].clone();
        if let Some(at) = trial_ends_at {
            lease["trial_ends_at"] = at.into();
        }
        serde_json::from_value(lease).unwrap()
    }

    /// The `--json` events are one compact object per line, exactly the
    /// documented members.
    #[test]
    fn trial_json_events_carry_the_documented_members() {
        let code = DeviceCodeV1 {
            schema: "leanctx.cloud.device-code/v1".into(),
            device_code: "secret-device-code".into(),
            user_code: "WDJB-MJHT".into(),
            verification_uri: "https://cloud.leanctx.com/device".into(),
            verification_uri_complete:
                "https://cloud.leanctx.com/device?user_code=WDJB-MJHT&intent=trial&plan=personal"
                    .into(),
            interval: 5,
            expires_in: 600,
            features: Vec::new(),
            intent: None,
            plan: None,
        };
        let event = code_event(&code);
        assert_eq!(
            event,
            json!({
                "event": "code",
                "verification_uri": "https://cloud.leanctx.com/device",
                "verification_uri_complete": "https://cloud.leanctx.com/device?user_code=WDJB-MJHT&intent=trial&plan=personal",
                "user_code": "WDJB-MJHT",
                "expires_in": 600,
            })
        );
        assert!(!event.to_string().contains("secret-device-code"));
        assert!(!event.to_string().contains('\n'));

        let trial = connected_event(&lease(Some("2026-12-02T10:00:00Z")), None);
        assert_eq!(trial["event"], "connected");
        assert_eq!(trial["trial_ends_at"], "2026-12-02T10:00:00Z");
        assert!(trial["plan"].is_string());
        let plain = connected_event(&lease(None), None);
        assert_eq!(plain["trial_ends_at"], Value::Null);
        assert_eq!(
            plain.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["event", "plan", "trial_ends_at"]
        );

        let used = CloudError::PlanRequired(Box::new(lean_ctx_cloud::error::PlanNeed {
            plan_needed: Some("personal".into()),
            reason: Some(lean_ctx_cloud::error::TRIAL_USED.into()),
            plans_url: Some("https://cloud.leanctx.com/plans".into()),
            ..Default::default()
        }));
        let event = error_event(&used);
        assert_eq!(event["event"], "error");
        assert_eq!(event["code"], "trial_used");
        assert!(event["message"].as_str().unwrap().contains("trial"));
        let expired = error_event(&CloudError::DeviceFlow(
            lean_ctx_cloud::error::CODE_EXPIRED.into(),
        ));
        assert_eq!(expired["code"], "expired");
        let denied = error_event(&CloudError::DeviceFlow(
            lean_ctx_cloud::error::CODE_DENIED.into(),
        ));
        assert_eq!(denied["code"], "denied");
        let old = error_event(&CloudError::TrialUnsupported {
            origin: "https://cloud.example".into(),
        });
        assert_eq!(old["code"], "trial_unsupported");
    }

    /// The top-level function `name` of this file, from its `fn` line to
    /// its closing brace at column 0. Line endings are normalised, as a
    /// Windows checkout may store this file with CRLF.
    fn source_of(name: &str) -> &'static str {
        static SOURCE: std::sync::LazyLock<String> =
            std::sync::LazyLock::new(|| include_str!("cloud_connect_cmd.rs").replace("\r\n", "\n"));
        let source: &'static str = &SOURCE;
        let tests = source
            .find("#[cfg(test)]\nmod tests")
            .expect("tests module");
        let start = [format!("\nfn {name}("), format!("\npub(super) fn {name}(")]
            .iter()
            .find_map(|head| source[..tests].find(head.as_str()))
            .unwrap_or_else(|| panic!("fn {name} not found"));
        let end = source[start..].find("\n}\n").expect("closing brace") + start;
        &source[start..end]
    }

    /// Text that reaches stdout outside `emit`: a print macro (not the
    /// stderr `eprint`s), a `write!` to a stream, or the stdout handle.
    fn stdout_writes(body: &str) -> Vec<&'static str> {
        let mut found = Vec::new();
        for needle in ["print!", "println!"] {
            for (at, _) in body.match_indices(needle) {
                if !body[..at].ends_with('e') {
                    found.push(needle);
                }
            }
        }
        for needle in [
            "io::stdout",
            "stdout()",
            "write!(",
            "writeln!(",
            "print_done(",
        ] {
            if body.contains(needle) {
                found.push(needle);
            }
        }
        found
    }

    /// Local Read §6a: with `--json` stdout carries only event lines and
    /// nothing comes before the first event; LeanCTX Code tells an older
    /// lean-ctx from a silent one by that. `emit` is the only stdout
    /// writer on the whole `--json` path of this file.
    #[test]
    fn json_connect_writes_stdout_only_through_the_event_writer() {
        let emit_body = source_of("emit");
        assert!(emit_body.contains("std::io::stdout()"), "{emit_body}");
        let run = source_of("run");
        let json_branch = &run[..run.find("let token = take_enrollment_token").unwrap()];
        assert!(json_branch.contains("run_json(config, args)"));
        assert!(
            stdout_writes(json_branch).is_empty(),
            "the --json branch of run prints: {json_branch}"
        );
        for name in [
            "run_json",
            "take_enrollment_token",
            "enrollment_token",
            "trial_choice",
            "origin_for",
            "default_label",
            "code_event",
            "connected_event",
            "error_event",
            "name_the_root",
            "trial_aware",
            "open_browser",
            "announce_and_pull",
        ] {
            let body = source_of(name);
            assert_eq!(
                stdout_writes(body),
                Vec::<&str>::new(),
                "fn {name} writes to stdout outside emit"
            );
        }
        // The first thing run_json writes is an event, and every write is one.
        let body = source_of("run_json");
        let emits: Vec<_> = body.match_indices("emit(").map(|(at, _)| at).collect();
        assert!(!emits.is_empty());
        for at in emits {
            let call = &body[at..];
            assert!(
                call.starts_with("emit(&code_event(") || call.starts_with("emit(&connected_event("),
                "{}",
                &call[..call.len().min(60)]
            );
        }
        assert!(
            body.find("emit(&code_event(").unwrap() < body.find("finish_device_flow(").unwrap()
        );
        // The terminal flow, by contrast, prints: the check sees it.
        assert!(stdout_writes(run).contains(&"println!"));
        assert!(stdout_writes("let _ = writeln!(std::io::stdout(), \"x\");").len() >= 2);
        assert!(stdout_writes("eprintln!(\"to stderr\");").is_empty());
    }

    /// Owner decisions 23.11.2026 for the onboarding (3.12.8): connect
    /// asks nothing about metrics or the runtime, a runtime that cannot be
    /// installed never fails it and says why in one distinct line, the
    /// output has no step counter, names this computer, links on a
    /// terminal, and `setup runtime install` works without flags.
    #[test]
    fn onboarding_connects_without_questions_and_says_it_plainly() {
        use lean_ctx::core::intelligence_runtime::{
            InstallError, USAGE, error_message, plain_install,
        };
        // Metrics: no question, one line naming the off switch.
        let connect = include_str!("cloud_connect_cmd.rs").replace("\r\n", "\n");
        let code = &connect[..connect.find("#[cfg(test)]\nmod tests").unwrap()];
        assert!(!code.contains("Share instance metrics with this workspace?"));
        let shared = metrics_line("Acme", Metrics::Shared).unwrap();
        assert!(shared.contains("`lean-ctx config set cloud.metrics_upload false`"));
        assert!(shared.contains("Settings"));
        // Runtime: no question; failures are plain, distinct and not fatal.
        assert!(!code.contains("RUNTIME_QUESTION"));
        let expired = InstallError::ChannelExpired.to_string();
        let mismatch = InstallError::ChannelMismatch.to_string();
        let manifest = InstallError::Manifest.to_string();
        assert!(expired.contains("expired") && mismatch.contains("channel"));
        assert!(manifest.contains("package manifest"));
        assert!(expired != mismatch && mismatch != manifest && expired != manifest);
        assert_eq!(
            runtime_line(&IncludedRuntime::Installed(Err(
                InstallError::PlatformUnavailable
            ))),
            "Runtime for this platform is not available yet."
        );
        assert!(
            runtime_line(&IncludedRuntime::Installed(Err(
                InstallError::ChannelExpired
            )))
            .contains("connected")
        );
        let event = connected_event(
            &lease(None),
            Some(runtime_member(&IncludedRuntime::Installed(Err(
                InstallError::ChannelExpired,
            )))),
        );
        assert_eq!(event["event"], "connected");
        assert_eq!(event["runtime"]["state"], "failed");
        assert_eq!(
            runtime_member(&IncludedRuntime::Installed(Err(
                InstallError::PlatformUnavailable
            )))["state"],
            "unavailable"
        );
        assert_eq!(
            runtime_member(&IncludedRuntime::Installed(Ok(String::new())))["state"],
            "installed"
        );
        for name in [
            "confirm_metrics",
            "settle_metrics",
            "included_runtime",
            "runtime_step",
            "runtime_deactivated",
            "install_included_runtime",
            "runtime_member",
            "label_from",
        ] {
            assert!(
                stdout_writes(source_of(name)).is_empty(),
                "fn {name} prints"
            );
        }
        // Log level: user-facing commands log errors only.
        assert_eq!(lean_ctx::core::logging::default_level(false), "error");
        assert_eq!(lean_ctx::core::logging::default_level(true), "warn");
        // No step counter.
        assert!(!code.contains(" of 3 ·"));
        // Label: computer name first, else the short host name; always sent.
        assert_eq!(
            label_from(Some("Sam's MacBook\n"), "mbp.local"),
            "Sam's MacBook"
        );
        assert_eq!(label_from(None, "mbp.fritz.box"), "mbp");
        assert_eq!(label_from(Some(" "), ""), "lean-ctx");
        assert!(!code.contains("start_device_flow_for(&client, chosen.as_deref()"));
        // No project: connected, and the link; nothing about git remotes.
        let done = done_lines(
            &Outcome::NoProject,
            "Acme",
            "https://cloud.leanctx.com/",
            "tmp",
            false,
        );
        assert_eq!(
            done,
            [
                "Done. This computer is connected to Acme.",
                "  Open:  https://cloud.leanctx.com/app/"
            ]
        );
        // Links: OSC 8 on a terminal, plain otherwise.
        assert_eq!(
            link("https://x.test/a", true),
            "\x1b]8;;https://x.test/a\x1b\\https://x.test/a\x1b]8;;\x1b\\"
        );
        assert_eq!(link("https://x.test/a", false), "https://x.test/a");
        assert_eq!(link("https://x.test/\x1b", true), "https://x.test/\x1b");
        // `setup runtime install` without flags is the signed channel; a
        // usage error shows the valid forms.
        let args = |a: &[&str]| a.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            plain_install(&args(&["install"])),
            Some([
                "sync-configured".to_string(),
                "--accept-proprietary".to_string()
            ])
        );
        assert_eq!(plain_install(&args(&["install", "--staging"])), None);
        assert!(matches!(
            lean_ctx::core::intelligence_runtime::run(&args(&["bogus"])),
            Err(InstallError::Usage)
        ));
        let usage = error_message(&InstallError::Usage);
        assert!(usage.contains("lean-ctx setup runtime install ") && usage.ends_with(USAGE));
    }

    /// MR !569 review (note 36077): connect honours a switched-off
    /// runtime, also one declined before the first install, and says so in
    /// one line and as the `runtime` member of `--json`.
    #[test]
    fn connect_keeps_a_switched_off_runtime_off() {
        use lean_ctx::core::config::IntelligenceRuntimeConfig;
        let mut declined = IntelligenceRuntimeConfig::default();
        declined.switch_off();
        assert_eq!(
            runtime_step(true, false, declined.deactivated()),
            Some(RuntimeStep::KeepOff)
        );
        let after_deactivate = IntelligenceRuntimeConfig {
            accept_proprietary: true,
            ..IntelligenceRuntimeConfig::default()
        };
        assert_eq!(
            runtime_step(true, false, after_deactivate.deactivated()),
            Some(RuntimeStep::KeepOff)
        );
        assert_eq!(runtime_step(true, false, false), Some(RuntimeStep::Install));
        assert_eq!(runtime_step(false, false, false), None);
        assert_eq!(runtime_step(true, true, true), None);
        let line = runtime_line(&IncludedRuntime::KeptOff);
        assert_eq!(line.lines().count(), 1);
        assert!(line.contains("lean-ctx setup runtime install"), "{line}");
        let event = connected_event(
            &lease(None),
            Some(runtime_member(&IncludedRuntime::KeptOff)),
        );
        assert_eq!(event["runtime"]["state"], "switched_off");
        assert_eq!(event["runtime"]["message"], line);
    }

    /// MR !569 review: connect confirms only the unset plan default. An
    /// off from Settings, an admin, the lease or policy stays off, and the
    /// "shared" line is printed only when usage metrics are shared.
    #[test]
    fn connect_metrics_keep_an_off_from_cloud_and_say_shared_only_when_shared() {
        use lean_ctx_cloud::metrics_v2::gate::{OffReason, Sharing};
        use lean_ctx_protocol::cloud::metrics_v2::PolicyLevel;
        use std::cell::Cell;
        use std::collections::BTreeSet;

        let on = || Sharing::On {
            vaults_off: BTreeSet::new(),
            unattributed: false,
            policy_version: 1,
            since: None,
            level: None,
        };
        let off = |reason| Sharing::Off {
            reason,
            level: Some(PolicyLevel::Instance),
        };
        // Runs the decision against a fake workspace; returns the result
        // and whether connect confirmed.
        let run_with =
            |allowed: bool, may_confirm: bool, fetched: bool, before: Sharing, after: Sharing| {
                let confirmed = Cell::new(false);
                let result = settle_metrics(
                    allowed,
                    may_confirm,
                    || {
                        if fetched {
                            Ok(())
                        } else {
                            Err(CloudError::Refused("unreachable".into()))
                        }
                    },
                    || {
                        if confirmed.get() {
                            after.clone()
                        } else {
                            before.clone()
                        }
                    },
                    || {
                        confirmed.set(true);
                        Ok(())
                    },
                );
                (result, confirmed.get())
            };
        let run = |allowed: bool, fetched: bool, before: Sharing, after: Sharing| {
            run_with(allowed, allowed, fetched, before, after)
        };

        // Off in Settings (or by an admin, the lease, the workspace):
        // never confirmed, no "shared" line.
        for reason in [
            OffReason::Policy,
            OffReason::Workspace,
            OffReason::Lease,
            OffReason::PolicyUnavailable,
            OffReason::Revoked,
        ] {
            let (result, confirmed) = run(true, true, off(reason), on());
            assert!(!confirmed, "{reason:?} was switched back on");
            assert!(matches!(result, Some(Ok(Metrics::Off))), "{reason:?}");
        }
        // The unset default: confirmed, then shared.
        let (result, confirmed) = run(true, true, off(OffReason::NotConfirmed), on());
        assert!(confirmed);
        assert!(matches!(result, Some(Ok(Metrics::Shared))));
        // Confirmed but the workspace does not share yet (a fresh
        // registration without policy): a line still says they are on and
        // how to switch them off, but not that they are shared.
        let (result, confirmed) = run(
            true,
            true,
            off(OffReason::NotConfirmed),
            off(OffReason::PolicyUnavailable),
        );
        assert!(confirmed);
        assert!(matches!(result, Some(Ok(Metrics::SwitchedOn))));
        let pending = metrics_line("Acme", Metrics::SwitchedOn).unwrap();
        assert!(pending.contains("switched on") && !pending.contains("are shared with"));
        assert!(pending.contains("cloud.metrics_upload false"));
        // Already on: shared, nothing to confirm.
        let (result, confirmed) = run(true, true, on(), on());
        assert!(!confirmed);
        assert!(matches!(result, Some(Ok(Metrics::Shared))));
        // `cloud.metrics_upload = false` (or not released): `None`.
        let (result, confirmed) = run(false, true, off(OffReason::NotConfirmed), on());
        assert!(!confirmed);
        assert!(result.is_none());
        // The workspace could not be asked: nothing is confirmed.
        let (result, confirmed) = run(true, false, off(OffReason::NotConfirmed), on());
        assert!(!confirmed);
        assert!(matches!(result, Some(Err(_))));
        // ... and a cached "on" is only the last known state.
        let (result, confirmed) = run(true, false, on(), on());
        assert!(!confirmed);
        assert!(matches!(result, Some(Ok(Metrics::LastKnownShared))));
        assert!(
            metrics_line("Acme", Metrics::LastKnownShared)
                .unwrap()
                .contains("last known")
        );
        assert_eq!(metrics_line("Acme", Metrics::Off), None);
        // DO_NOT_TRACK=1 and LEAN_CTX_TELEMETRY=off keep connect from
        // confirming, like `cloud.metrics_upload = false`.
        assert!(metrics_allowed(true, true, None, None));
        assert!(!metrics_allowed(true, true, Some("1"), None));
        assert!(!metrics_allowed(true, true, None, Some("off")));
        assert!(!metrics_allowed(true, false, None, None));
        assert!(!metrics_allowed(false, true, None, None));
        // MR !569 review (note 36077): under DO_NOT_TRACK=1 a workspace that
        // already shares keeps sharing (the upload gate is its state and
        // `cloud.metrics_upload`), so the line says so; nothing new is
        // switched on.
        let (result, confirmed) = run_with(true, false, true, on(), on());
        assert!(!confirmed);
        assert!(matches!(result, Some(Ok(Metrics::Shared))), "{result:?}");
        let (result, confirmed) = run_with(true, false, true, off(OffReason::NotConfirmed), on());
        assert!(!confirmed);
        assert!(matches!(result, Some(Ok(Metrics::Off))), "{result:?}");

        // The line: plain words, no banned product words.
        let line = metrics_line("Acme", Metrics::Shared).unwrap();
        assert!(line.starts_with("Usage metrics are shared with Acme"));
        assert!(line.contains("pseudonymous counts"));
        assert!(line.contains("the names of unregulated programs"));
        for banned in ["Instance", "instance", "engine", "Engine"] {
            assert!(!line.contains(banned), "{banned}");
        }
    }

    /// A `--trial` code that ran out points to the plans page (an older
    /// Cloud refuses a used trial only in the browser); a sign-in keeps
    /// "run connect again". Both stay event code `expired`.
    #[test]
    fn trial_expiry_names_the_plans_page_not_a_new_code() {
        let expired = || CloudError::DeviceFlow(lean_ctx_cloud::error::CODE_EXPIRED.into());
        let trial = error_event(&trial_aware(expired(), Some(TrialPlan::Personal)));
        assert_eq!(trial["code"], "expired");
        let message = trial["message"].as_str().unwrap();
        assert!(
            message.contains("choose a plan in LeanCTX Cloud"),
            "{message}"
        );
        assert!(
            message.contains("lean-ctx cloud connect --trial"),
            "{message}"
        );
        let plain = error_event(&trial_aware(expired(), None));
        assert_eq!(plain["code"], "expired");
        assert_eq!(plain["message"], expired().to_string());
        let denied = CloudError::DeviceFlow(lean_ctx_cloud::error::CODE_DENIED.into());
        assert_eq!(trial_aware(denied.clone(), Some(TrialPlan::Team)), denied);
    }

    #[test]
    fn yes_answers_sharing_only_where_the_terminal_asks() {
        let yes = vec!["--yes".to_string()];
        assert!(
            yes_answers_sharing(&yes, false),
            "older LeanCTX Cloud: as before"
        );
        assert!(
            !yes_answers_sharing(&yes, true),
            "C0 2.1 LeanCTX Cloud: the page asks, --yes never answers"
        );
        assert!(!yes_answers_sharing(&[], false));
    }
}
