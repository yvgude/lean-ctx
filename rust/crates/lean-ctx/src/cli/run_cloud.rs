// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx run --cloud`: the run on a LeanCTX computer (contract §35).
//!
//! LeanCTX Cloud only starts the computer (`POST /v1/computers/{id}/start`
//! with `purpose: run`; billed from the first minute, after the person
//! confirmed the price). Everything else goes over SSH with the pinned host
//! keys: the base commit as a `git push` to a bare repository on the
//! computer, the task text on the stdin of `lean-ctx run start` there, the
//! run's events and logs, and the run branch back with `git fetch`.

use std::io::BufRead as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value};

use lean_ctx_cloud::computers::{self, Answer, RunStartRequest};

use super::super::cloud_cmd::cloud_computer_cmd::{
    self as computer_cmd, cloud as computer_cloud, ssh,
};
use super::store::{self, RunRecord, Store};
use super::{Args, Code, Failure, internal, local_read};

/// How long `run --cloud` waits for the computer to run.
const START_POLLS: u32 = 60;
const START_POLL_S: u64 = 5;

/// What `run --cloud` starts.
pub(super) struct Start<'a> {
    pub task: &'a str,
    pub agent: &'static str,
    pub access: &'static str,
    pub max_minutes: Option<u64>,
    pub root: &'a Path,
    pub base: &'a str,
}

/// The bare repository of a project on the computer, by a hash of the
/// project root: `~/.leanctx/repos/<hash>.git`.
pub(super) fn repo_hash(root: &Path) -> String {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(root.display().to_string().as_bytes());
    hex::encode(&digest[..8])
}

fn valid_repo_hash(hash: &str) -> bool {
    (16..=64).contains(&hash.len())
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The ref the base commit is pushed to.
pub(super) fn base_ref(id: &str) -> String {
    format!("refs/leanctx/runs/{id}/base")
}

/// The computer the run goes to, from `GET /v1/computers` (the only
/// request before the person confirms): `--computer <name|id>`, or the
/// only one. A cloud that offers no computers (`available: false`, no
/// route, `503 computer_not_available`) ends here with
/// `computer_not_available`, before any start is sent.
pub(super) fn chosen_row(
    send: computers::Exchange<'_>,
    args: &Args,
    instance: &lean_ctx_cloud::state::InstanceRecord,
) -> Result<Map<String, Value>, Failure> {
    let listed = match computers::list(send).map_err(Failure::from)? {
        Answer::Done(list) => list,
        Answer::Refused(refusal) => return Err(computer_cloud::refused(&refusal)),
        Answer::NotOffered => return Err(computer_cloud::not_offered()),
    };
    let row = match args.value("--computer") {
        Some(wanted) => computer_cloud::pick(&listed.computers, wanted, instance)?,
        None => match listed.computers.as_slice() {
            [one] => one,
            [] => {
                return Err(Failure::new(
                    Code::NoComputer,
                    "no LeanCTX computer (lean-ctx cloud computer create)",
                ));
            }
            _ => {
                return Err(Failure::invalid(
                    "you have several computers; name one with --computer (lean-ctx cloud computer list)",
                ));
            }
        },
    };
    Ok(row.clone())
}

/// `lean-ctx run --cloud "<task>"` up to the started run (recorded here
/// with `place: computer`).
pub(super) fn start(args: &Args, start: &Start<'_>) -> Result<RunRecord, Failure> {
    let (cloud_store, config, instance) = computer_cloud::connected()?;
    let send = computers::connected(&cloud_store, &config);
    let listed = chosen_row(&send, args, &instance)?;
    let row = &listed;
    let computer_id = text(row, "id").to_owned();
    let name = text(row, "name").to_owned();
    let request_id = match args.value("--request-id") {
        Some(id) if computers::valid_request_id(id) => id.to_owned(),
        Some(id) => {
            return Err(Failure::invalid(format!(
                "--request-id '{id}' is not valid"
            )));
        }
        None => computers::new_request_id(),
    };
    let attended = lean_ctx::core::host_env::stdin_is_terminal();
    let row = start_paid(&send, row, &request_id, args, attended)?;
    let row = wait_running(&send, &computer_id, row, args.json())?;
    let paths = computer_cmd::ssh_paths().map_err(internal)?;
    let computer = ensure_registered(&paths, &name, &row)?;
    let hash = repo_hash(start.root);
    let id = store::new_id();
    ssh_output(
        &computer,
        &paths,
        &["lean-ctx", "run", "receive", "--init", "--repo", &hash],
        None,
    )?;
    push_base(start.root, &computer, &paths, &hash, start.base, &id)?;
    let max = start.max_minutes.map(|m| m.to_string());
    let mut command = vec![
        "lean-ctx",
        "run",
        "start",
        "--id",
        &id,
        "--agent",
        start.agent,
        "--repo",
        &hash,
        "--model-access",
        start.access,
        "--json",
    ];
    if let Some(max) = &max {
        command.extend(["--max-minutes", max]);
    }
    let out = ssh_output(&computer, &paths, &command, Some(start.task))?;
    let doc = remote_doc(&out, super::KIND_ACK)?;
    let mut record = RunRecord {
        schema: store::RECORD_SCHEMA.into(),
        id: id.clone(),
        place: store::PLACE_COMPUTER.into(),
        computer: Some(name),
        project_root: start.root.display().to_string(),
        agent: start.agent.into(),
        model_access: start.access.into(),
        state: store::STATE_STARTING.into(),
        started_at: store::now(),
        ended_at: None,
        branch: store::branch_of(&id),
        base: start.base.into(),
        commits: Vec::new(),
        tool_calls: 0,
        exit_code: None,
        stop_reason: None,
        worktree: None,
        pid: None,
        unit: None,
    };
    apply_view(&mut record, &doc["run"]);
    let local = Store::open().map_err(internal)?;
    local.create(&record, start.task).map_err(internal)?;
    Ok(record)
}

fn text<'a>(row: &'a Map<String, Value>, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The refusal of a paid start that no person asked for: the very text of
/// `cloud computer create|start` (owner rule, §44).
pub(super) const UNATTENDED: &str = computer_cloud::UNATTENDED;

/// Confirm the run's terms (from the list row, no request) and only then
/// start computer `row` for it. Only a person starts a paid computer:
/// `attended` is a CLI whose stdin is a terminal (no environment variable
/// counts, an agent could set it); otherwise nothing is sent and the answer
/// is [`UNATTENDED`]. Without `--yes` and without a yes at the
/// prompt nothing is sent: the person gets `confirmation_required` with the
/// terms in `error.details`. A `428` in answer to the confirmed start is an
/// error, never asked again.
pub(super) fn start_paid(
    send: computers::Exchange<'_>,
    row: &Map<String, Value>,
    request_id: &str,
    args: &Args,
    attended: bool,
) -> Result<Map<String, Value>, Failure> {
    let name = text(row, "name");
    if !attended {
        return Err(Failure::new(Code::ConfirmationRequired, UNATTENDED)
            .with_details(terms_details(&computers::run_terms(row), topup_state(row))));
    }
    confirm(args, row, &computers::run_terms(row))?;
    let request = RunStartRequest::confirmed(request_id.to_owned());
    match computers::start_for_run(send, text(row, "id"), &request).map_err(Failure::from)? {
        Answer::Done(row) => Ok(row),
        Answer::NotOffered => Err(computer_cloud::not_offered()),
        Answer::Refused(refusal)
            if refusal.code == "confirmation_required" || refusal.status == 428 =>
        {
            let terms = computers::terms(refusal.details.as_ref());
            Err(Failure::new(
                Code::ConfirmationRequired,
                format!(
                    "LeanCTX Cloud asked to confirm the confirmed run on computer '{name}' again; nothing was started. {}",
                    price_line(&terms)
                ),
            )
            .with_details(terms_details(&terms, topup_state(row))))
        }
        Answer::Refused(refusal) => Err(computer_cloud::refused(&refusal)),
    }
}

/// The person's confirmation of `terms`: `--yes`, or yes at the prompt of
/// a terminal (never under `--json`).
fn confirm(args: &Args, row: &Map<String, Value>, terms: &computers::Terms) -> Result<(), Failure> {
    if args.has("--yes") {
        return Ok(());
    }
    let name = text(row, "name");
    let asked = format!("A run on computer '{name}' is billed from the first minute.");
    let price = price_line(terms);
    if !args.json() && lean_ctx::core::host_env::stdin_is_terminal() {
        eprintln!("{asked}");
        eprintln!("{price}");
        if super::super::prompt::confirm("Start the run?", false) {
            return Ok(());
        }
        return Err(Failure::new(
            Code::ConfirmationRequired,
            "not started: the price was not confirmed",
        ));
    }
    Err(Failure::new(
        Code::ConfirmationRequired,
        format!("{asked} {price} Nothing was started; repeat with --yes to start the run."),
    )
    .with_details(terms_details(terms, topup_state(row))))
}

/// `error.details` of a run's `confirmation_required`, in the §44
/// vocabulary: `{billing, usd_per_hour, topup_usd, topup_state}`. A run is
/// always `billed`; `topup_state` is the list row's `balance.state`.
pub(super) fn terms_details(terms: &computers::Terms, topup_state: Option<&str>) -> Value {
    serde_json::json!({
        "billing": terms.billing,
        "usd_per_hour": terms.usd_per_hour,
        "topup_usd": terms.topup_usd,
        "topup_state": topup_state,
    })
}

/// The row's `balance.state` (the top-up's), when it names one.
pub(super) fn topup_state(row: &Map<String, Value>) -> Option<&str> {
    row.get("balance")
        .and_then(|b| b.get("state"))
        .and_then(Value::as_str)
}

/// The price of a run, from its terms (list row or the cloud's `428`), for
/// people; an unknown price is said to be unknown, never $0.
pub(super) fn price_line(terms: &computers::Terms) -> String {
    let mut parts = Vec::new();
    match terms.billing {
        Some("free") => parts.push("on the team's free hours".to_owned()),
        Some(_) => parts.push("billed from the top-up".to_owned()),
        None => {}
    }
    match terms.usd_per_hour {
        Some(price) => parts.push(format!("${price:.2} per hour from the first minute")),
        None => parts.push("price per hour not reported".to_owned()),
    }
    if let Some(left) = terms.topup_usd {
        parts.push(format!("top-up ${left:.2} left"));
    }
    format!("Price: {}.", parts.join(", "))
}

/// Poll the computer until it runs.
fn wait_running(
    send: computers::Exchange<'_>,
    id: &str,
    mut row: Map<String, Value>,
    quiet: bool,
) -> Result<Map<String, Value>, Failure> {
    for poll in 0..=START_POLLS {
        match text(&row, "state") {
            "running" => return Ok(row),
            "failed" | "deleted" => {
                return Err(Failure::new(
                    Code::Unavailable,
                    format!(
                        "the computer is {}; it cannot take the run",
                        text(&row, "state")
                    ),
                ));
            }
            _ => {}
        }
        if poll == START_POLLS {
            break;
        }
        if !quiet && poll == 0 {
            eprintln!("waiting for the computer to run…");
        }
        std::thread::sleep(std::time::Duration::from_secs(START_POLL_S));
        row = match computers::list(send).map_err(Failure::from)? {
            Answer::Done(list) => list
                .computers
                .into_iter()
                .find(|r| r.get("id").and_then(Value::as_str) == Some(id))
                .ok_or_else(|| Failure::new(Code::NoComputer, "the computer is gone"))?,
            Answer::Refused(refusal) => return Err(computer_cloud::refused(&refusal)),
            Answer::NotOffered => return Err(computer_cloud::not_offered()),
        };
    }
    Err(Failure::new(
        Code::Unavailable,
        "the computer did not start in time; try again later (lean-ctx cloud computer list)",
    ))
}

/// The registration of computer `name`; pinned now from the fingerprints
/// LeanCTX Cloud lists for it when this machine has none yet.
pub(super) fn ensure_registered(
    paths: &ssh::Paths,
    name: &str,
    row: &Map<String, Value>,
) -> Result<ssh::Computer, Failure> {
    let mut registry = ssh::Registry::load(&paths.registry).map_err(internal)?;
    if let Some(computer) = registry.get(name) {
        return Ok(computer.clone());
    }
    let address = row.get("ssh").and_then(Value::as_object).ok_or_else(|| {
        Failure::new(
            Code::Unavailable,
            "LeanCTX Cloud lists no SSH address for the computer yet",
        )
    })?;
    let hostname = text(address, "hostname");
    // The person's own user on the computer, as `cloud computer add` takes
    // it (`lc-` and 8-16 of a-z 0-9); never a default.
    let user = address.get("user").and_then(Value::as_str).ok_or_else(|| {
        Failure::new(
            Code::Unavailable,
            format!("LeanCTX Cloud names no SSH user for '{name}'; nothing was registered"),
        )
    })?;
    if !computer_cloud::cloud_user(user) {
        return Err(Failure::new(
            Code::Unavailable,
            format!(
                "LeanCTX Cloud sent an unexpected SSH user for '{name}'; nothing was registered"
            ),
        ));
    }
    let port = address
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|p| u16::try_from(p).ok())
        .unwrap_or(22);
    let wanted: Vec<String> = address
        .get("host_key_fingerprints")
        .and_then(Value::as_array)
        .map(|keys| {
            keys.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    ssh::check_name(name).map_err(Failure::invalid)?;
    ssh::check_hostname(hostname).map_err(|e| Failure::new(Code::Unavailable, e))?;
    ssh::check_user(user).map_err(|e| Failure::new(Code::Unavailable, e))?;
    if wanted.is_empty() {
        return Err(Failure::new(
            Code::Unavailable,
            "LeanCTX Cloud lists no host key for the computer",
        ));
    }
    for fingerprint in &wanted {
        ssh::check_fingerprint(fingerprint).map_err(|e| Failure::new(Code::Unavailable, e))?;
    }
    let scan = computer_cmd::keyscan_text(hostname, port)
        .map_err(|e| Failure::new(Code::Unavailable, e))?;
    let keys =
        ssh::keys_matching(&scan, &wanted).map_err(|e| Failure::new(Code::Unavailable, e))?;
    let computer = ssh::Computer {
        name: name.to_owned(),
        hostname: hostname.to_owned(),
        user: user.to_owned(),
        port,
        identity_file: None,
        host_keys: keys.iter().map(ssh::HostKey::openssh).collect(),
    };
    registry
        .upsert(computer.clone(), false)
        .map_err(Failure::invalid)?;
    registry.save(&paths.registry).map_err(internal)?;
    ssh::refresh(paths, &registry).map_err(internal)?;
    Ok(computer)
}

/// `ssh` to `computer` with the pin enforced: the remote command's stdout.
/// An unreachable computer is `unavailable`.
fn ssh_output(
    computer: &ssh::Computer,
    paths: &ssh::Paths,
    command: &[&str],
    stdin: Option<&str>,
) -> Result<String, Failure> {
    let command: Vec<String> = command.iter().map(|s| (*s).to_owned()).collect();
    let mut child = Command::new("ssh")
        .args(ssh::ssh_args(computer, &paths.known_hosts, &command))
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Failure::new(Code::Unavailable, format!("cannot run ssh: {e}")))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(text.as_bytes());
    }
    let output = child
        .wait_with_output()
        .map_err(|e| Failure::new(Code::Unavailable, format!("ssh: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.code() == Some(255) || (!output.status.success() && stdout.trim().is_empty()) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("no answer");
        return Err(Failure::new(
            Code::Unavailable,
            format!(
                "computer '{}' over SSH: {}",
                computer.name,
                remote_text(last)
            ),
        ));
    }
    Ok(stdout)
}

/// Longest text from the computer shown to the person.
const REMOTE_TEXT_MAX: usize = 300;

/// Text from the computer (an error message, the last stderr line) as the
/// person sees it: credentials redacted, control characters dropped,
/// at most [`REMOTE_TEXT_MAX`] characters.
pub(super) fn remote_text(text: &str) -> String {
    let redacted = lean_ctx_protocol::credential_redaction::redact_text(text);
    redacted
        .chars()
        .filter(|c| !c.is_control())
        .take(REMOTE_TEXT_MAX)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The Local Read document the computer printed (the last JSON line); its
/// `error` document as a failure.
pub(super) fn remote_doc(stdout: &str, kind: &str) -> Result<Value, Failure> {
    let doc: Value = stdout
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str(line.trim()).ok())
        .ok_or_else(|| {
            internal("the computer printed no Local Read document (is lean-ctx installed there?)")
        })?;
    if doc["kind"] == "error" {
        let code = doc["error"]["code"]
            .as_str()
            .filter(|c| {
                !c.is_empty()
                    && c.len() <= 40
                    && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
            })
            .unwrap_or("internal");
        let message = remote_text(doc["error"]["message"].as_str().unwrap_or_default());
        let local = match code {
            "invalid_argument" => Code::InvalidArgument,
            "not_found" => Code::NotFound,
            "conflict" => Code::Conflict,
            _ => Code::Unavailable,
        };
        return Err(Failure::new(
            local,
            format!("on the computer: {message} ({code})"),
        ));
    }
    if doc["kind"] != kind {
        return Err(internal(format!(
            "the computer answered {} for {kind}",
            doc["kind"]
        )));
    }
    Ok(doc)
}

/// The SSH URL of the bare repository `hash` on `computer`.
pub(super) fn repo_url(computer: &ssh::Computer, hash: &str) -> String {
    let host = if computer.hostname.contains(':') {
        format!("[{}]", computer.hostname)
    } else {
        computer.hostname.clone()
    };
    format!(
        "ssh://{}@{host}:{}/~/.leanctx/repos/{hash}.git",
        computer.user, computer.port
    )
}

/// `GIT_SSH_COMMAND` with the pin enforced (git adds port and host).
pub(super) fn git_ssh_command(computer: &ssh::Computer, known_hosts: &Path) -> String {
    let mut args = ssh::ssh_args(computer, known_hosts, &[]);
    args.pop(); // user@host: git names it.
    if let Some(at) = args.iter().position(|a| a == "-p") {
        args.drain(at..=at + 1);
    }
    let quoted: Vec<String> = args
        .iter()
        .map(|a| format!("'{}'", a.replace('\'', "'\\''")))
        .collect();
    format!("ssh {}", quoted.join(" "))
}

/// Push the base commit to the computer's bare repository.
fn push_base(
    root: &Path,
    computer: &ssh::Computer,
    paths: &ssh::Paths,
    hash: &str,
    base: &str,
    id: &str,
) -> Result<(), Failure> {
    let refspec = format!("{base}:{}", base_ref(id));
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "push",
            "--quiet",
            "--no-verify",
            &repo_url(computer, hash),
            &refspec,
        ])
        .env(
            "GIT_SSH_COMMAND",
            git_ssh_command(computer, &paths.known_hosts),
        )
        .stdin(Stdio::null())
        .output()
        .map_err(|e| internal(format!("cannot run git: {e}")))?;
    if output.status.success() {
        return Ok(());
    }
    Err(Failure::new(
        Code::Unavailable,
        format!(
            "cannot push the base commit to computer '{}': {}",
            computer.name,
            remote_text(&String::from_utf8_lossy(&output.stderr))
        ),
    ))
}

/// The computer of a `place: computer` run.
fn computer_of(record: &RunRecord) -> Result<(ssh::Computer, ssh::Paths), Failure> {
    let name = record
        .computer
        .as_deref()
        .ok_or_else(|| internal(format!("run {} names no computer", record.id)))?;
    let paths = computer_cmd::ssh_paths().map_err(internal)?;
    let registry = ssh::Registry::load(&paths.registry).map_err(internal)?;
    let computer = registry.get(name).cloned().ok_or_else(|| {
        Failure::new(
            Code::NoComputer,
            format!("computer '{name}' is not registered on this machine"),
        )
    })?;
    Ok((computer, paths))
}

/// Take the computer's view of the run into the local record.
fn apply_view(record: &mut RunRecord, view: &Value) {
    let text = |k: &str| view[k].as_str().map(str::to_owned);
    if let Some(state) = text("state") {
        record.state = state;
    }
    if store::is_final(&record.state) && record.ended_at.is_none() {
        record.ended_at = Some(store::now());
    }
    record.stop_reason = text("stop_reason");
    record.exit_code = view["exit_code"]
        .as_i64()
        .and_then(|c| i32::try_from(c).ok());
    record.tool_calls = view["tool_calls"].as_u64().unwrap_or(record.tool_calls);
    if let Some(commits) = view["commits"].as_array() {
        record.commits = commits
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
    }
}

/// Refresh a `place: computer` run from the computer. An unreachable
/// computer that LeanCTX Cloud lists as stopped ends the run here
/// ([`ended_with_computer`]); otherwise the run stays as it was and the
/// answer is `unavailable`.
pub(super) fn refresh(store: &Store, record: &mut RunRecord) -> Result<(), Failure> {
    if store::is_final(&record.state) {
        return Ok(());
    }
    let (computer, paths) = computer_of(record)?;
    let out = match ssh_output(
        &computer,
        &paths,
        &["lean-ctx", "run", "status", &record.id, "--json"],
        None,
    ) {
        Ok(out) => out,
        Err(failure) if failure.code == Code::Unavailable => {
            let Some(reason) = listed_row(&computer.name)
                .ok()
                .flatten()
                .and_then(|row| ended_with_computer(&row))
            else {
                return Err(failure);
            };
            super::end(store, record, store::STATE_STOPPED, Some(reason), None);
            return Ok(());
        }
        Err(failure) => return Err(failure),
    };
    let doc = remote_doc(&out, super::KIND_STATUS)?;
    apply_view(record, &doc["run"]);
    store.save(record).map_err(internal)
}

/// The list row of computer `name` from LeanCTX Cloud, when connected.
fn listed_row(name: &str) -> Result<Option<Map<String, Value>>, Failure> {
    let (cloud_store, config, instance) = computer_cloud::connected()?;
    let send = computers::connected(&cloud_store, &config);
    let Answer::Done(list) = computers::list(&send).map_err(Failure::from)? else {
        return Ok(None);
    };
    Ok(computer_cloud::pick(&list.computers, name, &instance)
        .ok()
        .cloned())
}

/// The `stop_reason` of a run whose computer LeanCTX Cloud lists as no
/// longer running (§35): `balance_empty` when the cloud stopped it because
/// the balance is used up (`stop_reason: balance_empty` on the row), else
/// `computer_stopped`. `None` while the computer runs or starts: the run
/// is then only out of reach.
pub(super) fn ended_with_computer(row: &Map<String, Value>) -> Option<&'static str> {
    match text(row, "state") {
        "stopped" | "stopping" | "deleted" | "failed" => {
            Some(if text(row, "stop_reason") == "balance_empty" {
                "balance_empty"
            } else {
                "computer_stopped"
            })
        }
        _ => None,
    }
}

pub(super) fn remote_stop(store: &Store, record: &mut RunRecord) -> Result<(), Failure> {
    let (computer, paths) = computer_of(record)?;
    let out = ssh_output(
        &computer,
        &paths,
        &["lean-ctx", "run", "stop", &record.id, "--json"],
        None,
    )?;
    let doc = remote_doc(&out, super::KIND_STATUS)?;
    apply_view(record, &doc["run"]);
    store.save(record).map_err(internal)
}

pub(super) fn remote_events(
    record: &RunRecord,
    since: u64,
    limit: u64,
) -> Result<Vec<Value>, Failure> {
    let (computer, paths) = computer_of(record)?;
    let (since, limit) = (since.to_string(), limit.to_string());
    let command = [
        "lean-ctx", "run", "events", &record.id, "--since", &since, "--limit", &limit, "--json",
    ];
    let out = ssh_output(&computer, &paths, &command, None)?;
    let doc = remote_doc(&out, super::KIND_EVENTS)?;
    Ok(doc["events"].as_array().cloned().unwrap_or_default())
}

pub(super) fn remote_logs(record: &RunRecord, follow: bool) -> Result<(), Failure> {
    let (computer, paths) = computer_of(record)?;
    let mut command = vec![
        "lean-ctx".to_owned(),
        "run".into(),
        "logs".into(),
        record.id.clone(),
    ];
    if follow {
        command.push("--follow".into());
    }
    let status = Command::new("ssh")
        .args(ssh::ssh_args(&computer, &paths.known_hosts, &command))
        .stdin(Stdio::null())
        .status()
        .map_err(|e| Failure::new(Code::Unavailable, format!("cannot run ssh: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Failure::new(
            Code::Unavailable,
            format!("computer '{}': logs failed", computer.name),
        ))
    }
}

/// Follow the run's events on the computer (one SSH session) until it
/// ends; `true` when it ended.
pub(super) fn follow(record: &RunRecord, json_lines: bool) -> Result<bool, Failure> {
    let (computer, paths) = computer_of(record)?;
    let command: Vec<String> = [
        "lean-ctx", "run", "events", &record.id, "--follow", "--json",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let mut child = Command::new("ssh")
        .args(ssh::ssh_args(&computer, &paths.known_hosts, &command))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| Failure::new(Code::Unavailable, format!("cannot run ssh: {e}")))?;
    let mut ended = false;
    if let Some(stdout) = child.stdout.take() {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(event) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            super::print_event(&event, json_lines);
            if super::is_final_event(&event) {
                ended = true;
                break;
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    if ended {
        let store = Store::open().map_err(internal)?;
        let mut record = record.clone();
        refresh(&store, &mut record)?;
    }
    Ok(ended)
}

/// `lean-ctx run fetch <id>`: the run branch into this repository
/// (`refs/remotes/leanctx-<name>/run/<id>` and the branch `leanctx/run/<id>`).
pub(super) fn fetch(args: &Args) -> Result<(), Failure> {
    let (store, mut record) = super::named_run(args)?;
    if record.place == store::PLACE_COMPUTER {
        fetch_branch(&record)?;
        refresh(&store, &mut record)?;
    }
    super::print_status(args, &record);
    Ok(())
}

/// After a followed run ended: fetch its branch, say where it is.
pub(super) fn fetch_quietly(record: &RunRecord) -> Result<(), Failure> {
    fetch_branch(record)?;
    println!(
        "fetched: branch {} in {}",
        record.branch, record.project_root
    );
    Ok(())
}

fn fetch_branch(record: &RunRecord) -> Result<(), Failure> {
    let (computer, paths) = computer_of(record)?;
    let root = PathBuf::from(&record.project_root);
    let hash = repo_hash(&root);
    let remote_ref = format!("refs/remotes/{}/run/{}", computer.alias(), record.id);
    let refspec = format!("+refs/heads/{}:{remote_ref}", record.branch);
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args([
            "fetch",
            "--quiet",
            "--no-tags",
            &repo_url(&computer, &hash),
            &refspec,
        ])
        .env(
            "GIT_SSH_COMMAND",
            git_ssh_command(&computer, &paths.known_hosts),
        )
        .stdin(Stdio::null())
        .output()
        .map_err(|e| internal(format!("cannot run git: {e}")))?;
    if !output.status.success() {
        return Err(Failure::new(
            Code::Unavailable,
            format!(
                "cannot fetch {} from computer '{}': {}",
                record.branch,
                computer.name,
                remote_text(&String::from_utf8_lossy(&output.stderr))
            ),
        ));
    }
    update_local_branch(&root, &record.branch, &remote_ref)
}

/// Bring the branch `branch` to the fetched `remote_ref`: create it when
/// missing, fast-forward it otherwise. A branch with commits of its own
/// (not an ancestor of the fetched tip) is never moved: `conflict`. The
/// checked-out branch is not moved under the working tree either: the
/// person merges it (`git merge --ff-only <remote_ref>`).
pub(super) fn update_local_branch(
    root: &Path,
    branch: &str,
    remote_ref: &str,
) -> Result<(), Failure> {
    let local = format!("refs/heads/{branch}");
    let Ok(old) = super::git(root, &["rev-parse", "--verify", "--quiet", &local]) else {
        super::git(root, &["branch", "--quiet", branch, remote_ref]).map_err(internal)?;
        return Ok(());
    };
    let new =
        super::git(root, &["rev-parse", "--verify", "--quiet", remote_ref]).map_err(internal)?;
    if old == new {
        return Ok(());
    }
    if super::git(root, &["merge-base", "--is-ancestor", &old, &new]).is_err() {
        return Err(Failure::new(
            Code::Conflict,
            format!(
                "branch {branch} has commits that are not on the computer; it was left as is (the run's tip is {remote_ref})"
            ),
        ));
    }
    let current = super::git(root, &["symbolic-ref", "--quiet", "HEAD"]).unwrap_or_default();
    if current == local {
        return Err(Failure::new(
            Code::Conflict,
            format!(
                "branch {branch} is checked out; it was left as is (git merge --ff-only {remote_ref})"
            ),
        ));
    }
    super::git(root, &["update-ref", &local, &new, &old]).map_err(internal)?;
    Ok(())
}

/// The bare repositories of `run --cloud` on this computer.
fn repos_dir() -> Result<PathBuf, Failure> {
    let home = dirs::home_dir().ok_or_else(|| internal("no home directory"))?;
    Ok(home.join(".leanctx").join("repos"))
}

fn repo_arg(args: &Args) -> Result<PathBuf, Failure> {
    let hash = args
        .value("--repo")
        .filter(|h| valid_repo_hash(h))
        .ok_or_else(|| Failure::invalid("--repo needs the repository hash"))?;
    Ok(repos_dir()?.join(format!("{hash}.git")))
}

/// `lean-ctx run receive --init --repo <hash>` (internal, on the computer):
/// the bare repository `run --cloud` pushes the base commit to.
pub(super) fn receive(args: &Args) -> Result<(), Failure> {
    if !args.has("--init") {
        return Err(Failure::invalid("receive needs --init"));
    }
    let repo = repo_arg(args)?;
    if repo.join("HEAD").is_file() {
        return Ok(());
    }
    create_private_dir(&repo)?;
    let output = Command::new("git")
        .args(["init", "--quiet", "--bare"])
        .arg(&repo)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| internal(format!("cannot run git: {e}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(internal(format!(
            "git init: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn create_private_dir(path: &Path) -> Result<(), Failure> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|e| internal(format!("cannot create {}: {e}", path.display())))
}

/// `lean-ctx run start --id <id> --agent <a> --repo <hash> --model-access
/// <m>` (internal, on the computer; the task on stdin): the run from the
/// pushed base commit, answered with `run.ack`.
pub(super) fn remote_start(args: &Args) -> Result<(), Failure> {
    let id = args
        .value("--id")
        .filter(|id| store::valid_id(id))
        .ok_or_else(|| Failure::invalid("start needs --id run_…"))?;
    let repo = repo_arg(args)?;
    let task = super::check_task(super::read_stdin()?)?;
    let base = super::git(
        &repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{}^{{commit}}", base_ref(id)),
        ],
    )
    .map_err(|_| {
        Failure::new(
            Code::NotFound,
            format!("no base commit for {id} (push it first)"),
        )
    })?;
    let launch = super::Launch {
        id: id.to_owned(),
        project_root: repo.display().to_string(),
        repo,
        base,
        agent: args.agent()?,
        access: args.model_access(true)?,
        max_minutes: args.max_minutes()?,
    };
    let store = Store::open().map_err(internal)?;
    let record = launch.run(&store, &task)?;
    local_read::emit(super::KIND_ACK, || {
        Ok(serde_json::json!({ "action": "start", "run": record.view() }))
    });
    Ok(())
}
