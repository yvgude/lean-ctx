use std::collections::HashMap;

use crate::core::context_field::{
    ContextItemId, ContextKind, ContextState, Provenance, ViewCosts, ViewKind,
};

use super::helpers::{
    DEFAULT_CONTEXT_WINDOW, GWT_MIN_ENTRIES, PHI_REREAD_ALPHA, acquire_ledger_lock,
    atomic_write_json, ledger_path,
};
use super::reinjection::ignition_z_threshold;
use super::types::{
    ContextLedger, ContextPressure, EvictOutcome, LedgerEntry, LedgerResolution, PressureAction,
};

impl ContextLedger {
    pub fn new() -> Self {
        Self {
            window_size: DEFAULT_CONTEXT_WINDOW,
            entries: Vec::new(),
            total_tokens_sent: 0,
            total_tokens_saved: 0,
            last_flush: None,
        }
    }

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

    pub fn record_shell(&mut self, command: &str, original_tokens: usize, sent_tokens: usize) {
        self.record_with_task(command, "shell", original_tokens, sent_tokens, None);
        if let Some(entry) = self.entries.iter_mut().find(|e| e.path == command) {
            entry.kind = Some(ContextKind::Shell);
        }
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
        let phi = ContextField::new().compute_phi(&signals);
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
        ContextField::new().compute_phi(&signals)
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

    /// Set the state for an entry. Accepts partial paths and basenames via
    /// [`Self::resolve_entry`] (#715).
    pub fn set_state(&mut self, path: &str, state: ContextState) {
        if let LedgerResolution::Unique(idx) = self.resolve_entry(path, None) {
            self.entries[idx].state = Some(state);
        }
    }

    /// Resolve a user-supplied target against the ledger (#715):
    /// exact match → project-root-relative → unambiguous suffix at a path
    /// component boundary. Entries are stored as normalized absolute paths
    /// (forward slashes); targets arrive as basenames, relative paths, or
    /// OS-native separators.
    pub fn resolve_entry(&self, target: &str, project_root: Option<&str>) -> LedgerResolution {
        let lex = crate::core::pathutil::normalize_tool_path_lexical(target);
        if lex.is_empty() {
            return LedgerResolution::NotFound;
        }

        // Stage 1 — exact: lexical form first (no FS access), then the fully
        // canonical form (resolves symlinks, matching what `record` stored).
        if let Some(idx) = self.entries.iter().position(|e| e.path == lex) {
            return LedgerResolution::Unique(idx);
        }
        let full = crate::core::pathutil::normalize_tool_path(target);
        if full != lex
            && let Some(idx) = self.entries.iter().position(|e| e.path == full)
        {
            return LedgerResolution::Unique(idx);
        }

        // Stage 2 — relative to the project root.
        if let Some(root) = project_root.filter(|r| !r.is_empty()) {
            let joined = format!(
                "{}/{}",
                root.trim_end_matches(['/', '\\']),
                lex.trim_start_matches('/')
            );
            let joined_lex = crate::core::pathutil::normalize_tool_path_lexical(&joined);
            if let Some(idx) = self.entries.iter().position(|e| e.path == joined_lex) {
                return LedgerResolution::Unique(idx);
            }
            let joined_full = crate::core::pathutil::normalize_tool_path(&joined_lex);
            if joined_full != joined_lex
                && let Some(idx) = self.entries.iter().position(|e| e.path == joined_full)
            {
                return LedgerResolution::Unique(idx);
            }
        }

        // Stage 3 — unambiguous suffix at a component boundary ("/…"), so
        // `main.rs` matches `src/main.rs` but never `domain.rs`.
        let suffix = format!("/{}", lex.trim_start_matches('/'));
        let matches: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.path.ends_with(&suffix))
            .map(|(idx, _)| idx)
            .collect();
        match matches.len() {
            1 => LedgerResolution::Unique(matches[0]),
            0 => LedgerResolution::NotFound,
            _ => LedgerResolution::Ambiguous(
                matches
                    .iter()
                    .map(|&idx| self.entries[idx].path.clone())
                    .collect(),
            ),
        }
    }

    /// Find an entry by its ContextItemId.
    pub fn find_by_id(&self, id: &ContextItemId) -> Option<&LedgerEntry> {
        self.entries.iter().find(|e| e.id.as_ref() == Some(id))
    }

    /// Get all entries with a specific state.
    pub fn items_by_state(&self, state: ContextState) -> Vec<&LedgerEntry> {
        self.entries
            .iter()
            .filter(|e| e.state == Some(state))
            .collect()
    }

    /// Eviction candidates ordered by Phi (lowest first), falling back to
    /// timestamp for entries without Phi scores.
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
        // Pinned/stale entries reduce eviction flexibility, so they nudge
        // pressure upward — but the nudge must stay bounded. Without a cap, a
        // long session where many entries end up Pinned (e.g. via GWT
        // ignition, #6) can alone drive utilization to 100% regardless of
        // actual token usage.
        const MAX_STATE_PRESSURE: f64 = 0.2;
        let effective_utilization =
            (utilization + (pinned_pressure + stale_penalty).min(MAX_STATE_PRESSURE)).min(1.0);

        // `remaining_tokens` is a literal token-budget figure consumed by
        // dashboards and the deficit-suggestion auto-loader
        // (`context_deficit.rs`) as "how much room is actually left" — it
        // must track real usage, not the heuristic-boosted
        // `effective_utilization`, or a heavily-pinned/stale session reports
        // 0 remaining (and silently starves auto-loading) while tokens of
        // real headroom are still free.
        let remaining = self.window_size.saturating_sub(self.total_tokens_sent);

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

    pub fn compression_ratio(&self) -> f64 {
        let total_original: usize = self.entries.iter().map(|e| e.original_tokens).sum();
        if total_original == 0 {
            return 1.0;
        }
        self.total_tokens_sent as f64 / total_original as f64
    }

    pub fn files_by_token_cost(&self) -> Vec<(String, usize)> {
        let mut costs: Vec<(String, usize)> = self
            .entries
            .iter()
            .map(|e| (e.path.clone(), e.sent_tokens))
            .collect();
        costs.sort_by_key(|b| std::cmp::Reverse(b.1));
        costs
    }

    pub fn mode_distribution(&self) -> HashMap<String, usize> {
        let mut dist: HashMap<String, usize> = HashMap::new();
        for entry in &self.entries {
            *dist.entry(entry.mode.clone()).or_insert(0) += 1;
        }
        dist
    }

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

    /// Remove one entry by target. Resolves partial paths and basenames
    /// (#715); an ambiguous target removes nothing.
    pub fn remove(&mut self, path: &str) -> bool {
        match self.resolve_entry(path, None) {
            LedgerResolution::Unique(idx) => {
                self.remove_at(idx);
                true
            }
            _ => false,
        }
    }

    fn remove_at(&mut self, idx: usize) {
        let entry = &self.entries[idx];
        self.total_tokens_sent = self.total_tokens_sent.saturating_sub(entry.sent_tokens);
        self.total_tokens_saved = self
            .total_tokens_saved
            .saturating_sub(entry.original_tokens.saturating_sub(entry.sent_tokens));
        self.entries.remove(idx);
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
    /// Targets resolve like [`Self::resolve_entry`] (#715).
    pub fn evict_paths(&mut self, paths: &[&str]) -> usize {
        self.evict_paths_resolved(paths, None)
            .iter()
            .filter(|o| o.resolved.is_some())
            .count()
    }

    /// Eviction with full per-target diagnostics (#715): resolves each target
    /// (exact → root-relative → unique suffix) and reports the canonical path
    /// it removed, or the ambiguous candidates, so callers can surface WHY
    /// nothing was evicted instead of a bare "Evicted 0/1".
    pub fn evict_paths_resolved(
        &mut self,
        paths: &[&str],
        project_root: Option<&str>,
    ) -> Vec<EvictOutcome> {
        paths
            .iter()
            .map(|target| match self.resolve_entry(target, project_root) {
                LedgerResolution::Unique(idx) => {
                    let resolved = self.entries[idx].path.clone();
                    self.remove_at(idx);
                    EvictOutcome {
                        target: (*target).to_string(),
                        resolved: Some(resolved),
                        ambiguous: Vec::new(),
                    }
                }
                LedgerResolution::Ambiguous(candidates) => EvictOutcome {
                    target: (*target).to_string(),
                    resolved: None,
                    ambiguous: candidates,
                },
                LedgerResolution::NotFound => EvictOutcome {
                    target: (*target).to_string(),
                    resolved: None,
                    ambiguous: Vec::new(),
                },
            })
            .collect()
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

    pub fn load() -> Self {
        Self::load_for_agent("default")
    }

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
        // #715 migration: older Windows builds persisted `\`-separated paths;
        // resolution and dedup assume the forward-slash canonical form.
        // Lexical only — no FS access on persisted paths (TCC, #356).
        let mut migrated = false;
        for entry in &mut ledger.entries {
            let normalized = crate::core::pathutil::normalize_tool_path_lexical(&entry.path);
            if normalized != entry.path {
                entry.path = normalized;
                migrated = true;
            }
        }
        let pruned = ledger.prune();
        if pruned > 0 || migrated {
            ledger.save_for_agent(agent_id);
        }
        ledger
    }

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

    pub fn adjusted_total_saved(&self) -> isize {
        match crate::core::bounce_tracker::global().lock() {
            Ok(bt) => bt.adjusted_savings(self.total_tokens_saved),
            _ => self.total_tokens_saved as isize,
        }
    }
}
