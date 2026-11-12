// SPDX-License-Identifier: Apache-2.0
//! `ctx_agent` actions `claims` and `plan_claims` (unit claims, A-claims).
//!
//! `claims` lists who holds which unit of this repository. `plan_claims`
//! plans a task against them before any edit: it hands the claims snapshot
//! (`leanctx.claims.v1`) to an optional external planner
//! (`LEAN_CTX_CLAIM_PLANNER`, else `weave` on `PATH`) and claims the free
//! units the task named; without a planner it checks the named paths and
//! units itself. No network, no model call.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lean_ctx::core::unit_claims::{self, ClaimEntry, WHOLE_FILE};
use serde::Deserialize;

const PLANNER_TIMEOUT: Duration = Duration::from_secs(60);
const PLAN_SCHEMA: &str = "weave.claim-plan.v1";

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

/// `claims`: the active unit claims and path leases of this repository.
pub(super) fn list(project_root: &str) -> String {
    let snapshot = unit_claims::snapshot(project_root);
    if snapshot.claims.is_empty() {
        return "No active unit claims for this repository.".to_string();
    }
    let mut out = format!("Active unit claims ({}):\n", snapshot.claims.len());
    for c in &snapshot.claims {
        out.push_str(&format!(
            "  {}#{} — {}{}\n",
            c.path,
            c.unit,
            c.holder,
            remaining(c.expires_ms)
        ));
    }
    out.trim_end().to_string()
}

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
