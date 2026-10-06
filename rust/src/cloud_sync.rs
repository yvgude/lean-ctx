use crate::core::config::Config;

/// Outcome of one background Personal-Cloud auto-push (GL #384).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoSyncOutcome {
    /// At least one surface pushed (or there was nothing to push).
    Synced,
    /// The server gated sync behind Pro (HTTP 402) — stop for today.
    Gated,
    /// The server rejected our credential (HTTP 401) — the key this machine
    /// holds is gone. Retrying cannot fix it, so this is the one outcome that
    /// must be *said out loud*: the user has to log in again.
    Unauthenticated,
    /// Every push failed without a 402 or 401 (offline / server down) — try
    /// again at the next opportunity, do not consume today's slot.
    NetworkFailure,
}

/// Whether the auto-sync should run now: opt-in flag, logged in, and not
/// already synced today (the debounce). Pure for unit testing.
#[must_use]
pub fn should_auto_sync(
    auto_sync: bool,
    logged_in: bool,
    last_auto_sync: Option<&str>,
    today: &str,
) -> bool {
    auto_sync && logged_in && last_auto_sync != Some(today)
}

/// Whether the background index push should run for this project (GL #392):
/// separate opt-in, logged in, a local index actually exists, and this
/// project hasn't pushed today. Pure for unit testing.
#[must_use]
pub fn should_auto_push_index(
    auto_index: bool,
    logged_in: bool,
    local_index_exists: bool,
    last_push_for_project: Option<&str>,
    today: &str,
) -> bool {
    auto_index && logged_in && local_index_exists && last_push_for_project != Some(today)
}

/// Whether an outcome should consume today's auto-sync slot. Only a network
/// failure leaves it open — a Pro gate or a dead credential will not resolve
/// by retrying in ten minutes, and each is announced once per process.
#[must_use]
pub fn consumes_daily_slot(outcome: AutoSyncOutcome) -> bool {
    outcome != AutoSyncOutcome::NetworkFailure
}

/// Classify per-surface push results into one [`AutoSyncOutcome`]. A 401
/// anywhere wins — a revoked credential makes every other signal moot and is
/// the only failure the user must act on. Then a 402 (the account is gated);
/// otherwise total failure means the network is down; anything else counts as
/// synced.
#[must_use]
pub fn classify_outcomes(results: &[Result<(), String>]) -> AutoSyncOutcome {
    if results
        .iter()
        .any(|r| r.as_ref().is_err_and(|e| e.contains("401")))
    {
        return AutoSyncOutcome::Unauthenticated;
    }
    if results
        .iter()
        .any(|r| r.as_ref().is_err_and(|e| e.contains("402")))
    {
        return AutoSyncOutcome::Gated;
    }
    if !results.is_empty() && results.iter().all(Result::is_err) {
        return AutoSyncOutcome::NetworkFailure;
    }
    AutoSyncOutcome::Synced
}

/// The only state `cloud_background_tasks` persists: the per-surface "last
/// done" stamps produced during this run.
///
/// Collected while the network work happens **outside** any lock, then applied
/// to a freshly loaded config inside one short locked closure. Keeping this a
/// delta instead of a mutated full `Config` is what stops the background task
/// from writing back a snapshot taken before the network I/O and thereby
/// reverting a telemetry preference the user changed in the meantime
/// (LR-TEL-01).
#[derive(Debug, Default, Clone)]
pub(crate) struct CloudBackgroundDelta {
    pub(crate) last_heartbeat: Option<String>,
    pub(crate) last_sync: Option<String>,
    pub(crate) last_gain_sync: Option<String>,
    pub(crate) last_model_pull: Option<String>,
    pub(crate) last_auto_sync: Option<String>,
    pub(crate) last_index_push: Vec<(String, String)>,
}

impl CloudBackgroundDelta {
    /// Nothing was produced this run, so skip the write — and the lock — entirely.
    pub(crate) fn is_empty(&self) -> bool {
        self.last_heartbeat.is_none()
            && self.last_sync.is_none()
            && self.last_gain_sync.is_none()
            && self.last_model_pull.is_none()
            && self.last_auto_sync.is_none()
            && self.last_index_push.is_empty()
    }

    /// Applies only the stamps this run produced. Every other field of `config`
    /// — notably the whole `telemetry` preference triple — is left exactly as it
    /// was loaded from disk, so a concurrent opt-out survives.
    pub(crate) fn apply(&self, config: &mut Config) {
        // Pure field assignment: this runs while the config write lock is held.
        if let Some(bucket) = &self.last_heartbeat {
            config.telemetry.last_heartbeat = Some(bucket.clone());
        }
        if let Some(day) = &self.last_sync {
            config.cloud.last_sync = Some(day.clone());
        }
        if let Some(day) = &self.last_gain_sync {
            config.cloud.last_gain_sync = Some(day.clone());
        }
        if let Some(day) = &self.last_model_pull {
            config.cloud.last_model_pull = Some(day.clone());
        }
        if let Some(day) = &self.last_auto_sync {
            config.cloud.last_auto_sync = Some(day.clone());
        }
        for (project_hash, day) in &self.last_index_push {
            config
                .cloud
                .last_index_push
                .insert(project_hash.clone(), day.clone());
        }
    }
}

/// Network budget for the send that runs as the MCP server exits.
pub const EXIT_TELEMETRY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const BACKGROUND_TELEMETRY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Whether this process may collect and send telemetry right now. Fails closed:
/// an unreadable config never counts as consent.
fn telemetry_send_eligible() -> bool {
    let Ok(config) = Config::try_load_global() else {
        return false;
    };
    let do_not_track = std::env::var("DO_NOT_TRACK").ok();
    let telemetry_override = std::env::var("LEAN_CTX_TELEMETRY").ok();
    config
        .telemetry
        .send_eligible(do_not_track.as_deref(), telemetry_override.as_deref())
}

/// Send the cumulative telemetry totals if the aggregate admits a send now.
/// Returns the acknowledged bucket. Admission (daily cap, spacing, "nothing
/// new") is decided under the aggregate lock, so callers may call freely.
pub fn send_telemetry(trigger: crate::core::telemetry_aggregate::SendTrigger) -> Option<String> {
    if !telemetry_send_eligible() {
        return None;
    }
    // Persist first so the counters survive even when no send is admitted.
    if let Err(error) = crate::core::telemetry_aggregate::persist_process_counters() {
        tracing::debug!("telemetry counters not persisted: {error}");
    }
    let lease = match crate::core::telemetry_aggregate::begin_send(trigger) {
        Ok(lease) => lease,
        Err(reason) => {
            tracing::debug!("telemetry send skipped: {reason}");
            return None;
        }
    };
    let batch = lease.batch().clone();
    let timeout = match trigger {
        crate::core::telemetry_aggregate::SendTrigger::Exit => EXIT_TELEMETRY_TIMEOUT,
        crate::core::telemetry_aggregate::SendTrigger::Periodic => BACKGROUND_TELEMETRY_TIMEOUT,
    };
    if let Err(error) = crate::cloud_client::telemetry_v2_batch_with_timeout(&batch, timeout) {
        tracing::debug!("telemetry send failed, batch kept for retry: {error}");
        return None;
    }
    let installation_id = batch.events.first()?.installation_id.clone();
    let payload = serde_json::to_vec(&batch).ok()?;
    use sha2::Digest;
    let record = crate::core::telemetry_ledger::HeartbeatRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        installation_id,
        version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        schema_version: batch.schema_version,
        event_names: batch
            .events
            .iter()
            .map(|event| event.event.name().to_string())
            .collect(),
        payload_hash: hex::encode(sha2::Sha256::digest(payload)),
        endpoint: telemetry_ledger_endpoint(),
        status: "success".to_string(),
    };
    // Without a ledger entry the send stays pending and is retried with the
    // same bytes; the server replaces per-day rows, so that is harmless.
    if let Err(error) = crate::core::telemetry_ledger::append(&record) {
        tracing::debug!("telemetry ledger append failed: {error}");
        return None;
    }
    if let Err(error) = lease.commit() {
        tracing::debug!("telemetry acknowledgement failed: {error}");
        return None;
    }
    batch
        .events
        .first()
        .map(|event| event.timestamp_bucket.clone())
}

/// How often the daemon offers the day's totals. The aggregate's admission
/// (daily cap, growing spacing, "nothing new") decides whether a send happens.
const DAEMON_TELEMETRY_INTERVAL: std::time::Duration = std::time::Duration::from_hours(1);
/// Delay before the daemon's first offer so its startup stays light.
const DAEMON_TELEMETRY_INITIAL_DELAY: std::time::Duration = std::time::Duration::from_mins(2);

/// Spawn the daemon's telemetry loop (must run inside a Tokio runtime).
///
/// Without it, sends happen only inside an MCP server (tool-call ticks and
/// exit), so installs that use lean-ctx through hooks, the CLI or the proxy
/// were never counted. The daemon is the one long-lived process setup starts;
/// it also forwards the counters other processes persisted. Eligibility is
/// re-checked on every tick, so an opt-out takes effect without a restart.
pub(crate) fn spawn_daemon_telemetry() {
    tokio::spawn(async {
        tokio::time::sleep(DAEMON_TELEMETRY_INITIAL_DELAY).await;
        loop {
            let _ = tokio::task::spawn_blocking(daemon_telemetry_tick).await;
            tokio::time::sleep(DAEMON_TELEMETRY_INTERVAL).await;
        }
    });
}

fn daemon_telemetry_tick() {
    if !telemetry_send_eligible() {
        return;
    }
    if let Err(error) = crate::core::telemetry_aggregate::record_current_version() {
        tracing::debug!("telemetry version aggregate unavailable: {error}");
    }
    send_telemetry(crate::core::telemetry_aggregate::SendTrigger::Periodic);
}

pub fn cloud_background_tasks() {
    // Decision snapshot only. Read global-only so the daily background save
    // never leaks a project-local override into the global config (#443), and
    // never written back: the persist below re-loads under the lock and applies
    // `delta` instead (LR-TEL-01). This also keeps edits made while the pass is
    // busy on the network (#1934).
    let config = Config::load_global();
    let mut delta = CloudBackgroundDelta::default();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    let already_synced = config
        .cloud
        .last_sync
        .as_deref()
        .is_some_and(|d| d == today);
    let already_gain_synced = config
        .cloud
        .last_gain_sync
        .as_deref()
        .is_some_and(|d| d == today);
    let already_pulled = config
        .cloud
        .last_model_pull
        .as_deref()
        .is_some_and(|d| d == today);

    // Anonymous usage telemetry: cumulative daily totals, resent as they grow.
    // The acknowledged bucket goes into `delta`, never into this snapshot, so
    // a concurrent opt-out is not reverted by the persist below (LR-TEL-01).
    if telemetry_send_eligible() {
        if let Err(error) = crate::core::telemetry_aggregate::record_current_version() {
            tracing::debug!("telemetry version aggregate unavailable: {error}");
        }
        if let Some(bucket) =
            send_telemetry(crate::core::telemetry_aggregate::SendTrigger::Periodic)
        {
            delta.last_heartbeat = Some(bucket);
        }
    }

    if crate::cloud_client::is_logged_in() {
        if config.cloud.sync_stats_enabled && !already_synced {
            let store = crate::core::stats::load();
            let entries = build_sync_entries(&store);
            if !entries.is_empty() {
                let result = crate::cloud_client::sync_stats(&entries);
                record_sync_telemetry(&result);
                if result.is_ok() {
                    delta.last_sync = Some(today.clone());
                }
            }
        }

        if config.cloud.sync_gain_enabled && !already_gain_synced {
            let engine = crate::core::gain::GainEngine::load();
            let summary = engine.summary(None);
            let trend = match summary.score.trend {
                crate::core::gain::gain_score::Trend::Rising => "rising",
                crate::core::gain::gain_score::Trend::Stable => "stable",
                crate::core::gain::gain_score::Trend::Declining => "declining",
            };
            let entry = serde_json::json!({
                "recorded_at": format!("{today}T00:00:00Z"),
                "total": summary.score.total as f64,
                "compression": summary.score.compression as f64,
                "cost_efficiency": summary.score.cost_efficiency as f64,
                "quality": summary.score.quality as f64,
                "consistency": summary.score.consistency as f64,
                "navigability": summary.score.navigability as f64,
                "trend": trend,
                "avoided_usd": summary.avoided_usd,
                "tool_spend_usd": summary.tool_spend_usd,
                "model_key": summary.model.model_key,
            });
            let result = crate::cloud_client::push_gain(&[entry]);
            record_sync_telemetry(&result);
            if result.is_ok() {
                delta.last_gain_sync = Some(today.clone());
            }
        }

        if config.cloud.sync_models_enabled && !already_pulled {
            let result = crate::cloud_client::pull_cloud_models().and_then(|data| {
                crate::cloud_client::save_cloud_models(&data).map_err(|error| error.to_string())
            });
            record_sync_telemetry(&result);
            if result.is_ok() {
                delta.last_model_pull = Some(today.clone());
            }
        }

        // Opt-in Personal-Cloud auto-push (GL #384): silent, once per day,
        // offline-tolerant. A network failure leaves the slot open so the
        // next background cycle retries; a Pro gate consumes it (one quiet
        // attempt per day on a Community account, never error spam).
        if should_auto_sync(
            config.cloud.auto_sync,
            true,
            config.cloud.last_auto_sync.as_deref(),
            &today,
        ) && consumes_daily_slot(auto_sync_personal_cloud())
        {
            delta.last_auto_sync = Some(today.clone());
        }

        // Opt-in hosted-index auto-push (GL #392): once per project per day,
        // only when a local index exists. Quota/Pro rejections consume the
        // slot (one quiet attempt per day); network failures leave it open.
        if let Ok(root) = std::env::current_dir() {
            let project_hash = crate::core::index_namespace::namespace_hash(&root);
            if should_auto_push_index(
                config.cloud.auto_index,
                true,
                crate::core::index_bundle::local_index_present(&root),
                config
                    .cloud
                    .last_index_push
                    .get(&project_hash)
                    .map(String::as_str),
                &today,
            ) {
                let result = crate::cloud_client::push_index_bundle(&root);
                record_sync_telemetry(&result);
                match result {
                    Ok((hash, bytes)) => {
                        tracing::debug!(project = %hash, bytes, "auto-index: pushed");
                        delta.last_index_push.push((project_hash, today.clone()));
                    }
                    Err(e) if e.contains("Pro") || e.contains("Quota") => {
                        tracing::debug!(error = %e, "auto-index: gated, retry tomorrow");
                        delta.last_index_push.push((project_hash, today.clone()));
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "auto-index: push failed, slot stays open");
                    }
                }
            }
        }
    }

    if let Err(e) = persist_background_delta(&delta) {
        tracing::warn!("could not persist cloud background state: {e}");
    }
}

/// Persist step of [`cloud_background_tasks`]: resolves the global config path
/// and delegates to [`persist_background_delta_at`].
fn persist_background_delta(
    delta: &CloudBackgroundDelta,
) -> Result<(), crate::core::error::LeanCtxError> {
    let path = Config::path().ok_or_else(|| {
        crate::core::error::LeanCtxError::Config("cannot determine home directory".to_string())
    })?;
    persist_background_delta_at(&path, delta)
}

/// Path-parameterized core of the persist step — the production code path,
/// shared with its regression test.
///
/// Re-loads the config from disk under the write lock and applies only the
/// stamps this run produced, so a telemetry choice committed during the network
/// work above is the base we mutate rather than something we overwrite. All
/// network work has already finished; nothing here does I/O beyond the guarded
/// config write.
pub(crate) fn persist_background_delta_at(
    path: &std::path::Path,
    delta: &CloudBackgroundDelta,
) -> Result<(), crate::core::error::LeanCtxError> {
    if delta.is_empty() {
        return Ok(());
    }
    Config::update_global_at(path, |on_disk| delta.apply(on_disk)).map(|_| ())
}

fn telemetry_ledger_endpoint() -> String {
    let raw =
        std::env::var("LEAN_CTX_API_URL").unwrap_or_else(|_| "https://api.leanctx.com".to_string());
    sanitized_telemetry_endpoint(&raw)
}

fn sanitized_telemetry_endpoint(raw: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(raw) else {
        return "https://api.leanctx.com/api/telemetry/v2/batch".to_string();
    };
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return "https://api.leanctx.com/api/telemetry/v2/batch".to_string();
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.set_path("/api/telemetry/v2/batch");
    url.to_string()
}

/// Push every Personal-Cloud surface silently (background variant of
/// `lean-ctx sync`'s interactive flow — tracing instead of stdout).
fn auto_sync_personal_cloud() -> AutoSyncOutcome {
    let store = crate::core::stats::load();
    let mut results: Vec<Result<(), String>> = Vec::new();

    let mut push = |label: &str, result: Result<String, String>| {
        record_sync_telemetry(&result);
        match result {
            Ok(_) => {
                tracing::debug!(surface = label, "auto-sync: pushed");
                results.push(Ok(()));
            }
            Err(e) => {
                tracing::debug!(surface = label, error = %e, "auto-sync: push failed");
                results.push(Err(e));
            }
        }
    };

    let commands = collect_command_entries(&store);
    if !commands.is_empty() {
        push("commands", crate::cloud_client::push_commands(&commands));
    }
    let cep = collect_cep_entries(&store);
    if !cep.is_empty() {
        push("cep", crate::cloud_client::push_cep(&cep));
    }
    let knowledge = collect_knowledge_entries();
    if !knowledge.is_empty() {
        push("knowledge", crate::cloud_client::push_knowledge(&knowledge));
    }
    let gotchas = collect_gotcha_entries();
    if !gotchas.is_empty() {
        push("gotchas", crate::cloud_client::push_gotchas(&gotchas));
    }
    let feedback = collect_feedback_entries();
    if !feedback.is_empty() {
        push("feedback", crate::cloud_client::push_feedback(&feedback));
    }

    let outcome = classify_outcomes(&results);
    tracing::info!(
        ?outcome,
        surfaces = results.len(),
        "personal-cloud auto-sync done"
    );

    static GATE_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if outcome == AutoSyncOutcome::Gated
        && !GATE_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        eprintln!(
            "\n  \x1b[33m⚠\x1b[0m  Personal Cloud sync requires Pro. \
             Your local data is safe.\n  \
             Unlock: lean-ctx cloud upgrade --plan pro ($9/mo)\n"
        );
    }

    // A dead credential used to look exactly like being offline: silent, and
    // retried forever. Say it once, and say what fixes it.
    static AUTH_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if outcome == AutoSyncOutcome::Unauthenticated
        && !AUTH_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        eprintln!(
            "\n  \x1b[33m⚠\x1b[0m  Personal Cloud sync is signed out on this machine \
             — the server rejected this device's key.\n  \
             Your local data is safe, and nothing on the server was lost.\n  \
             Sign in again to resume syncing: lean-ctx login\n"
        );
    }

    outcome
}

fn record_sync_telemetry<T>(result: &Result<T, String>) {
    if let Err(error) = crate::core::telemetry_aggregate::record_sync_result(result.is_ok()) {
        tracing::debug!("telemetry sync aggregate unavailable: {error}");
    }
}

pub fn build_sync_entries(store: &crate::core::stats::StatsStore) -> Vec<serde_json::Value> {
    let mut entries = Vec::new();
    let cep = &store.cep;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    let mut cep_cache_by_day: std::collections::HashMap<String, (u64, u64)> =
        std::collections::HashMap::new();
    for s in &cep.scores {
        if let Some(date) = s.timestamp.get(..10) {
            let entry = cep_cache_by_day.entry(date.to_string()).or_default();
            let calls = s.tool_calls.max(1);
            let hits = (calls as f64 * s.cache_hit_rate as f64 / 100.0).round() as u64;
            entry.0 += calls;
            entry.1 += hits;
        }
    }

    let mut mcp_saved_total = 0u64;
    for (cmd, s) in &store.commands {
        if cmd.starts_with("ctx_") {
            mcp_saved_total += s.input_tokens.saturating_sub(s.output_tokens);
        }
    }
    let global_saved = store
        .total_input_tokens
        .saturating_sub(store.total_output_tokens)
        .max(1);
    let mcp_ratio = mcp_saved_total as f64 / global_saved as f64;

    for day in &store.daily {
        let tokens_original = day.input_tokens;
        let tokens_compressed = day.output_tokens;
        let tokens_saved = tokens_original.saturating_sub(tokens_compressed);
        let (day_calls, day_hits) = cep_cache_by_day.get(&day.date).copied().unwrap_or((0, 0));
        let day_mcp_saved = (tokens_saved as f64 * mcp_ratio).round() as u64;
        let day_hook_saved = tokens_saved.saturating_sub(day_mcp_saved);
        entries.push(serde_json::json!({
            "date": day.date,
            "tokens_original": tokens_original,
            "tokens_compressed": tokens_compressed,
            "tokens_saved": tokens_saved,
            "mcp_tokens_saved": day_mcp_saved,
            "hook_tokens_saved": day_hook_saved,
            "tool_calls": day.commands,
            "cache_hits": day_hits,
            "cache_misses": day_calls.saturating_sub(day_hits),
        }));
    }

    let has_today = entries.iter().any(|e| e["date"].as_str() == Some(&today));
    if !has_today && (cep.total_tokens_original > 0 || store.total_commands > 0) {
        let today_saved = cep
            .total_tokens_original
            .saturating_sub(cep.total_tokens_compressed);
        let today_mcp = (today_saved as f64 * mcp_ratio).round() as u64;
        entries.push(serde_json::json!({
            "date": today,
            "tokens_original": cep.total_tokens_original,
            "tokens_compressed": cep.total_tokens_compressed,
            "tokens_saved": today_saved,
            "mcp_tokens_saved": today_mcp,
            "hook_tokens_saved": today_saved.saturating_sub(today_mcp),
            "tool_calls": store.total_commands,
            "cache_hits": cep.total_cache_hits,
            "cache_misses": cep.total_cache_reads.saturating_sub(cep.total_cache_hits),
        }));
    }

    entries
}

// ── Personal-Cloud surface collectors ────────────────────────────────────────
// Shared by the interactive `lean-ctx sync` flow and the background auto-sync
// (GL #384): pure local reads, no network, no stdout.

pub fn collect_knowledge_entries() -> Vec<serde_json::Value> {
    let Ok(data_dir) = crate::core::paths::data_dir() else {
        return Vec::new();
    };
    let knowledge_dir = data_dir.join("knowledge");
    if !knowledge_dir.is_dir() {
        return Vec::new();
    }

    let mut entries = Vec::new();

    for project_entry in std::fs::read_dir(&knowledge_dir).into_iter().flatten() {
        let Ok(project_entry) = project_entry else {
            continue;
        };
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }

        for file_entry in std::fs::read_dir(&project_path).into_iter().flatten() {
            let Ok(file_entry) = file_entry else { continue };
            let file_path = file_entry.path();
            if file_path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(data) = std::fs::read_to_string(&file_path) else {
                continue;
            };
            let parsed: serde_json::Value = match serde_json::from_str(&data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if let Some(facts) = parsed["facts"].as_array() {
                for fact in facts {
                    let cat = fact["category"].as_str().unwrap_or("general");
                    let key = fact["key"].as_str().unwrap_or("");
                    let val = fact["value"]
                        .as_str()
                        .or_else(|| fact["description"].as_str())
                        .unwrap_or("");
                    if !key.is_empty() {
                        entries.push(serde_json::json!({
                            "category": cat,
                            "key": key,
                            "value": val,
                        }));
                    }
                }
            }

            if let Some(gotchas) = parsed["gotchas"].as_array() {
                for g in gotchas {
                    let pattern = g["pattern"].as_str().unwrap_or("");
                    let fix = g["fix"].as_str().unwrap_or("");
                    if !pattern.is_empty() {
                        entries.push(serde_json::json!({
                            "category": "gotcha",
                            "key": pattern,
                            "value": fix,
                        }));
                    }
                }
            }
        }
    }

    entries
}

pub fn collect_command_entries(store: &crate::core::stats::StatsStore) -> Vec<serde_json::Value> {
    store
        .commands
        .iter()
        .map(|(name, stats)| {
            let tokens_saved = stats.input_tokens.saturating_sub(stats.output_tokens);
            serde_json::json!({
                "command": name,
                "source": if name.starts_with("ctx_") { "mcp" } else { "hook" },
                "count": stats.count,
                "input_tokens": stats.input_tokens,
                "output_tokens": stats.output_tokens,
                "tokens_saved": tokens_saved,
            })
        })
        .collect()
}

fn complexity_to_float(s: &str) -> f64 {
    match s.to_lowercase().as_str() {
        "trivial" => 0.1,
        "simple" => 0.3,
        "moderate" => 0.5,
        "complex" => 0.7,
        "architectural" => 0.9,
        other => other.parse::<f64>().unwrap_or(0.5),
    }
}

pub fn collect_cep_entries(store: &crate::core::stats::StatsStore) -> Vec<serde_json::Value> {
    store
        .cep
        .scores
        .iter()
        .map(|s| {
            serde_json::json!({
                "recorded_at": s.timestamp,
                "score": s.score as f64 / 100.0,
                "cache_hit_rate": s.cache_hit_rate as f64 / 100.0,
                "mode_diversity": s.mode_diversity as f64 / 100.0,
                "compression_rate": s.compression_rate as f64 / 100.0,
                "tool_calls": s.tool_calls,
                "tokens_saved": s.tokens_saved,
                "complexity": complexity_to_float(&s.complexity),
            })
        })
        .collect()
}

pub fn collect_gotcha_entries() -> Vec<serde_json::Value> {
    let mut all_gotchas = crate::core::gotcha_tracker::load_universal_gotchas();

    if let Ok(knowledge_dir) = crate::core::paths::data_dir().map(|d| d.join("knowledge"))
        && let Ok(entries) = std::fs::read_dir(&knowledge_dir)
    {
        for entry in entries.flatten() {
            let gotcha_path = entry.path().join("gotchas.json");
            if gotcha_path.exists()
                && let Ok(content) = std::fs::read_to_string(&gotcha_path)
                && let Ok(store) =
                    serde_json::from_str::<crate::core::gotcha_tracker::GotchaStore>(&content)
            {
                for g in store.gotchas {
                    if !all_gotchas
                        .iter()
                        .any(|existing| existing.trigger == g.trigger)
                    {
                        all_gotchas.push(g);
                    }
                }
            }
        }
    }

    all_gotchas
        .iter()
        .map(|g| {
            serde_json::json!({
                "pattern": g.trigger,
                "fix": g.resolution,
                "severity": format!("{:?}", g.severity).to_lowercase(),
                "category": format!("{:?}", g.category).to_lowercase(),
                "occurrences": g.occurrences,
                "prevented_count": g.prevented_count,
                "confidence": g.confidence,
            })
        })
        .collect()
}

pub fn collect_feedback_entries() -> Vec<serde_json::Value> {
    let store = crate::core::feedback::FeedbackStore::load();
    store
        .learned_thresholds
        .iter()
        .map(|(lang, thresholds)| {
            serde_json::json!({
                "language": lang,
                "entropy": thresholds.entropy,
                "jaccard": thresholds.jaccard,
                "sample_count": thresholds.sample_count,
                "avg_efficiency": thresholds.avg_efficiency,
            })
        })
        .collect()
}

pub fn collect_contribute_entries() -> Vec<serde_json::Value> {
    let mut entries = Vec::new();

    if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
        let mode_stats_path = data_dir.join("mode_stats.json");
        if let Ok(data) = std::fs::read_to_string(&mode_stats_path)
            && let Ok(predictor) = serde_json::from_str::<serde_json::Value>(&data)
            && let Some(history) = predictor["history"].as_object()
        {
            for (_key, outcomes) in history {
                if let Some(arr) = outcomes.as_array() {
                    for outcome in arr.iter().rev().take(3) {
                        let ext = outcome["ext"].as_str().unwrap_or("unknown");
                        let mode = outcome["mode"].as_str().unwrap_or("full");
                        let t_in = outcome["tokens_in"].as_u64().unwrap_or(0);
                        let t_out = outcome["tokens_out"].as_u64().unwrap_or(0);
                        let ratio = if t_in > 0 {
                            1.0 - t_out as f64 / t_in as f64
                        } else {
                            0.0
                        };
                        let bucket = match t_in {
                            0..=500 => "0-500",
                            501..=2000 => "500-2k",
                            2001..=10000 => "2k-10k",
                            _ => "10k+",
                        };
                        entries.push(serde_json::json!({
                            "file_ext": format!(".{ext}"),
                            "size_bucket": bucket,
                            "best_mode": mode,
                            "compression_ratio": (ratio * 100.0).round() / 100.0,
                        }));
                        if entries.len() >= 200 {
                            return entries;
                        }
                    }
                }
            }
        }
    }

    if entries.is_empty() {
        let stats_data = crate::core::stats::format_gain_json();
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&stats_data) {
            let original = parsed["cep"]["total_tokens_original"].as_u64().unwrap_or(0);
            let compressed = parsed["cep"]["total_tokens_compressed"]
                .as_u64()
                .unwrap_or(0);
            let ratio = if original > 0 {
                1.0 - compressed as f64 / original as f64
            } else {
                0.0
            };
            if let Some(modes) = parsed["cep"]["modes"].as_object() {
                let read_modes = [
                    "full",
                    "map",
                    "signatures",
                    "auto",
                    "aggressive",
                    "entropy",
                    "diff",
                    "lines",
                    "task",
                    "reference",
                ];
                for (mode, count) in modes {
                    if !read_modes.contains(&mode.as_str()) || count.as_u64().unwrap_or(0) == 0 {
                        continue;
                    }
                    entries.push(serde_json::json!({
                        "file_ext": "mixed",
                        "size_bucket": "mixed",
                        "best_mode": mode,
                        "compression_ratio": (ratio * 100.0).round() / 100.0,
                    }));
                }
            }
        }
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1934: the background pass must not revert config edits made while it
    /// was busy on the network — it writes back only the stamps it set.
    #[test]
    fn background_pass_keeps_edits_made_while_it_ran() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let delta = CloudBackgroundDelta {
            last_sync: Some("2026-09-30".into()),
            ..CloudBackgroundDelta::default()
        };

        // Meanwhile the user turns a setting on.
        Config::update_global(|c| c.proxy.ccr_inband = Some(true)).unwrap();
        persist_background_delta(&delta).unwrap();

        let on_disk = Config::load_global();
        assert_eq!(on_disk.proxy.ccr_inband, Some(true), "the edit survived");
        assert_eq!(on_disk.cloud.last_sync.as_deref(), Some("2026-09-30"));
    }

    #[test]
    fn telemetry_ledger_endpoint_removes_credentials_query_and_fragment() {
        assert_eq!(
            sanitized_telemetry_endpoint("https://user:secret@example.test/base?token=x#private"),
            "https://example.test/api/telemetry/v2/batch"
        );
        assert_eq!(
            sanitized_telemetry_endpoint("file:///private/path"),
            "https://api.leanctx.com/api/telemetry/v2/batch"
        );
    }

    #[test]
    fn auto_sync_requires_flag_login_and_unused_slot() {
        // Disabled flag blocks everything else.
        assert!(!should_auto_sync(false, true, None, "2026-06-10"));
        // Logged out never syncs.
        assert!(!should_auto_sync(true, false, None, "2026-06-10"));
        // Fresh slot + flag + login → go.
        assert!(should_auto_sync(true, true, None, "2026-06-10"));
        // Already synced today → debounced.
        assert!(!should_auto_sync(
            true,
            true,
            Some("2026-06-10"),
            "2026-06-10"
        ));
        // Synced yesterday → today's slot is free.
        assert!(should_auto_sync(
            true,
            true,
            Some("2026-06-09"),
            "2026-06-10"
        ));
    }

    #[test]
    fn auto_index_push_needs_flag_login_index_and_fresh_slot() {
        let t = "2026-06-10";
        // All preconditions met → push.
        assert!(should_auto_push_index(true, true, true, None, t));
        // Separate opt-in: auto_sync users are NOT auto-enrolled.
        assert!(!should_auto_push_index(false, true, true, None, t));
        // Logged out / no local index → silently skip, no error path.
        assert!(!should_auto_push_index(true, false, true, None, t));
        assert!(!should_auto_push_index(true, true, false, None, t));
        // Per-project debounce: today consumed, yesterday frees the slot.
        assert!(!should_auto_push_index(true, true, true, Some(t), t));
        assert!(should_auto_push_index(
            true,
            true,
            true,
            Some("2026-06-09"),
            t
        ));
    }

    #[test]
    fn outcome_classification_is_auth_then_gate_then_network_then_synced() {
        // Nothing to push counts as synced (slot consumed, no retry storm).
        assert_eq!(classify_outcomes(&[]), AutoSyncOutcome::Synced);
        // A 401 outranks everything: the credential is gone, so the gate and
        // the network tell us nothing useful.
        assert_eq!(
            classify_outcomes(&[
                Err("Push failed: http status: 401".into()),
                Err("HTTP 402: upgrade required".into()),
                Err("connection refused".into()),
            ]),
            AutoSyncOutcome::Unauthenticated
        );
        // A lone 401 among successes still has to surface — one revoked
        // machine is exactly the case that used to stay silent.
        assert_eq!(
            classify_outcomes(&[Ok(()), Err("Push failed: http status: 401".into())]),
            AutoSyncOutcome::Unauthenticated
        );
        // Any 402 means the account is gated, even with other failures.
        assert_eq!(
            classify_outcomes(&[
                Err("HTTP 402: upgrade required".into()),
                Err("connection refused".into()),
            ]),
            AutoSyncOutcome::Gated
        );
        // All failed without a 401 or 402 → offline, keep the slot open.
        assert_eq!(
            classify_outcomes(&[Err("connection refused".into()), Err("timeout".into()),]),
            AutoSyncOutcome::NetworkFailure
        );
        // Partial success is success.
        assert_eq!(
            classify_outcomes(&[Ok(()), Err("timeout".into())]),
            AutoSyncOutcome::Synced
        );
    }

    #[test]
    fn only_a_network_failure_leaves_the_daily_slot_open() {
        // A dead credential consumes the slot: retrying cannot fix it, and the
        // warning is printed once per process either way. Leaving the slot
        // open here is what turned one revoked key into a silent daily retry.
        assert!(consumes_daily_slot(AutoSyncOutcome::Unauthenticated));
        assert!(consumes_daily_slot(AutoSyncOutcome::Gated));
        assert!(consumes_daily_slot(AutoSyncOutcome::Synced));
        assert!(!consumes_daily_slot(AutoSyncOutcome::NetworkFailure));
    }

    #[test]
    #[serial_test::serial]
    fn daemon_tick_honours_an_explicit_opt_out() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let config = Config::path().expect("config path");
        std::fs::create_dir_all(config.parent().expect("config dir")).expect("config dir");
        std::fs::write(&config, "[telemetry]\nenabled = false\n").expect("write opt-out");
        daemon_telemetry_tick();
        let state = crate::core::paths::state_dir().expect("state dir");
        assert!(!state.join("telemetry_v2_aggregate.json").exists());
        assert!(!state.join("telemetry_v2_one_shots.json").exists());
    }

    #[test]
    #[serial_test::serial]
    fn daemon_tick_attempts_a_send_for_a_default_install() {
        // No config file: the default-on install the daemon exists to cover.
        // The test guard sends to a discard port, so the batch stays pending,
        // which proves the tick reached the send path.
        let _iso = crate::core::data_dir::isolated_data_dir();
        daemon_telemetry_tick();
        let state = crate::core::paths::state_dir().expect("state dir");
        assert!(state.join("telemetry_v2_aggregate.json").exists());
    }
}
