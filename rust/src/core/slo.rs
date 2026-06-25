//! Context SLOs — configurable service level objectives for context metrics.
//!
//! Loads SLO definitions from `.lean-ctx/slos.toml` and evaluates them
//! against live session counters after each tool call.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::core::budget_tracker::BudgetTracker;
use crate::core::events;

// ---------------------------------------------------------------------------
// Configuration types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SloConfig {
    #[serde(default)]
    pub slo: Vec<SloDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SloDefinition {
    pub name: String,
    pub metric: SloMetric,
    pub threshold: f64,
    #[serde(default)]
    pub direction: SloDirection,
    #[serde(default)]
    pub action: SloAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SloMetric {
    SessionContextTokens,
    SessionCostUsd,
    CompressionRatio,
    ShellInvocations,
    ToolCallsTotal,
    ToolCallCount,
    /// Rolling p95 latency (ms) across team-server `/v1` routes (GL #391).
    TeamQueryP95Ms,
    /// Percentage (0–100) of non-5xx team-server requests in the rolling window.
    TeamAvailabilityPct,
    /// Seconds since the last successful index-mutating tool call on the team server.
    TeamIndexLagSeconds,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SloDirection {
    #[default]
    Max,
    Min,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SloAction {
    #[default]
    Warn,
    Throttle,
    Block,
}

// ---------------------------------------------------------------------------
// Runtime state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SloStatus {
    pub name: String,
    pub metric: SloMetric,
    pub threshold: f64,
    pub actual: f64,
    pub direction: SloDirection,
    pub action: SloAction,
    pub violated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SloSnapshot {
    pub slos: Vec<SloStatus>,
    pub violations: Vec<SloStatus>,
    pub worst_action: Option<SloAction>,
}

#[derive(Debug, Default)]
struct ViolationHistory {
    entries: Vec<ViolationEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ViolationEntry {
    pub timestamp: String,
    pub slo_name: String,
    pub metric: SloMetric,
    pub threshold: f64,
    pub actual: f64,
    pub action: SloAction,
}

static SLO_CONFIG: OnceLock<Mutex<Vec<SloDefinition>>> = OnceLock::new();
static VIOLATION_LOG: OnceLock<Mutex<ViolationHistory>> = OnceLock::new();
static EMIT_STATE: OnceLock<Mutex<HashMap<String, EmitState>>> = OnceLock::new();

const VIOLATION_DEBOUNCE: Duration = Duration::from_secs(30);

#[derive(Debug, Default, Clone)]
struct EmitState {
    last_violated: bool,
    last_emit: Option<Instant>,
}

fn config_store() -> &'static Mutex<Vec<SloDefinition>> {
    SLO_CONFIG.get_or_init(|| Mutex::new(load_slos_from_disk()))
}

fn violation_store() -> &'static Mutex<ViolationHistory> {
    VIOLATION_LOG.get_or_init(|| Mutex::new(ViolationHistory::default()))
}

fn emit_state_store() -> &'static Mutex<HashMap<String, EmitState>> {
    EMIT_STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

fn slo_toml_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Ok(dir) = crate::core::data_dir::lean_ctx_data_dir() {
        paths.push(dir.join("slos.toml"));
    }

    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        paths.push(PathBuf::from(home).join(".lean-ctx").join("slos.toml"));
    }

    if let Ok(cwd) = std::env::current_dir() {
        paths.push(cwd.join(".lean-ctx").join("slos.toml"));
    }

    paths
}

fn load_slos_from_disk() -> Vec<SloDefinition> {
    for path in slo_toml_paths() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            match toml::from_str::<SloConfig>(&content) {
                Ok(cfg) => return cfg.slo,
                Err(e) => {
                    tracing::warn!("slo: parse error in {}: {e}", path.display());
                }
            }
        }
    }
    default_slos()
}

fn default_slos() -> Vec<SloDefinition> {
    vec![
        SloDefinition {
            name: "context_budget".into(),
            metric: SloMetric::SessionContextTokens,
            threshold: 200_000.0,
            direction: SloDirection::Max,
            action: SloAction::Warn,
        },
        SloDefinition {
            name: "cost_per_session".into(),
            metric: SloMetric::SessionCostUsd,
            threshold: 5.0,
            direction: SloDirection::Max,
            action: SloAction::Throttle,
        },
        SloDefinition {
            name: "compression_efficiency".into(),
            metric: SloMetric::CompressionRatio,
            // CompressionRatio = sent/original. Lower is better.
            // Warn when compression is poor (>90% of original still sent after 5000+ tokens).
            // Previous 0.75 threshold triggered false positives for full-mode reads.
            threshold: 0.90,
            direction: SloDirection::Max,
            action: SloAction::Warn,
        },
    ]
}

pub fn reload() {
    let fresh = load_slos_from_disk();
    if let Ok(mut store) = config_store().lock() {
        *store = fresh;
    }
}

#[must_use]
pub fn active_slos() -> Vec<SloDefinition> {
    config_store().lock().map(|s| s.clone()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

fn read_metric(metric: SloMetric) -> f64 {
    let tracker = BudgetTracker::global();
    match metric {
        SloMetric::SessionContextTokens => {
            let live = tracker.tokens_used();
            if live > 0 {
                live as f64
            } else {
                // Out-of-process consumers (dashboard daemon) have an empty
                // in-memory tracker; fall back to the persisted ledger so the
                // SLO reflects the real session instead of a hardcoded 0.
                crate::core::context_ledger::ContextLedger::load().total_tokens_sent as f64
            }
        }
        SloMetric::SessionCostUsd => tracker.cost_usd(),
        SloMetric::ShellInvocations => tracker.shell_used() as f64,
        SloMetric::CompressionRatio => {
            let ledger = crate::core::context_ledger::ContextLedger::load();
            let total_original: usize = ledger.entries.iter().map(|e| e.original_tokens).sum();
            if total_original < 5000 {
                0.0
            } else {
                ledger.compression_ratio()
            }
        }
        SloMetric::ToolCallsTotal | SloMetric::ToolCallCount => tracker.tool_calls_count() as f64,
        SloMetric::TeamQueryP95Ms => crate::core::team_slo::global().snapshot().p95_ms,
        SloMetric::TeamAvailabilityPct => {
            crate::core::team_slo::global().snapshot().availability_pct
        }
        SloMetric::TeamIndexLagSeconds => crate::core::team_slo::global()
            .snapshot()
            .index_lag_seconds
            // No index write yet means "nothing to be stale" — never a violation.
            .unwrap_or(0.0),
    }
}

fn is_violated(actual: f64, threshold: f64, direction: SloDirection) -> bool {
    match direction {
        SloDirection::Max => actual > threshold,
        SloDirection::Min => actual < threshold,
    }
}

pub fn evaluate() -> SloSnapshot {
    let defs = active_slos();
    let mut slos = Vec::with_capacity(defs.len());
    let mut violations = Vec::new();
    let now = Instant::now();
    let mut emit_state = emit_state_store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    for def in &defs {
        let actual = read_metric(def.metric);
        let violated = is_violated(actual, def.threshold, def.direction);

        let status = SloStatus {
            name: def.name.clone(),
            metric: def.metric,
            threshold: def.threshold,
            actual,
            direction: def.direction,
            action: def.action,
            violated,
        };

        if violated {
            let st = emit_state.entry(def.name.clone()).or_default();
            let is_first = !st.last_violated;
            let is_due = st
                .last_emit
                .is_none_or(|t| t.elapsed() >= VIOLATION_DEBOUNCE);
            if is_first || is_due {
                st.last_emit = Some(now);
                record_violation(&status);
                emit_slo_event(&status);
            }
            st.last_violated = true;
            violations.push(status.clone());
        } else if let Some(st) = emit_state.get_mut(&def.name) {
            st.last_violated = false;
        }

        slos.push(status);
    }

    let worst_action = violations.iter().map(|v| v.action).max_by_key(|a| match a {
        SloAction::Warn => 0,
        SloAction::Throttle => 1,
        SloAction::Block => 2,
    });

    SloSnapshot {
        slos,
        violations,
        worst_action,
    }
}

#[must_use]
pub fn evaluate_quiet() -> SloSnapshot {
    // Record that SLO evaluation happened (count-only observability).
    crate::core::verification_observability::record_slo_eval();
    let defs = active_slos();
    let mut slos = Vec::with_capacity(defs.len());
    let mut violations = Vec::new();

    for def in &defs {
        let actual = read_metric(def.metric);
        let violated = is_violated(actual, def.threshold, def.direction);

        let status = SloStatus {
            name: def.name.clone(),
            metric: def.metric,
            threshold: def.threshold,
            actual,
            direction: def.direction,
            action: def.action,
            violated,
        };

        if violated {
            violations.push(status.clone());
        }
        slos.push(status);
    }

    let worst_action = violations.iter().map(|v| v.action).max_by_key(|a| match a {
        SloAction::Warn => 0,
        SloAction::Throttle => 1,
        SloAction::Block => 2,
    });

    SloSnapshot {
        slos,
        violations,
        worst_action,
    }
}

fn record_violation(status: &SloStatus) {
    if let Ok(mut hist) = violation_store().lock() {
        let entry = ViolationEntry {
            timestamp: chrono::Local::now()
                .format("%Y-%m-%dT%H:%M:%S%.3f")
                .to_string(),
            slo_name: status.name.clone(),
            metric: status.metric,
            threshold: status.threshold,
            actual: status.actual,
            action: status.action,
        };
        hist.entries.push(entry);
        if hist.entries.len() > 500 {
            let excess = hist.entries.len() - 500;
            hist.entries.drain(..excess);
        }
    }
}

fn emit_slo_event(status: &SloStatus) {
    let _ = events::emit(events::EventKind::SloViolation {
        slo_name: status.name.clone(),
        metric: format!("{:?}", status.metric),
        threshold: status.threshold,
        actual: status.actual,
        action: format!("{:?}", status.action),
    });
}

#[must_use]
pub fn violation_history(limit: usize) -> Vec<ViolationEntry> {
    violation_store()
        .lock()
        .map(|h| {
            let start = h.entries.len().saturating_sub(limit);
            h.entries[start..].to_vec()
        })
        .unwrap_or_default()
}

pub fn clear_violations() {
    if let Ok(mut hist) = violation_store().lock() {
        hist.entries.clear();
    }
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

impl SloSnapshot {
    #[must_use]
    pub fn format_compact(&self) -> String {
        let total = self.slos.len();
        let violated = self.violations.len();
        let mut out = format!("SLOs: {}/{} passing", total - violated, total);

        for v in &self.violations {
            out.push_str(&format!(
                "\n  !! {} ({:?}): {:.2} vs threshold {:.2} → {:?}",
                v.name, v.metric, v.actual, v.threshold, v.action
            ));
        }

        out
    }

    #[must_use]
    pub fn should_block(&self) -> bool {
        self.worst_action == Some(SloAction::Block)
    }

    #[must_use]
    pub fn should_throttle(&self) -> bool {
        matches!(
            self.worst_action,
            Some(SloAction::Throttle | SloAction::Block)
        )
    }
}

impl std::fmt::Display for SloMetric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionContextTokens => write!(f, "session_context_tokens"),
            Self::SessionCostUsd => write!(f, "session_cost_usd"),
            Self::CompressionRatio => write!(f, "compression_ratio"),
            Self::ShellInvocations => write!(f, "shell_invocations"),
            Self::ToolCallsTotal => write!(f, "tool_calls_total"),
            Self::ToolCallCount => write!(f, "tool_call_count"),
            Self::TeamQueryP95Ms => write!(f, "team_query_p95_ms"),
            Self::TeamAvailabilityPct => write!(f, "team_availability_pct"),
            Self::TeamIndexLagSeconds => write!(f, "team_index_lag_seconds"),
        }
    }
}

impl std::fmt::Display for SloAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Warn => write!(f, "warn"),
            Self::Throttle => write!(f, "throttle"),
            Self::Block => write!(f, "block"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_slos_are_valid() {
        let defs = default_slos();
        assert_eq!(defs.len(), 3);
        assert_eq!(defs[0].name, "context_budget");
        assert_eq!(defs[1].action, SloAction::Throttle);
        assert_eq!(defs[2].direction, SloDirection::Max);
    }

    #[test]
    fn violation_detection_max() {
        assert!(is_violated(60_000.0, 50_000.0, SloDirection::Max));
        assert!(!is_violated(40_000.0, 50_000.0, SloDirection::Max));
    }

    #[test]
    fn violation_detection_min() {
        assert!(is_violated(0.2, 0.3, SloDirection::Min));
        assert!(!is_violated(0.5, 0.3, SloDirection::Min));
    }

    #[test]
    fn slo_config_parses_from_toml() {
        let toml_str = r#"
[[slo]]
name = "test_budget"
metric = "session_context_tokens"
threshold = 100000
action = "warn"

[[slo]]
name = "test_cost"
metric = "session_cost_usd"
threshold = 2.0
action = "block"
direction = "max"
"#;
        let cfg: SloConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.slo.len(), 2);
        assert_eq!(cfg.slo[0].name, "test_budget");
        assert_eq!(cfg.slo[0].metric, SloMetric::SessionContextTokens);
        assert_eq!(cfg.slo[1].action, SloAction::Block);
    }

    #[test]
    fn team_slo_metrics_parse_and_evaluate() {
        // The three hosted-index gate metrics (GL #391) must round-trip
        // through TOML and read live values without panicking.
        let toml_str = r#"
[[slo]]
name = "hosted_index_latency"
metric = "team_query_p95_ms"
threshold = 500
action = "warn"

[[slo]]
name = "hosted_index_availability"
metric = "team_availability_pct"
threshold = 99.5
direction = "min"
action = "warn"

[[slo]]
name = "hosted_index_freshness"
metric = "team_index_lag_seconds"
threshold = 300
action = "warn"
"#;
        let cfg: SloConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.slo.len(), 3);
        assert_eq!(cfg.slo[0].metric, SloMetric::TeamQueryP95Ms);
        assert_eq!(cfg.slo[1].metric, SloMetric::TeamAvailabilityPct);
        assert_eq!(cfg.slo[1].direction, SloDirection::Min);
        assert_eq!(cfg.slo[2].metric, SloMetric::TeamIndexLagSeconds);

        // read_metric must work for team metrics regardless of what other
        // (parallel) tests feed into the global store: only invariant ranges
        // are asserted here, exact values are covered in core::team_slo.
        let availability = read_metric(SloMetric::TeamAvailabilityPct);
        assert!((0.0..=100.0).contains(&availability));
        let lag = read_metric(SloMetric::TeamIndexLagSeconds);
        assert!(lag >= 0.0);
        let p95 = read_metric(SloMetric::TeamQueryP95Ms);
        assert!(p95 >= 0.0);

        assert_eq!(SloMetric::TeamQueryP95Ms.to_string(), "team_query_p95_ms");
        assert_eq!(
            SloMetric::TeamAvailabilityPct.to_string(),
            "team_availability_pct"
        );
        assert_eq!(
            SloMetric::TeamIndexLagSeconds.to_string(),
            "team_index_lag_seconds"
        );
    }

    #[test]
    fn snapshot_format_compact() {
        let snap = SloSnapshot {
            slos: vec![
                SloStatus {
                    name: "budget".into(),
                    metric: SloMetric::SessionContextTokens,
                    threshold: 50000.0,
                    actual: 30000.0,
                    direction: SloDirection::Max,
                    action: SloAction::Warn,
                    violated: false,
                },
                SloStatus {
                    name: "cost".into(),
                    metric: SloMetric::SessionCostUsd,
                    threshold: 1.0,
                    actual: 2.5,
                    direction: SloDirection::Max,
                    action: SloAction::Block,
                    violated: true,
                },
            ],
            violations: vec![SloStatus {
                name: "cost".into(),
                metric: SloMetric::SessionCostUsd,
                threshold: 1.0,
                actual: 2.5,
                direction: SloDirection::Max,
                action: SloAction::Block,
                violated: true,
            }],
            worst_action: Some(SloAction::Block),
        };
        let out = snap.format_compact();
        assert!(out.contains("1/2 passing"));
        assert!(out.contains("cost"));
        assert!(snap.should_block());
    }

    #[test]
    fn snapshot_no_violations() {
        let snap = SloSnapshot {
            slos: vec![SloStatus {
                name: "ok".into(),
                metric: SloMetric::SessionContextTokens,
                threshold: 100_000.0,
                actual: 5000.0,
                direction: SloDirection::Max,
                action: SloAction::Warn,
                violated: false,
            }],
            violations: vec![],
            worst_action: None,
        };
        assert!(!snap.should_block());
        assert!(!snap.should_throttle());
        assert!(snap.format_compact().contains("1/1 passing"));
    }

    #[test]
    fn violation_history_capped() {
        clear_violations();
        for i in 0..10 {
            record_violation(&SloStatus {
                name: format!("slo_{i}"),
                metric: SloMetric::SessionContextTokens,
                threshold: 100.0,
                actual: 200.0,
                direction: SloDirection::Max,
                action: SloAction::Warn,
                violated: true,
            });
        }
        let hist = violation_history(5);
        assert_eq!(hist.len(), 5);
        assert_eq!(hist[0].slo_name, "slo_5");
    }
}
