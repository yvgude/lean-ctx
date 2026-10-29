// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hive connect`: the single entry to Hive (CHECKLIST 2.1.1).
//!
//! 1. Browser login through the device flow. The person picks the
//!    workspace on the approval page. Skipped when this machine is
//!    already connected.
//! 2. One confirmation of what this machine shares with that workspace,
//!    asked once per workspace (`core::hive::connect`).
//! 3. The current project is mapped to its vault. A first share into a
//!    vault waits for the person's yes (`core::hive::mapping`), asked
//!    here at a terminal.
//!
//! Running it again is safe and asks nothing that was answered. Later
//! sessions never prompt.

use std::io::Write as _;
use std::path::Path;

use chrono::Utc;

use super::hive_cmd::{flag_value, plan_name};
use crate::core::hive::auth::{
    Session, SetupAnnouncement, announce_setup, finish_device_flow, start_device_flow,
};
use crate::core::hive::connect::{self as steps, ProjectStep};
use crate::core::hive::consent::ConsentError;
use crate::core::hive::mapping::MapError;
use crate::core::hive::state::InstanceRecord;
use crate::core::hive::status::report;
use crate::core::hive::transport::HiveClient;
use crate::core::hive::vault::VaultMap;
use crate::core::hive::{HiveConfig, HiveError, HiveStore};
use lean_ctx_protocol::hive::enroll::MAX_INSTANCE_LABEL_CHARS;

fn default_label() -> String {
    let host = gethostname::gethostname()
        .to_string_lossy()
        .trim()
        .to_string();
    let host: String = host
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_INSTANCE_LABEL_CHARS)
        .collect();
    if host.trim().is_empty() {
        "lean-ctx".into()
    } else {
        host
    }
}

/// A key set another root signed (Hive task lean-ctx #2): say which root
/// this engine checked against and how to use the right one; what follows
/// the refusal (the removed key, task #1) stays.
fn name_the_root(
    error: HiveError,
    trust: &crate::core::hive::root_cert::ConnectTrust,
    origin: &str,
) -> HiveError {
    use crate::core::hive::lease::ROOT_MISMATCH;
    let HiveError::Protocol(message) = error else {
        return error;
    };
    let Some(rest) = message.strip_prefix(ROOT_MISMATCH) else {
        return HiveError::Protocol(message);
    };
    let which = if trust.unvouched {
        format!(
            "the development root key from `[hive] root_key` in your lean-ctx config. For {origin}, remove `root_key` from `[hive]` so lean-ctx uses the production root it carries, or set it to the root key of that Hive"
        )
    } else if trust.certificate.is_some() {
        format!(
            "the root the certificate of {origin} names. Ask the operator of that Hive to renew its root certificate"
        )
    } else {
        format!(
            "the production root key built into lean-ctx {}. Update lean-ctx; for a self-hosted Hive, set `[hive] root_key` to its root key",
            env!("CARGO_PKG_VERSION")
        )
    };
    HiveError::Protocol(format!(
        "{origin} answered with a key set not signed by {which}{rest}"
    ))
}

/// `lean-ctx hive connect [--url <origin>] [--label <name>] [--no-browser] [--yes]`.
///
/// Prints the three steps the web shows too (approve this machine,
/// connected, share this project) and ends with "Done." and where to look.
pub(super) fn run(config: &HiveConfig, args: &[String]) -> Result<(), HiveError> {
    let store = HiveStore::open()?;
    let origin = match flag_value(args, "--url")? {
        Some(url) => crate::core::hive::config::normalize_origin(url)?,
        None => config.origin()?,
    };
    if let Some(existing) = store.instance()? {
        let current = report(&store, config, Utc::now());
        if current.connected() {
            println!(
                "Already connected to {} as {} ({}). Run `lean-ctx hive disconnect` first to switch.",
                existing.url, existing.label, existing.org_name
            );
            let session = crate::core::hive::root_cert::trust_root(&store, config, &existing.url)
                .and_then(|root| Session::load(store, root));
            return match session {
                Ok(session) => {
                    let outcome = after_login(config, &session, &existing, args)?;
                    print_done(&existing, &outcome);
                    Ok(())
                }
                // `status` explains a broken session; connect stays a no-op.
                Err(error) => {
                    tracing::info!("hive connect: {error}");
                    Ok(())
                }
            };
        }
        // A lapsed or broken enrollment is replaced; its keys are reused.
    }
    let chosen: Option<String> = flag_value(args, "--label")?
        .map(|label| label.chars().take(MAX_INSTANCE_LABEL_CHARS).collect());
    let label = chosen.clone().unwrap_or_else(default_label);
    let client = HiveClient::new(&origin)?;
    println!("LeanCTX Hive — connect this machine");
    // A self-hosted Hive is trusted when LeanCTX vouched for its root.
    let trust = crate::core::hive::root_cert::for_connect(&client, config, &origin)
        .map_err(|error| unreachable(error, &origin))?;
    if trust.unvouched {
        println!("(Development Hive: its root key comes from `[hive] root_key` in your config.)");
    }
    let root = trust.root;
    let code = start_device_flow(&client, chosen.as_deref())
        .map_err(|error| unreachable(error, &origin))?;
    // C0 2.1: the approval page also asks "Share this project" (one code).
    let here = project_here();
    let announced = announce_setup(&client, &code, &setup_announcement(config, here.as_deref()));
    println!("\nStep 1 of 3 · Approve this machine in your browser");
    if args.iter().any(|a| a == "--no-browser") || std::env::var_os("LEAN_CTX_NO_BROWSER").is_some()
    {
        println!(
            "  Open this link on any device: {}",
            code.verification_uri_complete
        );
    } else {
        let _ = open_browser(&code.verification_uri_complete);
        println!("  We opened: {}", code.verification_uri_complete);
    }
    println!(
        "  Check the code  {}  in the browser: it must be the same (valid {} minutes).",
        code.user_code,
        code.expires_in.div_ceil(60).max(1)
    );
    if announced {
        println!("  There you sign in, choose the workspace and confirm what this machine shares.");
    } else {
        println!("  There you sign in and choose the workspace for this machine.");
    }
    print!("  Waiting for you to approve…");
    let _ = std::io::stdout().flush();
    let (instance, sharing) =
        finish_device_flow(&store, &client, &code, &label, &root, &std::thread::sleep)
            .map_err(|error| name_the_root(error, &trust, &origin))
            .map_err(|error| {
                println!();
                // A cooling-down Personal slot: `hive status` keeps saying until when
                // and where the web recovery is (P7).
                if matches!(&error, HiveError::QuotaExceeded(q) if q.cooling_slot()) {
                    println!("Engines:      {origin} (Settings › Engines)");
                }
                crate::core::hive::throttle::note(
                    &store,
                    crate::core::hive::throttle::Scope::Connect,
                    error,
                    Utc::now(),
                )
            })?;
    println!(" approved.");
    if let Some(certificate) = &trust.certificate {
        crate::core::hive::root_cert::store_certificate(&store, certificate)?;
    }
    // An admin allowed this new enrolment: an earlier revocation is over.
    crate::core::hive::revoked::clear(&store)?;
    crate::core::hive::throttle::clear_connect(&store);
    let session = Session::load(store, root)?;
    // Prove the instance key works before reporting success.
    session.client()?;
    println!(
        "\nStep 2 of 3 · Connected {} to {} ({} plan).",
        instance.label,
        instance.workspace_label(),
        plan_name(&session.lease.lease.plan)
    );
    // E44 at connect (SPEC-ENGINE §3.6). A Hive before C0 2.0 has no
    // instance card; that never fails the connect.
    if let Err(error) = crate::core::hive::workers::register::announce_instance(&session) {
        tracing::info!("hive instance card: {error}");
    }
    // Unified Control (2.x): the effective policy is in force and acked from
    // the connect on; the scheduler checks it again every interval. A 1.x
    // Hive keeps its scheduled pull only. Never fails the connect.
    if crate::core::hive::bus::relay_v2::speaks_v2(&session.store)
        && session
            .lease
            .lease
            .grants(lean_ctx_protocol::hive::lease::HiveRight::PolicyPull)
        && let Err(error) = crate::core::hive::policy::pull(&session)
    {
        tracing::info!("hive policy at connect: {error}");
    }
    // The page answered "Share this project": recorded like the terminal's
    // answer, so no session asks again; a no switches every upload off.
    // E2E 27.10. E3: outside a project the page offers no share choice, so
    // whatever it sent is no answer and nothing is recorded or switched off.
    let sharing = sharing.filter(|_| project_here().is_some());
    let mut device = false;
    if let Some(Some(share)) = sharing {
        if !share {
            crate::core::config::Config::update_global(|c| steps::without_sharing(&mut c.hive))
                .map_err(|e| HiveError::Local(format!("record the answer: {e}")))?;
        }
        steps::record_from(
            &session.store,
            &instance.org_id,
            share,
            Utc::now(),
            Some(steps::SOURCE_BROWSER),
        )?;
        device = share;
    }
    let config = if sharing == Some(Some(false)) {
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
    if session
        .lease
        .lease
        .grants(lean_ctx_protocol::hive::lease::HiveRight::Runtime)
        && !config.runtime_consent
    {
        // Asked only at a terminal; nothing is installed without a yes.
        if std::io::IsTerminal::is_terminal(&std::io::stdin())
            && super::prompt::confirm(RUNTIME_QUESTION, false)
        {
            return install_runtime(true);
        }
        println!(
            "  Optional: `lean-ctx hive runtime install` adds the LeanCTX Intelligence runtime your plan includes; it enforces {}'s policies on this machine.",
            instance.org_name
        );
    }
    Ok(())
}

/// An unreachable Hive before anything was sent, in plain words.
fn unreachable(error: HiveError, origin: &str) -> HiveError {
    match error {
        HiveError::Offline(_) => HiveError::Refused(format!(
            "can't reach Hive at {origin}. Check your connection and try again; LeanCTX keeps working locally meanwhile."
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

/// The engine-metrics notice (telemetry SPEC §4, K4): once at connect, in
/// this terminal only, once engine metrics are released. On Personal nothing is shared until the person says yes here (or later with
/// `hive metrics confirm`); `--yes` does not answer it.
fn engine_metrics_notice(config: &HiveConfig, session: &Session, instance: &InstanceRecord) {
    use crate::core::hive::metrics_v2::{self, gate, upload};
    if !upload::released(&instance.url) || !config.metrics_upload {
        return;
    }
    let workspace = instance.workspace_label();
    let web_url = instance
        .web_url
        .clone()
        .unwrap_or_else(|| instance.url.clone());
    let lease = &session.lease.lease;
    let sharing = gate::decide(&session.store, config, instance, lease, Utc::now());
    // Before the release nothing is shared, so nothing is asked either.
    let needs =
        gate::needs_confirmation(lease.plan) && !sharing.is_on() && upload::released(&instance.url);
    println!(
        "\n{}",
        metrics_v2::disclosure(&workspace, &web_url, &sharing, needs)
    );
    let terminal = std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stdout());
    if needs
        && terminal
        && super::prompt::confirm("Share engine metrics with this workspace?", false)
    {
        match upload::confirm(session) {
            Ok(_) => println!("Engine metrics confirmed for {workspace}."),
            Err(error) => eprintln!("hive: engine metrics not confirmed: {error}"),
        }
    }
}

/// Steps 2 and 3 for a connected machine.
fn after_login(
    config: &HiveConfig,
    session: &Session,
    instance: &InstanceRecord,
    args: &[String],
) -> Result<Outcome, HiveError> {
    after_login_with(config, session, instance, args, false, false)
}

/// The checkout here, when LeanCTX can identify it (git origin, id file or
/// manifest): what connect shares and the page names.
fn project_here() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let root = crate::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    crate::core::hive::vault::project_identity(Path::new(&root))?;
    Some(root)
}

/// What the approval page shows (C0 2.1): the folder name (as the first
/// share's consent names it) and the remote, plus the upload channels.
fn setup_announcement(config: &HiveConfig, root: Option<&str>) -> SetupAnnouncement {
    let project = root.map(|root| {
        let root = Path::new(root);
        let name: String = project_name(root)
            .chars()
            .filter(|c| !c.is_control())
            .take(64)
            .collect();
        let remote = crate::core::hive::identity_v2::origin_remote(root)
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
    use lean_ctx_protocol::hive::enroll::{DEVICE_SETUP_RESULT_SCHEMA, DeviceSetupResultV1};
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
        .map_err(|e| HiveError::Local(e.to_string()))
        .and_then(|body| session.with_client(|client| client.post("/device/setup/result", &body)));
    if let Err(error) = sent {
        tracing::info!("hive device setup result: {error}");
    }
}

/// Steps 2 and 3; `device`: the approval page just confirmed sharing this
/// project, so its first share is answered there (C0 2.1), not here;
/// `announced`: this Hive asks about sharing on the page, so `--yes`
/// does not answer it.
fn after_login_with(
    config: &HiveConfig,
    session: &Session,
    instance: &InstanceRecord,
    args: &[String],
    device: bool,
    announced: bool,
) -> Result<Outcome, HiveError> {
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
    let cwd = std::env::current_dir().map_err(|e| HiveError::Local(e.to_string()))?;
    let root = crate::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    // A yes given again in this run turned the channels back on.
    let config = crate::core::config::Config::load().hive.clone();
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
    let web = web.trim_end_matches('/');
    let cwd = std::env::current_dir().unwrap_or_default();
    let root = crate::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let project = project_name(Path::new(&root));
    let org = &instance.org_name;
    println!();
    match outcome {
        Outcome::Shared { ns, name } => {
            println!(
                "Done. Your agents now share what they learn about {project} with your team in {org}."
            );
            println!("  Open:  {web}/o/{ns}/v/{name}/");
            println!("  Next:  Keep working as usual; LeanCTX syncs in the background.");
            println!("         Another project? Run `lean-ctx hive connect` there.");
        }
        Outcome::NoProject => {
            println!(
                "Done. This machine is connected to {org}. This folder is not a project LeanCTX can share (it has no git remote)."
            );
            println!("  Open:  {web}/app/");
            println!("  Next:  Run `lean-ctx hive connect` inside a project to share it.");
        }
        Outcome::KeptLocal(why) => {
            println!("Done. {project} stays on this machine: {why}.");
            println!("  Your agents still use {org}'s knowledge and policies.");
            println!("  Open:  {web}/app/");
            println!(
                "  Changed your mind? Run `lean-ctx hive connect` here again in your own terminal: it asks again."
            );
        }
        Outcome::Waiting => {
            println!(
                "Almost done. {project} stays on this machine until you confirm it yourself: run `lean-ctx hive connect` here, in your own terminal (agents cannot confirm it)."
            );
            println!("  Open:  {web}/app/");
        }
        Outcome::Retry(why) => {
            println!(
                "Connected, but {project} is not set up yet: {why}. LeanCTX tries again in the background; nothing leaves this machine before that."
            );
            println!("  Open:  {web}/app/");
        }
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
/// a Hive that took the setup (C0 2.1) asks on the approval page.
fn yes_answers_sharing(args: &[String], announced: bool) -> bool {
    !announced && super::prompt::wants_yes(args)
}

/// The sharing answer for the instance's workspace: the recorded one, or
/// asked now. `None` when no person could answer (no terminal, no
/// `--yes`); the `[hive]` settings then apply unchanged, as before.
fn confirm_sharing(
    config: &HiveConfig,
    store: &HiveStore,
    instance: &InstanceRecord,
    args: &[String],
    announced: bool,
) -> Result<Option<bool>, HiveError> {
    println!("\nStep 3 of 3 · Share this project");
    if let Some(decided) = steps::decision(store, &instance.org_id) {
        println!("  {}", decided_line(&decided, &instance.org_name));
        let terminal = std::io::IsTerminal::is_terminal(&std::io::stdin());
        if !asks_again(&decided, terminal) {
            return Ok(Some(decided.shared));
        }
        // E2E 27.10. E2: a recorded "no" is asked again here, in a terminal.
        if !super::prompt::confirm("Share it with the workspace now?", false) {
            return Ok(Some(false));
        }
        crate::core::config::Config::update_global(|c| steps::with_sharing(&mut c.hive))
            .map_err(|e| HiveError::Local(format!("record the answer: {e}")))?;
        steps::record(store, &instance.org_id, true, Utc::now())?;
        return Ok(Some(true));
    }
    let items = steps::shared_items(config);
    if items.is_empty() {
        println!(
            "  This machine shares nothing with {}: every upload is switched off in your `[hive]` settings.",
            instance.org_name
        );
        // Not recorded: turning a channel on later asks at the next connect.
        return Ok(Some(false));
    }
    let assume_yes = yes_answers_sharing(args, announced);
    if announced && super::prompt::wants_yes(args) {
        println!("  --yes does not answer this: Hive asks about sharing on the approval page.");
    }
    if !assume_yes && !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        println!(
            "  Nobody could answer here (no terminal), so sharing with {} follows your `[hive]` settings until you do.",
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
        "  Either way your agents get the workspace's knowledge and policies, and local tools never depend on Hive."
    );
    let shared = super::prompt::confirm("Share this with the workspace?", assume_yes);
    if !shared {
        crate::core::config::Config::update_global(|c| steps::without_sharing(&mut c.hive))
            .map_err(|e| HiveError::Local(format!("record the answer: {e}")))?;
        println!(
            "  Nothing is shared. Turn single channels on later with `lean-ctx config set hive.<key> true`."
        );
    }
    steps::record(store, &instance.org_id, shared, Utc::now())?;
    Ok(Some(shared))
}

/// Step 3: name the vault of the project at `root`. Never fails the
/// connect: the background sync retries what did not work here.
fn map_project(config: &HiveConfig, session: &Session, root: &Path, device: bool) -> Outcome {
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
                || crate::core::hive::knowledge::pull::resolve_project(session, config, root);
            let mut result = resolve();
            // C0 2.1: the approval page confirmed this share a moment ago.
            if device
                && let Err(MapError::Consent(ConsentError::Required { consent_id, .. })) = &result
            {
                match answer_on_device_page(config, session, consent_id) {
                    Ok(()) => result = resolve(),
                    Err(error) => println!("  hive: {error}"),
                }
            }
            // 2.1.3: the first share waits for the person; at a terminal
            // they answer here, through the same channel as `hive consent`.
            if let Err(MapError::Consent(ConsentError::Required { consent_id, .. })) = &result
                && std::io::IsTerminal::is_terminal(&std::io::stdin())
            {
                println!(
                    "  {project} has not shared anything with {org} yet. Confirm it once: what your agents learn here then goes into this project's vault in {org} (the one it already has there, or a new one)."
                );
                let approve = ["approve".to_string(), consent_id.clone()];
                match super::hive_consent_cmd::run(config, &approve) {
                    Ok(()) => result = resolve(),
                    Err(error) => println!("  hive: {error}"),
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
                Err(error) => Outcome::Retry(format!(
                    "it has no vault yet ({error}); `lean-ctx hive link <workspace>/<vault>` names one"
                )),
            }
        }
    }
}

/// The first share, answered on the device approval page (C0 2.1).
fn answer_on_device_page(
    config: &HiveConfig,
    session: &Session,
    consent_id: &str,
) -> Result<(), HiveError> {
    use crate::core::hive::consent::broker::{Broker, Trust};
    use crate::core::hive::consent::web::WebRun;
    let store = &session.store;
    let broker = Broker::open(store, Trust::load(store, config));
    let now = || u64::try_from(Utc::now().timestamp_millis()).unwrap_or_default();
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
    if record.state == crate::core::hive::consent::State::Approved {
        Ok(())
    } else {
        Err(ConsentError::Denied.into())
    }
}

const RUNTIME_QUESTION: &str = "Your plan includes the LeanCTX Intelligence runtime (proprietary, from the signed LeanCTX channel); it enforces your workspace's policy on this machine. Install it now?";

/// Record the consent, then install through the signed runtime channel.
pub(super) fn install_runtime(consented: bool) -> Result<(), HiveError> {
    if !super::prompt::confirm(RUNTIME_QUESTION, consented) {
        return Err(HiveError::Config(
            "the runtime was not installed: no consent".into(),
        ));
    }
    crate::core::config::Config::update_global(|config| config.hive.runtime_consent = true)
        .map_err(|e| HiveError::Local(format!("record the consent: {e}")))?;
    let result = crate::core::intelligence_runtime::run(&[
        "sync-configured".to_string(),
        "--accept-proprietary".to_string(),
    ])
    .map_err(|e| HiveError::Local(format!("runtime install: {e}")))?;
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

    #[test]
    fn yes_answers_sharing_only_where_the_terminal_asks() {
        let yes = vec!["--yes".to_string()];
        assert!(yes_answers_sharing(&yes, false), "older Hive: as before");
        assert!(
            !yes_answers_sharing(&yes, true),
            "C0 2.1 Hive: the page asks, --yes never answers"
        );
        assert!(!yes_answers_sharing(&[], false));
    }
}
