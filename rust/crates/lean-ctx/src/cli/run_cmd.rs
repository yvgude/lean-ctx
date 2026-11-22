// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx run`: start a coding agent on a task in a fresh git worktree,
//! on this machine or (`--cloud`) on a LeanCTX computer (contract §35).
//!
//! The agent starts only on this explicit command. Its model access is the
//! person's choice: the agent CLI's own login (`own`) or the LeanCTX balance
//! through the gateway (`balance`). With `--cloud` the code reaches the
//! computer only over SSH (a `git push` to a bare repository there); LeanCTX
//! Cloud is asked only to start the computer, with a request that carries no
//! code, no diff and no task text, and it decides role, price and balance.

use std::io::{BufRead as _, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Map, Value, json};

use super::local_read::{self, Code, Failure};

#[path = "run_store.rs"]
pub(crate) mod store;

#[path = "run_cloud.rs"]
mod cloud;

use store::{RunRecord, Store};

const HELP: &str = "\
lean-ctx run — a coding agent on a task, in a fresh git worktree

USAGE:
  lean-ctx run \"<task>\" [--agent claude|codex] [--base <ref>] [--include-dirty]
        [--model-access own|balance] [--max-minutes N] [--detach] [--json]
  lean-ctx run --cloud \"<task>\" --model-access own|balance [--computer <name|id>]
        [--agent claude|codex] [--base <ref>] [--include-dirty] [--max-minutes N]
        [--request-id <id>] [--yes] [--detach] [--json]
  lean-ctx run list [--json]
  lean-ctx run status <id> [--json]
  lean-ctx run events <id> [--since <seq>] [--limit N] [--follow] [--json]
  lean-ctx run logs <id> [--follow]
  lean-ctx run stop <id> [--json]
  lean-ctx run fetch <id> [--json]

The task is one argument (\"-\" reads it from stdin). The agent works on the branch
leanctx/run/<id>, started from --base (default HEAD); --include-dirty starts from the
uncommitted changes to tracked files as well. The agent runs only because of this
command, unattended in its own worktree: codex runs with --full-auto (it runs commands
there without asking), claude with --permission-mode acceptEdits (it edits files there
without asking). It starts with the model access you choose: own (the agent CLI's own
login where it runs) or balance (your own LeanCTX balance through the gateway, from your
own LeanCTX Cloud login where it runs; never a team's or another person's login).

--cloud runs the agent on a LeanCTX computer. LeanCTX computers are not available yet:
until LeanCTX Cloud offers them, --cloud sends nothing paid and ends with
computer_not_available (\"LeanCTX computers are not available yet.\"). Once they are
offered, a run is billed from its first minute at the price LeanCTX Cloud lists.
Starting a paid computer needs you at a terminal (or LeanCTX Code):
from a script or an agent without a terminal nothing is sent, not even with --yes.
At a terminal, without --yes the command shows the price and asks (with --json it sends
nothing and exits 1 with confirmation_required and the price in error.details).
With --model-access balance the computer must be connected to LeanCTX Cloud as you
(lean-ctx cloud connect, on the computer, with your own login). Your code reaches the
computer only over SSH, as a git push; it never passes through LeanCTX Cloud. Stopping a
run (SIGTERM to the agent, SIGKILL after 10 s) does not stop the computer
(lean-ctx cloud computer stop <name>). When LeanCTX Cloud stops the computer because the
balance is used up, the run ends as stopped with stop_reason balance_empty.

lean-ctx run fetch <id> creates the branch leanctx/run/<id> or fast-forwards it; a branch
with commits of its own, or the checked-out branch, is left as is (conflict).

Without --detach the command follows the run until it ends (and, for --cloud, fetches
its branch). --json prints the Local Read document run.ack and returns.
";

/// Flags that take a value.
const VALUE_FLAGS: &[&str] = &[
    "--agent",
    "--base",
    "--computer",
    "--id",
    "--limit",
    "--max-minutes",
    "--model-access",
    "--repo",
    "--request-id",
    "--since",
];

/// Flags without a value.
const BOOL_FLAGS: &[&str] = &[
    "--cloud",
    "--detach",
    "--follow",
    "--include-dirty",
    "--init",
    "--json",
    "--yes",
];

pub(crate) const KIND_ACK: &str = "run.ack";
pub(crate) const KIND_LIST: &str = "run.list";
pub(crate) const KIND_STATUS: &str = "run.status";
pub(crate) const KIND_EVENTS: &str = "run.events";

/// Agents `run` starts.
const AGENTS: [&str; 2] = ["claude", "codex"];
/// Model access choices.
const ACCESS: [&str; 2] = ["own", "balance"];
/// Longest `--max-minutes`.
const MAX_MINUTES: u64 = 24 * 60;

/// The parsed arguments of `lean-ctx run`.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    positional: Vec<String>,
    values: Vec<(String, String)>,
    bools: Vec<String>,
    /// `--` was given: what follows is the task, never a verb.
    task_only: bool,
}

impl Args {
    fn parse(raw: &[String]) -> Result<Self, Failure> {
        let mut out = Self::default();
        let mut i = 0;
        while i < raw.len() {
            let arg = &raw[i];
            i += 1;
            if out.task_only || arg == "-" || !arg.starts_with("--") {
                out.positional.push(arg.clone());
                continue;
            }
            if arg == "--" {
                out.task_only = true;
                continue;
            }
            let (name, attached) = match arg.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (arg.as_str(), None),
            };
            if VALUE_FLAGS.contains(&name) {
                let value = if let Some(value) = attached {
                    value.to_owned()
                } else {
                    let value = raw
                        .get(i)
                        .cloned()
                        .ok_or_else(|| Failure::invalid(format!("{name} needs a value")))?;
                    i += 1;
                    value
                };
                if value.is_empty() {
                    return Err(Failure::invalid(format!("{name} needs a value")));
                }
                if out.value(name).is_some() {
                    return Err(Failure::invalid(format!("{name} is given twice")));
                }
                out.values.push((name.to_owned(), value));
            } else if BOOL_FLAGS.contains(&name) && attached.is_none() {
                out.bools.push(name.to_owned());
            } else {
                return Err(Failure::invalid(format!(
                    "unknown flag {arg} (lean-ctx run --help)"
                )));
            }
        }
        Ok(out)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn has(&self, name: &str) -> bool {
        self.bools.iter().any(|b| b == name)
    }

    fn json(&self) -> bool {
        self.has("--json")
    }

    /// The run id after the verb.
    fn id(&self) -> Result<&str, Failure> {
        match self.positional.as_slice() {
            [_, id] if store::valid_id(id) => Ok(id),
            [_, id] => Err(Failure::invalid(format!("'{id}' is not a run id (run_…)"))),
            _ => Err(Failure::invalid("give one run id (lean-ctx run list)")),
        }
    }

    fn agent(&self) -> Result<&'static str, Failure> {
        let wanted = self.value("--agent").unwrap_or("claude");
        AGENTS
            .into_iter()
            .find(|a| *a == wanted)
            .ok_or_else(|| Failure::invalid(format!("--agent is claude or codex, not '{wanted}'")))
    }

    /// `--model-access`; `--cloud` needs it named (the person chooses).
    fn model_access(&self, required: bool) -> Result<&'static str, Failure> {
        let Some(wanted) = self.value("--model-access") else {
            if required {
                return Err(Failure::invalid(
                    "choose the model access: --model-access own (the agent CLI's login) or \
                     --model-access balance (the LeanCTX balance)",
                ));
            }
            return Ok("own");
        };
        ACCESS.into_iter().find(|a| *a == wanted).ok_or_else(|| {
            Failure::invalid(format!("--model-access is own or balance, not '{wanted}'"))
        })
    }

    fn max_minutes(&self) -> Result<Option<u64>, Failure> {
        self.value("--max-minutes")
            .map(|raw| {
                raw.parse::<u64>()
                    .ok()
                    .filter(|m| (1..=MAX_MINUTES).contains(m))
                    .ok_or_else(|| {
                        Failure::invalid(format!(
                            "--max-minutes is 1 to {MAX_MINUTES}, not '{raw}'"
                        ))
                    })
            })
            .transpose()
    }

    fn number(&self, name: &str, default: u64, max: u64) -> Result<u64, Failure> {
        match self.value(name) {
            None => Ok(default),
            Some(raw) => raw
                .parse::<u64>()
                .ok()
                .filter(|n| *n <= max)
                .ok_or_else(|| Failure::invalid(format!("{name} is 0 to {max}, not '{raw}'"))),
        }
    }
}

/// `lean-ctx run …`.
pub(crate) fn cmd_run(raw: &[String]) {
    if wants_help(raw) {
        print!("{HELP}");
        return;
    }
    let json = local_read::wants_json(raw);
    if let Err(failure) = Args::parse(raw).and_then(|args| dispatch(&args)) {
        if json {
            local_read::fail(&failure);
        }
        eprintln!("lean-ctx run: {}", failure.message);
        std::process::exit(failure.code.exit_status());
    }
}

/// `-h` or `--help` before any `--`: the usage, nothing else (a task that
/// is literally `--help` follows `--`).
fn wants_help(raw: &[String]) -> bool {
    raw.iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "-h" || arg == "--help")
}

fn dispatch(args: &Args) -> Result<(), Failure> {
    if args.task_only {
        return start(args);
    }
    match args.positional.first().map(String::as_str) {
        None => {
            eprint!("{HELP}");
            Err(Failure::invalid("give the task (lean-ctx run \"<task>\")"))
        }
        Some("help") => {
            print!("{HELP}");
            Ok(())
        }
        Some("list") if args.positional.len() == 1 => list(args),
        Some("status") => status(args),
        Some("events") => events(args),
        Some("logs") => logs(args),
        Some("stop") => stop(args),
        Some("fetch") => cloud::fetch(args),
        // Internal: the process that runs the agent of one run.
        Some("exec") if args.positional.len() == 1 => exec(args),
        // Internal, on a LeanCTX computer: `run --cloud` prepares the bare
        // repository there and starts the run over SSH.
        Some("receive") if args.positional.len() == 1 => cloud::receive(args),
        Some("start") if args.positional.len() == 1 && args.value("--id").is_some() => {
            cloud::remote_start(args)
        }
        Some(_) => start(args),
    }
}

/// The task text: one argument, or stdin for `-`.
fn task_text(args: &Args) -> Result<String, Failure> {
    let raw = match args.positional.as_slice() {
        [one] if one == "-" => read_stdin()?,
        [one] => one.clone(),
        [] => return Err(Failure::invalid("give the task (lean-ctx run \"<task>\")")),
        _ => {
            return Err(Failure::invalid(
                "give the task as one quoted argument (lean-ctx run \"<task>\")",
            ));
        }
    };
    check_task(raw)
}

fn read_stdin() -> Result<String, Failure> {
    let mut text = String::new();
    std::io::stdin()
        .take(store::MAX_TASK_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|e| Failure::invalid(format!("cannot read the task from stdin: {e}")))?;
    Ok(text)
}

fn check_task(text: String) -> Result<String, Failure> {
    if text.trim().is_empty() {
        return Err(Failure::invalid("the task is empty"));
    }
    if text.len() > store::MAX_TASK_BYTES {
        return Err(Failure::invalid(format!(
            "the task is longer than {} bytes",
            store::MAX_TASK_BYTES
        )));
    }
    Ok(text)
}

fn internal(message: impl Into<String>) -> Failure {
    Failure::new(Code::Internal, message)
}

fn open_store() -> Result<Store, Failure> {
    Store::open().map_err(internal)
}

/// `git -C <dir> <args>`: its trimmed stdout, or its stderr as the error.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(format!(
            "git {}: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// The project root (the git top level of the working directory).
fn project_root() -> Result<PathBuf, Failure> {
    let cwd =
        std::env::current_dir().map_err(|e| internal(format!("no working directory: {e}")))?;
    git(&cwd, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .map_err(|_| {
            Failure::new(
                Code::NoProject,
                "lean-ctx run works inside a git repository",
            )
        })
}

/// The commit the run starts from: `--base` (default `HEAD`), or with
/// `--include-dirty` a commit of the uncommitted tracked changes on it.
fn base_commit(root: &Path, args: &Args) -> Result<String, Failure> {
    let wanted = args.value("--base").unwrap_or("HEAD");
    if wanted.starts_with('-') {
        return Err(Failure::invalid(format!("--base '{wanted}' is not a ref")));
    }
    let base = git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{wanted}^{{commit}}"),
        ],
    )
    .map_err(|_| Failure::invalid(format!("--base '{wanted}' names no commit")))?;
    if !args.has("--include-dirty") {
        return Ok(base);
    }
    if args.value("--base").is_some() {
        return Err(Failure::invalid(
            "--include-dirty starts from HEAD; leave out --base",
        ));
    }
    let stash = git(
        root,
        &["stash", "create", "lean-ctx run: uncommitted changes"],
    )
    .map_err(internal)?;
    Ok(if stash.is_empty() { base } else { stash })
}

/// `lean-ctx run "<task>"` (with `--cloud`: on a LeanCTX computer).
fn start(args: &Args) -> Result<(), Failure> {
    let task = task_text(args)?;
    let agent = args.agent()?;
    let max_minutes = args.max_minutes()?;
    let root = project_root()?;
    let base = base_commit(&root, args)?;
    if args.has("--cloud") {
        let access = args.model_access(true)?;
        let start = cloud::Start {
            task: &task,
            agent,
            access,
            max_minutes,
            root: &root,
            base: &base,
        };
        let record = cloud::start(args, &start)?;
        return report_start(args, &record);
    }
    for flag in ["--computer", "--request-id"] {
        if args.value(flag).is_some() {
            return Err(Failure::invalid(format!("{flag} goes with --cloud")));
        }
    }
    if args.has("--yes") {
        return Err(Failure::invalid("--yes goes with --cloud"));
    }
    let access = args.model_access(false)?;
    let store = open_store()?;
    let launch = Launch {
        id: store::new_id(),
        project_root: root.display().to_string(),
        repo: root.clone(),
        base,
        agent,
        access,
        max_minutes,
    };
    let record = launch.run(&store, &task)?;
    report_start(args, &record)
}

/// One run to start on this machine.
pub(crate) struct Launch {
    pub id: String,
    pub project_root: String,
    /// The repository the worktree is added to.
    pub repo: PathBuf,
    pub base: String,
    pub agent: &'static str,
    pub access: &'static str,
    pub max_minutes: Option<u64>,
}

impl Launch {
    /// Record the run, add its worktree and start its agent process.
    pub(crate) fn run(&self, store: &Store, task: &str) -> Result<RunRecord, Failure> {
        let worktree = store.dir(&self.id).join("worktree");
        let mut record = RunRecord {
            schema: store::RECORD_SCHEMA.into(),
            id: self.id.clone(),
            place: store::PLACE_LOCAL.into(),
            computer: None,
            project_root: self.project_root.clone(),
            agent: self.agent.into(),
            model_access: self.access.into(),
            state: store::STATE_STARTING.into(),
            started_at: store::now(),
            ended_at: None,
            branch: store::branch_of(&self.id),
            base: self.base.clone(),
            commits: Vec::new(),
            tool_calls: 0,
            exit_code: None,
            stop_reason: None,
            worktree: Some(worktree.display().to_string()),
            pid: None,
            unit: None,
        };
        store.create(&record, task).map_err(internal)?;
        let worktree_text = worktree.display().to_string();
        let added = git(
            &self.repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                &record.branch,
                &worktree_text,
                &self.base,
            ],
        );
        if let Err(message) = added {
            end(
                store,
                &mut record,
                store::STATE_FAILED,
                Some("worktree"),
                None,
            );
            return Err(internal(format!(
                "cannot create the run's worktree: {message}"
            )));
        }
        let exe = std::env::current_exe().map_err(|e| internal(format!("no executable: {e}")))?;
        let mut command = Command::new(exe);
        command.args(["run", "exec", "--id", &self.id]);
        if let Some(minutes) = self.max_minutes {
            command.args(["--max-minutes", &minutes.to_string()]);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach(&mut command);
        if let Err(e) = command.spawn() {
            end(store, &mut record, store::STATE_FAILED, Some("exec"), None);
            return Err(internal(format!("cannot start the run: {e}")));
        }
        Ok(record)
    }
}

/// Start `command` in its own process group, so it outlives the terminal
/// (or the SSH session) that started it and a stop reaches all of it.
fn detach(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = command;
}

/// End `record` in `state` (final), with its final event.
fn end(
    store: &Store,
    record: &mut RunRecord,
    state: &str,
    reason: Option<&str>,
    exit: Option<i32>,
) {
    record.state = state.into();
    record.stop_reason = reason.map(str::to_owned);
    record.exit_code = exit;
    record.ended_at = Some(store::now());
    record.pid = None;
    let _ = store.save(record);
    let mut fields = Map::new();
    fields.insert("state".into(), state.into());
    fields.insert("stop_reason".into(), reason.into());
    fields.insert("exit_code".into(), exit.into());
    fields.insert("commits".into(), record.commits.len().into());
    let _ = store.append_event(&record.id, state, fields);
}

fn report_start(args: &Args, record: &RunRecord) -> Result<(), Failure> {
    if args.json() {
        local_read::emit(KIND_ACK, || {
            Ok(json!({ "action": "start", "run": record.view() }))
        });
        return Ok(());
    }
    let place = record.computer.as_deref().map_or_else(
        || "on this machine".to_owned(),
        |c| format!("on computer '{c}'"),
    );
    println!(
        "run {} started {place}: {} on branch {} (model access: {})",
        record.id, record.agent, record.branch, record.model_access
    );
    if args.has("--detach") {
        println!("follow: lean-ctx run events {} --follow", record.id);
        return Ok(());
    }
    let ended = follow(record, false)?;
    if record.place == store::PLACE_COMPUTER && ended {
        cloud::fetch_quietly(record)?;
    }
    Ok(())
}

/// Follow the events of `record` until it ends; `true` when it ended.
fn follow(record: &RunRecord, json_lines: bool) -> Result<bool, Failure> {
    if record.place == store::PLACE_COMPUTER {
        return cloud::follow(record, json_lines);
    }
    let store = open_store()?;
    let mut since = 0;
    loop {
        let page = store
            .events(&record.id, since, store::MAX_EVENTS_PAGE)
            .map_err(internal)?;
        for event in &page {
            print_event(event, json_lines);
            since = event["seq"].as_u64().unwrap_or(since);
            if is_final_event(event) {
                return Ok(true);
            }
        }
        if page.is_empty() {
            let current = store.load(&record.id).map_err(internal)?;
            if current.is_none_or(|r| store::is_final(&r.state)) {
                return Ok(true);
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}

pub(crate) fn is_final_event(event: &Value) -> bool {
    event["type"].as_str().is_some_and(store::is_final)
}

pub(crate) fn print_event(event: &Value, json_lines: bool) {
    if json_lines {
        println!("{event}");
        return;
    }
    println!("{}", event_line(event));
}

/// One event as a line for people.
fn event_line(event: &Value) -> String {
    let seq = event["seq"].as_u64().unwrap_or_default();
    let kind = event["type"].as_str().unwrap_or("?");
    let detail = match kind {
        "started" => format!("agent {}", event["agent"].as_str().unwrap_or("?")),
        "tool_calls" => format!(
            "{} tool calls",
            event["tool_calls"].as_u64().unwrap_or_default()
        ),
        "commit" => event["sha"]
            .as_str()
            .unwrap_or("?")
            .chars()
            .take(12)
            .collect(),
        _ => {
            let mut parts = vec![format!(
                "{} commit(s)",
                event["commits"].as_u64().unwrap_or_default()
            )];
            if let Some(reason) = event["stop_reason"].as_str() {
                parts.push(format!("reason {reason}"));
            }
            if let Some(code) = event["exit_code"].as_i64() {
                parts.push(format!("exit {code}"));
            }
            parts.join(", ")
        }
    };
    format!("[{seq}] {kind} {detail}")
}

/// The run named by the arguments, refreshed from its computer when it
/// runs on one.
fn named_run(args: &Args) -> Result<(Store, RunRecord), Failure> {
    let id = args.id()?;
    let store = open_store()?;
    let record = store
        .load(id)
        .map_err(Failure::invalid)?
        .ok_or_else(|| Failure::new(Code::NotFound, format!("no run {id} (lean-ctx run list)")))?;
    Ok((store, record))
}

fn list(args: &Args) -> Result<(), Failure> {
    let runs = open_store()?.list().map_err(internal)?;
    if args.json() {
        local_read::emit(KIND_LIST, || {
            Ok(json!({ "runs": runs.iter().map(RunRecord::view).collect::<Vec<_>>() }))
        });
        return Ok(());
    }
    if runs.is_empty() {
        println!("no runs (lean-ctx run \"<task>\")");
    }
    for run in &runs {
        println!(
            "{}  {:<9} {:<8} {:<6} {} commit(s)",
            run.id,
            run.state,
            run.computer.as_deref().unwrap_or("local"),
            run.agent,
            run.commits.len()
        );
    }
    Ok(())
}

fn status(args: &Args) -> Result<(), Failure> {
    let (store, mut record) = named_run(args)?;
    if record.place == store::PLACE_COMPUTER {
        cloud::refresh(&store, &mut record)?;
    }
    print_status(args, &record);
    Ok(())
}

fn print_status(args: &Args, record: &RunRecord) {
    if args.json() {
        local_read::emit(KIND_STATUS, || Ok(json!({ "run": record.view() })));
        return;
    }
    println!("run {} ({})", record.id, record.state);
    println!(
        "  {} on {}, model access {}",
        record.agent,
        record.computer.as_deref().unwrap_or("this machine"),
        record.model_access
    );
    println!("  branch {} from {}", record.branch, short(&record.base));
    println!(
        "  {} commit(s), {} tool call(s)",
        record.commits.len(),
        record.tool_calls
    );
    if let Some(reason) = &record.stop_reason {
        println!("  reason: {reason}");
    }
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

fn events(args: &Args) -> Result<(), Failure> {
    let (store, record) = named_run(args)?;
    let since = args.number("--since", 0, u64::MAX)?;
    let limit = args.number(
        "--limit",
        store::MAX_EVENTS_PAGE as u64,
        store::MAX_EVENTS_PAGE as u64,
    )?;
    if args.has("--follow") {
        if since != 0 {
            return Err(Failure::invalid(
                "--follow starts at the first event; leave out --since",
            ));
        }
        follow(&record, args.json())?;
        return Ok(());
    }
    let page = if record.place == store::PLACE_COMPUTER {
        cloud::remote_events(&record, since, limit)?
    } else {
        store
            .events(
                &record.id,
                since,
                usize::try_from(limit).unwrap_or(store::MAX_EVENTS_PAGE),
            )
            .map_err(internal)?
    };
    if args.json() {
        let next = page.last().and_then(|e| e["seq"].as_u64()).unwrap_or(since);
        local_read::emit(KIND_EVENTS, || {
            Ok(json!({ "id": record.id, "events": page, "next_since": next }))
        });
        return Ok(());
    }
    for event in &page {
        print_event(event, false);
    }
    Ok(())
}

fn logs(args: &Args) -> Result<(), Failure> {
    if args.json() {
        return Err(Failure::invalid(
            "logs prints the agent's output as text; leave out --json",
        ));
    }
    let (store, record) = named_run(args)?;
    if record.place == store::PLACE_COMPUTER {
        return cloud::remote_logs(&record, args.has("--follow"));
    }
    let path = store.log_path(&record.id);
    let mut offset = 0u64;
    loop {
        offset = copy_from(&path, offset)?;
        if !args.has("--follow") {
            return Ok(());
        }
        let done = store
            .load(&record.id)
            .map_err(internal)?
            .is_none_or(|r| store::is_final(&r.state));
        if done {
            copy_from(&path, offset)?;
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// Print `path` from `offset`; the new offset.
fn copy_from(path: &Path, offset: u64) -> Result<u64, Failure> {
    use std::io::Seek as _;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(offset),
        Err(e) => return Err(internal(format!("cannot read {}: {e}", path.display()))),
    };
    file.seek(std::io::SeekFrom::Start(offset))
        .map_err(|e| internal(e.to_string()))?;
    let mut out = std::io::stdout().lock();
    let copied = std::io::copy(&mut file, &mut out).map_err(|e| internal(e.to_string()))?;
    let _ = out.flush();
    Ok(offset + copied)
}

/// `lean-ctx run stop <id>`: stop the agent; the run keeps its commits.
fn stop(args: &Args) -> Result<(), Failure> {
    let (store, mut record) = named_run(args)?;
    if record.place == store::PLACE_COMPUTER {
        cloud::remote_stop(&store, &mut record)?;
    } else if !store::is_final(&record.state) {
        stop_local(&store, &mut record)?;
    }
    if !args.json() && record.place == store::PLACE_COMPUTER {
        eprintln!(
            "the computer keeps running: lean-ctx cloud computer stop {}",
            record.computer.as_deref().unwrap_or("<name>")
        );
    }
    print_status(args, &record);
    Ok(())
}

fn stop_local(store: &Store, record: &mut RunRecord) -> Result<(), Failure> {
    record.stop_reason = Some("user".into());
    store.save(record).map_err(internal)?;
    if let Some(pid) = record.pid.filter(|pid| alive(*pid)) {
        terminate_group(pid, KILL_GRACE);
        // The run's own process records the end (and the last commits).
        for _ in 0..20 {
            if let Some(now) = store.load(&record.id).map_err(internal)? {
                if store::is_final(&now.state) {
                    *record = now;
                    return Ok(());
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
    end(store, record, store::STATE_STOPPED, Some("user"), None);
    Ok(())
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(not(unix))]
fn alive(_pid: u32) -> bool {
    false
}

/// How long an agent's process group has after `SIGTERM` before `SIGKILL`
/// (§35).
const KILL_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// End the process group `pid` leads: `SIGTERM`, then `SIGKILL` when any
/// member is still alive after `grace`. Returns once the group is gone or
/// was killed. Nothing is sent unless `pid` still leads its own group (the
/// agent is started with `process_group(0)`): a recorded pid that was
/// reused by another process is left alone.
#[cfg(unix)]
pub(super) fn terminate_group(pid: u32, grace: std::time::Duration) {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    if pid <= 1 {
        return;
    }
    // SAFETY: getpgid only reads the process's group id.
    if unsafe { libc::getpgid(pid) } != pid {
        return;
    }
    // SAFETY: plain signals to the agent's own process group; signal 0 only
    // checks that a member still exists.
    let group_alive = || unsafe { libc::kill(-pid, 0) == 0 };
    // SAFETY: SIGTERM to the agent's own process group, checked above.
    unsafe {
        libc::kill(-pid, libc::SIGTERM);
    }
    let deadline = std::time::Instant::now() + grace;
    while std::time::Instant::now() < deadline {
        if !group_alive() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if group_alive() {
        // SAFETY: as above.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
pub(super) fn terminate_group(_pid: u32, _grace: std::time::Duration) {}

/// The agent command line: program and arguments (the task goes to stdin).
fn agent_command(agent: &str) -> (&'static str, &'static [&'static str]) {
    match agent {
        "codex" => ("codex", &["exec", "--full-auto", "--json", "-"]),
        _ => (
            "claude",
            &[
                "-p",
                "--permission-mode",
                "acceptEdits",
                "--output-format",
                "stream-json",
                "--verbose",
            ],
        ),
    }
}

/// The environment that points `agent` at the LeanCTX gateway.
fn gateway_env(
    agent: &str,
    key: &lean_ctx_cloud::gateway::GatewayKey,
) -> Vec<(&'static str, String)> {
    match agent {
        "codex" => vec![
            ("OPENAI_BASE_URL", key.base_url.clone()),
            ("OPENAI_API_KEY", key.key.clone()),
        ],
        _ => vec![
            ("ANTHROPIC_BASE_URL", key.base_url.clone()),
            ("ANTHROPIC_AUTH_TOKEN", key.key.clone()),
        ],
    }
}

/// A gateway key for `--model-access balance`: from the LeanCTX Cloud
/// session of the user the run runs as. On a LeanCTX computer that is the
/// person's own `lc-…` user, connected with the person's own login
/// (`lean-ctx cloud connect` there); the key, and the balance it draws on,
/// are that person's (§35). Never a team's or another person's login.
fn gateway_key() -> Result<lean_ctx_cloud::gateway::GatewayKey, String> {
    use lean_ctx_cloud::gateway::request;
    let (store, config, _) = super::cloud_cmd::cloud_computer_cmd::cloud::connected()
        .map_err(|_| NOT_CONNECTED_AS_YOU.to_owned())?;
    gateway_refusal(request(&store, &config).map_err(|e| e.to_string())?)
}

/// `--model-access balance` where this user has no LeanCTX Cloud session.
pub(super) const NOT_CONNECTED_AS_YOU: &str = "--model-access balance needs this machine connected to LeanCTX Cloud as you (run `lean-ctx cloud connect` there with your own login), or use --model-access own";

/// The gateway's answer as a key or the person's next step. Without a
/// top-up on offer (`next_step` is not `top_up`, as in a trial) no top-up
/// is named.
pub(super) fn gateway_refusal(
    answer: lean_ctx_cloud::gateway::GatewayAnswer,
) -> Result<lean_ctx_cloud::gateway::GatewayKey, String> {
    use lean_ctx_cloud::gateway::GatewayAnswer;
    match answer {
        GatewayAnswer::Ready(key) => Ok(key),
        GatewayAnswer::Stopped { top_up: true, .. } => Err(
            "your LeanCTX balance is used up: top it up in LeanCTX Cloud, or use --model-access own"
                .into(),
        ),
        GatewayAnswer::Stopped { top_up: false, .. } => Err(
            "your LeanCTX balance is used up: use --model-access own (the agent CLI's own login)"
                .into(),
        ),
        GatewayAnswer::TooManyKeys { .. } => Err("this machine holds too many gateway keys".into()),
        GatewayAnswer::Forbidden => Err(
            "this login may not use the balance; --model-access balance uses only your own; use --model-access own"
                .into(),
        ),
        GatewayAnswer::NotConfigured => Err("LeanCTX Cloud does not offer the gateway".into()),
    }
}

/// Tool calls in one line of the agent's JSON output.
fn tool_calls_in(agent: &str, line: &str) -> u64 {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return 0;
    };
    if agent == "codex" {
        return u64::from(
            value["type"] == "item.completed"
                && matches!(
                    value["item"]["type"].as_str(),
                    Some("command_execution" | "mcp_tool_call" | "file_change" | "web_search")
                ),
        );
    }
    if value["type"] != "assistant" {
        return 0;
    }
    value["message"]["content"].as_array().map_or(0, |items| {
        items.iter().filter(|i| i["type"] == "tool_use").count() as u64
    })
}

/// `lean-ctx run exec --id <id>` (internal): run the agent of one run in its
/// worktree, record its events and end the run with its commits.
fn exec(args: &Args) -> Result<(), Failure> {
    let id = args
        .value("--id")
        .filter(|id| store::valid_id(id))
        .ok_or_else(|| Failure::invalid("exec needs --id run_…"))?;
    let store = open_store()?;
    let mut record = store
        .load(id)
        .map_err(Failure::invalid)?
        .ok_or_else(|| Failure::new(Code::NotFound, format!("no run {id}")))?;
    if record.state != store::STATE_STARTING {
        return Err(Failure::new(
            Code::Conflict,
            format!("run {id} was started already"),
        ));
    }
    let task = std::fs::read_to_string(store.task_path(id)).map_err(|e| internal(e.to_string()))?;
    let worktree = PathBuf::from(record.worktree.clone().unwrap_or_default());
    let mut env = Vec::new();
    if record.model_access == "balance" {
        match gateway_key() {
            Ok(key) => env = gateway_env(&record.agent, &key),
            Err(message) => {
                let _ = append_log(
                    &store,
                    id,
                    &format!("lean-ctx run: no model access: {message}\n"),
                );
                end(
                    &store,
                    &mut record,
                    store::STATE_FAILED,
                    Some("model_access"),
                    None,
                );
                return Ok(());
            }
        }
    }
    let log = open_log(&store, id)?;
    let (program, agent_args) = agent_command(&record.agent);
    let mut command = Command::new(program);
    command
        .args(agent_args)
        .current_dir(&worktree)
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(
            log.try_clone().map_err(|e| internal(e.to_string()))?,
        ));
    detach(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let reason = if e.kind() == std::io::ErrorKind::NotFound {
                "agent_missing"
            } else {
                "agent_start"
            };
            let _ = append_log(
                &store,
                id,
                &format!("lean-ctx run: cannot start {program}: {e}\n"),
            );
            end(&store, &mut record, store::STATE_FAILED, Some(reason), None);
            return Ok(());
        }
    };
    record.pid = Some(child.id());
    record.state = store::STATE_RUNNING.into();
    store.save(&record).map_err(internal)?;
    let mut started = Map::new();
    started.insert("agent".into(), record.agent.clone().into());
    started.insert("branch".into(), record.branch.clone().into());
    let _ = store.append_event(id, "started", started);
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(task.as_bytes());
    }
    let done = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::new(AtomicBool::new(false));
    if let Some(minutes) = args.max_minutes()? {
        watchdog(
            child.id(),
            minutes,
            Arc::clone(&done),
            Arc::clone(&timed_out),
        );
    }
    let mut tool_calls = 0;
    if let Some(stdout) = child.stdout.take() {
        let mut log = log;
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let _ = writeln!(log, "{line}");
            let found = tool_calls_in(&record.agent, &line);
            if found > 0 {
                tool_calls += found;
                let mut fields = Map::new();
                fields.insert("tool_calls".into(), tool_calls.into());
                let _ = store.append_event(id, "tool_calls", fields);
                if let Ok(Some(mut now)) = store.load(id) {
                    now.tool_calls = tool_calls;
                    let _ = store.save(&now);
                }
            }
        }
    }
    let exit = child.wait().ok();
    done.store(true, Ordering::SeqCst);
    let mut record = store.load(id).map_err(internal)?.unwrap_or(record);
    record.tool_calls = tool_calls;
    record.commits = collect_commits(&store, &worktree, &record);
    let exit_code = exit.and_then(|s| s.code());
    let (state, reason) = if record.stop_reason.as_deref() == Some("user") {
        (store::STATE_STOPPED, Some("user"))
    } else if timed_out.load(Ordering::SeqCst) {
        (store::STATE_STOPPED, Some("max_minutes"))
    } else if exit.is_some_and(|s| s.success()) {
        (store::STATE_FINISHED, None)
    } else {
        (store::STATE_FAILED, Some("agent_exit"))
    };
    end(&store, &mut record, state, reason, exit_code);
    Ok(())
}

/// Stop the agent's process group after `minutes` (`SIGTERM`, `SIGKILL`
/// after [`KILL_GRACE`]).
fn watchdog(pid: u32, minutes: u64, done: Arc<AtomicBool>, timed_out: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(minutes * 60);
        while std::time::Instant::now() < deadline {
            if done.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        if !done.load(Ordering::SeqCst) {
            timed_out.store(true, Ordering::SeqCst);
            terminate_group(pid, KILL_GRACE);
        }
    });
}

/// Commit what the agent left uncommitted, then the run's commits (oldest
/// first), each recorded as a `commit` event.
fn collect_commits(store: &Store, worktree: &Path, record: &RunRecord) -> Vec<String> {
    let dirty = git(worktree, &["status", "--porcelain"]).unwrap_or_default();
    if !dirty.is_empty() && git(worktree, &["add", "-A"]).is_ok() {
        let message = format!(
            "lean-ctx run {}: uncommitted changes of the agent",
            record.id
        );
        let mut commit = vec!["commit", "--quiet", "--no-verify", "-m", &message];
        let named = git(worktree, &["config", "user.email"]).is_ok_and(|e| !e.is_empty());
        let identity = [
            "-c",
            "user.name=LeanCTX run",
            "-c",
            "user.email=run@leanctx.invalid",
        ];
        if !named {
            commit.splice(0..0, identity);
        }
        let _ = git(worktree, &commit);
    }
    let range = format!("{}..HEAD", record.base);
    let commits: Vec<String> = git(worktree, &["rev-list", "--reverse", &range])
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    for sha in commits.iter().filter(|sha| !record.commits.contains(sha)) {
        let mut fields = Map::new();
        fields.insert("sha".into(), sha.clone().into());
        let _ = store.append_event(&record.id, "commit", fields);
    }
    commits
}

fn open_log(store: &Store, id: &str) -> Result<std::fs::File, Failure> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(store.log_path(id))
        .map_err(|e| internal(format!("cannot open the agent log: {e}")))
}

fn append_log(store: &Store, id: &str, text: &str) -> Result<(), Failure> {
    open_log(store, id)?
        .write_all(text.as_bytes())
        .map_err(|e| internal(e.to_string()))
}

#[cfg(test)]
#[path = "run_cmd_tests.rs"]
mod tests;
