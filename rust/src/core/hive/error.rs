// SPDX-License-Identifier: Apache-2.0
//! Errors of the Hive connector. Messages are safe to print: they never
//! carry tokens, keys, device codes or request bodies.

use std::fmt;

use lean_ctx_protocol::hive::lease::HivePlan;
use lean_ctx_protocol::hive::{HiveErrorBody, HiveErrorCode, HiveErrorDetailV1, HiveUploadKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HiveError {
    /// Hive could not be reached (DNS, connect, TLS, timeout, reset).
    Offline(String),
    /// The instance is not enrolled on this machine.
    NotConnected,
    /// The service refused the instance's credentials.
    Unauthorized,
    /// `402 plan_required`: the plan lacks a feature (C0 2.x names it), or,
    /// from a 1.x Hive that names none, Hive itself (an engine seat).
    PlanRequired(Box<PlanNeed>),
    /// Every instance seat of the organization is taken.
    InstanceLimit {
        limit: Option<u64>,
        used: Option<u64>,
    },
    /// A Release B quota is used up (`409`/`429 quota_exceeded`, Hive API
    /// §8.1). Nothing is charged; monthly quotas are waited out (the outbox
    /// keeps local work), counted ones need a person.
    QuotaExceeded(Box<Quota>),
    /// Hive' own enforcement of a policy member refused (`403
    /// policy_denied`, API §7.2). Never retried until the policy changes.
    PolicyDenied(PolicyDenial),
    /// The lease is past `grace_until`; reconnect.
    LeaseLapsed,
    /// A workspace admin revoked this instance (`401 instance_revoked`, C0
    /// 2.0 E12). Recorded locally; no Hive call is made until a new
    /// connect (fail closed, never retried).
    Revoked(Revocation),
    /// The lease lacks the right this request needs.
    Forbidden,
    /// An organization veto bars this lease or handoff (`forbidden_right`,
    /// `reason: vetoed`; C5c). Never retried until the veto is lifted.
    Vetoed,
    /// The organization switched this upload off; stop it and drop its
    /// queue until a lease allows it again (never retried).
    UploadDisabled(HiveUploadKind),
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
    /// Hive refused a task request (C0 1.9 §1); `number` names the task.
    Task {
        refusal: TaskRefusal,
        number: Option<u64>,
        /// The roles a `not_permitted` refusal names (Hive task #17).
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
    /// `registered_engines`, `private_vaults`, or a newer one).
    pub resource: String,
    pub limit: Option<u64>,
    pub used: Option<u64>,
    /// When it frees, second precision `Z` (`None`: nothing frees by itself).
    pub reset_at: Option<String>,
    /// `retry_after_s` (or the `Retry-After` header).
    pub retry_after: Option<u64>,
    /// `registered_engines` in Personal: the slot cool-down.
    pub cooldown_days: Option<u32>,
}

/// What a `402 plan_required` says, as far as it is shown.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlanNeed {
    /// `[a-z_]` feature token (`repos`, `internal_vaults`, …);
    /// `None` from a 1.x Hive: Hive itself (the engine-seat text).
    pub feature: Option<String>,
    /// The lowest plan that includes it (`pro`, `team`, `enterprise`).
    pub plan_needed: Option<String>,
    /// A counted feature's limit and use (`repos`).
    pub limit: Option<u64>,
    pub used: Option<u64>,
    /// The service's own words, terminal-safe.
    pub message: Option<String>,
    /// The workspace's plan as this engine's lease names it (set by the
    /// session that made the call, [`HiveError::with_plan`]).
    pub plan: Option<HivePlan>,
    /// Why the workspace is locked (`trial_ended`, `plan_ended`, `no_plan`;
    /// `[a-z_]`, Hive API §1a).
    pub reason: Option<String>,
}

/// The part every locked-workspace line shares; `hive sync` prints such a
/// refusal once per run.
pub const LOCKED_MARK: &str = "this workspace is locked (read and export only)";

/// The 1.x engine-seat text: the limits worded as everywhere (E10).
pub const PLAN_LIMITS: &str = "Personal 3 engines, 2 connected at once; Team 5 per seat, 3 connected at once per seat; Enterprise on request";

impl PlanNeed {
    /// `trial_ended` or `plan_ended`: the workspace is locked.
    #[must_use]
    pub fn locked(&self) -> Option<&str> {
        self.reason
            .as_deref()
            .filter(|r| matches!(*r, "trial_ended" | "plan_ended"))
    }

    /// Team wording when the refusal or the lease says team.
    fn plan_word(&self) -> &'static str {
        let team = self.plan_needed.as_deref() == Some("team")
            || matches!(self.plan, Some(HivePlan::Team | HivePlan::Enterprise));
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
    format!("{what}: {LOCKED_MARK}. Choose a plan in Hive to sync again.")
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
    // A locked workspace is one line, whatever the feature (E9).
    if let Some(reason) = need.locked() {
        return f.write_str(&locked_line(reason, need.plan_word()));
    }
    let Some(feature) = need.feature.as_deref() else {
        // 1.x: the plan does not include Hive (an engine seat).
        return write!(
            f,
            "this workspace's plan does not include Hive here ({PLAN_LIMITS})"
        );
    };
    let plans = need.plan_needed.as_deref().and_then(plans_from);
    let on = need
        .plan
        .filter(|p| *p != HivePlan::Unknown)
        .map(|p| match p {
            HivePlan::NoPlan => "; this workspace has no plan".to_string(),
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
                "{on}; remove or publish a vault in Hive, or upgrade the plan"
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
pub const MONTHLY_RESOURCES: &[&str] = &["transfer_bytes", "bus_messages"];

impl Quota {
    /// Whether waiting frees it (a monthly quota, or any 429).
    #[must_use]
    pub fn waits(&self) -> bool {
        self.status == 429 || MONTHLY_RESOURCES.contains(&self.resource.as_str())
    }

    /// A Personal engine slot that frees after its cool-down (P7).
    #[must_use]
    pub fn cooling_slot(&self) -> bool {
        self.resource == "registered_engines"
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
    let parsed = chrono::DateTime::parse_from_rfc3339(value?).ok()?;
    Some(
        parsed
            .with_timezone(&chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
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

/// Which check refused a `not_permitted` claim (Hive task #17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermitCheck {
    /// The agent has no live presence on the bus.
    Presence,
    /// The agent is present in another role than the task's.
    Role,
    /// A Hive that does not say.
    Unknown,
}

/// Why Hive refused a task request (the `reason` of C0 1.9 §1).
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
    /// A confirmation that a threshold may not count.
    ReviewerNotIndependent,
    /// An object of another network (org bus vs a swarm).
    CrossNetwork,
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
}

impl TaskRefusal {
    fn from_reason(
        code: HiveErrorCode,
        reason: Option<&str>,
        retry_after: Option<u64>,
        check: Option<&str>,
    ) -> Option<Self> {
        Some(match (code, reason?) {
            (HiveErrorCode::InvalidState, "task_claimed") => Self::Claimed,
            (HiveErrorCode::InvalidState, "task_not_open") => Self::NotOpen,
            (HiveErrorCode::InvalidState, "stale_claim") => Self::StaleClaim,
            (HiveErrorCode::InvalidState, "task_closed") => Self::Closed,
            (HiveErrorCode::InvalidState, "reviewer_not_independent") => {
                Self::ReviewerNotIndependent
            }
            (HiveErrorCode::InvalidState, "cross_network") => Self::CrossNetwork,
            (HiveErrorCode::ForbiddenRight, "not_permitted") => Self::NotPermitted(match check {
                Some("presence") => PermitCheck::Presence,
                Some("role") => PermitCheck::Role,
                _ => PermitCheck::Unknown,
            }),
            (HiveErrorCode::ForbiddenRight, "optin_scope") => Self::OptinScope,
            (HiveErrorCode::InvalidState, "optin_changed") => Self::OptinChanged,
            (HiveErrorCode::InvalidState, "optin_inactive") => Self::OptinInactive,
            (HiveErrorCode::RateLimited, "daily_budget") => Self::DailyBudget { retry_after },
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
            Self::ReviewerNotIndependent => {
                f.write_str("this confirmation is not independent of the result")
            }
            Self::CrossNetwork => f.write_str("the task belongs to another network"),
            Self::NotPermitted(PermitCheck::Presence) => f.write_str(
                "this agent is not on the bus: its session (or the relay) must be running and announce it with the task's role",
            ),
            Self::NotPermitted(PermitCheck::Role) => {
                f.write_str("this agent is on the bus in another role than the task needs")
            }
            Self::NotPermitted(PermitCheck::Unknown) => f.write_str(
                "the task's role or the owner's worker consent does not allow this agent",
            ),
            Self::OptinScope => f.write_str("this engine's opt-in does not cover this vault"),
            Self::OptinChanged => f.write_str("the opt-in changed; it is read again"),
            Self::OptinInactive => f.write_str(
                "this engine is not opted in (or the owner stopped worker mode on the web)",
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
fn safe_message(message: &str) -> Option<String> {
    let text: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(300)
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The whole line of a `not_permitted` refusal: Hive's message (it names
/// the agent and the roles), the check that failed and, when the agent is
/// not on the bus, how to put it there.
fn permit_line(refusal: TaskRefusal, detail: &HiveErrorDetailV1) -> Option<String> {
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
            " [check: presence] Run `lean-ctx hive bus relay --for 30` (it relays, or makes the running relay announce the agent now) and claim again.",
        ),
        PermitCheck::Role => line.push_str(" [check: role]"),
        PermitCheck::Unknown => {}
    }
    Some(line)
}

/// The roles of a `not_permitted` refusal, in words (Hive task #17).
fn permit_roles(refusal: TaskRefusal, detail: &HiveErrorDetailV1) -> Option<String> {
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

/// The longest `retry_after_s` an answer may ask for (32 days); a larger one
/// (up to `u64::MAX`) is shortened to this before any time arithmetic.
pub const RETRY_AFTER_CAP_S: u64 = 32 * 86_400;

impl HiveError {
    /// Map a non-2xx answer to an error.
    pub(crate) fn from_response(status: u16, body: &serde_json::Value) -> Self {
        let value_reason = body["error"]["reason"].clone();
        let revocation = body["error"]["revocation"].clone();
        let Ok(body) = serde_json::from_value::<HiveErrorBody>(body.clone()) else {
            return Self::Server {
                status,
                code: "unknown".into(),
            };
        };
        let mut detail = body.error;
        // A wait beyond the cap is a broken or hostile answer; every later
        // `now + retry_after` stays inside chrono's range (which panics).
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
        match detail.code {
            HiveErrorCode::Unauthorized => Self::Unauthorized,
            HiveErrorCode::LeaseExpired => Self::LeaseLapsed,
            HiveErrorCode::InstanceRevoked => Self::Revoked(Revocation::of_statement(&revocation)),
            HiveErrorCode::PlanRequired => Self::PlanRequired(Box::new(PlanNeed {
                feature: wire_token(detail.feature.as_deref(), 64),
                plan_needed: wire_token(detail.plan_needed.as_deref(), 32),
                limit: detail.limit,
                used: detail.used,
                message: safe_message(&detail.message),
                plan: None,
                reason: wire_token(detail.reason.as_deref(), 32),
            })),
            HiveErrorCode::InstanceLimit => Self::InstanceLimit {
                limit: detail.limit,
                used: detail.used,
            },
            HiveErrorCode::QuotaExceeded => Self::QuotaExceeded(Box::new(Quota {
                status,
                resource: wire_token(detail.resource.as_deref(), 64)
                    .unwrap_or_else(|| "unknown".into()),
                limit: detail.limit,
                used: detail.used,
                reset_at: wire_time(detail.reset_at.as_deref()),
                retry_after: detail.retry_after_s,
                cooldown_days: detail.cooldown_days,
            })),
            // Engine metrics v2 name the member `field` and the level
            // `reason` (`metrics_hours.error.policy_denied.json`).
            HiveErrorCode::PolicyDenied => Self::PolicyDenied(PolicyDenial {
                member: wire_token(detail.member.as_deref().or(detail.field.as_deref()), 80),
                level: wire_token(
                    detail.level.as_deref().or(detail
                        .reason
                        .as_deref()
                        .filter(|r| matches!(*r, "org" | "workspace" | "vault" | "engine"))),
                    16,
                ),
                action: wire_token(detail.action.as_deref(), 64),
            }),
            HiveErrorCode::ForbiddenRight if value_reason.as_str() == Some("vetoed") => {
                Self::Vetoed
            }
            HiveErrorCode::ForbiddenRight => Self::Forbidden,
            HiveErrorCode::UploadDisabled => {
                Self::UploadDisabled(detail.kind.unwrap_or(HiveUploadKind::Unknown))
            }
            HiveErrorCode::NotFound => Self::NotFound,
            HiveErrorCode::RevisionConflict => Self::Conflict {
                expected: detail.expected,
                actual: detail.actual,
            },
            HiveErrorCode::ScreeningRequired => Self::ScreeningBlocked,
            HiveErrorCode::UpgradeRequired => Self::UpgradeRequired {
                min: detail.min_engine,
            },
            HiveErrorCode::RateLimited => Self::RateLimited {
                retry_after: detail.retry_after_s,
            },
            HiveErrorCode::ExpiredToken => Self::DeviceFlow(CODE_EXPIRED.into()),
            HiveErrorCode::AccessDenied => Self::DeviceFlow(
                "you chose \"Not mine\" in the browser; nothing changed on this machine".into(),
            ),
            other => Self::Server {
                status,
                code: serde_json::to_value(other)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| "unknown".into()),
            },
        }
    }

    /// A `plan_required` of a feature names the workspace's plan as the
    /// lease of the calling session says it; every other error is kept.
    #[must_use]
    pub fn with_plan(mut self, plan: HivePlan) -> Self {
        if let Self::PlanRequired(need) = &mut self
            && (need.feature.is_some() || need.locked().is_some())
            && plan != HivePlan::Unknown
        {
            need.plan.get_or_insert(plan);
        }
        self
    }

    /// Whether retrying later can help.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::QuotaExceeded(q) => q.waits(),
            other => matches!(
                other,
                Self::Offline(_) | Self::RateLimited { .. } | Self::Server { status: 500.., .. }
            ),
        }
    }

    /// Seconds to wait before this may be tried again, when Hive said so:
    /// `retry_after_s`/`Retry-After` of a 429, else the time until a
    /// waiting quota's `reset_at` (seen from `now`).
    pub fn retry_after_at(&self, now: chrono::DateTime<chrono::Utc>) -> Option<u64> {
        match self {
            Self::RateLimited { retry_after } => *retry_after,
            Self::QuotaExceeded(q) if q.waits() => q.retry_after.or_else(|| {
                let reset = chrono::DateTime::parse_from_rfc3339(q.reset_at.as_deref()?).ok()?;
                u64::try_from(
                    (reset.with_timezone(&chrono::Utc) - now)
                        .num_seconds()
                        .max(1),
                )
                .ok()
            }),
            _ => None,
        }
    }
}

impl fmt::Display for HiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Offline(why) => write!(f, "Hive is not reachable ({why})"),
            Self::NotConnected => f.write_str("not connected to Hive (run `lean-ctx hive connect`)"),
            Self::Unauthorized => f.write_str(
                "Hive does not accept this instance any more (run `lean-ctx hive connect` again)",
            ),
            Self::PlanRequired(need) => write_plan_need(f, need),
            Self::InstanceLimit { limit, used } => match (limit, used) {
                (Some(limit), Some(used)) => write!(
                    f,
                    "all engine instances of the workspace are in use ({used} of {limit}); remove one in Hive or upgrade the plan"
                ),
                _ => f.write_str(
                    "all engine instances of the workspace are in use; remove one in Hive or upgrade the plan",
                ),
            },
            Self::QuotaExceeded(q) => write_quota(f, q),
            Self::PolicyDenied(d) => {
                f.write_str("Hive policy refuses this")?;
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
                        "; a person does this on the Hive web ({})",
                        action.replace('_', " ")
                    ),
                    None => f.write_str(
                        "; it is not retried until an admin changes the policy in Hive",
                    ),
                }
            }
            Self::LeaseLapsed => {
                f.write_str("the Hive lease has lapsed (run `lean-ctx hive connect` again)")
            }
            Self::Revoked(r) => {
                f.write_str("this engine was revoked by ")?;
                match &r.workspace {
                    Some(workspace) => f.write_str(workspace)?,
                    None => f.write_str("its workspace")?,
                }
                if let Some(reason) = &r.reason {
                    write!(f, " ({})", reason.replace('_', " "))?;
                }
                f.write_str("; reconnect with `lean-ctx hive connect` after an admin allows it")
            }
            Self::Forbidden => f.write_str("this instance's lease does not allow that"),
            Self::Vetoed => f.write_str("a workspace veto bars it until the veto is lifted"),
            Self::UploadDisabled(kind) => match kind {
                HiveUploadKind::Metrics => {
                    f.write_str("the workspace has switched metrics upload off")
                }
                HiveUploadKind::Receipts => {
                    f.write_str("the workspace has switched receipts upload off")
                }
                HiveUploadKind::Unknown => {
                    f.write_str("the workspace has switched this upload off")
                }
            },
            Self::NotFound => f.write_str("Hive does not know that item"),
            Self::Conflict { expected, actual } => match (expected, actual) {
                (Some(e), Some(a)) => write!(f, "knowledge moved on (expected revision {e}, now {a})"),
                _ => f.write_str("knowledge moved on"),
            },
            Self::ScreeningBlocked => f.write_str("Hive screening refused the content"),
            Self::UpgradeRequired { min } => match min {
                Some(min) => write!(f, "Hive needs lean-ctx {min} or newer (run `lean-ctx update`)"),
                None => f.write_str("Hive needs a newer lean-ctx (run `lean-ctx update`)"),
            },
            Self::RateLimited { retry_after } => match retry_after {
                Some(s) => write!(f, "Hive asks to retry in {s} s"),
                None => f.write_str("Hive asks to retry later"),
            },
            Self::DeviceFlow(why) => write!(f, "connection not approved: {why}"),
            Self::Protocol(why) => write!(f, "unexpected answer from Hive: {why}"),
            Self::Config(why) => write!(f, "Hive configuration: {why}"),
            Self::Local(why) => write!(f, "local Hive state: {why}"),
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
            Self::Server { status, code } => write!(f, "Hive answered {status} ({code})"),
        }
    }
}

impl std::error::Error for HiveError {}

/// A device code that ran out before anyone approved it.
pub const CODE_EXPIRED: &str =
    "the code expired before it was approved; run `lean-ctx hive connect` again for a new one";

/// Where a person frees or recovers an engine slot (P7): the recovery
/// route (`POST /api/v1/orgs/{o}/instances/{id}/recover`, owner with a
/// second factor) is on the web only.
pub const RECOVER_HINT: &str = "for a lost or broken device, recover its slot now on the Hive web (Settings › Engines › Recover, with your second factor; once per 30 days)";

fn write_quota(f: &mut fmt::Formatter<'_>, q: &Quota) -> fmt::Result {
    let what = match q.resource.as_str() {
        "transfer_bytes" => "monthly transfer",
        "bus_messages" => "monthly bus messages",
        "registered_engines" => "engine slots",
        "private_vaults" => "private vaults",
        "connected_engines" => "connected engines",
        other => other,
    };
    write!(f, "this workspace has used all its {what}")?;
    if let (Some(used), Some(limit)) = (q.used, q.limit) {
        write!(f, " ({used} of {limit})")?;
    }
    f.write_str("; nothing is charged for more")?;
    match q.resource.as_str() {
        "registered_engines" if q.cooling_slot() => write!(
            f,
            "; a revoked engine's slot frees at {} ({}-day cool-down), or {RECOVER_HINT}",
            q.reset_at.as_deref().unwrap_or("?"),
            q.cooldown_days.unwrap_or(0)
        ),
        "registered_engines" => f.write_str(
            "; revoke an engine you no longer use in Hive (Settings › Engines), or upgrade the plan",
        ),
        "private_vaults" => {
            f.write_str("; remove or publish a vault in Hive, or upgrade the plan")
        }
        _ if q.waits() => match &q.reset_at {
            Some(at) => write!(f, "; uploads wait in the local outbox until {at}"),
            None => f.write_str("; uploads wait in the local outbox until it frees"),
        },
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn instance_limit_carries_the_seat_numbers() {
        let error = HiveError::from_response(
            409,
            &json!({"error": {"code": "instance_limit", "message": "x", "limit": 2, "used": 2}}),
        );
        assert_eq!(
            error,
            HiveError::InstanceLimit {
                limit: Some(2),
                used: Some(2)
            }
        );
        assert!(error.to_string().contains("2 of 2"));
    }

    #[test]
    fn a_huge_retry_after_is_capped_on_the_way_in() {
        for huge in [u64::MAX, i64::MAX.unsigned_abs(), RETRY_AFTER_CAP_S + 1] {
            let error = HiveError::from_response(
                429,
                &json!({"error": {"code": "rate_limited", "message": "x", "retry_after_s": huge}}),
            );
            assert_eq!(
                error,
                HiveError::RateLimited {
                    retry_after: Some(RETRY_AFTER_CAP_S)
                }
            );
        }
        let small = HiveError::from_response(
            429,
            &json!({"error": {"code": "rate_limited", "message": "x", "retry_after_s": 90}}),
        );
        assert_eq!(
            small,
            HiveError::RateLimited {
                retry_after: Some(90)
            }
        );
    }

    #[test]
    fn upload_disabled_names_the_kind_and_is_not_retried() {
        let error = HiveError::from_response(
            403,
            &json!({"error": {"code": "upload_disabled", "message": "x", "kind": "metrics"}}),
        );
        assert_eq!(error, HiveError::UploadDisabled(HiveUploadKind::Metrics));
        assert!(!error.is_transient());
        assert!(error.to_string().contains("metrics upload off"));
    }

    #[test]
    fn task_reasons_become_task_refusals_and_others_stay() {
        let refusal = |status, code: &str, reason: &str| {
            HiveError::from_response(
                status,
                &json!({"error": {"code": code, "message": "x", "reason": reason, "number": 7, "retry_after_s": 60}}),
            )
        };
        assert_eq!(
            refusal(409, "invalid_state", "stale_claim"),
            HiveError::Task {
                refusal: TaskRefusal::StaleClaim,
                number: Some(7),
                detail: None
            }
        );
        assert_eq!(
            refusal(403, "forbidden_right", "optin_scope"),
            HiveError::Task {
                refusal: TaskRefusal::OptinScope,
                number: Some(7),
                detail: None
            }
        );
        // Hive task #17: a not_permitted refusal says which check failed and the roles.
        let permit_said = |check: &str, agent_role: Option<&str>, message: &str| {
            let mut e = json!({"error": {"code": "forbidden_right", "message": message,
                                         "reason": "not_permitted", "number": 7, "check": check,
                                         "role": "backend"}});
            if let Some(r) = agent_role {
                e["error"]["agent_role"] = r.into();
            }
            HiveError::from_response(403, &e).to_string()
        };
        let permit = |check: &str, agent_role: Option<&str>| permit_said(check, agent_role, "");
        assert_eq!(
            permit("presence", None),
            "task #7: this agent is not on the bus: its session (or the relay) must be running and announce it with the task's role (the task needs role backend) [check: presence] Run `lean-ctx hive bus relay --for 30` (it relays, or makes the running relay announce the agent now) and claim again."
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
            "task #7: Agent coder-1 is not on the bus: this task needs it present with role backend. Start its session (or the relay) and claim again. [check: presence] Run `lean-ctx hive bus relay --for 30` (it relays, or makes the running relay announce the agent now) and claim again."
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
        // A Hive that names no check still has its message printed.
        assert_eq!(
            permit_said("", None, "Agent coder-1 may not take this task."),
            "task #7: Agent coder-1 may not take this task."
        );
        let inactive = refusal(409, "invalid_state", "optin_inactive");
        assert!(matches!(inactive, HiveError::Task { refusal, .. } if refusal.ends_claim()));
        assert!(matches!(
            refusal(409, "invalid_state", "optin_changed"),
            HiveError::Task {
                refusal: TaskRefusal::OptinChanged,
                ..
            }
        ));
        let budget = refusal(429, "rate_limited", "daily_budget");
        assert_eq!(
            budget,
            HiveError::Task {
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
                "{}/crates/lean-ctx-protocol/tests/fixtures/hive/{fixture}",
                env!("CARGO_MANIFEST_DIR")
            );
            let body: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            let code: HiveErrorCode =
                serde_json::from_value(body["error"]["code"].clone()).unwrap();
            assert!(
                matches!(
                    HiveError::from_response(code.http_status(), &body),
                    HiveError::Task { refusal, .. } if refusal == expected
                ),
                "{fixture}"
            );
        }
        assert_eq!(refusal(403, "forbidden_right", "vetoed"), HiveError::Vetoed);
        assert_eq!(
            refusal(409, "invalid_state", "sealed"),
            HiveError::Server {
                status: 409,
                code: "invalid_state".into()
            },
            "a reason of another section keeps its old meaning"
        );
    }

    /// The `plan_required` example of a C0 2.x web fixture, by feature.
    fn plan_required_example(file: &str, feature: &str) -> (u16, serde_json::Value) {
        let path = format!(
            "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/web/{file}",
            env!("CARGO_MANIFEST_DIR")
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
        let error = HiveError::from_response(status, &body);
        assert!(
            matches!(&error, HiveError::PlanRequired(need) if need.feature.as_deref() == Some("repos"))
        );
        assert_eq!(
            error.clone().with_plan(HivePlan::Unknown),
            error,
            "an unknown plan is not named"
        );
        assert!(!error.is_transient());
        let repos = error.with_plan(HivePlan::NoPlan);
        assert!(!repos.to_string().contains("engine"));
        assert_eq!(
            repos.to_string(),
            "this workspace has used all its vaults (3 of 3); more are included in Personal, Team and Enterprise; this workspace has no plan; remove or publish a vault in Hive, or upgrade the plan"
        );

        // Any other feature: the service's words (a company comes with Enterprise).
        let (status, body) = plan_required_example("organisation-create.json", "company");
        let org = HiveError::from_response(status, &body).with_plan(HivePlan::Team);
        assert_eq!(
            org.to_string(),
            "A company comes with Enterprise, on request: send a request and we set it up with you; this workspace is on Team"
        );
        let unnamed = HiveError::from_response(
            402,
            &json!({"error": {"code": "plan_required", "message": "", "feature": "sealed_vaults", "plan_needed": "team"}}),
        );
        assert_eq!(
            unnamed.to_string(),
            "sealed vaults is included in Team and Enterprise"
        );
        let hostile = HiveError::from_response(
            402,
            &json!({"error": {"code": "plan_required", "message": "x\u{1b}[2J", "feature": "Comp\u{1b}"}}),
        );
        assert_eq!(
            hostile,
            HiveError::PlanRequired(Box::new(PlanNeed {
                message: Some("x [2J".into()),
                ..PlanNeed::default()
            })),
            "a feature that is not a wire token is dropped, the message is terminal-safe"
        );
    }

    #[test]
    fn plan_required_without_a_feature_keeps_the_1x_seat_text() {
        let path = format!(
            "{}/crates/lean-ctx-protocol/tests/fixtures/hive/error.plan_required.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let body: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect(&path)).expect(&path);
        let error = HiveError::from_response(402, &body).with_plan(HivePlan::NoPlan);
        assert_eq!(
            error.to_string(),
            "this workspace's plan does not include Hive here (Personal 3 engines, 2 connected at once; Team 5 per seat, 3 connected at once per seat; Enterprise on request)"
        );
        assert!(
            matches!(&error, HiveError::PlanRequired(need) if need.plan.is_none()),
            "a 1.x refusal is about Hive itself, not a feature of the plan"
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
            HiveError::from_response(402, &body)
        };
        let trial = "Your Personal trial has ended: this workspace is locked (read and export only). Choose a plan in Hive to sync again.";
        for feature in [None, Some("read_only")] {
            let e = locked("trial_ended", feature, "personal");
            assert_eq!(e.to_string(), trial);
            assert!(matches!(&e, HiveError::PlanRequired(n) if n.locked() == Some("trial_ended")));
            assert_eq!(
                e.with_plan(HivePlan::Personal).to_string(),
                trial,
                "the lease's plan adds nothing to the line"
            );
        }
        assert_eq!(
            locked("plan_ended", Some("read_only"), "team").to_string(),
            "Your plan has ended: this workspace is locked (read and export only). Choose a plan in Hive to sync again."
        );
        assert_eq!(
            locked("trial_ended", None, "team").to_string(),
            "Your Team trial has ended: this workspace is locked (read and export only). Choose a plan in Hive to sync again."
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
            HiveError::from_response(502, &json!("<html>")),
            HiveError::Server {
                status: 502,
                code: "unknown".into()
            }
        );
        assert!(
            HiveError::Server {
                status: 502,
                code: "unknown".into()
            }
            .is_transient()
        );
    }
}
