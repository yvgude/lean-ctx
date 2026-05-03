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
                    eprintln!("[lean-ctx] slo: parse error in {}: {e}", path.display());
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
            // Warn only when compression is poor (e.g. >75% of original tokens still sent).
            threshold: 0.75,
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

pub fn active_slos() -> Vec<SloDefinition> {
    config_store().lock().map(|s| s.clone()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

fn read_metric(metric: SloMetric) -> f64 {
    let tracker = BudgetTracker::global();
    match metric {
        SloMetric::SessionContextTokens => tracker.tokens_used() as f64,
        SloMetric::SessionCostUsd => tracker.cost_usd(),
        SloMetric::ShellInvocations => tracker.shell_used() as f64,
        SloMetric::CompressionRatio => {
            let ledger = crate::core::context_ledger::ContextLedger::load();
            if ledger.entries.is_empty() {
                0.0
            } else {
                ledger.compression_ratio()
            }
        }
        SloMetric::ToolCallsTotal => (tracker.tokens_used().max(1) / 1000) as f64,
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

pub fn evaluate_quiet() -> SloSnapshot {
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
    events::emit(events::EventKind::SloViolation {
        slo_name: status.name.clone(),
        metric: format!("{:?}", status.metric),
        threshold: status.threshold,
        actual: status.actual,
        action: format!("{:?}", status.action),
    });
}

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

    pub fn should_block(&self) -> bool {
        self.worst_action == Some(SloAction::Block)
    }

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
