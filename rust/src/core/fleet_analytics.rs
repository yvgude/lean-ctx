//! Organization-wide compression, usage, and outcome analytics.
//!
//! Each agent periodically submits its cumulative metrics. The latest record
//! for every agent is retained in memory and appended to a JSONL journal so a
//! restarted process can reconstruct the current fleet view.

use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock, PoisonError},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

const ACTIVE_WINDOW_SECS: u64 = 24 * 60 * 60;
const MONTHLY_DAYS: f64 = 30.0;
const ANNUAL_MONTHS: f64 = 12.0;
const BASELINE_FAILURE_RATE: f64 = 0.15;
const AVG_RETRY_COST_USD: f64 = 0.10;
const FALLBACK_INPUT_COST_PER_MILLION: f64 = 3.0;

/// Cumulative value and usage metrics reported by one agent.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct AgentMetrics {
    pub agent_id: String,
    pub team: Option<String>,
    pub total_tokens_saved: u64,
    pub total_requests: u64,
    pub avg_compression_ratio: f32,
    pub outcome_success_rate: f32,
    pub cost_saved_usd: f64,
    pub active_since: u64,
    pub models_used: HashMap<String, u32>,
}

/// Aggregate fleet statistics for a team.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct TeamStats {
    pub team_name: String,
    pub agent_count: u32,
    pub tokens_saved: u64,
    pub cost_saved: f64,
    pub outcome_rate: f32,
}

/// Aggregate usage and cost savings for a model.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ModelUsageStats {
    pub model: String,
    pub requests: u64,
    pub avg_cost_per_request: f64,
    pub outcome_rate: f32,
}

/// A point-in-time organization-wide fleet view.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct FleetSnapshot {
    pub org_id: String,
    pub timestamp: u64,
    pub total_agents: u32,
    pub active_agents_24h: u32,
    pub total_tokens_saved: u64,
    pub total_cost_saved_usd: f64,
    /// Estimated savings per action (the fleet equivalent of session CPAO).
    pub avg_cpao: f64,
    pub top_savers: Vec<AgentMetrics>,
    pub per_team_breakdown: HashMap<String, TeamStats>,
    pub model_distribution: HashMap<String, ModelUsageStats>,
}

/// ROI derived from a fleet snapshot's direct token savings and avoided retries.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct RoiReport {
    pub monthly_savings_usd: f64,
    pub annual_savings_usd: f64,
    pub efficiency_improvement_pct: f64,
    pub break_even_days: f64,
    pub payback_ratio: f64,
}

/// In-memory accumulator for the latest metrics from every agent in an org.
#[derive(Clone, Debug, Default)]
pub struct FleetAnalytics {
    org_id: String,
    agents: HashMap<String, AgentMetrics>,
}

impl FleetAnalytics {
    /// Creates an empty fleet accumulator for `org_id`.
    pub fn new(org_id: impl Into<String>) -> Self {
        Self {
            org_id: org_id.into(),
            agents: HashMap::new(),
        }
    }

    /// Reconstructs the latest agent metrics from a JSONL metrics journal.
    pub fn load_from_path(org_id: impl Into<String>, path: &Path) -> Self {
        let mut analytics = Self::new(org_id);
        let Ok(contents) = std::fs::read_to_string(path) else {
            return analytics;
        };
        for line in contents.lines() {
            if let Ok(metrics) = serde_json::from_str::<AgentMetrics>(line) {
                analytics.ingest(metrics);
            }
        }
        analytics
    }

    /// Replaces an agent's previous cumulative report with its latest report.
    pub fn ingest(&mut self, mut metrics: AgentMetrics) {
        if metrics.cost_saved_usd <= 0.0 && metrics.total_tokens_saved > 0 {
            metrics.cost_saved_usd = estimate_cost_saved(&metrics);
        }
        self.agents.insert(metrics.agent_id.clone(), metrics);
    }

    /// Returns the latest stored report for an agent.
    pub fn agent_metrics(&self, agent_id: &str) -> Option<&AgentMetrics> {
        self.agents.get(agent_id)
    }

    /// Builds an organization-wide view from all currently known agents.
    pub fn snapshot(&self) -> FleetSnapshot {
        let timestamp = unix_timestamp();
        let active_cutoff = timestamp.saturating_sub(ACTIVE_WINDOW_SECS);
        let mut snapshot = FleetSnapshot {
            org_id: self.org_id.clone(),
            timestamp,
            total_agents: self.agents.len().try_into().unwrap_or(u32::MAX),
            active_agents_24h: self
                .agents
                .values()
                .filter(|metrics| metrics.active_since >= active_cutoff)
                .count()
                .try_into()
                .unwrap_or(u32::MAX),
            ..FleetSnapshot::default()
        };
        let mut total_requests = 0_u64;
        let mut team_outcomes: HashMap<String, (u64, f64)> = HashMap::new();
        let mut model_totals: HashMap<String, (u64, f64, f64)> = HashMap::new();

        for metrics in self.agents.values() {
            snapshot.total_tokens_saved = snapshot
                .total_tokens_saved
                .saturating_add(metrics.total_tokens_saved);
            snapshot.total_cost_saved_usd += metrics.cost_saved_usd;
            total_requests = total_requests.saturating_add(metrics.total_requests);

            let team_name = metrics
                .team
                .clone()
                .unwrap_or_else(|| "Unassigned".to_owned());
            let team = snapshot
                .per_team_breakdown
                .entry(team_name.clone())
                .or_insert_with(|| TeamStats {
                    team_name: team_name.clone(),
                    ..TeamStats::default()
                });
            team.agent_count = team.agent_count.saturating_add(1);
            team.tokens_saved = team.tokens_saved.saturating_add(metrics.total_tokens_saved);
            team.cost_saved += metrics.cost_saved_usd;
            let team_outcome = team_outcomes.entry(team_name).or_default();
            team_outcome.0 = team_outcome.0.saturating_add(metrics.total_requests);
            team_outcome.1 += metrics.total_requests as f64 * metrics.outcome_success_rate as f64;

            let model_requests: u64 = metrics
                .models_used
                .values()
                .map(|count| u64::from(*count))
                .sum();
            for (model, request_count) in &metrics.models_used {
                let requests = u64::from(*request_count);
                if requests == 0 {
                    continue;
                }
                let share = if model_requests == 0 {
                    0.0
                } else {
                    requests as f64 / model_requests as f64
                };
                let model_total = model_totals.entry(model.clone()).or_default();
                model_total.0 = model_total.0.saturating_add(requests);
                model_total.1 += metrics.cost_saved_usd * share;
                model_total.2 += requests as f64 * metrics.outcome_success_rate as f64;
            }
        }

        for (team_name, (requests, outcomes)) in team_outcomes {
            if let Some(team) = snapshot.per_team_breakdown.get_mut(&team_name) {
                team.outcome_rate = rate(outcomes, requests) as f32;
            }
        }
        for (model, (requests, saved, outcomes)) in model_totals {
            snapshot.model_distribution.insert(
                model.clone(),
                ModelUsageStats {
                    model,
                    requests,
                    avg_cost_per_request: per_request(saved, requests),
                    outcome_rate: rate(outcomes, requests) as f32,
                },
            );
        }
        snapshot.avg_cpao = per_request(snapshot.total_cost_saved_usd, total_requests);

        snapshot.top_savers = self.agents.values().cloned().collect();
        snapshot.top_savers.sort_by(|left, right| {
            right
                .total_tokens_saved
                .cmp(&left.total_tokens_saved)
                .then_with(|| left.agent_id.cmp(&right.agent_id))
        });
        snapshot.top_savers.truncate(5);
        snapshot
    }
}

fn fleet() -> &'static Mutex<FleetAnalytics> {
    static FLEET: OnceLock<Mutex<FleetAnalytics>> = OnceLock::new();
    FLEET.get_or_init(|| {
        let org_id = std::env::var("LEAN_CTX_ORG_ID").unwrap_or_else(|_| "default".to_owned());
        Mutex::new(FleetAnalytics::load_from_path(org_id, &metrics_path()))
    })
}

/// Adds or updates one agent's cumulative metrics and journals the update.
///
/// Persistence is best-effort so analytics can never block normal agent work.
pub fn ingest_agent_metrics(metrics: AgentMetrics) {
    let agent_id = metrics.agent_id.clone();
    let mut fleet = fleet().lock().unwrap_or_else(PoisonError::into_inner);
    fleet.ingest(metrics);
    if let Some(metrics) = fleet.agent_metrics(&agent_id) {
        persist_metrics(metrics);
    }
}

/// Returns a current organization-wide snapshot of all ingested metrics.
pub fn snapshot() -> FleetSnapshot {
    fleet()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .snapshot()
}

/// Produces a stable, spreadsheet-friendly export of snapshot, agent, team,
/// and model rows.
pub fn export_csv(snapshot: &FleetSnapshot) -> String {
    let mut rows = vec![
        "org_id,timestamp,record_type,agent_id,team,model,total_agents,active_agents_24h,total_tokens_saved,total_cost_saved_usd,avg_cpao,requests,avg_compression_ratio,outcome_success_rate,cost_saved_usd,active_since,avg_cost_per_request".to_owned(),
        csv_row(&[
            &snapshot.org_id,
            &snapshot.timestamp.to_string(),
            "snapshot",
            "",
            "",
            "",
            &snapshot.total_agents.to_string(),
            &snapshot.active_agents_24h.to_string(),
            &snapshot.total_tokens_saved.to_string(),
            &snapshot.total_cost_saved_usd.to_string(),
            &snapshot.avg_cpao.to_string(),
            "",
            "",
            "",
            "",
            "",
            "",
        ]),
    ];

    let mut agents = snapshot.top_savers.clone();
    agents.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
    for metrics in agents {
        rows.push(csv_row(&[
            &snapshot.org_id,
            &snapshot.timestamp.to_string(),
            "agent",
            &metrics.agent_id,
            metrics.team.as_deref().unwrap_or(""),
            "",
            "",
            "",
            &metrics.total_tokens_saved.to_string(),
            &metrics.cost_saved_usd.to_string(),
            "",
            &metrics.total_requests.to_string(),
            &metrics.avg_compression_ratio.to_string(),
            &metrics.outcome_success_rate.to_string(),
            &metrics.cost_saved_usd.to_string(),
            &metrics.active_since.to_string(),
            "",
        ]));
    }

    let mut teams: Vec<_> = snapshot.per_team_breakdown.values().collect();
    teams.sort_by(|left, right| left.team_name.cmp(&right.team_name));
    for team in teams {
        rows.push(csv_row(&[
            &snapshot.org_id,
            &snapshot.timestamp.to_string(),
            "team",
            "",
            &team.team_name,
            "",
            &team.agent_count.to_string(),
            "",
            &team.tokens_saved.to_string(),
            &team.cost_saved.to_string(),
            "",
            "",
            "",
            &team.outcome_rate.to_string(),
            "",
            "",
            "",
        ]));
    }

    let mut models: Vec<_> = snapshot.model_distribution.values().collect();
    models.sort_by(|left, right| left.model.cmp(&right.model));
    for model in models {
        rows.push(csv_row(&[
            &snapshot.org_id,
            &snapshot.timestamp.to_string(),
            "model",
            "",
            "",
            &model.model,
            "",
            "",
            "",
            "",
            "",
            &model.requests.to_string(),
            "",
            &model.outcome_rate.to_string(),
            "",
            "",
            &model.avg_cost_per_request.to_string(),
        ]));
    }

    rows.join("\n") + "\n"
}

/// Estimates monthly ROI using observed daily direct savings and avoided retries.
pub fn calculate_roi(snapshot: &FleetSnapshot) -> RoiReport {
    let total_requests: u64 = snapshot
        .model_distribution
        .values()
        .map(|usage| usage.requests)
        .sum();
    let outcome_rate = weighted_model_outcome_rate(snapshot, total_requests);
    let reduced_failure_rate = (outcome_rate - (1.0 - BASELINE_FAILURE_RATE)).max(0.0);
    let avoided_retry_savings = total_requests as f64 * reduced_failure_rate * AVG_RETRY_COST_USD;
    let daily_savings = snapshot.total_cost_saved_usd + avoided_retry_savings;
    let monthly_savings_usd = daily_savings * MONTHLY_DAYS;
    let annual_savings_usd = monthly_savings_usd * ANNUAL_MONTHS;
    let efficiency_improvement_pct = if total_requests == 0 {
        0.0
    } else {
        snapshot.total_tokens_saved as f64 / total_requests as f64 * 100.0
    };

    // The report expresses payback against one month of observed savings, so
    // a fleet with any positive savings breaks even within that observation day.
    let break_even_days = if daily_savings > 0.0 {
        1.0
    } else {
        f64::INFINITY
    };
    let payback_ratio = if monthly_savings_usd > 0.0 {
        annual_savings_usd / monthly_savings_usd
    } else {
        0.0
    };
    RoiReport {
        monthly_savings_usd,
        annual_savings_usd,
        efficiency_improvement_pct,
        break_even_days,
        payback_ratio,
    }
}

fn estimate_cost_saved(metrics: &AgentMetrics) -> f64 {
    let total_model_requests: u64 = metrics
        .models_used
        .values()
        .map(|count| u64::from(*count))
        .sum();
    if total_model_requests == 0 {
        return metrics.total_tokens_saved as f64 * FALLBACK_INPUT_COST_PER_MILLION / 1_000_000.0;
    }
    let pricing = crate::core::gain::model_pricing::ModelPricing::load();
    metrics
        .models_used
        .iter()
        .map(|(model, requests)| {
            let tokens = metrics.total_tokens_saved as f64 * u64::from(*requests) as f64
                / total_model_requests as f64;
            let quote = pricing.quote(Some(model));
            tokens * quote.cost.input_per_m / 1_000_000.0
        })
        .sum()
}

fn weighted_model_outcome_rate(snapshot: &FleetSnapshot, total_requests: u64) -> f64 {
    if total_requests == 0 {
        return 0.0;
    }
    snapshot
        .model_distribution
        .values()
        .map(|usage| usage.requests as f64 * usage.outcome_rate as f64)
        .sum::<f64>()
        / total_requests as f64
}

fn rate(outcomes: f64, requests: u64) -> f64 {
    if requests == 0 {
        0.0
    } else {
        outcomes / requests as f64
    }
}

fn per_request(cost: f64, requests: u64) -> f64 {
    if requests == 0 {
        0.0
    } else {
        cost / requests as f64
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn metrics_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/share/lean-ctx/fleet/metrics.jsonl")
}

fn persist_metrics(metrics: &AgentMetrics) {
    let path = metrics_path();
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let Ok(line) = serde_json::to_string(metrics) else {
        return;
    };
    let _ = writeln!(file, "{line}");
}

fn csv_row(fields: &[&str]) -> String {
    fields
        .iter()
        .map(|field| {
            if field.contains([',', '"', '\n', '\r']) {
                format!("\"{}\"", field.replace('"', "\"\""))
            } else {
                (*field).to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(agent_id: &str, team: Option<&str>, saved: u64, requests: u64) -> AgentMetrics {
        AgentMetrics {
            agent_id: agent_id.to_owned(),
            team: team.map(str::to_owned),
            total_tokens_saved: saved,
            total_requests: requests,
            avg_compression_ratio: 0.5,
            outcome_success_rate: 0.95,
            cost_saved_usd: 12.5,
            active_since: unix_timestamp(),
            models_used: HashMap::from([("gpt-4o".to_owned(), requests as u32)]),
        }
    }

    #[test]
    fn ingest_and_snapshot_round_trip() {
        let mut analytics = FleetAnalytics::new("acme");
        analytics.ingest(metrics("agent-1", Some("Platform"), 1_000, 10));

        let snapshot = analytics.snapshot();
        assert_eq!(snapshot.org_id, "acme");
        assert_eq!(snapshot.total_agents, 1);
        assert_eq!(snapshot.total_tokens_saved, 1_000);
        assert_eq!(snapshot.top_savers[0].agent_id, "agent-1");
    }

    #[test]
    fn roi_calculation_uses_direct_and_retry_savings() {
        let snapshot = FleetSnapshot {
            total_cost_saved_usd: 10.0,
            total_tokens_saved: 5_000,
            model_distribution: HashMap::from([(
                "gpt-4o".to_owned(),
                ModelUsageStats {
                    model: "gpt-4o".to_owned(),
                    requests: 100,
                    outcome_rate: 0.95,
                    ..ModelUsageStats::default()
                },
            )]),
            ..FleetSnapshot::default()
        };

        let roi = calculate_roi(&snapshot);
        assert!(
            (roi.monthly_savings_usd - 330.0).abs() < 0.01,
            "monthly={}",
            roi.monthly_savings_usd
        );
        assert!(
            (roi.annual_savings_usd - 3_960.0).abs() < 0.01,
            "annual={}",
            roi.annual_savings_usd
        );
        assert!((roi.payback_ratio - 12.0).abs() < 0.01);
    }

    #[test]
    fn per_team_breakdown_aggregates_agents() {
        let mut analytics = FleetAnalytics::new("acme");
        analytics.ingest(metrics("agent-1", Some("Platform"), 1_000, 10));
        analytics.ingest(metrics("agent-2", Some("Platform"), 2_000, 30));
        analytics.ingest(metrics("agent-3", None, 500, 5));

        let snapshot = analytics.snapshot();
        let platform = snapshot.per_team_breakdown.get("Platform").unwrap();
        assert_eq!(platform.agent_count, 2);
        assert_eq!(platform.tokens_saved, 3_000);
        assert!((platform.cost_saved - 25.0).abs() < f64::EPSILON);
        assert_eq!(snapshot.per_team_breakdown["Unassigned"].agent_count, 1);
    }

    #[test]
    fn model_distribution_tracks_requests_and_outcomes() {
        let mut analytics = FleetAnalytics::new("acme");
        let mut first = metrics("agent-1", None, 1_000, 10);
        first.models_used = HashMap::from([("gpt-4o".to_owned(), 10)]);
        let mut second = metrics("agent-2", None, 2_000, 20);
        second.models_used = HashMap::from([
            ("gpt-4o".to_owned(), 5),
            ("claude-sonnet-4.5".to_owned(), 15),
        ]);
        analytics.ingest(first);
        analytics.ingest(second);

        let snapshot = analytics.snapshot();
        assert_eq!(snapshot.model_distribution["gpt-4o"].requests, 15);
        assert_eq!(
            snapshot.model_distribution["claude-sonnet-4.5"].requests,
            15
        );
        assert!((snapshot.model_distribution["gpt-4o"].outcome_rate - 0.95).abs() < f32::EPSILON);
    }

    #[test]
    fn csv_export_contains_snapshot_and_escaped_agent_values() {
        let snapshot = FleetSnapshot {
            org_id: "acme, inc".to_owned(),
            timestamp: 42,
            total_agents: 1,
            top_savers: vec![metrics("agent,1", Some("Platform"), 1_000, 10)],
            ..FleetSnapshot::default()
        };

        let csv = export_csv(&snapshot);
        assert!(csv.starts_with("org_id,timestamp,record_type"));
        assert!(csv.contains("\"acme, inc\""));
        assert!(csv.contains("\"agent,1\""));
        assert!(csv.ends_with('\n'));
    }

    #[test]
    fn jsonl_load_uses_the_latest_agent_record() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("metrics.jsonl");
        let first = metrics("agent-1", None, 100, 1);
        let replacement = metrics("agent-1", None, 200, 2);
        std::fs::write(
            &path,
            format!(
                "{}\n{}\nnot-json\n",
                serde_json::to_string(&first).unwrap(),
                serde_json::to_string(&replacement).unwrap()
            ),
        )
        .unwrap();

        let analytics = FleetAnalytics::load_from_path("acme", &path);
        assert_eq!(analytics.snapshot().total_tokens_saved, 200);
    }
}
