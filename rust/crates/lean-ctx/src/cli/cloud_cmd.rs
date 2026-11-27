// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx cloud connect|status|disconnect|link|worker`.

use lean_ctx_time::UtcDateTime;

use lean_ctx_cloud::auth::Session;
use lean_ctx_cloud::status::{CloudStatusReport, report};
use lean_ctx_cloud::{CloudConfig, CloudConfigExt as _, CloudError, CloudStore, link};
use lean_ctx_protocol::cloud::lease::LeaseState;

const HELP: &str = "\
lean-ctx cloud — share what your agents learn with your team in LeanCTX Cloud

START HERE:
  lean-ctx cloud connect      Connect this machine and this project: sign in in the
                              browser, choose the workspace, approve. Run it again
                              in any other project. [--label <name>] [--no-browser]
                              [--url <origin>]
                              Unattended (MDM, servers): a team admin's one-time
                              enrollment token joins without a browser, read from
                              one line of stdin with --enrollment-token-stdin or
                              from LEAN_CTX_CLOUD_ENROLLMENT_TOKEN (never as an
                              argument).
                              LeanCTX Code only (needs its launch key in
                              LEAN_CTX_APP_KEY, else app_only): --print-instance-key
                              prints this computer's public key; --chat-key - reads
                              the app's chat key offer from stdin and binds it.
  lean-ctx cloud connect --trial [--plan personal|team] [--yes] [--json]
                              Start the plan's one-step trial (default personal)
                              while connecting: the browser signs you in and starts
                              it. Once per address; afterwards choose a plan.
                              A used trial ends as trial_used only with a Cloud that
                              answers it at /device/token; an older Cloud shows it in
                              the browser and connect ends with expired or denied.
                              Ignored on a machine that is already connected.
                              --json (also without --trial) prints one JSON event
                              per line and asks nothing:
                                {\"event\":\"code\",\"verification_uri\",
                                 \"verification_uri_complete\",\"user_code\",\"expires_in\"}
                                {\"event\":\"connected\",\"plan\",\"trial_ends_at\"}
                                {\"event\":\"error\",\"code\",\"message\"}  (exit 1)
                              Error codes: trial_used, trial_unsupported, expired,
                              denied, offline, plan_required, instance_limit, …
  lean-ctx cloud status      Is this machine connected, and is this project shared?
                              Shows when a running trial ends.
  lean-ctx cloud disconnect   Remove this machine from the workspace and free its
                              slot (--keep-local keeps the local copies).
  lean-ctx cloud stop --reason <text> [--json]
                              Stop the team (contributor and up): LeanCTX Cloud
                              refuses its cloud calls, landings and running Code
                              sessions. Local agent tools keep working.
  lean-ctx cloud stop --release --confirm <team-slug> [--note <text>] [--json]
                              Release the stop (team or company admin), at once
                              or on the page it prints (see `cloud stop --help`).

EVERYDAY (LeanCTX does these in the background; run them to do it now):
  lean-ctx cloud sync         Send what your agents learned and get the team's.
  lean-ctx cloud consent      Answer a question lean-ctx asked you (agents cannot):
                              list | approve <id> | deny <id> | --web <id>.
  lean-ctx cloud link <workspace>/<vault>
                              Use a specific vault for this project instead of the
                              one connect chose.
  lean-ctx cloud unlink [--json] [--project-root <p>]
                              Stop sharing this project with its vault on this
                              machine. The vault and its data stay in Cloud.

MORE:
  lean-ctx cloud help advanced  Review, maintenance, worker mode, tasks, the
                                agent bus and the Intelligence runtime.

Plans: Personal up to 3 hosts registered, up to 2 connected at once; Team and
Enterprise up to 3 hosts registered, up to 2 connected at once per seat.
Local tools never depend on LeanCTX Cloud: offline or a lapsed lease only pause sync.
";

/// `lean-ctx cloud help advanced`, and `--help` of the commands it lists.
const ADVANCED_HELP: &str = "\
lean-ctx cloud — advanced commands

USAGE:
  lean-ctx cloud metrics [preview [--json] | hours --json --range 24h|7d|30d | confirm | delete]
  lean-ctx cloud capture [--recapture]
  lean-ctx cloud replica
  lean-ctx cloud maintain <workspace>/<vault> [--stale] [--contradictions] [--dedupe] [--limit N]
  lean-ctx cloud maintain <workspace>/<vault> --stale --confirm <node_id>
  lean-ctx cloud runtime install [--yes]
  lean-ctx cloud bus relay [--for <seconds>]
  lean-ctx cloud worker [status|pause|resume|stop|list|rename <wkr> <name>|register --cli <host> [--role <role>]|retire <wkr> [--force]|retire --idle <dur>]
  lean-ctx cloud graph publish
  lean-ctx cloud decisions list --json [--project-root <p>]
  lean-ctx cloud decisions push [--project-root <p>] [--dry-run] [--allow-empty] [--json]
  lean-ctx cloud members list --json
  lean-ctx cloud tasks [list|show|claim|update|submit|release|propose] …
  lean-ctx cloud status [--json [--contract local-read/1]]
  lean-ctx cloud sync status --json
  lean-ctx cloud policy sessions --json | session --session <id> --json
  lean-ctx cloud presence --json
  lean-ctx cloud docs list|show <id> --json [--project-root <p>]
  lean-ctx cloud tasks list|show <n> --json
  lean-ctx cloud vaults --json [--project-root <p>]
  lean-ctx cloud usage --json
  lean-ctx cloud stop --status --json
  lean-ctx cloud link --vault <workspace>/<vault> --json [--project-root <p>] [--dry-run]
  lean-ctx cloud computer list|create|start|stop|add|show|remove|ssh-config|ssh|deploy-key …

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
replica     The local replica of this repository's vault (LeanCTX Cloud
            servers with C0 2.0 only): place, cursor, live nodes and the
            graph digest two instances that folded the same log agree on.
            Read only.
maintain    Keep a vault healthy from this checkout (a LeanCTX Cloud server
            with C0 2.0, writer+): --stale re-checks every anchored claim
            against the code here and marks changed ones to_verify, missing
            ones gone (no model, no cost); --confirm <node_id> re-anchors a
            claim you checked at the current code. --contradictions and
            --dedupe run in the vault's review loop. Prints counts, never
            claim texts.
runtime    install: with your consent, install the proprietary LeanCTX
            Intelligence runtime your plan licenses (signed channel); it
            enforces your workspace's policy. --yes consents without asking.
bus relay   Join this machine's agent bus to the workspace's (the daemon
            runs the same relay in the background; --for limits the run).
graph      publish: send this repository's code graph (files, symbols, imports;
            never code text) to LeanCTX Cloud, when the vault owner turned on the
            full graph on the web.
worker      Worker mode: your agents pull tasks you opted in for on the web,
            within its daily budget ([cloud.worker] can only lower it).
            status shows the opt-in and today's use (--json: the opted-in vault ids too); pause/resume stop and
            restart claiming on this machine; stop switches worker mode off
            for this instance and ends its claims (the owner turns it on again
            on the web). list shows this instance's durable workers (wkr_ ids);
            rename gives one a name of 1-64 letters, digits, spaces and
            . _ · - from one alphabet. You use your own subscription; follow your provider's
            terms (OpenCode or Pi with a Claude Pro/Max login and third-party
            tools with a Gemini CLI login are not allowed by the providers).
tasks       The vault's tasks: read them, work one under a claim (worker
            mode), report, submit a result with evidence, or propose one
            (`lean-ctx cloud tasks --help`).
computer    Your LeanCTX computers: list, create, start and stop them, and
            reach them over SSH with pinned host keys
            (`lean-ctx cloud computer --help`).
status      --json prints the cockpit shape; --json --contract local-read/1
            adds the Local Read v1 fields (plan, lease, rights, capture).
presence    --json: the workers of other machines on this machine's vault
            buses, as the relay last recorded them.
usage       --json: how far each LeanCTX Cloud quota of your plan is used (asked
            only when the server offers it; otherwise \"available\": false),
            or the last answer with \"live\": false when it does not answer.
--json      on presence, docs list|show, tasks list|show, vaults, usage and link
            (and status with --contract local-read/1): one JSON object with
            \"contract\": \"local-read/1\" (Local Read v1,
            docs/contracts/local-read-v1.md), for local readers such as
            LeanCTX Code. Read only (docs come from the local replica), except
            link, which writes the link file and shares the project at once
            under the usual consent rules (--dry-run writes nothing).
";

// Declared here, not in cli/mod.rs (frozen for the crate split).
#[path = "cloud_docs_cmd.rs"]
mod cloud_docs_cmd;

/// `lean-ctx cloud docs import …` for the checkout of the current directory.
fn docs(rest: &[String]) -> Result<(), CloudError> {
    let cwd = std::env::current_dir().map_err(|e| CloudError::Local(e.to_string()))?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    for line in cloud_docs_cmd::run(rest, &root)? {
        println!("{line}");
    }
    Ok(())
}

#[path = "cloud_computer_cmd.rs"]
pub(super) mod cloud_computer_cmd;

/// `cloud.computers` and `cloud.computer.ack` for the Local Service socket
/// (LeanCTX Code): `args` = `<verb> [<id>] --json …`, as the route built them.
pub(crate) fn computer_answer(
    args: &[String],
    via: super::local_read::Via,
) -> super::local_read::Answer {
    cloud_computer_cmd::answer(args, via)
}

pub(crate) fn cmd_cloud(args: &[String]) {
    let sub = args.first().map(String::as_str).unwrap_or("status");
    let rest = args.get(1..).unwrap_or(&[]);
    // `cloud computer` has its own arguments and help (arguments after `--`
    // belong to the remote command of `ssh`).
    if sub == "computer" {
        if let Err(error) = cloud_computer_cmd::run(rest) {
            eprintln!("cloud computer: {error}");
            std::process::exit(1);
        }
        return;
    }
    if sub == "tasks"
        && (rest.first().map(String::as_str) == Some("help")
            || rest.iter().any(|a| a == "--help" || a == "-h"))
    {
        print!("{}", super::cloud_tasks_cmd::HELP);
        return;
    }
    if sub == "consent"
        && (rest.first().map(String::as_str) == Some("help")
            || rest.iter().any(|a| a == "--help" || a == "-h"))
    {
        print!("{}", super::cloud_consent_cmd::HELP);
        return;
    }
    if sub == "docs"
        && (rest.first().map(String::as_str) == Some("help")
            || rest.iter().any(|a| a == "--help" || a == "-h"))
    {
        println!(
            "lean-ctx cloud docs: this checkout's docs as pages of its LeanCTX Cloud vault\n\nusage:\n  {}",
            cloud_docs_cmd::USAGE
        );
        return;
    }
    if matches!(sub, "help" | "--help" | "-h") || rest.iter().any(|a| a == "--help" || a == "-h") {
        print!("{}", help_for(sub, rest));
        return;
    }
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    if !connector_open(&config, sub, rest) {
        eprintln!("{}", lean_ctx_cloud::NOT_AVAILABLE);
        std::process::exit(1);
    }
    // rc4-16: `cloud worker on` forwards a turn-on LeanCTX Code signed with
    // its opt-in key (stdin); nothing here can sign one.
    if sub == "worker" && super::local_read::positionals(rest).first() == Some(&"on") {
        std::process::exit(super::local_read::pickup_part::run_worker_on(rest));
    }
    // Local Read v1: the reads a local reader (Cloud Desktop) makes, after
    // the same release gate as every other cloud command.
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
            "unlink" => {
                super::local_read::unlink_part::run(rest);
                return;
            }
            "usage" => return super::local_read::cloud_usage::run(&config),
            "gateway" if super::local_read::positionals(rest).first() == Some(&"key") => {
                return super::local_read::cloud_gateway::run(&config);
            }
            "worker" if super::local_read::positionals(rest).first() == Some(&"list") => {
                return super::local_read::engine_files::run_workers(rest);
            }
            "cpao" => return super::local_read::cloud_cpao::run(&config),
            "exchange" => return super::local_read::cloud_exchange::run(rest),
            _ => {}
        }
    }
    if sub == "decisions" {
        return super::cloud_decisions_cmd::run(rest);
    }
    // `cloud sync status` and `cloud policy …` read local files only, as JSON;
    // a plain `cloud sync status` never falls through to a manual sync.
    // `cloud usage` and `cloud gateway key` are for local readers; people
    // see usage in `cloud status`.
    if (sub == "sync" && reads_status(rest))
        || sub == "policy"
        || sub == "usage"
        || sub == "gateway"
    {
        eprintln!("cloud {sub}: only --json is supported");
        std::process::exit(2);
    }
    let result = match sub {
        "connect" => super::cloud_connect_cmd::run(&config, rest),
        "status" => status(&config, rest),
        "disconnect" => disconnect(&config, rest),
        "sync" => sync(&config),
        "metrics" => metrics(&config, rest),
        "capture" => capture(&config, rest),
        "replica" => replica_status(),
        "cpao" => super::local_read::cloud_cpao::run_human(&config),
        "exchange" => super::local_read::cloud_exchange::run_human(rest),
        "maintain" => maintain(&config, rest),
        "bus" => bus(&config, rest),
        "link" => link_project(rest),
        "unlink" => {
            super::local_read::unlink_part::run_human(rest);
            Ok(())
        }
        "runtime" if rest.first().map(String::as_str) == Some("install") => {
            super::cloud_connect_cmd::install_runtime()
        }
        "worker" => worker(&config, rest),
        "graph" if rest.first().map(String::as_str) == Some("publish") => publish_graph(&config),
        "tasks" => super::cloud_tasks_cmd::run(rest),
        "docs" => docs(rest),
        "presence" => {
            eprintln!("cloud presence: only --json is supported (`ctx_agent list` shows the rows)");
            std::process::exit(2);
        }
        "vaults" => {
            eprintln!(
                "cloud vaults: only --json is supported (`lean-ctx cloud status` names the workspace)"
            );
            std::process::exit(2);
        }
        "consent" => super::cloud_consent_cmd::run(&config, rest),
        other => {
            eprintln!("cloud: unknown subcommand '{other}'\n");
            eprint!("{HELP}");
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        eprintln!("cloud: {error}");
        std::process::exit(1);
    }
}

/// `cloud sync status`: the background sync's state, not a sync.
fn reads_status(rest: &[String]) -> bool {
    super::local_read::positionals(rest).first().copied() == Some("status")
}

/// The release switch ([`lean_ctx_cloud::CLOUD_AVAILABLE`]) for the two
/// commands that start a Cloud relationship, decided before any store or
/// network access. Everything else keeps its "not connected" answers.
fn connector_open(config: &CloudConfig, sub: &str, rest: &[String]) -> bool {
    if lean_ctx_cloud::CLOUD_AVAILABLE {
        return true;
    }
    match sub {
        "connect" => {
            let origin = match flag_value(rest, "--url") {
                Ok(Some(url)) => lean_ctx_cloud::config::normalize_origin(url).ok(),
                Ok(None) => config.origin().ok(),
                Err(_) => None,
            };
            origin.is_some_and(|origin| lean_ctx_cloud::available_for(&origin))
        }
        "link" => lean_ctx_cloud::available_locally(),
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
        "stop" => STOP_HELP,
        _ => HELP,
    }
}

/// `lean-ctx cloud stop --help` (rc4-88): the team stop on its own.
const STOP_HELP: &str = "\
lean-ctx cloud stop — stop or release your team

USAGE:
  lean-ctx cloud stop --reason <text> [--json]
      Stop the team (contributor and up). Your team's cloud then holds its
      cloud requests, landings and running Code sessions until the stop is
      released. Local agent tools keep working.
  lean-ctx cloud stop --release --confirm <team-slug> [--note <text>] [--json]
      Release the stop (team or company admin). Released at once, or prints
      the page where you confirm it in the browser. --confirm names the team,
      so a release never hits the wrong one.
  lean-ctx cloud stop --status --json
      Whether the team is stopped now, by whom and since when, and whether
      you may stop or release it. Asks the cloud each time.

`lean-ctx cloud status` shows the last known stop without asking the cloud.
";

pub(super) fn flag_value<'a>(
    args: &'a [String],
    name: &str,
) -> Result<Option<&'a str>, CloudError> {
    match args.iter().position(|a| a == name) {
        None => Ok(None),
        Some(i) => args
            .get(i + 1)
            .map(|v| Some(v.as_str()))
            .ok_or_else(|| CloudError::Config(format!("{name} needs a value"))),
    }
}

/// The plan as people read it (not its wire name: a workspace without a
/// paid plan is `none` on the wire and "No plan" to people).
pub(super) fn plan_name(plan: &lean_ctx_protocol::cloud::lease::CloudPlan) -> String {
    plan.product_name().to_string()
}

fn status(config: &CloudConfig, args: &[String]) -> Result<(), CloudError> {
    let store = CloudStore::open()?;
    let current = report(&store, config, UtcDateTime::now());
    if args.iter().any(|a| a == "--json") {
        // Local Read v1 only on explicit opt-in: plain `--json` stays the
        // cockpit shape, which its readers decode strictly.
        if super::local_read::status::opted_in(args) {
            super::local_read::status::print(&current, store.data_dir(), config.auto_capture, args);
            return Ok(());
        }
        let mut status = current.cli_status();
        status.vault_mismatch = stale_here(&store).map(|stale| stale.wire());
        super::local_read::team_stop_part::fill_status(&mut status, &store, config);
        let json = serde_json::to_string(&status).map_err(|e| CloudError::Local(e.to_string()))?;
        println!("{json}");
        return Ok(());
    }
    print_report(&current);
    if current.connected()
        && let Some(line) = super::local_read::team_stop_part::status_line(&store, config)
    {
        println!("{line}");
    }
    if lean_ctx_cloud::disconnect::pending(&store).is_some() {
        println!(
            "Disconnect:   pending (LeanCTX Cloud was not reachable); retried in the background, or run `lean-ctx cloud disconnect` again"
        );
    }
    // Release B: quota and policy throttles in force (what waits, until when).
    for line in lean_ctx_cloud::throttle::status_lines(&store, UtcDateTime::now()) {
        println!("{line}");
    }
    // LeanCTX Cloud usage per quota, only from a server that serves it.
    for line in lean_ctx_cloud::usage::status(&store, config, UtcDateTime::now()) {
        println!("{line}");
    }
    // §3.16: the upgrade duties (local; once) and their counts, printed once.
    if current.instance.is_some() {
        let root = std::env::current_dir().ok().map(|cwd| {
            std::path::PathBuf::from(lean_ctx::core::protocol::detect_project_root_or_cwd(
                &cwd.to_string_lossy(),
            ))
        });
        for line in lean_ctx_cloud::effective_policy::status_lines(
            store.data_dir(),
            &current.policy,
            UtcDateTime::now(),
            root.as_deref(),
        ) {
            println!("{line}");
        }
        if let Err(error) = lean_ctx_cloud::migrate::run(&store, config) {
            println!("Upgrade:      {error}");
        }
        for line in lean_ctx_cloud::migrate::status_lines_once(&store) {
            println!("{line}");
        }
    }
    Ok(())
}

/// The stale vault resolution of the project here, if any (local files).
fn stale_here(store: &CloudStore) -> Option<lean_ctx_cloud::resolution::Stale> {
    let cwd = std::env::current_dir().ok()?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let map = lean_ctx_cloud::vault::VaultMap::load(store.data_dir());
    lean_ctx_cloud::resolution::stale_for_root(&map, std::path::Path::new(&root))
}

/// Whether the project here is shared, in one line: its vault, a first
/// share that waits for the person, or not set up yet. `None` outside a
/// project LeanCTX can identify.
fn project_line() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let store = CloudStore::open().ok()?;
    project_line_in(&store, std::path::Path::new(&root))
}

/// [`project_line`] for the checkout at `root` with `store`'s records.
fn project_line_in(store: &CloudStore, root: &std::path::Path) -> Option<String> {
    lean_ctx_cloud::vault::project_identity(root)?;
    let name = root.file_name()?.to_string_lossy().into_owned();
    // A damaged opt-out list holds every folder back (bd 39936): say so.
    if let Some(damage) = lean_ctx_cloud::unlinked::damage(store.data_dir()) {
        return Some(format!("{name} → not shared: {damage}"));
    }
    let map = lean_ctx_cloud::vault::VaultMap::load(store.data_dir());
    // Dogfood 5.11.: a resolution the link file or identity no longer
    // backs is never shown as "shared" (connect and sync drop it).
    if let Some(stale) = lean_ctx_cloud::resolution::stale_for_root(&map, root) {
        return Some(format!(
            "{name} → MISMATCH: {}. That vault is not used; run `lean-ctx cloud connect` here to resolve the project again",
            stale.describe()
        ));
    }
    if let Some(record) =
        lean_ctx_cloud::unlinked::shared_record(store.data_dir(), &map, &root.to_string_lossy())
    {
        return Some(format!("{name} → {}/{} (shared)", record.ns, record.name));
    }
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    let broker = lean_ctx_cloud::consent::Broker::open(
        store,
        lean_ctx_cloud::consent::Trust::load(&store, &config),
    );
    let now = u64::try_from(UtcDateTime::now().timestamp_millis()).unwrap_or_default();
    let waiting = broker.list().ok()?.into_iter().any(|l| {
        l.valid
            && l.record.kind == lean_ctx_cloud::consent::Kind::LinkVault
            && l.record.state == lean_ctx_cloud::consent::State::Pending
            && l.record.expires_at > now
            && l.record.subject["bound_repo"].as_str() == Some(name.as_str())
            && !lean_ctx_cloud::mapping::satisfied(&l.record, &map)
    });
    let linked = link::read_link(root)
        .ok()
        .flatten()
        .map(|linked| linked.address().to_string());
    Some(unshared_line(&name, linked.as_deref(), waiting))
}

/// The status line of a project this connected machine has not shared yet.
/// A linked project needs no second connect: `cloud sync` here shares it
/// (and asks for the yes a first share needs).
fn unshared_line(name: &str, linked: Option<&str>, waiting: bool) -> String {
    match (linked, waiting) {
        (Some(vault), true) => format!(
            "{name} → linked to {vault}, not shared yet: waiting for your yes (run `lean-ctx cloud sync` here, in your own terminal)"
        ),
        (Some(vault), false) => {
            format!("{name} → linked to {vault}, not shared yet (run `lean-ctx cloud sync` here)")
        }
        (None, true) => format!(
            "{name} → not shared yet: waiting for your yes (run `lean-ctx cloud connect` here, in your own terminal)"
        ),
        (None, false) => format!("{name} → not shared yet (run `lean-ctx cloud connect` here)"),
    }
}

/// `Managed:` lines: the carrier, then each member the organisation set.
fn print_managed(current: &CloudStatusReport) {
    let Some(managed) = &current.managed else {
        return;
    };
    use lean_ctx_protocol::cloud::managed::ManagedCarrierKindV1 as Kind;
    let carrier = match managed.carrier {
        Kind::MacosProfile => "macOS configuration profile",
        Kind::WindowsPolicy => "Windows policy",
        Kind::LinuxFile => "managed file",
        Kind::Unknown => "carrier",
    };
    println!("Managed:      yes, {carrier} {}", managed.location);
    if let Some(error) = &managed.error {
        println!("              refused: {error} (LeanCTX Cloud and tools stay off)");
    }
    for member in &managed.members {
        println!("              {} = {}", member.member, member.value);
    }
}

fn print_report(current: &CloudStatusReport) {
    print_managed(current);
    let Some(instance) = &current.instance else {
        if lean_ctx_cloud::CLOUD_AVAILABLE {
            println!(
                "LeanCTX Cloud: not connected ({}). Run `lean-ctx cloud connect`.",
                current.url
            );
        } else {
            println!(
                "LeanCTX Cloud: not connected. {}",
                lean_ctx_cloud::NOT_AVAILABLE
            );
        }
        return;
    };
    println!("Cloud:        {}", current.url);
    if current.unvouched_root {
        println!("Root:         development (`[cloud] root_key` in your config)");
    }
    println!("Workspace:    {}", instance.workspace_label());
    println!(
        "Instance:     {} ({})",
        instance.label, instance.instance_id
    );
    if let Some(line) = project_line() {
        println!("Project:      {line}");
    }
    let cap = lean_ctx_cloud::workers::attach::WORKER_CAP;
    if current.workers >= cap || current.workers_retiring > 0 {
        let retiring = match current.workers_retiring {
            0 => String::new(),
            n => format!("; {n} given up, waiting to be retired on LeanCTX Cloud"),
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
            LeaseState::Lapsed => "lapsed: run `lean-ctx cloud connect`".to_string(),
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
                "Plan:         {} · up to {} {} connected at once",
                plan_name(&lease.plan),
                lease.seats_limit,
                if lease.seats_limit == 1 {
                    "host"
                } else {
                    "hosts"
                }
            ),
        }
        println!("Lease:        {state}");
        if let Some(trial) = lean_ctx_cloud::status::trial_line(lease) {
            println!("Trial:        {trial}");
        }
        if let Some(reason) = &current.activity.renew_error {
            println!("              last renewal failed: {reason} (tried again every pass)");
        }
        println!("Rights:       {}", rights_line(&lease.rights));
    }
    if !current.enabled {
        println!("Sync:         paused ([cloud] enabled = false)");
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
    use lean_ctx_protocol::cloud::status::{CloudBusControlActionV1, CloudBusControlsInactiveV1};
    for control in &current.bus_controls {
        let action = match control.action {
            CloudBusControlActionV1::Hold => "hold",
            CloudBusControlActionV1::Stop => "stop",
            CloudBusControlActionV1::Veto => "veto",
            CloudBusControlActionV1::Unknown => "control",
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
        Some(CloudBusControlsInactiveV1::LeaseLapsed) => {
            println!("Bus controls: inactive (lease lapsed)");
        }
        Some(CloudBusControlsInactiveV1::NotEnrolled) => {
            println!("Bus controls: inactive (not enrolled)");
        }
        Some(CloudBusControlsInactiveV1::Unknown) => println!("Bus controls: inactive"),
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
    if let Ok(store) = CloudStore::open() {
        for (place, counts) in lean_ctx_cloud::capture::load_counts(store.data_dir()) {
            println!("Capture gate: {place}: {}", counts.summary());
        }
    }
}

/// What a revoke frees, as the web's Hosts page says it (rc3-81): the
/// connection at once; the registration at once on Team and Enterprise, in
/// Personal after the cool-down, a recovery, or a pairing that replaces it.
const DISCONNECT_FREES: &str = "Its connection is free now. On Team and Enterprise its registration is free now too; on Personal it stays registered for 7 days, unless you recover its slot on the Hosts page or connect the same computer again in its place.";

fn disconnect(config: &CloudConfig, args: &[String]) -> Result<(), CloudError> {
    let keep_local = args.iter().any(|a| a == "--keep-local");
    let store = CloudStore::open()?;
    // The account-scope check may be unable to remove another account's
    // pulled knowledge (a damaged store) and keeps every reader out; the
    // disconnect still ends the stored enrolment, cleans what it can and
    // names the rest, so there is always a way out.
    let (instance, scope_failed) = match store.instance() {
        Ok(instance) => (instance, false),
        Err(_) => (store.enrolled_instance()?, true),
    };
    let Some(instance) = instance else {
        // A connect that failed after enrolment may have left its key
        // (Cloud task #1); Cloud refuses that key a second time.
        if lean_ctx_cloud::instance_key::InstanceKeys::load(&store).is_ok() {
            store.forget(keep_local)?;
            println!(
                "Not connected to LeanCTX Cloud. Removed the instance key an unfinished connect left behind."
            );
        } else {
            println!("Not connected to LeanCTX Cloud.");
        }
        return Ok(());
    };
    if scope_failed {
        // Cloud cannot be asked: every session passes the check that just
        // failed. The local side goes now, the seat is freed by hand.
        let seat = format!(
            "LeanCTX Cloud could not be asked to end this instance, so remove the instance \"{}\" in LeanCTX Cloud to free its seat.",
            instance.label
        );
        return match store.forget(keep_local) {
            Ok(()) => {
                println!("Disconnected {} on this computer. {seat}", instance.label);
                Ok(())
            }
            Err(CloudError::Local(failed)) => Err(CloudError::Local(format!("{failed} {seat}"))),
            Err(other) => Err(other),
        };
    }
    // Cloud first; the local data goes only once Cloud ended the instance.
    use lean_ctx_cloud::disconnect::{self, Told};
    match disconnect::tell_cloud(&store, config, &instance) {
        Told::Revoked => println!(
            "Disconnected {} from {}: LeanCTX Cloud revoked this host. {DISCONNECT_FREES}",
            instance.label, instance.org_name
        ),
        Told::AlreadyGone => println!(
            "Disconnected {} from {}: LeanCTX Cloud had already revoked this host.",
            instance.label, instance.org_name
        ),
        Told::Unreachable(error) => {
            disconnect::mark_pending(&store, keep_local)?;
            println!(
                "LeanCTX Cloud is not reachable ({error}). Nothing was removed: the disconnect is pending, \
                 lean-ctx retries it in the background (or run `lean-ctx cloud disconnect` again)."
            );
            return Ok(());
        }
        Told::Refused(error) if args.iter().any(|a| a == "--force") => println!(
            "Removed the local credentials, but LeanCTX Cloud did not end this host ({error}). Revoke the host \"{}\" on the Hosts page to free it.",
            instance.label
        ),
        Told::Refused(error) => {
            return Err(CloudError::Refused(format!(
                "LeanCTX Cloud did not end this host ({error}); nothing was removed. Revoke the host \"{}\" on the Hosts page, or run `lean-ctx cloud disconnect --force` to remove the local credentials anyway.",
                instance.label
            )));
        }
    }
    store.forget(keep_local)?;
    if keep_local {
        println!(
            "Removed the knowledge pulled from LeanCTX Cloud; the other local LeanCTX Cloud data (drafts, policy) stays on this machine for a reconnect to the same team."
        );
    } else {
        println!("Removed the local LeanCTX Cloud data and the knowledge pulled from it.");
    }
    Ok(())
}

/// `cloud maintain` in host mode (SPEC-ENGINE §3.15b, V9.4).
fn maintain(config: &CloudConfig, args: &[String]) -> Result<(), CloudError> {
    use lean_ctx_cloud::review::maintain;
    use lean_ctx_cloud::sync::writer;
    let req = maintain::parse(args)?;
    let store = CloudStore::open()?;
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    let (ns, name) = req
        .vault
        .split_once('/')
        .ok_or_else(|| CloudError::Config("name the vault as <workspace>/<vault>".into()))?;
    let cwd = std::env::current_dir().map_err(|e| CloudError::Local(e.to_string()))?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let map = lean_ctx_cloud::vault::VaultMap::load(store.data_dir());
    let record = lean_ctx_cloud::resolution::vault_in(&map, std::path::Path::new(&root))?
        .filter(|r| r.ns == ns && r.name == name)
        .cloned()
        .ok_or_else(|| {
            CloudError::Config(format!(
                "run it in a checkout linked to {ns}/{name} (`lean-ctx cloud link {ns}/{name}`)"
            ))
        })?;
    let dir = lean_ctx_cloud::replica::store::place_dir(store.data_dir(), &record.vault_id);
    if !dir.join("replica.db").exists() {
        println!("No replica for {ns}/{name} yet: run `lean-ctx cloud sync` first.");
        return Ok(());
    }
    let trust = lean_ctx_cloud::root_cert::trust_root(&store, config, &instance.url)?;
    let session = Session::load(store.clone(), trust)?;
    let (mk, _) = lean_ctx_cloud::replica::crypt::master_key(store.config_dir(), false)?;
    let (replica, _) =
        lean_ctx_cloud::replica::Replica::open(store.data_dir(), &record.vault_id, &mk)?;
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
    let api = lean_ctx_cloud::sync::place::SessionApi(&session);
    let place_ref = writer::vault_ref(&record);
    let mut checkout = lean_ctx_cloud::capture::refs::Checkout::new(std::path::Path::new(&root));
    let (_, lines) = maintain::run_host(
        &target,
        &mut checkout,
        &req,
        Some((&api, &place_ref)),
        UtcDateTime::now().timestamp_millis().unsigned_abs(),
    )?;
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

fn replica_status() -> Result<(), CloudError> {
    let store = CloudStore::open()?;
    let cwd = std::env::current_dir().map_err(|e| CloudError::Local(e.to_string()))?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let identity = lean_ctx_cloud::vault::project_identity(std::path::Path::new(&root))
        .ok_or_else(|| CloudError::Config("not inside a git repository".into()))?;
    let stable = lean_ctx_protocol::cloud::vault::stable_identity(&identity);
    let record = lean_ctx_cloud::vault::VaultMap::load(store.data_dir())
        .vaults
        .get(&stable)
        .cloned()
        .ok_or_else(|| CloudError::Config("this repository is not linked to a vault".into()))?;
    let dir = lean_ctx_cloud::replica::store::place_dir(store.data_dir(), &record.vault_id);
    if !dir.join("replica.db").exists() {
        println!(
            "No replica for @{}/{} yet (`lean-ctx cloud sync` clones it from a LeanCTX Cloud server that speaks C0 2.0).",
            record.ns, record.name
        );
        return Ok(());
    }
    let (mk, at_rest) = lean_ctx_cloud::replica::crypt::master_key(store.config_dir(), false)?;
    let (replica, _) =
        lean_ctx_cloud::replica::Replica::open(store.data_dir(), &record.vault_id, &mk)?;
    let mut graph = lean_ctx_cloud::replica::fold::Graph::default();
    let events = replica.fold_events()?;
    graph
        .fold(&events)
        .map_err(|e| CloudError::Protocol(e.to_string()))?;
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

fn capture(config: &CloudConfig, args: &[String]) -> Result<(), CloudError> {
    use lean_ctx_cloud::knowledge::draft;
    let store = CloudStore::open()?;
    store.instance()?.ok_or(CloudError::NotConnected)?;
    let cwd = std::env::current_dir().map_err(|e| CloudError::Local(e.to_string()))?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let root = std::path::Path::new(&root);
    let mut screen = lean_ctx_cloud::knowledge::screening::Screen::for_installation();
    if lean_ctx_cloud::vault::project_identity(root)
        .map(|i| lean_ctx_protocol::cloud::vault::stable_identity(&i))
        .is_some_and(|stable| draft::syncs_through_replica(&store, &stable, root))
    {
        println!(
            "This vault syncs through its replica: `lean-ctx cloud sync` captures its knowledge as signed events; no draft is queued."
        );
    }
    if args.iter().any(|a| a == "--recapture") {
        let done = draft::recapture(&store, config, root, &mut screen, UtcDateTime::now())?;
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
            UtcDateTime::now(),
        )?;
        println!(
            "{}",
            if id.is_some() {
                "Queued a draft; `lean-ctx cloud sync` sends it."
            } else {
                "Nothing new passed the capture gate."
            }
        );
    }
    for (place, counts) in lean_ctx_cloud::capture::load_counts(store.data_dir()) {
        println!("{place}: {}", counts.summary());
    }
    Ok(())
}

/// `lean-ctx cloud metrics`: engine metrics v2. Never sends metrics.
fn metrics(config: &CloudConfig, args: &[String]) -> Result<(), CloudError> {
    use lean_ctx_cloud::metrics_v2::{self, gate, upload};
    let store = CloudStore::open()?;
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
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
    let print_json = |value: &dyn erased::Json| -> Result<(), CloudError> {
        println!("{}", value.pretty()?);
        Ok(())
    };
    let root = lean_ctx_cloud::root_cert::trust_root(&store, config, &instance.url)?;
    let session = Session::load(store.clone(), root)?;
    let lease = &session.lease.lease;
    if wants("delete") {
        upload::delete_local(&store);
        let remote = match upload::delete_remote(&session) {
            Ok(true) => {
                " LeanCTX Cloud deleted every hour it stored for this instance.".to_string()
            }
            Ok(false) => String::new(),
            Err(error) => {
                format!(
                    " LeanCTX Cloud could not delete its stored hours now ({error}); run it again."
                )
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
                "Usage metrics are not released yet: nothing is shared with {workspace}, so there is nothing to confirm."
            );
            return Ok(());
        }
        // Connect switches metrics on without asking (owner decision
        // 23.11.2026: no onboarding question, default on, off in Settings);
        // this command stays for a person, at a terminal, who switched them
        // off and wants them back: never an agent or a script (`--yes` does
        // not count here).
        if !lean_ctx::core::host_env::stdin_is_terminal()
            || !std::io::IsTerminal::is_terminal(&std::io::stdout())
        {
            return Err(CloudError::Refused(
                "usage metrics are confirmed by a person in a terminal: run `lean-ctx cloud metrics confirm` there".into(),
            ));
        }
        let sharing = gate::decide(&store, config, &instance, lease, UtcDateTime::now());
        if !sharing.waits_for_confirmation() {
            println!(
                "Nothing to confirm: {workspace} shares usage metrics by default (now: {}). Its admins control it with the policy rule \"Share instance metrics\".",
                sharing.now_words()
            );
            return Ok(());
        }
        println!(
            "{}",
            metrics_v2::disclosure(&workspace, &web_url, &sharing, true)
        );
        if super::prompt::confirm("Share usage metrics with this workspace?", false) {
            upload::confirm(&session)?;
            let now = gate::decide(&store, config, &instance, lease, UtcDateTime::now());
            println!("Usage metrics for {workspace}: {}.", now.describe());
        } else {
            println!("Nothing changed: usage metrics stay off for {workspace}.");
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
                return Err(CloudError::Config(format!(
                    "cloud metrics hours: --range is 24h, 7d or 30d, not {other}"
                )));
            }
        };
        let body = upload::hours(&store, config, &instance, lease, hours, UtcDateTime::now())?;
        return print_json(&body);
    }
    if wants("preview") || args.is_empty() || args.iter().all(|a| a == "--json") {
        let (sharing, hour, request) =
            upload::preview(&store, config, &instance, lease, UtcDateTime::now())?;
        if json_out {
            return print_json(&request);
        }
        println!(
            "{}",
            metrics_v2::disclosure(
                &workspace,
                &web_url,
                &sharing,
                sharing.waits_for_confirmation()
            )
        );
        println!("\nInstance metrics for {workspace}: {}", sharing.describe());
        if !upload::released(&instance.url) {
            println!("Not released yet: this instance sends nothing; below is what it would send.");
        }
        let shown = if request.buckets.is_empty() {
            upload::sample_hour(&store, config, &instance, lease, UtcDateTime::now())?
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
    Err(CloudError::Config(
        "cloud metrics: use preview [--json], hours --json --range 24h|7d|30d, confirm or delete"
            .into(),
    ))
}

mod erased {
    use lean_ctx_cloud::CloudError;

    /// Anything printed as pretty JSON.
    pub(super) trait Json {
        fn pretty(&self) -> Result<String, CloudError>;
    }

    impl<T: serde::Serialize> Json for T {
        fn pretty(&self) -> Result<String, CloudError> {
            serde_json::to_string_pretty(self).map_err(|e| CloudError::Local(e.to_string()))
        }
    }
}

/// The refusals of one `cloud sync`: a locked workspace is said once (E9),
/// and remembered for `cloud status`.
#[derive(Default)]
struct SyncSay {
    locked: Option<String>,
}

impl SyncSay {
    /// Print `line` on stderr, unless it repeats the locked refusal; then
    /// only that one line, once.
    fn refusal(&mut self, line: &str) {
        use lean_ctx_cloud::error::LOCKED_MARK;
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
        eprintln!("cloud: {sentence}");
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

fn sync(config: &CloudConfig) -> Result<(), CloudError> {
    let mut say = SyncSay::default();
    let store = CloudStore::open()?;
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    let root = lean_ctx_cloud::root_cert::trust_root(&store, config, &instance.url)?;
    let session = Session::load(store, root)?;
    let cwd = std::env::current_dir().map_err(|e| CloudError::Local(e.to_string()))?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let mut screen = lean_ctx_cloud::knowledge::screening::Screen::for_installation();
    // A C0 2.0 Cloud: the bound vault syncs through its replica (signed
    // events), and no 1.x draft is queued for it. A 1.x Cloud keeps the
    // draft path below unchanged.
    let bound = lean_ctx_cloud::vault::project_identity(std::path::Path::new(&root))
        .map(|i| lean_ctx_protocol::cloud::vault::stable_identity(&i))
        .and_then(|stable| {
            lean_ctx_cloud::vault::VaultMap::load(session.store.data_dir())
                .vaults
                .get(&stable)
                .cloned()
        });
    let v2 = lean_ctx_cloud::bus::relay_v2::speaks_v2(&session.store);
    // A folder the person stopped sharing (`cloud unlink`) syncs nothing,
    // also not through another checkout of the same repository (rc4-6).
    let unlinked =
        lean_ctx_cloud::unlinked::contains(session.store.data_dir(), std::path::Path::new(&root));
    let bound = bound.filter(|_| !unlinked);
    // The first sync of a project (after `cloud link`, or the first share
    // that maps it) has no record yet: resolve it now, so this sync already
    // clones the replica and doc writes find it.
    let bound = match bound {
        None if v2 && !unlinked => lean_ctx_cloud::knowledge::pull::resolve_project(
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
            let _lock = lean_ctx_cloud::sync::auto::lock_vault(
                session.store.data_dir(),
                &record.vault_id,
                true,
            )?;
            let synced = lean_ctx_cloud::sync::place::sync_vault(
                &session,
                record,
                std::path::Path::new(&root),
                &mut screen,
            );
            lean_ctx_cloud::sync::auto::note_manual(&session.store, record, synced.as_ref());
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
                        say.refusal(&format!("cloud: event {id} refused: {refusal}"));
                    }
                }
                Err(error) => {
                    say.refusal(&format!(
                        "cloud: vault @{}/{}: {error}",
                        record.ns, record.name
                    ));
                }
            }
        }
        _ => {
            // Queue this project's learnings first, so `sync` also covers the current session.
            lean_ctx_cloud::knowledge::draft::enqueue_session_draft(
                &session.store,
                config,
                std::path::Path::new(&root),
                "cli-sync",
                lean_ctx_cloud::knowledge::draft::Trigger::SessionEnd,
                &mut screen,
                UtcDateTime::now(),
            )?;
        }
    }
    let report =
        lean_ctx_cloud::knowledge::pull::sync(&session, config, Some(std::path::Path::new(&root)));
    if session
        .lease
        .lease
        .grants(lean_ctx_protocol::cloud::lease::CloudRight::PolicyPull)
    {
        match lean_ctx_cloud::policy::pull(&session).unwrap_or_else(|error| {
            lean_ctx_cloud::policy::PolicyPull::Refused(format!("not fetched: {error}"))
        }) {
            lean_ctx_cloud::policy::PolicyPull::Accepted(state) => {
                lean_ctx_cloud::policy::note_pull(
                    session.store.data_dir(),
                    lean_ctx_cloud::policy::PulledVia::Manual,
                );
                println!(
                    "Policy bundle verified ({} filter packs).",
                    state.filter_packs.len()
                );
            }
            lean_ctx_cloud::policy::PolicyPull::Refused(reason) => {
                say.refusal(&format!(
                    "cloud: policy bundle {}",
                    if reason.starts_with("not fetched") {
                        reason
                    } else {
                        format!("refused: {reason}")
                    }
                ));
            }
            lean_ctx_cloud::policy::PolicyPull::Unchanged => lean_ctx_cloud::policy::note_pull(
                session.store.data_dir(),
                lean_ctx_cloud::policy::PulledVia::Manual,
            ),
        }
    }
    for number in &report.drafts_sent {
        println!("Sent draft #{number}.");
    }
    for (repo, revision, loaded) in &report.pulled {
        println!("Pulled {repo} revision {revision} ({loaded} facts).");
    }
    let proposals = lean_ctx_cloud::tasks::propose::drain(&session);
    for (number, state) in &proposals.sent {
        println!(
            "{}",
            lean_ctx_cloud::tasks::propose::created_line(*number, *state)
        );
    }
    if proposals.pending > 0 || proposals.failed > 0 {
        println!(
            "{} task proposals still queued, {} refused.",
            proposals.pending, proposals.failed
        );
    }
    for problem in &proposals.problems {
        say.refusal(&format!("cloud: task proposal: {problem}"));
    }
    use lean_ctx_cloud::metrics_v2::upload::{self as hours, MetricsV2Run};
    match hours::run(&session, config, UtcDateTime::now(), true) {
        Ok(MetricsV2Run::Sent { buckets, refused }) => {
            println!("Sent instance metrics ({buckets} hours stored, {refused} slices refused).");
        }
        Ok(MetricsV2Run::Off(sharing)) => {
            println!("Instance metrics not sent: {}.", sharing.describe());
        }
        Ok(MetricsV2Run::Waiting) => {
            println!(
                "Instance metrics wait for LeanCTX Cloud (rate limit or outage); retried later."
            );
        }
        Ok(MetricsV2Run::NotReleased | MetricsV2Run::Empty | MetricsV2Run::NotDue) => {}
        Err(error) => say.refusal(&format!("cloud: instance metrics not sent: {error}")),
    }
    use lean_ctx_cloud::receipts::{self, ReceiptsRun};
    match receipts::run(&session, config, UtcDateTime::now()) {
        Ok(ReceiptsRun::Sent {
            accepted,
            duplicates,
            ..
        }) if accepted + duplicates > 0 => {
            println!("Sent receipts ({accepted} new, {duplicates} already in LeanCTX Cloud).");
        }
        Ok(_) => {}
        Err(error) => say.refusal(&format!("cloud: receipts not sent: {error}")),
    }
    // rc4-57: a refusal comes first and is never preceded by "Nothing new."
    for problem in &report.problems {
        say.refusal(&format!("cloud: {problem}"));
    }
    if report.drafts_sent.is_empty()
        && report.pulled.is_empty()
        && proposals.sent.is_empty()
        && report.problems.is_empty()
    {
        println!("Nothing new.");
    }
    if report.drafts_pending > 0 || report.drafts_failed > 0 {
        println!(
            "{} drafts still queued, {} refused (see `lean-ctx cloud status`).",
            report.drafts_pending, report.drafts_failed
        );
    }
    if let Some(reason) = &say.locked {
        session.store.record_locked(Some(reason));
    } else if report.problems.is_empty() {
        // Cloud served this sync: a plan was chosen meanwhile, the lock is over
        // (the lease that still names it may run for hours, E11).
        session.store.record_locked(None);
    }
    Ok(())
}

fn bus(config: &CloudConfig, args: &[String]) -> Result<(), CloudError> {
    if args.first().map(String::as_str) != Some("relay") {
        return Err(CloudError::Config(
            "usage: lean-ctx cloud bus relay [--for <seconds>]".into(),
        ));
    }
    let seconds = match flag_value(args, "--for")? {
        Some(value) => value
            .parse::<u64>()
            .map_err(|_| CloudError::Config("--for takes seconds".into()))?,
        None => u64::MAX / 4,
    };
    // The daemon runs the same relay in the background; this runs it in
    // the foreground (watch it, or relay without a daemon). Either way it
    // says what was sent, or why nothing was (#252).
    let report = lean_ctx_cloud::bus::Report::to(|line| println!("{line}"));
    if lean_ctx_cloud::bus::run_for(
        config.clone(),
        std::time::Duration::from_secs(seconds),
        report,
    )? {
        println!(
            "bus relay: stopped after {seconds} s. LeanCTX Cloud keeps a worker on the bus only while a relay is connected; the daemon's relay takes over when the daemon runs."
        );
        return Ok(());
    }
    println!(
        "This machine already runs the bus relay (normally the daemon's, in the background), so nothing needs to be done here; it was asked to announce this machine's agents (and their roles) now."
    );
    match lean_ctx_cloud::bus::presence::announce_and_wait(std::time::Duration::from_secs(
        ANNOUNCE_WAIT_SECS.min(seconds.max(1)),
    )) {
        Some(text) => print!("{text}"),
        None => println!(
            "bus relay: the running relay sent no fresh hello within {} s: it is not connected to LeanCTX Cloud right now (see the last error in `lean-ctx cloud status`).",
            ANNOUNCE_WAIT_SECS.min(seconds.max(1))
        ),
    }
    Ok(())
}

/// How long `cloud bus relay` waits for the running relay's fresh hello.
const ANNOUNCE_WAIT_SECS: u64 = 15;

fn link_project(args: &[String]) -> Result<(), CloudError> {
    let cwd = std::env::current_dir().map_err(|e| CloudError::Local(e.to_string()))?;
    let root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let root = std::path::Path::new(&root);
    let store = CloudStore::open()?;
    let instance = store.instance()?;
    let prior = link::prior_link(root);
    // An explicit link shares a folder `cloud unlink` stopped (rc4-6). Its
    // opt-out keeps every sender quiet through the link and the share and is
    // lifted only once the link is kept (bd 40060); a refused or failed link
    // keeps it, and a damaged list fails the link.
    let opted_out = lean_ctx_cloud::unlinked::before_link(root)?;
    // A refused link restores this machine's vault records as they were, and
    // so does a link that fails after its file is written (bd 40158).
    let records = (
        store.data_dir().to_path_buf(),
        lean_ctx_cloud::vault::VaultMap::load(store.data_dir()),
    );
    let (lines, outcome) = link_project_at(root, args, instance.as_ref())?;
    // From here every failure undoes the link: the folder is left as it was.
    let failed = |error: CloudError| {
        super::local_read::link::undo_failed_link(
            root,
            &outcome,
            prior.as_deref(),
            Some(&records),
            error,
        )
    };
    let kept = || -> Result<(), CloudError> {
        if opted_out {
            lean_ctx_cloud::unlinked::forget_on_link(root).map_err(failed)?;
        }
        Ok(())
    };
    let Some(instance) = instance else {
        kept()?;
        for line in lines {
            println!("{line}");
        }
        return Ok(());
    };
    // Dogfood 5.11.: after a connect this machine may have resolved the
    // project to another vault. That resolution is dropped, so neither
    // status nor sync keeps using it.
    let stale =
        lean_ctx_cloud::resolution::forget_stale_at(store.data_dir(), root).map_err(failed)?;
    // Team-knowledge e2e: a connected machine shares the linked
    // project right away, so decisions, tasks and docs work after `link`
    // without a `cloud sync` first (as `cloud link --json` does).
    let Some(linked) = link::read_link(root).map_err(failed)? else {
        return kept();
    };
    let address = linked.address();
    let share = super::local_read::link::share_now(&store, &instance, root, &address);
    // rc4-56: a vault Cloud refuses is never left linked.
    if share.unlinks() {
        link::undo(root, &outcome, prior.as_deref())?;
        if let Err(e) = records.1.save(store.data_dir()) {
            tracing::warn!("cloud link: vault records not restored: {e}");
        }
        let why = share.message.as_deref().and_then(|m| shown_text(m, 300));
        return Err(CloudError::Refused(format!(
            "not linked: {}",
            why.unwrap_or_else(|| format!("LeanCTX Cloud refused {address}"))
        )));
    }
    kept()?;
    for line in lines {
        println!("{line}");
    }
    for line in link_share_lines(
        stale
            .as_ref()
            .map(|s| (s.describe(), s.linked().map(str::to_owned))),
        &address.to_string(),
        &share,
    ) {
        println!("{line}");
    }
    Ok(())
}

/// What `cloud link` says after sharing the linked project: a dropped
/// stale resolution, then where sharing stands. `stale` is the dropped
/// resolution's description and the vault it was linked to.
fn link_share_lines(
    stale: Option<(String, Option<String>)>,
    vault: &str,
    share: &super::local_read::link::Share,
) -> Vec<String> {
    let shared = share.state == "shared";
    let mut lines = Vec::new();
    if let Some((describe, linked)) = stale {
        lines.push(if shared {
            format!("Before this link, {describe}. That resolution is dropped and never used again.")
        } else {
            format!(
                "Before this link, {describe}. That resolution is dropped and never used again: run `lean-ctx cloud connect` here to resolve the project to {} (sync does the same; both ask for your yes where a first share needs it).",
                linked.as_deref().unwrap_or("the linked vault")
            )
        });
    }
    let why = share
        .message
        .as_deref()
        .and_then(|m| shown_text(m, 300))
        .map(|m| format!(": {m}"))
        .unwrap_or_default();
    lines.push(match share.state {
        "shared" => format!(
            "Shared with {vault}: decisions, tasks and docs of this project work here now."
        ),
        "consent_required" => format!(
            "Not shared with {vault} yet: the first share waits for your yes. Run `{}` in your own terminal (or `{}`).",
            share
                .answer_terminal
                .as_deref()
                .and_then(|a| shown_text(a, 200))
                .unwrap_or_else(|| "lean-ctx cloud consent".to_owned()),
            share
                .answer_web
                .as_deref()
                .and_then(|a| shown_text(a, 200))
                .unwrap_or_else(|| "lean-ctx cloud consent --web".to_owned()),
        ),
        "declined" => format!("Not shared with {vault}: you said no to sharing it."),
        _ => format!(
            "Not shared with {vault} yet{why}. Run `lean-ctx cloud sync` here to share it."
        ),
    });
    lines
}

/// Text the Cloud answered, as the terminal may show it: control and
/// bidirectional formatting characters become spaces (they could rewrite or
/// reorder what the person sees), runs of spaces collapse, and at most `max`
/// characters remain. `None` when nothing is left.
fn shown_text(text: &str, max: usize) -> Option<String> {
    let cleaned: String = text
        .chars()
        .map(|c| {
            let bidi = matches!(
                c,
                '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
            );
            if c.is_control() || bidi { ' ' } else { c }
        })
        .collect();
    let shown: String = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect();
    let shown = shown.trim_end().to_owned();
    (!shown.is_empty()).then_some(shown)
}

/// `cloud link <workspace>/<vault> [--leave-unsettled-local]` for the
/// project at `root`: refused while local tasks are unsettled (they would
/// be stranded once the project's tasks are Cloud's), unless the person
/// leaves them local on purpose. An address without `@<server>` is on the
/// Cloud of `instance` (rc4-56).
fn link_project_at(
    root: &std::path::Path,
    args: &[String],
    instance: Option<&lean_ctx_cloud::state::InstanceRecord>,
) -> Result<(Vec<String>, link::LinkOutcome), CloudError> {
    use lean_ctx_cloud::bring_local;
    let repo = args.iter().find(|a| !a.starts_with("--")).ok_or_else(|| {
        CloudError::Config("usage: lean-ctx cloud link <workspace>/<vault>".into())
    })?;
    let server = instance.and_then(|i| super::local_read::vaults::server_of(&i.url));
    let repo = match server {
        Some(server) if !repo.contains('@') => format!("{repo}@{server}"),
        _ => repo.clone(),
    };
    let leave = args.iter().any(|a| a == bring_local::LEAVE_UNSETTLED_FLAG);
    let unsettled = bring_local::link_gate(root, leave).map_err(CloudError::Refused)?;
    let outcome = link::link(root, &repo)?;
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
    Ok((lines, outcome))
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

/// Worker mode as `cloud worker status` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerState {
    NotOptedIn,
    Stopped,
    /// Turned off on this computer (rc4-16): `cloud.pickup.set` off or `cloud worker
    /// stop`, lifted only by the app's signed turn-on.
    Off,
    Paused,
    Active,
}

impl WorkerState {
    fn of(
        optin: &lean_ctx_cloud::worker::Optin,
        local: &lean_ctx_cloud::worker::WorkerState,
    ) -> Self {
        if local.explicit_off {
            return Self::Off;
        }
        match (&optin.optin_id, optin.active, local.paused) {
            (None, _, _) => Self::NotOptedIn,
            (Some(_), false, _) => Self::Stopped,
            (Some(_), true, true) => Self::Paused,
            (Some(_), true, false) => Self::Active,
        }
    }

    /// The `state` word of `--json`.
    fn word(self) -> &'static str {
        match self {
            Self::NotOptedIn => "not_opted_in",
            Self::Stopped => "stopped",
            Self::Off => "off",
            Self::Paused => "paused",
            Self::Active => "active",
        }
    }

    fn text(self) -> &'static str {
        match self {
            Self::NotOptedIn => "not opted in",
            Self::Stopped => "stopped",
            Self::Off => "off on this computer (LeanCTX Code turns task pickup on again)",
            Self::Paused => {
                "paused on this machine (resume: `lean-ctx cloud worker resume` in your terminal)"
            }
            Self::Active => "active",
        }
    }
}

/// `cloud worker status --json`: `state`, `paused_here` and the opt-in as
/// the Cloud returned it (`optin.vaults`: the vault ids tasks are pulled from).
fn worker_status_json(
    state: WorkerState,
    paused_here: bool,
    optin: &lean_ctx_cloud::worker::Optin,
) -> serde_json::Value {
    serde_json::json!({
        "state": state.word(),
        "paused_here": paused_here,
        "optin": optin,
    })
}

/// rc4-16: with the app's opt-in key bound here, `cloud worker status
/// --json` advertises `cloud worker on` (`capabilities.worker_on`) and
/// carries what the app signs for it (`instance_id`, `turn_on`). Without
/// the key the document stays as it was.
fn with_worker_on(
    mut doc: serde_json::Value,
    key_bound: bool,
    instance_id: &str,
    optin: &lean_ctx_cloud::worker::Optin,
    folder_vault: Option<String>,
    name_of: &dyn Fn(&str) -> Option<String>,
) -> serde_json::Value {
    if !key_bound {
        return doc;
    }
    // The opt-in's vaults plus the vault of the folder the app runs in, so
    // that `cloud worker on` in the same folder accepts what is signed.
    let mut vaults = optin.vaults.clone();
    vaults.extend(folder_vault);
    let turn_on =
        super::local_read::pickup_part::proposal(optin, vaults, None, instance_id, name_of)
            .ok()
            .flatten();
    doc["capabilities"] = serde_json::json!({"worker_on": true});
    doc["instance_id"] = serde_json::json!(instance_id);
    doc["turn_on"] = serde_json::to_value(turn_on).unwrap_or_default();
    doc
}

/// `cloud worker [status|pause|resume|stop|list|rename|register|retire]`.
fn worker(config: &CloudConfig, args: &[String]) -> Result<(), CloudError> {
    use lean_ctx_cloud::worker;
    let store = CloudStore::open()?;
    let json = args.iter().any(|a| a == "--json");
    let positionals = super::local_read::positionals(args);
    match positionals.first().copied().unwrap_or("status") {
        "list" => {
            use lean_ctx_cloud::workers::attach;
            let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
            let workers = attach::list(&store, &instance.instance_id)?;
            let now_ms = u64::try_from(lean_ctx_time::UtcDateTime::now().timestamp_millis())
                .unwrap_or_default();
            if workers.is_empty() {
                println!("No workers yet: one appears when an agent session posts or captures.");
            }
            for w in workers {
                let name = w.name.clone().unwrap_or_else(|| {
                    lean_ctx_cloud::workers::default_name(
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
            use lean_ctx_cloud::workers::{attach, card, register};
            let cli = flag_value(args, "--cli")?.ok_or_else(|| {
                CloudError::Config(
                    "usage: lean-ctx cloud worker register --cli <host> [--role <role>]".into(),
                )
            })?;
            let role = lean_ctx_cloud::workers::normalize_role(
                flag_value(args, "--role")?.unwrap_or("general"),
            );
            let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
            let root = lean_ctx_cloud::root_cert::trust_root(&store, config, &instance.url)?;
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
            use lean_ctx_cloud::workers::{attach, register};
            let usage = || {
                CloudError::Config(
                    "usage: lean-ctx cloud worker retire <wkr_…> [--force] | --idle <dur> (e.g. 24h, 90m, 7d; default 24h)"
                        .into(),
                )
            };
            let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
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
                if !lean_ctx_cloud::bus::address::is_worker_id(id) {
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
            let root = lean_ctx_cloud::root_cert::trust_root(&store, config, &instance.url)?;
            let session = Session::load(store, root)?;
            let sent = register::retire_evicted(&session);
            if gone.is_empty() {
                println!("No worker to retire.");
            }
            for id in &gone {
                println!("Retired {id}");
            }
            if let Err(error) = sent {
                println!(
                    "LeanCTX Cloud was not told yet ({error}); the next sync retires them there."
                );
            }
        }
        "rename" => {
            let (Some(id), Some(name)) = (args.get(1), args.get(2)) else {
                return Err(CloudError::Config(
                    "usage: lean-ctx cloud worker rename <wkr_…> <name>".into(),
                ));
            };
            let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
            let root = lean_ctx_cloud::root_cert::trust_root(&store, config, &instance.url)?;
            let session = Session::load(store, root)?;
            let renamed = session.with_client(|client| {
                lean_ctx_cloud::workers::register::patch(client, id, Some(name), None)
            })?;
            lean_ctx_cloud::workers::attach::update_book(
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
                "Worker mode paused on this machine: no new claims, and held claims are no longer renewed, until `lean-ctx cloud worker resume`."
            );
        }
        "resume" => {
            // rc4-16: resume lifts only the pause; an explicit off stays
            // until LeanCTX Code turns task pickup on again.
            worker::set_paused(&store, false)?;
            if worker::read_state(&store).explicit_off {
                println!(
                    "Worker mode resumed on this machine, but task pickup stays off: it was turned off, and only the owner turns it on again, in LeanCTX Code or on the web."
                );
            } else {
                println!("Worker mode resumed on this machine.");
            }
        }
        "stop" => {
            let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
            let root = lean_ctx_cloud::root_cert::trust_root(&store, config, &instance.url)?;
            let session = Session::load(store, root)?;
            let stopped = worker::stop(&session);
            // Cloud ended them, or they end at their expiry when the stop did
            // not reach it: either way this machine stops renewing them.
            lean_ctx_cloud::tasks::claims::clear(session.store.data_dir())?;
            stopped?;
            println!(
                "Worker mode stopped for this computer; its claims ended. The owner turns it on again on the web or in LeanCTX Code."
            );
        }
        "status" => {
            let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
            let root = lean_ctx_cloud::root_cert::trust_root(&store, config, &instance.url)?;
            let session = Session::load(store, root)?;
            let optin = worker::fetch_optin(&session)?;
            // A turn-on on the web lifts the off kept here (rc4-16 M3).
            let local = worker::observe(&session.store, &optin)?;
            let state = WorkerState::of(&optin, &local);
            if json {
                // The app limits task pickup to `vaults` (rc4-101).
                let key = lean_ctx_cloud::consent::optin_key::bound_optin_key(&session.store);
                // Vault names for the app to show at the click (rc4-16 S4).
                let map = lean_ctx::core::vault_map::VaultMap::load(session.store.data_dir());
                let raw = serde_json::to_value(&optin).unwrap_or_default();
                let name_of = |vault_id: &str| {
                    super::local_read::pickup_part::vault_name(&map, &raw, vault_id)
                };
                let doc = with_worker_on(
                    worker_status_json(state, local.paused, &optin),
                    key.is_some(),
                    &session.instance.instance_id,
                    &optin,
                    super::local_read::pickup_part::current_folder()
                        .ok()
                        .and_then(|root| {
                            super::local_read::pickup_part::folder_vault(
                                session.store.data_dir(),
                                &root,
                            )
                            .ok()
                        }),
                    &name_of,
                );
                println!("{doc}");
                return Ok(());
            }
            let state = state.text();
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
            return Err(CloudError::Config(format!(
                "unknown worker command '{other}' (status, on, pause, resume, stop)"
            )));
        }
    }
    Ok(())
}

/// `cloud graph publish`: the current project's code graph (N1).
fn publish_graph(config: &CloudConfig) -> Result<(), CloudError> {
    use lean_ctx_cloud::knowledge::code_graph;
    let store = CloudStore::open()?;
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    let root = lean_ctx_cloud::root_cert::trust_root(&store, config, &instance.url)?;
    let session = Session::load(store, root)?;
    let cwd = std::env::current_dir().map_err(|e| CloudError::Local(e.to_string()))?;
    let project_root = lean_ctx::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy());
    let record = lean_ctx_cloud::resolution::vault_for_root(
        session.store.data_dir(),
        std::path::Path::new(&project_root),
    )?
    .ok_or_else(|| {
        CloudError::Config(
            "this repository is not linked to a vault (run `lean-ctx cloud sync` in it)".into(),
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
            "{} files left out (not admitted as knowledge, or paths LeanCTX Cloud does not take).",
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

    /// The terminal `cloud link` asks the same gate as the JSON one: refused
    /// while a local task is unsettled, linked with --leave-unsettled-local,
    /// and then it says which tasks stay local.
    #[test]
    fn the_terminal_link_refuses_while_local_tasks_are_unsettled() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        lean_ctx_cloud::tasks::local::write(root, |board, now| {
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
        let refused = link_project_at(root, &args(&["platform/api"]), None).unwrap_err();
        assert!(refused.to_string().contains("#1 (proposed)"), "{refused}");
        assert!(!root.join(link::LINK_FILE).exists(), "nothing written");

        let (lines, _) = link_project_at(
            root,
            &args(&["platform/api", "--leave-unsettled-local"]),
            None,
        )
        .unwrap();
        assert!(root.join(link::LINK_FILE).exists());
        assert!(
            lines
                .iter()
                .any(|l| l.contains("local, not shared: #1 (proposed)")),
            "{lines:?}"
        );
    }

    fn share(state: &'static str, message: Option<&str>) -> super::super::local_read::link::Share {
        super::super::local_read::link::Share {
            state,
            vault_id: (state == "shared").then(|| "vlt_1".to_owned()),
            consent_id: None,
            answer_web: None,
            answer_terminal: None,
            message: message.map(str::to_owned),
        }
    }

    /// Team-knowledge e2e: a linked project on a connected machine
    /// is shared by `link` itself; no "run connect" once it is.
    #[test]
    fn a_shared_link_says_so_and_asks_for_no_connect() {
        let lines = link_share_lines(None, "acme/platform", &share("shared", None));
        assert_eq!(
            lines,
            ["Shared with acme/platform: decisions, tasks and docs of this project work here now."]
        );
        let stale = Some((
            "this machine resolved the project to acme/old".to_owned(),
            Some("acme/platform".to_owned()),
        ));
        let lines = link_share_lines(stale.clone(), "acme/platform", &share("shared", None));
        assert!(
            lines.iter().all(|l| !l.contains("cloud connect")),
            "{lines:?}"
        );
        assert!(lines[0].contains("That resolution is dropped"), "{lines:?}");

        // Refused (another binding): the dropped resolution keeps its hint.
        let lines = link_share_lines(
            stale,
            "acme/platform",
            &share("refused", Some("vault_mismatch")),
        );
        assert!(lines[0].contains("lean-ctx cloud connect"), "{lines:?}");
        assert_eq!(
            lines[1],
            "Not shared with acme/platform yet: vault_mismatch. Run `lean-ctx cloud sync` here to share it."
        );
    }

    #[test]
    fn a_first_share_names_how_to_answer() {
        let mut waiting = share("consent_required", None);
        waiting.answer_terminal = Some("lean-ctx cloud consent approve c_1".into());
        waiting.answer_web = Some("lean-ctx cloud consent --web c_1".into());
        let lines = link_share_lines(None, "acme/platform", &waiting);
        assert_eq!(
            lines,
            [
                "Not shared with acme/platform yet: the first share waits for your yes. Run `lean-ctx cloud consent approve c_1` in your own terminal (or `lean-ctx cloud consent --web c_1`)."
            ]
        );
    }

    /// The Cloud's reason reaches the terminal without control or bidi
    /// characters and bounded in length.
    #[test]
    fn the_clouds_reason_is_shown_clean_and_bounded() {
        let said = format!(
            "vault\u{1b}[2J is \u{202E}gone\u{2069}\n{}",
            "x".repeat(400)
        );
        let lines = link_share_lines(None, "acme/platform", &share("refused", Some(&said)));
        let line = &lines[0];
        assert!(
            line.starts_with("Not shared with acme/platform yet: vault [2J is gone x"),
            "{line}"
        );
        assert!(
            !line
                .chars()
                .any(|c| c.is_control() || ('\u{202A}'..='\u{2069}').contains(&c))
        );
        assert!(line.chars().count() < 300 + 100, "{line}");
        assert_eq!(shown_text("\u{7}\u{200F} \n", 300), None);
        assert_eq!(
            link_share_lines(None, "acme/platform", &share("refused", Some("\u{0}")))[0],
            "Not shared with acme/platform yet. Run `lean-ctx cloud sync` here to share it."
        );
    }

    /// Team-knowledge e2e: `cloud status` on a connected machine
    /// never asks to connect again for a linked project.
    #[test]
    fn status_of_a_linked_unshared_project_suggests_sync_not_connect() {
        for waiting in [false, true] {
            let line = unshared_line("app", Some("acme/platform"), waiting);
            assert!(!line.contains("cloud connect"), "{line}");
            assert!(line.contains("lean-ctx cloud sync"), "{line}");
            assert!(line.contains("linked to acme/platform"), "{line}");
        }
        assert_eq!(
            unshared_line("app", None, false),
            "app → not shared yet (run `lean-ctx cloud connect` here)"
        );
    }
}

/// The lease's rights by their wire names, for `cloud status`. A right this
/// build does not know decodes as `CloudRight::Unknown` and grants nothing,
/// so it is left out (serde writes that variant as "Unknown", not a wire name).
fn rights_line(rights: &[lean_ctx_protocol::cloud::lease::CloudRight]) -> String {
    rights
        .iter()
        .filter(|r| **r != lean_ctx_protocol::cloud::lease::CloudRight::Unknown)
        .filter_map(|r| serde_json::to_value(r).ok()?.as_str().map(str::to_string))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod unlink_project_line_tests {
    use super::*;

    /// bd 40060: `cloud status` says a damaged opt-out list holds the
    /// project back, and never calls an unlinked worktree shared through
    /// its main checkout's record.
    #[test]
    fn unlink_project_line_names_damage_and_never_shared() {
        let base = tempfile::tempdir().unwrap();
        let base = base.path().canonicalize().unwrap();
        let store = CloudStore::at(base.join("c"), base.join("d"));
        let (main, wt) = lean_ctx_cloud::unlinked::fake_worktree(&base);
        std::fs::write(wt.join(".lean-ctx-id"), "payments-repo").unwrap();
        let stable = lean_ctx_protocol::cloud::vault::stable_identity(
            &lean_ctx_cloud::vault::project_identity(&wt).unwrap(),
        );
        let mut map = lean_ctx_cloud::vault::VaultMap::default();
        map.vaults.insert(
            stable,
            lean_ctx_cloud::vault::VaultRecord {
                vault_id: "vlt_1".into(),
                ns: "acme".into(),
                name: "payments".into(),
                sealed: false,
                roots: vec![main.to_string_lossy().into_owned()],
            },
        );
        map.save(store.data_dir()).unwrap();
        assert_eq!(
            project_line_in(&store, &wt).as_deref(),
            Some("wt → acme/payments (shared)")
        );

        lean_ctx_cloud::unlinked::add(store.data_dir(), &wt).unwrap();
        let line = project_line_in(&store, &wt).unwrap_or_default();
        assert!(!line.contains("(shared)"), "{line}");

        std::fs::write(store.data_dir().join(lean_ctx_cloud::unlinked::FILE), "[").unwrap();
        let line = project_line_in(&store, &wt).unwrap();
        assert!(line.starts_with("wt → not shared: "), "{line}");
        assert!(line.contains(lean_ctx_cloud::unlinked::FILE), "{line}");
    }
}

#[cfg(test)]
mod rights_line_tests {
    use super::rights_line;
    use lean_ctx_protocol::cloud::lease::CloudRight;

    /// A lease from a newer Cloud names a right this build does not know: it
    /// decodes as Unknown and `cloud status` lists only the known wire names
    /// (it printed "…, tasks.propose, Unknown" against staging on 18.11.).
    #[test]
    fn an_unknown_right_is_left_out() {
        let rights: Vec<CloudRight> =
            serde_json::from_str(r#"["knowledge.read","tasks.propose","models.future"]"#).unwrap();
        assert_eq!(rights.last(), Some(&CloudRight::Unknown));
        assert_eq!(rights_line(&rights), "knowledge.read, tasks.propose");
    }
}

#[cfg(test)]
mod worker_retire_tests {
    use super::{parse_idle, seen_ago};
    use std::time::Duration;

    #[test]
    fn idle_durations_read_with_their_unit() {
        assert_eq!(parse_idle("24h"), Some(Duration::from_hours(24)));
        assert_eq!(parse_idle("90m"), Some(Duration::from_mins(90)));
        assert_eq!(parse_idle("7d"), Some(Duration::from_hours(168)));
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

#[cfg(test)]
mod stop_help_tests {
    use super::{HELP, STOP_HELP, help_for};

    #[test]
    fn stop_help_names_stop_release_and_status() {
        let rest = ["--help".to_owned()];
        assert_eq!(help_for("stop", &rest), STOP_HELP);
        assert_eq!(help_for("help", &["stop".to_owned()]), STOP_HELP);
        assert_eq!(help_for("status", &rest), HELP);
        for usage in [
            "--reason <text>",
            "--release --confirm <team-slug>",
            "--status --json",
        ] {
            assert!(STOP_HELP.contains(usage), "{usage}");
        }
        assert!(!STOP_HELP.contains("two-factor") && !STOP_HELP.contains("This Mac"));
        let words: Vec<String> = STOP_HELP
            .split(|c: char| !c.is_alphanumeric())
            .map(str::to_lowercase)
            .collect();
        for banned in ["hive", "lease", "instance", "workspace", "engine"] {
            assert!(!words.iter().any(|w| w == banned), "{banned}");
        }
    }
}

#[cfg(test)]
mod worker_status_json_tests {
    use super::{WorkerState, with_worker_on, worker_status_json};
    use lean_ctx_cloud::worker::Optin;

    fn optin(optin_id: Option<&str>, active: bool) -> Optin {
        serde_json::from_value(serde_json::json!({
            "schema": "leanctx.engine_optin.v1",
            "optin_id": optin_id,
            "active": active,
            "vaults": ["vlt_a", "vlt_b"],
            "daily_budget": {},
            "used_today": {"day": "2026-11-24", "tasks": 0, "tokens": 0, "usd_micros": 0},
        }))
        .expect("opt-in fixture")
    }

    #[test]
    fn every_state_has_its_word() {
        let on = optin(Some("opt_1"), true);
        assert_eq!(
            WorkerState::of(&optin(None, false), &here(false, false)).word(),
            "not_opted_in"
        );
        assert_eq!(
            WorkerState::of(&optin(Some("opt_1"), false), &here(true, false)).word(),
            "stopped"
        );
        assert_eq!(WorkerState::of(&on, &here(true, false)).word(), "paused");
        assert_eq!(WorkerState::of(&on, &here(false, false)).word(), "active");
    }

    fn here(paused: bool, explicit_off: bool) -> lean_ctx_cloud::worker::WorkerState {
        lean_ctx_cloud::worker::WorkerState {
            paused,
            explicit_off,
            ..Default::default()
        }
    }

    /// An off kept on this computer shows as `off` whatever the Cloud says, so
    /// `cloud worker status` never reports `active` while task pickup is off.
    #[test]
    fn pickup_worker_status_shows_an_explicit_off() {
        let on = optin(Some("opt_1"), true);
        for paused in [false, true] {
            assert_eq!(WorkerState::of(&on, &here(paused, true)).word(), "off");
        }
        assert_eq!(
            WorkerState::of(&optin(Some("opt_1"), false), &here(true, true)).word(),
            "off"
        );
        let doc = worker_status_json(WorkerState::of(&on, &here(true, true)), true, &on);
        assert_eq!(doc["state"], "off");
    }

    /// The app reads these keys (rc4-101); renaming one breaks its pickup.
    #[test]
    fn json_keeps_the_keys_the_app_reads() {
        let on = optin(Some("opt_1"), true);
        let doc = worker_status_json(WorkerState::of(&on, &here(true, false)), true, &on);
        assert_eq!(doc["state"], "paused");
        assert_eq!(doc["paused_here"], true);
        assert_eq!(
            doc["optin"]["vaults"],
            serde_json::json!(["vlt_a", "vlt_b"])
        );
        assert_eq!(doc["optin"]["active"], true);
    }

    /// af reads `capabilities.worker_on` only when the opt-in key is bound.
    #[test]
    fn pickup_worker_status_advertises_worker_on_only_with_the_opt_in_key() {
        let instance = "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c09";
        let mut on: Optin = serde_json::from_value(serde_json::json!({
            "schema": "leanctx.engine_optin.v1",
            "optin_id": "opt_1",
            "active": false,
            "vaults": ["0f3c2b6a8d4e4a7b9c105e2f6a7b8c01"],
            "daily_budget": {"tasks": 3, "tokens": 1_000_000},
            "used_today": {"day": "2026-11-24", "tasks": 0, "tokens": 0, "usd_micros": 0},
        }))
        .expect("opt-in fixture");
        let plain = worker_status_json(WorkerState::of(&on, &here(false, false)), false, &on);
        let without = with_worker_on(plain.clone(), false, instance, &on, None, &|_| None);
        assert_eq!(without, plain);
        assert!(without.get("capabilities").is_none());
        let with = with_worker_on(
            plain,
            true,
            instance,
            &on,
            Some("ffffffffffffffffffffffffffffffff".into()),
            &|_| None,
        );
        assert_eq!(with["capabilities"], serde_json::json!({"worker_on": true}));
        assert_eq!(with["instance_id"], instance);
        assert_eq!(with["turn_on"]["body"]["prev_optin_id"], "opt_1");
        assert_eq!(with["turn_on"]["domain"], "leanctx.cloud.optin.v1\n");
        assert_eq!(
            with["turn_on"]["body"]["vaults"],
            serde_json::json!([
                "0f3c2b6a8d4e4a7b9c105e2f6a7b8c01",
                "ffffffffffffffffffffffffffffffff"
            ])
        );
        on.vaults.clear();
        let none = with_worker_on(serde_json::json!({}), true, instance, &on, None, &|_| None);
        assert!(none["turn_on"].is_null());
    }
}
