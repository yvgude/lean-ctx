// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hive connect|status|disconnect|link|worker`.

use chrono::Utc;

use crate::core::hive::auth::Session;
use crate::core::hive::status::{HiveStatusReport, report};
use crate::core::hive::{HiveConfig, HiveError, HiveStore, link};
use lean_ctx_protocol::hive::lease::LeaseState;

const HELP: &str = "\
lean-ctx hive — share what your agents learn with your team in LeanCTX Hive

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
  lean-ctx hive help advanced  Sealed vaults, review, maintenance, worker mode,
                                tasks, the agent bus and the Intelligence runtime.

Plans: Personal 3 engines, 2 connected at once; Team 5 per seat,
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
  lean-ctx hive grant [--yes]
  lean-ctx hive review [merge <workspace>/<vault> <number>... | rollback <workspace>/<vault> <revision>]
  lean-ctx hive worker [status|pause|resume|stop|list|rename <wkr> <name>|register --cli <host> [--role <role>]]
  lean-ctx hive graph publish
  lean-ctx hive tasks [list|show|claim|update|submit|release|propose] …
  lean-ctx hive status [--json] [--health]

metrics     Engine metrics for the workspace (hourly counts, including full
            re-fetches of shortened output, the names of known programs and
            host families no enforcing rule covers, MCP servers only when the
            workspace policy names them; never content, paths, prompts, model output,
            secrets or command lines).
            preview prints the notice and exactly what would be sent next
            for the last hour (--json: the unsigned batch; nothing is sent);
            confirm (Personal, in a terminal only) shares them once
            you agree; delete removes what this engine buffered for it.
capture     Run the capture gate for this repository now: only single,
            referenced claims leave (one sentence of 24-300 characters with a
            file:, sym:, test:, commit: or https:// reference); everything
            else stays local and is counted by reason. --recapture removes this
            repository's unsent session drafts and captures all local
            knowledge again.
replica     The local replica of this repository's vault (C0 2.0 Hive only):
            place, cursor, live nodes and the graph digest two engines that
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
grant       Sealed vaults: give the vault key to instances a maintainer
            approved. Compare each fingerprint with that instance's own
            `lean-ctx hive status` before you confirm; --yes confirms that
            you did.
review      Open proposals of your vaults, readable here even when the
            vault is sealed; `merge` writes them as the next revision
            (needs the knowledge.merge right); `rollback` restores the
            document of an earlier revision as a new one (sealed vaults: opened
            with that revision's key and sealed again with the current one).
            `decide [<rv_id>] [--reason <code>]` (C0 2.0 sealed vaults): list the
            items waiting for a person, or show one decrypted here, confirm
            each command with y and let a maintainer answer in the browser.
graph      publish: send this repository's code graph (files, symbols, imports;
            never code text) to Hive, when the vault owner turned on the
            full graph on the web. Sealed vaults send it sealed.
worker      Worker mode: your agents pull tasks you opted in for on the web,
            within its daily budget ([hive.worker] can only lower it).
            status shows the opt-in and today's use; pause/resume stop and
            restart claiming on this machine; stop switches worker mode off
            for this engine and ends its claims (the owner turns it on again
            on the web). list shows this engine's durable workers (wkr_ ids);
            rename gives one a name of 1-64 letters, digits, spaces and
            . _ · - from one alphabet. You use your own subscription; follow your provider's
            terms (OpenCode or Pi with a Claude Pro/Max login and third-party
            tools with a Gemini CLI login are not allowed by the providers).
tasks       The vault's tasks: read them, work one under a claim (worker
            mode), report, submit a result with evidence, or propose one
            (`lean-ctx hive tasks --help`).
status      --json prints the cockpit shape; --health adds the health figures
            of sealed vaults, computed here and never sent.
";

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
    if matches!(sub, "help" | "--help" | "-h") || rest.iter().any(|a| a == "--help" || a == "-h") {
        print!("{}", help_for(sub, rest));
        return;
    }
    let config = crate::core::config::Config::load().hive.clone();
    if !connector_open(&config, sub, rest) {
        eprintln!("{}", crate::core::hive::NOT_AVAILABLE);
        std::process::exit(1);
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
        "grant" => grant_sealed(&config, super::prompt::wants_yes(rest)),
        "review" => super::hive_review_cmd::review(&config, rest),
        "worker" => worker(&config, rest),
        "graph" if rest.first().map(String::as_str) == Some("publish") => publish_graph(&config),
        "tasks" => super::hive_tasks_cmd::run(rest),
        "consent" => super::hive_consent_cmd::run(&config, rest),
        "swarm" | "compute" => {
            eprintln!(
                "hive: `{sub}` was retired in 3.12.1 (Hive swarms and Task Compute were \
                 removed); use `lean-ctx hive tasks` for this vault's tasks"
            );
            std::process::exit(2);
        }
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

/// The release switch ([`crate::core::hive::HIVE_AVAILABLE`]) for the two
/// commands that start a Hive relationship, decided before any store or
/// network access. Everything else keeps its "not connected" answers.
fn connector_open(config: &HiveConfig, sub: &str, rest: &[String]) -> bool {
    if crate::core::hive::HIVE_AVAILABLE {
        return true;
    }
    match sub {
        "connect" => {
            let origin = match flag_value(rest, "--url") {
                Ok(Some(url)) => crate::core::hive::config::normalize_origin(url).ok(),
                Ok(None) => config.origin().ok(),
                Err(_) => None,
            };
            origin.is_some_and(|origin| crate::core::hive::available_for(&origin))
        }
        "link" => crate::core::hive::available_locally(),
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
        "advanced" | "all" | "capture" | "replica" | "maintain" | "runtime" | "bus" | "grant"
        | "review" | "worker" | "graph" | "tasks" => ADVANCED_HELP,
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
        let json = serde_json::to_string(&current.cli_status())
            .map_err(|e| HiveError::Local(e.to_string()))?;
        println!("{json}");
        return Ok(());
    }
    print_report(&current);
    if crate::core::hive::disconnect::pending(&store).is_some() {
        println!(
            "Disconnect:   pending (Hive was not reachable); retried in the background, or run `lean-ctx hive disconnect` again"
        );
    }
    // Release B: quota and policy throttles in force (what waits, until when).
    for line in crate::core::hive::throttle::status_lines(&store, Utc::now()) {
        println!("{line}");
    }
    // §3.16: the upgrade duties (local; once) and their counts, printed once.
    if current.instance.is_some() {
        let root = std::env::current_dir().ok().map(|cwd| {
            std::path::PathBuf::from(crate::core::protocol::detect_project_root_or_cwd(
                &cwd.to_string_lossy(),
            ))
        });
        for line in crate::core::hive::effective_policy::status_lines(
            store.data_dir(),
            &current.policy,
            Utc::now(),
            root.as_deref(),
        ) {
            println!("{line}");
        }
        if let Err(error) = crate::core::hive::migrate::run(&store, config) {
            println!("Upgrade:      {error}");
        }
        for line in crate::core::hive::migrate::status_lines_once(&store) {
            println!("{line}");
        }
    }
    if args.iter().any(|a| a == "--health") {
        for line in crate::core::hive::delivery::sealed_health_lines(&store) {
            println!("{line}");
        }
    }
    Ok(())
}

/// Whether the project here is shared, in one line: its vault, a first
/// share that waits for the person, or not set up yet. `None` outside a
/// project LeanCTX can identify.
fn project_line() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let root = crate::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let root = std::path::Path::new(&root);
    crate::core::hive::vault::project_identity(root)?;
    let name = root.file_name()?.to_string_lossy().into_owned();
    let store = HiveStore::open().ok()?;
    let map = crate::core::hive::vault::VaultMap::load(store.data_dir());
    if let Some(record) = map.for_root(&root.to_string_lossy()) {
        return Some(format!("{name} → {}/{} (shared)", record.ns, record.name));
    }
    let config = crate::core::config::Config::load().hive.clone();
    let broker = crate::core::hive::consent::Broker::open(
        &store,
        crate::core::hive::consent::Trust::load(&store, &config),
    );
    let now = u64::try_from(Utc::now().timestamp_millis()).unwrap_or_default();
    let waiting = broker.list().ok()?.into_iter().any(|l| {
        l.valid
            && l.record.kind == crate::core::hive::consent::Kind::LinkVault
            && l.record.state == crate::core::hive::consent::State::Pending
            && l.record.expires_at > now
            && l.record.subject["bound_repo"].as_str() == Some(name.as_str())
            && !crate::core::hive::mapping::satisfied(&l.record, &map)
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
        if crate::core::hive::HIVE_AVAILABLE {
            println!(
                "Hive: not connected ({}). Run `lean-ctx hive connect`.",
                current.url
            );
        } else {
            println!("Hive: not connected. {}", crate::core::hive::NOT_AVAILABLE);
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
                    "engine"
                } else {
                    "engines"
                }
            ),
        }
        println!("Lease:        {state}");
        if let Ok(store) = HiveStore::open()
            && let Some(fingerprint) = crate::core::hive::sealed::own_fingerprint(&store)
        {
            println!(
                "Key print:    {fingerprint} (compare it when a member grants you a sealed vault)"
            );
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
    match policy.org_state.as_deref() {
        Some("required") if policy.enforced => println!(
            "Company policy: v{} enforced (valid until {})",
            policy.policy_version,
            policy.not_after.as_deref().unwrap_or("?")
        ),
        Some("required") => println!(
            "Company policy: v{} received and verified (valid until {}); enforcement needs the LeanCTX Intelligence runtime",
            policy.policy_version,
            policy.not_after.as_deref().unwrap_or("?")
        ),
        Some("unpinned") => println!(
            "Company policy: lifted by a signed unpin (v{})",
            policy.policy_version
        ),
        _ => println!("Company policy: none"),
    }
    if !policy.filter_packs.is_empty() {
        let packs: Vec<String> = policy
            .filter_packs
            .iter()
            .map(|(id, v)| format!("{id} v{v}"))
            .collect();
        println!("Filter packs: {}", packs.join(", "));
    }
    if !policy.required_filter_packs.is_empty() {
        let packs: Vec<String> = policy
            .required_filter_packs
            .iter()
            .zip(
                current
                    .required_installed
                    .iter()
                    .copied()
                    .chain(std::iter::repeat(false)),
            )
            .map(|(r, ok)| {
                let state = if ok {
                    "installed"
                } else {
                    "NOT installed as signed"
                };
                format!("{} v{} ({state})", r.pack_id, r.version)
            })
            .collect();
        println!("Required:     {}", packs.join(", "));
    }
    if let Some(refusal) = &policy.last_refusal {
        println!(
            "Policy refused: {refusal} (not compliant with the newest bundle; the last verified one stays)"
        );
    }
    if let Some(error) = &policy.enforcement_error {
        println!("Enforcement:  not set up: {error}");
    }
    if current.lease.is_some() {
        match current.metrics_off {
            None => println!("Metrics:      sent hourly (numbers per vault and day, no content)"),
            Some(reason) => println!("Metrics:      not sent: {reason}"),
        }
        if let (Some(v2), Some(instance)) = (&current.metrics_v2, &current.instance) {
            println!("Engine metrics: {}", v2.line(&instance.workspace_label()));
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
        for (place, counts) in crate::core::hive::capture::load_counts(store.data_dir()) {
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
        if crate::core::hive::instance_key::InstanceKeys::load(&store).is_ok() {
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
    use crate::core::hive::disconnect::{self, Told};
    match disconnect::tell_hive(&store, config, &instance) {
        Told::Revoked => println!(
            "Disconnected {} from {}: Hive revoked this engine and the seat is free.",
            instance.label, instance.org_name
        ),
        Told::AlreadyGone => println!(
            "Disconnected {} from {}: Hive had already ended this engine.",
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
                "Hive did not end this engine ({error}); nothing was removed. Remove the instance \"{}\" in Hive, or run `lean-ctx hive disconnect --force` to remove the local credentials anyway.",
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
    use crate::core::hive::review::maintain;
    use crate::core::hive::sync::writer;
    let req = maintain::parse(args)?;
    let store = HiveStore::open()?;
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let (ns, name) = req
        .vault
        .split_once('/')
        .ok_or_else(|| HiveError::Config("name the vault as <workspace>/<vault>".into()))?;
    let cwd = std::env::current_dir().map_err(|e| HiveError::Local(e.to_string()))?;
    let root = crate::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let map = crate::core::hive::vault::VaultMap::load(store.data_dir());
    let record = map
        .for_root(&root)
        .filter(|r| r.ns == ns && r.name == name)
        .cloned()
        .ok_or_else(|| {
            HiveError::Config(format!(
                "run it in a checkout linked to {ns}/{name} (`lean-ctx hive link {ns}/{name}`)"
            ))
        })?;
    let dir = crate::core::hive::replica::store::place_dir(store.data_dir(), &record.vault_id);
    if !dir.join("replica.db").exists() {
        println!("No replica for {ns}/{name} yet: run `lean-ctx hive sync` first.");
        return Ok(());
    }
    let trust = crate::core::hive::root_cert::trust_root(&store, config, &instance.url)?;
    let session = Session::load(store.clone(), trust)?;
    let (mk, _) = crate::core::hive::replica::crypt::master_key(store.config_dir(), false)?;
    let (replica, _) =
        crate::core::hive::replica::Replica::open(store.data_dir(), &record.vault_id, &mk)?;
    // The signer, place and place ref as every vault writer builds them
    // (C0 2.0 spellings: hlc.node and place id as hyphenated UUIDs).
    let writer = writer::VaultWriter::open(&session, maintain::MAINTENANCE_ROLE)?;
    let place = writer::vault_place(&record, &replica)?;
    let signer = writer.signer();
    // Sealed vaults (§3.5.5): the tasks come from the decrypted replica (its
    // id_salt is the sealed pk_id since the sync), the events are sealed
    // with the newest epoch key held.
    let seal = writer::vault_seal(&store, &record)?;
    let target = maintain::Target {
        replica: &replica,
        place: &place,
        signer: &signer,
        seal: seal.as_ref(),
    };
    let api = crate::core::hive::sync::place::SessionApi(&session);
    let place_ref = writer::vault_ref(&record);
    let mut checkout = crate::core::hive::capture::refs::Checkout::new(std::path::Path::new(&root));
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
    let root = crate::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let identity = crate::core::hive::vault::project_identity(std::path::Path::new(&root))
        .ok_or_else(|| HiveError::Config("not inside a git repository".into()))?;
    let stable = lean_ctx_protocol::hive::vault::stable_identity(&identity);
    let record = crate::core::hive::vault::VaultMap::load(store.data_dir())
        .vaults
        .get(&stable)
        .cloned()
        .ok_or_else(|| HiveError::Config("this repository is not linked to a vault".into()))?;
    let dir = crate::core::hive::replica::store::place_dir(store.data_dir(), &record.vault_id);
    if !dir.join("replica.db").exists() {
        println!(
            "No replica for @{}/{} yet (`lean-ctx hive sync` clones it on a C0 2.0 Hive).",
            record.ns, record.name
        );
        return Ok(());
    }
    let (mk, at_rest) = crate::core::hive::replica::crypt::master_key(store.config_dir(), false)?;
    let (replica, _) =
        crate::core::hive::replica::Replica::open(store.data_dir(), &record.vault_id, &mk)?;
    let mut graph = crate::core::hive::replica::fold::Graph::default();
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
    use crate::core::hive::knowledge::draft;
    let store = HiveStore::open()?;
    store.instance()?.ok_or(HiveError::NotConnected)?;
    let cwd = std::env::current_dir().map_err(|e| HiveError::Local(e.to_string()))?;
    let root = crate::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let root = std::path::Path::new(&root);
    let mut screen = crate::core::hive::knowledge::screening::Screen::for_installation();
    if crate::core::hive::vault::project_identity(root)
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
    for (place, counts) in crate::core::hive::capture::load_counts(store.data_dir()) {
        println!("{place}: {}", counts.summary());
    }
    Ok(())
}

/// `lean-ctx hive metrics`: engine metrics v2. Never sends metrics.
fn metrics(config: &HiveConfig, args: &[String]) -> Result<(), HiveError> {
    use crate::core::hive::metrics_v2::{self, gate, upload};
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
    let root = crate::core::hive::root_cert::trust_root(&store, config, &instance.url)?;
    let session = Session::load(store.clone(), root)?;
    let lease = &session.lease.lease;
    if wants("delete") {
        upload::delete_local(&store);
        let remote = match upload::delete_remote(&session) {
            Ok(true) => " Hive deleted every hour it stored for this engine.".to_string(),
            Ok(false) => String::new(),
            Err(error) => {
                format!(" Hive could not delete its stored hours now ({error}); run it again.")
            }
        };
        println!(
            "Deleted what this engine buffered for {workspace} (hours not sent yet, the \
             fingerprint key).{remote}"
        );
        return Ok(());
    }
    if wants("confirm") {
        // Before the release there is nothing to confirm: say so, ask nothing.
        if !upload::released(&instance.url) {
            println!(
                "Engine metrics are not released yet: nothing is shared with {workspace}, so there is nothing to confirm."
            );
            return Ok(());
        }
        // A person, at a terminal: never an agent or a script (`--yes` does
        // not count here, owner decision).
        if !std::io::IsTerminal::is_terminal(&std::io::stdin())
            || !std::io::IsTerminal::is_terminal(&std::io::stdout())
        {
            return Err(HiveError::Refused(
                "engine metrics are confirmed by a person in a terminal: run `lean-ctx hive metrics confirm` there".into(),
            ));
        }
        if !gate::needs_confirmation(lease.plan) {
            println!(
                "{workspace} shares engine metrics by default on its plan; its admins control it with the policy rule \"Share engine metrics\"."
            );
            return Ok(());
        }
        let sharing = gate::decide(&store, config, &instance, lease, Utc::now());
        println!(
            "{}",
            metrics_v2::disclosure(&workspace, &web_url, &sharing, true)
        );
        if super::prompt::confirm("Share engine metrics with this workspace?", false) {
            upload::confirm(&session)?;
            let now = gate::decide(&store, config, &instance, lease, Utc::now());
            println!("Engine metrics for {workspace}: {}.", now.describe());
        } else {
            println!("Nothing changed: engine metrics stay off for {workspace}.");
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
        println!("\nEngine metrics for {workspace}: {}", sharing.describe());
        if !upload::released(&instance.url) {
            println!("Not released yet: this engine sends nothing; below is what it would send.");
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
    use crate::core::hive::HiveError;

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
        use crate::core::hive::error::LOCKED_MARK;
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
    let root = crate::core::hive::root_cert::trust_root(&store, config, &instance.url)?;
    let session = Session::load(store, root)?;
    let cwd = std::env::current_dir().map_err(|e| HiveError::Local(e.to_string()))?;
    let root = crate::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let mut screen = crate::core::hive::knowledge::screening::Screen::for_installation();
    // A C0 2.0 Hive: the bound vault syncs through its replica (signed
    // events), and no 1.x draft is queued for it. A 1.x Hive keeps the
    // draft path below unchanged.
    let bound = crate::core::hive::vault::project_identity(std::path::Path::new(&root))
        .map(|i| lean_ctx_protocol::hive::vault::stable_identity(&i))
        .and_then(|stable| {
            crate::core::hive::vault::VaultMap::load(session.store.data_dir())
                .vaults
                .get(&stable)
                .cloned()
        });
    let v2 = crate::core::hive::bus::relay_v2::speaks_v2(&session.store);
    // The first sync after `hive link` knows the link file but no record
    // yet: resolve it now, so it goes to the replica and not into a draft.
    let bound = match bound {
        None if v2 && link::has_link_file(std::path::Path::new(&root)) => {
            crate::core::hive::knowledge::pull::resolve_project(
                &session,
                config,
                std::path::Path::new(&root),
            )
            .ok()
        }
        other => other,
    };
    match (&bound, v2) {
        (Some(record), true) => {
            match crate::core::hive::sync::place::sync_vault(
                &session,
                record,
                std::path::Path::new(&root),
                &mut screen,
            ) {
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
            crate::core::hive::knowledge::draft::enqueue_session_draft(
                &session.store,
                config,
                std::path::Path::new(&root),
                "cli-sync",
                crate::core::hive::knowledge::draft::Trigger::SessionEnd,
                &mut screen,
                Utc::now(),
            )?;
        }
    }
    let report = crate::core::hive::knowledge::pull::sync(
        &session,
        config,
        Some(std::path::Path::new(&root)),
    );
    if session
        .lease
        .lease
        .grants(lean_ctx_protocol::hive::lease::HiveRight::PolicyPull)
    {
        match crate::core::hive::policy::pull(&session).unwrap_or_else(|error| {
            crate::core::hive::policy::PolicyPull::Refused(format!("not fetched: {error}"))
        }) {
            crate::core::hive::policy::PolicyPull::Accepted(state) => {
                println!(
                    "Policy bundle verified (org policy v{}, {} filter packs).",
                    state.policy_version,
                    state.filter_packs.len()
                );
            }
            crate::core::hive::policy::PolicyPull::Refused(reason) => {
                say.refusal(&format!(
                    "hive: policy bundle {}",
                    if reason.starts_with("not fetched") {
                        reason
                    } else {
                        format!("refused: {reason}")
                    }
                ));
            }
            crate::core::hive::policy::PolicyPull::Unchanged => {}
        }
    }
    for number in &report.drafts_sent {
        println!("Sent draft #{number}.");
    }
    for (repo, revision, loaded) in &report.pulled {
        println!("Pulled {repo} revision {revision} ({loaded} facts).");
    }
    let proposals = crate::core::hive::tasks::propose::drain(&session);
    for (number, state) in &proposals.sent {
        println!(
            "{}",
            crate::core::hive::tasks::propose::created_line(*number, *state)
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
    for (repo, revision) in &report.merged_ready {
        println!("Merged the confirmed sealed proposals of {repo} into revision {revision}.");
    }
    use crate::core::hive::metrics_v2::upload::{self as hours, MetricsV2Run};
    match hours::run(&session, config, Utc::now(), true) {
        Ok(MetricsV2Run::Sent { buckets, refused }) => {
            println!("Sent engine metrics ({buckets} hours stored, {refused} slices refused).");
        }
        Ok(MetricsV2Run::Off(sharing)) => {
            println!("Engine metrics not sent: {}.", sharing.describe());
        }
        Ok(MetricsV2Run::Waiting) => {
            println!("Engine metrics wait for Hive (rate limit or outage); retried later.");
        }
        Ok(MetricsV2Run::NotReleased | MetricsV2Run::Empty | MetricsV2Run::NotDue) => {}
        Err(error) => say.refusal(&format!("hive: engine metrics not sent: {error}")),
    }
    use crate::core::hive::receipts::{self, ReceiptsRun};
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

/// Grant the sealed vault keys this instance holds to the recipients a
/// maintainer approved, each after its fingerprint was confirmed.
fn grant_sealed(config: &HiveConfig, confirmed: bool) -> Result<(), HiveError> {
    use crate::core::hive::config::HiveSealedGrants;
    use crate::core::hive::sealed;
    use crate::core::hive::vault::VaultMap;
    let store = HiveStore::open()?;
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let root = crate::core::hive::root_cert::trust_root(&store, config, &instance.url)?;
    let session = Session::load(store, root)?;
    let map = VaultMap::load(session.store.data_dir());
    let mut granted = 0usize;
    for record in map.vaults.values().filter(|record| record.sealed) {
        let repo = format!("{}/{}", record.ns, record.name);
        let pending = sealed::pending(&session, &record.vault_id)?;
        if pending.recipients.is_empty() {
            continue;
        }
        let epoch = pending.epoch_required;
        if sealed::newest_held_epoch(&session.store, &record.vault_id)? != Some(epoch) {
            println!(
                "{repo}: {} waiting, but this instance holds no key for epoch {epoch} (another member grants them).",
                pending.recipients.len()
            );
            continue;
        }
        for recipient in &pending.recipients {
            println!(
                "{repo}: {} of {} (approved by {}), fingerprint {}",
                recipient.label,
                recipient.owner.name,
                recipient.approved_by.name,
                recipient.kx_fingerprint
            );
            let ask = config.sealed_grants == HiveSealedGrants::Confirm;
            if ask
                && !super::prompt::confirm(
                    "Does this fingerprint match the one that instance shows in `lean-ctx hive status`?",
                    confirmed,
                )
            {
                println!("  not granted.");
                continue;
            }
            sealed::grant(&session, &record.vault_id, epoch, recipient)?;
            granted += 1;
            println!("  granted.");
        }
    }
    if granted == 0 {
        println!("No sealed vault key was granted.");
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
    // the foreground (watch it, or relay without a daemon).
    if !crate::core::hive::bus::run_for(config.clone(), std::time::Duration::from_secs(seconds))? {
        println!(
            "A bus relay is already running on this machine; it was asked to announce this machine's agents (and their roles) now."
        );
    }
    Ok(())
}

fn link_project(args: &[String]) -> Result<(), HiveError> {
    let repo = args
        .first()
        .ok_or_else(|| HiveError::Config("usage: lean-ctx hive link <workspace>/<vault>".into()))?;
    let cwd = std::env::current_dir().map_err(|e| HiveError::Local(e.to_string()))?;
    let root = crate::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let outcome = link::link(std::path::Path::new(&root), repo)?;
    println!("Linked {root} to {repo} ({}).", outcome.file.display());
    if outcome.created_identity {
        println!(
            "Created .lean-ctx-id so the project keeps its identity when it moves; commit both files."
        );
    }
    Ok(())
}

/// `hive worker [status|pause|resume|stop]`.
fn worker(config: &HiveConfig, args: &[String]) -> Result<(), HiveError> {
    use crate::core::hive::worker;
    let store = HiveStore::open()?;
    match args.first().map_or("status", String::as_str) {
        "list" => {
            use crate::core::hive::workers::attach;
            let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
            let workers = attach::list(&store, &instance.instance_id)?;
            if workers.is_empty() {
                println!("No workers yet: one appears when an agent session posts or captures.");
            }
            for w in workers {
                let name = w.name.clone().unwrap_or_else(|| {
                    crate::core::hive::workers::default_name(
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
                println!("{}  {name}  ({state})", w.worker_id);
            }
        }
        "register" => {
            // E10 for the workspace worker of (--cli, --role) now, before an
            // agent session posts.
            use crate::core::hive::workers::{attach, card, register};
            let cli = flag_value(args, "--cli")?.ok_or_else(|| {
                HiveError::Config(
                    "usage: lean-ctx hive worker register --cli <host> [--role <role>]".into(),
                )
            })?;
            let role = crate::core::hive::workers::normalize_role(
                flag_value(args, "--role")?.unwrap_or("general"),
            );
            let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
            let root = crate::core::hive::root_cert::trust_root(&store, config, &instance.url)?;
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
        "rename" => {
            let (Some(id), Some(name)) = (args.get(1), args.get(2)) else {
                return Err(HiveError::Config(
                    "usage: lean-ctx hive worker rename <wkr_…> <name>".into(),
                ));
            };
            let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
            let root = crate::core::hive::root_cert::trust_root(&store, config, &instance.url)?;
            let session = Session::load(store, root)?;
            let renamed = session.with_client(|client| {
                crate::core::hive::workers::register::patch(client, id, Some(name), None)
            })?;
            crate::core::hive::workers::attach::update_book(
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
            let root = crate::core::hive::root_cert::trust_root(&store, config, &instance.url)?;
            let session = Session::load(store, root)?;
            let stopped = worker::stop(&session);
            // Hive ended them, or they end at their expiry when the stop did
            // not reach it: either way this machine stops renewing them.
            crate::core::hive::tasks::claims::clear(session.store.data_dir())?;
            stopped?;
            println!(
                "Worker mode stopped for this engine; its claims ended. The owner turns it on again on the web."
            );
        }
        "status" => {
            let local = worker::read_state(&store);
            let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
            let root = crate::core::hive::root_cert::trust_root(&store, config, &instance.url)?;
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
    use crate::core::hive::knowledge::code_graph;
    use crate::core::hive::vault::VaultMap;
    let store = HiveStore::open()?;
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let root = crate::core::hive::root_cert::trust_root(&store, config, &instance.url)?;
    let session = Session::load(store, root)?;
    let cwd = std::env::current_dir().map_err(|e| HiveError::Local(e.to_string()))?;
    let project_root = crate::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let record = VaultMap::load(session.store.data_dir())
        .for_root(&project_root)
        .cloned()
        .ok_or_else(|| {
            HiveError::Config(
                "this repository is not linked to a vault (run `lean-ctx hive sync` in it)".into(),
            )
        })?;
    let sent = code_graph::publish(&session, &record, std::path::Path::new(&project_root))?;
    println!(
        "Code graph of {}/{} sent{}: {} nodes, {} edges{}.",
        record.ns,
        record.name,
        if sent.sealed { " sealed" } else { "" },
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
