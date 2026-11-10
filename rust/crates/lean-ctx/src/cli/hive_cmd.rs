// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hive connect|status|disconnect|link|worker`.

use chrono::Utc;

use lean_ctx_hive::auth::Session;
use lean_ctx_hive::status::{HiveStatusReport, report};
use lean_ctx_hive::{HiveConfig, HiveConfigExt as _, HiveError, HiveStore, link};
use lean_ctx_protocol::hive::lease::LeaseState;

const HELP: &str = "\
lean-ctx hive — share what your agents learn with your team in LeanCTX | Hive

START HERE:
  lean-ctx hive connect      Connect this machine and this project: sign in in the
                              browser, choose the workspace, approve. Run it again
                              in any other project. [--label <name>] [--no-browser]
                              [--url <origin>]
  lean-ctx hive status       Is this machine connected, and is this project shared?
  lean-ctx hive disconnect   Remove this machine from the workspace and free its
                              slot (--keep-local keeps the local copies).

EVERYDAY (LeanCTX does these in the background; run them to do it now):
  lean-ctx hive sync         Send what your agents learned and get the team's.
  lean-ctx hive consent      Answer a question lean-ctx asked you (agents cannot):
                              list | approve <id> | deny <id> | --web <id>.
  lean-ctx hive link <workspace>/<vault>
                              Use a specific vault for this project instead of the
                              one connect chose.

MORE:
  lean-ctx hive help advanced  Review, maintenance, worker mode, tasks, the
                                agent bus and the Intelligence runtime.

Plans: Personal 3 instances, 2 connected at once; Team 5 per seat,
3 connected at once per seat; Enterprise on request.
Local tools never depend on Hive: offline or a lapsed lease only pause sync.
";

/// `lean-ctx hive help advanced`, and `--help` of the commands it lists.
const ADVANCED_HELP: &str = "\
lean-ctx hive — advanced commands

USAGE:
  lean-ctx hive metrics [preview [--json] | hours --json --range 24h|7d|30d | confirm | delete]
  lean-ctx hive capture [--recapture]
  lean-ctx hive replica
  lean-ctx hive maintain <workspace>/<vault> [--stale] [--contradictions] [--dedupe] [--limit N]
  lean-ctx hive maintain <workspace>/<vault> --stale --confirm <node_id>
  lean-ctx hive runtime install [--yes]
  lean-ctx hive bus relay [--for <seconds>]
  lean-ctx hive worker [status|pause|resume|stop|list|rename <wkr> <name>|register --cli <host> [--role <role>]|retire <wkr> [--force]|retire --idle <dur>]
  lean-ctx hive graph publish
  lean-ctx hive tasks [list|show|claim|update|submit|release|propose] …
  lean-ctx hive status [--json [--contract local-read/1]]
  lean-ctx hive sync status --json
  lean-ctx hive policy sessions --json | session --session <id> --json
  lean-ctx hive presence --json
  lean-ctx hive docs list|show <id> --json [--project-root <p>]
  lean-ctx hive tasks list|show <n> --json
  lean-ctx hive vaults --json [--project-root <p>]
  lean-ctx hive link --vault <workspace>/<vault> --json [--project-root <p>] [--dry-run]

metrics     Instance metrics for the workspace (hourly counts, including full
            re-fetches of shortened output, the names of known programs and
            host families no enforcing rule covers, MCP servers only when the
            workspace policy names them; never content, paths, prompts, model output,
            secrets or command lines).
            preview prints the notice and exactly what would be sent next
            for the last hour (--json: the unsigned batch; nothing is sent);
            confirm (Personal, in a terminal only) shares them once
            you agree; delete removes what this instance buffered for it.
capture     Run the capture gate for this repository now: only single,
            referenced claims leave (one sentence of 24-300 characters with a
            file:, sym:, test:, commit: or https:// reference); everything
            else stays local and is counted by reason. --recapture removes this
            repository's unsent session drafts and captures all local
            knowledge again.
replica     The local replica of this repository's vault (C0 2.0 Hive only):
            place, cursor, live nodes and the graph digest two instances that
            folded the same log agree on. Read only.
maintain    Keep a vault healthy from this checkout (C0 2.0 Hive, writer+):
            --stale re-checks every anchored claim against the code here and
            marks changed ones to_verify, missing ones gone (no model, no
            cost); --confirm <node_id> re-anchors a claim you checked at the
            current code. --contradictions and --dedupe run in the vault's
            review loop. Prints counts, never claim texts.
runtime    install: with your consent, install the proprietary LeanCTX
            Intelligence runtime your plan licenses (signed channel); it
            enforces your workspace's policy. --yes consents without asking.
bus relay   Join this machine's agent bus to the workspace's (the daemon
            runs the same relay in the background; --for limits the run).
graph      publish: send this repository's code graph (files, symbols, imports;
            never code text) to Hive, when the vault owner turned on the
            full graph on the web.
worker      Worker mode: your agents pull tasks you opted in for on the web,
            within its daily budget ([hive.worker] can only lower it).
            status shows the opt-in and today's use; pause/resume stop and
            restart claiming on this machine; stop switches worker mode off
            for this instance and ends its claims (the owner turns it on again
            on the web). list shows this instance's durable workers (wkr_ ids);
            rename gives one a name of 1-64 letters, digits, spaces and
            . _ · - from one alphabet. You use your own subscription; follow your provider's
            terms (OpenCode or Pi with a Claude Pro/Max login and third-party
            tools with a Gemini CLI login are not allowed by the providers).
tasks       The vault's tasks: read them, work one under a claim (worker
            mode), report, submit a result with evidence, or propose one
            (`lean-ctx hive tasks --help`).
status      --json prints the cockpit shape; --json --contract local-read/1
            adds the Local Read v1 fields (plan, lease, rights, capture).
presence    --json: the workers of other machines on this machine's vault
            buses, as the relay last recorded them.
--json      on presence, docs list|show, tasks list|show, vaults and link
            (and status with --contract local-read/1): one JSON object with
            \"contract\": \"local-read/1\" (Local Read v1,
            docs/contracts/local-read-v1.md), for local readers such as
            Hive Desktop. Read only (docs come from the local replica), except
            link, which writes the link file and shares the project at once
            under the usual consent rules (--dry-run writes nothing).
";

// Declared here, not in cli/mod.rs (frozen for the crate split).
#[path = "hive_docs_cmd.rs"]
mod hive_docs_cmd;

/// `lean-ctx hive docs import …` for the checkout of the current directory.
fn docs(rest: &[String]) -> Result<(), HiveError> {
    let cwd = std::env::current_dir().map_err(|e| HiveError::Local(e.to_string()))?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    for line in hive_docs_cmd::run(rest, &root)? {
        println!("{line}");
    }
    Ok(())
}

pub(crate) fn cmd_hive(args: &[String]) {
    let sub = args.first().map(String::as_str).unwrap_or("status");
    let rest = args.get(1..).unwrap_or(&[]);
    if sub == "tasks"
        && (rest.first().map(String::as_str) == Some("help")
            || rest.iter().any(|a| a == "--help" || a == "-h"))
    {
        print!("{}", super::hive_tasks_cmd::HELP);
        return;
    }
    if sub == "consent"
        && (rest.first().map(String::as_str) == Some("help")
            || rest.iter().any(|a| a == "--help" || a == "-h"))
    {
        print!("{}", super::hive_consent_cmd::HELP);
        return;
    }
    if sub == "docs"
        && (rest.first().map(String::as_str) == Some("help")
            || rest.iter().any(|a| a == "--help" || a == "-h"))
    {
        println!(
            "lean-ctx hive docs: this checkout's docs as pages of its Hive vault\n\nusage:\n  {}",
            hive_docs_cmd::USAGE
        );
        return;
    }
    if matches!(sub, "help" | "--help" | "-h") || rest.iter().any(|a| a == "--help" || a == "-h") {
        print!("{}", help_for(sub, rest));
        return;
    }
    let config = lean_ctx::core::config::Config::load().hive.clone();
    if !connector_open(&config, sub, rest) {
        eprintln!("{}", lean_ctx_hive::NOT_AVAILABLE);
        std::process::exit(1);
    }
    // Local Read v1: the reads a local reader (Hive Desktop) makes, after
    // the same release gate as every other hive command.
    if super::local_read::wants_json(rest) {
        let reads = matches!(
            super::local_read::positionals(rest).first().copied(),
            None | Some("list" | "show")
        );
        let fields = matches!(
            super::local_read::positionals(rest).first().copied(),
            Some("priority" | "labels")
        );
        if sub == "tasks" && fields {
            return super::local_read::tasks::run(rest);
        }
        match sub {
            "docs" if reads => return super::local_read::docs::run(rest),
            "tasks" if reads => return super::local_read::tasks::run(rest),
            "presence" => return super::local_read::presence::run(rest),
            "sync" if reads_status(rest) => return super::local_read::sync::run_sync_status(rest),
            "policy" => return super::local_read::sync::run_policy(rest),
            "vaults" => return super::local_read::vaults::run(rest),
            "link" => return super::local_read::link::run(rest),
            _ => {}
        }
    }
    // `hive sync status` and `hive policy …` read local files only, as JSON;
    // a plain `hive sync status` never falls through to a manual sync.
    if (sub == "sync" && reads_status(rest)) || sub == "policy" {
        eprintln!("hive {sub}: only --json is supported");
        std::process::exit(2);
    }
    let result = match sub {
        "connect" => super::hive_connect_cmd::run(&config, rest),
        "status" => status(&config, rest),
        "disconnect" => disconnect(&config, rest),
        "sync" => sync(&config),
        "metrics" => metrics(&config, rest),
        "capture" => capture(&config, rest),
        "replica" => replica_status(),
        "maintain" => maintain(&config, rest),
        "bus" => bus(&config, rest),
        "link" => link_project(rest),
        "runtime" if rest.first().map(String::as_str) == Some("install") => {
            super::hive_connect_cmd::install_runtime(super::prompt::wants_yes(rest))
        }
        "worker" => worker(&config, rest),
        "graph" if rest.first().map(String::as_str) == Some("publish") => publish_graph(&config),
        "tasks" => super::hive_tasks_cmd::run(rest),
        "docs" => docs(rest),
        "presence" => {
            eprintln!("hive presence: only --json is supported (`ctx_agent list` shows the rows)");
            std::process::exit(2);
        }
        "vaults" => {
            eprintln!(
                "hive vaults: only --json is supported (`lean-ctx hive status` names the workspace)"
            );
            std::process::exit(2);
        }
        "consent" => super::hive_consent_cmd::run(&config, rest),
        other => {
            eprintln!("hive: unknown subcommand '{other}'\n");
            eprint!("{HELP}");
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        eprintln!("hive: {error}");
        std::process::exit(1);
    }
}

/// `hive sync status`: the background sync's state, not a sync.
fn reads_status(rest: &[String]) -> bool {
    super::local_read::positionals(rest).first().copied() == Some("status")
}

/// The release switch ([`lean_ctx_hive::HIVE_AVAILABLE`]) for the two
/// commands that start a Hive relationship, decided before any store or
/// network access. Everything else keeps its "not connected" answers.
fn connector_open(config: &HiveConfig, sub: &str, rest: &[String]) -> bool {
    if lean_ctx_hive::HIVE_AVAILABLE {
        return true;
    }
    match sub {
        "connect" => {
            let origin = match flag_value(rest, "--url") {
                Ok(Some(url)) => lean_ctx_hive::config::normalize_origin(url).ok(),
                Ok(None) => config.origin().ok(),
                Err(_) => None,
            };
            origin.is_some_and(|origin| lean_ctx_hive::available_for(&origin))
        }
        "link" => lean_ctx_hive::available_locally(),
        _ => true,
    }
}

/// The help a `--help` (or `help <topic>`) asks for: the advanced commands
/// with theirs, everything else the short start-here help.
fn help_for(sub: &str, rest: &[String]) -> &'static str {
    let topic = if sub == "help" {
        rest.first().map_or("", String::as_str)
    } else {
        sub
    };
    match topic {
        "advanced" | "all" | "capture" | "replica" | "maintain" | "runtime" | "bus" | "worker"
        | "graph" | "tasks" => ADVANCED_HELP,
        _ => HELP,
    }
}

pub(super) fn flag_value<'a>(args: &'a [String], name: &str) -> Result<Option<&'a str>, HiveError> {
    match args.iter().position(|a| a == name) {
        None => Ok(None),
        Some(i) => args
            .get(i + 1)
            .map(|v| Some(v.as_str()))
            .ok_or_else(|| HiveError::Config(format!("{name} needs a value"))),
    }
}

/// The plan as people read it (not its wire name: a workspace without a
/// paid plan is `none` on the wire and "No plan" to people).
pub(super) fn plan_name(plan: &lean_ctx_protocol::hive::lease::HivePlan) -> String {
    plan.product_name().to_string()
}

fn status(config: &HiveConfig, args: &[String]) -> Result<(), HiveError> {
    let store = HiveStore::open()?;
    let current = report(&store, config, Utc::now());
    if args.iter().any(|a| a == "--json") {
        // Local Read v1 only on explicit opt-in: plain `--json` stays the
        // cockpit shape, which its readers decode strictly.
        if super::local_read::status::opted_in(args) {
            super::local_read::status::print(&current, store.data_dir(), config.auto_capture, args);
            return Ok(());
        }
        let mut status = current.cli_status();
        status.vault_mismatch = stale_here(&store).map(|stale| stale.wire());
        let json = serde_json::to_string(&status).map_err(|e| HiveError::Local(e.to_string()))?;
        println!("{json}");
        return Ok(());
    }
    print_report(&current);
    if lean_ctx_hive::disconnect::pending(&store).is_some() {
        println!(
            "Disconnect:   pending (Hive was not reachable); retried in the background, or run `lean-ctx hive disconnect` again"
        );
    }
    // Release B: quota and policy throttles in force (what waits, until when).
    for line in lean_ctx_hive::throttle::status_lines(&store, Utc::now()) {
        println!("{line}");
    }
    // §3.16: the upgrade duties (local; once) and their counts, printed once.
    if current.instance.is_some() {
        let root = std::env::current_dir().ok().map(|cwd| {
            std::path::PathBuf::from(lean_ctx::core::protocol::detect_project_root_or_cwd(
                &cwd.to_string_lossy(),
            ))
        });
        for line in lean_ctx_hive::effective_policy::status_lines(
            store.data_dir(),
            &current.policy,
            Utc::now(),
            root.as_deref(),
        ) {
            println!("{line}");
        }
        if let Err(error) = lean_ctx_hive::migrate::run(&store, config) {
            println!("Upgrade:      {error}");
        }
        for line in lean_ctx_hive::migrate::status_lines_once(&store) {
            println!("{line}");
        }
    }
    Ok(())
}

/// The stale vault resolution of the project here, if any (local files).
fn stale_here(store: &HiveStore) -> Option<lean_ctx_hive::resolution::Stale> {
    let cwd = std::env::current_dir().ok()?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let map = lean_ctx_hive::vault::VaultMap::load(store.data_dir());
    lean_ctx_hive::resolution::stale_for_root(&map, std::path::Path::new(&root))
}

/// Whether the project here is shared, in one line: its vault, a first
/// share that waits for the person, or not set up yet. `None` outside a
/// project LeanCTX can identify.
fn project_line() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let root = std::path::Path::new(&root);
    lean_ctx_hive::vault::project_identity(root)?;
    let name = root.file_name()?.to_string_lossy().into_owned();
    let store = HiveStore::open().ok()?;
    let map = lean_ctx_hive::vault::VaultMap::load(store.data_dir());
    // Dogfood 5.11.: a resolution the link file or identity no longer
    // backs is never shown as "shared" (connect and sync drop it).
    if let Some(stale) = lean_ctx_hive::resolution::stale_for_root(&map, root) {
        return Some(format!(
            "{name} → MISMATCH: {}. That vault is not used; run `lean-ctx hive connect` here to resolve the project again",
            stale.describe()
        ));
    }
    if let Some(record) = map.for_root(&root.to_string_lossy()) {
        return Some(format!("{name} → {}/{} (shared)", record.ns, record.name));
    }
    let config = lean_ctx::core::config::Config::load().hive.clone();
    let broker = lean_ctx_hive::consent::Broker::open(
        &store,
        lean_ctx_hive::consent::Trust::load(&store, &config),
    );
    let now = u64::try_from(Utc::now().timestamp_millis()).unwrap_or_default();
    let waiting = broker.list().ok()?.into_iter().any(|l| {
        l.valid
            && l.record.kind == lean_ctx_hive::consent::Kind::LinkVault
            && l.record.state == lean_ctx_hive::consent::State::Pending
            && l.record.expires_at > now
            && l.record.subject["bound_repo"].as_str() == Some(name.as_str())
            && !lean_ctx_hive::mapping::satisfied(&l.record, &map)
    });
    Some(if waiting {
        format!(
            "{name} → not shared yet: waiting for your yes (run `lean-ctx hive connect` here, in your own terminal)"
        )
    } else {
        format!("{name} → not shared yet (run `lean-ctx hive connect` here)")
    })
}

fn print_report(current: &HiveStatusReport) {
    let Some(instance) = &current.instance else {
        if lean_ctx_hive::HIVE_AVAILABLE {
            println!(
                "Hive: not connected ({}). Run `lean-ctx hive connect`.",
                current.url
            );
        } else {
            println!("Hive: not connected. {}", lean_ctx_hive::NOT_AVAILABLE);
        }
        return;
    };
    println!("Hive:        {}", current.url);
    if current.unvouched_root {
        println!("Root:         development (`[hive] root_key` in your config)");
    }
    println!("Workspace:    {}", instance.workspace_label());
    println!(
        "Instance:     {} ({})",
        instance.label, instance.instance_id
    );
    if let Some(line) = project_line() {
        println!("Project:      {line}");
    }
    let cap = lean_ctx_hive::workers::attach::WORKER_CAP;
    if current.workers >= cap || current.workers_retiring > 0 {
        let retiring = match current.workers_retiring {
            0 => String::new(),
            n => format!("; {n} given up, waiting to be retired on Hive"),
        };
        println!(
            "Workers:      {} of {cap} (worker_cap_reached: a new role takes the slot of the longest-unused worker without a session{retiring})",
            current.workers
        );
    }
    if let Some(problem) = &current.problem {
        println!("Problem:      {problem}");
    }
    if let (Some(lease), Some(state)) = (&current.lease, current.lease_state) {
        let state = match state {
            LeaseState::Valid => format!("valid until {}", lease.expires_at.as_str()),
            LeaseState::Grace => format!("in grace until {}", lease.grace_until.as_str()),
            LeaseState::Lapsed => "lapsed: run `lean-ctx hive connect`".to_string(),
        };
        match current.activity.locked.as_deref() {
            Some(reason) => println!(
                "Plan:         {} · locked ({})",
                plan_name(&lease.plan),
                if reason == "trial_ended" {
                    "trial ended"
                } else {
                    "plan ended"
                }
            ),
            None => println!(
                "Plan:         {} · {} {} connected at once",
                plan_name(&lease.plan),
                lease.seats_limit,
                if lease.seats_limit == 1 {
                    "instance"
                } else {
                    "instances"
                }
            ),
        }
        println!("Lease:        {state}");
        if let Some(reason) = &current.activity.renew_error {
            println!("              last renewal failed: {reason} (tried again every pass)");
        }
        let rights: Vec<String> = lease
            .rights
            .iter()
            .filter_map(|r| serde_json::to_value(r).ok()?.as_str().map(str::to_string))
            .filter(|r| r != "unknown")
            .collect();
        println!("Rights:       {}", rights.join(", "));
    }
    if !current.enabled {
        println!("Sync:         paused ([hive] enabled = false)");
    }
    match &current.activity.last_seen {
        Some(seen) => println!("Last contact: {seen}"),
        None => println!("Last contact: never"),
    }
    for (channel, when) in &current.activity.last_success {
        println!("  {channel:<10}  {when}");
    }
    if let Some(error) = &current.activity.last_error {
        println!("Last error:   {error}");
    }
    let policy = &current.policy;
    if !policy.filter_packs.is_empty() {
        let packs: Vec<String> = policy
            .filter_packs
            .iter()
            .map(|(id, v)| format!("{id} v{v}"))
            .collect();
        println!("Filter packs: {}", packs.join(", "));
    }
    if let Some(refusal) = &policy.last_refusal {
        println!(
            "Policy refused: {refusal} (not compliant with the newest bundle; the last verified one stays)"
        );
    }
    if current.lease.is_some() {
        if let (Some(v2), Some(instance)) = (&current.metrics_v2, &current.instance) {
            println!("Instance metrics: {}", v2.line(&instance.workspace_label()));
        }
        match current.receipts_off {
            None => println!("Receipts:     sent ({} queued)", current.receipts_queued),
            Some(reason) => println!("Receipts:     not sent: {reason}"),
        }
    }
    use lean_ctx_protocol::hive::status::{HiveBusControlActionV1, HiveBusControlsInactiveV1};
    for control in &current.bus_controls {
        let action = match control.action {
            HiveBusControlActionV1::Hold => "hold",
            HiveBusControlActionV1::Stop => "stop",
            HiveBusControlActionV1::Veto => "veto",
            HiveBusControlActionV1::Unknown => "control",
        };
        println!(
            "Bus control:  {action} {} by {} since {}: {}",
            control.target_agent.as_deref().unwrap_or("(every agent)"),
            control.issued_by,
            control.since.as_str(),
            control.reason
        );
    }
    match current.bus_controls_inactive {
        Some(HiveBusControlsInactiveV1::LeaseLapsed) => {
            println!("Bus controls: inactive (lease lapsed)");
        }
        Some(HiveBusControlsInactiveV1::NotEnrolled) => {
            println!("Bus controls: inactive (not enrolled)");
        }
        Some(HiveBusControlsInactiveV1::Unknown) => println!("Bus controls: inactive"),
        None => {}
    }
    println!(
        "Drafts:       {} queued, {} refused",
        current.drafts_pending.len(),
        current.drafts_failed.len()
    );
    for (id, summary, error) in &current.drafts_pending {
        match error {
            Some(error) => println!("  queued   {id}  {summary}  (last try: {error})"),
            None => println!("  queued   {id}  {summary}"),
        }
    }
    for (id, summary, error) in &current.drafts_failed {
        println!(
            "  refused  {id}  {summary}  ({})",
            error.as_deref().unwrap_or("unknown")
        );
    }
    if let Ok(store) = HiveStore::open() {
        for (place, counts) in lean_ctx_hive::capture::load_counts(store.data_dir()) {
            println!("Capture gate: {place}: {}", counts.summary());
        }
    }
}

fn disconnect(config: &HiveConfig, args: &[String]) -> Result<(), HiveError> {
    let keep_local = args.iter().any(|a| a == "--keep-local");
    let store = HiveStore::open()?;
    let Some(instance) = store.instance()? else {
        // A connect that failed after enrolment may have left its key
        // (Hive task #1); Hive refuses that key a second time.
        if lean_ctx_hive::instance_key::InstanceKeys::load(&store).is_ok() {
            store.forget(keep_local)?;
            println!(
                "Not connected to Hive. Removed the instance key an unfinished connect left behind."
            );
        } else {
            println!("Not connected to Hive.");
        }
        return Ok(());
    };
    // Hive first; the local data goes only once Hive ended the instance.
    use lean_ctx_hive::disconnect::{self, Told};
    match disconnect::tell_hive(&store, config, &instance) {
        Told::Revoked => println!(
            "Disconnected {} from {}: Hive revoked this instance and the seat is free.",
            instance.label, instance.org_name
        ),
        Told::AlreadyGone => println!(
            "Disconnected {} from {}: Hive had already ended this instance.",
            instance.label, instance.org_name
        ),
        Told::Unreachable(error) => {
            disconnect::mark_pending(&store, keep_local)?;
            println!(
                "Hive is not reachable ({error}). Nothing was removed: the disconnect is pending, \
                 lean-ctx retries it in the background (or run `lean-ctx hive disconnect` again)."
            );
            return Ok(());
        }
        Told::Refused(error) if args.iter().any(|a| a == "--force") => println!(
            "Removed the local credentials, but Hive did not end the instance ({error}). Remove the instance \"{}\" in Hive to free its seat.",
            instance.label
        ),
        Told::Refused(error) => {
            return Err(HiveError::Refused(format!(
                "Hive did not end this instance ({error}); nothing was removed. Remove the instance \"{}\" in Hive, or run `lean-ctx hive disconnect --force` to remove the local credentials anyway.",
                instance.label
            )));
        }
    }
    store.forget(keep_local)?;
    if keep_local {
        println!("The local Hive data (drafts, pulled knowledge, policy) stays on this machine.");
    } else {
        println!("Removed the local Hive data; knowledge already in project knowledge stays.");
    }
    Ok(())
}

/// `hive maintain` in host mode (SPEC-ENGINE §3.15b, V9.4).
fn maintain(config: &HiveConfig, args: &[String]) -> Result<(), HiveError> {
    use lean_ctx_hive::review::maintain;
    use lean_ctx_hive::sync::writer;
    let req = maintain::parse(args)?;
    let store = HiveStore::open()?;
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let (ns, name) = req
        .vault
        .split_once('/')
        .ok_or_else(|| HiveError::Config("name the vault as <workspace>/<vault>".into()))?;
    let cwd = std::env::current_dir().map_err(|e| HiveError::Local(e.to_string()))?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let map = lean_ctx_hive::vault::VaultMap::load(store.data_dir());
    let record = lean_ctx_hive::resolution::vault_in(&map, std::path::Path::new(&root))?
        .filter(|r| r.ns == ns && r.name == name)
        .cloned()
        .ok_or_else(|| {
            HiveError::Config(format!(
                "run it in a checkout linked to {ns}/{name} (`lean-ctx hive link {ns}/{name}`)"
            ))
        })?;
    let dir = lean_ctx_hive::replica::store::place_dir(store.data_dir(), &record.vault_id);
    if !dir.join("replica.db").exists() {
        println!("No replica for {ns}/{name} yet: run `lean-ctx hive sync` first.");
        return Ok(());
    }
    let trust = lean_ctx_hive::root_cert::trust_root(&store, config, &instance.url)?;
    let session = Session::load(store.clone(), trust)?;
    let (mk, _) = lean_ctx_hive::replica::crypt::master_key(store.config_dir(), false)?;
    let (replica, _) =
        lean_ctx_hive::replica::Replica::open(store.data_dir(), &record.vault_id, &mk)?;
    // The signer, place and place ref as every vault writer builds them
    // (C0 2.0 spellings: hlc.node and place id as hyphenated UUIDs).
    let writer = writer::VaultWriter::open(&session, maintain::MAINTENANCE_ROLE)?;
    let place = writer::vault_place(&record, &replica)?;
    let signer = writer.signer();
    let target = maintain::Target {
        replica: &replica,
        place: &place,
        signer: &signer,
    };
    let api = lean_ctx_hive::sync::place::SessionApi(&session);
    let place_ref = writer::vault_ref(&record);
    let mut checkout = lean_ctx_hive::capture::refs::Checkout::new(std::path::Path::new(&root));
    let (_, lines) = maintain::run_host(
        &target,
        &mut checkout,
        &req,
        Some((&api, &place_ref)),
        Utc::now().timestamp_millis().unsigned_abs(),
    )?;
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

fn replica_status() -> Result<(), HiveError> {
    let store = HiveStore::open()?;
    let cwd = std::env::current_dir().map_err(|e| HiveError::Local(e.to_string()))?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let identity = lean_ctx_hive::vault::project_identity(std::path::Path::new(&root))
        .ok_or_else(|| HiveError::Config("not inside a git repository".into()))?;
    let stable = lean_ctx_protocol::hive::vault::stable_identity(&identity);
    let record = lean_ctx_hive::vault::VaultMap::load(store.data_dir())
        .vaults
        .get(&stable)
        .cloned()
        .ok_or_else(|| HiveError::Config("this repository is not linked to a vault".into()))?;
    let dir = lean_ctx_hive::replica::store::place_dir(store.data_dir(), &record.vault_id);
    if !dir.join("replica.db").exists() {
        println!(
            "No replica for @{}/{} yet (`lean-ctx hive sync` clones it on a C0 2.0 Hive).",
            record.ns, record.name
        );
        return Ok(());
    }
    let (mk, at_rest) = lean_ctx_hive::replica::crypt::master_key(store.config_dir(), false)?;
    let (replica, _) =
        lean_ctx_hive::replica::Replica::open(store.data_dir(), &record.vault_id, &mk)?;
    let mut graph = lean_ctx_hive::replica::fold::Graph::default();
    let events = replica.fold_events()?;
    graph
        .fold(&events)
        .map_err(|e| HiveError::Protocol(e.to_string()))?;
    let unverified: Vec<&str> = events
        .iter()
        .filter(|e| e.sig_invalid)
        .map(|e| e.event_id.as_str())
        .collect();
    println!(
        "Vault:   @{}/{} (vault:{})",
        record.ns, record.name, record.vault_id
    );
    println!(
        "Cursor:  {}",
        replica
            .cursor()?
            .map_or_else(|| "none".to_string(), |c| c.to_string())
    );
    println!(
        "Nodes:   {} live of {}, {} edges",
        graph.live_nodes().count(),
        graph.nodes.len(),
        graph.edges.len()
    );
    println!("Pending: {} own event(s)", replica.pending()?.len());
    if !unverified.is_empty() {
        println!(
            "Unverified: {} event(s) stored but not folded (signature): {}",
            unverified.len(),
            unverified.join(", ")
        );
    }
    println!(
        "Epoch:   {}",
        replica
            .epoch()?
            .map_or_else(|| "none".to_string(), |e| e.to_string())
    );
    let lost = replica.key_lost_ids()?.len();
    if lost > 0 {
        println!("Lost:    {lost} event(s): Content lost in a server failover");
    }
    println!("Digest:  {}", graph.digest());
    println!("At rest: {}", at_rest.as_str());
    Ok(())
}

fn capture(config: &HiveConfig, args: &[String]) -> Result<(), HiveError> {
    use lean_ctx_hive::knowledge::draft;
    let store = HiveStore::open()?;
    store.instance()?.ok_or(HiveError::NotConnected)?;
    let cwd = std::env::current_dir().map_err(|e| HiveError::Local(e.to_string()))?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let root = std::path::Path::new(&root);
    let mut screen = lean_ctx_hive::knowledge::screening::Screen::for_installation();
    if lean_ctx_hive::vault::project_identity(root)
        .map(|i| lean_ctx_protocol::hive::vault::stable_identity(&i))
        .is_some_and(|stable| draft::syncs_through_replica(&store, &stable, root))
    {
        println!(
            "This vault syncs through its replica: `lean-ctx hive sync` captures its knowledge as signed events; no draft is queued."
        );
    }
    if args.iter().any(|a| a == "--recapture") {
        let done = draft::recapture(&store, config, root, &mut screen, Utc::now())?;
        println!(
            "Removed {} unsent session draft(s); queued {} draft(s).",
            done.removed,
            done.queued.len()
        );
        println!("Capture gate: {}", done.counts.summary());
    } else {
        let id = draft::enqueue_session_draft(
            &store,
            config,
            root,
            "cli-capture",
            draft::Trigger::SessionEnd,
            &mut screen,
            Utc::now(),
        )?;
        println!(
            "{}",
            if id.is_some() {
                "Queued a draft; `lean-ctx hive sync` sends it."
            } else {
                "Nothing new passed the capture gate."
            }
        );
    }
    for (place, counts) in lean_ctx_hive::capture::load_counts(store.data_dir()) {
        println!("{place}: {}", counts.summary());
    }
    Ok(())
}

/// `lean-ctx hive metrics`: engine metrics v2. Never sends metrics.
fn metrics(config: &HiveConfig, args: &[String]) -> Result<(), HiveError> {
    use lean_ctx_hive::metrics_v2::{self, gate, upload};
    let store = HiveStore::open()?;
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let workspace = instance.workspace_label();
    let web_url = instance
        .web_url
        .clone()
        .unwrap_or_else(|| instance.url.clone());
    let wants = |word: &str| {
        args.iter()
            .any(|a| a == word || a.strip_prefix("--") == Some(word))
    };
    let json_out = args.iter().any(|a| a == "--json");
    let print_json = |value: &dyn erased::Json| -> Result<(), HiveError> {
        println!("{}", value.pretty()?);
        Ok(())
    };
    let root = lean_ctx_hive::root_cert::trust_root(&store, config, &instance.url)?;
    let session = Session::load(store.clone(), root)?;
    let lease = &session.lease.lease;
    if wants("delete") {
        upload::delete_local(&store);
        let remote = match upload::delete_remote(&session) {
            Ok(true) => " Hive deleted every hour it stored for this instance.".to_string(),
            Ok(false) => String::new(),
            Err(error) => {
                format!(" Hive could not delete its stored hours now ({error}); run it again.")
            }
        };
        println!(
            "Deleted what this instance buffered for {workspace} (hours not sent yet, the \
             fingerprint key).{remote}"
        );
        return Ok(());
    }
    if wants("confirm") {
        // Before the release there is nothing to confirm: say so, ask nothing.
        if !upload::released(&instance.url) {
            println!(
                "Instance metrics are not released yet: nothing is shared with {workspace}, so there is nothing to confirm."
            );
            return Ok(());
        }
        // A person, at a terminal: never an agent or a script (`--yes` does
        // not count here, owner decision).
        if !lean_ctx::core::host_env::stdin_is_terminal()
            || !std::io::IsTerminal::is_terminal(&std::io::stdout())
        {
            return Err(HiveError::Refused(
                "instance metrics are confirmed by a person in a terminal: run `lean-ctx hive metrics confirm` there".into(),
            ));
        }
        if !gate::needs_confirmation(lease.plan) {
            println!(
                "{workspace} shares instance metrics by default on its plan; its admins control it with the policy rule \"Share instance metrics\"."
            );
            return Ok(());
        }
        let sharing = gate::decide(&store, config, &instance, lease, Utc::now());
        println!(
            "{}",
            metrics_v2::disclosure(&workspace, &web_url, &sharing, true)
        );
        if super::prompt::confirm("Share instance metrics with this workspace?", false) {
            upload::confirm(&session)?;
            let now = gate::decide(&store, config, &instance, lease, Utc::now());
            println!("Instance metrics for {workspace}: {}.", now.describe());
        } else {
            println!("Nothing changed: instance metrics stay off for {workspace}.");
        }
        return Ok(());
    }
    if wants("hours") {
        let range = flag_value(args, "--range")?.unwrap_or("24h");
        let hours = match range {
            "24h" => 24,
            "7d" => 168,
            "30d" => 720,
            other => {
                return Err(HiveError::Config(format!(
                    "hive metrics hours: --range is 24h, 7d or 30d, not {other}"
                )));
            }
        };
        let body = upload::hours(&store, config, &instance, lease, hours, Utc::now())?;
        return print_json(&body);
    }
    if wants("preview") || args.is_empty() || args.iter().all(|a| a == "--json") {
        let (sharing, hour, request) =
            upload::preview(&store, config, &instance, lease, Utc::now())?;
        if json_out {
            return print_json(&request);
        }
        println!(
            "{}",
            metrics_v2::disclosure(
                &workspace,
                &web_url,
                &sharing,
                gate::needs_confirmation(lease.plan)
            )
        );
        println!("\nInstance metrics for {workspace}: {}", sharing.describe());
        if !upload::released(&instance.url) {
            println!("Not released yet: this instance sends nothing; below is what it would send.");
        }
        let shown = if request.buckets.is_empty() {
            upload::sample_hour(&store, config, &instance, lease, Utc::now())?
        } else {
            request
        };
        if shown.buckets.is_empty() {
            println!("Nothing to send for {hour}: no activity in a shared vault.");
        } else {
            println!("Batch for {hour} (POST /v1/metrics/hours, before signing):");
            print_json(&shown)?;
        }
        return Ok(());
    }
    Err(HiveError::Config(
        "hive metrics: use preview [--json], hours --json --range 24h|7d|30d, confirm or delete"
            .into(),
    ))
}

mod erased {
    use lean_ctx_hive::HiveError;

    /// Anything printed as pretty JSON.
    pub(super) trait Json {
        fn pretty(&self) -> Result<String, HiveError>;
    }

    impl<T: serde::Serialize> Json for T {
        fn pretty(&self) -> Result<String, HiveError> {
            serde_json::to_string_pretty(self).map_err(|e| HiveError::Local(e.to_string()))
        }
    }
}

/// The refusals of one `hive sync`: a locked workspace is said once (E9),
/// and remembered for `hive status`.
#[derive(Default)]
struct SyncSay {
    locked: Option<String>,
}

impl SyncSay {
    /// Print `line` on stderr, unless it repeats the locked refusal; then
    /// only that one line, once.
    fn refusal(&mut self, line: &str) {
        use lean_ctx_hive::error::LOCKED_MARK;
        let Some(at) = line.find(LOCKED_MARK) else {
            eprintln!("{line}");
            return;
        };
        if self.locked.is_some() {
            return;
        }
        // The sentence starts at "Your …" (`locked_line`).
        let start = line[..at].rfind("Your ").unwrap_or(0);
        let sentence = &line[start..];
        eprintln!("hive: {sentence}");
        self.locked = Some(
            if sentence.contains("trial has ended") {
                "trial_ended"
            } else {
                "plan_ended"
            }
            .to_string(),
        );
    }
}

fn sync(config: &HiveConfig) -> Result<(), HiveError> {
    let mut say = SyncSay::default();
    let store = HiveStore::open()?;
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let root = lean_ctx_hive::root_cert::trust_root(&store, config, &instance.url)?;
    let session = Session::load(store, root)?;
    let cwd = std::env::current_dir().map_err(|e| HiveError::Local(e.to_string()))?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let mut screen = lean_ctx_hive::knowledge::screening::Screen::for_installation();
    // A C0 2.0 Hive: the bound vault syncs through its replica (signed
    // events), and no 1.x draft is queued for it. A 1.x Hive keeps the
    // draft path below unchanged.
    let bound = lean_ctx_hive::vault::project_identity(std::path::Path::new(&root))
        .map(|i| lean_ctx_protocol::hive::vault::stable_identity(&i))
        .and_then(|stable| {
            lean_ctx_hive::vault::VaultMap::load(session.store.data_dir())
                .vaults
                .get(&stable)
                .cloned()
        });
    let v2 = lean_ctx_hive::bus::relay_v2::speaks_v2(&session.store);
    // The first sync of a project (after `hive link`, or the first share
    // that maps it) has no record yet: resolve it now, so this sync already
    // clones the replica and doc writes find it.
    let bound = match bound {
        None if v2 => lean_ctx_hive::knowledge::pull::resolve_project(
            &session,
            config,
            std::path::Path::new(&root),
        )
        .ok(),
        other => other,
    };
    match (&bound, v2) {
        (Some(record), true) => {
            // The daemon's background sync of this vault finishes first.
            let _lock = lean_ctx_hive::sync::auto::lock_vault(
                session.store.data_dir(),
                &record.vault_id,
                true,
            )?;
            let synced = lean_ctx_hive::sync::place::sync_vault(
                &session,
                record,
                std::path::Path::new(&root),
                &mut screen,
            );
            lean_ctx_hive::sync::auto::note_manual(&session.store, record, synced.as_ref());
            match synced {
                Ok(done) => {
                    println!(
                        "Vault @{}/{}: {} event(s) stored{}, {} captured, {} episode(s), {} pushed{}; capture gate: {}",
                        record.ns,
                        record.name,
                        done.stored,
                        if done.cloned { " (cloned)" } else { "" },
                        done.captured,
                        done.episodes,
                        done.pushed,
                        if done.failed.is_empty() {
                            String::new()
                        } else {
                            format!(", {} refused", done.failed.len())
                        },
                        done.counts.summary()
                    );
                    if done.superseded + done.retracted + done.confirmed > 0 {
                        println!(
                            "  knowledge lifecycle: {} superseded, {} withdrawn, {} re-anchored",
                            done.superseded, done.retracted, done.confirmed
                        );
                    }
                    if done.docs_behind > 0 {
                        println!(
                            "  docs: {} page(s) now behind their code (`ctx_doc action=status`)",
                            done.docs_behind
                        );
                    }
                    for (id, refusal) in &done.failed {
                        say.refusal(&format!("hive: event {id} refused: {refusal}"));
                    }
                }
                Err(error) => {
                    say.refusal(&format!(
                        "hive: vault @{}/{}: {error}",
                        record.ns, record.name
                    ));
                }
            }
        }
        _ => {
            // Queue this project's learnings first, so `sync` also covers the current session.
            lean_ctx_hive::knowledge::draft::enqueue_session_draft(
                &session.store,
                config,
                std::path::Path::new(&root),
                "cli-sync",
                lean_ctx_hive::knowledge::draft::Trigger::SessionEnd,
                &mut screen,
                Utc::now(),
            )?;
        }
    }
    let report =
        lean_ctx_hive::knowledge::pull::sync(&session, config, Some(std::path::Path::new(&root)));
    if session
        .lease
        .lease
        .grants(lean_ctx_protocol::hive::lease::HiveRight::PolicyPull)
    {
        match lean_ctx_hive::policy::pull(&session).unwrap_or_else(|error| {
            lean_ctx_hive::policy::PolicyPull::Refused(format!("not fetched: {error}"))
        }) {
            lean_ctx_hive::policy::PolicyPull::Accepted(state) => {
                lean_ctx_hive::policy::note_pull(
                    session.store.data_dir(),
                    lean_ctx_hive::policy::PulledVia::Manual,
                );
                println!(
                    "Policy bundle verified ({} filter packs).",
                    state.filter_packs.len()
                );
            }
            lean_ctx_hive::policy::PolicyPull::Refused(reason) => {
                say.refusal(&format!(
                    "hive: policy bundle {}",
                    if reason.starts_with("not fetched") {
                        reason
                    } else {
                        format!("refused: {reason}")
                    }
                ));
            }
            lean_ctx_hive::policy::PolicyPull::Unchanged => lean_ctx_hive::policy::note_pull(
                session.store.data_dir(),
                lean_ctx_hive::policy::PulledVia::Manual,
            ),
        }
    }
    for number in &report.drafts_sent {
        println!("Sent draft #{number}.");
    }
    for (repo, revision, loaded) in &report.pulled {
        println!("Pulled {repo} revision {revision} ({loaded} facts).");
    }
    let proposals = lean_ctx_hive::tasks::propose::drain(&session);
    for (number, state) in &proposals.sent {
        println!(
            "{}",
            lean_ctx_hive::tasks::propose::created_line(*number, *state)
        );
    }
    if proposals.pending > 0 || proposals.failed > 0 {
        println!(
            "{} task proposals still queued, {} refused.",
            proposals.pending, proposals.failed
        );
    }
    for problem in &proposals.problems {
        say.refusal(&format!("hive: task proposal: {problem}"));
    }
    use lean_ctx_hive::metrics_v2::upload::{self as hours, MetricsV2Run};
    match hours::run(&session, config, Utc::now(), true) {
        Ok(MetricsV2Run::Sent { buckets, refused }) => {
            println!("Sent instance metrics ({buckets} hours stored, {refused} slices refused).");
        }
        Ok(MetricsV2Run::Off(sharing)) => {
            println!("Instance metrics not sent: {}.", sharing.describe());
        }
        Ok(MetricsV2Run::Waiting) => {
            println!("Instance metrics wait for Hive (rate limit or outage); retried later.");
        }
        Ok(MetricsV2Run::NotReleased | MetricsV2Run::Empty | MetricsV2Run::NotDue) => {}
        Err(error) => say.refusal(&format!("hive: instance metrics not sent: {error}")),
    }
    use lean_ctx_hive::receipts::{self, ReceiptsRun};
    match receipts::run(&session, config, Utc::now()) {
        Ok(ReceiptsRun::Sent {
            accepted,
            duplicates,
            ..
        }) if accepted + duplicates > 0 => {
            println!("Sent receipts ({accepted} new, {duplicates} already in Hive).");
        }
        Ok(_) => {}
        Err(error) => say.refusal(&format!("hive: receipts not sent: {error}")),
    }
    if report.drafts_sent.is_empty() && report.pulled.is_empty() && proposals.sent.is_empty() {
        println!("Nothing new.");
    }
    if report.drafts_pending > 0 || report.drafts_failed > 0 {
        println!(
            "{} drafts still queued, {} refused (see `lean-ctx hive status`).",
            report.drafts_pending, report.drafts_failed
        );
    }
    for problem in &report.problems {
        say.refusal(&format!("hive: {problem}"));
    }
    if let Some(reason) = &say.locked {
        session.store.record_locked(Some(reason));
    } else if report.problems.is_empty() {
        // Hive served this sync: a plan was chosen meanwhile, the lock is over
        // (the lease that still names it may run for hours, E11).
        session.store.record_locked(None);
    }
    Ok(())
}

fn bus(config: &HiveConfig, args: &[String]) -> Result<(), HiveError> {
    if args.first().map(String::as_str) != Some("relay") {
        return Err(HiveError::Config(
            "usage: lean-ctx hive bus relay [--for <seconds>]".into(),
        ));
    }
    let seconds = match flag_value(args, "--for")? {
        Some(value) => value
            .parse::<u64>()
            .map_err(|_| HiveError::Config("--for takes seconds".into()))?,
        None => u64::MAX / 4,
    };
    // The daemon runs the same relay in the background; this runs it in
    // the foreground (watch it, or relay without a daemon). Either way it
    // says what was sent, or why nothing was (#252).
    let report = lean_ctx_hive::bus::Report::to(|line| println!("{line}"));
    if lean_ctx_hive::bus::run_for(
        config.clone(),
        std::time::Duration::from_secs(seconds),
        report,
    )? {
        println!(
            "bus relay: stopped after {seconds} s. Hive keeps a worker on the bus only while a relay is connected; the daemon's relay takes over when the daemon runs."
        );
        return Ok(());
    }
    println!(
        "A bus relay is already running on this machine; it was asked to announce this machine's agents (and their roles) now."
    );
    match lean_ctx_hive::bus::presence::announce_and_wait(std::time::Duration::from_secs(
        ANNOUNCE_WAIT_SECS.min(seconds.max(1)),
    )) {
        Some(text) => print!("{text}"),
        None => println!(
            "bus relay: the running relay sent no fresh hello within {} s: it is not connected to Hive right now (see the last error in `lean-ctx hive status`).",
            ANNOUNCE_WAIT_SECS.min(seconds.max(1))
        ),
    }
    Ok(())
}

/// How long `hive bus relay` waits for the running relay's fresh hello.
const ANNOUNCE_WAIT_SECS: u64 = 15;

fn link_project(args: &[String]) -> Result<(), HiveError> {
    let cwd = std::env::current_dir().map_err(|e| HiveError::Local(e.to_string()))?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    for line in link_project_at(std::path::Path::new(&root), args)? {
        println!("{line}");
    }
    // Dogfood 5.11.: after a connect this machine may have resolved the
    // project to another vault. That resolution is dropped, so neither
    // status nor sync keeps using it; connect resolves it again.
    let store = HiveStore::open()?;
    if store.instance()?.is_some()
        && let Some(stale) = lean_ctx_hive::resolution::forget_stale_at(
            store.data_dir(),
            std::path::Path::new(&root),
        )?
    {
        println!(
            "Before this link, {}. That resolution is dropped and never used again: run `lean-ctx hive connect` here to resolve the project to {} (sync does the same; both ask for your yes where a first share needs it).",
            stale.describe(),
            stale.linked().unwrap_or("the linked vault")
        );
    }
    Ok(())
}

/// `hive link <workspace>/<vault> [--leave-unsettled-local]` for the
/// project at `root`: refused while local tasks are unsettled (they would
/// be stranded once the project's tasks are Hive's), unless the person
/// leaves them local on purpose.
fn link_project_at(root: &std::path::Path, args: &[String]) -> Result<Vec<String>, HiveError> {
    use lean_ctx_hive::bring_local;
    let repo = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .ok_or_else(|| HiveError::Config("usage: lean-ctx hive link <workspace>/<vault>".into()))?;
    let leave = args.iter().any(|a| a == bring_local::LEAVE_UNSETTLED_FLAG);
    let unsettled = bring_local::link_gate(root, leave).map_err(HiveError::Refused)?;
    let outcome = link::link(root, repo)?;
    let mut lines = vec![format!(
        "Linked {} to {repo} ({}).",
        root.display(),
        outcome.file.display()
    )];
    if outcome.created_identity {
        lines.push(
            "Created .lean-ctx-id so the project keeps its identity when it moves; commit both files."
                .into(),
        );
    }
    if let Some(note) = bring_local::not_shared_note(&unsettled) {
        lines.push(note);
    }
    Ok(lines)
}

/// `24h`, `90m`, `7d`, `3600s` (a bare number: hours).
fn parse_idle(text: &str) -> Option<std::time::Duration> {
    let text = text.trim();
    let (number, unit) = match text.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((i, _)) => (&text[..i], &text[i..]),
        None => (text, "h"),
    };
    let n: u64 = number.parse().ok()?;
    let secs = match unit {
        "s" => n,
        "m" => n.checked_mul(60)?,
        "h" => n.checked_mul(3600)?,
        "d" => n.checked_mul(86_400)?,
        _ => return None,
    };
    Some(std::time::Duration::from_secs(secs))
}

/// `ms` as a short age: `40s ago`, `12m ago`, `5h ago`, `3d ago`.
fn seen_ago(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s}s ago"),
        60..3600 => format!("{}m ago", s / 60),
        3600..86_400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

/// `hive worker [status|pause|resume|stop|list|rename|register|retire]`.
fn worker(config: &HiveConfig, args: &[String]) -> Result<(), HiveError> {
    use lean_ctx_hive::worker;
    let store = HiveStore::open()?;
    match args.first().map_or("status", String::as_str) {
        "list" => {
            use lean_ctx_hive::workers::attach;
            let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
            let workers = attach::list(&store, &instance.instance_id)?;
            let now_ms = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default();
            if workers.is_empty() {
                println!("No workers yet: one appears when an agent session posts or captures.");
            }
            for w in workers {
                let name = w.name.clone().unwrap_or_else(|| {
                    lean_ctx_hive::workers::default_name(
                        &instance.label,
                        &attach::host_display(&w.host_app),
                        &w.role,
                    )
                });
                let state = if w.registered_at.is_some() {
                    "registered"
                } else {
                    "local"
                };
                let seen = attach::last_seen_ms(&w).map_or_else(
                    || "never seen".to_string(),
                    |ms| format!("last seen {}", seen_ago(now_ms.saturating_sub(ms))),
                );
                println!("{}  {name}  ({state}, {seen})", w.worker_id);
            }
        }
        "register" => {
            // E10 for the workspace worker of (--cli, --role) now, before an
            // agent session posts.
            use lean_ctx_hive::workers::{attach, card, register};
            let cli = flag_value(args, "--cli")?.ok_or_else(|| {
                HiveError::Config(
                    "usage: lean-ctx hive worker register --cli <host> [--role <role>]".into(),
                )
            })?;
            let role = lean_ctx_hive::workers::normalize_role(
                flag_value(args, "--role")?.unwrap_or("general"),
            );
            let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
            let root = lean_ctx_hive::root_cert::trust_root(&store, config, &instance.url)?;
            let session = Session::load(store, root)?;
            let attached =
                attach::attach(&session.store, &instance.instance_id, cli, &role)?.worker;
            let worker = register::ensure_registered(
                &session,
                &attached,
                &card::WorkerCard::new(
                    cli,
                    None,
                    Vec::new(),
                    card::Budget::default(),
                    card::Availability::Online,
                ),
            )?;
            println!("{}", worker.worker_id);
        }
        "retire" => {
            // Owner 09.11.: workers that are not active do not count and are
            // switched off. Out of the book, then `DELETE /v1/workers/{id}`.
            use lean_ctx_hive::workers::{attach, register};
            let usage = || {
                HiveError::Config(
                    "usage: lean-ctx hive worker retire <wkr_…> [--force] | --idle <dur> (e.g. 24h, 90m, 7d; default 24h)"
                        .into(),
                )
            };
            let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
            let gone = if args.iter().any(|a| a == "--idle") {
                let idle = match flag_value(args, "--idle")? {
                    Some(text) => parse_idle(text).ok_or_else(usage)?,
                    None => register::IDLE_RETIRE,
                };
                attach::give_up_idle(&store, &instance.instance_id, idle, false)?
            } else {
                let id = args
                    .get(1)
                    .filter(|a| !a.starts_with("--"))
                    .ok_or_else(usage)?;
                if !lean_ctx_hive::bus::address::is_worker_id(id) {
                    return Err(usage());
                }
                // A worker an active session holds is refused unless forced.
                let force = args.iter().any(|a| a == "--force");
                attach::give_up(
                    &store,
                    &instance.instance_id,
                    std::slice::from_ref(id),
                    force,
                )?
            };
            let root = lean_ctx_hive::root_cert::trust_root(&store, config, &instance.url)?;
            let session = Session::load(store, root)?;
            let sent = register::retire_evicted(&session);
            if gone.is_empty() {
                println!("No worker to retire.");
            }
            for id in &gone {
                println!("Retired {id}");
            }
            if let Err(error) = sent {
                println!("Hive was not told yet ({error}); the next sync retires them there.");
            }
        }
        "rename" => {
            let (Some(id), Some(name)) = (args.get(1), args.get(2)) else {
                return Err(HiveError::Config(
                    "usage: lean-ctx hive worker rename <wkr_…> <name>".into(),
                ));
            };
            let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
            let root = lean_ctx_hive::root_cert::trust_root(&store, config, &instance.url)?;
            let session = Session::load(store, root)?;
            let renamed = session.with_client(|client| {
                lean_ctx_hive::workers::register::patch(client, id, Some(name), None)
            })?;
            lean_ctx_hive::workers::attach::update_book(
                &session.store,
                &instance.instance_id,
                |workers| {
                    if let Some(w) = workers.iter_mut().find(|w| &w.worker_id == id) {
                        w.name = Some(renamed.name.clone());
                    }
                },
            )?;
            println!("{} is now named {}", renamed.id, renamed.name);
        }
        "pause" => {
            worker::set_paused(&store, true)?;
            println!(
                "Worker mode paused on this machine: no new claims, and held claims are no longer renewed, until `lean-ctx hive worker resume`."
            );
        }
        "resume" => {
            worker::set_paused(&store, false)?;
            println!("Worker mode resumed on this machine.");
        }
        "stop" => {
            let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
            let root = lean_ctx_hive::root_cert::trust_root(&store, config, &instance.url)?;
            let session = Session::load(store, root)?;
            let stopped = worker::stop(&session);
            // Hive ended them, or they end at their expiry when the stop did
            // not reach it: either way this machine stops renewing them.
            lean_ctx_hive::tasks::claims::clear(session.store.data_dir())?;
            stopped?;
            println!(
                "Worker mode stopped for this instance; its claims ended. The owner turns it on again on the web."
            );
        }
        "status" => {
            let local = worker::read_state(&store);
            let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
            let root = lean_ctx_hive::root_cert::trust_root(&store, config, &instance.url)?;
            let session = Session::load(store, root)?;
            let optin = worker::fetch_optin(&session)?;
            let state = match (&optin.optin_id, optin.active, local.paused) {
                (None, _, _) => "not opted in",
                (Some(_), false, _) => "stopped",
                (Some(_), true, true) => {
                    "paused on this machine (resume: `lean-ctx hive worker resume` in your terminal)"
                }
                (Some(_), true, false) => "active",
            };
            println!("Worker mode:  {state}");
            println!("Opted in:     {} vaults", optin.vaults.len());
            if optin.optin_id.is_some() {
                let budget = optin.daily_budget;
                let cap = |server: Option<u64>, local: Option<u64>| match (server, local) {
                    (Some(a), Some(b)) => a.min(b).to_string(),
                    (a, b) => a.or(b).map_or_else(|| "no limit".into(), |v| v.to_string()),
                };
                let caps = &config.worker;
                let used = |server: u64, here: u64| server.max(here);
                println!(
                    "Tasks today:  {} of {}",
                    used(u64::from(optin.used_today.tasks), local.used.tasks),
                    cap(
                        budget.tasks.map(u64::from),
                        caps.max_tasks_per_day.map(u64::from)
                    )
                );
                println!(
                    "Tokens today: {} of {}",
                    used(optin.used_today.tokens, local.used.tokens),
                    cap(budget.tokens, caps.max_tokens_per_day)
                );
                println!(
                    "USD today:    {:.2} of {}",
                    used(optin.used_today.usd_micros, local.used.usd_micros) as f64 / 1e6,
                    match (budget.usd_micros, caps.max_usd_micros_per_day) {
                        (None, None) => "no limit".into(),
                        (a, b) => format!(
                            "{:.2}",
                            a.into_iter().chain(b).min().unwrap_or(0) as f64 / 1e6
                        ),
                    }
                );
            }
            println!("\n{}", worker::PROVIDER_TERMS_NOTE);
        }
        other => {
            return Err(HiveError::Config(format!(
                "unknown worker command '{other}' (status, pause, resume, stop)"
            )));
        }
    }
    Ok(())
}

/// `hive graph publish`: the current project's code graph (N1).
fn publish_graph(config: &HiveConfig) -> Result<(), HiveError> {
    use lean_ctx_hive::knowledge::code_graph;
    let store = HiveStore::open()?;
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let root = lean_ctx_hive::root_cert::trust_root(&store, config, &instance.url)?;
    let session = Session::load(store, root)?;
    let cwd = std::env::current_dir().map_err(|e| HiveError::Local(e.to_string()))?;
    let project_root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let record = lean_ctx_hive::resolution::vault_for_root(
        session.store.data_dir(),
        std::path::Path::new(&project_root),
    )?
    .ok_or_else(|| {
        HiveError::Config(
            "this repository is not linked to a vault (run `lean-ctx hive sync` in it)".into(),
        )
    })?;
    let sent = code_graph::publish(&session, &record, std::path::Path::new(&project_root))?;
    println!(
        "Code graph of {}/{} sent: {} nodes, {} edges{}.",
        record.ns,
        record.name,
        sent.nodes,
        sent.edges,
        sent.revision_hint
            .as_deref()
            .map(|c| format!(" at {}", &c[..12]))
            .unwrap_or_default()
    );
    if sent.withheld_files > 0 {
        println!(
            "{} files left out (not admitted as knowledge, or paths Hive does not take).",
            sent.withheld_files
        );
    }
    if sent.truncated {
        println!("The checkout exceeds the graph limits; whole files were left out.");
    }
    Ok(())
}

#[cfg(test)]
mod link_tests {
    use super::*;

    /// The terminal `hive link` asks the same gate as the JSON one: refused
    /// while a local task is unsettled, linked with --leave-unsettled-local,
    /// and then it says which tasks stay local.
    #[test]
    fn the_terminal_link_refuses_while_local_tasks_are_unsettled() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        lean_ctx_hive::tasks::local::write(root, |board, now| {
            board.propose(
                &lean_ctx_tasks::Actor::person("person"),
                lean_ctx_tasks::Proposal {
                    title: "Proposed only".into(),
                    goal: None,
                    acceptance: "It works".into(),
                    role: None,
                },
                now,
            )
        })
        .unwrap();
        let args = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let refused = link_project_at(root, &args(&["platform/api"])).unwrap_err();
        assert!(refused.to_string().contains("#1 (proposed)"), "{refused}");
        assert!(!root.join(link::LINK_FILE).exists(), "nothing written");

        let lines =
            link_project_at(root, &args(&["platform/api", "--leave-unsettled-local"])).unwrap();
        assert!(root.join(link::LINK_FILE).exists());
        assert!(
            lines
                .iter()
                .any(|l| l.contains("local, not shared: #1 (proposed)")),
            "{lines:?}"
        );
    }
}

#[cfg(test)]
mod worker_retire_tests {
    use super::{parse_idle, seen_ago};
    use std::time::Duration;

    #[test]
    fn idle_durations_read_with_their_unit() {
        assert_eq!(parse_idle("24h"), Some(Duration::from_secs(86_400)));
        assert_eq!(parse_idle("90m"), Some(Duration::from_secs(5_400)));
        assert_eq!(parse_idle("7d"), Some(Duration::from_secs(604_800)));
        assert_eq!(parse_idle("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_idle("2"), Some(Duration::from_secs(7_200)));
        assert_eq!(parse_idle("1w"), None);
        assert_eq!(parse_idle("h"), None);
    }

    #[test]
    fn last_seen_reads_as_a_short_age() {
        assert_eq!(seen_ago(40_000), "40s ago");
        assert_eq!(seen_ago(12 * 60_000), "12m ago");
        assert_eq!(seen_ago(5 * 3_600_000), "5h ago");
        assert_eq!(seen_ago(3 * 86_400_000), "3d ago");
    }
}
