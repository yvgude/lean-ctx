//! Stigmergic scent field for zero-token multi-agent coordination (#540, EFF-3).
//!
//! Agents coordinate indirectly through a shared, time-decaying field of
//! "scent" deposits instead of reading each other's scratchpad messages
//! (Many Tems: 3.4x fewer coordination tokens; Pressure Fields 2601.08129:
//! temporal decay prevents premature convergence). Deposits happen as side
//! effects of normal work — reads, bounces, claims, handoffs — and the `sync`
//! view is pure arithmetic over the field: no LLM calls, no message reads.
//!
//! Storage: one JSON file under `data_dir/agents/scent_field.json`, guarded by
//! the same create-new file lock the agent registry uses. Decayed entries are
//! garbage-collected lazily on every locked operation — no daemon, no timer.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Scents below this effective intensity are dead and get collected.
const GC_THRESHOLD: f64 = 0.05;
/// Superposed intensity per (agent, kind, target) is capped here.
const INTENSITY_CAP: f64 = 3.0;
/// A foreign claim is considered active at or above this effective intensity.
pub const CLAIM_ACTIVE_THRESHOLD: f64 = 0.3;
/// Max rendered lines in the sync view.
const SYNC_TOP_K: usize = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScentKind {
    /// Agent is actively working on this target (file, task, deploy unit).
    Claimed,
    /// Agent finished something related to the target.
    Done,
    /// Agent hit a wall here (edit failures, bounces).
    Stuck,
    /// Target is being read/touched a lot right now.
    Hot,
    /// Target should not be touched (e.g. broken generated file).
    Avoid,
}

impl ScentKind {
    /// Exponential-decay half-life per kind, in seconds.
    fn half_life_secs(self) -> f64 {
        match self {
            ScentKind::Claimed | ScentKind::Hot => 600.0,
            ScentKind::Stuck => 1800.0,
            ScentKind::Done | ScentKind::Avoid => 3600.0,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ScentKind::Claimed => "CLAIMED",
            ScentKind::Done => "DONE",
            ScentKind::Stuck => "STUCK",
            ScentKind::Hot => "HOT",
            ScentKind::Avoid => "AVOID",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scent {
    pub agent_id: String,
    pub kind: ScentKind,
    /// Normalized target: relative file path, task label, or deploy unit.
    pub target: String,
    /// Deposited (pre-decay) intensity.
    pub intensity: f64,
    /// Unix seconds at deposit time.
    pub deposited_at: u64,
}

impl Scent {
    #[must_use]
    pub fn effective_intensity(&self, now: u64) -> f64 {
        let dt = now.saturating_sub(self.deposited_at) as f64;
        self.intensity * (-(std::f64::consts::LN_2) * dt / self.kind.half_life_secs()).exp()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScentField {
    pub scents: Vec<Scent>,
    pub schema_version: u32,
    /// Lifetime count of rejected claims (#549): every rejection is a piece
    /// of duplicate work the field prevented — the efficacy currency of #540.
    #[serde(default)]
    pub claims_rejected: u64,
}

fn field_path() -> Result<PathBuf, String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()?.join("agents");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("scent_field.json"))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Scent-field identity (#547). `agent_identity::current_agent_id` falls back
/// to a shared `"local"` for every unconfigured process, which would make
/// claims between two parallel MCP servers on the same machine invisible to
/// each other (`foreign_claim` filters on `agent_id != self`). For scents we
/// disambiguate with the PID; ledger/heatmap attribution keeps using the
/// stable shared identity and is intentionally NOT changed.
#[must_use]
pub fn scent_agent_id() -> &'static str {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        let base = crate::core::agent_identity::current_agent_id();
        if base == "local" {
            format!("local-{}", std::process::id())
        } else {
            base.to_string()
        }
    })
}

impl ScentField {
    fn load_unlocked(path: &PathBuf) -> Self {
        if let Ok(content) = std::fs::read_to_string(path)
            && let Ok(f) = serde_json::from_str::<ScentField>(&content)
        {
            return f;
        }
        ScentField {
            schema_version: 1,
            ..Default::default()
        }
    }

    fn save_unlocked(&self, path: &PathBuf) -> Result<(), String> {
        let json = serde_json::to_string(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }

    /// Drop scents whose effective intensity fell below the GC threshold.
    pub fn gc(&mut self, now: u64) {
        self.scents
            .retain(|s| s.effective_intensity(now) >= GC_THRESHOLD);
    }

    /// Superpose a deposit: same (agent, kind, target) folds into one scent
    /// with summed effective intensity (capped), fresh timestamp.
    pub fn deposit(
        &mut self,
        agent_id: &str,
        kind: ScentKind,
        target: &str,
        intensity: f64,
        now: u64,
    ) {
        self.gc(now);
        let target = target.trim();
        if target.is_empty() || agent_id.is_empty() {
            return;
        }
        if let Some(existing) = self
            .scents
            .iter_mut()
            .find(|s| s.agent_id == agent_id && s.kind == kind && s.target == target)
        {
            let carried = existing.effective_intensity(now);
            existing.intensity = (carried + intensity).min(INTENSITY_CAP);
            existing.deposited_at = now;
        } else {
            self.scents.push(Scent {
                agent_id: agent_id.to_string(),
                kind,
                target: target.to_string(),
                intensity: intensity.min(INTENSITY_CAP),
                deposited_at: now,
            });
        }
    }

    /// Active foreign claim on `target`, if any: returns (`agent_id`, `age_secs`).
    #[must_use]
    pub fn foreign_claim(&self, target: &str, self_agent: &str, now: u64) -> Option<(String, u64)> {
        self.scents
            .iter()
            .filter(|s| {
                s.kind == ScentKind::Claimed
                    && s.target == target
                    && s.agent_id != self_agent
                    && s.effective_intensity(now) >= CLAIM_ACTIVE_THRESHOLD
            })
            .max_by(|a, b| {
                a.effective_intensity(now)
                    .partial_cmp(&b.effective_intensity(now))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|s| (s.agent_id.clone(), now.saturating_sub(s.deposited_at)))
    }

    /// Arithmetic sync view: targets grouped, intensities superposed across
    /// agents, sorted by total intensity, capped at `SYNC_TOP_K` lines.
    #[must_use]
    pub fn render_sync(&self, now: u64) -> String {
        use std::collections::HashMap;
        // (kind, target) -> (total intensity, agents)
        type SyncKey<'a> = (ScentKind, &'a str);
        type SyncAgg<'a> = (f64, Vec<&'a str>);
        let mut groups: HashMap<SyncKey<'_>, SyncAgg<'_>> = HashMap::new();
        for s in &self.scents {
            let eff = s.effective_intensity(now);
            if eff < GC_THRESHOLD {
                continue;
            }
            let entry = groups.entry((s.kind, s.target.as_str())).or_default();
            entry.0 += eff;
            if !entry.1.contains(&s.agent_id.as_str()) {
                entry.1.push(s.agent_id.as_str());
            }
        }
        if groups.is_empty() {
            return String::new();
        }
        let mut rows: Vec<(SyncKey<'_>, SyncAgg<'_>)> = groups.into_iter().collect();
        rows.sort_by(|a, b| {
            b.1.0
                .partial_cmp(&a.1.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.1.cmp(b.0.1))
        });

        let mut out = String::from("Scent field (decaying, zero-token coordination):\n");
        for ((kind, target), (total, agents)) in rows.iter().take(SYNC_TOP_K) {
            let who = if agents.len() == 1 {
                agents[0].to_string()
            } else {
                format!("{} agents", agents.len())
            };
            out.push_str(&format!(
                "  {} {} ({:.1}) by {}\n",
                kind.as_str(),
                target,
                total,
                who
            ));
        }
        let extra = rows.len().saturating_sub(SYNC_TOP_K);
        if extra > 0 {
            out.push_str(&format!("  … {extra} weaker scent(s) below cutoff\n"));
        }
        out
    }
}

/// Locked load-modify-save against the shared field file.
fn with_field<R>(f: impl FnOnce(&mut ScentField, u64) -> R) -> Result<R, String> {
    let path = field_path()?;
    let lock_path = path.with_extension("json.lock");
    let _lock = crate::core::agents::FileLock::acquire(&lock_path)?;
    let mut field = ScentField::load_unlocked(&path);
    let now = now_secs();
    let result = f(&mut field, now);
    field.gc(now);
    field.save_unlocked(&path)?;
    Ok(result)
}

/// Deposit a scent as a side effect of normal work. Errors are swallowed —
/// coordination hints must never break the primary operation.
pub fn deposit(agent_id: &str, kind: ScentKind, target: &str, intensity: f64) {
    let _ = with_field(|field, now| field.deposit(agent_id, kind, target, intensity, now));
}

/// Atomic claim: fails with the holder's id if another agent's claim is still
/// active, otherwise deposits a strong Claimed scent for `agent_id`.
pub fn claim(agent_id: &str, target: &str) -> Result<(), String> {
    with_field(|field, now| {
        if let Some((holder, age)) = field.foreign_claim(target, agent_id, now) {
            field.claims_rejected += 1;
            return Err(format!(
                "already claimed by {holder} ({}m ago, still active)",
                age / 60
            ));
        }
        field.deposit(agent_id, ScentKind::Claimed, target, 2.0, now);
        Ok(())
    })?
}

/// Lifetime rejected-claim counter (#549): duplicate work prevented.
#[must_use]
pub fn claims_rejected_total() -> u64 {
    field_path().map_or(0, |p| ScentField::load_unlocked(&p).claims_rejected)
}

/// Read-only view of currently effective scents for the dashboard (#548):
/// `(scent, effective_intensity_now)`, strongest first. Lock-free read —
/// a slightly stale view is fine for display.
#[must_use]
pub fn active_scents() -> Vec<(Scent, f64)> {
    let Ok(path) = field_path() else {
        return Vec::new();
    };
    let field = ScentField::load_unlocked(&path);
    let now = now_secs();
    let mut v: Vec<(Scent, f64)> = field
        .scents
        .into_iter()
        .filter_map(|s| {
            let eff = s.effective_intensity(now);
            (eff >= GC_THRESHOLD).then_some((s, eff))
        })
        .collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    v
}

/// Release a claim (and any Hot scent) on `target` held by `agent_id`.
pub fn release(agent_id: &str, target: &str) {
    let _ = with_field(|field, now| {
        field.scents.retain(|s| {
            !(s.agent_id == agent_id && s.target == target && s.kind == ScentKind::Claimed)
        });
        field.gc(now);
    });
}

/// One-line hint for `ctx_read` when someone else actively claimed this path.
/// Costs ~10 tokens and prevents duplicate work.
#[must_use]
pub fn read_hint(path: &str, self_agent: &str) -> Option<String> {
    let field_file = field_path().ok()?;
    // Read-only fast path: no lock needed for a hint; stale reads are fine.
    let field = ScentField::load_unlocked(&field_file);
    let now = now_secs();
    let rel = crate::core::pathutil::normalize_tool_path(path);
    let (holder, age) = field.foreign_claim(&rel, self_agent, now)?;
    Some(format!("[scent: claimed by {holder} {}m ago]", age / 60))
}

/// Arithmetic sync block for `ctx_agent` sync.
#[must_use]
pub fn sync_block() -> String {
    let Ok(path) = field_path() else {
        return String::new();
    };
    let field = ScentField::load_unlocked(&path);
    field.render_sync(now_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_780_000_000;

    #[test]
    fn decay_halves_at_half_life() {
        let s = Scent {
            agent_id: "a1".into(),
            kind: ScentKind::Hot,
            target: "src/x.rs".into(),
            intensity: 1.0,
            deposited_at: NOW,
        };
        let eff = s.effective_intensity(NOW + 600);
        assert!((eff - 0.5).abs() < 0.01, "half-life decay, got {eff}");
        // After two half-lives < 0.3 (ticket acceptance).
        assert!(s.effective_intensity(NOW + 1200) < 0.3);
    }

    #[test]
    fn superposition_caps_intensity() {
        let mut f = ScentField::default();
        for _ in 0..20 {
            f.deposit("a1", ScentKind::Hot, "src/x.rs", 0.3, NOW);
        }
        assert_eq!(f.scents.len(), 1);
        assert!(f.scents[0].intensity <= INTENSITY_CAP + f64::EPSILON);
    }

    #[test]
    fn gc_drops_dead_scents() {
        let mut f = ScentField::default();
        f.deposit("a1", ScentKind::Hot, "src/x.rs", 0.3, NOW);
        f.gc(NOW + 6 * 600); // six half-lives: 0.3 -> ~0.0047
        assert!(f.scents.is_empty());
    }

    #[test]
    fn foreign_claim_detected_and_own_ignored() {
        let mut f = ScentField::default();
        f.deposit("a1", ScentKind::Claimed, "src/x.rs", 2.0, NOW);
        assert!(f.foreign_claim("src/x.rs", "a2", NOW + 60).is_some());
        assert!(f.foreign_claim("src/x.rs", "a1", NOW + 60).is_none());
        // Expired claim no longer blocks.
        assert!(f.foreign_claim("src/x.rs", "a2", NOW + 3 * 600).is_none());
    }

    #[test]
    fn sync_view_caps_lines_and_superposes() {
        let mut f = ScentField::default();
        for i in 0..50 {
            f.deposit("a1", ScentKind::Hot, &format!("src/f{i}.rs"), 0.5, NOW);
        }
        f.deposit("a2", ScentKind::Hot, "src/f0.rs", 0.5, NOW);
        let view = f.render_sync(NOW);
        let lines: Vec<&str> = view.lines().collect();
        assert!(
            lines.len() <= SYNC_TOP_K + 2,
            "header + topk + overflow, got {}",
            lines.len()
        );
        assert!(view.contains("2 agents"), "superposed line: {view}");
        assert!(view.contains("weaker scent"));
    }

    #[test]
    fn empty_field_renders_empty() {
        let f = ScentField::default();
        assert!(f.render_sync(NOW).is_empty());
    }

    #[test]
    fn scent_identity_disambiguates_unconfigured_processes() {
        let id = scent_agent_id();
        let base = crate::core::agent_identity::current_agent_id();
        if base == "local" {
            // #547: two parallel unconfigured processes must not collide.
            assert_eq!(id, format!("local-{}", std::process::id()));
        } else {
            // Explicitly configured identity is kept verbatim.
            assert_eq!(id, base);
        }
        // Claims between distinct PIDs are mutually foreign.
        let mut f = ScentField::default();
        f.deposit("local-1111", ScentKind::Claimed, "src/x.rs", 2.0, NOW);
        assert!(
            f.foreign_claim("src/x.rs", "local-2222", NOW + 30)
                .is_some()
        );
    }
}
