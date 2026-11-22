// SPDX-License-Identifier: Apache-2.0
//! `ctx_agent` unit claims: `claim`/`release` with `units`, `claims` and
//! `plan_claims`. One claims model, one store.
//!
//! Every claim is a unit claim of [`lean_ctx::core::unit_claims`]: a
//! machine-wide `Symbol` lease `project-unit:<repo>/<path>#<unit>` in the
//! agent lease store. The edit tools (`ctx_edit`, `ctx_patch`) take such
//! claims on the units they change (10 minutes, renewed by each edit) and
//! check other agents' claims before they write (`LEAN_CTX_CLAIM_MODE`
//! `off|warn|strict`). An agent can also claim units before it edits:
//!
//! - `claim units=[…]` claims project-relative files (`src/lib.rs`, or
//!   `src/lib.rs#*` as `ctx_graph action=landing` writes it) or symbols
//!   (`src/lib.rs#fn:parse`, format `path#kind:name`; nested symbols use `/`,
//!   `impl:Config/fn:new`) for `ttl_minutes` (default 30, at most 60, the
//!   lease store's maximum). The unit key is the `kind:name` text (`*` for a
//!   whole file); the edit gate matches it by its innermost symbol name, so
//!   `fn:parse` covers an edit of `parse`. Re-claiming renews. A unit another
//!   agent already holds is not claimed (one holder per unit, as for the edit
//!   tools); the reply names the holder.
//! - `release units=[…]` drops the caller's claims on those units; `[]`
//!   drops all of its claims in this repository, explicit and edit-made.
//! - `claims` lists every active claim of the repository (edit-made, explicit
//!   and path leases) and the overlaps between agents: `same unit` (equal,
//!   one contains the other — a whole file contains its symbols, `impl:A`
//!   contains `impl:A/fn:b`, `fn:parse` meets the edit-made `parse`) or
//!   `same file`.
//! - `plan_claims` plans a task against them before any edit: it hands the
//!   claims snapshot (`leanctx.claims.v1`) to an optional external planner
//!   (`LEAN_CTX_CLAIM_PLANNER`, else `weave` on `PATH`) and claims the free
//!   units the task named; without a planner it checks the named paths and
//!   units itself. No network, no model call.
//!
//! Claims are released on `status=finished` and on server shutdown. Replies
//! of `claim`, `release` and `claims` carry no clock values.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lean_ctx::core::unit_claims::{self, ClaimEntry, ExplicitClaim, WHOLE_FILE};
use serde::Deserialize;

const PLANNER_TIMEOUT: Duration = Duration::from_secs(60);
const PLAN_SCHEMA: &str = "weave.claim-plan.v1";

/// Default lifetime of an explicit claim.
pub(crate) const DEFAULT_TTL_MINUTES: u64 = 30;
/// Longest explicit claim: the lease store's maximum lease.
pub(crate) const MAX_TTL_MINUTES: u64 = unit_claims::MAX_EXPLICIT_TTL_MS / 60_000;
/// Units per claim or release call.
pub(crate) const MAX_UNITS_PER_CALL: usize = 64;
/// Longest accepted unit.
const MAX_UNIT_BYTES: usize = 512;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn remaining(expires_ms: Option<u64>) -> String {
    expires_ms.map_or_else(String::new, |at| {
        format!(" ({}s left)", at.saturating_sub(now_ms()) / 1_000)
    })
}

// ─── Units ───────────────────────────────────────────────────────────────────

/// A parsed, normalized unit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Unit {
    /// Project-relative file path with `/` separators.
    pub file: String,
    /// Unit inside the file (`kind:name`, or an edit-made symbol name);
    /// `None` is the whole file.
    pub symbol: Option<String>,
}

impl Unit {
    pub(crate) fn render(&self) -> String {
        match &self.symbol {
            Some(symbol) => format!("{}#{symbol}", self.file),
            None => self.file.clone(),
        }
    }

    /// The unit key of the claim store (`*` for the whole file).
    fn key(&self) -> &str {
        self.symbol.as_deref().unwrap_or(WHOLE_FILE)
    }

    fn from_entry(entry: &ClaimEntry) -> Self {
        Self {
            file: entry.path.clone(),
            symbol: (entry.unit != WHOLE_FILE).then(|| entry.unit.clone()),
        }
    }

    /// `self` covers `other`: the whole file (or a directory above it), the
    /// same symbol, an enclosing symbol (`impl:A` covers `impl:A/fn:b`), or —
    /// when one side is an edit-made bare name — the same symbol name
    /// (`fn:parse` and `parse`).
    fn contains(&self, other: &Unit) -> bool {
        let same_file = self.file == other.file;
        match (&self.symbol, &other.symbol) {
            (None, _) => {
                same_file
                    || other
                        .file
                        .strip_prefix(self.file.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            }
            (Some(_), None) => false,
            (Some(outer), Some(inner)) => {
                let outer_name = unit_claims::unit_symbol_name(outer);
                let inner_name = unit_claims::unit_symbol_name(inner);
                let bare = outer_name == outer.as_str() || inner_name == inner.as_str();
                same_file
                    && (inner == outer
                        || inner
                            .strip_prefix(outer.as_str())
                            .is_some_and(|rest| rest.starts_with('/'))
                        || (bare && outer_name == inner_name))
            }
        }
    }
}

/// Parse `path` or `path#kind:name` relative to `project_root`.
pub(crate) fn parse_unit(raw: &str, project_root: &str) -> Result<Unit, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("empty unit".to_string());
    }
    if raw.len() > MAX_UNIT_BYTES {
        return Err(format!("unit exceeds {MAX_UNIT_BYTES} bytes"));
    }
    if raw.chars().any(char::is_control) {
        return Err("unit contains control characters".to_string());
    }
    let (path, symbol) = match raw.split_once('#') {
        // `path#*` is the whole-file unit `ctx_graph action=landing` emits.
        Some((path, "*")) => (path, None),
        Some((path, symbol)) => {
            let Some((kind, name)) = symbol.split_once(':') else {
                return Err(format!("unit {raw}: symbol must be kind:name"));
            };
            let kind_ok = !kind.is_empty()
                && kind
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
            if !kind_ok || name.trim().is_empty() {
                return Err(format!(
                    "unit {raw}: kind must be lower-case letters, digits or _ and name non-empty"
                ));
            }
            (path, Some(format!("{kind}:{}", name.trim())))
        }
        None => (raw, None),
    };
    let file = relative_file(path, project_root)
        .ok_or_else(|| format!("unit {raw}: path must be a file inside the project"))?;
    Ok(Unit { file, symbol })
}

fn relative_file(path: &str, project_root: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    let candidate = Path::new(&path);
    let relative: PathBuf = if candidate.is_absolute() {
        candidate
            .strip_prefix(Path::new(project_root))
            .ok()?
            .to_path_buf()
    } else {
        candidate.to_path_buf()
    };
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_string()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn parse_units(raw: &[String], project_root: &str) -> Result<Vec<Unit>, String> {
    if raw.len() > MAX_UNITS_PER_CALL {
        return Err(format!("at most {MAX_UNITS_PER_CALL} units per call"));
    }
    let mut units = raw
        .iter()
        .map(|unit| parse_unit(unit, project_root))
        .collect::<Result<Vec<_>, _>>()?;
    units.sort();
    units.dedup();
    Ok(units)
}

// ─── Claims and overlaps ─────────────────────────────────────────────────────

/// One active claim of the repository.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ActiveClaim {
    pub unit: Unit,
    pub agent_id: String,
}

/// Every active claim of the repository at `project_root` (edit-made,
/// explicit, path leases), sorted by file, unit, agent.
pub(crate) fn active(project_root: &str) -> Vec<ActiveClaim> {
    let mut claims: Vec<ActiveClaim> = unit_claims::snapshot(project_root)
        .claims
        .iter()
        .map(|entry| ActiveClaim {
            unit: Unit::from_entry(entry),
            agent_id: entry.holder.clone(),
        })
        .collect();
    claims.sort();
    claims.dedup();
    claims
}

/// How another agent's claim meets one of ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OverlapKind {
    SameUnit,
    SameFile,
}

impl OverlapKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::SameUnit => "same unit",
            Self::SameFile => "same file",
        }
    }
}

/// Another agent's active claim meeting one of `agent`'s units.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Overlap {
    pub ours: Unit,
    pub own_agent: String,
    pub kind: OverlapKind,
    pub other_agent: String,
    pub theirs: Unit,
}

fn overlaps_for(units: &[Unit], agent: &str, others: &[ActiveClaim]) -> Vec<Overlap> {
    let mut found = Vec::new();
    for ours in units {
        for claim in others.iter().filter(|c| c.agent_id != agent) {
            let theirs = &claim.unit;
            let kind = if ours.contains(theirs) || theirs.contains(ours) {
                OverlapKind::SameUnit
            } else if ours.file == theirs.file {
                OverlapKind::SameFile
            } else {
                continue;
            };
            found.push(Overlap {
                ours: ours.clone(),
                own_agent: agent.to_string(),
                kind,
                other_agent: claim.agent_id.clone(),
                theirs: theirs.clone(),
            });
        }
    }
    found.sort();
    found.dedup();
    found
}

/// What an explicit `claim` did.
#[derive(Debug, Default)]
pub(crate) struct ClaimOutcome {
    /// Units now held by the caller.
    pub claimed: Vec<Unit>,
    /// Units another agent holds: `(unit, holder)`; not claimed.
    pub held: Vec<(Unit, String)>,
    /// Units that could not be claimed and why.
    pub failed: Vec<String>,
    /// Other agents' claims meeting the requested units.
    pub overlaps: Vec<Overlap>,
}

/// Claim (or renew) `units` for `agent` for `ttl_minutes` in the unit claim
/// store, then report the overlaps with other agents' active claims.
pub(crate) fn claim(
    project_root: &str,
    agent: &str,
    raw_units: &[String],
    ttl_minutes: u64,
) -> Result<ClaimOutcome, String> {
    let units = parse_units(raw_units, project_root)?;
    if units.is_empty() {
        return Err("units is required for claim (path or path#kind:name)".to_string());
    }
    let scope = unit_claims::repo_scope(project_root);
    let ttl_ms = ttl_minutes.clamp(1, MAX_TTL_MINUTES) * 60_000;
    let mut outcome = ClaimOutcome::default();
    for unit in &units {
        match unit_claims::claim_explicit(&scope, agent, &unit.file, unit.key(), ttl_ms) {
            ExplicitClaim::Held { .. } => outcome.claimed.push(unit.clone()),
            ExplicitClaim::HeldBy { holder, .. } => outcome.held.push((unit.clone(), holder)),
            ExplicitClaim::Failed(reason) => outcome.failed.push(reason),
        }
    }
    outcome.overlaps = overlaps_for(&units, agent, &active(project_root));
    Ok(outcome)
}

/// Drop `agent`'s claims on `units` (all of its claims in this repository
/// when `units` is empty); returns how many were released.
pub(crate) fn release(
    project_root: &str,
    agent: &str,
    raw_units: &[String],
) -> Result<usize, String> {
    let units: Vec<(String, String)> = parse_units(raw_units, project_root)?
        .into_iter()
        .map(|unit| {
            let key = unit.key().to_string();
            (unit.file, key)
        })
        .collect();
    let scope = unit_claims::repo_scope(project_root);
    Ok(unit_claims::release_units(&scope, agent, &units))
}

fn render_overlaps(out: &mut String, overlaps: &[Overlap]) {
    for overlap in overlaps {
        out.push_str(&format!(
            "\n  OVERLAP {}: {} ({}) ~ {} ({})",
            overlap.kind.as_str(),
            overlap.ours.render(),
            overlap.own_agent,
            overlap.theirs.render(),
            overlap.other_agent
        ));
    }
}

pub(crate) fn render_claim(outcome: &ClaimOutcome, agent: &str, ttl: u64) -> String {
    let mut out = format!(
        "Claimed {} unit(s) for {agent} (ttl {}m):",
        outcome.claimed.len(),
        ttl.clamp(1, MAX_TTL_MINUTES)
    );
    for unit in &outcome.claimed {
        out.push_str(&format!("\n  {}", unit.render()));
    }
    if !outcome.held.is_empty() {
        out.push_str(&format!(
            "\nNot claimed, held by another agent ({}):",
            outcome.held.len()
        ));
        for (unit, holder) in &outcome.held {
            out.push_str(&format!("\n  {} — {holder}", unit.render()));
        }
    }
    for reason in &outcome.failed {
        out.push_str(&format!("\nNot claimed: {reason}"));
    }
    if outcome.overlaps.is_empty() {
        out.push_str("\nNo overlap with other agents' claims.");
    } else {
        out.push_str(&format!(
            "\n{} overlap(s) with other agents' claims (coordinate before editing; \
             the edit tools warn, or refuse under LEAN_CTX_CLAIM_MODE=strict):",
            outcome.overlaps.len()
        ));
        render_overlaps(&mut out, &outcome.overlaps);
    }
    out
}

/// The `claims` listing: every active claim, then the overlaps (only
/// `agent`'s when given).
pub(crate) fn render_claims(claims: &[ActiveClaim], agent: Option<&str>) -> String {
    if claims.is_empty() {
        return "No active unit claims for this repository.".to_string();
    }
    let mut out = format!("Active unit claims ({}):", claims.len());
    for claim in claims {
        out.push_str(&format!("\n  {}  {}", claim.unit.render(), claim.agent_id));
    }
    // Overlaps between every pair of agents, reported once per pair of units.
    let mut overlaps: Vec<Overlap> = Vec::new();
    for claim in claims {
        if agent.is_some_and(|a| a != claim.agent_id) {
            continue;
        }
        for overlap in overlaps_for(std::slice::from_ref(&claim.unit), &claim.agent_id, claims) {
            let mirrored = overlaps.iter().any(|o| {
                o.ours == overlap.theirs
                    && o.theirs == overlap.ours
                    && o.other_agent == claim.agent_id
            });
            if agent.is_some() || !mirrored {
                overlaps.push(overlap);
            }
        }
    }
    overlaps.sort();
    overlaps.dedup();
    if overlaps.is_empty() {
        out.push_str("\nNo overlaps.");
    } else {
        out.push_str(&format!("\nOverlaps ({}):", overlaps.len()));
        render_overlaps(&mut out, &overlaps);
    }
    out
}

/// `claim` with units.
pub(crate) fn handle_claim(
    project_root: &str,
    agent: Option<&str>,
    units: &[String],
    ttl_minutes: Option<u64>,
) -> String {
    let Some(agent) = agent else {
        return "Error: agent must be registered first (use action=register)".to_string();
    };
    let ttl = ttl_minutes.unwrap_or(DEFAULT_TTL_MINUTES);
    match claim(project_root, agent, units, ttl) {
        Ok(outcome) => render_claim(&outcome, agent, ttl),
        Err(error) => format!("Error: {error}"),
    }
}

/// `release` with units (an empty list releases all of the agent's claims).
pub(crate) fn handle_release(project_root: &str, agent: Option<&str>, units: &[String]) -> String {
    let Some(agent) = agent else {
        return "Error: agent must be registered first (use action=register)".to_string();
    };
    match release(project_root, agent, units) {
        Ok(count) => format!("Released {count} unit claim(s) of {agent}."),
        Err(error) => format!("Error: {error}"),
    }
}

/// `claims`: active claims of the repository and their overlaps; with a
/// registered agent only that agent's overlaps.
pub(crate) fn handle_claims(project_root: &str, agent: Option<&str>) -> String {
    let listing = render_claims(&active(project_root), agent);
    let notice = lean_ctx::core::data_dir::lean_ctx_data_dir()
        .ok()
        .and_then(|dir| legacy_store_notice(&dir));
    match notice {
        Some(notice) => format!("{notice}\n{listing}"),
        None => listing,
    }
}

/// Store file of the advisory edit claims that preceded the unit claims
/// store, relative to the data directory. It is no longer read or written.
const LEGACY_EDIT_CLAIMS: &str = "agents/edit_claims.json";

/// One line naming the old edit claims store under `data_dir` when it still
/// exists, so its claims are not dropped silently.
fn legacy_store_notice(data_dir: &Path) -> Option<String> {
    let file = data_dir.join(LEGACY_EDIT_CLAIMS);
    file.exists().then(|| {
        format!(
            "Note: old edit claims from {} are no longer read; re-claim with \
             ctx_agent action=claim units=[...].",
            file.display()
        )
    })
}

// ─── plan_claims ─────────────────────────────────────────────────────────────

/// Bare name of a planner unit key: `impl:A/fn:b#2` → `b`, `*` stays.
fn bare(key: &str) -> &str {
    let seg = key.rsplit('/').next().unwrap_or(key);
    let seg = seg.split_once(':').map_or(seg, |(_, n)| n);
    match seg.rfind('#') {
        Some(i) if i + 1 < seg.len() && seg[i + 1..].bytes().all(|b| b.is_ascii_digit()) => {
            &seg[..i]
        }
        _ => seg,
    }
}

/// `path`, `path#unit` or `path::unit` items of a task text that look like
/// files (contain `/` or `.`).
fn named_units(task: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in task.split(|c: char| c.is_whitespace() || c == ',') {
        let item = raw.trim_matches(|c: char| matches!(c, '`' | '"' | '\'' | '(' | ')' | ';'));
        if item.is_empty() {
            continue;
        }
        let (path, unit) = match item.split_once('#') {
            Some((p, u)) => (p, u),
            None => match item.split_once("::") {
                Some((p, u)) if p.contains('.') => (p, u),
                _ => (item, WHOLE_FILE),
            },
        };
        let path = path.trim_start_matches("./");
        let file_like = path.contains('/')
            || path.rsplit_once('.').is_some_and(|(s, e)| {
                !s.is_empty() && !e.is_empty() && e.bytes().all(|b| b.is_ascii_alphanumeric())
            });
        if !file_like || path.starts_with('/') || path.split('/').any(|p| p == "..") {
            continue;
        }
        let unit = if unit.is_empty() { WHOLE_FILE } else { unit };
        let pair = (path.to_string(), unit.to_string());
        if !out.contains(&pair) {
            out.push(pair);
        }
    }
    out
}

fn overlaps(a: &str, b: &str) -> bool {
    a == WHOLE_FILE || b == WHOLE_FILE || a == b || bare(a) == bare(b)
}

/// The OSS check without a planner: each named unit against the claims.
fn local_plan(
    task: &str,
    claims: &[ClaimEntry],
    holder: &str,
) -> (Vec<(String, String)>, Vec<String>) {
    let mut free = Vec::new();
    let mut held = Vec::new();
    for (path, unit) in named_units(task) {
        let holders: Vec<&ClaimEntry> = claims
            .iter()
            .filter(|c| c.holder != holder && c.path == path && overlaps(&c.unit, &unit))
            .collect();
        if holders.is_empty() {
            free.push((path, unit));
        } else {
            for c in holders {
                held.push(format!(
                    "  {path}#{unit} — held by {} on {}{}: serialize (queue behind it) or pick another unit",
                    c.holder,
                    c.unit,
                    remaining(c.expires_ms)
                ));
            }
        }
    }
    (free, held)
}

#[derive(Debug, Deserialize)]
struct PlanUnit {
    path: String,
    unit: String,
    source: String,
}

#[derive(Debug, Deserialize)]
struct PlanHeld {
    path: String,
    unit: String,
    holder: String,
    action: String,
    suggestion: String,
}

#[derive(Debug, Deserialize)]
struct PlanHotspot {
    path: String,
    unit: String,
    tier: String,
    op: String,
}

#[derive(Debug, Deserialize)]
struct PlannerOutput {
    schema: String,
    free: Vec<PlanUnit>,
    held: Vec<PlanHeld>,
    #[serde(default)]
    hotspots: Vec<PlanHotspot>,
    verdict: String,
}

fn planner_command() -> Option<String> {
    match std::env::var("LEAN_CTX_CLAIM_PLANNER") {
        Ok(cmd) if cmd.trim().eq_ignore_ascii_case("off") => None,
        Ok(cmd) if !cmd.trim().is_empty() => Some(cmd.trim().to_string()),
        _ => Some("weave".to_string()),
    }
}

/// Runs the planner; `None` when it is absent, fails or times out.
fn run_planner(
    planner: &str,
    project_root: &str,
    task: &str,
    claims_file: &std::path::Path,
    holder: &str,
) -> Option<PlannerOutput> {
    let mut cmd = Command::new(planner);
    cmd.arg("plan-claims")
        .arg(project_root)
        .arg("--task")
        .arg(task)
        .arg("--claims")
        .arg(claims_file)
        .arg("--holder")
        .arg(holder)
        .arg("--json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Ok(hotspots) = std::env::var("LEAN_CTX_CLAIM_HOTSPOTS")
        && !hotspots.trim().is_empty()
    {
        cmd.arg("--hotspots").arg(hotspots.trim());
    }
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut buf);
        buf
    });
    let deadline = Instant::now() + PLANNER_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let out = reader.join().ok()?;
    if !status.success() {
        return None;
    }
    let plan: PlannerOutput = serde_json::from_slice(&out).ok()?;
    (plan.schema == PLAN_SCHEMA).then_some(plan)
}

fn claim_units(project_root: &str, holder: &str, units: &[(String, String)]) -> Vec<String> {
    let scope = unit_claims::repo_scope(project_root);
    let mut claimed = Vec::new();
    for (path, unit) in units {
        let names = BTreeSet::from([bare(unit).to_string()]);
        for name in unit_claims::claim(&scope, holder, path, &names) {
            claimed.push(format!("{path}#{name}"));
        }
    }
    claimed
}

/// `plan_claims`: plan the task `message` against the active claims.
pub(super) fn plan(project_root: &str, holder: &str, task: Option<&str>) -> String {
    let Some(task) = task.map(str::trim).filter(|t| !t.is_empty()) else {
        return "Error: message is required for plan_claims (the task text or a file list \
                like `src/a.rs#parse src/b.rs`)"
            .to_string();
    };
    let snapshot = unit_claims::snapshot(project_root);
    let planned = planner_command().and_then(|planner| {
        let mut file = tempfile::Builder::new()
            .prefix("leanctx-claims-")
            .suffix(".json")
            .tempfile()
            .ok()?;
        file.write_all(serde_json::to_string(&snapshot).ok()?.as_bytes())
            .ok()?;
        file.flush().ok()?;
        run_planner(&planner, project_root, task, file.path(), holder)
    });

    let mut out = String::new();
    let explicit: Vec<(String, String)> = if let Some(plan) = &planned {
        out.push_str(&format!("Claim plan (planner): verdict {}\n", plan.verdict));
        out.push_str(&format!("Free ({}):\n", plan.free.len()));
        for u in &plan.free {
            out.push_str(&format!("  {}#{} [{}]\n", u.path, u.unit, u.source));
        }
        if !plan.held.is_empty() {
            out.push_str(&format!("Held ({}):\n", plan.held.len()));
            for h in &plan.held {
                out.push_str(&format!(
                    "  {}#{} — {} ({}): {}\n",
                    h.path, h.unit, h.holder, h.action, h.suggestion
                ));
            }
        }
        if !plan.hotspots.is_empty() {
            out.push_str(&format!("Hotspots ({}):\n", plan.hotspots.len()));
            for h in &plan.hotspots {
                out.push_str(&format!("  {}#{} [{}]: {}\n", h.path, h.unit, h.tier, h.op));
            }
        }
        plan.free
            .iter()
            .filter(|u| u.source == "explicit")
            .map(|u| (u.path.clone(), u.unit.clone()))
            .collect()
    } else {
        let (free, held) = local_plan(task, &snapshot.claims, holder);
        if free.is_empty() && held.is_empty() {
            return "No file paths named in the task and no planner available \
                        (set LEAN_CTX_CLAIM_PLANNER or put `weave` on PATH); name \
                        units as `path#unit` or `path`."
                .to_string();
        }
        let verdict = if held.is_empty() {
            "claim"
        } else {
            "serialize"
        };
        out.push_str(&format!("Claim plan (local check): verdict {verdict}\n"));
        out.push_str(&format!("Free ({}):\n", free.len()));
        for (path, unit) in &free {
            out.push_str(&format!("  {path}#{unit}\n"));
        }
        if !held.is_empty() {
            out.push_str(&format!("Held ({}):\n", held.len()));
            for line in &held {
                out.push_str(line);
                out.push('\n');
            }
        }
        free
    };
    let claimed = claim_units(project_root, holder, &explicit);
    if claimed.is_empty() {
        out.push_str("Claimed: none");
    } else {
        out.push_str(&format!(
            "Claimed for {holder} ({} min): {}",
            unit_claims::CLAIM_TTL_MS / 60_000,
            claimed.join(", ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/work/app";

    fn units(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|s| (*s).to_string()).collect()
    }

    /// A fresh repository root (own claim scope) and agent ids unique to
    /// this test process: the lease store is shared by parallel tests.
    fn fresh() -> (tempfile::TempDir, String, impl Fn(&str) -> String) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let pid = std::process::id();
        (dir, root, move |name: &str| format!("{name}-{pid}"))
    }

    #[test]
    fn edit_claims_parse_units() {
        let unit = parse_unit("./src/lib.rs#fn:parse", ROOT).unwrap();
        assert_eq!(unit.file, "src/lib.rs");
        assert_eq!(unit.symbol.as_deref(), Some("fn:parse"));
        // An absolute path inside the root is made root-relative. The root is
        // a real absolute directory: `/work/app` has no drive on Windows.
        let (dir, root, _) = fresh();
        assert_eq!(
            parse_unit(&format!("{root}/src/a.rs"), &root)
                .unwrap()
                .render(),
            "src/a.rs"
        );
        let elsewhere = dir.path().parent().unwrap().join("elsewhere/a.rs");
        assert!(parse_unit(&elsewhere.to_string_lossy(), &root).is_err());
        assert!(parse_unit("../other/a.rs", ROOT).is_err());
        assert!(parse_unit("/elsewhere/a.rs", ROOT).is_err());
        assert!(parse_unit("src/a.rs#parse", ROOT).is_err());
        assert!(parse_unit("src/a.rs#Fn:parse", ROOT).is_err());
        assert!(parse_unit("", ROOT).is_err());
        // `path#*` (ctx_graph action=landing) is the whole-file unit.
        assert_eq!(
            parse_unit("src/a.rs#*", ROOT).unwrap(),
            parse_unit("src/a.rs", ROOT).unwrap()
        );
        assert_eq!(parse_unit("src/a.rs#*", ROOT).unwrap().key(), WHOLE_FILE);
    }

    #[test]
    fn edit_claims_nested_units_are_the_same_unit() {
        let (_dir, root, id) = fresh();
        let (a, b) = (id("nested-a"), id("nested-b"));
        claim(&root, &a, &units(&["src/lib.rs#impl:A"]), 30).unwrap();
        let outcome = claim(
            &root,
            &b,
            &units(&[
                "src/lib.rs#impl:A/fn:b",
                "src/lib.rs#impl:AB",
                "src/lib.rs#*",
            ]),
            30,
        )
        .unwrap();
        assert_eq!(outcome.claimed.len(), 3);
        let kinds: Vec<(String, OverlapKind)> = outcome
            .overlaps
            .iter()
            .map(|o| (o.ours.render(), o.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("src/lib.rs".to_string(), OverlapKind::SameUnit),
                ("src/lib.rs#impl:A/fn:b".to_string(), OverlapKind::SameUnit),
                ("src/lib.rs#impl:AB".to_string(), OverlapKind::SameFile),
            ]
        );
        release(&root, &a, &[]).unwrap();
        release(&root, &b, &[]).unwrap();
    }

    #[test]
    fn explicit_and_edit_made_claims_meet_by_symbol_name() {
        let explicit = parse_unit("src/lib.rs#fn:parse", ROOT).unwrap();
        let edit_made = Unit {
            file: "src/lib.rs".into(),
            symbol: Some("parse".into()),
        };
        assert!(explicit.contains(&edit_made) && edit_made.contains(&explicit));
        let other = parse_unit("src/lib.rs#struct:parse", ROOT).unwrap();
        assert!(!explicit.contains(&other), "two kinded units stay distinct");
        let dir_lease = Unit {
            file: "src".into(),
            symbol: None,
        };
        assert!(dir_lease.contains(&explicit));
    }

    /// One store for both surfaces: an explicit `units` claim of agent A
    /// warns (strict: refuses) agent B's edit through the pre-edit gate, an
    /// edit-made claim of B blocks A's explicit claim of the same symbol, and
    /// `claims` lists the edit-made and explicit claims once each.
    #[test]
    fn explicit_units_and_edit_made_claims_share_one_store() {
        use lean_ctx::core::unit_claims::{ClaimMode, Gate};
        const SRC: &str = "fn parse() {\n    let a = 1;\n}\n\nfn emit() {\n    let b = 2;\n}\n";
        let (dir, root, id) = fresh();
        let file = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(&file, SRC).unwrap();
        let (a, b) = (id("cross-a"), id("cross-b"));
        let edit_parse = SRC.replace("let a = 1;", "let a = 7;");
        let edit_emit = SRC.replace("let b = 2;", "let b = 9;");

        let outcome = claim(&root, &a, &units(&["src/lib.rs#fn:parse"]), 30).unwrap();
        assert_eq!(outcome.claimed.len(), 1);
        assert!(matches!(
            unit_claims::gate_with(ClaimMode::Strict, &root, &b, &file, SRC, &edit_parse),
            Gate::Refuse(_)
        ));
        // Warn mode writes anyway and claims the edited `parse` for B.
        match unit_claims::gate_with(ClaimMode::Warn, &root, &b, &file, SRC, &edit_parse) {
            Gate::Warn(text) => assert!(text.contains(&a), "{text}"),
            other => panic!("expected a warning, got {other:?}"),
        }
        // B edits `emit` (no claim on it): the edit tools claim it for B.
        assert_eq!(
            unit_claims::gate_with(ClaimMode::Strict, &root, &b, &file, SRC, &edit_emit),
            Gate::Clear
        );
        let scope = unit_claims::repo_scope(&root);
        let names = BTreeSet::from(["emit".to_string()]);
        assert_eq!(
            unit_claims::claim(&scope, &b, "src/lib.rs", &names),
            vec!["emit".to_string()]
        );
        // A cannot take `emit` explicitly while B's edit claim holds it.
        let blocked = claim(&root, &a, &units(&["src/lib.rs#fn:emit"]), 30).unwrap();
        assert!(blocked.claimed.is_empty());
        assert_eq!(blocked.held.len(), 1);

        let listed = active(&root);
        let rendered: Vec<(String, String)> = listed
            .iter()
            .map(|c| (c.unit.render(), c.agent_id.clone()))
            .collect();
        assert_eq!(
            rendered,
            vec![
                ("src/lib.rs#emit".to_string(), b.clone()),
                ("src/lib.rs#fn:parse".to_string(), a.clone()),
                ("src/lib.rs#parse".to_string(), b.clone()),
            ]
        );
        let text = render_claims(&listed, None);
        assert!(text.starts_with("Active unit claims (3):"), "{text}");
        assert!(
            text.contains(&format!(
                "OVERLAP same unit: src/lib.rs#fn:parse ({a}) ~ src/lib.rs#parse ({b})"
            )),
            "{text}"
        );
        assert!(text.contains("Overlaps (2):"), "{text}");
        assert_eq!(release(&root, &a, &[]).unwrap(), 1);
        assert_eq!(unit_claims::release_agent(&b), 2);
    }

    #[test]
    fn edit_claims_report_same_unit_and_same_file_overlaps() {
        let (_dir, root, id) = fresh();
        let (_other_dir, other_root, _) = fresh();
        let (a, b, c) = (id("overlap-a"), id("overlap-b"), id("overlap-c"));
        let first = claim(&root, &a, &units(&["src/lib.rs#fn:parse"]), 30).unwrap();
        assert!(first.overlaps.is_empty());
        let outcome = claim(
            &root,
            &b,
            &units(&[
                "src/lib.rs#fn:parse",
                "src/lib.rs#struct:Config",
                "src/main.rs",
            ]),
            30,
        )
        .unwrap();
        // One holder per unit: `a` keeps fn:parse, `b` is told so.
        assert_eq!(outcome.claimed.len(), 2);
        assert_eq!(
            outcome
                .held
                .iter()
                .map(|(u, h)| (u.render(), h.clone()))
                .collect::<Vec<_>>(),
            vec![("src/lib.rs#fn:parse".to_string(), a.clone())]
        );
        let rendered: Vec<(OverlapKind, String, String)> = outcome
            .overlaps
            .iter()
            .map(|o| (o.kind, o.ours.render(), o.theirs.render()))
            .collect();
        assert_eq!(
            rendered,
            vec![
                (
                    OverlapKind::SameUnit,
                    "src/lib.rs#fn:parse".into(),
                    "src/lib.rs#fn:parse".into()
                ),
                (
                    OverlapKind::SameFile,
                    "src/lib.rs#struct:Config".into(),
                    "src/lib.rs#fn:parse".into()
                ),
            ]
        );
        let text = render_claim(&outcome, &b, 30);
        assert!(text.contains("held by another agent (1)"), "{text}");
        // Another repository never sees these claims.
        let foreign = claim(&other_root, &c, &units(&["src/lib.rs"]), 30).unwrap();
        assert!(foreign.overlaps.is_empty());
        // The listing is deterministic and reports each overlap once.
        let claims = active(&root);
        assert_eq!(claims.len(), 3);
        let text = render_claims(&claims, None);
        assert_eq!(text, render_claims(&active(&root), None));
        assert!(
            text.contains(&format!(
                "OVERLAP same file: src/lib.rs#fn:parse ({a}) ~ src/lib.rs#struct:Config ({b})"
            )),
            "{text}"
        );
        assert!(text.contains("Overlaps (1):"), "{text}");
        assert!(!text.contains(&c));
        release(&root, &a, &[]).unwrap();
        release(&root, &b, &[]).unwrap();
        release(&other_root, &c, &[]).unwrap();
    }

    #[test]
    fn edit_claims_ttl_is_capped_and_renewed_on_reclaim() {
        let (_dir, root, id) = fresh();
        let a = id("ttl-a");
        let expiry = || {
            let snapshot = unit_claims::snapshot(&root);
            assert_eq!(snapshot.claims.len(), 1, "re-claiming never duplicates");
            snapshot.claims[0].expires_ms.unwrap()
        };
        claim(&root, &a, &units(&["src/a.rs"]), 1).unwrap();
        let short = expiry();
        assert!(short <= now_ms() + 60_000);
        claim(&root, &a, &units(&["src/a.rs"]), 30).unwrap();
        let renewed = expiry();
        assert!(renewed >= now_ms() + 29 * 60_000, "re-claim renews");
        claim(&root, &a, &units(&["src/a.rs"]), 24 * 60).unwrap();
        assert!(expiry() <= now_ms() + MAX_TTL_MINUTES * 60_000);
        assert_eq!(release(&root, &a, &[]).unwrap(), 1);
    }

    #[test]
    fn edit_claims_release_named_units_or_all() {
        let (_dir, root, id) = fresh();
        let (a, b) = (id("release-a"), id("release-b"));
        claim(&root, &a, &units(&["src/a.rs", "src/b.rs#fn:x"]), 30).unwrap();
        claim(&root, &b, &units(&["src/c.rs"]), 30).unwrap();
        assert_eq!(release(&root, &a, &units(&["src/b.rs#fn:x"])).unwrap(), 1);
        assert_eq!(release(&root, &a, &[]).unwrap(), 1);
        let left = active(&root);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].agent_id, b);
        assert_eq!(release(&root, &b, &[]).unwrap(), 1);
    }

    #[test]
    fn legacy_edit_claims_store_is_named_not_dropped_silently() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(legacy_store_notice(dir.path()), None);
        let file = dir.path().join(LEGACY_EDIT_CLAIMS);
        std::fs::create_dir_all(file.parent().expect("agents dir")).expect("mkdir");
        std::fs::write(&file, "{\"version\":1,\"claims\":[]}").expect("write");
        let notice = legacy_store_notice(dir.path()).expect("notice for an existing store");
        assert!(
            notice.contains(&format!(
                "old edit claims from {} are no longer read",
                file.display()
            )),
            "{notice}"
        );
    }

    #[test]
    fn named_units_reads_paths_and_units() {
        let got = named_units("fix `src/a.rs#parse`, docs/x.md and src/b.rs::run in the parser");
        assert_eq!(
            got,
            vec![
                ("src/a.rs".to_string(), "parse".to_string()),
                ("docs/x.md".to_string(), "*".to_string()),
                ("src/b.rs".to_string(), "run".to_string()),
            ]
        );
        assert!(named_units("../etc/passwd /abs/x.rs").is_empty());
    }

    #[test]
    fn bare_strips_kinds_and_ordinals() {
        assert_eq!(bare("impl:A/fn:b#2"), "b");
        assert_eq!(bare("md:Title"), "Title");
        assert_eq!(bare("*"), "*");
    }

    #[test]
    fn local_plan_splits_free_and_held() {
        let claims = vec![
            ClaimEntry {
                holder: "other".into(),
                path: "src/a.rs".into(),
                unit: "parse".into(),
                expires_ms: None,
            },
            ClaimEntry {
                holder: "me".into(),
                path: "src/b.rs".into(),
                unit: "*".into(),
                expires_ms: None,
            },
        ];
        let (free, held) = local_plan("src/a.rs#fn:parse src/a.rs#emit src/b.rs", &claims, "me");
        assert_eq!(
            free,
            vec![
                ("src/a.rs".to_string(), "emit".to_string()),
                ("src/b.rs".to_string(), "*".to_string()),
            ]
        );
        assert_eq!(held.len(), 1);
        assert!(held[0].contains("held by other"), "{}", held[0]);
    }
}
