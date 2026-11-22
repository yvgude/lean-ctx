// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx cloud computer list|create|start|stop`: the LeanCTX computers of
//! the person and their teams, as LeanCTX Cloud serves them (`/v1/computers`,
//! [`lean_ctx_cloud::computers`]), for the terminal and for LeanCTX Code
//! (Local Read kinds `cloud.computers` and `cloud.computer.ack`).
//!
//! LeanCTX Cloud decides who may create or start a computer, its role, the
//! free hours, the top-up and the trial; the CLI passes the request on and
//! shows the cloud's refusal as it was given. Computer minutes after the
//! free hours are paid from the top-up only.
//!
//! `create` and `start` cost money, so they fail closed. They ask for a
//! person (owner rule): a CLI whose stdin is a terminal, or a call through
//! the 0600 Local Service socket ([`Via::Socket`], LeanCTX Code); any other
//! caller is refused with `confirmation_required` before anything is sent,
//! `--yes` or not. This guards against an agent starting a computer by
//! accident; it is not a security boundary: a process of the same user can
//! call the socket or fake a terminal. Without `--yes` nothing is sent
//! either: the CLI reads the
//! list (`GET`, no change) for the terms the cloud last reported and ends
//! with `confirmation_required` (exit 1), the terms in `error.details`. With
//! `--yes` the request carries `confirm: true`; a cloud that still answers
//! `428 confirmation_required` is passed on the same way (a second guard).
//! Every write goes through the release point (G08) before it is sent.
//! `stop` always runs.

use serde::Serialize;
use serde_json::{Map, Value, json};

use lean_ctx_cloud::CloudStore;
use lean_ctx_cloud::computers::{
    self, Answer, ComputerList, CreateRequest, Exchange, ROLE_GENERAL, ROLE_WEAVE_HOST, ROLES,
    Refusal, Terms, Transition,
};
use lean_ctx_cloud::state::InstanceRecord;

use super::Args;
use super::ssh;
use crate::cli::local_read::{self, Code, Failure, Via};

/// Local Read kind of the list.
pub(super) const KIND_LIST: &str = "cloud.computers";
/// Local Read kind of a write's answer.
pub(super) const KIND_ACK: &str = "cloud.computer.ack";
/// The CLI tool run a write's release decision is made in (as the other
/// Cloud writes: billing, team stop).
const TOOL: &str = "ctx_agent";
/// The name a Weave host is created under when none is given (LeanCTX Code).
const WEAVE_HOST_NAME: &str = "weave-host";

/// What a write is about to do or did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Create,
    Start,
    Stop,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Start => "start",
            Self::Stop => "stop",
        }
    }
}

/// The refusal of a paid start that no person asked for.
pub(crate) const UNATTENDED: &str =
    "Starting a paid computer needs you at a terminal or in LeanCTX Code.";

/// Whether a person asked (owner rule): a call over the 0600 Local Service
/// socket (LeanCTX Code's channel, as for `tasks decide|stop`), or a CLI
/// whose stdin is a terminal. No environment variable counts: an agent
/// could set it. A guard against accidental agent calls, not a security
/// boundary: any process of the same user can reach the socket or run the
/// CLI under a pseudo-terminal (follow-up: a peer check of the socket
/// client, or the confirmation in LeanCTX Cloud).
fn attended(via: Via, terminal: bool) -> bool {
    via == Via::Socket || terminal
}

/// `cloud computer list|create|start|stop`.
pub(super) fn run(verb: &str, args: &Args) {
    let json = args.json();
    let terminal = lean_ctx::core::host_env::stdin_is_terminal();
    let result = connected().and_then(|(store, config, instance)| {
        let send = computers::connected(&store, &config);
        let ctx = Ctx {
            send: &send,
            instance: &instance,
            args,
            via: Via::Cli,
            attended: attended(Via::Cli, terminal),
        };
        match verb {
            "list" => list(&ctx),
            "create" => create(&ctx).and_then(|write| finish(&ctx, write)),
            "start" => transition(&ctx, Action::Start).and_then(|write| finish(&ctx, write)),
            _ => transition(&ctx, Action::Stop).and_then(|write| finish(&ctx, write)),
        }
    });
    if let Err(failure) = result {
        if json {
            local_read::fail(&failure);
        }
        eprintln!("cloud computer: {}", failure.message);
        std::process::exit(failure.code.exit_status());
    }
}

/// The answer of `cloud computer list|create|start|stop` for LeanCTX Code
/// (`Via::Socket`): the same document `--json` prints.
pub(super) fn answer(args: &Args, via: Via) -> local_read::Answer {
    let verb = args.positionals.first().map_or("", String::as_str);
    connected()
        .map(|(store, config, instance)| {
            let send = computers::connected(&store, &config);
            let ctx = Ctx {
                send: &send,
                instance: &instance,
                args,
                via,
                attended: attended(via, false),
            };
            answer_of(&ctx, verb)
        })
        .unwrap_or_else(|failure| local_read::Answer::failure(&failure))
}

fn answer_of(ctx: &Ctx<'_>, verb: &str) -> local_read::Answer {
    let write = match verb {
        "list" => return list_answer(ctx),
        "create" => create(ctx),
        "start" => transition(ctx, Action::Start),
        "stop" => transition(ctx, Action::Stop),
        other => Err(Failure::invalid(format!(
            "unknown computer command {other:?}: list, create, start or stop"
        ))),
    };
    match write {
        Ok(write) => write_answer(ctx, write),
        Err(failure) => local_read::Answer::failure(&failure),
    }
}

struct Ctx<'a> {
    send: Exchange<'a>,
    instance: &'a InstanceRecord,
    args: &'a Args,
    /// The channel the document goes out on (the release point's).
    via: Via,
    /// A person asked ([`attended`]): only then may a paid write be sent.
    attended: bool,
}

/// Refuse a paid write no person asked for, before anything is sent.
fn require_person(ctx: &Ctx<'_>) -> Result<(), Failure> {
    if ctx.attended {
        Ok(())
    } else {
        Err(Failure::new(Code::ConfirmationRequired, UNATTENDED))
    }
}

pub(crate) fn connected() -> Result<
    (
        CloudStore,
        lean_ctx_cloud::config::CloudConfig,
        InstanceRecord,
    ),
    Failure,
> {
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    let store = CloudStore::open().map_err(Failure::from)?;
    let instance = store
        .instance()
        .map_err(Failure::from)?
        .ok_or_else(|| Failure::from(lean_ctx_cloud::CloudError::NotConnected))?;
    Ok((store, config, instance))
}

/// `cloud.computers`: the cloud's rows, passed on, plus the local SSH fact.
/// No local clock: the same answer gives the same bytes (#498).
#[derive(Debug, Serialize, PartialEq)]
struct ListDoc {
    /// `false`: this cloud does not offer computers.
    available: bool,
    live: bool,
    computers: Vec<Map<String, Value>>,
}

fn list_doc(
    answer: Answer<ComputerList>,
    written: impl Fn(&str, &Map<String, Value>) -> bool,
) -> Result<ListDoc, Failure> {
    match answer {
        Answer::Done(list) => Ok(ListDoc {
            available: true,
            live: true,
            computers: list
                .computers
                .into_iter()
                .map(|mut row| {
                    mark_config_written(&mut row, &written);
                    row
                })
                .collect(),
        }),
        // Not offered (`404`, `503 not_configured`, `200 {available:
        // false}`) or no provider (`503 computer_not_available`): the list
        // document LeanCTX Code shows as "not available yet", never an error.
        Answer::NotOffered => Ok(ListDoc {
            available: false,
            live: false,
            computers: Vec::new(),
        }),
        Answer::Refused(refusal) if refusal.code == "computer_not_available" => Ok(ListDoc {
            available: false,
            live: false,
            computers: Vec::new(),
        }),
        Answer::Refused(refusal) => Err(refused(&refusal)),
    }
}

/// `ssh.config_written` is a local fact (engine spec §2.2): whether this
/// machine's SSH config has the computer's entry, pointing at the row's own
/// host and user (and port, when sent).
fn mark_config_written(
    row: &mut Map<String, Value>,
    written: &impl Fn(&str, &Map<String, Value>) -> bool,
) {
    let name = row
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if let Some(Value::Object(ssh)) = row.get_mut("ssh") {
        let fact = written(&name, ssh);
        ssh.insert("config_written".into(), Value::Bool(fact));
    }
}

fn config_written(name: &str, row_ssh: &Map<String, Value>) -> bool {
    let text = |k: &str| row_ssh.get(k).and_then(Value::as_str);
    let (Some(hostname), Some(user)) = (text("hostname"), text("user")) else {
        return false;
    };
    let port = row_ssh
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|p| u16::try_from(p).ok());
    super::paths().is_ok_and(|paths| ssh::config_written_to(&paths, name, hostname, user, port))
}

fn fetch_list(ctx: &Ctx<'_>) -> Result<ComputerList, Failure> {
    match computers::list(ctx.send).map_err(Failure::from)? {
        Answer::Done(list) => Ok(list),
        Answer::NotOffered => Err(not_offered()),
        Answer::Refused(refusal) => Err(refused(&refusal)),
    }
}

/// `cloud.computers`, released for its channel.
fn list_answer(ctx: &Ctx<'_>) -> local_read::Answer {
    local_read::answer_released(TOOL, "", KIND_LIST, ctx.via, || {
        list_doc(
            computers::list(ctx.send).map_err(Failure::from)?,
            config_written,
        )
    })
}

fn list(ctx: &Ctx<'_>) -> Result<(), Failure> {
    if ctx.args.json() {
        local_read::print(&list_answer(ctx));
        return Ok(());
    }
    let doc = list_doc(
        computers::list(ctx.send).map_err(Failure::from)?,
        config_written,
    )?;
    if !doc.available {
        println!("{NOT_AVAILABLE}");
        super::print_local_registrations();
        return Ok(());
    }
    if doc.computers.is_empty() {
        println!("no LeanCTX computer yet (lean-ctx cloud computer create --help)");
    }
    for row in &doc.computers {
        let text = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or("-");
        println!(
            "{:<20} {:<12} {:<18} {}",
            text("name"),
            text("role"),
            text("state"),
            text("id")
        );
        if let Some(balance) = row_balance(row) {
            println!("  {}", topup_line(balance));
        }
    }
    super::print_local_registrations();
    Ok(())
}

/// "top-up $12.40 left (ok) · 494.8 free hours left this month · $0.09 per hour after them"
fn topup_line(balance: &Map<String, Value>) -> String {
    let num = |k: &str| balance_num(Some(balance), k);
    let mut parts = Vec::new();
    if let Some(usd) = num("usd") {
        let state = balance.get("state").and_then(Value::as_str).unwrap_or("?");
        parts.push(format!("top-up ${usd:.2} left ({state})"));
    }
    if let Some(free) = num("free_hours_left") {
        parts.push(format!("{free:.1} free hours left this month"));
    }
    if let Some(price) = num("usd_per_hour") {
        parts.push(format!("${price:.2} per hour after them"));
    }
    parts.join(" · ")
}

/// The team a write is for, as its org id: none when this machine is
/// connected to the person's own account, else the team it is connected to.
///
/// LeanCTX Cloud creates a computer for the scope this machine is connected
/// to, whatever the request names. So `--personal` is refused on a machine
/// connected to a team (it would create and bill a team computer), and
/// `--team` must name the connected team; both fail here, before anything
/// is sent.
fn team_of(args: &Args, instance: &InstanceRecord) -> Result<Option<String>, Failure> {
    let personal_machine = instance.org_kind.as_deref() == Some("personal");
    match (args.value("--team"), args.has("--personal")) {
        (Some(_), true) => Err(Failure::invalid("give --team or --personal, not both")),
        (Some(_), false) if personal_machine => Err(Failure::invalid(
            "--team: this machine is connected to your Personal account; a team computer is created from a machine connected to that team",
        )),
        (Some(team), false) if team == instance.org_id => Ok(Some(team.to_owned())),
        (Some(_), false) => Err(Failure::invalid(format!(
            "--team: this machine is connected to the team {} ({}); a computer for another team is created from a machine connected to that team",
            instance.org_name, instance.org_id
        ))),
        (None, true) if !personal_machine => Err(Failure::invalid(
            "this machine is connected to a team; a personal computer is created from a machine connected to your Personal account",
        )),
        (None, _) if personal_machine => Ok(None),
        (None, _) => Ok(Some(instance.org_id.clone())),
    }
}

fn request_id(args: &Args) -> Result<String, Failure> {
    match args.value("--request-id") {
        Some(id) if computers::valid_request_id(id) => Ok(id.to_owned()),
        Some(id) => Err(Failure::invalid(format!(
            "--request-id '{id}': use 8-64 of A-Z, a-z, 0-9, _ and -"
        ))),
        None => Ok(computers::new_request_id()),
    }
}

/// Who pays the first hours of a start, as the cloud's row says.
#[derive(Debug, Serialize, PartialEq)]
struct Billing {
    /// `free` (the team's free `weave_host` hours) or `billed` (from the
    /// top-up), as `error.details.billing`; `null` when the row does not say.
    basis: Option<&'static str>,
    free_hours_left: Option<f64>,
    usd_per_hour: Option<f64>,
    /// What is left of the top-up (the row's `balance.usd`).
    topup_usd: Option<f64>,
    topup_state: Option<String>,
}

/// The ack's billing. Numbers are finite and not negative, else `null`.
fn billing(row: &Map<String, Value>) -> Billing {
    let balance = row_balance(row);
    Billing {
        basis: computers::start_basis(row),
        free_hours_left: balance_num(balance, "free_hours_left"),
        usd_per_hour: row_price(row).or_else(|| balance_num(balance, "usd_per_hour")),
        topup_usd: balance_num(balance, "usd"),
        topup_state: balance
            .and_then(|b| b.get("state"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

fn row_balance(row: &Map<String, Value>) -> Option<&Map<String, Value>> {
    row.get("balance").and_then(Value::as_object)
}

/// The row's `price.usd_per_hour`, when finite and not negative.
fn row_price(row: &Map<String, Value>) -> Option<f64> {
    row.get("price")
        .and_then(|p| p.get("usd_per_hour"))
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
}

/// A number of the row's balance the terms may show: finite and not
/// negative, else unknown.
fn balance_num(balance: Option<&Map<String, Value>>, key: &str) -> Option<f64> {
    balance
        .and_then(|b| b.get(key))
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
}

/// The terms of starting `row`, from the list LeanCTX Cloud just sent.
fn start_terms(row: &Map<String, Value>) -> Terms {
    let balance = row_balance(row);
    Terms {
        billing: computers::start_basis(row),
        usd_per_hour: row_price(row).or_else(|| balance_num(balance, "usd_per_hour")),
        free_hours_left: balance_num(balance, "free_hours_left"),
        topup_usd: balance_num(balance, "usd"),
    }
}

/// The terms of creating a `role` computer for `team`, from the `balance`
/// of a listed computer in the same scope (it is the scope's, §4.3).
/// The free hours are a team's and only for `weave_host`, so anything else
/// is billed; a team's `weave_host` with no listed computer is unknown.
fn create_terms(rows: &[Map<String, Value>], team: Option<&str>, role: &str) -> Terms {
    let balance = rows
        .iter()
        .find(|row| in_scope(row, team))
        .and_then(row_balance);
    let free_hours_left = balance_num(balance, "free_hours_left");
    let billing = if team.is_none() || role != ROLE_WEAVE_HOST {
        Some("billed")
    } else {
        match free_hours_left {
            Some(hours) if hours > 0.0 => Some("free"),
            Some(_) => Some("billed"),
            None => None,
        }
    };
    Terms {
        billing,
        usd_per_hour: balance_num(balance, "usd_per_hour"),
        free_hours_left,
        topup_usd: balance_num(balance, "usd"),
    }
}

/// `cloud.computer.ack`: what the cloud did with a write.
#[derive(Debug, Serialize, PartialEq)]
struct AckDoc {
    action: &'static str,
    id: Value,
    name: Value,
    state: Value,
    role: Value,
    request_id: String,
    /// For `create` and `start`: who pays; `null` for `stop`.
    billing: Option<Billing>,
}

/// The ack of `row`; `listed` (the row `start`/`stop` found in the list)
/// lends the scope and balance an answer row left out.
fn ack(
    action: Action,
    row: &Map<String, Value>,
    request_id: String,
    listed: Option<&Map<String, Value>>,
) -> AckDoc {
    let pick = |k: &str| row.get(k).cloned().unwrap_or(Value::Null);
    let mut full = row.clone();
    if let Some(listed) = listed {
        for key in ["scope", "balance", "price", "role"] {
            if let (None, Some(value)) = (full.get(key), listed.get(key)) {
                full.insert(key.to_owned(), value.clone());
            }
        }
    }
    AckDoc {
        action: action.as_str(),
        id: pick("id"),
        name: pick("name"),
        state: pick("state"),
        role: pick("role"),
        request_id,
        billing: (action != Action::Stop).then(|| billing(&full)),
    }
}

/// Whether the person already confirmed the terms when a refusal came.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Asked {
    /// No `--yes`: nothing was sent.
    Unconfirmed,
    /// `--yes` was given and LeanCTX Cloud still answered `428`.
    Confirmed,
}

/// The refusal of a billable write that was not confirmed: nothing was
/// sent (or, the second guard, LeanCTX Cloud answered
/// `428 confirmation_required` to a confirmed one). `error.details` =
/// `{billing: free|billed|null, usd_per_hour, free_hours_left, topup_usd}`.
/// The free hours are a team's, so a personal computer is never shown as free.
fn confirmation(action: Action, name: &str, terms: Terms, personal: bool, asked: Asked) -> Failure {
    let mut terms = terms;
    if personal && terms.billing == Some("free") {
        terms.billing = Some("billed");
    }
    let left = terms
        .topup_usd
        .map(|usd| format!(" (top-up ${usd:.2} left)"))
        .unwrap_or_default();
    let next = match asked {
        Asked::Unconfirmed => format!(
            "Nothing was sent; repeat with --yes to {}.",
            action.as_str()
        ),
        // `--yes` was given: repeating it is not the answer.
        Asked::Confirmed => format!(
            "LeanCTX Cloud asked for a new confirmation, so nothing was done; check the terms with `lean-ctx cloud computer list` before you {} it again.",
            action.as_str()
        ),
    };
    Failure::new(
        Code::ConfirmationRequired,
        format!(
            "{} computer '{name}': {}{left}. {next}",
            action.as_str(),
            terms_text(&terms),
        ),
    )
    .with_details(json!({
        "billing": terms.billing,
        "usd_per_hour": terms.usd_per_hour,
        "free_hours_left": terms.free_hours_left,
        "topup_usd": terms.topup_usd,
    }))
}

/// Who pays, in words; an unknown price is said to be unknown, never $0.
fn terms_text(terms: &Terms) -> String {
    let price = terms.usd_per_hour.map(|p| format!("${p:.2} per hour"));
    match terms.billing {
        Some("free") => {
            let hours = terms
                .free_hours_left
                .map(|h| format!(" ({h:.1} left this month)"))
                .unwrap_or_default();
            let after = match price {
                Some(price) => format!(", then {price} from the top-up"),
                None => ", then billed at a price LeanCTX Cloud did not report".to_owned(),
            };
            format!("it runs on the team's free hours{hours}{after}")
        }
        Some(_) => match price {
            Some(price) => format!("it is billed to the top-up at {price}"),
            None => "it is billed; LeanCTX Cloud did not report the price per hour".to_owned(),
        },
        None => "LeanCTX Cloud did not say who pays".to_owned(),
    }
}

/// A `428 confirmation_required` from the cloud.
fn asks_confirmation(refusal: &Refusal) -> bool {
    refusal.status == 428 || refusal.code == "confirmation_required"
}

/// A write ready to be sent: the request's fields for the release point,
/// and the exchange that sends it and builds the ack.
struct Write<'a> {
    preview: Value,
    send: Box<dyn FnOnce() -> Result<AckDoc, Failure> + 'a>,
}

/// The role of a create: `weave_host` for a team, `general` for the
/// person's own account. A `weave_host` is for teams only (LeanCTX Cloud
/// answers `400 invalid_field role`), so a personal one is refused here,
/// before any terms are shown.
fn role_of(args: &Args, personal: bool) -> Result<&str, Failure> {
    let role = args.value("--role").unwrap_or(if personal {
        ROLE_GENERAL
    } else {
        ROLE_WEAVE_HOST
    });
    if !ROLES.contains(&role) {
        return Err(Failure::invalid(format!(
            "--role '{role}': use {}",
            ROLES.join(" or ")
        )));
    }
    if personal && role == ROLE_WEAVE_HOST {
        return Err(Failure::invalid(
            "--role weave_host: a Weave host is for teams; this machine is connected to your Personal account (use --role general)",
        ));
    }
    Ok(role)
}

/// `create`: checked, then refused before anything is sent unless `--yes`
/// confirmed the terms.
fn create<'a>(ctx: &Ctx<'a>) -> Result<Write<'a>, Failure> {
    let team = team_of(ctx.args, ctx.instance)?;
    let role = role_of(ctx.args, team.is_none())?;
    let name = match ctx
        .args
        .value("--name")
        .or_else(|| ctx.args.positionals.get(1).map(String::as_str))
    {
        Some(name) => name,
        None if role == ROLE_WEAVE_HOST => WEAVE_HOST_NAME,
        None => return Err(Failure::invalid("name the computer (--name <name>)")),
    };
    ssh::check_name(name).map_err(Failure::invalid)?;
    let request = CreateRequest {
        request_id: request_id(ctx.args)?,
        name: name.to_owned(),
        team,
        role: role.to_owned(),
        // Only a confirmed create is ever sent.
        confirm: true,
    };
    let personal = request.team.is_none();
    require_person(ctx)?;
    if !ctx.args.yes() {
        let list = fetch_list(ctx)?;
        let terms = create_terms(&list.computers, request.team.as_deref(), role);
        return Err(confirmation(
            Action::Create,
            name,
            terms,
            personal,
            Asked::Unconfirmed,
        ));
    }
    let preview = serde_json::to_value(&request)
        .map_err(|e| Failure::new(Code::Internal, format!("encode request: {e}")))?;
    let send = ctx.send;
    Ok(Write {
        preview,
        send: Box::new(
            move || match computers::create(send, &request).map_err(Failure::from)? {
                Answer::Done(row) => Ok(ack(Action::Create, &row, request.request_id, None)),
                Answer::NotOffered => Err(not_offered()),
                Answer::Refused(refusal) if asks_confirmation(&refusal) => Err(confirmation(
                    Action::Create,
                    &request.name,
                    computers::terms(refusal.details.as_ref()),
                    personal,
                    Asked::Confirmed,
                )),
                Answer::Refused(refusal) => Err(refused(&refusal)),
            },
        ),
    })
}

/// `start` or `stop` of a listed computer; a start is refused before
/// anything is sent unless `--yes` confirmed the terms.
fn transition<'a>(ctx: &Ctx<'a>, action: Action) -> Result<Write<'a>, Failure> {
    let wanted = ctx
        .args
        .positionals
        .get(1)
        .map(String::as_str)
        .ok_or_else(|| Failure::invalid("name the computer (or give its id)"))?;
    if !computers::valid_id(wanted) {
        ssh::check_name(wanted).map_err(Failure::invalid)?;
    }
    let request_id = request_id(ctx.args)?;
    if action == Action::Start {
        require_person(ctx)?;
    }
    let list = fetch_list(ctx)?;
    let row = pick(&list.computers, wanted, ctx.instance)?.clone();
    let personal = row
        .get("scope")
        .and_then(|s| s.get("kind"))
        .and_then(Value::as_str)
        != Some("team");
    if action == Action::Start && !ctx.args.yes() {
        return Err(confirmation(
            action,
            wanted,
            start_terms(&row),
            personal,
            Asked::Unconfirmed,
        ));
    }
    let id = row
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let transition = match action {
        Action::Stop => Transition::Stop,
        _ => Transition::Start,
    };
    let preview = json!({
        "action": action.as_str(),
        "id": id,
        "name": row.get("name").cloned().unwrap_or(Value::Null),
        "request_id": request_id,
    });
    let send = ctx.send;
    let wanted = wanted.to_owned();
    Ok(Write {
        preview,
        send: Box::new(move || {
            // Only a confirmed start is ever sent (a stop takes no confirmation).
            match computers::transition(send, &id, transition, &request_id, true)
                .map_err(Failure::from)?
            {
                Answer::Done(answer) => Ok(ack(action, &answer, request_id, Some(&row))),
                Answer::NotOffered => Err(not_offered()),
                Answer::Refused(refusal)
                    if action == Action::Start && asks_confirmation(&refusal) =>
                {
                    Err(confirmation(
                        action,
                        &wanted,
                        computers::terms(refusal.details.as_ref()),
                        personal,
                        Asked::Confirmed,
                    ))
                }
                Answer::Refused(refusal) => Err(refused(&refusal)),
            }
        }),
    })
}

/// Whether `row` belongs to `team`, a team's org id (`None`: the person's
/// own computer). The row's `scope.team` is the team's org id, the same id
/// `--team` takes and a create sends; a display name never matches.
fn in_scope(row: &Map<String, Value>, team: Option<&str>) -> bool {
    let scope = row.get("scope");
    let kind = scope.and_then(|s| s.get("kind")).and_then(Value::as_str);
    let of = scope.and_then(|s| s.get("team")).and_then(Value::as_str);
    match team {
        None => kind == Some("personal"),
        Some(team) => kind == Some("team") && of == Some(team),
    }
}

/// The row `wanted` (an id, or a name) names; a name used in several
/// scopes picks the team this machine is connected to, else it is ambiguous.
pub(crate) fn pick<'a>(
    rows: &'a [Map<String, Value>],
    wanted: &str,
    instance: &InstanceRecord,
) -> Result<&'a Map<String, Value>, Failure> {
    let key = if computers::valid_id(wanted) {
        "id"
    } else {
        "name"
    };
    let matches: Vec<&Map<String, Value>> = rows
        .iter()
        .filter(|row| row.get(key).and_then(Value::as_str) == Some(wanted))
        .collect();
    match matches.as_slice() {
        [] => Err(Failure::new(
            Code::NoComputer,
            format!("no LeanCTX computer '{wanted}' (lean-ctx cloud computer list)"),
        )),
        [one] => Ok(one),
        several => {
            let home = (instance.org_kind.as_deref() != Some("personal"))
                .then_some(instance.org_id.as_str());
            let mine: Vec<_> = several.iter().filter(|row| in_scope(row, home)).collect();
            match mine.as_slice() {
                [one] => Ok(one),
                _ => Err(Failure::invalid(format!(
                    "'{wanted}' names several computers; give its id (lean-ctx cloud computer list)"
                ))),
            }
        }
    }
}

/// Send `write` inside the release point (G08): the request's fields are
/// released first, and only then is anything sent. `--json` prints the
/// released `cloud.computer.ack` (or its error document); the text form is
/// the same answer in words.
fn finish(ctx: &Ctx<'_>, write: Write<'_>) -> Result<(), Failure> {
    let answer = write_answer(ctx, write);
    if ctx.args.json() || answer.body().is_none() {
        local_read::print(&answer);
        return Ok(());
    }
    let doc: Value = serde_json::from_str(&answer.text).unwrap_or(Value::Null);
    if let Some(code) = answer.failed {
        let message = doc["error"]["message"]
            .as_str()
            .unwrap_or("LeanCTX Cloud refused the request.");
        return Err(Failure::new(code, message));
    }
    println!("{}", ack_text(&doc));
    Ok(())
}

/// `write`, released for its channel (G08) and only then sent.
fn write_answer(ctx: &Ctx<'_>, write: Write<'_>) -> local_read::Answer {
    local_read::answer_released_write(TOOL, "", KIND_ACK, ctx.via, &write.preview, write.send)
}

/// The ack in words: what happened, then who pays.
fn ack_text(doc: &Value) -> String {
    let text = |v: &Value| v.as_str().unwrap_or("-").to_owned();
    let mut out = format!(
        "{}: computer '{}' ({}) is {}",
        text(&doc["action"]),
        text(&doc["name"]),
        text(&doc["id"]),
        text(&doc["state"])
    );
    if doc["billing"].is_object() {
        out.push('\n');
        out.push_str(&billing_text(&doc["billing"]));
    }
    out
}

/// Who pays, after the write; an unknown price is said to be unknown.
fn billing_text(bill: &Value) -> String {
    match (bill["basis"].as_str(), bill["usd_per_hour"].as_f64()) {
        (Some("free"), _) => format!(
            "runs on the team's free hours{}",
            bill["free_hours_left"]
                .as_f64()
                .map(|h| format!(" ({h:.1} left this month)"))
                .unwrap_or_default()
        ),
        (Some(_), Some(price)) => format!("billed to the top-up at ${price:.2} per hour"),
        (Some(_), None) => "billed; LeanCTX Cloud did not report the price per hour".to_owned(),
        (None, _) => "LeanCTX Cloud did not say who pays".to_owned(),
    }
}

/// What a person reads when LeanCTX Cloud offers no computers yet.
pub(crate) const NOT_AVAILABLE: &str = "LeanCTX computers are not available yet.";

/// LeanCTX Cloud offers no computers (yet): no provider, an older cloud,
/// or computers switched off. Said plainly, without the cloud's raw text.
pub(crate) fn not_offered() -> Failure {
    Failure::new(Code::ComputerNotAvailable, NOT_AVAILABLE)
}

/// The cloud's refusal as a Local Read error: its code where Local Read
/// has it, its message as it was given, and the HTTP status and cloud code
/// for the record.
pub(crate) fn refused(refusal: &Refusal) -> Failure {
    if refusal.code == "computer_not_available" {
        return not_offered();
    }
    let code = match refusal.code.as_str() {
        "balance_empty" => Code::BalanceEmpty,
        "plan_required" => Code::PlanRequired,
        "forbidden" => Code::Forbidden,
        "sso_required" => Code::SsoRequired,
        "no_computer" => Code::NoComputer,
        "computer_exists" => Code::ComputerExists,
        // The metering routes' shape: `409 invalid_state`, reason `computer_exists`.
        "invalid_state" if refusal.reason.as_deref() == Some("computer_exists") => {
            Code::ComputerExists
        }
        "computer_busy" => Code::ComputerBusy,
        "invalid_state" => Code::InvalidState,
        "confirmation_required" => Code::ConfirmationRequired,
        "invalid_request" | "invalid_field" => Code::InvalidField,
        "rate_limited" | "quota_exceeded" => Code::RateLimited,
        "computer_provider_unavailable" => Code::ComputerProviderUnavailable,
        _ => match refusal.status {
            402 => Code::PlanRequired,
            403 => Code::Forbidden,
            404 => Code::NoComputer,
            409 if refusal.reason.as_deref() == Some("computer_exists") => Code::ComputerExists,
            409 => Code::Conflict,
            400 | 422 => Code::InvalidField,
            428 => Code::ConfirmationRequired,
            429 => Code::RateLimited,
            503 => Code::Unavailable,
            _ => Code::Internal,
        },
    };
    let mut message = refusal
        .message
        .clone()
        .unwrap_or_else(|| default_message(code).to_owned());
    if refusal.next_step.as_deref() == Some("top_up") {
        message.push_str(" Add a top-up to continue.");
    }
    if let Some(seconds) = refusal.retry_after_s {
        message.push_str(&format!(" Try again in {seconds} s."));
    }
    message.push_str(&format!(
        " (LeanCTX Cloud: {} {})",
        refusal.status, refusal.code
    ));
    Failure::new(code, message)
}

fn default_message(code: Code) -> &'static str {
    match code {
        Code::BalanceEmpty => "The top-up is used up and no free hours are left.",
        Code::NoComputer => "That LeanCTX computer is gone.",
        Code::ComputerExists => "This team or account already has a LeanCTX computer.",
        Code::ComputerBusy => "The computer is starting or stopping; try again in a minute.",
        Code::ComputerProviderUnavailable => "LeanCTX computers cannot start right now.",
        Code::Forbidden => "You may not do this for this team.",
        Code::PlanRequired => "The plan does not include LeanCTX computers.",
        Code::RateLimited => "Too many requests just now.",
        _ => "LeanCTX Cloud refused the request.",
    }
}

/// The SSH user LeanCTX Cloud gives the person on computer `name` (one
/// Unix user per person, `lc-…`), for `add` without `--user`.
pub(super) fn ssh_user(name: &str) -> Result<String, String> {
    let hint = "give --user (your user on the computer is ssh.user in `lean-ctx cloud computer list --json`)";
    let (store, config, instance) = connected().map_err(|f| format!("{}; {hint}", f.message))?;
    let send = computers::connected(&store, &config);
    let list = match computers::list(&send).map_err(|e| format!("{e}; {hint}"))? {
        Answer::Done(list) => list,
        Answer::NotOffered | Answer::Refused(_) => {
            return Err(format!("LeanCTX Cloud did not list its computers; {hint}"));
        }
    };
    let row =
        pick(&list.computers, name, &instance).map_err(|f| format!("{}; {hint}", f.message))?;
    let user = row
        .get("ssh")
        .and_then(|s| s.get("user"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("LeanCTX Cloud names no SSH user for '{name}'; {hint}"))?;
    if !cloud_user(user) {
        return Err(format!(
            "LeanCTX Cloud sent an unexpected SSH user for '{name}'; {hint}"
        ));
    }
    Ok(user.to_owned())
}

/// A per-person user as LeanCTX Cloud creates them: `lc-` and 8-16 of
/// `a-z 0-9`.
pub(crate) fn cloud_user(user: &str) -> bool {
    user.strip_prefix("lc-").is_some_and(|rest| {
        (8..=16).contains(&rest.len())
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}

#[cfg(test)]
#[path = "cloud_computer_cloud_tests.rs"]
mod tests;
