// SPDX-License-Identifier: Apache-2.0
//! Savings and spend rollups as JSON for a local reader (no text tables):
//! `gain --daily --json` (`gain.daily`), `gain --by-project --json`
//! (`gain.projects`) and `spend --json --by-project` (`spend.projects`).
//! Savings are priced like the `gain --daily` table (the default cost
//! model); spend is the proxy's measured, priced usage.

use std::collections::BTreeMap;

use serde::Serialize;

use lean_ctx::core::stats::{CostModel, DayStats, StatsStore};

use super::Failure;
use super::session::{CostRow, cost_row, usd};

const DEFAULT_DAYS: usize = 30;
const MAX_DAYS: usize = 366;

#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub(crate) struct GainRow {
    pub commands: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tokens_saved: u64,
    pub saved_usd: f64,
}

impl GainRow {
    fn of(commands: u64, input: u64, output: u64, price_per_m: f64) -> Self {
        let saved = input.saturating_sub(output);
        Self {
            commands,
            input_tokens: input,
            output_tokens: output,
            tokens_saved: saved,
            saved_usd: usd(saved as f64 * price_per_m / 1_000_000.0),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct DayRow {
    pub date: String,
    #[serde(flatten)]
    pub gain: GainRow,
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct GainDaily {
    pub days: Vec<DayRow>,
    pub total: GainRow,
}

/// The newest `days` of `daily` (oldest first) and their sum.
pub(crate) fn daily_of(daily: &[DayStats], days: usize, price_per_m: f64) -> GainDaily {
    let mut sorted: Vec<&DayStats> = daily.iter().collect();
    sorted.sort_by(|a, b| a.date.cmp(&b.date));
    let skip = sorted.len().saturating_sub(days);
    let rows: Vec<DayRow> = sorted
        .into_iter()
        .skip(skip)
        .map(|d| DayRow {
            date: d.date.clone(),
            gain: GainRow::of(d.commands, d.input_tokens, d.output_tokens, price_per_m),
        })
        .collect();
    let (commands, input, output) = rows.iter().fold((0, 0, 0), |acc, r| {
        (
            acc.0 + r.gain.commands,
            acc.1 + r.gain.input_tokens,
            acc.2 + r.gain.output_tokens,
        )
    });
    GainDaily {
        total: GainRow::of(commands, input, output, price_per_m),
        days: rows,
    }
}

/// `gain --daily --json [--days N]`.
pub(crate) fn build_daily(args: &[String]) -> Result<GainDaily, Failure> {
    let days = match super::flag(args, "--days")? {
        None => DEFAULT_DAYS,
        Some(raw) => raw
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_DAYS).contains(n))
            .ok_or_else(|| Failure::invalid(format!("--days is 1-{MAX_DAYS}")))?,
    };
    let store = lean_ctx::core::stats::load_for_display();
    Ok(daily_of(
        &store.daily,
        days,
        CostModel::default().input_price_per_m,
    ))
}

/// Project key → folder for the projects this machine can still name
/// (the roots of its saved sessions).
fn known_roots() -> BTreeMap<String, String> {
    let mut roots = BTreeMap::new();
    for summary in lean_ctx::core::session::SessionState::list_sessions() {
        let Some(root) = summary.project_root else {
            continue;
        };
        if let Some(key) = lean_ctx::core::savings_ledger::project_scope::project_key_of(
            std::path::Path::new(&root),
        ) {
            roots.entry(key).or_insert(root);
        }
    }
    roots
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct ProjectGainRow {
    pub project_key: String,
    pub project_root: Option<String>,
    #[serde(flatten)]
    pub gain: GainRow,
    pub last_use: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct GainProjects {
    pub projects: Vec<ProjectGainRow>,
}

/// Each project store as a row, most saved first (then by key).
pub(crate) fn projects_of(
    stores: &[(String, StatsStore)],
    roots: &BTreeMap<String, String>,
    price_per_m: f64,
) -> GainProjects {
    let mut projects: Vec<ProjectGainRow> = stores
        .iter()
        .map(|(key, s)| ProjectGainRow {
            project_key: key.clone(),
            project_root: roots.get(key).cloned(),
            gain: GainRow::of(
                s.total_commands,
                s.total_input_tokens,
                s.total_output_tokens,
                price_per_m,
            ),
            last_use: s.last_use.clone(),
        })
        .collect();
    projects.sort_by(|a, b| {
        b.gain
            .tokens_saved
            .cmp(&a.gain.tokens_saved)
            .then_with(|| a.project_key.cmp(&b.project_key))
    });
    GainProjects { projects }
}

/// `gain --by-project --json`.
pub(crate) fn build_projects(_args: &[String]) -> Result<GainProjects, Failure> {
    Ok(projects_of(
        &lean_ctx::core::stats::project_stores(),
        &known_roots(),
        CostModel::default().input_price_per_m,
    ))
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct SpendProjectRow {
    pub project_key: String,
    pub project_root: Option<String>,
    pub total_usd: f64,
    pub requests: u64,
    pub per_model: Vec<CostRow>,
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct SpendProjects {
    pub available: bool,
    pub total_usd: f64,
    pub projects: Vec<SpendProjectRow>,
}

/// `spend --json --by-project`: the proxy's spend per project key, most
/// spent first (then by key).
pub(crate) fn build_spend(_args: &[String]) -> Result<SpendProjects, Failure> {
    lean_ctx::core::gain::live_pricing::ensure_loaded();
    let roots = known_roots();
    let mut projects: Vec<SpendProjectRow> = lean_ctx_proxy::usage_meter::persisted_projects()
        .into_iter()
        .map(|(key, rows)| SpendProjectRow {
            project_root: roots.get(&key).cloned(),
            total_usd: usd(rows.iter().map(|r| r.cost_usd).sum()),
            requests: rows.iter().map(|r| r.requests).sum(),
            per_model: rows.iter().map(cost_row).collect(),
            project_key: key,
        })
        .collect();
    projects.sort_by(|a, b| {
        b.total_usd
            .total_cmp(&a.total_usd)
            .then_with(|| a.project_key.cmp(&b.project_key))
    });
    Ok(SpendProjects {
        available: !projects.is_empty(),
        total_usd: usd(projects.iter().map(|p| p.total_usd).sum()),
        projects,
    })
}

/// Prints the rollup `command` (`gain`/`spend`) asks for when it is one
/// (`--json` with `--daily` / `--by-project`); `false` leaves the command
/// to its other views.
pub(crate) fn try_run(command: &str, args: &[String]) -> bool {
    if !super::wants_json(args) {
        return false;
    }
    let has = |flag: &str| args.iter().any(|a| a == flag);
    match command {
        "gain" if has("--daily") => super::kinds::run("gain.daily", args),
        "gain" if has("--by-project") => super::kinds::run("gain.projects", args),
        "spend" if has("--by-project") => super::kinds::run("spend.projects", args),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(date: &str, input: u64, output: u64) -> DayStats {
        DayStats {
            date: date.into(),
            commands: 1,
            input_tokens: input,
            output_tokens: output,
            ..Default::default()
        }
    }

    #[test]
    fn daily_keeps_the_newest_days_oldest_first_and_sums_them() {
        let daily = [
            day("2026-11-03", 2_000_000, 1_000_000),
            day("2026-11-01", 500, 900),
            day("2026-11-04", 1_000_000, 0),
        ];
        let doc = daily_of(&daily, 2, 3.0);
        let dates: Vec<&str> = doc.days.iter().map(|d| d.date.as_str()).collect();
        assert_eq!(dates, ["2026-11-03", "2026-11-04"]);
        assert_eq!(doc.days[0].gain.tokens_saved, 1_000_000);
        assert_eq!(doc.days[0].gain.saved_usd, 3.0);
        assert_eq!(doc.total.tokens_saved, 2_000_000);
        assert_eq!(doc.total.saved_usd, 6.0);
        assert_eq!(
            daily_of(&daily, 30, 3.0).days[0].gain.tokens_saved,
            0,
            "no negative saving"
        );
    }
}
