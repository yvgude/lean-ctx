// SPDX-License-Identifier: Apache-2.0
//! The layered policy editor (rel12 WP6) as Local Read kinds: Cloud >
//! project > personal, tighten-only.
//!
//! - `policies.layers`: `lean-ctx policy layers --json --project-root P`,
//!   every rule of the three layers with its origin, lock state and whether
//!   it can be edited, the Cloud policy's state, and each editable layer's
//!   revision (the digest a change names).
//! - `policies.changed`: `lean-ctx policy change --json --project-root P
//!   --level project|personal --expected-revision R` with `{op, rule}` on
//!   stdin (or `--op`/`--rule`); POST on the socket. One rule is added,
//!   edited or removed when the change only tightens what a higher layer
//!   sets; otherwise the document names the typed refusal and nothing is
//!   written. Each write appends its evidence to the local policy history
//!   and to the audit trail; the release receipt of the write rides the
//!   existing receipt outbox to Cloud (content-free, when uploads are allowed).
//! - `policies.history`: `lean-ctx policy history --json --project-root P
//!   [--limit N]`, the recorded changes of the project and of the personal
//!   layer, newest first.
//!
//! The Cloud layer is the installed Cloud policy floor
//! ([`lean_ctx_cloud::effective_floor`], same read and check as enforcement):
//! read only, every rule locked. The project layer is the project's
//! `.lean-ctx/policies.json` (written through PathJail). The personal layer
//! is `<data_dir>/policy-layers/personal.json`, free for everyone (paid
//! features are Cloud rules, team and audit); it is shown and stored, not
//! yet enforced by the engine.
//!
//! `policies.layers` and `policies.history` are functions of the stored data
//! (and of whether the stored Cloud policy is still in force): they generate
//! no timestamp. A `policies.changed` write result carries the time of its
//! evidence (`evidence.at`).
//!
//! A change holds one lock (`<data_dir>/policy-layers/.lock`) across reading
//! the layer, comparing its revision, recording the evidence (audit trail,
//! then history) and replacing the layer file: two writers naming the same
//! revision never both apply, and evidence is recorded before the change.

use std::path::{Path, PathBuf};

use lean_ctx_time::UtcDateTime;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use lean_ctx::core::context_policies::{ContextPolicy, PolicyAction, PolicySet, PolicySource};
use lean_ctx_cloud::effective_floor;
use lean_ctx_protocol::cloud::effective_policy::EffectivePolicyV1;

use super::policies::policy_row;
use super::routes::{Method, Param, Project, Route, Shape};
use super::{Answer, Code, Failure, Via};

/// The tool run these documents are released under.
const TOOL: &str = "ctx_policy";

const PROJECT_ROOT: Param = Param {
    name: "project_root",
    flag: "--project-root",
    shape: Shape::Value,
};
const LEVEL: Param = Param {
    name: "level",
    flag: "--level",
    shape: Shape::Value,
};
const EXPECTED_REVISION: Param = Param {
    name: "expected_revision",
    flag: "--expected-revision",
    shape: Shape::Value,
};
const OP: Param = Param {
    name: "op",
    flag: "--op",
    shape: Shape::Value,
};
const RULE: Param = Param {
    name: "rule",
    flag: "--rule",
    shape: Shape::Json,
};
const LIMIT: Param = Param {
    name: "limit",
    flag: "--limit",
    shape: Shape::Value,
};

/// This part's routes (Route as in `routes.rs`).
pub(crate) const ROUTES: &[Route] = &[
    Route {
        kind: "policies.layers",
        method: Method::Get,
        path: "/v1/local/policies/layers",
        cli: &["policy", "layers", "--json"],
        action: &["layers"],
        params: &[PROJECT_ROOT],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "policies.changed",
        method: Method::Post,
        path: "/v1/local/policies/change",
        cli: &["policy", "change", "--json"],
        action: &["change"],
        params: &[PROJECT_ROOT, LEVEL, EXPECTED_REVISION, OP, RULE],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "policies.history",
        method: Method::Get,
        path: "/v1/local/policies/history",
        cli: &["policy", "history", "--json"],
        action: &["history"],
        params: &[PROJECT_ROOT, LIMIT],
        project: Project::Required,
        names: None,
    },
];

/// This part's document kinds (sorted).
pub(crate) const KINDS: &[&str] = &["policies.changed", "policies.history", "policies.layers"];

/// Command path → CLI verbs, as `capabilities::COMMANDS`.
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[("policy", &["change", "history", "layers"])];

/// The project layer's file below the project's `.lean-ctx`.
const PROJECT_FILE: &str = "policies.json";
/// The personal layer and the change history, below the data directory.
const LAYERS_DIR: &str = "policy-layers";
const PERSONAL_FILE: &str = "personal.json";
const HISTORY_FILE: &str = "history.jsonl";
/// The lock a change holds while it reads, records and writes.
const LOCK_FILE: &str = ".lock";
/// Entries the history keeps (oldest dropped first).
const MAX_HISTORY: usize = 1000;
/// The largest change body read from stdin.
const MAX_BODY: usize = 64 * 1024;

/// What the documents are built from (the data directories and the clock
/// that decides whether the stored Cloud policy is still in force).
pub(crate) struct Ctx<'a> {
    pub project_root: &'a Path,
    /// `<data_dir>/cloud` (the Cloud store), when there is one.
    pub cloud_data: Option<&'a Path>,
    /// The LeanCTX data directory (personal layer, history).
    pub data_dir: &'a Path,
    pub now: UtcDateTime,
}

// ------------------------------------------------------------------- layers

/// Where a rule comes from.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct Origin {
    /// `org` | `vault` (Cloud) | `project` | `default` (built-in, while the
    /// project has no policy file) | `security_default` | `personal`.
    pub level: &'static str,
    /// The vault whose Cloud policy applies to this project, for `vault`.
    pub vault: Option<String>,
    /// The Cloud policy version.
    pub version: Option<u64>,
    /// The key the Cloud policy was signed with, when recorded.
    pub key_id: Option<String>,
    pub issued_at: Option<String>,
}

impl Origin {
    fn local(level: &'static str) -> Self {
        Self {
            level,
            vault: None,
            version: None,
            key_id: None,
            issued_at: None,
        }
    }
}

/// One rule of a layer.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct RuleRow {
    /// `cloud:<member>` / `cloud:<member>[i]`, `security:<name>`,
    /// `project:<name>` or `personal:<name>`.
    pub id: String,
    pub name: String,
    /// The path glob, or `null` for a rule that matches no path.
    #[serde(rename = "match")]
    pub match_pattern: Option<String>,
    /// A context-policy action (`exclude`, `include`, `pin`, `set_view`,
    /// `max_tokens`, `mark_outdated`, `redact`, `audit`) or, for Cloud,
    /// `exclude` | `never_send` | `restrict_roots` | `block_secrets` |
    /// `filter` | `allow_tools` | `deny_tools` | `max_tokens`.
    pub action: String,
    pub action_params: Value,
    pub condition: Option<String>,
    pub condition_params: Value,
    pub reason: Option<String>,
    /// `cloud` | `project` | `personal`.
    pub layer: &'static str,
    /// No layer below may change it (Cloud rules, security defaults).
    pub locked: bool,
    /// A `policies.changed` request may change it now.
    pub editable: bool,
    pub origin: Origin,
}

/// The Cloud layer: the installed Cloud policy as this Instance checks it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct CloudView {
    /// `in_force` | `offline_last_known` (past `not_after`: the last valid
    /// policy still restricts) | `none` | `invalid` (damaged: fails closed).
    pub state: &'static str,
    /// `verified` | `invalid` | `none`.
    pub signature: String,
    pub version: Option<u64>,
    pub digest: Option<String>,
    pub key_id: Option<String>,
    pub issued_at: Option<String>,
    pub not_after: Option<String>,
    /// When this Instance last pulled the policy from Cloud.
    pub last_pull_at: Option<String>,
    /// The vault whose entry applies to this project, else `null` (default).
    pub vault: Option<String>,
    /// The path globs the Cloud rules cover (`excluded_paths` and
    /// `never_send`), sorted.
    pub covered: Vec<String>,
    /// The coverage cannot be bounded (damaged policy): every path counts.
    pub covers_everything: bool,
    pub rules: Vec<RuleRow>,
    /// Why the installed policy could not be read, else `null`.
    pub error: Option<String>,
}

/// An editable layer (project or personal).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct LayerDoc {
    /// `project` | `personal`.
    pub level: &'static str,
    pub file: String,
    pub exists: bool,
    /// The file exists but is no policy file (or cannot be read safely):
    /// changes are refused until it is fixed.
    pub damaged: bool,
    /// `sha256:<hex>` of the file's bytes (of no bytes when absent): the
    /// `expected_revision` of a change; `null` when the file cannot be read.
    pub revision: Option<String>,
    /// Changes are accepted now.
    pub editable: bool,
    /// The engine applies the layer's rules.
    pub enforced: bool,
    pub rules: Vec<RuleRow>,
    pub error: Option<String>,
}

/// The `policies.layers` body.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct LayersDoc {
    pub project_root: String,
    /// Highest first: a lower layer may only tighten a higher one.
    pub precedence: [&'static str; 3],
    pub cloud: CloudView,
    pub project: LayerDoc,
    pub personal: LayerDoc,
}

/// `sha256:<hex>` of `bytes`.
fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// `path` canonical when it resolves, else as given.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn empty_cloud(state: &'static str, signature: &str, error: Option<String>) -> CloudView {
    CloudView {
        state,
        signature: signature.to_owned(),
        version: None,
        digest: None,
        key_id: None,
        issued_at: None,
        not_after: None,
        last_pull_at: None,
        vault: None,
        covered: Vec::new(),
        covers_everything: state == "invalid",
        rules: Vec::new(),
        error,
    }
}

/// The Cloud policy members as locked rows.
fn cloud_rows(policy: &EffectivePolicyV1, origin: &Origin) -> Vec<RuleRow> {
    let mut rows = Vec::new();
    let mut push = |id: String, name: &str, glob: Option<&String>, action: &str, params: Value| {
        rows.push(RuleRow {
            id,
            name: name.to_owned(),
            match_pattern: glob.cloned(),
            action: action.to_owned(),
            action_params: params,
            condition: None,
            condition_params: json!({}),
            reason: None,
            layer: "cloud",
            locked: true,
            editable: false,
            origin: origin.clone(),
        });
    };
    let security = &policy.security;
    let context = &policy.context;
    for (member, action, globs) in [
        ("excluded_paths", "exclude", &security.excluded_paths),
        ("never_send", "never_send", &security.never_send),
    ] {
        for (i, glob) in globs.iter().flatten().enumerate() {
            push(
                format!("cloud:{member}[{i}]"),
                &format!("security.{member}"),
                Some(glob),
                action,
                json!({}),
            );
        }
    }
    if let Some(roots) = &security.readable_roots {
        push(
            "cloud:readable_roots".into(),
            "security.readable_roots",
            None,
            "restrict_roots",
            json!({ "roots": roots }),
        );
    }
    if let Some(enabled) = security.block_secrets {
        push(
            "cloud:block_secrets".into(),
            "security.block_secrets",
            None,
            "block_secrets",
            json!({ "enabled": enabled }),
        );
    }
    for (name, action) in [
        ("security.pii", security.pii),
        ("security.classification", security.classification),
        ("context.injection_filter", context.injection_filter),
    ] {
        if let Some(action) = action {
            let member = name.split('.').nth(1).unwrap_or(name);
            push(
                format!("cloud:{member}"),
                name,
                None,
                "filter",
                json!({ "action": action.as_str() }),
            );
        }
    }
    for (member, action, tools) in [
        ("allowed_tools", "allow_tools", &context.allowed_tools),
        ("denied_tools", "deny_tools", &context.denied_tools),
    ] {
        if let Some(tools) = tools {
            push(
                format!("cloud:{member}"),
                &format!("context.{member}"),
                None,
                action,
                json!({ "tools": tools }),
            );
        }
    }
    if let Some(limit) = context.max_context_tokens {
        push(
            "cloud:max_context_tokens".into(),
            "context.max_context_tokens",
            None,
            "max_tokens",
            json!({ "limit": limit }),
        );
    }
    rows
}

/// The Cloud layer for `project_root`: the installed floor, read and
/// checked exactly as enforcement does.
pub(crate) fn cloud_view(
    cloud_data: Option<&Path>,
    project_root: &Path,
    now: UtcDateTime,
) -> CloudView {
    let Some(cloud) = cloud_data else {
        return empty_cloud("none", "none", None);
    };
    let last_pull_at = lean_ctx_cloud::policy::load_pulls(cloud).last_pull_at;
    let checked = effective_floor::read(&effective_floor::floor_dir_in(cloud)).and_then(
        |(floor, required)| {
            effective_floor::check(floor.as_deref(), required.as_deref()).map(|f| (f, floor))
        },
    );
    let (floor, text) = match checked {
        Ok((Some(floor), text)) => (floor, text),
        Ok((None, _)) => {
            return CloudView {
                last_pull_at,
                ..empty_cloud("none", "none", None)
            };
        }
        Err(error) => {
            return CloudView {
                last_pull_at,
                ..empty_cloud("invalid", "invalid", Some(error))
            };
        }
    };
    let installed = lean_ctx_cloud::policy_sessions::installed(cloud, now);
    let state = if installed.state == "in_force" {
        "in_force"
    } else {
        "offline_last_known"
    };
    let vault = text
        .as_deref()
        .and_then(|text| effective_floor::vault_for(text, cloud, project_root));
    let effective = lean_ctx_cloud::policy::load_state(cloud)
        .effective
        .filter(|e| e.version == floor.version && e.digest == floor.digest);
    let origin = Origin {
        level: if vault.is_some() { "vault" } else { "org" },
        vault: vault.clone(),
        version: Some(floor.version),
        key_id: effective.as_ref().map(|e| e.key_id.clone()),
        issued_at: effective.as_ref().map(|e| e.issued_at.clone()),
    };
    let policy = floor.policy.for_vault(vault.as_deref());
    let mut covered: Vec<String> = policy
        .security
        .excluded_paths
        .iter()
        .chain(policy.security.never_send.iter())
        .flatten()
        .cloned()
        .collect();
    covered.sort();
    covered.dedup();
    CloudView {
        state,
        signature: installed.signature,
        version: Some(floor.version),
        digest: Some(floor.digest.clone()),
        key_id: origin.key_id.clone(),
        issued_at: origin.issued_at.clone(),
        not_after: Some(floor.not_after.clone()),
        last_pull_at,
        vault,
        covered,
        covers_everything: false,
        rules: cloud_rows(policy, &origin),
        error: None,
    }
}

/// One local rule as a row of `layer`.
fn local_row(
    policy: &ContextPolicy,
    source: PolicySource,
    layer: &'static str,
    layer_editable: bool,
) -> RuleRow {
    let row = policy_row(policy, source);
    let (prefix, origin, locked) = match (layer, source) {
        (_, PolicySource::SecurityDefault) => ("security", "security_default", true),
        ("personal", _) => ("personal", "personal", false),
        (_, PolicySource::Default) => ("project", "default", false),
        _ => ("project", "project", false),
    };
    RuleRow {
        id: format!("{prefix}:{}", row.name),
        name: row.name,
        match_pattern: Some(row.match_pattern),
        action: row.action.to_owned(),
        action_params: row.action_params,
        condition: row.condition.map(str::to_owned),
        condition_params: row.condition_params,
        reason: row.reason,
        layer,
        locked,
        editable: layer_editable && !locked,
        origin: Origin::local(origin),
    }
}

/// A layer file as stored: its bytes (absent: `None`) or why it cannot be
/// read safely.
struct Stored {
    file: PathBuf,
    bytes: Result<Option<Vec<u8>>, String>,
}

impl Stored {
    /// The digest of the stored bytes (absent: of no bytes); `None` when the
    /// file cannot be read.
    fn revision(&self) -> Option<String> {
        match &self.bytes {
            Ok(Some(bytes)) => Some(digest(bytes)),
            Ok(None) => Some(digest(b"")),
            Err(_) => None,
        }
    }

    /// The policies of the file: `Ok(None)` when absent, `Err` when damaged.
    fn policies(&self) -> Result<Option<PolicySet>, String> {
        match &self.bytes {
            Ok(None) => Ok(None),
            Ok(Some(bytes)) => serde_json::from_slice::<PolicySet>(bytes)
                .map(Some)
                .map_err(|_| format!("{} is not a policy file", self.file.display())),
            Err(error) => Err(error.clone()),
        }
    }
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{} is unreadable: {e}", path.display())),
    }
}

/// The project layer's file, read only through PathJail (a symlinked
/// `.lean-ctx` or file is refused, never followed).
fn stored_project(root: &Path) -> Stored {
    let file = root.join(".lean-ctx").join(PROJECT_FILE);
    let bytes =
        lean_ctx::core::pathjail_write::project_data_destination(root, Path::new(PROJECT_FILE))
            .and_then(|path| read_optional(&path));
    Stored { file, bytes }
}

fn stored_personal(data_dir: &Path) -> Stored {
    let file = data_dir.join(LAYERS_DIR).join(PERSONAL_FILE);
    let bytes = read_optional(&file);
    Stored { file, bytes }
}

/// The project layer's policies in effect order, built from the jailed bytes
/// only (as `PolicySet::load_project_sourced` orders them): the security
/// defaults, then the project file's policies, or the built-in defaults
/// without a usable file. A file that cannot be read safely shows only the
/// security defaults: nothing outside the project is ever shown.
fn project_sourced(root: &Path, stored: &Stored) -> Vec<(ContextPolicy, PolicySource)> {
    let tagged =
        |set: PolicySet, source: PolicySource| set.policies.into_iter().map(move |p| (p, source));
    let mut out: Vec<(ContextPolicy, PolicySource)> = tagged(
        PolicySet::security_defaults(),
        PolicySource::SecurityDefault,
    )
    .collect();
    let Ok(bytes) = &stored.bytes else {
        return out;
    };
    let project = bytes
        .as_deref()
        .filter(|_| !lean_ctx::core::pathutil::is_data_dir_collision(root))
        .and_then(|bytes| serde_json::from_slice::<PolicySet>(bytes).ok());
    match project {
        None => out.extend(tagged(
            PolicySet::attach_compat_defaults(),
            PolicySource::Default,
        )),
        Some(project) => {
            let reserved: Vec<String> = out.iter().map(|(p, _)| p.name.clone()).collect();
            out.extend(
                project
                    .policies
                    .into_iter()
                    .filter(|p| !reserved.contains(&p.name))
                    .map(|p| (p, PolicySource::Project)),
            );
        }
    }
    out
}

fn project_layer(root: &Path) -> LayerDoc {
    let stored = stored_project(root);
    let parsed = stored.policies();
    let damaged = parsed.is_err();
    LayerDoc {
        level: "project",
        file: stored.file.display().to_string(),
        exists: matches!(stored.bytes, Ok(Some(_))),
        damaged,
        revision: stored.revision(),
        editable: !damaged,
        enforced: true,
        rules: project_sourced(root, &stored)
            .iter()
            .map(|(policy, source)| local_row(policy, *source, "project", !damaged))
            .collect(),
        error: parsed.err(),
    }
}

fn personal_layer(data_dir: &Path) -> LayerDoc {
    let stored = stored_personal(data_dir);
    let parsed = stored.policies();
    let damaged = parsed.is_err();
    let editable = !damaged;
    let rules = match &parsed {
        Ok(Some(set)) => set
            .policies
            .iter()
            .map(|policy| local_row(policy, PolicySource::Project, "personal", editable))
            .collect(),
        _ => Vec::new(),
    };
    LayerDoc {
        level: "personal",
        file: stored.file.display().to_string(),
        exists: matches!(stored.bytes, Ok(Some(_))),
        damaged,
        revision: stored.revision(),
        editable,
        enforced: false,
        rules,
        error: parsed.err(),
    }
}

/// The `policies.layers` body.
pub(crate) fn layers_doc(ctx: &Ctx<'_>) -> LayersDoc {
    LayersDoc {
        project_root: ctx.project_root.display().to_string(),
        precedence: ["cloud", "project", "personal"],
        cloud: cloud_view(ctx.cloud_data, ctx.project_root, ctx.now),
        project: project_layer(ctx.project_root),
        personal: personal_layer(ctx.data_dir),
    }
}

// ------------------------------------------------------------------- change

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Op {
    Add,
    Edit,
    Remove,
}

impl Op {
    fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Edit => "edit",
            Self::Remove => "remove",
        }
    }
}

/// A rule as a change names it: the row shape of `policies.layers` (its
/// other fields are ignored).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct RuleInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    pub match_pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition_params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
}

/// One change request.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChangeRequest {
    /// `project` | `personal` (`cloud` is accepted only to be refused).
    pub level: &'static str,
    pub op: Op,
    pub expected_revision: String,
    pub rule: RuleInput,
}

/// Why a change was not applied.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct Refusal {
    /// `policy.loosens_higher_layer` | `policy.cloud_locked` |
    /// `policy.locked` | `policy.cloud_unreachable` |
    /// `policy.revision_changed`.
    pub reason: &'static str,
    /// The layer whose rule refused it.
    pub layer: Option<&'static str>,
    /// The id of that rule.
    pub rule: Option<String>,
    pub origin: Option<Origin>,
    pub message: String,
    /// The layer's revision now (`policy.revision_changed`).
    pub current_revision: Option<String>,
}

/// What a change recorded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct Evidence {
    pub at: String,
    pub actor: String,
    pub level: String,
    /// The project of a project-layer change (canonical).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    pub op: Op,
    pub rule_id: String,
    /// The rule as added or edited (removed: as it was).
    pub rule: Value,
    pub before_digest: String,
    pub after_digest: String,
    /// The audit-trail entry of the change (`audit:<entry hash>`): recorded
    /// before the change, which is not applied without it.
    pub evidence_ref: Option<String>,
}

/// The `policies.changed` body. A refusal is a document too (`applied:
/// false`, `refused` set): nothing was written.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct ChangedDoc {
    pub project_root: String,
    pub level: &'static str,
    pub op: Op,
    pub rule_id: Option<String>,
    pub applied: bool,
    pub refused: Option<Refusal>,
    /// The layer's revision after the change (refused: as it is).
    pub revision: Option<String>,
    pub evidence: Option<Evidence>,
}

fn parse_op(raw: &str) -> Result<Op, Failure> {
    serde_json::from_value(Value::String(raw.to_owned()))
        .map_err(|_| Failure::invalid(format!("op is add, edit or remove, not {raw}")))
}

/// The change `args` (and the stdin body, without `--op`) ask for.
pub(crate) fn parse_request(
    args: &[String],
    stdin: Option<&str>,
) -> Result<ChangeRequest, Failure> {
    let level = match super::flag(args, "--level")? {
        Some("project") => "project",
        Some("personal") => "personal",
        Some("cloud") => "cloud",
        Some(other) => {
            return Err(Failure::invalid(format!(
                "--level is project or personal, not {other}"
            )));
        }
        None => return Err(Failure::invalid("--level project|personal is required")),
    };
    let expected_revision = super::flag(args, "--expected-revision")?
        .ok_or_else(|| {
            Failure::invalid("--expected-revision R is required (the layer's revision)")
        })?
        .to_owned();
    let (op, rule) = if let Some(op) = super::flag(args, "--op")? {
        let op = parse_op(op)?;
        let text = super::flag(args, "--rule")?
            .ok_or_else(|| Failure::invalid("--rule '<json>' is required with --op"))?;
        let rule: RuleInput = serde_json::from_str(text)
            .map_err(|e| Failure::invalid(format!("--rule is not a rule object: {e}")))?;
        (op, rule)
    } else {
        #[derive(Deserialize)]
        struct Body {
            op: String,
            rule: RuleInput,
        }
        let text = stdin.ok_or_else(|| {
            Failure::invalid("give --op and --rule, or the body {op, rule} on stdin")
        })?;
        let body: Body = serde_json::from_str(text)
            .map_err(|e| Failure::invalid(format!("the body is not {{op, rule}}: {e}")))?;
        (parse_op(&body.op)?, body.rule)
    };
    Ok(ChangeRequest {
        level,
        op,
        expected_revision,
        rule,
    })
}

/// The rule name a change names: `name`, else the name of its `id`.
fn target_name(rule: &RuleInput) -> Option<String> {
    rule.name.clone().filter(|n| !n.is_empty()).or_else(|| {
        rule.id
            .as_deref()
            .and_then(|id| id.split_once(':'))
            .map(|(_, name)| name.to_owned())
    })
}

fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

/// `{tag: params}` when `params` is a non-empty object, else `"tag"` (the
/// context-policy JSON form).
fn tagged(tag: &str, params: Option<&Value>) -> Value {
    match params {
        Some(Value::Object(map)) if !map.is_empty() => {
            let mut out = Map::new();
            out.insert(tag.to_owned(), Value::Object(map.clone()));
            Value::Object(out)
        }
        _ => Value::String(tag.to_owned()),
    }
}

/// The context policy a change adds or edits (the one policy parser:
/// `ContextPolicy`'s own deserializer).
fn to_policy(rule: &RuleInput, name: &str) -> Result<ContextPolicy, Failure> {
    if !valid_name(name) {
        return Err(Failure::invalid(
            "a rule name is 1-64 characters of A-Z a-z 0-9 _ . -",
        ));
    }
    let glob = rule
        .match_pattern
        .as_deref()
        .filter(|m| !m.is_empty() && m.len() <= 512 && !m.contains(['\n', '\r']))
        .ok_or_else(|| Failure::invalid("match is a path glob of 1-512 characters"))?;
    // The engine matches normalised project-relative paths: these globs
    // would never match, so the rule would silently do nothing.
    if glob.starts_with('/') || glob.starts_with("./") || glob.split('/').any(|s| s == "..") {
        return Err(Failure::invalid(
            "match is relative to the project root: no leading / or ./ and no .. segment",
        ));
    }
    let action = rule
        .action
        .as_deref()
        .ok_or_else(|| Failure::invalid("action is required"))?;
    let mut out = Map::new();
    out.insert("name".into(), name.into());
    out.insert("match".into(), glob.into());
    out.insert("action".into(), tagged(action, rule.action_params.as_ref()));
    if let Some(condition) = rule.condition.as_deref() {
        out.insert(
            "condition".into(),
            tagged(condition, rule.condition_params.as_ref()),
        );
    }
    if let Some(reason) = rule.reason.as_deref() {
        if reason.len() > 512 {
            return Err(Failure::invalid("reason is at most 512 characters"));
        }
        out.insert("reason".into(), reason.into());
    }
    serde_json::from_value(Value::Object(out))
        .map_err(|e| Failure::invalid(format!("not a valid rule: {e}")))
}

/// `prefix` up to its first glob metacharacter (`*`, `?`, `[`, `{`);
/// `None` when nothing literal is left.
fn literal(prefix: &str) -> Option<&str> {
    let cut = prefix
        .find(['*', '?', '[', '{'])
        .map_or(prefix, |i| &prefix[..i]);
    (!cut.is_empty()).then_some(cut)
}

/// The literal prefix every path a local glob matches starts with, as the
/// context-policy matcher reads it; `None` when the glob is not anchored
/// (it may match anywhere).
fn anchor(glob: &str) -> Option<&str> {
    if glob == "**/*" || glob.starts_with("**") {
        return None;
    }
    let prefix = if let Some(prefix) = glob.strip_suffix("/**") {
        prefix
    } else if glob.contains("**") {
        let parts: Vec<&str> = glob.split("**").collect();
        if parts.len() != 2 {
            return None;
        }
        parts[0]
    } else {
        glob.strip_suffix('*')?
    };
    literal(prefix)
}

/// The literal prefix of a Cloud glob (gitignore grammar: `excluded_paths`,
/// `never_send`). A glob that starts with `/` or `!`, or has no `/` other
/// than a trailing one (it matches at any depth), counts as unanchored.
fn cloud_anchor(glob: &str) -> Option<&str> {
    if glob.starts_with(['/', '!']) {
        return None;
    }
    let trimmed = glob.trim_end_matches('/');
    if !trimmed.contains('/') {
        return None;
    }
    literal(trimmed)
}

/// Whether two literal prefixes may lead to one path (ASCII case folded:
/// case-insensitive file systems).
fn prefixes_meet(x: &str, y: &str) -> bool {
    let (x, y) = (x.to_ascii_lowercase(), y.to_ascii_lowercase());
    x.starts_with(&y) || y.starts_with(&x)
}

/// Whether some path may match both local globs (conservative: unanchored
/// globs overlap everything).
fn may_overlap(a: &str, b: &str) -> bool {
    match (anchor(a), anchor(b)) {
        (Some(x), Some(y)) => prefixes_meet(x, y),
        _ => true,
    }
}

/// Whether some path may match the Cloud glob `cloud` and the local glob
/// `local` (conservative, as [`may_overlap`]).
fn may_overlap_cloud(cloud: &str, local: &str) -> bool {
    match (cloud_anchor(cloud), anchor(local)) {
        (Some(x), Some(y)) => prefixes_meet(x, y),
        _ => true,
    }
}

/// An action that admits or keeps more context than without it.
fn permissive(action: &PolicyAction) -> bool {
    matches!(
        action,
        PolicyAction::Include | PolicyAction::Pin | PolicyAction::MarkOutdated
    )
}

/// An action that keeps context out or smaller.
fn restrictive(action: &PolicyAction) -> bool {
    matches!(
        action,
        PolicyAction::Exclude | PolicyAction::Redact | PolicyAction::MaxTokens { .. }
    )
}

fn security_default_names() -> Vec<String> {
    PolicySet::security_defaults()
        .policies
        .into_iter()
        .map(|p| p.name)
        .collect()
}

fn refused(
    ctx: &Ctx<'_>,
    req: &ChangeRequest,
    rule_id: Option<String>,
    refusal: Refusal,
    revision: Option<String>,
) -> ChangedDoc {
    ChangedDoc {
        project_root: ctx.project_root.display().to_string(),
        level: req.level,
        op: req.op,
        rule_id,
        applied: false,
        refused: Some(refusal),
        revision,
        evidence: None,
    }
}

/// The Cloud path rule `glob` overlaps, if any.
fn cloud_rule_over<'a>(cloud: &'a CloudView, glob: &str) -> Option<&'a RuleRow> {
    cloud.rules.iter().find(|row| {
        matches!(row.action.as_str(), "exclude" | "never_send")
            && row
                .match_pattern
                .as_deref()
                .is_some_and(|m| may_overlap_cloud(m, glob))
    })
}

/// Apply `req` when it only tightens the higher layers; the document says
/// what was written, or why nothing was. `audit` records the evidence in
/// the audit trail and returns its reference; when it fails, nothing is
/// written.
pub(crate) fn change(
    ctx: &Ctx<'_>,
    req: &ChangeRequest,
    actor: &str,
    audit: &dyn Fn(&Evidence) -> Result<String, String>,
) -> Result<ChangedDoc, Failure> {
    let cloud = cloud_view(ctx.cloud_data, ctx.project_root, ctx.now);
    let cloud_origin = || Origin {
        level: if cloud.vault.is_some() {
            "vault"
        } else {
            "org"
        },
        vault: cloud.vault.clone(),
        version: cloud.version,
        key_id: cloud.key_id.clone(),
        issued_at: cloud.issued_at.clone(),
    };
    // Cloud rules are changed in Cloud only.
    let cloud_id = req.rule.id.as_deref().filter(|id| id.starts_with("cloud:"));
    if req.level == "cloud" || cloud_id.is_some() || req.rule.layer.as_deref() == Some("cloud") {
        let row = cloud_id.and_then(|id| cloud.rules.iter().find(|r| r.id == id));
        return Ok(refused(
            ctx,
            req,
            req.rule.id.clone(),
            Refusal {
                reason: "policy.cloud_locked",
                layer: Some("cloud"),
                rule: req.rule.id.clone(),
                origin: Some(row.map_or_else(cloud_origin, |r| r.origin.clone())),
                message: "Cloud rules are set in LeanCTX Cloud; no local layer changes them".into(),
                current_revision: None,
            },
            None,
        ));
    }
    let name = target_name(&req.rule).ok_or_else(|| Failure::invalid("the rule needs a name"))?;
    let rule_id = format!("{}:{name}", req.level);
    let security_id = req
        .rule
        .id
        .as_deref()
        .filter(|id| id.starts_with("security:"));
    if security_default_names().contains(&name) || security_id.is_some() {
        return Ok(refused(
            ctx,
            req,
            Some(rule_id),
            Refusal {
                reason: "policy.locked",
                layer: Some("project"),
                rule: Some(format!("security:{name}")),
                origin: Some(Origin::local("security_default")),
                message: format!("{name} is a security default: it cannot be changed"),
                current_revision: None,
            },
            None,
        ));
    }
    // An id names its layer: a rule of another layer is never retargeted.
    if let Some((layer, _)) = req.rule.id.as_deref().and_then(|id| id.split_once(':')) {
        if layer != req.level {
            return Err(Failure::invalid(format!(
                "rule id {} is not a {} rule",
                req.rule.id.as_deref().unwrap_or_default(),
                req.level
            )));
        }
    }
    let new_policy = match req.op {
        Op::Remove => None,
        Op::Add | Op::Edit => Some(to_policy(&req.rule, &name)?),
    };
    // One writer at a time from here to the evidence and the write: the
    // revision compared is the one replaced.
    let layers_dir = ctx.data_dir.join(LAYERS_DIR);
    lean_ctx::core::secure_fs::private_dir(&layers_dir)
        .map_err(|e| Failure::new(Code::Internal, e.to_string()))?;
    let _lock = lean_ctx::core::agents::FileLock::acquire(&layers_dir.join(LOCK_FILE))
        .map_err(|e| Failure::new(Code::Internal, e))?;
    let stored = if req.level == "project" {
        stored_project(ctx.project_root)
    } else {
        stored_personal(ctx.data_dir)
    };
    let before = match (&stored.bytes, stored.revision()) {
        (Ok(_), Some(revision)) => revision,
        (Err(error), _) => return Err(Failure::new(Code::Forbidden, error.clone())),
        (Ok(_), None) => return Err(Failure::new(Code::Internal, "no revision")),
    };
    if req.expected_revision != before {
        return Ok(refused(
            ctx,
            req,
            Some(rule_id),
            Refusal {
                reason: "policy.revision_changed",
                layer: Some(req.level),
                rule: None,
                origin: None,
                message: "the layer changed since it was read; read policies.layers again".into(),
                current_revision: Some(before.clone()),
            },
            Some(before),
        ));
    }
    let mut set = match stored.policies() {
        Ok(Some(set)) => set,
        Ok(None) if req.level == "project" => PolicySet::attach_compat_defaults(),
        Ok(None) => PolicySet::default(),
        Err(error) => return Err(Failure::new(Code::Conflict, error)),
    };
    let index = set.policies.iter().position(|p| p.name == name);
    let old_policy = match (req.op, index) {
        (Op::Add, Some(_)) => {
            return Err(Failure::new(
                Code::Conflict,
                format!("a {} rule named {name} exists", req.level),
            ));
        }
        (Op::Edit | Op::Remove, None) => {
            return Err(Failure::new(
                Code::NotFound,
                format!("no {} rule named {name}", req.level),
            ));
        }
        (_, Some(i)) => Some(set.policies[i].clone()),
        (_, None) => None,
    };
    // Offline (or damaged), what the Cloud policy covers cannot be
    // confirmed: a change touching it fails closed.
    if matches!(cloud.state, "offline_last_known" | "invalid") {
        let globs: Vec<&str> = old_policy
            .iter()
            .chain(new_policy.iter())
            .map(|p| p.match_pattern.as_str())
            .collect();
        let over = globs.iter().find_map(|glob| cloud_rule_over(&cloud, glob));
        if cloud.covers_everything || over.is_some() {
            return Ok(refused(
                ctx,
                req,
                Some(rule_id),
                Refusal {
                    reason: "policy.cloud_unreachable",
                    layer: Some("cloud"),
                    rule: over.map(|r| r.id.clone()),
                    origin: Some(over.map_or_else(cloud_origin, |r| r.origin.clone())),
                    message: format!(
                        "the Cloud policy is {} and this rule touches what it covers; reconnect to change it",
                        cloud.state
                    ),
                    current_revision: None,
                },
                Some(before),
            ));
        }
    }
    if let Some(new) = new_policy.as_ref().filter(|p| permissive(&p.action)) {
        if let Some(row) = cloud_rule_over(&cloud, &new.match_pattern) {
            return Ok(refused(
                ctx,
                req,
                Some(rule_id),
                Refusal {
                    reason: "policy.loosens_higher_layer",
                    layer: Some("cloud"),
                    rule: Some(row.id.clone()),
                    origin: Some(row.origin.clone()),
                    message: format!(
                        "{} would admit what the Cloud rule {} keeps out",
                        new.match_pattern, row.id
                    ),
                    current_revision: None,
                },
                Some(before),
            ));
        }
        if req.level == "personal" {
            let project = project_sourced(ctx.project_root, &stored_project(ctx.project_root));
            let over = project.iter().find(|(p, source)| {
                *source != PolicySource::SecurityDefault
                    && restrictive(&p.action)
                    && may_overlap(&p.match_pattern, &new.match_pattern)
            });
            if let Some((p, source)) = over {
                let row = local_row(p, *source, "project", false);
                return Ok(refused(
                    ctx,
                    req,
                    Some(rule_id),
                    Refusal {
                        reason: "policy.loosens_higher_layer",
                        layer: Some("project"),
                        rule: Some(row.id.clone()),
                        origin: Some(row.origin),
                        message: format!(
                            "{} would admit what the project rule {} keeps out",
                            new.match_pattern, row.id
                        ),
                        current_revision: None,
                    },
                    Some(before),
                ));
            }
        }
    }
    match (index, new_policy.clone()) {
        (Some(i), Some(new)) => set.policies[i] = new,
        (Some(i), None) => {
            set.policies.remove(i);
        }
        (None, Some(new)) => set.policies.push(new),
        (None, None) => {}
    }
    let bytes = serde_json::to_string_pretty(&set)
        .map_err(|e| Failure::new(Code::Internal, e.to_string()))?;
    let after = digest(bytes.as_bytes());
    let recorded = new_policy
        .as_ref()
        .or(old_policy.as_ref())
        .map_or(Value::Null, |p| {
            serde_json::to_value(local_row(p, PolicySource::Project, req.level, true))
                .unwrap_or(Value::Null)
        });
    let mut evidence = Evidence {
        at: ctx
            .now
            .to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true),
        actor: actor.to_owned(),
        level: req.level.to_owned(),
        project_root: (req.level == "project")
            .then(|| canonical(ctx.project_root).display().to_string()),
        op: req.op,
        rule_id: rule_id.clone(),
        rule: recorded,
        before_digest: before,
        after_digest: after.clone(),
        evidence_ref: None,
    };
    // Evidence first, under the lock: a change whose evidence cannot be
    // recorded is not applied.
    let audit_ref = audit(&evidence).map_err(|e| {
        Failure::new(
            Code::Internal,
            format!("the change was not applied: its audit record failed: {e}"),
        )
    })?;
    evidence.evidence_ref = Some(audit_ref);
    let previous = history_lines(ctx.data_dir).map_err(|e| {
        Failure::new(
            Code::Internal,
            format!("the change was not applied: the history is unreadable: {e}"),
        )
    })?;
    append_history(ctx.data_dir, previous.clone(), &evidence)?;
    let written = if req.level == "project" {
        set.save_project(ctx.project_root)
            .map_err(|e| Failure::new(Code::Forbidden, e))
    } else {
        lean_ctx::core::secure_fs::write_private(&layers_dir, PERSONAL_FILE, bytes.as_bytes())
            .map_err(|e| Failure::new(Code::Internal, e.to_string()))
    };
    if let Err(failure) = written {
        // The layer is as it was: its history entry goes again (the audit
        // record stays; its digest names no revision the layer had).
        let _ = write_history(ctx.data_dir, &previous);
        return Err(failure);
    }
    Ok(ChangedDoc {
        project_root: ctx.project_root.display().to_string(),
        level: req.level,
        op: req.op,
        rule_id: Some(rule_id),
        applied: true,
        refused: None,
        revision: Some(after),
        evidence: Some(evidence),
    })
}

// ------------------------------------------------------------------ history

/// The recorded history lines (absent: none).
fn history_lines(data_dir: &Path) -> std::io::Result<Vec<String>> {
    match std::fs::read_to_string(data_dir.join(LAYERS_DIR).join(HISTORY_FILE)) {
        Ok(text) => Ok(text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_owned)
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Replace the history with `lines` (the newest [`MAX_HISTORY`]).
fn write_history(data_dir: &Path, lines: &[String]) -> std::io::Result<()> {
    let keep = &lines[lines.len().saturating_sub(MAX_HISTORY)..];
    let mut text = keep.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    lean_ctx::core::secure_fs::write_private(
        &data_dir.join(LAYERS_DIR),
        HISTORY_FILE,
        text.as_bytes(),
    )
}

/// `lines` (the history as read under the change lock) plus `evidence`.
fn append_history(
    data_dir: &Path,
    mut lines: Vec<String>,
    evidence: &Evidence,
) -> Result<(), Failure> {
    lines.push(
        serde_json::to_string(evidence).map_err(|e| Failure::new(Code::Internal, e.to_string()))?,
    );
    write_history(data_dir, &lines).map_err(|e| {
        Failure::new(
            Code::Internal,
            format!("the change was not applied: its history entry failed: {e}"),
        )
    })
}

/// The `policies.history` body.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct HistoryDoc {
    pub project_root: String,
    /// Newest first: the project's changes and the personal layer's.
    pub entries: Vec<Evidence>,
    /// Matching entries recorded (before `limit`).
    pub total: usize,
}

pub(crate) fn history_doc(ctx: &Ctx<'_>, limit: usize) -> HistoryDoc {
    let root = canonical(ctx.project_root).display().to_string();
    let matching: Vec<Evidence> = history_lines(ctx.data_dir)
        .unwrap_or_default()
        .iter()
        .filter_map(|line| serde_json::from_str::<Evidence>(line).ok())
        .filter(|e| e.level == "personal" || e.project_root.as_deref() == Some(root.as_str()))
        .collect();
    let total = matching.len();
    HistoryDoc {
        project_root: ctx.project_root.display().to_string(),
        entries: matching.into_iter().rev().take(limit).collect(),
        total,
    }
}

// ----------------------------------------------------------------- dispatch

/// The machine's directories and clock for one answer.
struct Env {
    data_dir: PathBuf,
    cloud_data: Option<PathBuf>,
    now: UtcDateTime,
}

impl Env {
    fn load() -> Result<Self, Failure> {
        let data_dir = lean_ctx::core::data_dir::lean_ctx_data_dir()
            .map_err(|e| Failure::new(Code::Internal, e))?;
        let cloud_data = lean_ctx_cloud::CloudStore::open()
            .ok()
            .map(|store| store.data_dir().to_path_buf())
            .or_else(|| {
                effective_floor::floor_dir()
                    .and_then(|dir| dir.parent()?.parent().map(Path::to_path_buf))
            });
        Ok(Self {
            data_dir,
            cloud_data,
            now: UtcDateTime::now(),
        })
    }

    fn ctx<'a>(&'a self, project_root: &'a Path) -> Ctx<'a> {
        Ctx {
            project_root,
            cloud_data: self.cloud_data.as_deref(),
            data_dir: &self.data_dir,
            now: self.now,
        }
    }
}

/// The person changing a layer on this machine.
fn actor() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|user| valid_name(user))
        .map_or_else(|| "local".to_owned(), |user| format!("local:{user}"))
}

/// The change's audit-trail entry (`x-policy_changed`, digests only).
fn record_audit(evidence: &Evidence) -> Result<String, String> {
    use lean_ctx::core::audit_trail::{AuditEntryData, AuditEventType, try_record};
    try_record(AuditEntryData {
        agent_id: evidence.actor.clone(),
        tool: TOOL.into(),
        action: Some(format!(
            "policies.changed:{}:{}",
            evidence.level,
            evidence.op.as_str()
        )),
        input_hash: evidence.after_digest.clone(),
        output_tokens: 0,
        role: "person".into(),
        event_type: AuditEventType::Unknown("x-policy_changed".into()),
    })
    .map(|entry| format!("audit:{}", entry.entry_hash))
}

/// The change body on stdin (CLI only; never on a terminal).
fn read_stdin() -> Option<String> {
    use std::io::{IsTerminal, Read};
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut text = String::new();
    stdin
        .lock()
        .take(MAX_BODY as u64 + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() <= MAX_BODY && !text.trim().is_empty()).then_some(text)
}

/// `f` with the project of `args`, or the `no_project` error document.
fn with_root(args: &[String], f: impl FnOnce(&str) -> Answer) -> Answer {
    match super::project_root(args) {
        Ok(root) => f(&root),
        Err(failure) => Answer::failure(&failure),
    }
}

/// The document of `kind` for `args` (same bytes on the CLI and the
/// socket), released through the `ctx_policy` tool run.
pub(crate) fn answer(kind: &str, args: &[String], via: Via) -> Answer {
    match kind {
        "policies.layers" => with_root(args, |root| {
            super::answer_released(TOOL, root, kind, via, || {
                let env = Env::load()?;
                Ok(layers_doc(&env.ctx(Path::new(root))))
            })
        }),
        "policies.history" => with_root(args, |root| {
            super::answer_released(TOOL, root, kind, via, || {
                let limit = super::limit(args, 50, 500)?;
                let env = Env::load()?;
                Ok(history_doc(&env.ctx(Path::new(root)), limit))
            })
        }),
        "policies.changed" => with_root(args, |root| {
            let stdin = match (via, super::flag(args, "--op")) {
                (Via::Cli, Ok(None)) => read_stdin(),
                _ => None,
            };
            let request = match parse_request(args, stdin.as_deref()) {
                Ok(request) => request,
                Err(failure) => return Answer::failure(&failure),
            };
            let preview = json!({
                "level": request.level,
                "op": request.op,
                "expected_revision": request.expected_revision,
                "rule": request.rule,
            });
            super::answer_released_write(TOOL, root, kind, via, &preview, || {
                let env = Env::load()?;
                change(&env.ctx(Path::new(root)), &request, &actor(), &record_audit)
            })
        }),
        other => Answer::failure(&Failure::invalid(format!("unknown kind {other}"))),
    }
}

/// `lean-ctx policy layers|change|history --json …`.
pub(crate) fn run(verb: &str, args: &[String]) {
    let kind = match verb {
        "layers" => "policies.layers",
        "change" => "policies.changed",
        _ => "policies.history",
    };
    super::print(&answer(kind, args, Via::Cli));
}

#[cfg(test)]
mod tests {
    use super::*;

    const IN_FORCE: &str = "2999-01-01T00:00:00Z";
    const EXPIRED: &str = "2026-01-01T00:00:00Z";

    fn now() -> UtcDateTime {
        "2026-11-15T00:00:00Z".parse().unwrap()
    }

    /// A project, a data dir and a Cloud store dir.
    struct World {
        project: tempfile::TempDir,
        data: tempfile::TempDir,
        cloud: tempfile::TempDir,
    }

    impl World {
        fn new() -> Self {
            let project = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(project.path().join(".git")).unwrap();
            Self {
                project,
                data: tempfile::tempdir().unwrap(),
                cloud: tempfile::tempdir().unwrap(),
            }
        }

        fn ctx(&self) -> Ctx<'_> {
            self.ctx_at(now())
        }

        fn ctx_at(&self, now: UtcDateTime) -> Ctx<'_> {
            Ctx {
                project_root: self.project.path(),
                cloud_data: Some(self.cloud.path()),
                data_dir: self.data.path(),
                now,
            }
        }

        fn revision(&self, level: &str) -> String {
            let doc = layers_doc(&self.ctx());
            if level == "project" {
                doc.project.revision
            } else {
                doc.personal.revision
            }
            .unwrap_or_default()
        }

        fn request(&self, level: &'static str, op: Op, rule: Value) -> ChangeRequest {
            ChangeRequest {
                level,
                op,
                expected_revision: self.revision(level),
                rule: serde_json::from_value(rule).unwrap(),
            }
        }

        fn apply(&self, level: &'static str, op: Op, rule: Value) -> ChangedDoc {
            let req = self.request(level, op, rule);
            change(
                &self.ctx(),
                &req,
                "local:test",
                &|_| Ok("audit:test".into()),
            )
            .unwrap()
        }

        /// Install a verified Cloud policy excluding `excluded` (sorted).
        fn install_floor(&self, excluded: &[&str], not_after: &str) {
            use lean_ctx_cloud::effective_floor::{
                FLOOR_FILE, FLOOR_SCHEMA, Floor, REQUIRED_FILE, floor_dir_in,
            };
            use lean_ctx_protocol::cloud::effective_policy::{
                BundlePolicyV1, POLICY_DOCUMENT_SCHEMA, SecurityPolicyV1,
            };
            let floor = Floor {
                schema: FLOOR_SCHEMA.into(),
                version: 3,
                digest: format!("sha256:{}", "a".repeat(64)),
                not_after: not_after.into(),
                policy: BundlePolicyV1 {
                    effective: EffectivePolicyV1 {
                        schema: POLICY_DOCUMENT_SCHEMA.into(),
                        security: SecurityPolicyV1 {
                            excluded_paths: Some(
                                excluded.iter().map(|s| (*s).to_owned()).collect(),
                            ),
                            block_secrets: Some(true),
                            ..Default::default()
                        },
                        context: Default::default(),
                        sharing: Default::default(),
                        agents: None,
                        orchestrator: None,
                        other: Default::default(),
                    },
                    vaults: Vec::new(),
                },
            };
            let dir = floor_dir_in(self.cloud.path());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(FLOOR_FILE), serde_json::to_vec(&floor).unwrap()).unwrap();
            std::fs::write(dir.join(REQUIRED_FILE), br#"{"version":3}"#).unwrap();
        }

        fn project_file(&self) -> PathBuf {
            self.project.path().join(".lean-ctx").join(PROJECT_FILE)
        }
    }

    fn reason(doc: &ChangedDoc) -> Option<&'static str> {
        doc.refused.as_ref().map(|r| r.reason)
    }

    fn rule(name: &str, glob: &str, action: &str) -> Value {
        json!({"name": name, "match": glob, "action": action})
    }

    /// Tighten-only: project against Cloud, personal against Cloud and
    /// project.
    #[test]
    fn tighten_only_matrix() {
        let w = World::new();
        w.install_floor(&["secrets/**"], IN_FORCE);

        let loosen = w.apply("project", Op::Add, rule("api", "secrets/api/**", "include"));
        assert_eq!(reason(&loosen), Some("policy.loosens_higher_layer"));
        let refusal = loosen.refused.as_ref().unwrap();
        assert_eq!(refusal.layer, Some("cloud"));
        assert_eq!(refusal.rule.as_deref(), Some("cloud:excluded_paths[0]"));
        let origin = refusal.origin.as_ref().unwrap();
        assert_eq!((origin.level, origin.version), ("org", Some(3)));
        assert!(!w.project_file().exists(), "nothing written on refusal");

        let tighter = w.apply(
            "project",
            Op::Add,
            rule("keys", "secrets/keys/**", "exclude"),
        );
        assert!(tighter.applied, "{tighter:?}");
        let disjoint = w.apply("project", Op::Add, rule("docs", "docs/**", "pin"));
        assert!(disjoint.applied, "{disjoint:?}");
        let unanchored = w.apply("project", Op::Add, rule("rs", "**/*.rs", "pin"));
        assert_eq!(reason(&unanchored), Some("policy.loosens_higher_layer"));

        let build = w.apply("project", Op::Add, rule("build", "build/**", "exclude"));
        assert!(build.applied);
        let personal = w.apply("personal", Op::Add, rule("out", "build/out/**", "pin"));
        assert_eq!(reason(&personal), Some("policy.loosens_higher_layer"));
        let refusal = personal.refused.as_ref().unwrap();
        assert_eq!(refusal.layer, Some("project"));
        assert_eq!(refusal.rule.as_deref(), Some("project:build"));
        let over_cloud = w.apply("personal", Op::Add, rule("s", "secrets/x/**", "include"));
        assert_eq!(over_cloud.refused.as_ref().unwrap().layer, Some("cloud"));
        let personal_tight = w.apply("personal", Op::Add, rule("tmp", "build/tmp/**", "exclude"));
        assert!(personal_tight.applied, "{personal_tight:?}");

        let doc = layers_doc(&w.ctx());
        let names: Vec<&str> = doc.project.rules.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"keys") && names.contains(&"docs") && names.contains(&"build"));
        assert!(!names.contains(&"api"));
        assert!(
            names.contains(&"compress_large_files"),
            "the first write keeps the built-in defaults in effect: {names:?}"
        );
        assert_eq!(doc.personal.rules.len(), 1);
        assert_eq!(doc.personal.rules[0].id, "personal:tmp");
    }

    /// Cloud rules and security defaults are locked and refused.
    #[test]
    fn cloud_and_security_rules_are_locked() {
        let w = World::new();
        w.install_floor(&["secrets/**"], IN_FORCE);
        let doc = layers_doc(&w.ctx());
        assert_eq!(doc.cloud.state, "in_force");
        assert_eq!(doc.cloud.signature, "verified");
        assert_eq!(doc.cloud.covered, ["secrets/**"]);
        assert!(
            doc.cloud
                .rules
                .iter()
                .all(|r| r.locked && !r.editable && r.layer == "cloud")
        );
        let ids: Vec<&str> = doc.cloud.rules.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["cloud:excluded_paths[0]", "cloud:block_secrets"]);
        let secrets = doc
            .project
            .rules
            .iter()
            .find(|r| r.name == "never_include_secrets")
            .unwrap();
        assert!(secrets.locked && !secrets.editable);
        assert_eq!(secrets.id, "security:never_include_secrets");
        assert_eq!(secrets.origin.level, "security_default");
        let default = doc
            .project
            .rules
            .iter()
            .find(|r| r.name == "compress_large_files")
            .unwrap();
        assert!(!default.locked && default.editable);
        assert_eq!(default.origin.level, "default");

        let by_id = w.apply(
            "project",
            Op::Edit,
            json!({"id": "cloud:excluded_paths[0]", "match": "secrets/**", "action": "include"}),
        );
        assert_eq!(reason(&by_id), Some("policy.cloud_locked"));
        assert_eq!(by_id.refused.unwrap().origin.unwrap().version, Some(3));
        let mut at_cloud = w.request("project", Op::Add, rule("x", "a/**", "exclude"));
        at_cloud.level = "cloud";
        let doc = change(&w.ctx(), &at_cloud, "local:test", &|_| {
            Ok("audit:none".into())
        })
        .unwrap();
        assert_eq!(reason(&doc), Some("policy.cloud_locked"));
        let security = w.apply(
            "project",
            Op::Remove,
            json!({"name": "never_include_secrets"}),
        );
        assert_eq!(reason(&security), Some("policy.locked"));
        assert!(!w.project_file().exists());
    }

    /// Past `not_after` (or damaged) the Cloud coverage cannot be
    /// confirmed: changes touching it fail closed, others go through.
    #[test]
    fn offline_cloud_fails_closed() {
        let w = World::new();
        w.install_floor(&["secrets/**"], EXPIRED);
        assert_eq!(layers_doc(&w.ctx()).cloud.state, "offline_last_known");
        let touching = w.apply("project", Op::Add, rule("k", "secrets/k/**", "exclude"));
        assert_eq!(reason(&touching), Some("policy.cloud_unreachable"));
        assert_eq!(
            touching.refused.unwrap().rule.as_deref(),
            Some("cloud:excluded_paths[0]")
        );
        let disjoint = w.apply("project", Op::Add, rule("d", "docs/**", "exclude"));
        assert!(disjoint.applied, "{disjoint:?}");

        let dir = effective_floor::floor_dir_in(w.cloud.path());
        std::fs::write(dir.join(effective_floor::FLOOR_FILE), b"{").unwrap();
        let doc = layers_doc(&w.ctx());
        assert_eq!(doc.cloud.state, "invalid");
        assert!(doc.cloud.covers_everything);
        let any = w.apply("project", Op::Add, rule("e", "elsewhere/**", "exclude"));
        assert_eq!(reason(&any), Some("policy.cloud_unreachable"));
    }

    /// A stale revision is refused with the current one; nothing is written.
    #[test]
    fn revision_conflict_is_refused() {
        let w = World::new();
        let first = w.apply("project", Op::Add, rule("a", "a/**", "exclude"));
        assert!(first.applied);
        let mut stale = w.request("project", Op::Add, rule("b", "b/**", "exclude"));
        stale.expected_revision = digest(b"");
        let before = std::fs::read(w.project_file()).unwrap();
        let doc = change(&w.ctx(), &stale, "local:test", &|_| Ok("audit:none".into())).unwrap();
        assert_eq!(reason(&doc), Some("policy.revision_changed"));
        let current = w.revision("project");
        assert_eq!(
            doc.refused.unwrap().current_revision.as_deref(),
            Some(current.as_str())
        );
        assert_eq!(first.revision.as_deref(), Some(current.as_str()));
        assert_eq!(std::fs::read(w.project_file()).unwrap(), before);
    }

    /// The history lists this project's and the personal changes, newest
    /// first, with the digest chain.
    #[test]
    fn history_is_newest_first() {
        let w = World::new();
        let other = World::new();
        let add = w.apply("project", Op::Add, rule("a", "a/**", "exclude"));
        let personal = w.apply("personal", Op::Add, rule("p", "p/**", "exclude"));
        let edit = w.apply("project", Op::Edit, rule("a", "a/b/**", "exclude"));
        assert!(add.applied && personal.applied && edit.applied);
        // Another project's change in the same data dir is not listed.
        let ctx = Ctx {
            data_dir: w.data.path(),
            ..other.ctx()
        };
        let req = ChangeRequest {
            level: "project",
            op: Op::Add,
            expected_revision: digest(b""),
            rule: serde_json::from_value(rule("o", "o/**", "exclude")).unwrap(),
        };
        assert!(
            change(&ctx, &req, "local:test", &|_| Ok("audit:none".into()))
                .unwrap()
                .applied
        );

        let doc = history_doc(&w.ctx(), 2);
        assert_eq!(doc.total, 3);
        let ops: Vec<(String, Op, String)> = doc
            .entries
            .iter()
            .map(|e| (e.level.clone(), e.op, e.rule_id.clone()))
            .collect();
        assert_eq!(
            ops,
            [
                ("project".into(), Op::Edit, "project:a".into()),
                ("personal".into(), Op::Add, "personal:p".into()),
            ]
        );
        let all = history_doc(&w.ctx(), 50);
        let (newest, oldest) = (&all.entries[0], &all.entries[2]);
        assert_eq!(newest.before_digest, oldest.after_digest);
        assert_eq!(oldest.before_digest, digest(b""));
        assert_eq!(newest.after_digest, w.revision("project"));
        assert_eq!(newest.evidence_ref.as_deref(), Some("audit:test"));
        assert_eq!(newest.actor, "local:test");
        assert_eq!(newest.at, "2026-11-15T00:00:00Z");
        assert_eq!(newest.rule["match"], "a/b/**");
    }

    /// A symlinked `.lean-ctx` is never followed: neither read nor written.
    #[cfg(unix)]
    #[test]
    fn project_layer_writes_through_pathjail() {
        let w = World::new();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), w.project.path().join(".lean-ctx")).unwrap();
        let doc = layers_doc(&w.ctx());
        assert!(
            doc.project.damaged && !doc.project.editable,
            "{:?}",
            doc.project
        );
        let req = ChangeRequest {
            level: "project",
            op: Op::Add,
            expected_revision: digest(b""),
            rule: serde_json::from_value(rule("a", "a/**", "exclude")).unwrap(),
        };
        let failure =
            change(&w.ctx(), &req, "local:test", &|_| Ok("audit:none".into())).unwrap_err();
        assert_eq!(failure.code, Code::Forbidden);
        assert!(!outside.path().join(PROJECT_FILE).exists());
    }

    /// The document is a function of the stored data, not of the clock.
    #[test]
    fn layers_document_is_deterministic() {
        let w = World::new();
        w.install_floor(&["secrets/**"], IN_FORCE);
        assert!(
            w.apply("project", Op::Add, rule("a", "a/**", "exclude"))
                .applied
        );
        let text = |at: &str| {
            super::super::document(
                "policies.layers",
                &layers_doc(&w.ctx_at(at.parse().unwrap())),
            )
        };
        assert_eq!(text("2026-11-15T00:00:00Z"), text("2027-03-01T12:34:56Z"));
        let history = |at: &str| {
            super::super::document(
                "policies.history",
                &history_doc(&w.ctx_at(at.parse().unwrap()), 10),
            )
        };
        assert_eq!(
            history("2026-11-15T00:00:00Z"),
            history("2028-01-01T00:00:00Z")
        );
    }

    /// Personal rules are free: add, edit and remove need no license.
    #[test]
    fn personal_layer_needs_no_license() {
        let w = World::new();
        let doc = layers_doc(&w.ctx());
        assert!(doc.personal.editable && !doc.personal.enforced);
        let add = w.apply("personal", Op::Add, rule("p", "p/**", "exclude"));
        assert!(add.applied, "{add:?}");
        let edit = w.apply("personal", Op::Edit, rule("p", "p/q/**", "exclude"));
        assert!(edit.applied, "{edit:?}");
        let doc = layers_doc(&w.ctx());
        assert_eq!(doc.personal.rules.len(), 1);
        let remove = w.apply("personal", Op::Remove, json!({"id": "personal:p"}));
        assert!(remove.applied, "{remove:?}");
        assert!(layers_doc(&w.ctx()).personal.rules.is_empty());
    }

    /// Writers naming the same revision: exactly one per layer applies, the
    /// others see `policy.revision_changed`, and no history line is lost.
    #[test]
    fn concurrent_writers_with_one_revision_apply_once() {
        let w = World::new();
        let project = w.revision("project");
        let personal = w.revision("personal");
        let threads = 12;
        let barrier = std::sync::Barrier::new(threads);
        let docs: Vec<ChangedDoc> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..threads)
                .map(|i| {
                    let (w, barrier) = (&w, &barrier);
                    let (level, expected) = if i % 2 == 0 {
                        ("project", project.clone())
                    } else {
                        ("personal", personal.clone())
                    };
                    scope.spawn(move || {
                        let req = ChangeRequest {
                            level,
                            op: Op::Add,
                            expected_revision: expected,
                            rule: serde_json::from_value(rule(
                                &format!("r{i}"),
                                &format!("r{i}/**"),
                                "exclude",
                            ))
                            .unwrap(),
                        };
                        barrier.wait();
                        change(&w.ctx(), &req, "local:test", &|_| Ok("audit:test".into())).unwrap()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for level in ["project", "personal"] {
            let of_level: Vec<&ChangedDoc> = docs.iter().filter(|d| d.level == level).collect();
            let applied = of_level.iter().filter(|d| d.applied).count();
            assert_eq!(applied, 1, "{level}: {of_level:?}");
            assert!(
                of_level
                    .iter()
                    .filter(|d| !d.applied)
                    .all(|d| reason(d) == Some("policy.revision_changed")),
                "{of_level:?}"
            );
        }
        let doc = layers_doc(&w.ctx());
        assert_eq!(doc.personal.rules.len(), 1);
        let history = history_doc(&w.ctx(), 50);
        assert_eq!(history.total, 2, "{:?}", history.entries);
        let personal_entry = history
            .entries
            .iter()
            .find(|e| e.level == "personal")
            .unwrap();
        assert_eq!(
            Some(personal_entry.after_digest.clone()),
            doc.personal.revision
        );
    }

    /// The evidence comes first: when the audit record or the history entry
    /// fails, the layer is not changed.
    #[test]
    fn failed_evidence_leaves_the_layer_unchanged() {
        let w = World::new();
        assert!(
            w.apply("personal", Op::Add, rule("p", "p/**", "exclude"))
                .applied
        );
        let personal_file = w.data.path().join(LAYERS_DIR).join(PERSONAL_FILE);
        let before = std::fs::read(&personal_file).unwrap();
        let req = w.request("personal", Op::Add, rule("q", "q/**", "exclude"));
        let failure =
            change(&w.ctx(), &req, "local:test", &|_| Err("disk full".into())).unwrap_err();
        assert_eq!(failure.code, Code::Internal);
        assert_eq!(std::fs::read(&personal_file).unwrap(), before);
        assert_eq!(history_doc(&w.ctx(), 50).total, 1);

        // A history that cannot be written: nothing is applied either.
        let history = w.data.path().join(LAYERS_DIR).join(HISTORY_FILE);
        std::fs::remove_file(&history).unwrap();
        std::fs::create_dir(&history).unwrap();
        let project_req = w.request("project", Op::Add, rule("a", "a/**", "exclude"));
        let failure = change(&w.ctx(), &project_req, "local:test", &|_| {
            Ok("audit:test".into())
        })
        .unwrap_err();
        assert_eq!(failure.code, Code::Internal);
        assert!(!w.project_file().exists());
        let failure =
            change(&w.ctx(), &req, "local:test", &|_| Ok("audit:test".into())).unwrap_err();
        assert_eq!(failure.code, Code::Internal);
        assert_eq!(std::fs::read(&personal_file).unwrap(), before);
    }

    /// Cloud globs are gitignore globs: anything not provably disjoint
    /// overlaps.
    #[test]
    fn cloud_globs_overlap_with_gitignore_grammar() {
        for (cloud, local) in [
            ("secrets*", "a/**"),
            ("/secrets/**", "secrets/x/**"),
            ("src/*/keys/**", "src/app/keys/**"),
            ("secret?/**", "secrets/x/**"),
            ("[s]ecrets/**", "secrets/x/**"),
            ("{a,b}/**", "a/x/**"),
            ("!keep/**", "a/**"),
            ("secrets/", "a/secrets/**"),
            ("secrets/**", "Secrets/x/**"),
        ] {
            assert!(may_overlap_cloud(cloud, local), "{cloud} vs {local}");
        }
        for (cloud, local) in [
            ("secrets/**", "docs/**"),
            ("src/*/keys/**", "docs/**"),
            ("secrets/**", "secretsx/**"),
        ] {
            assert!(!may_overlap_cloud(cloud, local), "{cloud} vs {local}");
        }
        // Local globs: the anchor stops at the first metacharacter.
        assert!(may_overlap("src/*/keys/**", "src/app/**"));

        let w = World::new();
        w.install_floor(&["secrets*"], IN_FORCE);
        let include = w.apply("project", Op::Add, rule("a", "a/**", "include"));
        assert_eq!(reason(&include), Some("policy.loosens_higher_layer"));
        let w = World::new();
        w.install_floor(&["src/*/keys/**"], IN_FORCE);
        let include = w.apply("project", Op::Add, rule("k", "src/app/keys/**", "include"));
        assert_eq!(reason(&include), Some("policy.loosens_higher_layer"));
    }

    /// The project layer's rules come from the jailed read only: a
    /// symlinked `.lean-ctx` shows the security defaults, never the outside
    /// file's rules.
    #[cfg(unix)]
    #[test]
    fn project_rules_are_read_through_pathjail() {
        let w = World::new();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(
            outside.path().join(PROJECT_FILE),
            serde_json::to_vec(&json!({"policies": [
                {"name": "outside", "match": "o/**", "action": "exclude"}
            ]}))
            .unwrap(),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), w.project.path().join(".lean-ctx")).unwrap();
        let doc = layers_doc(&w.ctx());
        assert!(doc.project.damaged);
        assert!(doc.project.revision.is_none());
        assert!(!doc.project.rules.is_empty());
        assert!(
            doc.project
                .rules
                .iter()
                .all(|r| r.origin.level == "security_default"),
            "{:?}",
            doc.project.rules
        );
    }

    /// A match that never meets a project-relative path is refused.
    #[test]
    fn non_relative_matches_are_refused() {
        let w = World::new();
        for glob in ["/abs/**", "./a/**", "a/../b/**", ".."] {
            let req = w.request("project", Op::Add, rule("x", glob, "exclude"));
            let failure =
                change(&w.ctx(), &req, "local:test", &|_| Ok("audit:test".into())).unwrap_err();
            assert_eq!(failure.code, Code::InvalidArgument, "{glob}");
        }
        assert!(!w.project_file().exists());
    }

    /// An unreadable personal file has no revision (not the empty one).
    #[test]
    fn unreadable_personal_file_has_no_revision() {
        let w = World::new();
        std::fs::create_dir_all(w.data.path().join(LAYERS_DIR).join(PERSONAL_FILE)).unwrap();
        let doc = layers_doc(&w.ctx());
        assert!(doc.personal.damaged && !doc.personal.editable);
        assert_eq!(doc.personal.revision, None);
        assert!(doc.personal.error.is_some());
    }

    /// An id of another layer is refused, never retargeted.
    #[test]
    fn an_id_of_another_layer_is_refused() {
        let w = World::new();
        assert!(
            w.apply("project", Op::Add, rule("p", "p/**", "exclude"))
                .applied
        );
        assert!(
            w.apply("personal", Op::Add, rule("p", "p/**", "exclude"))
                .applied
        );
        let before = std::fs::read(w.project_file()).unwrap();
        let req = w.request(
            "project",
            Op::Edit,
            json!({"id": "personal:p", "match": "p/q/**", "action": "exclude"}),
        );
        let failure =
            change(&w.ctx(), &req, "local:test", &|_| Ok("audit:test".into())).unwrap_err();
        assert_eq!(failure.code, Code::InvalidArgument);
        assert_eq!(std::fs::read(w.project_file()).unwrap(), before);
    }

    #[test]
    fn requests_parse_from_flags_and_body() {
        let args = |w: &[&str]| w.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let body = r#"{"op":"edit","rule":{"id":"project:a","match":"a/**","action":"max_tokens","action_params":{"limit":500},"locked":false}}"#;
        let req = parse_request(
            &args(&[
                "--level",
                "project",
                "--expected-revision",
                "sha256:x",
                "--json",
            ]),
            Some(body),
        )
        .unwrap();
        assert_eq!((req.level, req.op), ("project", Op::Edit));
        assert_eq!(target_name(&req.rule).as_deref(), Some("a"));
        let policy = to_policy(&req.rule, "a").unwrap();
        assert!(matches!(
            policy.action,
            PolicyAction::MaxTokens { limit: 500 }
        ));
        assert!(
            parse_request(
                &args(&["--level", "team", "--expected-revision", "r"]),
                Some(body)
            )
            .is_err()
        );
        assert!(parse_request(&args(&["--level", "project"]), Some(body)).is_err());
        assert!(
            parse_request(
                &args(&["--level", "project", "--expected-revision", "r"]),
                None
            )
            .is_err()
        );
        assert!(
            to_policy(
                &RuleInput {
                    match_pattern: Some("a".into()),
                    action: Some("explode".into()),
                    ..Default::default()
                },
                "a"
            )
            .is_err()
        );
        assert!(to_policy(&RuleInput::default(), "bad name").is_err());
    }

    /// Routed, listed with the `policy` verbs, and a socket body's rule
    /// object reaches the CLI parser intact.
    #[test]
    fn the_policy_layer_kinds_are_served_and_listed() {
        use super::super::{capabilities, routes};
        let mut routed: Vec<&str> = ROUTES.iter().map(|r| r.kind).collect();
        routed.sort_unstable();
        assert_eq!(routed, KINDS);
        let table = routes::table_json();
        let caps = capabilities::build_with(table.clone(), routes::events_json());
        let commands = capabilities::commands_of(&table);
        for (kind, method, path, verb) in [
            (
                "policies.layers",
                "GET",
                "/v1/local/policies/layers",
                "layers",
            ),
            (
                "policies.changed",
                "POST",
                "/v1/local/policies/change",
                "change",
            ),
            (
                "policies.history",
                "GET",
                "/v1/local/policies/history",
                "history",
            ),
        ] {
            let row = table
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["path"] == path)
                .unwrap_or_else(|| panic!("{path} in capabilities.routes"));
            assert_eq!(
                (row["kind"].as_str(), row["method"].as_str()),
                (Some(kind), Some(method))
            );
            assert_eq!(row["command"], "policy", "{row}");
            assert_eq!(row["verb"], verb, "{row}");
            assert!(
                commands["policy"]
                    .as_array()
                    .is_some_and(|v| v.contains(&json!({"verb": verb, "via": ["cli", "socket"]}))),
                "{verb}: {commands:?}"
            );
            assert!(caps.kinds.contains(&kind));
            assert!(super::super::part_of(kind).is_some());
        }
        let route = ROUTES
            .iter()
            .find(|r| r.kind == "policies.changed")
            .unwrap();
        assert_eq!(
            routes::args_of(route, None, &[]).unwrap_err().code,
            Code::NoProject
        );
        let dir = tempfile::tempdir().unwrap();
        let args = routes::args_of(
            route,
            None,
            &[
                ("project_root".into(), json!(dir.path().to_string_lossy())),
                ("level".into(), json!("project")),
                ("expected_revision".into(), json!("sha256:abc")),
                ("op".into(), json!("add")),
                (
                    "rule".into(),
                    json!({"name": "a", "match": "a/**", "action": "exclude"}),
                ),
            ],
        )
        .unwrap();
        let req = parse_request(&args, None).unwrap();
        assert_eq!(req.op, Op::Add);
        assert_eq!(req.rule.match_pattern.as_deref(), Some("a/**"));
        assert_eq!(req.expected_revision, "sha256:abc");
        assert!(routes::args_of(route, None, &[("rule".into(), json!(3))]).is_err());
    }
}
