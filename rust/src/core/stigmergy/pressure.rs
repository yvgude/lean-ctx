//! Spatial pressure fields aggregated from stigmergic pheromone signals.
//!
//! Computes per-path activity pressure used to detect contested hotspots
//! and prioritize context for multi-agent workflows.

use std::collections::{HashMap, HashSet};

use super::signal::{PheromoneSignal, SignalKind};

/// Aggregated pressure from pheromone signals at a location.
#[derive(Debug, Clone, Default)]
pub struct PressureField {
    /// Total signal strength from all agents.
    pub total_strength: f64,
    /// Number of distinct agents with signals here.
    pub agent_count: usize,
    /// Dominant signal kind (most total strength).
    pub dominant_kind: Option<SignalKind>,
    /// Whether any agent is actively working here.
    pub is_contested: bool,
}

/// Map of file paths to their pressure fields.
#[derive(Debug, Clone, Default)]
pub struct PressureMap {
    /// Per-path aggregated pressure from all current signals.
    pub fields: HashMap<String, PressureField>,
}

impl PressureMap {
    /// Compute pressure fields from all current signals.
    ///
    /// Aggregates strength by path, counts distinct agents, and selects the
    /// dominant [`SignalKind`] per location.
    pub fn from_signals(signals: &[PheromoneSignal]) -> Self {
        let mut fields = HashMap::<String, PressureField>::new();
        let mut agents = HashMap::<String, HashSet<&str>>::new();
        let mut strengths = HashMap::<String, [f64; 7]>::new();

        for signal in signals {
            let field = fields.entry(signal.path.clone()).or_default();
            field.total_strength += signal.strength;
            field.is_contested |= signal.kind == SignalKind::Active;

            agents
                .entry(signal.path.clone())
                .or_default()
                .insert(&signal.agent_id);
            strengths.entry(signal.path.clone()).or_default()[kind_index(signal.kind)] +=
                signal.strength;
        }

        for (path, field) in &mut fields {
            field.agent_count = agents.get(path).map_or(0, HashSet::len);
            field.dominant_kind = strengths.get(path).and_then(dominant_kind);
        }

        Self { fields }
    }

    /// Get pressure for a specific path.
    pub fn pressure_at(&self, path: &str) -> PressureField {
        self.fields.get(path).cloned().unwrap_or_default()
    }

    /// Files with the highest pressure (most agent activity).
    pub fn hotspots(&self, top_n: usize) -> Vec<(&str, &PressureField)> {
        let mut hotspots: Vec<_> = self
            .fields
            .iter()
            .map(|(path, field)| (path.as_str(), field))
            .collect();
        hotspots.sort_by(|(path_a, field_a), (path_b, field_b)| {
            field_b
                .total_strength
                .total_cmp(&field_a.total_strength)
                .then_with(|| path_a.cmp(path_b))
        });
        hotspots.truncate(top_n);
        hotspots
    }

    /// Owned file paths with pressure at or above `threshold`, highest first.
    #[must_use]
    pub fn hot_files(&self, threshold: f64) -> Vec<(String, f64)> {
        let mut hot_files = self
            .fields
            .iter()
            .filter(|(_, field)| field.total_strength >= threshold)
            .map(|(path, field)| (path.clone(), field.total_strength))
            .collect::<Vec<_>>();
        hot_files.sort_by(|(path_a, strength_a), (path_b, strength_b)| {
            strength_b
                .total_cmp(strength_a)
                .then_with(|| path_a.cmp(path_b))
        });
        hot_files
    }
}

fn kind_index(kind: SignalKind) -> usize {
    match kind {
        SignalKind::Active => 0,
        SignalKind::Complexity => 1,
        SignalKind::ReviewNeeded => 2,
        SignalKind::Issue => 3,
        SignalKind::Completed => 4,
        SignalKind::Exploration => 5,
        SignalKind::Modification => 6,
    }
}

fn dominant_kind(strengths: &[f64; 7]) -> Option<SignalKind> {
    const KINDS: [SignalKind; 7] = [
        SignalKind::Active,
        SignalKind::Complexity,
        SignalKind::ReviewNeeded,
        SignalKind::Issue,
        SignalKind::Completed,
        SignalKind::Exploration,
        SignalKind::Modification,
    ];
    strengths
        .iter()
        .enumerate()
        .max_by(|(index_a, strength_a), (index_b, strength_b)| {
            strength_a
                .total_cmp(strength_b)
                .then_with(|| index_b.cmp(index_a))
        })
        .map(|(index, _)| KINDS[index])
}

#[cfg(test)]
pub mod tests {
    use chrono::Utc;

    use super::*;

    fn signal(agent_id: &str, path: &str, kind: SignalKind, strength: f64) -> PheromoneSignal {
        PheromoneSignal {
            agent_id: agent_id.to_string(),
            kind,
            path: path.to_string(),
            symbol: None,
            strength,
            deposited_at: Utc::now(),
            note: None,
        }
    }

    #[test]
    fn empty_signals_gives_empty_map() {
        let map = PressureMap::from_signals(&[]);

        assert!(map.fields.is_empty());
        assert_eq!(map.pressure_at("missing.rs").agent_count, 0);
    }

    #[test]
    fn single_signal_creates_field() {
        let map =
            PressureMap::from_signals(&[signal("agent-1", "src/lib.rs", SignalKind::Active, 0.8)]);

        let field = map.pressure_at("src/lib.rs");
        assert_eq!(field.total_strength, 0.8);
        assert_eq!(field.agent_count, 1);
        assert_eq!(field.dominant_kind, Some(SignalKind::Active));
        assert!(field.is_contested);
    }

    #[test]
    fn multiple_agents_increase_pressure() {
        let map = PressureMap::from_signals(&[
            signal("agent-1", "src/lib.rs", SignalKind::Complexity, 0.7),
            signal("agent-2", "src/lib.rs", SignalKind::Complexity, 0.6),
            signal("agent-1", "src/lib.rs", SignalKind::Completed, 0.2),
        ]);

        let field = map.pressure_at("src/lib.rs");
        assert!((field.total_strength - 1.5).abs() < 1e-10);
        assert_eq!(field.agent_count, 2);
        assert_eq!(field.dominant_kind, Some(SignalKind::Complexity));
        assert!(!field.is_contested);
    }

    #[test]
    fn hotspots_returns_top_n() {
        let map = PressureMap::from_signals(&[
            signal("agent-1", "low.rs", SignalKind::Completed, 0.2),
            signal("agent-2", "high.rs", SignalKind::Active, 0.9),
            signal("agent-3", "medium.rs", SignalKind::Issue, 0.5),
        ]);

        let hotspots = map.hotspots(2);

        assert_eq!(hotspots.len(), 2);
        assert_eq!(hotspots[0].0, "high.rs");
        assert_eq!(hotspots[1].0, "medium.rs");
    }

    #[test]
    fn three_explorations_mark_file_hot() {
        let map = PressureMap::from_signals(&[
            signal("agent-1", "src/hot.rs", SignalKind::Exploration, 0.4),
            signal("agent-2", "src/hot.rs", SignalKind::Exploration, 0.4),
            signal("agent-3", "src/hot.rs", SignalKind::Exploration, 0.4),
        ]);

        let hot_files = map.hot_files(1.0);

        assert_eq!(hot_files.len(), 1);
        assert_eq!(hot_files[0].0, "src/hot.rs");
        assert!((hot_files[0].1 - 1.2).abs() < 1e-10);
    }
}
