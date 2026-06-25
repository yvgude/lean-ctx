use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::context_field::{
    ContextItemId, ContextKind, ContextState, Provenance, ViewCosts, ViewKind,
};

const DEFAULT_CONTEXT_WINDOW: usize = 128_000;

/// EMA weight for the freshly computed Phi on a re-read (#2). 0.5 keeps equal
/// weight on the new signal and the running history, so salience tracks recency
/// and task changes without overreacting to one read.
const PHI_REREAD_ALPHA: f64 = 0.5;

/// Default Global-Workspace ignition threshold (#6) as a Phi z-score: an item
/// must stand more than this many standard deviations above the mean salience to
/// "ignite" and be broadcast (promoted to Pinned) into the global workspace.
const GWT_IGNITION_Z: f64 = 1.5;
/// Minimum number of scored entries before ignition can fire — below this the
/// Phi distribution is too small to identify a meaningful outlier, so ignition
/// is suppressed to avoid pinning everything on a cold ledger.
const GWT_MIN_ENTRIES: usize = 4;

fn ledger_path(agent_id: &str) -> Result<std::path::PathBuf, String> {
    let dir = crate::core::paths::state_dir()?;
    if agent_id == "default" {
        Ok(dir.join("context_ledger.json"))
    } else {
        let ledger_dir = dir.join("ledger");
        let safe_id: String = agent_id
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        Ok(ledger_dir.join(format!("{safe_id}.json")))
    }
}

fn atomic_write_json(path: &std::path::Path, data: &str) {
    let _ = crate::config_io::write_atomic(path, data);
}

/// Acquire an advisory file lock for cross-process safety.
/// Returns the lock file handle (lock released on drop).
#[cfg(unix)]
fn acquire_ledger_lock(path: &std::path::Path) -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let lock_path = path.with_extension("json.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .ok()?;
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is a valid open descriptor owned by `file`, which outlives
    // this call; `flock` dereferences no pointers.
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        // Lock held — block up to 2s
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            std::thread::sleep(Duration::from_millis(50));
            // SAFETY: `fd` is still a valid open descriptor owned by `file`,
            // which outlives this call; `flock` dereferences no pointers.
            let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if ret == 0 {
                break;
            }
            if Instant::now() >= deadline {
                return None;
            }
        }
    }
    Some(file)
}

#[cfg(not(unix))]
fn acquire_ledger_lock(_path: &std::path::Path) -> Option<std::fs::File> {
    None
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextLedger {
    pub window_size: usize,
    pub entries: Vec<LedgerEntry>,
    pub total_tokens_sent: usize,
    pub total_tokens_saved: usize,
    #[serde(skip)]
    last_flush: Option<std::time::Instant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub path: String,
    pub mode: String,
    pub original_tokens: usize,
    pub sent_tokens: usize,
    pub timestamp: i64,
    #[serde(default)]
    pub id: Option<ContextItemId>,
    #[serde(default)]
    pub kind: Option<ContextKind>,
    #[serde(default)]
    pub source_hash: Option<String>,
    #[serde(default)]
    pub state: Option<ContextState>,
    #[serde(default)]
    pub phi: Option<f64>,
    #[serde(default)]
    pub view_costs: Option<ViewCosts>,
    #[serde(default)]
    pub active_view: Option<ViewKind>,
    #[serde(default)]
    pub provenance: Option<Provenance>,
    /// How many times this item has been (re)read into context. Drives the
    /// "high tokens + low recent use" eviction-candidate heuristic.
    #[serde(default)]
    pub access_count: u32,
}

#[derive(Debug, Clone)]
pub struct ContextPressure {
    pub utilization: f64,
    pub remaining_tokens: usize,
    pub entries_count: usize,
    pub recommendation: PressureAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressureAction {
    NoAction,
    SuggestCompression,
    ForceCompression,
    EvictLeastRelevant,
}

impl ContextLedger {
    #[must_use]
    pub fn new() -> Self {
        Self {
            window_size: DEFAULT_CONTEXT_WINDOW,
            entries: Vec::new(),
            total_tokens_sent: 0,
            total_tokens_saved: 0,
            last_flush: None,
        }
    }

    #[must_use]
    pub fn with_window_size(size: usize) -> Self {
        Self {
            window_size: size,
            entries: Vec::new(),
            total_tokens_sent: 0,
            total_tokens_saved: 0,
            last_flush: None,
        }
    }

    pub fn record(&mut self, path: &str, mode: &str, original_tokens: usize, sent_tokens: usize) {
        self.record_with_task(path, mode, original_tokens, sent_tokens, None);
    }

    pub fn record_with_task(
        &mut self,
        path: &str,
        mode: &str,
        original_tokens: usize,
        sent_tokens: usize,
        task: Option<&str>,
    ) {
        let path = crate::core::pathutil::normalize_tool_path(path);
        let item_id = ContextItemId::from_file(&path);

        let phi =
            Self::compute_real_phi(&path, sent_tokens, original_tokens, self.window_size, task);

        if let Some(existing) = self.entries.iter_mut().find(|e| e.path == path) {
            self.total_tokens_sent -= existing.sent_tokens;
            self.total_tokens_saved -= existing
                .original_tokens
                .saturating_sub(existing.sent_tokens);
            existing.mode = mode.to_string();
            existing.original_tokens = original_tokens;
            existing.sent_tokens = sent_tokens;
            existing.timestamp = chrono::Utc::now().timestamp();
            existing.access_count = existing.access_count.saturating_add(1);
            existing.active_view = Some(ViewKind::parse(mode));
            if existing.id.is_none() {
                existing.id = Some(item_id);
            }
            if existing.state.is_none() || existing.state == Some(ContextState::Candidate) {
                existing.state = Some(ContextState::Included);
            }
            // #2 Sticky-Phi fix: salience is time-variant (recency, task match,
            // access frequency all changed since the first read), so recompute
            // Phi on every re-read instead of freezing the first value. Blend
            // with the prior score via a fixed-alpha EMA — deterministic, and
            // damped so a single noisy read can't whipsaw eviction order.
            existing.phi = Some(match existing.phi {
                Some(old) => PHI_REREAD_ALPHA * phi + (1.0 - PHI_REREAD_ALPHA) * old,
                None => phi,
            });
            crate::core::introspect::tick("phi_recompute");
        } else {
            self.entries.push(LedgerEntry {
                path: path.clone(),
                mode: mode.to_string(),
                original_tokens,
                sent_tokens,
                timestamp: chrono::Utc::now().timestamp(),
                id: Some(item_id),
                kind: Some(ContextKind::File),
                source_hash: None,
                state: Some(ContextState::Included),
                phi: Some(phi),
                view_costs: Some(ViewCosts::from_full_tokens(original_tokens)),
                active_view: Some(ViewKind::parse(mode)),
                provenance: None,
                access_count: 1,
            });
        }
        self.total_tokens_sent += sent_tokens;
        self.total_tokens_saved += original_tokens.saturating_sub(sent_tokens);
    }

    fn compute_real_phi(
        path: &str,
        sent_tokens: usize,
        original_tokens: usize,
        window_size: usize,
        task: Option<&str>,
    ) -> f64 {
        use crate::core::context_field::{ContextField, compute_signals_for_path};

        let (signals, _costs) =
            compute_signals_for_path(path, task, None, window_size, original_tokens);
        // #4: use the learned (bandit-selected) field weights when available.
        let phi = ContextField::active().compute_phi(&signals);
        if phi > 0.0 {
            return phi;
        }

        Self::compute_lightweight_phi(sent_tokens, window_size)
    }

    fn compute_lightweight_phi(sent_tokens: usize, window_size: usize) -> f64 {
        use crate::core::context_field::{ContextField, FieldSignals};
        let token_cost_norm = if window_size > 0 {
            (sent_tokens as f64 / window_size as f64).min(1.0)
        } else {
            0.0
        };
        let signals = FieldSignals {
            relevance: 1.0,
            surprise: 0.5,
            graph_proximity: 0.0,
            history_signal: 0.0,
            token_cost_norm,
            redundancy: 0.0,
        };
        ContextField::active().compute_phi(&signals)
    }

    /// Record with full CFT metadata including source hash and provenance.
    pub fn upsert(
        &mut self,
        path: &str,
        mode: &str,
        original_tokens: usize,
        sent_tokens: usize,
        source_hash: Option<&str>,
        kind: ContextKind,
        provenance: Option<Provenance>,
    ) {
        self.record(path, mode, original_tokens, sent_tokens);
        if let Some(entry) = self.entries.iter_mut().find(|e| e.path == path) {
            entry.kind = Some(kind);
            if let Some(h) = source_hash
                && entry.source_hash.as_deref() != Some(h)
            {
                if entry.source_hash.is_some() {
                    entry.state = Some(ContextState::Stale);
                }
                entry.source_hash = Some(h.to_string());
            }
            if let Some(prov) = provenance {
                entry.provenance = Some(prov);
            }
        }
    }

    /// Update the Phi score for an entry.
    pub fn update_phi(&mut self, path: &str, phi: f64) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.path == path) {
            entry.phi = Some(phi);
        }
    }

    /// Set the state for an entry.
    pub fn set_state(&mut self, path: &str, state: ContextState) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.path == path) {
            entry.state = Some(state);
        }
    }

    /// Find an entry by its `ContextItemId`.
    #[must_use]
    pub fn find_by_id(&self, id: &ContextItemId) -> Option<&LedgerEntry> {
        self.entries.iter().find(|e| e.id.as_ref() == Some(id))
    }

    /// Get all entries with a specific state.
    #[must_use]
    pub fn items_by_state(&self, state: ContextState) -> Vec<&LedgerEntry> {
        self.entries
            .iter()
            .filter(|e| e.state == Some(state))
            .collect()
    }

    /// Eviction candidates ordered by Phi (lowest first), falling back to
    /// timestamp for entries without Phi scores.
    #[must_use]
    pub fn eviction_candidates_by_phi(&self, keep_count: usize) -> Vec<String> {
        if self.entries.len() <= keep_count {
            return Vec::new();
        }
        let mut sorted = self.entries.clone();
        sorted.sort_by(|a, b| {
            let a_phi = a.phi.unwrap_or(0.0);
            let b_phi = b.phi.unwrap_or(0.0);
            a_phi
                .partial_cmp(&b_phi)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.timestamp.cmp(&b.timestamp))
        });
        sorted
            .iter()
            .filter(|e| e.state != Some(ContextState::Pinned))
            .take(self.entries.len() - keep_count)
            .map(|e| e.path.clone())
            .collect()
    }

    /// Global-Workspace ignition (#6): context items compete on salience (Phi);
    /// any whose z-score exceeds the ignition threshold is "broadcast" — promoted
    /// to Pinned so it survives eviction (`eviction_candidates_by_phi` already
    /// skips Pinned) and pressure reinjection, and reaches the compiler's working
    /// set as a pinned candidate. Deterministic: a pure threshold over the current
    /// Phi distribution, no sampling. Returns the paths newly ignited this call.
    pub fn ignite_high_salience(&mut self) -> Vec<String> {
        let z_threshold = ignition_z_threshold();
        let phis: Vec<f64> = self.entries.iter().filter_map(|e| e.phi).collect();
        if phis.len() < GWT_MIN_ENTRIES {
            return Vec::new();
        }
        let n = phis.len() as f64;
        let mean = phis.iter().sum::<f64>() / n;
        let var = phis.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / n;
        let std = var.sqrt();
        if std <= f64::EPSILON {
            return Vec::new();
        }

        let mut ignited = Vec::new();
        for e in &mut self.entries {
            let Some(phi) = e.phi else { continue };
            let state = e.state.unwrap_or(ContextState::Included);
            if matches!(state, ContextState::Excluded | ContextState::Pinned) {
                continue;
            }
            if (phi - mean) / std > z_threshold {
                e.state = Some(ContextState::Pinned);
                ignited.push(e.path.clone());
            }
        }
        if !ignited.is_empty() {
            crate::core::introspect::tick("gwt_ignition");
        }
        ignited
    }

    /// Mark entries as stale if their source hash has changed.
    pub fn mark_stale_by_hash(&mut self, path: &str, new_hash: &str) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.path == path)
            && let Some(ref old_hash) = entry.source_hash
            && old_hash != new_hash
        {
            entry.state = Some(ContextState::Stale);
            entry.source_hash = Some(new_hash.to_string());
        }
    }

    #[must_use]
    pub fn pressure(&self) -> ContextPressure {
        let utilization = self.total_tokens_sent as f64 / self.window_size as f64;

        let pinned_count = self
            .entries
            .iter()
            .filter(|e| e.state == Some(ContextState::Pinned))
            .count();
        let stale_count = self
            .entries
            .iter()
            .filter(|e| e.state == Some(ContextState::Stale))
            .count();
        let pinned_pressure = pinned_count as f64 * 0.02;
        let stale_penalty = stale_count as f64 * 0.01;
        let effective_utilization = (utilization + pinned_pressure + stale_penalty).min(1.0);

        let effective_used = (effective_utilization * self.window_size as f64).round() as usize;
        let remaining = self.window_size.saturating_sub(effective_used);

        let recommendation = if effective_utilization > 0.9 {
            PressureAction::EvictLeastRelevant
        } else if effective_utilization > 0.75 {
            PressureAction::ForceCompression
        } else if effective_utilization > 0.5 {
            PressureAction::SuggestCompression
        } else {
            PressureAction::NoAction
        };

        ContextPressure {
            utilization: effective_utilization,
            remaining_tokens: remaining,
            entries_count: self.entries.len(),
            recommendation,
        }
    }

    #[must_use]
    pub fn compression_ratio(&self) -> f64 {
        let total_original: usize = self.entries.iter().map(|e| e.original_tokens).sum();
        if total_original == 0 {
            return 1.0;
        }
        self.total_tokens_sent as f64 / total_original as f64
    }

    #[must_use]
    pub fn files_by_token_cost(&self) -> Vec<(String, usize)> {
        let mut costs: Vec<(String, usize)> = self
            .entries
            .iter()
            .map(|e| (e.path.clone(), e.sent_tokens))
            .collect();
        costs.sort_by_key(|b| std::cmp::Reverse(b.1));
        costs
    }

    #[must_use]
    pub fn mode_distribution(&self) -> HashMap<String, usize> {
        let mut dist: HashMap<String, usize> = HashMap::new();
        for entry in &self.entries {
            *dist.entry(entry.mode.clone()).or_insert(0) += 1;
        }
        dist
    }

    #[must_use]
    pub fn eviction_candidates(&self, keep_count: usize) -> Vec<String> {
        if self.entries.len() <= keep_count {
            return Vec::new();
        }
        let mut sorted = self.entries.clone();
        sorted.sort_by_key(|e| e.timestamp);
        sorted
            .iter()
            .take(self.entries.len() - keep_count)
            .map(|e| e.path.clone())
            .collect()
    }

    pub fn remove(&mut self, path: &str) -> bool {
        if let Some(idx) = self.entries.iter().position(|e| e.path == path) {
            let entry = &self.entries[idx];
            self.total_tokens_sent = self.total_tokens_sent.saturating_sub(entry.sent_tokens);
            self.total_tokens_saved = self
                .total_tokens_saved
                .saturating_sub(entry.original_tokens.saturating_sub(entry.sent_tokens));
            self.entries.remove(idx);
            true
        } else {
            false
        }
    }

    /// Clear all entries and reset totals to zero.
    pub fn reset(&mut self) {
        let pinned_count = self
            .entries
            .iter()
            .filter(|e| e.state == Some(ContextState::Pinned))
            .count();
        self.entries.clear();
        self.total_tokens_sent = 0;
        self.total_tokens_saved = 0;
        if pinned_count > 0 {
            tracing::info!("{pinned_count} pinned entries were also cleared");
        }
    }

    /// Remove specific paths from the ledger. Returns count of entries removed.
    /// Paths are normalized before matching.
    pub fn evict_paths(&mut self, paths: &[&str]) -> usize {
        let mut removed = 0;
        for path in paths {
            let normalized = crate::core::pathutil::normalize_tool_path(path);
            if self.remove(&normalized) {
                removed += 1;
            }
        }
        removed
    }

    pub fn save(&self) {
        self.save_for_agent("default");
    }

    /// Debounced save: only flushes to disk if >=3s since last save.
    /// Reduces I/O overhead during burst sequences of tool calls.
    pub fn save_debounced(&mut self) {
        let now = std::time::Instant::now();
        if let Some(last) = self.last_flush
            && now.duration_since(last) < std::time::Duration::from_secs(3)
        {
            return;
        }
        self.save();
        self.last_flush = Some(now);
    }

    pub fn save_for_agent(&self, agent_id: &str) {
        if let Ok(path) = ledger_path(agent_id) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _lock = acquire_ledger_lock(&path);
            if let Ok(json) = serde_json::to_string(self) {
                atomic_write_json(&path, &json);
            }
        }
    }

    const MAX_LEDGER_ENTRIES: usize = 200;
    const STALE_AGE_SECS: i64 = 7 * 24 * 3600;

    pub fn prune(&mut self) -> usize {
        let before = self.entries.len();
        let now = chrono::Utc::now().timestamp();

        for entry in &mut self.entries {
            if let Some(phi) = entry.phi {
                let hours_since = ((now - entry.timestamp) as f64 / 3600.0).max(0.0);
                let decayed = phi * 0.95_f64.powf(hours_since);
                entry.phi = Some(decayed.max(0.0));
            }
        }

        self.entries
            .retain(|e| !(e.mode == "error" && e.original_tokens == 0));

        self.entries.retain(|e| {
            let age = now - e.timestamp;
            let phi = e.phi.unwrap_or(0.0);
            !(age > Self::STALE_AGE_SECS && phi < 0.1)
        });

        let mut seen = std::collections::HashSet::new();
        self.entries.sort_by_key(|e| std::cmp::Reverse(e.timestamp));
        self.entries.retain(|e| {
            // Lexical key only: entries were normalized when written, and the
            // full variant would `realpath` every persisted path — the daemon
            // runs this at boot (ContextLedger::load → prune) and stat-ing
            // stored paths under ~/Documents from a launchd process pops the
            // macOS TCC prompt (#356).
            let key = crate::core::pathutil::normalize_tool_path_lexical(&e.path);
            seen.insert(key)
        });

        if self.entries.len() > Self::MAX_LEDGER_ENTRIES {
            self.entries.sort_by(|a, b| {
                let pa = a.phi.unwrap_or(0.0);
                let pb = b.phi.unwrap_or(0.0);
                pb.partial_cmp(&pa).unwrap_or(std::cmp::Ordering::Equal)
            });
            self.entries.truncate(Self::MAX_LEDGER_ENTRIES);
        }

        self.rebuild_totals();
        before - self.entries.len()
    }

    fn rebuild_totals(&mut self) {
        self.total_tokens_sent = self.entries.iter().map(|e| e.sent_tokens).sum();
        self.total_tokens_saved = self
            .entries
            .iter()
            .map(|e| e.original_tokens.saturating_sub(e.sent_tokens))
            .sum();
    }

    #[must_use]
    pub fn load() -> Self {
        Self::load_for_agent("default")
    }

    #[must_use]
    pub fn load_for_agent(agent_id: &str) -> Self {
        let mut ledger: Self = ledger_path(agent_id)
            .ok()
            .and_then(|p| {
                let _lock = acquire_ledger_lock(&p);
                std::fs::read_to_string(p).ok()
            })
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if let Some((_model, window)) = crate::hook_handlers::load_detected_model() {
            ledger.window_size = window;
        }
        let pruned = ledger.prune();
        if pruned > 0 {
            ledger.save_for_agent(agent_id);
        }
        ledger
    }

    #[must_use]
    pub fn format_summary(&self) -> String {
        let pressure = self.pressure();
        format!(
            "CTX: {}/{} tokens ({:.0}%), {} files, ratio {:.2}, action: {:?}",
            self.total_tokens_sent,
            self.window_size,
            pressure.utilization * 100.0,
            self.entries.len(),
            self.compression_ratio(),
            pressure.recommendation,
        )
    }

    #[must_use]
    pub fn adjusted_total_saved(&self) -> isize {
        match crate::core::bounce_tracker::global().lock() {
            Ok(bt) => bt.adjusted_savings(self.total_tokens_saved),
            _ => self.total_tokens_saved as isize,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReinjectionAction {
    pub path: String,
    pub current_mode: String,
    pub new_mode: String,
    pub tokens_freed: usize,
}

#[derive(Debug, Clone)]
pub struct ReinjectionPlan {
    pub actions: Vec<ReinjectionAction>,
    pub total_tokens_freed: usize,
    pub new_utilization: f64,
}

impl ContextLedger {
    pub fn reinjection_plan(
        &self,
        intent: &super::intent_engine::StructuredIntent,
        target_utilization: f64,
    ) -> ReinjectionPlan {
        let current_util = self.total_tokens_sent as f64 / self.window_size as f64;
        if current_util <= target_utilization {
            return ReinjectionPlan {
                actions: Vec::new(),
                total_tokens_freed: 0,
                new_utilization: current_util,
            };
        }

        let tokens_to_free =
            self.total_tokens_sent - (self.window_size as f64 * target_utilization) as usize;

        let target_set: std::collections::HashSet<&str> = intent
            .targets
            .iter()
            .map(std::string::String::as_str)
            .collect();

        let mut candidates: Vec<(usize, &LedgerEntry)> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !target_set.iter().any(|t| e.path.contains(t)))
            .collect();

        candidates.sort_by(|a, b| {
            let a_phi = a.1.phi.unwrap_or(0.0);
            let b_phi = b.1.phi.unwrap_or(0.0);
            a_phi
                .partial_cmp(&b_phi)
                .unwrap_or_else(|| a.1.timestamp.cmp(&b.1.timestamp))
        });

        let mut actions = Vec::new();
        let mut freed = 0usize;

        for (_, entry) in &candidates {
            if freed >= tokens_to_free {
                break;
            }
            if let Some((new_mode, new_tokens)) = downgrade_mode(&entry.mode, entry.sent_tokens) {
                let saving = entry.sent_tokens.saturating_sub(new_tokens);
                if saving > 0 {
                    actions.push(ReinjectionAction {
                        path: entry.path.clone(),
                        current_mode: entry.mode.clone(),
                        new_mode,
                        tokens_freed: saving,
                    });
                    freed += saving;
                }
            }
        }

        let new_sent = self.total_tokens_sent.saturating_sub(freed);
        let new_utilization = new_sent as f64 / self.window_size as f64;

        ReinjectionPlan {
            actions,
            total_tokens_freed: freed,
            new_utilization,
        }
    }
}

fn downgrade_mode(current_mode: &str, current_tokens: usize) -> Option<(String, usize)> {
    match current_mode {
        "full" => Some(("signatures".to_string(), current_tokens / 5)),
        "aggressive" => Some(("signatures".to_string(), current_tokens / 3)),
        "signatures" => Some(("map".to_string(), current_tokens / 2)),
        "map" => Some(("reference".to_string(), current_tokens / 4)),
        _ => None,
    }
}

/// Resolve the Global-Workspace ignition z-score threshold (#6): the
/// `LEAN_CTX_GWT_IGNITION_Z` env override (must be > 0) wins, else the default
/// [`GWT_IGNITION_Z`]. Deterministic for a given environment.
fn ignition_z_threshold() -> f64 {
    std::env::var("LEAN_CTX_GWT_IGNITION_Z")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(GWT_IGNITION_Z)
}

impl Default for ContextLedger {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_ledger_is_empty() {
        let ledger = ContextLedger::new();
        assert_eq!(ledger.total_tokens_sent, 0);
        assert_eq!(ledger.entries.len(), 0);
        assert_eq!(ledger.pressure().recommendation, PressureAction::NoAction);
    }

    #[test]
    fn record_tracks_tokens() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("src/main.rs", "full", 500, 500);
        ledger.record("src/lib.rs", "signatures", 1000, 200);
        assert_eq!(ledger.total_tokens_sent, 700);
        assert_eq!(ledger.total_tokens_saved, 800);
        assert_eq!(ledger.entries.len(), 2);
    }

    #[test]
    fn ignition_broadcasts_high_salience_outlier() {
        // #6: an item far above the mean salience ignites and is pinned.
        let mut ledger = ContextLedger::with_window_size(100_000);
        for i in 0..5 {
            ledger.record(&format!("low{i}.rs"), "map", 100, 100);
        }
        ledger.record("hot.rs", "full", 100, 100);
        for e in &mut ledger.entries {
            e.phi = Some(if e.path == "hot.rs" { 0.95 } else { 0.1 });
        }
        let ignited = ledger.ignite_high_salience();
        assert_eq!(ignited, vec!["hot.rs".to_string()]);
        let hot = ledger.entries.iter().find(|e| e.path == "hot.rs").unwrap();
        assert_eq!(hot.state, Some(ContextState::Pinned));
    }

    #[test]
    fn ignition_skips_small_ledger() {
        // Below GWT_MIN_ENTRIES the distribution is too small — no ignition.
        let mut ledger = ContextLedger::with_window_size(100_000);
        ledger.record("a.rs", "full", 100, 100);
        ledger.entries[0].phi = Some(0.99);
        assert!(ledger.ignite_high_salience().is_empty());
    }

    #[test]
    fn ignition_is_deterministic() {
        // Determinism contract (#498): same Phi distribution → same ignitions.
        let build = || {
            let mut l = ContextLedger::with_window_size(100_000);
            for i in 0..5 {
                l.record(&format!("f{i}.rs"), "map", 100, 100);
            }
            for (i, e) in l.entries.iter_mut().enumerate() {
                e.phi = Some(if i == 0 { 0.95 } else { 0.1 });
            }
            l
        };
        let mut a = build();
        let mut b = build();
        assert_eq!(a.ignite_high_salience(), b.ignite_high_salience());
    }

    #[test]
    fn record_updates_existing_entry() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("src/main.rs", "full", 500, 500);
        ledger.record("src/main.rs", "signatures", 500, 100);
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.total_tokens_sent, 100);
        assert_eq!(ledger.total_tokens_saved, 400);
    }

    #[test]
    fn access_count_tracks_rereads() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("src/main.rs", "full", 500, 500);
        assert_eq!(ledger.entries[0].access_count, 1);
        ledger.record("src/main.rs", "signatures", 500, 100);
        ledger.record("src/main.rs", "map", 500, 50);
        assert_eq!(ledger.entries[0].access_count, 3);
        // A different file starts its own count.
        ledger.record("src/other.rs", "full", 200, 200);
        let other = ledger.entries.iter().find(|e| e.path == "src/other.rs");
        assert_eq!(other.map(|e| e.access_count), Some(1));
    }

    #[test]
    fn pressure_escalates() {
        let mut ledger = ContextLedger::with_window_size(1000);
        ledger.record("a.rs", "full", 600, 600);
        assert_eq!(
            ledger.pressure().recommendation,
            PressureAction::SuggestCompression
        );
        ledger.record("b.rs", "full", 200, 200);
        assert_eq!(
            ledger.pressure().recommendation,
            PressureAction::ForceCompression
        );
        ledger.record("c.rs", "full", 150, 150);
        assert_eq!(
            ledger.pressure().recommendation,
            PressureAction::EvictLeastRelevant
        );
    }

    #[test]
    fn compression_ratio_accurate() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("a.rs", "full", 1000, 1000);
        ledger.record("b.rs", "signatures", 1000, 200);
        let ratio = ledger.compression_ratio();
        assert!((ratio - 0.6).abs() < 0.01);
    }

    #[test]
    fn eviction_returns_oldest() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("old.rs", "full", 100, 100);
        std::thread::sleep(std::time::Duration::from_millis(10));
        ledger.record("new.rs", "full", 100, 100);
        let candidates = ledger.eviction_candidates(1);
        assert_eq!(candidates, vec!["old.rs"]);
    }

    #[test]
    fn remove_updates_totals() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("a.rs", "full", 500, 500);
        ledger.record("b.rs", "full", 300, 300);
        assert!(ledger.remove("a.rs"));
        assert_eq!(ledger.total_tokens_sent, 300);
        assert_eq!(ledger.entries.len(), 1);
        assert!(!ledger.remove("nonexistent.rs"));
    }

    #[test]
    fn reset_clears_everything() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("a.rs", "full", 500, 500);
        ledger.record("b.rs", "full", 300, 300);
        ledger.reset();
        assert_eq!(ledger.entries.len(), 0);
        assert_eq!(ledger.total_tokens_sent, 0);
        assert_eq!(ledger.total_tokens_saved, 0);
        assert_eq!(ledger.pressure().recommendation, PressureAction::NoAction);
    }

    #[test]
    fn evict_paths_removes_matching() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("a.rs", "full", 500, 500);
        ledger.record("b.rs", "full", 300, 300);
        ledger.record("c.rs", "full", 200, 200);
        let removed = ledger.evict_paths(&["a.rs", "c.rs", "nonexistent.rs"]);
        assert_eq!(removed, 2);
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.entries[0].path, "b.rs");
        assert_eq!(ledger.total_tokens_sent, 300);
    }

    #[test]
    fn mode_distribution_counts() {
        let mut ledger = ContextLedger::new();
        ledger.record("a.rs", "full", 100, 100);
        ledger.record("b.rs", "signatures", 100, 50);
        ledger.record("c.rs", "full", 100, 100);
        let dist = ledger.mode_distribution();
        assert_eq!(dist.get("full"), Some(&2));
        assert_eq!(dist.get("signatures"), Some(&1));
    }

    #[test]
    fn format_summary_includes_key_info() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("a.rs", "full", 500, 500);
        let summary = ledger.format_summary();
        assert!(summary.contains("500/10000"));
        assert!(summary.contains("1 files"));
    }

    #[test]
    fn reinjection_no_action_when_low_pressure() {
        use crate::core::intent_engine::StructuredIntent;

        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("a.rs", "full", 100, 100);
        let intent = StructuredIntent::from_query("fix bug in a.rs");
        let plan = ledger.reinjection_plan(&intent, 0.7);
        assert!(plan.actions.is_empty());
        assert_eq!(plan.total_tokens_freed, 0);
    }

    #[test]
    fn reinjection_downgrades_non_target_files() {
        use crate::core::intent_engine::StructuredIntent;

        let mut ledger = ContextLedger::with_window_size(1000);
        ledger.record("src/target.rs", "full", 400, 400);
        std::thread::sleep(std::time::Duration::from_millis(10));
        ledger.record("src/other.rs", "full", 400, 400);
        std::thread::sleep(std::time::Duration::from_millis(10));
        ledger.record("src/utils.rs", "full", 200, 200);

        let intent = StructuredIntent::from_query("fix bug in target.rs");
        let plan = ledger.reinjection_plan(&intent, 0.5);

        assert!(!plan.actions.is_empty());
        assert!(
            plan.actions.iter().all(|a| !a.path.contains("target")),
            "should not downgrade target file"
        );
        assert!(plan.total_tokens_freed > 0);
    }

    #[test]
    fn reinjection_preserves_targets() {
        use crate::core::intent_engine::StructuredIntent;

        let mut ledger = ContextLedger::with_window_size(1000);
        ledger.record("src/auth.rs", "full", 900, 900);
        let intent = StructuredIntent::from_query("fix bug in auth.rs");
        let plan = ledger.reinjection_plan(&intent, 0.5);
        assert!(
            plan.actions.is_empty(),
            "should not downgrade target files even under pressure"
        );
    }

    #[test]
    fn downgrade_mode_chain() {
        assert_eq!(
            downgrade_mode("full", 1000),
            Some(("signatures".to_string(), 200))
        );
        assert_eq!(
            downgrade_mode("signatures", 200),
            Some(("map".to_string(), 100))
        );
        assert_eq!(
            downgrade_mode("map", 100),
            Some(("reference".to_string(), 25))
        );
        assert_eq!(downgrade_mode("reference", 25), None);
    }

    #[test]
    fn record_assigns_item_id() {
        let mut ledger = ContextLedger::new();
        ledger.record("src/main.rs", "full", 500, 500);
        let entry = &ledger.entries[0];
        assert!(entry.id.is_some());
        assert_eq!(entry.id.as_ref().unwrap().as_str(), "file:src/main.rs");
    }

    #[test]
    fn record_sets_state_to_included() {
        let mut ledger = ContextLedger::new();
        ledger.record("src/main.rs", "full", 500, 500);
        assert_eq!(
            ledger.entries[0].state,
            Some(crate::core::context_field::ContextState::Included)
        );
    }

    #[test]
    fn record_generates_view_costs() {
        let mut ledger = ContextLedger::new();
        ledger.record("src/main.rs", "full", 5000, 5000);
        let vc = ledger.entries[0].view_costs.as_ref().unwrap();
        assert_eq!(vc.get(&crate::core::context_field::ViewKind::Full), 5000);
        assert_eq!(
            vc.get(&crate::core::context_field::ViewKind::Signatures),
            1000
        );
    }

    #[test]
    fn update_phi_works() {
        let mut ledger = ContextLedger::new();
        ledger.record("a.rs", "full", 100, 100);
        ledger.update_phi("a.rs", 0.85);
        assert_eq!(ledger.entries[0].phi, Some(0.85));
    }

    #[test]
    fn set_state_works() {
        let mut ledger = ContextLedger::new();
        ledger.record("a.rs", "full", 100, 100);
        ledger.set_state("a.rs", crate::core::context_field::ContextState::Pinned);
        assert_eq!(
            ledger.entries[0].state,
            Some(crate::core::context_field::ContextState::Pinned)
        );
    }

    #[test]
    fn items_by_state_filters() {
        let mut ledger = ContextLedger::new();
        ledger.record("a.rs", "full", 100, 100);
        ledger.record("b.rs", "full", 100, 100);
        ledger.set_state("b.rs", crate::core::context_field::ContextState::Excluded);
        let included = ledger.items_by_state(crate::core::context_field::ContextState::Included);
        assert_eq!(included.len(), 1);
        assert_eq!(included[0].path, "a.rs");
    }

    #[test]
    fn eviction_by_phi_prefers_low_phi() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("high.rs", "full", 100, 100);
        ledger.update_phi("high.rs", 0.9);
        ledger.record("low.rs", "full", 100, 100);
        ledger.update_phi("low.rs", 0.1);
        let candidates = ledger.eviction_candidates_by_phi(1);
        assert_eq!(candidates, vec!["low.rs"]);
    }

    #[test]
    fn eviction_by_phi_skips_pinned() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("pinned.rs", "full", 100, 100);
        ledger.update_phi("pinned.rs", 0.01);
        ledger.set_state(
            "pinned.rs",
            crate::core::context_field::ContextState::Pinned,
        );
        ledger.record("normal.rs", "full", 100, 100);
        ledger.update_phi("normal.rs", 0.5);
        let candidates = ledger.eviction_candidates_by_phi(1);
        assert_eq!(candidates, vec!["normal.rs"]);
    }

    #[test]
    fn mark_stale_by_hash_detects_change() {
        let mut ledger = ContextLedger::new();
        ledger.record("a.rs", "full", 100, 100);
        ledger.entries[0].source_hash = Some("hash_v1".to_string());
        ledger.mark_stale_by_hash("a.rs", "hash_v2");
        assert_eq!(
            ledger.entries[0].state,
            Some(crate::core::context_field::ContextState::Stale)
        );
    }

    #[test]
    fn find_by_id_works() {
        let mut ledger = ContextLedger::new();
        ledger.record("src/lib.rs", "full", 100, 100);
        let id = crate::core::context_field::ContextItemId::from_file("src/lib.rs");
        assert!(ledger.find_by_id(&id).is_some());
    }

    #[test]
    fn phi_recomputed_on_reread_not_sticky() {
        // #2: Phi must track time-variant salience, not freeze on first read.
        let _env = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.path());

        let mut ledger = ContextLedger::with_window_size(100_000);
        // First read carries a task whose keyword matches the path → relevance up.
        ledger.record_with_task(
            "src/authentication.rs",
            "full",
            2000,
            2000,
            Some("fix authentication login flow"),
        );
        let phi_with_task = ledger.entries[0].phi.unwrap();
        // Re-read with no task context → relevance collapses, so the blended Phi
        // must move. Before the fix this stayed frozen at the first value.
        ledger.record_with_task("src/authentication.rs", "full", 2000, 2000, None);
        let phi_after = ledger.entries[0].phi.unwrap();
        assert_ne!(
            phi_with_task, phi_after,
            "Phi must be recomputed on re-read (#2)"
        );
        assert!(
            phi_after < phi_with_task,
            "dropping task relevance should lower Phi ({phi_with_task} -> {phi_after})"
        );
    }

    #[test]
    fn upsert_sets_source_hash_and_kind() {
        let mut ledger = ContextLedger::new();
        ledger.upsert(
            "src/main.rs",
            "full",
            500,
            500,
            Some("sha256_abc"),
            crate::core::context_field::ContextKind::File,
            None,
        );
        let entry = &ledger.entries[0];
        assert_eq!(entry.source_hash.as_deref(), Some("sha256_abc"));
        assert_eq!(
            entry.kind,
            Some(crate::core::context_field::ContextKind::File)
        );
    }
}
