// SPDX-License-Identifier: Apache-2.0
//! Context controls, session context and machine status (3.12.3, rel12
//! WP3), as Local Read kinds. Each one replaces text the app used to parse
//! (`ctx_control list`, `ctx_checkpoint log`, `update --status`) or a
//! command it could only run.
//!
//! - `overlays.list`: `lean-ctx overlays list --json --project-root P`, each
//!   controlled file of the project with its effective state, full paths.
//! - `overlays.removed`: `lean-ctx overlays remove --json --path ABS
//!   --project-root P`, removes the file's pin / keep-out overlays (write).
//! - `checkpoints.list`: `lean-ctx checkpoints list --json --project-root P`,
//!   the shadow history, newest first, with the files each one changed.
//! - `session.context`: `lean-ctx session context --json --session ID`.
//! - `session.handoff`: `lean-ctx session handoff --json --session ID`.
//! - `session.value`: `lean-ctx session value --json --session ID`, the
//!   receipt document of `lean-ctx value --session ID --json`.
//! - `update.status`, `security.status`, `telemetry.status`: `lean-ctx
//!   update|security|telemetry status --json`. Versions, switches and
//!   counts only: no paths, digests, secrets or installation id.
//!
//! Every document is a function of the stored data: nothing generates a
//! timestamp or counter. Wired as one `local_read::Part`.

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use lean_ctx::core::context_overlay::{OverlayAuthor, OverlayOp, OverlayScope, OverlayStore};
use lean_ctx::core::session::SessionState;

use super::routes::{Method, Param, Project, Route, Shape};
use super::{Answer, Code, Failure, Via};

const PROJECT_ROOT: Param = Param {
    name: "project_root",
    flag: "--project-root",
    shape: Shape::Value,
};
const SESSION: Param = Param {
    name: "session",
    flag: "--session",
    shape: Shape::Value,
};
const LIMIT: Param = Param {
    name: "limit",
    flag: "--limit",
    shape: Shape::Value,
};
const PATH: Param = Param {
    name: "path",
    flag: "--path",
    shape: Shape::Value,
};

/// This part's routes (Route as in `routes.rs`).
pub(crate) const ROUTES: &[Route] = &[
    Route {
        kind: "overlays.list",
        method: Method::Get,
        path: "/v1/local/overlays",
        cli: &["overlays", "list", "--json"],
        action: &["list"],
        params: &[PROJECT_ROOT],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "overlays.removed",
        method: Method::Post,
        path: "/v1/local/overlays/remove",
        cli: &["overlays", "remove", "--json"],
        action: &["remove"],
        params: &[PATH, PROJECT_ROOT],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "checkpoints.list",
        method: Method::Get,
        path: "/v1/local/checkpoints",
        cli: &["checkpoints", "list", "--json"],
        action: &["list"],
        params: &[PROJECT_ROOT, LIMIT],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "session.context",
        method: Method::Get,
        path: "/v1/local/session/context",
        cli: &["session", "context", "--json"],
        action: &["context"],
        params: &[SESSION],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "session.handoff",
        method: Method::Get,
        path: "/v1/local/session/handoff",
        cli: &["session", "handoff", "--json"],
        action: &["handoff"],
        params: &[SESSION],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "session.value",
        method: Method::Get,
        path: "/v1/local/session/value",
        cli: &["session", "value", "--json"],
        action: &["value"],
        params: &[SESSION],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "update.status",
        method: Method::Get,
        path: "/v1/local/update/status",
        cli: &["update", "status", "--json"],
        action: &["status"],
        params: &[],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "security.status",
        method: Method::Get,
        path: "/v1/local/security/status",
        cli: &["security", "status", "--json"],
        action: &["status"],
        params: &[],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "telemetry.status",
        method: Method::Get,
        path: "/v1/local/telemetry/status",
        cli: &["telemetry", "status", "--json"],
        action: &["status"],
        params: &[],
        project: Project::None,
        names: None,
    },
];

/// This part's document kinds (sorted).
pub(crate) const KINDS: &[&str] = &[
    "checkpoints.list",
    "overlays.list",
    "overlays.removed",
    "security.status",
    "session.context",
    "session.handoff",
    "session.value",
    "telemetry.status",
    "update.status",
];

/// Command path → CLI verbs, as `capabilities::COMMANDS`.
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[
    ("checkpoints", &["list"]),
    ("overlays", &["list", "remove"]),
    ("security", &["status"]),
    ("session", &["context", "handoff", "value"]),
    ("telemetry", &["status"]),
    ("update", &["status"]),
];

/// The release root of the machine-wide and session documents.
const NO_PROJECT: &str = "";

// ------------------------------------------------------------------ overlays

/// One controlled file of the project: its effective control from its
/// overlays in the store's order (later wins).
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct OverlayRow {
    /// The file's absolute path (inside the project).
    pub path: String,
    /// The key LeanCTX holds the newest overlay under (`ctx_control`'s target).
    pub target: String,
    /// `pinned`, `excluded` or `none`.
    pub state: &'static str,
    pub view: Option<&'static str>,
    pub priority: Option<f64>,
    pub outdated: bool,
    /// The newest overlay's scope, as `ctx_control` names it.
    pub scope: String,
    pub stale: bool,
    /// `user`, `policy:<name>` or `agent:<id>`, of the newest overlay.
    pub set_by: Option<String>,
    /// When the newest overlay was set (from the store).
    pub set_at: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct OverlaysDoc {
    pub overlays: Vec<OverlayRow>,
    pub full_paths: bool,
}

fn scope_label(scope: &OverlayScope) -> String {
    match scope {
        OverlayScope::Call => "this call".into(),
        OverlayScope::Session => "this session".into(),
        OverlayScope::Project => "persistent".into(),
        OverlayScope::Global => "global".into(),
        OverlayScope::Agent(_) => "agent".into(),
    }
}

fn author_label(author: &OverlayAuthor) -> String {
    match author {
        OverlayAuthor::User => "user".into(),
        OverlayAuthor::Policy(name) => format!("policy:{name}"),
        OverlayAuthor::Agent(id) => format!("agent:{id}"),
    }
}

/// The path inside the project a `file:` target names: relative to the
/// root, or absolute under it (as given or canonical). `None` for anything
/// that leaves it (`..`, another root) or names no file.
fn rel_of(root: &Path, canonical_root: &Path, raw: &str) -> Option<PathBuf> {
    let raw = Path::new(raw);
    let rel = if raw.is_absolute() {
        raw.strip_prefix(root)
            .or_else(|_| raw.strip_prefix(canonical_root))
            .ok()?
            .to_path_buf()
    } else {
        raw.to_path_buf()
    };
    let mut clean = PathBuf::new();
    for part in rel.components() {
        match part {
            Component::Normal(p) => clean.push(p),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!clean.as_os_str().is_empty()).then_some(clean)
}

fn canonical(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

/// The `overlays.list` body of `store` for the project `root`.
pub(crate) fn overlays_doc(root: &Path, store: &OverlayStore) -> OverlaysDoc {
    let canonical_root = canonical(root);
    let mut rows: Vec<OverlayRow> = Vec::new();
    for overlay in store.all() {
        let Some(raw) = overlay.target.as_str().strip_prefix("file:") else {
            continue;
        };
        let Some(rel) = rel_of(root, &canonical_root, raw) else {
            continue;
        };
        let path = root.join(&rel).to_string_lossy().into_owned();
        let at = if let Some(i) = rows.iter().position(|r| r.path == path) {
            i
        } else {
            rows.push(OverlayRow {
                path,
                target: String::new(),
                state: "none",
                view: None,
                priority: None,
                outdated: false,
                scope: String::new(),
                stale: false,
                set_by: None,
                set_at: None,
            });
            rows.len() - 1
        };
        let row = &mut rows[at];
        match &overlay.operation {
            OverlayOp::Pin { .. } => row.state = "pinned",
            OverlayOp::Exclude { .. } => row.state = "excluded",
            OverlayOp::Include | OverlayOp::Unpin => row.state = "none",
            OverlayOp::SetView(view) => row.view = Some(view.as_str()),
            OverlayOp::SetPriority { set_priority } => row.priority = Some(*set_priority),
            OverlayOp::MarkOutdated => row.outdated = true,
            OverlayOp::Rewrite { .. } | OverlayOp::Expire { .. } => {}
        }
        raw.clone_into(&mut row.target);
        row.scope = scope_label(&overlay.scope);
        row.stale |= overlay.stale;
        row.set_by = Some(author_label(&overlay.author));
        row.set_at = Some(
            overlay
                .created_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
    }
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    OverlaysDoc {
        overlays: rows,
        full_paths: true,
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct RemovedDoc {
    pub path: String,
    /// Whether a pin or keep-out overlay was removed.
    pub removed: bool,
    pub state_before: &'static str,
    pub state: &'static str,
}

/// The project file `path` names: absolute, inside `root`, no `..`.
fn file_of(root: &Path, path: &str) -> Result<PathBuf, Failure> {
    if !Path::new(path).is_absolute() {
        return Err(Failure::invalid("--path must be the file's absolute path"));
    }
    rel_of(root, &canonical(root), path)
        .ok_or_else(|| Failure::new(Code::Forbidden, format!("{path} is not inside the project")))
}

/// Removes the pin / keep-out / let-in / unpin overlays of `rel` from
/// `store` (views, priorities and outdated marks stay). Returns the body
/// and whether the store changed.
pub(crate) fn remove_state(
    root: &Path,
    rel: &Path,
    store: &mut OverlayStore,
) -> (RemovedDoc, bool) {
    let before = overlays_doc(root, store);
    let path = root.join(rel).to_string_lossy().into_owned();
    let state_before = before
        .overlays
        .iter()
        .find(|r| r.path == path)
        .map_or("none", |r| r.state);
    let canonical_root = canonical(root);
    let dropped = store.retain(|o| {
        let state_op = matches!(
            o.operation,
            OverlayOp::Pin { .. }
                | OverlayOp::Exclude { .. }
                | OverlayOp::Include
                | OverlayOp::Unpin
        );
        let this_file = o
            .target
            .as_str()
            .strip_prefix("file:")
            .and_then(|raw| rel_of(root, &canonical_root, raw))
            .is_some_and(|r| r == rel);
        !(state_op && this_file)
    });
    let removed = dropped > 0;
    (
        RemovedDoc {
            path,
            removed,
            state_before,
            state: "none",
        },
        removed,
    )
}

fn load_overlays(root: &Path) -> OverlayStore {
    let mut store = OverlayStore::load_project(root);
    store.prune_expired();
    store
}

// ------------------------------------------------------------------ checkpoints

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct CheckpointRow {
    pub id: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub created: String,
    pub message: String,
    pub files_changed: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct CheckpointsDoc {
    pub checkpoints: Vec<CheckpointRow>,
    pub files_known: bool,
    pub source: &'static str,
}

/// The `checkpoints.list` body of `log` (newest first); `files` gives the
/// files a checkpoint changed.
pub(crate) fn checkpoints_doc(
    log: Vec<lean_ctx::core::git::shadow::Checkpoint>,
    files: &dyn Fn(&str) -> Vec<String>,
) -> CheckpointsDoc {
    CheckpointsDoc {
        checkpoints: log
            .into_iter()
            .map(|c| CheckpointRow {
                id: c.sha.get(..12).unwrap_or(&c.sha).to_owned(),
                files_changed: files(&c.sha),
                git_ref: c.sha,
                created: c.time,
                message: c.message,
            })
            .collect(),
        files_known: true,
        source: "shadow",
    }
}

// ------------------------------------------------------------------ session

/// `--session ID`, validated like the app's id.
fn session_arg(args: &[String]) -> Result<String, Failure> {
    super::flag(args, "--session")?
        .and_then(lean_ctx::core::activity::session_id)
        .ok_or_else(|| Failure::invalid("--session <id> is required (1-128 of [A-Za-z0-9._:-])"))
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct ContextDoc {
    pub session_id: String,
    /// The model with the most proxy turns in the session.
    pub model: Option<String>,
    pub window_tokens: Option<u64>,
    /// Tokens the session's tools put into the context (input − saved).
    pub used_tokens: u64,
    /// `used_tokens / window_tokens`, 0..1, three decimals.
    pub pressure: Option<f64>,
    /// `session+proxy` with a model from the proxy, else `session`.
    pub source: &'static str,
}

/// The `session.context` body: `sessions` attributed to `id`, `spend` its
/// proxy turns per model as (model, requests); `window` the model's window.
pub(crate) fn context_doc(
    id: &str,
    sessions: &[SessionState],
    spend: &[(String, u64)],
    window: &dyn Fn(&str) -> u64,
) -> Result<ContextDoc, Failure> {
    if sessions.is_empty() && spend.is_empty() {
        return Err(Failure::new(
            Code::NotFound,
            format!("no LeanCTX session for {id}"),
        ));
    }
    let model = spend
        .iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map(|(m, _)| m.clone());
    let window_tokens = model.as_deref().map(window).filter(|w| *w > 0);
    let used_tokens = sessions
        .iter()
        .map(|s| {
            s.stats
                .total_tokens_input
                .saturating_sub(s.stats.total_tokens_saved)
        })
        .fold(0u64, u64::saturating_add);
    #[allow(clippy::cast_precision_loss)]
    let pressure = window_tokens.map(|w| {
        let p = (used_tokens as f64 / w as f64).clamp(0.0, 1.0);
        (p * 1000.0).round() / 1000.0
    });
    Ok(ContextDoc {
        session_id: id.to_owned(),
        source: if model.is_some() {
            "session+proxy"
        } else {
            "session"
        },
        model,
        window_tokens,
        used_tokens,
        pressure,
    })
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct HandoffDoc {
    pub session_id: String,
    /// The LeanCTX session the handoff comes from (newest attributed one).
    pub lean_session: String,
    pub task: Option<String>,
    pub findings: Vec<String>,
    pub decisions: Vec<String>,
    /// Files the session changed (else every file it touched), sorted.
    pub files: Vec<String>,
    pub next_steps: Vec<String>,
    /// The cross-session decisions handed over, as `ctx_session handoff`
    /// prints them; `null` when there are none.
    pub text: Option<String>,
}

/// The `session.handoff` body of the (admitted) session `s`.
pub(crate) fn handoff_doc(id: &str, s: &SessionState) -> HandoffDoc {
    let modified: Vec<String> = s
        .files_touched
        .iter()
        .filter(|f| f.modified)
        .map(|f| f.path.clone())
        .collect();
    let mut files = if modified.is_empty() {
        s.files_touched.iter().map(|f| f.path.clone()).collect()
    } else {
        modified
    };
    files.sort();
    files.dedup();
    let text = (!s.handoff_context.is_empty()).then(|| {
        let mut out = format!(
            "Handoff context ({} recent cross-session decisions):",
            s.handoff_context.len()
        );
        for fact in &s.handoff_context {
            out.push_str(&format!(
                "\n  [{}] {} (confidence: {:.0}%)",
                fact.category,
                fact.value,
                f64::from(fact.confidence) * 100.0
            ));
        }
        out
    });
    HandoffDoc {
        session_id: id.to_owned(),
        lean_session: s.id.clone(),
        task: s
            .task
            .as_ref()
            .map(|t| t.description.trim().to_owned())
            .filter(|t| !t.is_empty()),
        findings: s.findings.iter().map(|f| f.summary.clone()).collect(),
        decisions: s.decisions.iter().map(|d| d.summary.clone()).collect(),
        files,
        next_steps: s.next_steps.clone(),
        text,
    }
}

fn build_handoff(args: &[String]) -> Result<HandoffDoc, Failure> {
    let id = session_arg(args)?;
    let newest = SessionState::load_attributed(&id)
        .into_iter()
        .max_by_key(|s| s.updated_at)
        .ok_or_else(|| Failure::new(Code::NotFound, format!("no LeanCTX session for {id}")))?;
    let admitted = lean_ctx::core::session::admission::for_delivery(&newest)
        .map_err(|reason| Failure::new(Code::Withheld, reason))?;
    Ok(handoff_doc(&id, &admitted))
}

fn build_context(args: &[String]) -> Result<ContextDoc, Failure> {
    let id = session_arg(args)?;
    let sessions = SessionState::load_attributed(&id);
    let spend: Vec<(String, u64)> = lean_ctx_proxy::usage_meter::persisted_session(&id)
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.model, r.requests))
        .collect();
    context_doc(&id, &sessions, &spend, &|model| {
        lean_ctx::core::model_registry::context_window_for_model(model) as u64
    })
}

// ------------------------------------------------------------------ status

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct UpdateDoc {
    /// The version of this binary.
    pub current: String,
    /// The receipt's active version.
    pub active: Option<String>,
    /// The version kept for a roll back (the receipt's previous one).
    pub kept: Option<String>,
    /// A valid update receipt is retained.
    pub receipt: bool,
    /// A receipt exists but did not validate.
    pub receipt_error: bool,
    /// The release pin (`updates.pinned_version`); `null` follows the latest.
    pub pinned: Option<String>,
    pub auto_update: bool,
    pub notify_only: bool,
    pub check_interval_hours: u64,
}

pub(crate) fn update_doc(
    current: &str,
    receipt: lean_ctx::core::updater::ReceiptStatus,
    updates: &lean_ctx::core::config::UpdatesConfig,
) -> UpdateDoc {
    use lean_ctx::core::updater::ReceiptStatus;
    let (active, kept, valid, invalid) = match receipt {
        ReceiptStatus::Missing => (None, None, false, false),
        ReceiptStatus::Valid { active, previous } => (Some(active), Some(previous), true, false),
        ReceiptStatus::Invalid => (None, None, false, true),
    };
    UpdateDoc {
        current: current.to_owned(),
        active,
        kept,
        receipt: valid,
        receipt_error: invalid,
        pinned: updates.pinned_version.clone(),
        auto_update: updates.auto_update,
        notify_only: updates.notify_only,
        check_interval_hours: updates.check_interval_hours,
    }
}

fn build_update() -> UpdateDoc {
    update_doc(
        lean_ctx::core::updater::current_version(),
        lean_ctx::core::updater::receipt_status(),
        &lean_ctx::core::config::Config::load().updates,
    )
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct SecurityDoc {
    /// `strict`, `relaxed` or `open`.
    pub level: &'static str,
    /// `enforced`, `relaxed`, `disabled` or `workspace`.
    pub path_jail: &'static str,
    /// What disabled the jail, or the Workspace binding.
    pub jail_detail: Option<String>,
    /// The knobs that widen the jail (labels, never paths).
    pub relaxed_by: Vec<String>,
    /// Shell gating: `enforce`, `warn` or `off`.
    pub shell: &'static str,
    /// How many `allow_paths` are configured (the paths are not shown).
    pub allow_paths_count: usize,
    pub secrets_detection: bool,
    pub secrets_redact: bool,
    pub secrets_protected: bool,
}

pub(crate) fn security_doc(
    posture: &lean_ctx::core::security_posture::SecurityPosture,
    allow_paths_count: usize,
) -> SecurityDoc {
    use lean_ctx::core::security_posture::JailState;
    let (path_jail, jail_detail, relaxed_by) = match &posture.jail {
        JailState::Enforced => ("enforced", None, Vec::new()),
        JailState::Relaxed(by) => ("relaxed", None, by.clone()),
        JailState::Disabled(why) => ("disabled", Some(why.clone()), Vec::new()),
        JailState::Workspace(binding) => ("workspace", Some(binding.clone()), Vec::new()),
    };
    SecurityDoc {
        level: posture.level().as_str(),
        path_jail,
        jail_detail,
        relaxed_by,
        shell: posture.shell.as_str(),
        allow_paths_count,
        secrets_detection: posture.secrets_enabled,
        secrets_redact: posture.secrets_redact,
        secrets_protected: posture.secrets_protected(),
    }
}

fn build_security() -> SecurityDoc {
    security_doc(
        &lean_ctx::core::security_posture::SecurityPosture::detect(),
        lean_ctx::core::config::Config::load().allow_paths.len(),
    )
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct TelemetryDoc {
    /// The saved preference: `enabled` or `disabled`.
    pub preference: &'static str,
    /// `default_on`, `explicit_enabled` or `explicit_disabled`.
    pub preference_source: &'static str,
    /// Whether this installation sends now.
    pub sending: bool,
    /// Why it does not: `preference`, `environment`, `ci` or `policy`.
    pub blocked_by: Option<&'static str>,
    /// The last acknowledged send (day bucket), `null` for never.
    pub last_sent: Option<String>,
    /// What is collected and what never is.
    pub disclosure: Vec<String>,
}

/// The `telemetry.status` body, with the same verdict and reason order as
/// `lean-ctx telemetry status` (the config's own eligibility authority).
pub(crate) fn telemetry_doc(
    telemetry: &lean_ctx::core::config::TelemetryConfig,
    do_not_track: Option<&str>,
    env_override: Option<&str>,
    in_ci: bool,
    last_sent: Option<String>,
) -> TelemetryDoc {
    use lean_ctx::core::config::{TelemetryConfig, TelemetryPreference};
    let blocked_by = if telemetry.send_eligible(do_not_track, env_override) {
        in_ci.then_some("ci")
    } else if telemetry.explicitly_disabled() {
        Some("preference")
    } else if TelemetryConfig::environment_disables(do_not_track, env_override) {
        Some("environment")
    } else {
        Some("policy")
    };
    TelemetryDoc {
        preference: if telemetry.explicitly_disabled() {
            "disabled"
        } else {
            "enabled"
        },
        preference_source: match telemetry.preference {
            TelemetryPreference::DefaultOn => "default_on",
            TelemetryPreference::ExplicitlyEnabled => "explicit_enabled",
            TelemetryPreference::ExplicitlyDisabled => "explicit_disabled",
        },
        sending: blocked_by.is_none(),
        blocked_by,
        last_sent,
        disclosure: lean_ctx::core::telemetry_consent::disclosure_lines(),
    }
}

fn build_telemetry() -> Result<TelemetryDoc, Failure> {
    // Global-only and fail-closed, like every path that sends.
    let cfg = lean_ctx::core::config::Config::try_load_global()
        .map_err(|error| Failure::new(Code::Unavailable, format!("config unreadable: {error}")))?;
    let last_sent = lean_ctx::core::telemetry_aggregate::last_sent_bucket()
        .or_else(|| cfg.telemetry.last_heartbeat.clone());
    Ok(telemetry_doc(
        &cfg.telemetry,
        lean_ctx::core::host_env::var("DO_NOT_TRACK").as_deref(),
        lean_ctx::core::host_env::var("LEAN_CTX_TELEMETRY").as_deref(),
        lean_ctx::core::telemetry_consent::running_in_ci(),
        last_sent,
    ))
}

// ------------------------------------------------------------------ dispatch

/// The document of `kind` for `args` (same bytes on the CLI and the
/// socket), released through the tool run of its tool.
pub(crate) fn answer(kind: &str, args: &[String], via: Via) -> Answer {
    match kind {
        "overlays.list" => with_root(args, |root| {
            super::answer_released("ctx_control", root, kind, via, || {
                Ok(overlays_doc(
                    Path::new(root),
                    &load_overlays(Path::new(root)),
                ))
            })
        }),
        "overlays.removed" => with_root(args, |root| {
            let preview = super::request_preview(args, &["--path", "--project-root"]);
            super::answer_released_write("ctx_control", root, kind, via, &preview, || {
                let path = super::flag(args, "--path")?
                    .ok_or_else(|| Failure::invalid("--path <absolute file path> is required"))?;
                let root = Path::new(root);
                let rel = file_of(root, path)?;
                let mut store = load_overlays(root);
                let (doc, changed) = remove_state(root, &rel, &mut store);
                if changed {
                    store
                        .save_project(root)
                        .map_err(|e| Failure::new(Code::Internal, e))?;
                }
                Ok(doc)
            })
        }),
        "checkpoints.list" => with_root(args, |root| {
            super::answer_released("ctx_checkpoint", root, kind, via, || {
                let limit = super::limit(args, 20, 200)?;
                let root = Path::new(root);
                let log = lean_ctx::core::git::shadow::log(root, limit)
                    .map_err(|e| Failure::new(Code::Unavailable, e))?;
                Ok(checkpoints_doc(log, &|sha| {
                    lean_ctx::core::git::shadow::files_of(root, sha).unwrap_or_default()
                }))
            })
        }),
        "session.context" => {
            super::answer_released("ctx_session", NO_PROJECT, kind, via, || build_context(args))
        }
        "session.handoff" => {
            super::answer_released("ctx_session", NO_PROJECT, kind, via, || build_handoff(args))
        }
        "session.value" => super::answer_released("ctx_proof", NO_PROJECT, kind, via, || {
            let id = session_arg(args)?;
            Ok(lean_ctx::core::value::proof::build(Some(&id), false))
        }),
        "update.status" => {
            super::answer_released("ctx_agent", NO_PROJECT, kind, via, || Ok(build_update()))
        }
        "security.status" => {
            super::answer_released("ctx_agent", NO_PROJECT, kind, via, || Ok(build_security()))
        }
        "telemetry.status" => {
            super::answer_released("ctx_agent", NO_PROJECT, kind, via, build_telemetry)
        }
        other => Answer::failure(&Failure::invalid(format!("unknown kind {other}"))),
    }
}

/// `f` with the project of `args`, or the `no_project` error document.
fn with_root(args: &[String], f: impl FnOnce(&str) -> Answer) -> Answer {
    match super::project_root(args) {
        Ok(root) => f(&root),
        Err(failure) => Answer::failure(&failure),
    }
}

/// The kind `lean-ctx <command> <verb> … --json` prints, when it is one of
/// this part's.
fn kind_of(command: &str, rest: &[String]) -> Option<&'static str> {
    let verb = super::positionals(rest).first().copied()?;
    Some(match (command, verb) {
        ("overlays", "list") => "overlays.list",
        ("overlays", "remove") => "overlays.removed",
        ("checkpoints", "list") => "checkpoints.list",
        ("session", "context") => "session.context",
        ("session", "handoff") => "session.handoff",
        ("session", "value") => "session.value",
        ("update", "status") => "update.status",
        ("security", "status") => "security.status",
        ("telemetry", "status") => "telemetry.status",
        _ => return None,
    })
}

/// `lean-ctx <command> <verb> … --json` for this part's kinds: prints the
/// document and returns `true`; anything else is left to the command.
pub(crate) fn try_run(command: &str, rest: &[String]) -> bool {
    if !super::wants_json(rest) {
        return false;
    }
    let Some(kind) = kind_of(command, rest) else {
        return false;
    };
    super::print(&answer(kind, rest, Via::Cli));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx::core::context_field::{ContextItemId, ViewKind};
    use lean_ctx::core::context_overlay::ContextOverlay;
    use serde_json::{Value, json};

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| (*s).to_owned()).collect()
    }

    fn overlay(target: &str, op: OverlayOp, author: OverlayAuthor) -> ContextOverlay {
        ContextOverlay::new(
            ContextItemId::from_file(target),
            op,
            OverlayScope::Project,
            "h".into(),
            author,
        )
    }

    /// Every kind is routed, listed with its command verb on both
    /// transports, in the capabilities kinds, and reachable by the socket.
    #[test]
    fn the_context_and_status_kinds_are_served_and_listed() {
        use super::super::{capabilities, routes};
        let table = routes::table_json();
        let caps = capabilities::build_with(table.clone(), routes::events_json());
        let commands = capabilities::commands_of(&table);
        for (kind, method, path, command, verb, project) in [
            (
                "overlays.list",
                "GET",
                "/v1/local/overlays",
                "overlays",
                "list",
                true,
            ),
            (
                "overlays.removed",
                "POST",
                "/v1/local/overlays/remove",
                "overlays",
                "remove",
                true,
            ),
            (
                "checkpoints.list",
                "GET",
                "/v1/local/checkpoints",
                "checkpoints",
                "list",
                true,
            ),
            (
                "session.context",
                "GET",
                "/v1/local/session/context",
                "session",
                "context",
                false,
            ),
            (
                "session.handoff",
                "GET",
                "/v1/local/session/handoff",
                "session",
                "handoff",
                false,
            ),
            (
                "session.value",
                "GET",
                "/v1/local/session/value",
                "session",
                "value",
                false,
            ),
            (
                "update.status",
                "GET",
                "/v1/local/update/status",
                "update",
                "status",
                false,
            ),
            (
                "security.status",
                "GET",
                "/v1/local/security/status",
                "security",
                "status",
                false,
            ),
            (
                "telemetry.status",
                "GET",
                "/v1/local/telemetry/status",
                "telemetry",
                "status",
                false,
            ),
        ] {
            let route = routes::all()
                .find(|r| r.path == path)
                .unwrap_or_else(|| panic!("{path} is routed"));
            assert_eq!(route.kind, kind);
            let row = table
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["path"] == path)
                .unwrap_or_else(|| panic!("{path} is in capabilities.routes"));
            assert_eq!(row["kind"], kind);
            assert_eq!(row["method"], method);
            assert_eq!(row["project"], json!(project), "{row}");
            assert_eq!(row["command"], command, "{row}");
            assert_eq!(row["verb"], verb, "{row}");
            assert!(
                commands[command]
                    .as_array()
                    .is_some_and(|v| v.contains(&json!({"verb": verb, "via": ["cli", "socket"]}))),
                "{command} {verb}: {commands:?}"
            );
            assert!(caps.kinds.contains(&kind), "{kind} in capabilities.kinds");
            assert!(super::super::part_of(kind).is_some(), "{kind} dispatched");
        }
        // The session stays a stats/cost verb too.
        assert!(
            commands["session"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["verb"] == "stats")
        );
    }

    #[test]
    fn the_part_lists_exactly_its_routes() {
        let mut routed: Vec<&str> = ROUTES.iter().map(|r| r.kind).collect();
        routed.sort_unstable();
        assert_eq!(routed, KINDS);
        let mut sorted = KINDS.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, KINDS);
    }

    #[test]
    fn requests_become_the_cli_arguments() {
        use super::super::routes;
        let route = |kind: &str| ROUTES.iter().find(|r| r.kind == kind).unwrap();
        // The project kinds need the project.
        for kind in ["overlays.list", "overlays.removed", "checkpoints.list"] {
            let e = routes::args_of(route(kind), None, &[]).unwrap_err();
            assert_eq!(e.code, Code::NoProject, "{kind}");
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let args = routes::args_of(
            route("overlays.removed"),
            None,
            &[
                ("path".into(), json!(format!("{root}/a.rs"))),
                ("project_root".into(), json!(root)),
            ],
        )
        .unwrap();
        assert_eq!(args[..2], words(&["remove", "--json"])[..]);
        assert!(
            super::super::flag(&args, "--path")
                .unwrap()
                .unwrap()
                .ends_with("/a.rs")
        );
        assert_eq!(
            super::super::positionals(&args),
            vec!["remove"],
            "--path takes a value"
        );
        let args = routes::args_of(
            route("session.context"),
            None,
            &[("session".into(), json!("app-1"))],
        )
        .unwrap();
        assert_eq!(session_arg(&args).unwrap(), "app-1");
        assert!(session_arg(&words(&["context", "--json"])).is_err());
        assert!(session_arg(&words(&["context", "--session", "../x"])).is_err());
    }

    #[test]
    fn the_cli_claims_only_its_json_verbs() {
        assert_eq!(
            kind_of("session", &words(&["context", "--json", "--session", "a"])),
            Some("session.context")
        );
        assert_eq!(
            kind_of("session", &words(&["--session", "a", "value", "--json"])),
            Some("session.value")
        );
        assert_eq!(
            kind_of("overlays", &words(&["remove", "--path", "/p/a", "--json"])),
            Some("overlays.removed")
        );
        assert_eq!(
            kind_of("telemetry", &words(&["status", "--json"])),
            Some("telemetry.status")
        );
        assert_eq!(kind_of("session", &words(&["stats", "--json"])), None);
        assert_eq!(kind_of("telemetry", &words(&["on"])), None);
        assert!(
            !try_run("update", &words(&["status"])),
            "text stays the command's"
        );
    }

    #[test]
    fn overlays_list_the_effective_control_per_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Joined part by part: the listed path uses the platform separator.
        let abs = root.join("src").join("a.rs").to_string_lossy().into_owned();
        let mut store = OverlayStore::new();
        store.add(overlay(
            &abs,
            OverlayOp::Pin { verbatim: false },
            OverlayAuthor::User,
        ));
        store.add(overlay(
            "src/a.rs",
            OverlayOp::SetView(ViewKind::Signatures),
            OverlayAuthor::User,
        ));
        store.add(overlay(
            "lib.rs",
            OverlayOp::Exclude { reason: "x".into() },
            OverlayAuthor::Agent("cl-1".into()),
        ));
        store.add(overlay("lib.rs", OverlayOp::Include, OverlayAuthor::User));
        store.add(overlay(
            "lib.rs",
            OverlayOp::SetPriority { set_priority: 0.9 },
            OverlayAuthor::User,
        ));
        store.add(overlay(
            "lib.rs",
            OverlayOp::MarkOutdated,
            OverlayAuthor::Policy("p".into()),
        ));
        store.add(overlay(
            "../escape.rs",
            OverlayOp::Pin { verbatim: false },
            OverlayAuthor::User,
        ));
        store.add(overlay(
            "/elsewhere/b.rs",
            OverlayOp::Pin { verbatim: false },
            OverlayAuthor::User,
        ));
        let doc = overlays_doc(root, &store);
        assert!(doc.full_paths);
        assert_eq!(doc.overlays.len(), 2, "{doc:?}");
        let lib = &doc.overlays[0];
        assert_eq!(lib.path, root.join("lib.rs").to_string_lossy());
        assert_eq!(
            (lib.state, lib.priority, lib.outdated),
            ("none", Some(0.9), true)
        );
        assert_eq!(lib.set_by.as_deref(), Some("policy:p"));
        assert_eq!(lib.scope, "persistent");
        let a = &doc.overlays[1];
        assert_eq!(a.path, abs);
        assert_eq!((a.state, a.view), ("pinned", Some("signatures")));
        assert_eq!(a.target, "src/a.rs", "the newest key");
        assert!(a.set_at.as_deref().is_some_and(|t| t.contains('T')));
        // The app's row fields.
        let v = serde_json::to_value(&doc).unwrap();
        for key in [
            "path", "target", "state", "view", "priority", "outdated", "scope", "stale", "set_by",
            "set_at",
        ] {
            assert!(v["overlays"][0].get(key).is_some(), "{key}");
        }
        // Same store, same bytes.
        assert_eq!(
            super::super::document("overlays.list", &doc),
            super::super::document("overlays.list", &overlays_doc(root, &store))
        );
    }

    #[test]
    fn removing_drops_the_pin_and_keeps_the_view() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let abs = root.join("a.rs");
        let mut store = OverlayStore::new();
        store.add(overlay(
            "a.rs",
            OverlayOp::Pin { verbatim: false },
            OverlayAuthor::User,
        ));
        store.add(overlay(
            &abs.to_string_lossy(),
            OverlayOp::Exclude { reason: "x".into() },
            OverlayAuthor::User,
        ));
        store.add(overlay(
            "a.rs",
            OverlayOp::SetView(ViewKind::Map),
            OverlayAuthor::User,
        ));
        store.add(overlay(
            "b.rs",
            OverlayOp::Pin { verbatim: false },
            OverlayAuthor::User,
        ));
        let rel = file_of(root, &abs.to_string_lossy()).unwrap();
        let (doc, changed) = remove_state(root, &rel, &mut store);
        assert!(changed && doc.removed);
        assert_eq!((doc.state_before, doc.state), ("excluded", "none"));
        assert_eq!(doc.path, abs.to_string_lossy());
        let after = overlays_doc(root, &store);
        let a = after.overlays.iter().find(|r| r.path == doc.path).unwrap();
        assert_eq!((a.state, a.view), ("none", Some("map")));
        assert!(
            after.overlays.iter().any(|r| r.state == "pinned"),
            "b.rs keeps its pin"
        );
        // Saved and loaded through the project store.
        store.save_project(root).unwrap();
        let (again, changed) = remove_state(root, &rel, &mut load_overlays(root));
        assert!(!changed && !again.removed);
        // The app's proof fields.
        let v: Value =
            serde_json::from_str(&super::super::document("overlays.removed", &doc)).unwrap();
        assert!(v["path"].is_string() && v["removed"].is_boolean());
        // Only files of the project.
        assert_eq!(
            file_of(root, "a.rs").unwrap_err().code,
            Code::InvalidArgument
        );
        // An absolute path outside the project (on Windows, `/elsewhere`
        // has no drive and is not absolute).
        let other = tempfile::tempdir().unwrap();
        let elsewhere = other.path().join("a.rs");
        assert_eq!(
            file_of(root, &elsewhere.to_string_lossy())
                .unwrap_err()
                .code,
            Code::Forbidden
        );
        let up = format!("{}/../a.rs", root.display());
        assert_eq!(file_of(root, &up).unwrap_err().code, Code::Forbidden);
    }

    #[test]
    fn checkpoints_carry_their_ref_and_files() {
        use lean_ctx::core::git::shadow::Checkpoint;
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let doc = checkpoints_doc(
            vec![Checkpoint {
                sha: sha.into(),
                time: "2026-11-05T08:00:00+00:00".into(),
                message: "first".into(),
                files_changed: None,
            }],
            &|s| {
                if s == sha {
                    vec!["a.txt".into()]
                } else {
                    Vec::new()
                }
            },
        );
        let v = serde_json::to_value(&doc).unwrap();
        assert_eq!(
            v,
            json!({"checkpoints": [{"id": "0123456789ab", "ref": sha, "created": "2026-11-05T08:00:00+00:00",
                "message": "first", "files_changed": ["a.txt"]}], "files_known": true, "source": "shadow"})
        );
    }

    #[test]
    fn handoff_names_task_findings_files_and_text() {
        use lean_ctx::core::session::{Decision, Finding, TaskInfo};
        let mut s = SessionState::new();
        let at = s.updated_at;
        s.task = Some(TaskInfo {
            description: " ship wp3 ".into(),
            intent: None,
            progress_pct: None,
        });
        s.findings.push(Finding {
            file: None,
            line: None,
            summary: "found".into(),
            timestamp: at,
        });
        s.decisions.push(Decision {
            summary: "decided".into(),
            rationale: None,
            timestamp: at,
        });
        s.next_steps.push("next".into());
        let doc = handoff_doc("app-1", &s);
        assert_eq!(doc.task.as_deref(), Some("ship wp3"));
        assert_eq!(
            (doc.findings.clone(), doc.decisions.clone()),
            (vec!["found".to_string()], vec!["decided".to_string()])
        );
        assert_eq!(doc.text, None, "no cross-session decisions");
        assert_eq!(doc.lean_session, s.id);
        let v = serde_json::to_value(&doc).unwrap();
        assert!(v["text"].is_null() && v["files"] == json!([]));
        assert_eq!(handoff_doc("app-1", &s), doc, "deterministic");
    }

    #[test]
    fn context_pressure_is_use_over_the_window() {
        let mut s = SessionState::new();
        s.stats.total_tokens_input = 60_000;
        s.stats.total_tokens_saved = 10_000;
        let window = |m: &str| if m == "claude-x" { 200_000 } else { 0 };
        let spend = vec![("gpt-y".to_string(), 2), ("claude-x".to_string(), 5)];
        let doc = context_doc("app-1", std::slice::from_ref(&s), &spend, &window).unwrap();
        assert_eq!(doc.model.as_deref(), Some("claude-x"));
        assert_eq!(
            (doc.window_tokens, doc.used_tokens, doc.pressure),
            (Some(200_000), 50_000, Some(0.25))
        );
        assert_eq!(doc.source, "session+proxy");
        let alone = context_doc("app-1", &[s], &[], &window).unwrap();
        assert_eq!(
            (
                alone.model,
                alone.window_tokens,
                alone.pressure,
                alone.source
            ),
            (None, None, None, "session")
        );
        assert_eq!(
            context_doc("app-1", &[], &[], &window).unwrap_err().code,
            Code::NotFound
        );
        let v = serde_json::to_value(&doc).unwrap();
        for key in [
            "model",
            "window_tokens",
            "used_tokens",
            "pressure",
            "source",
        ] {
            assert!(v.get(key).is_some(), "{key}");
        }
    }

    #[test]
    fn update_status_names_versions_only() {
        use lean_ctx::core::config::UpdatesConfig;
        use lean_ctx::core::updater::ReceiptStatus;
        let updates = UpdatesConfig {
            pinned_version: Some("3.12.4".into()),
            ..UpdatesConfig::default()
        };
        let doc = update_doc(
            "3.12.5",
            ReceiptStatus::Valid {
                active: "3.12.5".into(),
                previous: "3.12.4".into(),
            },
            &updates,
        );
        assert_eq!(
            (doc.kept.as_deref(), doc.receipt, doc.receipt_error),
            (Some("3.12.4"), true, false)
        );
        assert_eq!(doc.pinned.as_deref(), Some("3.12.4"));
        let bad = update_doc("3.12.5", ReceiptStatus::Invalid, &updates);
        assert_eq!(
            (bad.kept, bad.receipt, bad.receipt_error),
            (None, false, true)
        );
        let text = super::super::document("update.status", &build_update());
        assert_eq!(
            text,
            super::super::document("update.status", &build_update()),
            "deterministic"
        );
        let keys: Vec<String> = serde_json::to_value(build_update())
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            keys.len(),
            9,
            "versions and switches only, no paths or digests: {keys:?}"
        );
    }

    #[test]
    fn security_status_shows_switches_not_paths() {
        use lean_ctx::core::security_posture::{JailState, SecurityPosture};
        use lean_ctx::core::shell_allowlist::ShellSecurity;
        let posture = SecurityPosture {
            jail: JailState::Relaxed(vec!["LEAN_CTX_ALLOW_PATH".into()]),
            shell: ShellSecurity::Enforce,
            secrets_enabled: true,
            secrets_redact: true,
        };
        let doc = security_doc(&posture, 2);
        assert_eq!(
            (doc.level, doc.path_jail, doc.shell),
            ("relaxed", "relaxed", "enforce")
        );
        assert_eq!(doc.relaxed_by, vec!["LEAN_CTX_ALLOW_PATH".to_string()]);
        assert!(doc.secrets_protected);
        assert_eq!(doc.allow_paths_count, 2);
        let text = super::super::document("security.status", &build_security());
        assert_eq!(
            text,
            super::super::document("security.status", &build_security()),
            "deterministic"
        );
    }

    #[test]
    fn telemetry_status_follows_the_eligibility_authority() {
        use lean_ctx::core::config::{TelemetryConfig, TelemetryPreference};
        let on = TelemetryConfig::default();
        let doc = telemetry_doc(&on, None, None, false, Some("2026-11-14".into()));
        assert_eq!(
            (
                doc.preference,
                doc.preference_source,
                doc.sending,
                doc.blocked_by
            ),
            ("enabled", "default_on", true, None)
        );
        assert_eq!(
            telemetry_doc(&on, Some("1"), None, false, None).blocked_by,
            Some("environment")
        );
        assert_eq!(
            telemetry_doc(&on, None, None, true, None).blocked_by,
            Some("ci")
        );
        let off = TelemetryConfig {
            enabled: false,
            preference: TelemetryPreference::ExplicitlyDisabled,
            last_heartbeat: None,
        };
        let doc = telemetry_doc(&off, None, None, false, None);
        assert_eq!(
            (doc.preference, doc.preference_source, doc.blocked_by),
            ("disabled", "explicit_disabled", Some("preference"))
        );
        assert!(!doc.disclosure.is_empty());
        let v = serde_json::to_value(&doc).unwrap();
        assert!(v.get("install_id").is_none() && v.get("installation_id").is_none());
        if let Ok(a) = build_telemetry() {
            let b = build_telemetry().unwrap();
            assert_eq!(
                super::super::document("telemetry.status", &a),
                super::super::document("telemetry.status", &b)
            );
        }
    }
}
