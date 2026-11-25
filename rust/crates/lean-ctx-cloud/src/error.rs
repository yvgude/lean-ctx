// SPDX-License-Identifier: Apache-2.0
//! Errors of the Cloud connector. Messages are safe to print: they never
//! carry tokens, keys, device codes or request bodies.

use std::fmt;

use lean_ctx_protocol::cloud::lease::CloudPlan;
use lean_ctx_protocol::cloud::{
    CloudErrorBody, CloudErrorCode, CloudErrorDetailV1, CloudUploadKind,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudError {
    /// Cloud could not be reached (DNS, connect, TLS, timeout, reset).
    Offline(String),
    /// The instance is not enrolled on this machine.
    NotConnected,
    /// The service refused the instance's credentials.
    Unauthorized,
    /// `402 plan_required`: the plan lacks a feature (C0 2.x names it), or,
    /// from a 1.x Cloud that names none, Cloud itself (an engine seat).
    PlanRequired(Box<PlanNeed>),
    /// Every instance seat of the organization is taken (`409
    /// instance_limit`): registered instances, or those connected at once.
    InstanceLimit(Box<InstanceCap>),
    /// A Release B quota is used up (`409`/`429 quota_exceeded`, Cloud API
    /// §8.1). Nothing is charged; monthly quotas are waited out (the outbox
    /// keeps local work), counted ones need a person.
    QuotaExceeded(Box<Quota>),
    /// The workspace's LeanCTX Cloud storage is full (`403
    /// storage_quota_exceeded`). Waiting does not free it: the background
    /// sync parks the vault until a person frees storage or syncs by hand.
    StorageFull(Box<StorageFull>),
    /// The sliding-hour upload allowance is used up (`429
    /// upload_rate_limited`). Transient: the rest goes out after
    /// `retry_after` seconds (`retry_after_s` or the `Retry-After` header).
    UploadRateLimited {
        retry_after: Option<u64>,
        /// The allowance per hour in bytes, when the service names it.
        per_hour: Option<u64>,
    },
    /// Cloud' own enforcement of a policy member refused (`403
    /// policy_denied`, API §7.2). Never retried until the policy changes.
    PolicyDenied(PolicyDenial),
    /// The lease is past `grace_until`; reconnect.
    LeaseLapsed,
    /// A workspace admin revoked this instance (`401 instance_revoked`, C0
    /// 2.0 E12). Recorded locally; no Cloud call is made until a new
    /// connect (fail closed, never retried).
    Revoked(Revocation),
    /// The lease lacks the right this request needs.
    Forbidden,
    /// An organization veto bars this lease or handoff (`forbidden_right`,
    /// `reason: vetoed`; C5c). Never retried until the veto is lifted.
    Vetoed,
    /// The organization switched this upload off; stop it and drop its
    /// queue until a lease allows it again (never retried).
    UploadDisabled(CloudUploadKind),
    /// The operator switched a feature off on this Cloud (`404
    /// feature_disabled`, e.g. `instance_metrics`). Not an error of this
    /// instance: what waits stays local, tried again much later.
    FeatureDisabled {
        feature: Option<String>,
    },
    NotFound,
    Conflict {
        expected: Option<u64>,
        actual: Option<u64>,
    },
    ScreeningBlocked,
    UpgradeRequired {
        min: Option<String>,
    },
    RateLimited {
        retry_after: Option<u64>,
    },
    /// The device flow ended without approval.
    DeviceFlow(String),
    /// `cloud connect --trial` against a Cloud that does not take the trial
    /// members of the device code request yet (`400 invalid_request`). Never
    /// retried without them: the person starts the trial on the web.
    TrialUnsupported {
        /// The Cloud web origin to start the trial on.
        origin: String,
    },
    /// A response or stored artifact broke the contract (bad signature,
    /// wrong shape, unknown key).
    Protocol(String),
    /// Configuration that cannot work (URL scheme, root key).
    Config(String),
    /// Local storage failed.
    Local(String),
    /// A local rule refused the request before it left (worker gate,
    /// outgoing screening); the message carries no content.
    Refused(String),
    /// Cloud refused a task request (C0 1.9 §1); `number` names the task.
    Task {
        refusal: TaskRefusal,
        number: Option<u64>,
        /// The roles a `not_permitted` refusal names (Cloud task #17).
        detail: Option<String>,
    },
    /// Any other service answer.
    Server {
        status: u16,
        code: String,
    },
}

/// A used-up quota as far as it is shown and acted on (API §8.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quota {
    /// HTTP status: 409 counted, 429 monthly.
    pub status: u16,
    /// `[a-z_]` token (`transfer_bytes`, `bus_messages`,
    /// `registered_instances`, `private_vaults`, or a newer one).
    pub resource: String,
    pub limit: Option<u64>,
    pub used: Option<u64>,
    /// When it frees, second precision `Z` (`None`: nothing frees by itself).
    pub reset_at: Option<String>,
    /// `retry_after_s` (or the `Retry-After` header).
    pub retry_after: Option<u64>,
    /// `registered_instances` in Personal: the slot cool-down.
    pub cooldown_days: Option<u32>,
    /// What helps, when the service says so.
    pub next_step: Option<NextStep>,
}

/// An `instance_limit` refusal as far as it is shown.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InstanceCap {
    /// `connected_instances` (connected at once) or `registered_instances`
    /// (`None` from an older service: registered).
    pub resource: Option<String>,
    pub limit: Option<u64>,
    pub used: Option<u64>,
    /// Team: the workspace's seats and the allowance of each.
    pub seats: Option<u64>,
    pub per_seat: Option<u64>,
    pub next_step: Option<NextStep>,
}

/// A full LeanCTX Cloud storage (`storage_quota_exceeded`), in bytes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StorageFull {
    pub limit: Option<u64>,
    pub used: Option<u64>,
    pub next_step: Option<NextStep>,
}

/// What helps with a limit refusal (`next_step` of the service).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextStep {
    Subscribe,
    TopUp,
    UpgradeTeam,
    AddSeats,
    Contact,
    Wait,
}

impl NextStep {
    /// The service's `next_step` word; an unknown one is `None`.
    #[must_use]
    pub fn of_wire(value: Option<&str>) -> Option<Self> {
        Some(match value? {
            "subscribe" => Self::Subscribe,
            "top_up" => Self::TopUp,
            "upgrade_team" => Self::UpgradeTeam,
            "add_seats" => Self::AddSeats,
            "contact" => Self::Contact,
            "wait" => Self::Wait,
            _ => return None,
        })
    }

    /// The short hint shown after the refusal (`None`: the line already
    /// says to wait).
    #[must_use]
    pub fn hint(self) -> Option<&'static str> {
        Some(match self {
            Self::Subscribe => "choose a plan in LeanCTX Cloud",
            Self::TopUp => "a top-up in LeanCTX Cloud adds more",
            Self::UpgradeTeam => "Team gives every seat its own allowance",
            Self::AddSeats => "every seat you add in LeanCTX Cloud brings its own allowance",
            Self::Contact => "contact us to extend your Enterprise contract",
            Self::Wait => return None,
        })
    }
}

/// `; <hint>` of a `next_step`, when there is one to show.
fn write_hint(f: &mut fmt::Formatter<'_>, next: Option<NextStep>) -> fmt::Result {
    match next.and_then(NextStep::hint) {
        Some(hint) => write!(f, "; {hint}"),
        None => Ok(()),
    }
}

/// Bytes in words: `1 GiB`, `256 MiB`, `1.5 GiB`.
fn bytes_text(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("TiB", 1 << 40),
        ("GiB", 1 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
    ];
    for (unit, size) in UNITS {
        if bytes >= size {
            let tenths = u128::from(bytes) * 10 / u128::from(size);
            return if tenths % 10 == 0 {
                format!("{} {unit}", tenths / 10)
            } else {
                format!("{}.{} {unit}", tenths / 10, tenths % 10)
            };
        }
    }
    format!("{bytes} B")
}

/// A quota amount in its resource's unit: GiB for bytes, $ for model
/// credit, a count otherwise.
#[must_use]
pub fn amount_text(resource: &str, value: u64) -> String {
    if resource.ends_with("_bytes") {
        bytes_text(value)
    } else if resource.ends_with("_usd_micros") {
        format!("${}.{:02}", value / 1_000_000, value % 1_000_000 / 10_000)
    } else {
        value.to_string()
    }
}

/// What a `402 plan_required` says, as far as it is shown.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlanNeed {
    /// `[a-z_]` feature token (`repos`, `internal_vaults`, …);
    /// `None` from a 1.x Cloud: Cloud itself (the engine-seat text).
    pub feature: Option<String>,
    /// The lowest plan that includes it (`pro`, `team`, `enterprise`).
    pub plan_needed: Option<String>,
    /// A counted feature's limit and use (`repos`).
    pub limit: Option<u64>,
    pub used: Option<u64>,
    /// The service's own words, terminal-safe.
    pub message: Option<String>,
    /// The workspace's plan as this engine's lease names it (set by the
    /// session that made the call, [`CloudError::with_plan`]).
    pub plan: Option<CloudPlan>,
    /// Why the workspace is locked (`trial_ended`, `plan_ended`, `no_plan`;
    /// `[a-z_]`, Cloud API §1a), or `trial_used`: a one-step trial asked
    /// for a trial this address already had.
    pub reason: Option<String>,
    /// What helps, when the service says so.
    pub next_step: Option<NextStep>,
    /// `trial_used`: where to choose a plan (an `http(s)://` URL, or a path
    /// on the Cloud's origin until [`CloudError::with_origin`]; else `None`).
    pub plans_url: Option<String>,
    /// `trial_used`: why no trial starts, as the service names it
    /// (`trial_used`, `trial_ended`, `no_plan`; `[a-z_]`, else `None`).
    pub trial_reason: Option<String>,
}

/// The part every locked-workspace line shares; `cloud sync` prints such a
/// refusal once per run.
pub const LOCKED_MARK: &str = "this workspace is locked (read and export only)";

/// The 1.x engine-seat text: the limits worded as everywhere (E10; plan
/// limits of 17.11.: an Enterprise seat gets what a Team seat gets), in the
/// two host counts of the web (rc4-58): registered and connected at once.
pub const PLAN_LIMITS: &str = "Personal up to 3 hosts registered, up to 2 connected at once; Team and Enterprise up to 3 hosts registered, up to 2 connected at once per seat";

/// The label of `registered_instances`: hosts that connected at least once
/// and are not revoked (the web's "Registered", rc4-58).
pub const HOSTS_REGISTERED: &str = "hosts registered";

/// The label of `connected_instances`: hosts connected now, against the
/// plan's limit of hosts connected at once (rc4-58).
pub const HOSTS_CONNECTED: &str = "hosts connected at once";

/// How a full registered count frees (rc4-62): one revoked host frees its
/// registration (in Personal after the cool-down or a recovery).
pub const FREE_REGISTERED: &str = "to free one, revoke a host you no longer use on the Hosts page";

/// How a full connected-at-once count frees (rc4-58).
pub const FREE_CONNECTED: &str =
    "to free one, disconnect a host (`lean-ctx cloud disconnect` on that machine)";

impl PlanNeed {
    /// `trial_ended` or `plan_ended`: the workspace is locked.
    #[must_use]
    pub fn locked(&self) -> Option<&str> {
        self.reason
            .as_deref()
            .filter(|r| matches!(*r, "trial_ended" | "plan_ended"))
    }

    /// `402 trial_used`: this address already had the trial it asked for.
    #[must_use]
    pub fn trial_used(&self) -> bool {
        self.reason.as_deref() == Some(TRIAL_USED)
    }

    /// Team wording when the refusal or the lease says team.
    fn plan_word(&self) -> &'static str {
        let team = self.plan_needed.as_deref() == Some("team")
            || matches!(self.plan, Some(CloudPlan::Team | CloudPlan::Enterprise));
        if team { "Team" } else { "Personal" }
    }
}

/// The one line of a locked workspace (`reason` `trial_ended`/`plan_ended`).
#[must_use]
pub fn locked_line(reason: &str, plan_word: &str) -> String {
    let what = if reason == "trial_ended" {
        format!("Your {plan_word} trial has ended")
    } else {
        "Your plan has ended".to_string()
    };
    format!("{what}: {LOCKED_MARK}. Choose a plan in LeanCTX Cloud to sync again.")
}

/// The plans that include a feature whose lowest plan is `needed`.
fn plans_from(needed: &str) -> Option<&'static str> {
    Some(match needed {
        "personal" => "Personal, Team and Enterprise",
        "team" => "Team and Enterprise",
        "enterprise" => "Enterprise",
        _ => return None,
    })
}

fn write_plan_need(f: &mut fmt::Formatter<'_>, need: &PlanNeed) -> fmt::Result {
    if need.trial_used() {
        let plan = need.plan_word();
        match need.trial_reason.as_deref() {
            Some("trial_ended") => write!(
                f,
                "the {plan} trial of this address has ended; no second trial starts. Choose a plan"
            )?,
            Some("no_plan") => write!(
                f,
                "this address has no {plan} trial left and no plan; no second trial starts. Choose a plan"
            )?,
            _ => write!(
                f,
                "this address already had its {plan} trial; no second trial starts. Choose a plan"
            )?,
        }
        return match need.plans_url.as_deref().filter(|u| u.starts_with("http")) {
            Some(url) => write!(f, " at {url}, then run `lean-ctx cloud connect`"),
            None => f.write_str(" in LeanCTX Cloud, then run `lean-ctx cloud connect`"),
        };
    }
    // A locked workspace is one line, whatever the feature (E9).
    if let Some(reason) = need.locked() {
        return f.write_str(&locked_line(reason, need.plan_word()));
    }
    let Some(feature) = need.feature.as_deref() else {
        // 1.x: the plan does not include Cloud (an engine seat).
        return write!(
            f,
            "this workspace's plan does not include LeanCTX Cloud here ({PLAN_LIMITS})"
        );
    };
    let plans = need.plan_needed.as_deref().and_then(plans_from);
    let on = need
        .plan
        .filter(|p| *p != CloudPlan::Unknown)
        .map(|p| match p {
            CloudPlan::NoPlan => "; this workspace has no plan".to_string(),
            p => format!("; this workspace is on {}", p.display_name()),
        })
        .unwrap_or_default();
    match feature {
        "repos" | "vaults" | "private_vaults" => {
            f.write_str("this workspace has used all its vaults")?;
            if let (Some(used), Some(limit)) = (need.used, need.limit) {
                write!(f, " ({used} of {limit})")?;
            }
            if let Some(plans) = plans {
                write!(f, "; more are included in {plans}")?;
            }
            write!(
                f,
                "{on}; remove or publish a vault in LeanCTX Cloud, or upgrade the plan"
            )
        }
        other => {
            match (&need.message, plans) {
                (Some(message), _) => f.write_str(message.trim_end_matches('.'))?,
                (None, Some(plans)) => {
                    write!(f, "{} is included in {plans}", other.replace('_', " "))?;
                }
                (None, None) => write!(
                    f,
                    "this workspace's plan does not include {}",
                    other.replace('_', " ")
                )?,
            }
            f.write_str(&on)
        }
    }
}

/// Monthly quotas reset at the start of the next calendar month (UTC).
pub const MONTHLY_RESOURCES: &[&str] =
    &["transfer_bytes", "bus_messages", "model_credit_usd_micros"];

impl Quota {
    /// Whether waiting frees it (a monthly quota, or any 429).
    #[must_use]
    pub fn waits(&self) -> bool {
        self.status == 429 || MONTHLY_RESOURCES.contains(&self.resource.as_str())
    }

    /// A Personal engine slot that frees after its cool-down (P7).
    #[must_use]
    pub fn cooling_slot(&self) -> bool {
        self.resource == "registered_instances"
            && self.cooldown_days.is_some_and(|d| d > 0)
            && self.reset_at.is_some()
    }
}

/// A server-enforced policy refusal (`policy_denied`, API §7.2).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PolicyDenial {
    /// Dotted member path (`sharing.allowed_vaults`).
    pub member: Option<String>,
    /// `org`, `workspace`, `vault` or `engine`.
    pub level: Option<String>,
    /// `consent_required_for`: the action that needs a person.
    pub action: Option<String>,
}

/// A value of the service shown on a terminal: `[a-z0-9_.-]`, 1..=max.
fn wire_token(value: Option<&str>, max: usize) -> Option<String> {
    value
        .filter(|s| {
            (1..=max).contains(&s.len())
                && s.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
                })
        })
        .map(str::to_string)
}

/// An RFC 3339 time of the service in second-precision `Z` form.
pub(crate) fn wire_time(value: Option<&str>) -> Option<String> {
    let parsed = lean_ctx_time::OffsetDateTime::parse_from_rfc3339(value?).ok()?;
    Some(
        parsed
            .to_utc()
            .to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true),
    )
}

/// The code of a one-step trial refusal (`402 trial_used`).
pub const TRIAL_USED: &str = "trial_used";
/// `409 not_ready`: Cloud cannot serve a signed answer yet (plans without
/// a control key, contract §37.2). Kept as the code of a `Server` error.
pub const NOT_READY: &str = "not_ready";
/// `403 vault_create_forbidden`: the caller's role may not add a new vault
/// to the team (rc4-71); read as [`CloudError::Forbidden`].
pub const VAULT_CREATE_FORBIDDEN: &str = "vault_create_forbidden";

/// A web address of the service shown on a terminal: `http(s)://` or a
/// path on the Cloud's own origin (`/o/…`, resolved by
/// [`CloudError::with_origin`]), at most 512 characters, no spaces,
/// control, format (`Cf`, bidi overrides among them) or other invisible
/// characters.
fn web_url(value: Option<&str>) -> Option<String> {
    value
        .filter(|s| {
            (s.starts_with("https://")
                || s.starts_with("http://")
                || (s.starts_with('/') && !s.starts_with("//")))
                && s.len() <= 512
                && !s.chars().any(hidden_in_url)
        })
        .map(str::to_string)
}

/// A character a person cannot see or that reorders what a terminal shows.
fn hidden_in_url(c: char) -> bool {
    use unicode_general_category::{GeneralCategory, get_general_category};
    c.is_control()
        || c.is_whitespace()
        || get_general_category(c) == GeneralCategory::Format
        || lean_ctx_protocol::cloud::invisible(c)
}

/// Whether the absolute `url` has the origin `base` (`scheme://host[:port]`,
/// compared without case): no other host, port or scheme, no userinfo.
fn same_origin(url: &str, base: &str) -> bool {
    let Some(start) = url.find("://").map(|i| i + 3) else {
        return false;
    };
    let end = url[start..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |i| start + i);
    url[..end].eq_ignore_ascii_case(base)
}

/// What a revocation statement says, as far as it is shown (E12).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Revocation {
    /// The workspace that revoked it, when the engine knows its name.
    pub workspace: Option<String>,
    /// `web_revoke`, `admin` or `key_compromise` (`[a-z_]`, ≤ 32).
    pub reason: Option<String>,
    /// `revoked_at` of the statement (ms).
    pub revoked_at: Option<u64>,
    /// `instance_id` of the statement.
    pub instance_id: Option<String>,
}

impl Revocation {
    /// The statement of an `instance_revoked` error body; values that are
    /// not of the contract's shape are left out (they reach a terminal).
    fn of_statement(statement: &serde_json::Value) -> Self {
        let token = |v: &serde_json::Value, max: usize| {
            v.as_str()
                .filter(|s| {
                    (1..=max).contains(&s.len())
                        && s.bytes().all(|b| {
                            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'
                        })
                })
                .map(str::to_string)
        };
        Self {
            workspace: None,
            reason: token(&statement["reason"], 32),
            revoked_at: statement["revoked_at"].as_u64(),
            instance_id: token(&statement["instance_id"], 36),
        }
    }
}

/// Which check refused a `not_permitted` claim (Cloud task #17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermitCheck {
    /// The agent has no live presence on the bus.
    Presence,
    /// The agent is present in another role than the task's.
    Role,
    /// A Cloud that does not say.
    Unknown,
}

/// Why Cloud refused a task request (the `reason` of C0 1.9 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskRefusal {
    /// Another claim holds the task.
    Claimed,
    /// A claim on a task that is not `open`.
    NotOpen,
    /// The lease id or fencing token is not the current claim's.
    StaleClaim,
    /// A write to a `done`, `rejected` or `cancelled` task.
    Closed,
    /// C0 2.2 E48: the agent that did the work checked its own result.
    SelfVerify,
    /// C0 2.2 E48: a verdict on a task that is not `submitted`.
    NotSubmitted,
    /// The task's role or the owner's worker consent does not allow this agent.
    NotPermitted(PermitCheck),
    /// The engine's opt-in does not cover this vault or network (§4,
    /// `optin_scope`).
    OptinScope,
    /// The claim named an older `optin_id`: read the opt-in again (§4,
    /// `optin_changed`).
    OptinChanged,
    /// No opt-in, or the owner stopped it: every claim of the engine ended
    /// (§4, `optin_inactive`).
    OptinInactive,
    /// The opt-in's daily budget is used up until 00:00 UTC (§4,
    /// `daily_budget`); never retried before `retry_after`.
    DailyBudget { retry_after: Option<u64> },
    /// C0 2.2 E47: a priority or labels write named an older
    /// `fields_version` (`stale_base`): read the task again.
    StaleBase,
}

impl TaskRefusal {
    fn from_reason(
        code: CloudErrorCode,
        reason: Option<&str>,
        retry_after: Option<u64>,
        check: Option<&str>,
    ) -> Option<Self> {
        Some(match (code, reason?) {
            (CloudErrorCode::InvalidState, "task_claimed") => Self::Claimed,
            (CloudErrorCode::InvalidState, "task_not_open") => Self::NotOpen,
            (CloudErrorCode::InvalidState, "stale_claim") => Self::StaleClaim,
            (CloudErrorCode::InvalidState, "task_closed") => Self::Closed,
            (CloudErrorCode::ForbiddenRight, "self_verify") => Self::SelfVerify,
            (CloudErrorCode::InvalidState, "not_submitted") => Self::NotSubmitted,
            (CloudErrorCode::ForbiddenRight, "not_permitted") => Self::NotPermitted(match check {
                Some("presence") => PermitCheck::Presence,
                Some("role") => PermitCheck::Role,
                _ => PermitCheck::Unknown,
            }),
            (CloudErrorCode::ForbiddenRight, "optin_scope") => Self::OptinScope,
            (CloudErrorCode::InvalidState, "optin_changed") => Self::OptinChanged,
            (CloudErrorCode::InvalidState, "optin_inactive") => Self::OptinInactive,
            (CloudErrorCode::RateLimited, "daily_budget") => Self::DailyBudget { retry_after },
            (CloudErrorCode::InvalidState | CloudErrorCode::RevisionConflict, "stale_base") => {
                Self::StaleBase
            }
            _ => return None,
        })
    }

    /// Whether the claim this request acted under is gone for good (a stop
    /// of the opt-in ends every claim of the engine).
    #[must_use]
    pub fn ends_claim(self) -> bool {
        matches!(self, Self::StaleClaim | Self::Closed | Self::OptinInactive)
    }
}

impl fmt::Display for TaskRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Claimed => f.write_str("another agent holds the task"),
            Self::NotOpen => f.write_str("the task is not open"),
            Self::StaleClaim => f.write_str("this claim is no longer the task's claim"),
            Self::Closed => f.write_str("the task is closed"),
            Self::SelfVerify => f.write_str(
                "the agent that did the work does not check its own result; another agent verifies it",
            ),
            Self::NotSubmitted => f.write_str("the task has no result waiting for a check"),
            Self::NotPermitted(PermitCheck::Presence) => f.write_str(
                "this agent is not on the bus: its session (or the relay) must be running and announce it with the task's role",
            ),
            Self::NotPermitted(PermitCheck::Role) => {
                f.write_str("this agent is on the bus in another role than the task needs")
            }
            Self::NotPermitted(PermitCheck::Unknown) => f.write_str(
                "the task's role or the owner's worker consent does not allow this agent",
            ),
            Self::OptinScope => f.write_str("this instance's opt-in does not cover this vault"),
            Self::OptinChanged => f.write_str("the opt-in changed; it is read again"),
            Self::OptinInactive => f.write_str(
                "this instance is not opted in (or the owner stopped worker mode on the web)",
            ),
            Self::StaleBase => f.write_str(
                "the task's priority or labels changed meanwhile; read it again and retry",
            ),
            Self::DailyBudget { retry_after } => match retry_after {
                Some(s) => write!(f, "today's task budget is used up (resets in {s} s)"),
                None => f.write_str("today's task budget is used up"),
            },
        }
    }
}

/// A role as the bus names it, or nothing: only `[a-z0-9-]`, 1-64 chars,
/// so a service answer cannot put anything else on the terminal.
fn bus_role(role: Option<&String>) -> Option<&str> {
    role.map(String::as_str).filter(|r| {
        (1..=64).contains(&r.len())
            && r.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

/// The service's own words, safe for a terminal: no control characters
/// (escape sequences), at most 300 characters; `None` when nothing is left.
pub(crate) fn safe_message(message: &str) -> Option<String> {
    let text: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(300)
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The whole line of a `not_permitted` refusal: Cloud's message (it names
/// the agent and the roles), the check that failed and, when the agent is
/// not on the bus, how to put it there.
fn permit_line(refusal: TaskRefusal, detail: &CloudErrorDetailV1) -> Option<String> {
    let TaskRefusal::NotPermitted(check) = refusal else {
        return None;
    };
    let mut line =
        safe_message(&detail.message).unwrap_or_else(|| match permit_roles(refusal, detail) {
            Some(roles) => format!("{refusal} ({roles})"),
            None => refusal.to_string(),
        });
    match check {
        PermitCheck::Presence => line.push_str(
            " [check: presence] Run `lean-ctx cloud bus relay --for 30`: it says who this machine announced, or why an agent is missing. LeanCTX Cloud keeps an agent present only while a relay stays connected (the daemon's), so claim while it runs.",
        ),
        PermitCheck::Role => line.push_str(" [check: role]"),
        PermitCheck::Unknown => {}
    }
    Some(line)
}

/// The roles of a `not_permitted` refusal, in words (Cloud task #17).
fn permit_roles(refusal: TaskRefusal, detail: &CloudErrorDetailV1) -> Option<String> {
    let role = bus_role(detail.role.as_ref())?;
    match refusal {
        TaskRefusal::NotPermitted(PermitCheck::Presence) => {
            Some(format!("the task needs role {role}"))
        }
        TaskRefusal::NotPermitted(PermitCheck::Role) => {
            Some(match bus_role(detail.agent_role.as_ref()) {
                Some(agent_role) => {
                    format!("the agent works as {agent_role}, the task needs {role}")
                }
                None => format!("the task needs role {role}"),
            })
        }
        _ => None,
    }
}

/// A non-negative integer, also when a service sent it as a decimal string.
fn lenient_u64(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
}

/// The `error` object of an answer whose typed decode failed (a member of
/// an unexpected type, e.g. `number` as a string): the code, message and
/// every member that decodes on its own are kept, a numeric string is read
/// as its number, the rest is dropped. `None` without a string `code`.
fn lenient_detail(error: &serde_json::Value) -> Option<CloudErrorDetailV1> {
    let object = error.as_object()?;
    let code = object.get("code").filter(|c| c.is_string())?.clone();
    let message = object
        .get("message")
        .filter(|m| m.is_string())
        .cloned()
        .unwrap_or_else(|| serde_json::Value::String(String::new()));
    let base = || {
        let mut kept = serde_json::Map::new();
        kept.insert("code".into(), code.clone());
        kept.insert("message".into(), message.clone());
        kept
    };
    let decodes = |kept: &serde_json::Map<String, serde_json::Value>| {
        serde_json::from_value::<CloudErrorDetailV1>(serde_json::Value::Object(kept.clone()))
            .is_ok()
    };
    let mut kept = base();
    for (name, value) in object {
        if matches!(name.as_str(), "code" | "message") {
            continue;
        }
        let candidates = [
            Some(value.clone()),
            lenient_u64(value).map(serde_json::Value::from),
        ];
        for candidate in candidates.into_iter().flatten() {
            let mut probe = base();
            probe.insert(name.clone(), candidate.clone());
            if decodes(&probe) {
                kept.insert(name.clone(), candidate);
                break;
            }
        }
    }
    serde_json::from_value(serde_json::Value::Object(kept)).ok()
}

/// The longest `retry_after_s` an answer may ask for (32 days); a larger one
/// (up to `u64::MAX`) is shortened to this before any time arithmetic.
pub const RETRY_AFTER_CAP_S: u64 = 32 * 86_400;

impl CloudError {
    /// Map a non-2xx answer to an error.
    pub(crate) fn from_response(status: u16, body: &serde_json::Value) -> Self {
        let value_reason = body["error"]["reason"].clone();
        let revocation = body["error"]["revocation"].clone();
        let trial_used = body["error"]["code"].as_str() == Some(TRIAL_USED);
        let plans_url = web_url(body["error"]["plans_url"].as_str());
        let decoded = serde_json::from_value::<CloudErrorBody>(body.clone())
            .map(|body| body.error)
            .ok()
            .or_else(|| lenient_detail(&body["error"]));
        // rc4-32 re-pair refusals (p37 §3.18.1), named by `reason` or `code`:
        // kept so the connect says which one, never dropped as `unknown`.
        if matches!(status, 400 | 409)
            && let Some(name) = [&body["error"]["reason"], &body["error"]["code"]]
                .iter()
                .filter_map(|v| v.as_str())
                .find(|name| crate::auth::CHAT_KEY_REPAIR_REASONS.contains(name))
        {
            return Self::Server {
                status,
                code: name.to_owned(),
            };
        }
        // A code this protocol version does not name, kept so a caller can
        // tell "not available yet" from a failure.
        if status == 409 && body["error"]["code"].as_str() == Some(NOT_READY) {
            return Self::Server {
                status,
                code: NOT_READY.into(),
            };
        }
        // rc4-71: a contributor's first share of a vault the team has not
        // added yet; the role refuses it as `forbidden_right` does.
        if status == 403 && body["error"]["code"].as_str() == Some(VAULT_CREATE_FORBIDDEN) {
            return Self::Forbidden;
        }
        let Some(mut detail) = decoded else {
            return Self::of_status(status, lenient_u64(&body["error"]["retry_after_s"]));
        };
        // A wait beyond the cap is a broken or hostile answer; every later
        // `now + retry_after` stays inside the time range (overflow panics).
        detail.retry_after_s = detail.retry_after_s.map(|s| s.min(RETRY_AFTER_CAP_S));
        if let Some(refusal) = TaskRefusal::from_reason(
            detail.code,
            detail.reason.as_deref(),
            detail.retry_after_s,
            detail.check.as_deref(),
        ) {
            return Self::Task {
                refusal,
                number: detail.number,
                detail: permit_line(refusal, &detail),
            };
        }
        let next_step = NextStep::of_wire(detail.next_step.as_deref());
        // One-step trial (`402 trial_used`, a code this protocol version
        // does not name): a plan refusal that says which trial was used.
        if trial_used {
            return Self::PlanRequired(Box::new(PlanNeed {
                plan_needed: wire_token(detail.plan_needed.as_deref(), 32),
                message: safe_message(&detail.message),
                reason: Some(TRIAL_USED.into()),
                next_step,
                plans_url,
                trial_reason: wire_token(detail.reason.as_deref(), 32),
                ..PlanNeed::default()
            }));
        }
        match detail.code {
            CloudErrorCode::Unauthorized => Self::Unauthorized,
            CloudErrorCode::LeaseExpired => Self::LeaseLapsed,
            CloudErrorCode::InstanceRevoked => Self::Revoked(Revocation::of_statement(&revocation)),
            CloudErrorCode::PlanRequired => Self::PlanRequired(Box::new(PlanNeed {
                feature: wire_token(detail.feature.as_deref(), 64),
                plan_needed: wire_token(detail.plan_needed.as_deref(), 32),
                limit: detail.limit,
                used: detail.used,
                message: safe_message(&detail.message),
                plan: None,
                reason: wire_token(detail.reason.as_deref(), 32),
                next_step,
                plans_url: None,
                trial_reason: None,
            })),
            CloudErrorCode::InstanceLimit => Self::InstanceLimit(Box::new(InstanceCap {
                resource: wire_token(detail.resource.as_deref(), 64),
                limit: detail.limit,
                used: detail.used,
                seats: detail.seats,
                per_seat: detail.per_seat,
                next_step,
            })),
            CloudErrorCode::QuotaExceeded => Self::QuotaExceeded(Box::new(Quota {
                status,
                resource: wire_token(detail.resource.as_deref(), 64)
                    .unwrap_or_else(|| "unknown".into()),
                limit: detail.limit,
                used: detail.used,
                reset_at: wire_time(detail.reset_at.as_deref()),
                retry_after: detail.retry_after_s,
                cooldown_days: detail.cooldown_days,
                next_step,
            })),
            CloudErrorCode::StorageQuotaExceeded => Self::StorageFull(Box::new(StorageFull {
                limit: detail.limit_bytes.or(detail.limit),
                used: detail.used_bytes.or(detail.used),
                next_step,
            })),
            CloudErrorCode::UploadRateLimited => Self::UploadRateLimited {
                retry_after: detail.retry_after_s,
                per_hour: detail.limit_bytes_per_hour,
            },
            // Engine metrics v2 name the member `field` and the level
            // `reason` (`metrics_hours.error.policy_denied.json`).
            CloudErrorCode::PolicyDenied => Self::PolicyDenied(PolicyDenial {
                member: wire_token(detail.member.as_deref().or(detail.field.as_deref()), 80),
                level: wire_token(
                    detail.level.as_deref().or(detail
                        .reason
                        .as_deref()
                        .filter(|r| matches!(*r, "org" | "workspace" | "vault" | "instance"))),
                    16,
                ),
                action: wire_token(detail.action.as_deref(), 64),
            }),
            CloudErrorCode::ForbiddenRight if value_reason.as_str() == Some("vetoed") => {
                Self::Vetoed
            }
            CloudErrorCode::ForbiddenRight => Self::Forbidden,
            CloudErrorCode::UploadDisabled => {
                Self::UploadDisabled(detail.kind.unwrap_or(CloudUploadKind::Unknown))
            }
            CloudErrorCode::NotFound => Self::NotFound,
            CloudErrorCode::FeatureDisabled => Self::FeatureDisabled {
                feature: wire_token(detail.feature.as_deref(), 64),
            },
            CloudErrorCode::RevisionConflict => Self::Conflict {
                expected: detail.expected,
                actual: detail.actual,
            },
            CloudErrorCode::ScreeningRequired => Self::ScreeningBlocked,
            CloudErrorCode::UpgradeRequired => Self::UpgradeRequired {
                min: detail.min_instance,
            },
            CloudErrorCode::RateLimited => Self::RateLimited {
                retry_after: detail.retry_after_s,
            },
            CloudErrorCode::ExpiredToken => Self::DeviceFlow(CODE_EXPIRED.into()),
            CloudErrorCode::AccessDenied => Self::DeviceFlow(CODE_DENIED.into()),
            // Any other code on a 429 (`server_busy`, a newer one) is a rate
            // limit: back off, never give up.
            _ if status == 429 => Self::RateLimited {
                retry_after: detail.retry_after_s,
            },
            CloudErrorCode::Unknown => Self::of_status(status, detail.retry_after_s),
            other => Self::Server {
                status,
                code: serde_json::to_value(other)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| "unknown".into()),
            },
        }
    }

    /// A plans page the service named as a path on its own origin
    /// (`402 trial_used`, `"/o/…"`) becomes a full address on `origin`, the
    /// Cloud that answered. A full address is kept only on that same
    /// origin (another host or an https→http downgrade is dropped like an
    /// invalid one); without an `http(s)` origin nothing is shown.
    #[must_use]
    pub fn with_origin(mut self, origin: &str) -> Self {
        if let Self::PlanRequired(need) = &mut self
            && let Some(url) = need.plans_url.take()
        {
            let base = origin.trim_end_matches('/');
            need.plans_url = if !(base.starts_with("https://") || base.starts_with("http://")) {
                None
            } else if url.starts_with('/') {
                Some(format!("{base}{url}"))
            } else if same_origin(&url, base) {
                Some(url)
            } else {
                None
            };
        }
        self
    }

    /// A device code of a `--trial` connect that ran out: with a Cloud
    /// that refuses a used trial only in the browser, the code just
    /// expires, so the hint names the plans page instead of a new code.
    #[must_use]
    pub fn for_trial(self) -> Self {
        match self {
            Self::DeviceFlow(why) if why == CODE_EXPIRED => {
                Self::DeviceFlow(TRIAL_CODE_EXPIRED.into())
            }
            other => other,
        }
    }

    /// An answer that names no code this engine can use (an edge proxy's
    /// HTML page, an unknown code), classified by its HTTP status.
    fn of_status(status: u16, retry_after: Option<u64>) -> Self {
        match status {
            429 => Self::RateLimited {
                retry_after: retry_after.map(|s| s.min(RETRY_AFTER_CAP_S)),
            },
            413 => Self::Server {
                status,
                code: "payload_too_large".into(),
            },
            _ => Self::Server {
                status,
                code: "unknown".into(),
            },
        }
    }

    /// A `plan_required` of a feature names the workspace's plan as the
    /// lease of the calling session says it; every other error is kept.
    #[must_use]
    pub fn with_plan(mut self, plan: CloudPlan) -> Self {
        if let Self::PlanRequired(need) = &mut self
            && (need.feature.is_some() || need.locked().is_some())
            && plan != CloudPlan::Unknown
        {
            need.plan.get_or_insert(plan);
        }
        self
    }

    /// Whether retrying later can help (a 408 request timeout counts: the
    /// request never reached a verdict).
    pub fn is_transient(&self) -> bool {
        match self {
            Self::QuotaExceeded(q) => q.waits(),
            other => matches!(
                other,
                Self::Offline(_)
                    | Self::RateLimited { .. }
                    | Self::UploadRateLimited { .. }
                    | Self::Server {
                        status: 408 | 500..,
                        ..
                    }
            ),
        }
    }

    /// Seconds to wait before this may be tried again, when Cloud said so:
    /// `retry_after_s`/`Retry-After` of a 429, else the time until a
    /// waiting quota's `reset_at` (seen from `now`).
    pub fn retry_after_at(&self, now: lean_ctx_time::UtcDateTime) -> Option<u64> {
        match self {
            Self::RateLimited { retry_after } | Self::UploadRateLimited { retry_after, .. } => {
                *retry_after
            }
            Self::QuotaExceeded(q) if q.waits() => q.retry_after.or_else(|| {
                let reset =
                    lean_ctx_time::OffsetDateTime::parse_from_rfc3339(q.reset_at.as_deref()?)
                        .ok()?;
                u64::try_from((reset.to_utc() - now).num_seconds().max(1)).ok()
            }),
            _ => None,
        }
    }
}

impl fmt::Display for CloudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Offline(why) => write!(f, "LeanCTX Cloud is not reachable ({why})"),
            Self::NotConnected => f.write_str("not connected to LeanCTX Cloud (run `lean-ctx cloud connect`)"),
            Self::Unauthorized => f.write_str(
                "LeanCTX Cloud does not accept this instance any more (run `lean-ctx cloud connect` again)",
            ),
            Self::PlanRequired(need) => {
                write_plan_need(f, need)?;
                // A locked or trial_used line already says to choose a plan.
                if need.locked().is_none() && !need.trial_used() {
                    write_hint(f, need.next_step)?;
                }
                Ok(())
            }
            Self::InstanceLimit(cap) => write_instance_cap(f, cap),
            Self::QuotaExceeded(q) => write_quota(f, q),
            Self::StorageFull(s) => {
                f.write_str("this workspace's LeanCTX Cloud storage is full")?;
                if let (Some(used), Some(limit)) = (s.used, s.limit) {
                    write!(f, " ({} of {})", bytes_text(used), bytes_text(limit))?;
                }
                f.write_str(
                    "; nothing is charged for more; uploads stop until storage frees (remove vaults or files in LeanCTX Cloud)",
                )?;
                write_hint(f, s.next_step)
            }
            Self::UploadRateLimited {
                retry_after,
                per_hour,
            } => {
                f.write_str("LeanCTX Cloud's upload allowance for this hour is used up")?;
                if let Some(per_hour) = per_hour {
                    write!(f, " ({} per hour)", bytes_text(*per_hour))?;
                }
                match retry_after {
                    Some(s) => write!(f, "; the rest goes out in {s} s")?,
                    None => f.write_str("; the rest goes out later")?,
                }
                f.write_str(" (a large first sync can take a few hours)")
            }
            Self::PolicyDenied(d) => {
                f.write_str("LeanCTX Cloud policy refuses this")?;
                if let Some(member) = &d.member {
                    write!(f, " ({member}")?;
                    if let Some(level) = &d.level {
                        write!(f, ", set at the {level} level")?;
                    }
                    f.write_str(")")?;
                }
                match &d.action {
                    Some(action) => write!(
                        f,
                        "; a person does this on the LeanCTX Cloud web ({})",
                        action.replace('_', " ")
                    ),
                    None => f.write_str(
                        "; it is not retried until an admin changes the policy in LeanCTX Cloud",
                    ),
                }
            }
            Self::LeaseLapsed => {
                f.write_str("the LeanCTX Cloud lease has lapsed (run `lean-ctx cloud connect` again)")
            }
            Self::Revoked(r) => {
                f.write_str("this instance was revoked by ")?;
                match &r.workspace {
                    Some(workspace) => f.write_str(workspace)?,
                    None => f.write_str("its workspace")?,
                }
                if let Some(reason) = &r.reason {
                    write!(f, " ({})", reason.replace('_', " "))?;
                }
                f.write_str("; reconnect with `lean-ctx cloud connect` after an admin allows it")
            }
            Self::Forbidden => f.write_str("LeanCTX Cloud does not allow this for your account"),
            Self::Vetoed => f.write_str("a workspace veto bars it until the veto is lifted"),
            Self::UploadDisabled(kind) => match kind {
                CloudUploadKind::Metrics => {
                    f.write_str("the workspace has switched metrics upload off")
                }
                CloudUploadKind::Receipts => {
                    f.write_str("the workspace has switched receipts upload off")
                }
                CloudUploadKind::Unknown => {
                    f.write_str("the workspace has switched this upload off")
                }
            },
            Self::NotFound => f.write_str("LeanCTX Cloud does not know that item"),
            Self::FeatureDisabled { feature } => match feature.as_deref() {
                Some("instance_metrics") | None => f.write_str(
                    "this LeanCTX Cloud server has switched off instance metrics; nothing was sent",
                ),
                Some(other) => write!(
                    f,
                    "this LeanCTX Cloud server has switched off {}; nothing was sent",
                    other.replace('_', " ")
                ),
            },
            Self::Conflict { expected, actual } => match (expected, actual) {
                (Some(e), Some(a)) => {
                    write!(f, "knowledge moved on (expected revision {e}, now {a})")
                }
                _ => f.write_str("knowledge moved on"),
            },
            Self::ScreeningBlocked => f.write_str("LeanCTX Cloud screening refused the content"),
            Self::UpgradeRequired { min } => match min {
                Some(min) => write!(f, "LeanCTX Cloud needs lean-ctx {min} or newer (run `lean-ctx update`)"),
                None => f.write_str("LeanCTX Cloud needs a newer lean-ctx (run `lean-ctx update`)"),
            },
            Self::RateLimited { retry_after } => match retry_after {
                Some(s) => write!(f, "LeanCTX Cloud asks to retry in {s} s"),
                None => f.write_str("LeanCTX Cloud asks to retry later"),
            },
            Self::DeviceFlow(why) => write!(f, "connection not approved: {why}"),
            Self::TrialUnsupported { origin } => write!(
                f,
                "LeanCTX does not offer the one-step trial here yet. Start the trial on {origin}, or sign in with `lean-ctx cloud connect`"
            ),
            Self::Protocol(why) => write!(f, "unexpected answer from Cloud: {why}"),
            Self::Config(why) => write!(f, "Cloud configuration: {why}"),
            Self::Local(why) => write!(f, "local Cloud state: {why}"),
            Self::Refused(why) => f.write_str(why),
            Self::Task {
                refusal,
                number,
                detail,
            } => {
                if let Some(number) = number {
                    write!(f, "task #{number}: ")?;
                }
                match detail {
                    // A not_permitted detail is the whole line (`permit_line`).
                    Some(detail) if matches!(refusal, TaskRefusal::NotPermitted(_)) => {
                        f.write_str(detail)
                    }
                    Some(detail) => write!(f, "{refusal} ({detail})"),
                    None => write!(f, "{refusal}"),
                }
            }
            Self::Server { status, code } => write!(f, "LeanCTX Cloud answered {status} ({code})"),
        }
    }
}

impl std::error::Error for CloudError {}

/// A device code that ran out before anyone approved it.
pub const CODE_EXPIRED: &str =
    "the code expired before it was approved; run `lean-ctx cloud connect` again for a new one";

/// A `--trial` device code that ran out ([`CloudError::for_trial`]).
pub const TRIAL_CODE_EXPIRED: &str = "the code expired before it was approved. If the browser said this address already had its trial, choose a plan in LeanCTX Cloud, then run `lean-ctx cloud connect`; otherwise run `lean-ctx cloud connect --trial` again for a new code";

/// A device code the person refused in the browser.
pub const CODE_DENIED: &str =
    "you chose \"Not mine\" in the browser; nothing changed on this machine";

impl CloudError {
    /// The `code` of a `cloud connect --json` error event: `trial_used`,
    /// `trial_unsupported`, `expired`, `denied`, `plan_required`,
    /// `instance_limit`, `offline`, `unauthorized`, `rate_limited`,
    /// `upgrade_required`, `config`, `protocol`, `local`, the service's own
    /// code for any other answer, else `error`.
    #[must_use]
    pub fn event_code(&self) -> String {
        match self {
            Self::PlanRequired(need) if need.trial_used() => TRIAL_USED.into(),
            Self::PlanRequired(_) => "plan_required".into(),
            Self::TrialUnsupported { .. } => "trial_unsupported".into(),
            Self::DeviceFlow(why) if why == CODE_EXPIRED || why == TRIAL_CODE_EXPIRED => {
                "expired".into()
            }
            Self::DeviceFlow(why) if why == CODE_DENIED => "denied".into(),
            Self::DeviceFlow(_) => "not_approved".into(),
            Self::InstanceLimit(_) => "instance_limit".into(),
            Self::QuotaExceeded(_) => "quota_exceeded".into(),
            Self::Offline(_) => "offline".into(),
            Self::Unauthorized => "unauthorized".into(),
            Self::Revoked(_) => "instance_revoked".into(),
            Self::RateLimited { .. } => "rate_limited".into(),
            Self::UpgradeRequired { .. } => "upgrade_required".into(),
            Self::Config(_) => "config".into(),
            Self::Protocol(_) => "protocol".into(),
            Self::Local(_) => "local".into(),
            Self::Server { code, .. } if wire_token(Some(code), 64).is_some() => code.clone(),
            _ => "error".into(),
        }
    }
}

/// Where a person frees or recovers an engine slot (P7): the recovery
/// route (`POST /api/v1/orgs/{o}/instances/{id}/recover`, owner with a
/// second factor) is on the web only.
pub const RECOVER_HINT: &str = "for a lost or broken device, recover its slot now on the LeanCTX Cloud web (Hosts › Recover the slot now, with your second factor; once per 30 days)";

/// The head of a full host count, in the web's words (rc4-58, rc4-62).
fn host_cap_head(resource: &str) -> Option<&'static str> {
    match resource {
        "registered_instances" => Some("all hosts the plan includes are registered"),
        "connected_instances" => Some("all hosts that may be connected at once are connected"),
        _ => None,
    }
}

/// An `instance_limit` refusal: connected at once is freed by disconnecting
/// one, registered by revoking one.
fn write_instance_cap(f: &mut fmt::Formatter<'_>, cap: &InstanceCap) -> fmt::Result {
    let connected = cap.resource.as_deref() == Some("connected_instances");
    let resource = if connected {
        "connected_instances"
    } else {
        "registered_instances"
    };
    f.write_str(host_cap_head(resource).unwrap_or_default())?;
    if let (Some(used), Some(limit)) = (cap.used, cap.limit) {
        write!(f, " ({used} of {limit}")?;
        if let (Some(seats), Some(per_seat)) = (cap.seats, cap.per_seat) {
            write!(f, ": {seats} seats, {per_seat} per seat")?;
        }
        f.write_str(")")?;
    }
    if connected {
        write!(f, "; {FREE_CONNECTED}, or upgrade the plan")?;
    } else {
        write!(f, "; {FREE_REGISTERED}, or upgrade the plan")?;
    }
    write_hint(f, cap.next_step)
}

/// A quota resource as people read it (`storage_bytes` is "storage").
#[must_use]
pub fn quota_label(resource: &str) -> &str {
    match resource {
        "transfer_bytes" => "monthly transfer",
        "bus_messages" => "monthly bus messages",
        "registered_instances" => HOSTS_REGISTERED,
        "private_vaults" => "private vaults",
        "connected_instances" => HOSTS_CONNECTED,
        "storage_bytes" => "storage",
        "model_credit_usd_micros" => "model credit",
        other => other,
    }
}

fn write_quota(f: &mut fmt::Formatter<'_>, q: &Quota) -> fmt::Result {
    let what = quota_label(&q.resource);
    if let Some(head) = host_cap_head(&q.resource) {
        f.write_str(head)?;
        if let (Some(used), Some(limit)) = (q.used, q.limit) {
            write!(f, " ({used} of {limit})")?;
        }
    } else {
        write!(f, "this workspace has used all its {what}")?;
        if let (Some(used), Some(limit)) = (q.used, q.limit) {
            write!(
                f,
                " ({} of {})",
                amount_text(&q.resource, used),
                amount_text(&q.resource, limit)
            )?;
        }
    }
    f.write_str("; nothing is charged for more")?;
    match q.resource.as_str() {
        "registered_instances" if q.cooling_slot() => write!(
            f,
            "; a revoked host stays registered until {} ({}-day cool-down), or {RECOVER_HINT}",
            q.reset_at.as_deref().unwrap_or("?"),
            q.cooldown_days.unwrap_or(0)
        )?,
        "registered_instances" => write!(f, "; {FREE_REGISTERED}, or upgrade the plan")?,
        "connected_instances" => write!(f, "; {FREE_CONNECTED}")?,
        "private_vaults" => {
            f.write_str("; remove or publish a vault in LeanCTX Cloud, or upgrade the plan")?;
        }
        // Model credit holds like a monthly quota (mz6) but queues no upload.
        "model_credit_usd_micros" => {}
        _ if q.waits() => match &q.reset_at {
            Some(at) => write!(f, "; uploads wait in the local outbox until {at}")?,
            None => f.write_str("; uploads wait in the local outbox until it frees")?,
        },
        _ => {}
    }
    write_hint(f, q.next_step)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// p37 `402 trial_used` (browser approve of a second trial): a plan
    /// refusal that says no second trial starts and where to choose a plan.
    #[test]
    fn trial_used_refusal_names_the_used_trial_and_the_plans_page() {
        let body = json!({"error": {
            "code": "trial_used",
            "message": "This address already had a trial.",
            "plan_needed": "team",
            "next_step": "subscribe",
            "plans_url": "https://cloud.leanctx.com/app/plans"
        }});
        let error = CloudError::from_response(402, &body);
        let CloudError::PlanRequired(need) = &error else {
            panic!("{error:?}");
        };
        assert!(need.trial_used());
        assert_eq!(need.plan_needed.as_deref(), Some("team"));
        assert_eq!(
            need.plans_url.as_deref(),
            Some("https://cloud.leanctx.com/app/plans")
        );
        assert_eq!(error.event_code(), "trial_used");
        assert!(!error.is_transient());
        assert_eq!(
            error.to_string(),
            "this address already had its Team trial; no second trial starts. Choose a plan at https://cloud.leanctx.com/app/plans, then run `lean-ctx cloud connect`"
        );
        // The service's reason words the refusal; the event code stays.
        let ended = CloudError::from_response(
            402,
            &json!({"error": {"code": "trial_used", "reason": "trial_ended", "message": "",
                "plan_needed": "personal", "plans_url": "https://cloud.leanctx.com/app/plans"}}),
        );
        assert_eq!(ended.event_code(), "trial_used");
        assert_eq!(
            ended.to_string(),
            "the Personal trial of this address has ended; no second trial starts. Choose a plan at https://cloud.leanctx.com/app/plans, then run `lean-ctx cloud connect`"
        );
        let no_plan = CloudError::from_response(
            402,
            &json!({"error": {"code": "trial_used", "reason": "no_plan", "message": "",
                "plan_needed": "team"}}),
        );
        assert_eq!(no_plan.event_code(), "trial_used");
        assert!(no_plan.to_string().starts_with(
            "this address has no Team trial left and no plan; no second trial starts."
        ));
        // p37 !134 contract fixture: the plans page is a path on the
        // Cloud's origin, shown once the origin is known.
        let path = format!(
            "{}/crates/lean-ctx-protocol/tests/fixtures/cloud/error.trial_used.json",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../..")
        );
        let fixture: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let refused = CloudError::from_response(402, &fixture);
        assert_eq!(refused.event_code(), "trial_used");
        assert_eq!(
            refused.to_string(),
            "this address already had its Personal trial; no second trial starts. Choose a plan in LeanCTX Cloud, then run `lean-ctx cloud connect`"
        );
        assert_eq!(
            refused
                .with_origin("https://cloud.leanctx.com/")
                .to_string(),
            "this address already had its Personal trial; no second trial starts. Choose a plan at https://cloud.leanctx.com/o/bob/billing/?plan=personal, then run `lean-ctx cloud connect`"
        );
        // A plans URL that is no web address is never shown.
        let hostile = CloudError::from_response(
            402,
            &json!({"error": {"code": "trial_used", "message": "", "plan_needed": "personal",
                "plans_url": "javascript:alert(1)"}}),
        );
        assert_eq!(
            hostile.to_string(),
            "this address already had its Personal trial; no second trial starts. Choose a plan in LeanCTX Cloud, then run `lean-ctx cloud connect`"
        );
        // An ordinary plan refusal keeps its own code.
        let plain = CloudError::from_response(
            402,
            &json!({"error": {"code": "plan_required", "message": "", "feature": "audit_export", "plan_needed": "team"}}),
        );
        assert_eq!(plain.event_code(), "plan_required");
    }

    fn trial_used_with(plans_url: &str) -> CloudError {
        CloudError::from_response(
            402,
            &json!({"error": {"code": "trial_used", "message": "", "plan_needed": "personal",
                "plans_url": plans_url}}),
        )
    }

    const NO_PAGE: &str = "this address already had its Personal trial; no second trial starts. Choose a plan in LeanCTX Cloud, then run `lean-ctx cloud connect`";

    /// An absolute plans page is shown only on the origin of the Cloud
    /// that answered; another host, port or scheme is dropped.
    #[test]
    fn trial_plans_url_on_another_origin_is_not_shown() {
        let origin = "https://cloud.leanctx.com";
        assert_eq!(
            trial_used_with("https://Cloud.LeanCTX.com/app/plans?plan=personal")
                .with_origin(origin)
                .to_string(),
            "this address already had its Personal trial; no second trial starts. Choose a plan at https://Cloud.LeanCTX.com/app/plans?plan=personal, then run `lean-ctx cloud connect`"
        );
        for foreign in [
            "http://cloud.leanctx.com/app/plans",
            "https://evil.example/app/plans",
            "https://cloud.leanctx.com.evil.example/app/plans",
            "https://cloud.leanctx.com@evil.example/app/plans",
            "https://cloud.leanctx.com:8443/app/plans",
            "https://cloud.leanctx.com",
        ] {
            let shown = trial_used_with(foreign).with_origin(origin).to_string();
            let expected = if foreign == "https://cloud.leanctx.com" {
                "this address already had its Personal trial; no second trial starts. Choose a plan at https://cloud.leanctx.com, then run `lean-ctx cloud connect`"
            } else {
                NO_PAGE
            };
            assert_eq!(shown, expected, "{foreign}");
        }
        // Without a web origin nothing is shown, a full address neither.
        assert_eq!(
            trial_used_with("https://cloud.leanctx.com/app/plans")
                .with_origin("file:///tmp")
                .to_string(),
            NO_PAGE
        );
    }

    /// Bidi overrides, isolates and other format or invisible characters
    /// make a plans page unsafe to print; it is dropped like an invalid one.
    #[test]
    fn trial_plans_url_with_format_characters_is_not_shown() {
        for c in [
            '\u{202E}',
            '\u{202A}',
            '\u{2066}',
            '\u{2069}',
            '\u{200B}',
            '\u{200D}',
            '\u{200E}',
            '\u{061C}',
            '\u{00AD}',
            '\u{FEFF}',
            '\u{E0041}',
            '\u{3164}',
        ] {
            let url = format!("/o/bob/billing/{c}?plan=personal");
            assert_eq!(
                trial_used_with(&url)
                    .with_origin("https://cloud.leanctx.com")
                    .to_string(),
                NO_PAGE,
                "U+{:04X}",
                c as u32
            );
        }
        assert_eq!(
            trial_used_with("/o/bob/billing/?plan=personal")
                .with_origin("https://cloud.leanctx.com")
                .to_string(),
            "this address already had its Personal trial; no second trial starts. Choose a plan at https://cloud.leanctx.com/o/bob/billing/?plan=personal, then run `lean-ctx cloud connect`"
        );
    }

    /// A Cloud that refuses a used trial only in the browser lets the
    /// code expire: the trial hint names the plans page, the event code
    /// stays `expired`, other errors are kept.
    #[test]
    fn trial_code_expiry_points_to_the_plans_page() {
        let expired = CloudError::DeviceFlow(CODE_EXPIRED.into()).for_trial();
        assert_eq!(expired, CloudError::DeviceFlow(TRIAL_CODE_EXPIRED.into()));
        assert_eq!(expired.event_code(), "expired");
        assert!(
            expired
                .to_string()
                .contains("choose a plan in LeanCTX Cloud")
        );
        assert!(
            !expired
                .to_string()
                .contains("run `lean-ctx cloud connect` again")
        );
        let denied = CloudError::DeviceFlow(CODE_DENIED.into());
        assert_eq!(denied.clone().for_trial(), denied);
        assert_eq!(
            CloudError::Offline("reset".into()).for_trial(),
            CloudError::Offline("reset".into())
        );
    }

    #[test]
    fn trial_event_codes_name_the_device_flow_outcomes() {
        assert_eq!(
            CloudError::DeviceFlow(CODE_EXPIRED.into()).event_code(),
            "expired"
        );
        assert_eq!(
            CloudError::from_response(
                400,
                &json!({"error": {"code": "access_denied", "message": ""}})
            )
            .event_code(),
            "denied"
        );
        assert_eq!(
            CloudError::Server {
                status: 400,
                code: "invalid_request".into()
            }
            .event_code(),
            "invalid_request"
        );
        assert_eq!(
            CloudError::Server {
                status: 500,
                code: "Bad Code\n".into()
            }
            .event_code(),
            "error"
        );
        assert_eq!(CloudError::Offline("reset".into()).event_code(), "offline");
    }

    /// Review fix: one member of an unexpected type no longer collapses the
    /// answer to `Server { "unknown" }`; code, reason and retry stay.
    #[test]
    fn a_detail_with_a_mistyped_member_keeps_its_code_and_retry() {
        let limited = CloudError::from_response(
            429,
            &json!({"error": {"code": "rate_limited", "message": "slow down",
                "retry_after_s": 12, "number": "seventeen"}}),
        );
        assert_eq!(
            limited,
            CloudError::RateLimited {
                retry_after: Some(12)
            }
        );
        // A numeric string is read as its number; the task refusal survives.
        let claimed = CloudError::from_response(
            409,
            &json!({"error": {"code": "invalid_state", "message": "x",
                "reason": "task_claimed", "number": "17", "limit": {"odd": true}}}),
        );
        assert!(
            matches!(
                claimed,
                CloudError::Task {
                    refusal: TaskRefusal::Claimed,
                    number: Some(17),
                    ..
                }
            ),
            "{claimed:?}"
        );
        let quota = CloudError::from_response(
            429,
            &json!({"error": {"code": "quota_exceeded", "message": "x",
                "resource": "bus_messages", "retry_after_s": "60", "used": -1}}),
        );
        let CloudError::QuotaExceeded(quota) = quota else {
            panic!("{quota:?}");
        };
        assert_eq!(
            (quota.resource.as_str(), quota.retry_after, quota.used),
            ("bus_messages", Some(60), None)
        );
        // No code at all: the status decides.
        assert_eq!(
            CloudError::from_response(400, &json!({"error": {"code": 7}})),
            CloudError::Server {
                status: 400,
                code: "unknown".into()
            }
        );
    }

    /// Review fix: an edge's answer (no JSON, an unknown code) is classified
    /// by status: every 429 backs off, 5xx stays transient, 413 is named.
    #[test]
    fn edge_answers_are_classified_by_status() {
        let busy = CloudError::from_response(
            429,
            &json!({"error": {"code": "server_busy", "message": "x", "retry_after_s": 5}}),
        );
        assert_eq!(
            busy,
            CloudError::RateLimited {
                retry_after: Some(5)
            }
        );
        // The transport turns a bare `Retry-After` into this body.
        let header_only = CloudError::from_response(429, &json!({"error": {"retry_after_s": 9}}));
        assert_eq!(
            header_only,
            CloudError::RateLimited {
                retry_after: Some(9)
            }
        );
        assert_eq!(
            CloudError::from_response(429, &serde_json::Value::Null),
            CloudError::RateLimited { retry_after: None }
        );
        // 408: the server or an edge gave up waiting for the request; a retry can pass.
        for status in [408, 500, 502, 503, 504] {
            let down = CloudError::from_response(status, &serde_json::Value::Null);
            assert!(down.is_transient(), "{status}: {down:?}");
        }
        let too_large = CloudError::from_response(413, &serde_json::Value::Null);
        assert_eq!(
            too_large,
            CloudError::Server {
                status: 413,
                code: "payload_too_large".into()
            }
        );
        assert!(!too_large.is_transient());
    }

    #[test]
    fn instance_limit_carries_the_seat_numbers() {
        let error = CloudError::from_response(
            409,
            &json!({"error": {"code": "instance_limit", "message": "x", "limit": 2, "used": 2}}),
        );
        assert_eq!(
            error,
            CloudError::InstanceLimit(Box::new(InstanceCap {
                limit: Some(2),
                used: Some(2),
                ..InstanceCap::default()
            }))
        );
        assert_eq!(
            error.to_string(),
            "all hosts the plan includes are registered (2 of 2); to free one, revoke a host you no longer use on the Hosts page, or upgrade the plan"
        );
    }

    /// `connected_instances` is freed by disconnecting one, not removing it;
    /// a Team refusal names seats and the next step.
    #[test]
    fn a_connected_limit_says_disconnect_and_names_the_seats() {
        let error = CloudError::from_response(
            409,
            &json!({"error": {"code": "instance_limit", "message": "x", "resource": "connected_instances",
                              "limit": 4, "used": 4, "plan": "team", "next_step": "add_seats",
                              "seats": 2, "per_seat": 2}}),
        );
        assert_eq!(
            error,
            CloudError::InstanceLimit(Box::new(InstanceCap {
                resource: Some("connected_instances".into()),
                limit: Some(4),
                used: Some(4),
                seats: Some(2),
                per_seat: Some(2),
                next_step: Some(NextStep::AddSeats),
            }))
        );
        assert_eq!(
            error.to_string(),
            "all hosts that may be connected at once are connected (4 of 4: 2 seats, 2 per seat); to free one, disconnect a host (`lean-ctx cloud disconnect` on that machine), or upgrade the plan; every seat you add in LeanCTX Cloud brings its own allowance"
        );
        assert!(!error.is_transient());
        let personal = CloudError::from_response(
            409,
            &json!({"error": {"code": "instance_limit", "message": "x", "resource": "connected_instances",
                              "limit": 2, "used": 2, "plan": "personal", "next_step": "upgrade_team"}}),
        );
        assert!(
            personal
                .to_string()
                .ends_with("; Team gives every seat its own allowance"),
            "{personal}"
        );
    }

    #[test]
    fn storage_full_decodes_with_bytes_and_is_not_retried() {
        let error = CloudError::from_response(
            403,
            &json!({"error": {"code": "storage_quota_exceeded", "message": "x", "resource": "storage_bytes",
                              "limit": 1_073_741_824_u64, "used": 1_073_741_824_u64,
                              "limit_bytes": 1_073_741_824_u64, "used_bytes": 1_073_741_824_u64,
                              "plan": "personal", "next_step": "upgrade_team"}}),
        );
        assert_eq!(
            error,
            CloudError::StorageFull(Box::new(StorageFull {
                limit: Some(1 << 30),
                used: Some(1 << 30),
                next_step: Some(NextStep::UpgradeTeam),
            }))
        );
        assert!(!error.is_transient(), "waiting does not free storage");
        assert_eq!(
            error.retry_after_at(lean_ctx_time::UtcDateTime::now()),
            None
        );
        assert_eq!(
            error.to_string(),
            "this workspace's LeanCTX Cloud storage is full (1 GiB of 1 GiB); nothing is charged for more; uploads stop until storage frees (remove vaults or files in LeanCTX Cloud); Team gives every seat its own allowance"
        );
        assert!(!error.to_string().contains("unknown"));
        // Without the byte fields, `limit`/`used` stand in; an unknown next
        // step adds nothing.
        let bare = CloudError::from_response(
            403,
            &json!({"error": {"code": "storage_quota_exceeded", "message": "x", "limit": 1536, "used": 2048,
                              "next_step": "something_new"}}),
        );
        assert_eq!(
            bare,
            CloudError::StorageFull(Box::new(StorageFull {
                limit: Some(1536),
                used: Some(2048),
                next_step: None,
            }))
        );
        assert!(bare.to_string().contains("(2 KiB of 1.5 KiB)"), "{bare}");
    }

    #[test]
    fn upload_rate_limited_is_transient_and_honours_retry_after() {
        let error = CloudError::from_response(
            429,
            &json!({"error": {"code": "upload_rate_limited", "message": "x",
                              "limit_bytes_per_hour": 268_435_456_u64, "plan": "personal",
                              "next_step": "wait", "retry_after_s": 1200}}),
        );
        assert_eq!(
            error,
            CloudError::UploadRateLimited {
                retry_after: Some(1200),
                per_hour: Some(256 << 20),
            }
        );
        assert!(error.is_transient());
        assert_eq!(
            error.retry_after_at(lean_ctx_time::UtcDateTime::now()),
            Some(1200)
        );
        assert_eq!(
            error.to_string(),
            "LeanCTX Cloud's upload allowance for this hour is used up (256 MiB per hour); the rest goes out in 1200 s (a large first sync can take a few hours)"
        );
        // A huge wait is capped on the way in, like every retry_after_s.
        let huge = CloudError::from_response(
            429,
            &json!({"error": {"code": "upload_rate_limited", "message": "x", "retry_after_s": u64::MAX}}),
        );
        assert_eq!(
            huge.retry_after_at(lean_ctx_time::UtcDateTime::now()),
            Some(RETRY_AFTER_CAP_S)
        );
    }

    #[test]
    fn quota_hints_and_new_resource_labels() {
        let quota = |resource: &str, limit: u64, next: &str| {
            CloudError::from_response(
                409,
                &json!({"error": {"code": "quota_exceeded", "message": "x", "resource": resource,
                                  "limit": limit, "used": limit, "plan": "personal", "next_step": next}}),
            )
            .to_string()
        };
        assert_eq!(
            quota("storage_bytes", 1 << 30, "upgrade_team"),
            "this workspace has used all its storage (1 GiB of 1 GiB); nothing is charged for more; Team gives every seat its own allowance"
        );
        assert_eq!(
            quota("model_credit_usd_micros", 8_000_000, "top_up"),
            "this workspace has used all its model credit ($8.00 of $8.00); nothing is charged for more; a top-up in LeanCTX Cloud adds more"
        );
        assert!(
            quota("registered_instances", 3, "contact")
                .ends_with("; contact us to extend your Enterprise contract")
        );
        // rc4-58/rc4-62: the host counts in the web's words, with how to free one.
        assert_eq!(
            quota("registered_instances", 3, "upgrade_team"),
            "all hosts the plan includes are registered (3 of 3); nothing is charged for more; \
             to free one, revoke a host you no longer use on the Hosts page, or upgrade the plan; \
             Team gives every seat its own allowance"
        );
        assert_eq!(
            quota("connected_instances", 2, "wait"),
            "all hosts that may be connected at once are connected (2 of 2); \
             nothing is charged for more; to free one, disconnect a host (`lean-ctx cloud disconnect` on that machine)"
        );
        // The counts never say "slot" (the web dropped "Host slots"); only the
        // recovery names the web's menu item "Recover the slot now" (RECOVER_HINT).
        for resource in ["registered_instances", "connected_instances"] {
            let line = quota(resource, 2, "upgrade_team");
            for word in ["instance", "slot", "workspace"] {
                assert!(!line.contains(word), "{word} in {line}");
            }
        }
        // `wait` adds no hint: the line already says the outbox waits.
        let monthly = CloudError::from_response(
            429,
            &json!({"error": {"code": "quota_exceeded", "message": "x", "resource": "bus_messages",
                              "limit": 200_000, "used": 200_000, "next_step": "wait",
                              "reset_at": "2026-12-01T00:00:00Z", "retry_after_s": 60}}),
        );
        assert_eq!(
            monthly.to_string(),
            "this workspace has used all its monthly bus messages (200000 of 200000); nothing is charged for more; uploads wait in the local outbox until 2026-12-01T00:00:00Z"
        );
        // A feature refusal shows its next step; a locked line does not add one.
        let feature = CloudError::from_response(
            402,
            &json!({"error": {"code": "plan_required", "message": "", "feature": "audit_export",
                              "plan_needed": "team", "next_step": "upgrade_team"}}),
        );
        assert_eq!(
            feature.to_string(),
            "audit export is included in Team and Enterprise; Team gives every seat its own allowance"
        );
        let locked = CloudError::from_response(
            402,
            &json!({"error": {"code": "plan_required", "message": "x", "feature": "read_only",
                              "reason": "trial_ended", "plan": "locked", "next_step": "subscribe",
                              "plan_needed": "personal"}}),
        );
        assert_eq!(
            locked.to_string(),
            "Your Personal trial has ended: this workspace is locked (read and export only). Choose a plan in LeanCTX Cloud to sync again."
        );
    }

    #[test]
    fn bytes_are_worded_in_binary_units() {
        assert_eq!(bytes_text(0), "0 B");
        assert_eq!(bytes_text(1023), "1023 B");
        assert_eq!(bytes_text(256 << 20), "256 MiB");
        assert_eq!(bytes_text(40 << 30), "40 GiB");
        assert_eq!(bytes_text(3 << 29), "1.5 GiB");
        assert_eq!(bytes_text(u64::MAX), "16777215.9 TiB");
    }

    #[test]
    fn a_huge_retry_after_is_capped_on_the_way_in() {
        for huge in [u64::MAX, i64::MAX.unsigned_abs(), RETRY_AFTER_CAP_S + 1] {
            let error = CloudError::from_response(
                429,
                &json!({"error": {"code": "rate_limited", "message": "x", "retry_after_s": huge}}),
            );
            assert_eq!(
                error,
                CloudError::RateLimited {
                    retry_after: Some(RETRY_AFTER_CAP_S)
                }
            );
        }
        let small = CloudError::from_response(
            429,
            &json!({"error": {"code": "rate_limited", "message": "x", "retry_after_s": 90}}),
        );
        assert_eq!(
            small,
            CloudError::RateLimited {
                retry_after: Some(90)
            }
        );
    }

    #[test]
    fn upload_disabled_names_the_kind_and_is_not_retried() {
        let error = CloudError::from_response(
            403,
            &json!({"error": {"code": "upload_disabled", "message": "x", "kind": "metrics"}}),
        );
        assert_eq!(error, CloudError::UploadDisabled(CloudUploadKind::Metrics));
        assert!(!error.is_transient());
        assert!(error.to_string().contains("metrics upload off"));
    }

    #[test]
    fn task_reasons_become_task_refusals_and_others_stay() {
        let refusal = |status, code: &str, reason: &str| {
            CloudError::from_response(
                status,
                &json!({"error": {"code": code, "message": "x", "reason": reason, "number": 7, "retry_after_s": 60}}),
            )
        };
        assert_eq!(
            refusal(409, "invalid_state", "stale_claim"),
            CloudError::Task {
                refusal: TaskRefusal::StaleClaim,
                number: Some(7),
                detail: None
            }
        );
        assert_eq!(
            refusal(403, "forbidden_right", "optin_scope"),
            CloudError::Task {
                refusal: TaskRefusal::OptinScope,
                number: Some(7),
                detail: None
            }
        );
        // Cloud task #17: a not_permitted refusal says which check failed and the roles.
        let permit_said = |check: &str, agent_role: Option<&str>, message: &str| {
            let mut e = json!({"error": {"code": "forbidden_right", "message": message,
                                         "reason": "not_permitted", "number": 7, "check": check,
                                         "role": "backend"}});
            if let Some(r) = agent_role {
                e["error"]["agent_role"] = r.into();
            }
            CloudError::from_response(403, &e).to_string()
        };
        let permit = |check: &str, agent_role: Option<&str>| permit_said(check, agent_role, "");
        assert_eq!(
            permit("presence", None),
            "task #7: this agent is not on the bus: its session (or the relay) must be running and announce it with the task's role (the task needs role backend) [check: presence] Run `lean-ctx cloud bus relay --for 30`: it says who this machine announced, or why an agent is missing. LeanCTX Cloud keeps an agent present only while a relay stays connected (the daemon's), so claim while it runs."
        );
        assert_eq!(
            permit("role", Some("web")),
            "task #7: this agent is on the bus in another role than the task needs (the agent works as web, the task needs backend) [check: role]"
        );
        assert!(
            permit("role", Some("web\u{1b}[31m"))
                .ends_with("(the task needs role backend) [check: role]"),
            "a role that is not a bus identifier is not printed"
        );
        // The service's own message is printed, with the check and the hint.
        assert_eq!(
            permit_said(
                "presence",
                None,
                "Agent coder-1 is not on the bus: this task needs it present with role backend. Start its session (or the relay) and claim again."
            ),
            "task #7: Agent coder-1 is not on the bus: this task needs it present with role backend. Start its session (or the relay) and claim again. [check: presence] Run `lean-ctx cloud bus relay --for 30`: it says who this machine announced, or why an agent is missing. LeanCTX Cloud keeps an agent present only while a relay stays connected (the daemon's), so claim while it runs."
        );
        assert_eq!(
            permit_said(
                "role",
                Some("web"),
                "Agent coder-1 works as role web;\u{1b}[31m this task needs role backend."
            ),
            "task #7: Agent coder-1 works as role web; [31m this task needs role backend. [check: role]",
            "control characters of the service's message never reach the terminal"
        );
        // A Cloud that names no check still has its message printed.
        assert_eq!(
            permit_said("", None, "Agent coder-1 may not take this task."),
            "task #7: Agent coder-1 may not take this task."
        );
        let inactive = refusal(409, "invalid_state", "optin_inactive");
        assert!(matches!(inactive, CloudError::Task { refusal, .. } if refusal.ends_claim()));
        assert!(matches!(
            refusal(409, "invalid_state", "optin_changed"),
            CloudError::Task {
                refusal: TaskRefusal::OptinChanged,
                ..
            }
        ));
        let budget = refusal(429, "rate_limited", "daily_budget");
        assert_eq!(
            budget,
            CloudError::Task {
                refusal: TaskRefusal::DailyBudget {
                    retry_after: Some(60)
                },
                number: Some(7),
                detail: None
            }
        );
        assert!(!budget.is_transient(), "a used-up budget is not retried");
        assert!(
            budget
                .to_string()
                .starts_with("task #7: today's task budget")
        );
        // The contract's own error fixtures decode to the same refusals.
        for (fixture, expected) in [
            ("error.optin_scope.json", TaskRefusal::OptinScope),
            ("error.optin_changed.json", TaskRefusal::OptinChanged),
            ("error.optin_inactive.json", TaskRefusal::OptinInactive),
            (
                "error.daily_budget.json",
                TaskRefusal::DailyBudget {
                    retry_after: Some(43200),
                },
            ),
            ("error.task_claimed.json", TaskRefusal::Claimed),
            (
                "error.task_not_permitted.json",
                TaskRefusal::NotPermitted(PermitCheck::Unknown),
            ),
            (
                "error.task_not_permitted_role.json",
                TaskRefusal::NotPermitted(PermitCheck::Role),
            ),
        ] {
            let path = format!(
                "{}/crates/lean-ctx-protocol/tests/fixtures/cloud/{fixture}",
                concat!(env!("CARGO_MANIFEST_DIR"), "/../..")
            );
            let body: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            let code: CloudErrorCode =
                serde_json::from_value(body["error"]["code"].clone()).unwrap();
            assert!(
                matches!(
                    CloudError::from_response(code.http_status(), &body),
                    CloudError::Task { refusal, .. } if refusal == expected
                ),
                "{fixture}"
            );
        }
        // C0 2.2 E48: the verify refusals of the cloud-v2 contract.
        for (fixture, expected) in [
            (
                "task_verify.error.self_verify.json",
                TaskRefusal::SelfVerify,
            ),
            (
                "task_verify.error.not_submitted.json",
                TaskRefusal::NotSubmitted,
            ),
        ] {
            let path = format!(
                "{}/crates/lean-ctx-protocol/tests/fixtures/cloud-v2/routes/{fixture}",
                concat!(env!("CARGO_MANIFEST_DIR"), "/../..")
            );
            let body: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            let code: CloudErrorCode =
                serde_json::from_value(body["error"]["code"].clone()).unwrap();
            assert!(
                matches!(
                    CloudError::from_response(code.http_status(), &body),
                    CloudError::Task { refusal, .. } if refusal == expected
                ),
                "{fixture}"
            );
        }
        assert_eq!(
            refusal(403, "forbidden_right", "vetoed"),
            CloudError::Vetoed
        );
        assert_eq!(
            refusal(409, "invalid_state", "code_graph_disabled"),
            CloudError::Server {
                status: 409,
                code: "invalid_state".into()
            },
            "a reason of another section keeps its old meaning"
        );
    }

    /// The `plan_required` example of a C0 2.x web fixture, by feature.
    fn plan_required_example(file: &str, feature: &str) -> (u16, serde_json::Value) {
        let path = format!(
            "{}/crates/lean-ctx-protocol/tests/fixtures/cloud-v2/web/{file}",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../..")
        );
        let fixture: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect(&path)).expect(&path);
        let example = fixture["errors"]
            .as_array()
            .expect("errors")
            .iter()
            .find(|e| {
                e["body"]["error"]["code"] == "plan_required"
                    && e["body"]["error"]["feature"] == feature
            })
            .unwrap_or_else(|| panic!("{file}: no plan_required {feature}"));
        (
            u16::try_from(example["status"].as_u64().unwrap()).unwrap(),
            example["body"].clone(),
        )
    }

    #[test]
    fn plan_required_keeps_the_feature_and_words_it() {
        // A vault limit (W11 transfer into a no-plan workspace).
        let (status, body) = plan_required_example("vault-transfer.json", "repos");
        assert_eq!(status, 402);
        let error = CloudError::from_response(status, &body);
        assert!(
            matches!(&error, CloudError::PlanRequired(need) if need.feature.as_deref() == Some("repos"))
        );
        assert_eq!(
            error.clone().with_plan(CloudPlan::Unknown),
            error,
            "an unknown plan is not named"
        );
        assert!(!error.is_transient());
        let repos = error.with_plan(CloudPlan::NoPlan);
        assert!(!repos.to_string().contains("engine"));
        assert_eq!(
            repos.to_string(),
            "this workspace has used all its vaults (3 of 3); more are included in Personal, Team and Enterprise; this workspace has no plan; remove or publish a vault in LeanCTX Cloud, or upgrade the plan"
        );

        // Any other feature: the service's words (a company comes with Enterprise).
        let (status, body) = plan_required_example("organisation-create.json", "company");
        let org = CloudError::from_response(status, &body).with_plan(CloudPlan::Team);
        assert_eq!(
            org.to_string(),
            "A company comes with Enterprise, on request: send a request and we set it up with you; this workspace is on Team"
        );
        let unnamed = CloudError::from_response(
            402,
            &json!({"error": {"code": "plan_required", "message": "", "feature": "audit_export", "plan_needed": "team"}}),
        );
        assert_eq!(
            unnamed.to_string(),
            "audit export is included in Team and Enterprise"
        );
        let hostile = CloudError::from_response(
            402,
            &json!({"error": {"code": "plan_required", "message": "x\u{1b}[2J", "feature": "Comp\u{1b}"}}),
        );
        assert_eq!(
            hostile,
            CloudError::PlanRequired(Box::new(PlanNeed {
                message: Some("x [2J".into()),
                ..PlanNeed::default()
            })),
            "a feature that is not a wire token is dropped, the message is terminal-safe"
        );
    }

    #[test]
    fn plan_required_without_a_feature_keeps_the_1x_seat_text() {
        let path = format!(
            "{}/crates/lean-ctx-protocol/tests/fixtures/cloud/error.plan_required.json",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../..")
        );
        let body: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect(&path)).expect(&path);
        let error = CloudError::from_response(402, &body).with_plan(CloudPlan::NoPlan);
        assert_eq!(
            error.to_string(),
            "this workspace's plan does not include LeanCTX Cloud here (Personal up to 3 hosts registered, up to 2 connected at once; Team and Enterprise up to 3 hosts registered, up to 2 connected at once per seat)"
        );
        assert!(
            matches!(&error, CloudError::PlanRequired(need) if need.plan.is_none()),
            "a 1.x refusal is about LeanCTX Cloud itself, not a feature of the plan"
        );
    }

    #[test]
    fn a_locked_workspace_is_one_line_whatever_the_feature() {
        let locked = |reason: &str, feature: Option<&str>, needed: &str| {
            let mut body = json!({"error": {"code": "plan_required", "message": "Your trial has ended: this workspace is locked.",
                                            "plan_needed": needed, "reason": reason}});
            if let Some(f) = feature {
                body["error"]["feature"] = f.into();
            }
            CloudError::from_response(402, &body)
        };
        let trial = "Your Personal trial has ended: this workspace is locked (read and export only). Choose a plan in LeanCTX Cloud to sync again.";
        for feature in [None, Some("read_only")] {
            let e = locked("trial_ended", feature, "personal");
            assert_eq!(e.to_string(), trial);
            assert!(matches!(&e, CloudError::PlanRequired(n) if n.locked() == Some("trial_ended")));
            assert_eq!(
                e.with_plan(CloudPlan::Personal).to_string(),
                trial,
                "the lease's plan adds nothing to the line"
            );
        }
        assert_eq!(
            locked("plan_ended", Some("read_only"), "team").to_string(),
            "Your plan has ended: this workspace is locked (read and export only). Choose a plan in LeanCTX Cloud to sync again."
        );
        assert_eq!(
            locked("trial_ended", None, "team").to_string(),
            "Your Team trial has ended: this workspace is locked (read and export only). Choose a plan in LeanCTX Cloud to sync again."
        );
        // no_plan is not a lock of an ended plan: the feature's words stay.
        let no_plan = locked("no_plan", None, "personal");
        assert!(
            no_plan.to_string().contains("2 connected at once"),
            "{no_plan}"
        );
        assert!(!no_plan.to_string().contains(LOCKED_MARK));
    }

    #[test]
    fn non_contract_bodies_become_server_errors() {
        assert_eq!(
            CloudError::from_response(502, &json!("<html>")),
            CloudError::Server {
                status: 502,
                code: "unknown".into()
            }
        );
        assert!(
            CloudError::Server {
                status: 502,
                code: "unknown".into()
            }
            .is_transient()
        );
    }

    #[test]
    fn a_role_that_may_not_add_a_vault_is_forbidden() {
        let body = json!({"error": {
            "code": "vault_create_forbidden",
            "message": "Only an owner, admin or maintainer of this team can add a new vault.",
            "role_needed": "maintainer",
        }});
        assert_eq!(CloudError::from_response(403, &body), CloudError::Forbidden);
        // Another status with that code is no role refusal.
        assert_ne!(CloudError::from_response(500, &body), CloudError::Forbidden);
    }
}
