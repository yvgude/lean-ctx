//! Cognition introspection — proves that each scientific subsystem is wired
//! *and* actually active at runtime.
//!
//! The Cognition v2 stack (power-law decay, Hebbian eviction, global-workspace
//! ignition, replay consolidation, FEP prefetch, immune detection, …) is only
//! useful if it is genuinely reached on the hot path — not dead code. Every
//! subsystem calls [`tick`] at its real call site; [`flush`] persists the
//! counters to a small JSON file so a *separate* process (`lean-ctx introspect
//! cognition`) can report, per subsystem: wired (present in this build), active
//! (call count > 0), last-run, and total count.
//!
//! Determinism note (#498): this is diagnostic CLI output, never an MCP tool
//! body, so wall-clock timestamps here do not affect provider prompt caching.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// One cognition subsystem in the activation registry. `key` is the stable
/// identifier passed to [`tick`]; `label`/`science` are for human-readable
/// reports only.
pub struct Subsystem {
    pub key: &'static str,
    pub label: &'static str,
    pub science: &'static str,
}

/// The full set of Cognition v2 subsystems that must be wired and active.
/// Order is the natural reading order for the report (foundation → experiment).
pub const SUBSYSTEMS: &[Subsystem] = &[
    Subsystem {
        key: "phi_recompute",
        label: "Sticky-Phi fix",
        science: "time-variant salience (attention)",
    },
    Subsystem {
        key: "power_law_decay",
        label: "Power-law decay",
        science: "Ebbinghaus forgetting + spacing effect",
    },
    Subsystem {
        key: "hebbian_cache",
        label: "Hebbian eviction",
        science: "co-activation (cells that fire together)",
    },
    Subsystem {
        key: "memory_consolidation",
        label: "Memory consolidation",
        science: "complementary learning systems",
    },
    Subsystem {
        key: "integration_phi",
        label: "Integration-aware Phi",
        science: "IIT non-redundancy (MMR)",
    },
    Subsystem {
        key: "gwt_ignition",
        label: "Global-workspace ignition",
        science: "global workspace theory",
    },
    Subsystem {
        key: "field_weights_bandit",
        label: "Learned field weights",
        science: "reinforcement learning (bandit)",
    },
    Subsystem {
        key: "replay_consolidation",
        label: "Idle replay",
        science: "sharp-wave-ripple replay",
    },
    Subsystem {
        key: "fep_prefetch",
        label: "FEP prefetch",
        science: "active inference / free energy",
    },
    Subsystem {
        key: "immune_detector",
        label: "Immune detector",
        science: "artificial immune system",
    },
    Subsystem {
        key: "observation_synthesis",
        label: "Observation synthesis",
        science: "entity-summary memory (Hindsight)",
    },
    Subsystem {
        key: "qubo_select",
        label: "QUBO selection (spike)",
        science: "quantum-inspired optimization",
    },
];

/// Persisted activity record for a single subsystem.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Activity {
    pub count: u64,
    pub last_unix: i64,
}

#[derive(Default)]
struct Entry {
    count: u64,
    /// Count already merged to disk — lets [`flush`] write only the delta so
    /// concurrent processes accumulate instead of clobbering each other.
    flushed: u64,
    last_unix: i64,
}

#[derive(Default)]
struct Registry {
    entries: HashMap<&'static str, Entry>,
    last_flush: Option<Instant>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(Registry::default()));

/// Minimum gap between debounced flushes, keeping `tick` cheap on the hot path.
const FLUSH_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(2);

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Record one activation of `key`. Cheap (one mutex + map update); call this at
/// the *real* call site of each subsystem so the registry reflects reality.
pub fn tick(key: &'static str) {
    if let Ok(mut reg) = REGISTRY.lock() {
        let e = reg.entries.entry(key).or_default();
        e.count += 1;
        e.last_unix = now_unix();
    }
}

fn activity_path() -> Option<std::path::PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join("cognition_activity.json"))
}

fn read_disk() -> HashMap<String, Activity> {
    activity_path()
        .filter(|p| p.exists())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist in-memory deltas to the shared JSON file (delta-merge so multiple
/// processes accumulate). Best-effort: failures are swallowed — diagnostics
/// must never break the hot path.
pub fn flush() {
    let Ok(mut reg) = REGISTRY.lock() else {
        return;
    };
    if reg.entries.values().all(|e| e.count == e.flushed) {
        reg.last_flush = Some(Instant::now());
        return;
    }
    let mut disk = read_disk();
    for (key, e) in &mut reg.entries {
        let delta = e.count - e.flushed;
        if delta == 0 {
            continue;
        }
        let rec = disk.entry((*key).to_string()).or_default();
        rec.count += delta;
        rec.last_unix = rec.last_unix.max(e.last_unix);
        e.flushed = e.count;
    }
    if let Some(path) = activity_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&disk) {
            let _ = crate::config_io::write_atomic(&path, &json);
        }
    }
    reg.last_flush = Some(Instant::now());
}

/// Debounced [`flush`]: persists at most once per `FLUSH_DEBOUNCE`. Safe to
/// call from `post_dispatch` on every tool call.
pub fn flush_if_due() {
    let due = {
        match REGISTRY.lock() {
            Ok(reg) => reg.last_flush.is_none_or(|t| t.elapsed() >= FLUSH_DEBOUNCE),
            Err(_) => false,
        }
    };
    if due {
        flush();
    }
}

/// Snapshot of all known subsystems merged with persisted activity, for the
/// in-process and CLI reports. Reads disk and overlays unflushed in-memory
/// deltas so a freshly-active subsystem shows up even before the next flush.
pub fn snapshot() -> Vec<(&'static Subsystem, Activity)> {
    let mut disk = read_disk();
    if let Ok(reg) = REGISTRY.lock() {
        for (key, e) in &reg.entries {
            let rec = disk.entry((*key).to_string()).or_default();
            rec.count += e.count - e.flushed;
            rec.last_unix = rec.last_unix.max(e.last_unix);
        }
    }
    SUBSYSTEMS
        .iter()
        .map(|s| {
            let act = disk.get(s.key).cloned().unwrap_or_default();
            (s, act)
        })
        .collect()
}

/// Render a human-readable cognition activity report.
#[must_use]
pub fn format_report() -> String {
    let snap = snapshot();
    let active = snap.iter().filter(|(_, a)| a.count > 0).count();
    let total = snap.len();
    let mut out = String::new();
    out.push_str(&format!(
        "Cognition subsystems: {active}/{total} active ({total} wired)\n\n"
    ));
    for (sys, act) in snap {
        let status = if act.count > 0 { "active" } else { "idle  " };
        let last = if act.last_unix > 0 {
            format_age(now_unix() - act.last_unix)
        } else {
            "never".to_string()
        };
        out.push_str(&format!(
            "  [{status}] {label:<26} count={count:<7} last={last:<10} {science}\n",
            label = sys.label,
            count = act.count,
            science = sys.science,
        ));
    }
    out
}

/// Machine-readable variant for `--json`.
#[must_use]
pub fn snapshot_json() -> String {
    let map: HashMap<&str, Activity> = snapshot().into_iter().map(|(s, a)| (s.key, a)).collect();
    serde_json::to_string_pretty(&map).unwrap_or_else(|_| "{}".to_string())
}

fn format_age(secs: i64) -> String {
    if secs < 0 {
        return "just now".to_string();
    }
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_increments_in_memory_snapshot() {
        // Use a dedicated data dir so flush/snapshot stay isolated.
        let _env = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.path());

        tick("phi_recompute");
        tick("phi_recompute");
        let snap = snapshot();
        let phi = snap
            .iter()
            .find(|(s, _)| s.key == "phi_recompute")
            .map(|(_, a)| a.count)
            .unwrap();
        assert!(phi >= 2, "tick should be reflected in snapshot, got {phi}");
    }

    #[test]
    fn flush_persists_and_accumulates() {
        let _env = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.path());

        tick("hebbian_cache");
        flush();
        let disk = read_disk();
        assert!(disk.get("hebbian_cache").is_some_and(|a| a.count >= 1));

        // A second flush of new ticks must add, not clobber.
        tick("hebbian_cache");
        flush();
        let disk2 = read_disk();
        assert!(
            disk2.get("hebbian_cache").unwrap().count >= 2,
            "deltas should accumulate across flushes"
        );
    }

    #[test]
    fn report_lists_all_subsystems() {
        let _env = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.path());
        let report = format_report();
        for sys in SUBSYSTEMS {
            assert!(
                report.contains(sys.label),
                "report must mention {}",
                sys.label
            );
        }
    }
}
