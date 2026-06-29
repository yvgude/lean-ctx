//! `GET /v1/storage` + `GET /v1/usage` — the team server's billing-plane
//! surface (`docs/contracts/billing-plane-v2.md`, GL #463).
//!
//! `/v1/storage` reports the hosted workspace footprint (retrieval index,
//! knowledge store, event log — everything the server persists under its data
//! root and the workspaces' `.lean-ctx` state dirs). It is **server-measured**:
//! the control plane's hourly `metering_job` polls it for Stripe meter events
//! and threshold mails, so the report carries plain numbers and no content.
//! Field casing is `camelCase` (`usedBytes`), matching what
//! `lean-ctx-cloud/src/metering_job.rs` and `metering.rs::from_storage` read.
//!
//! `/v1/usage` is the unified usage snapshot for the account dashboard: the
//! savings roll-up (from the same signed-batch store as `/v1/savings/summary`)
//! plus a `storage` block in `snake_case` (`used_bytes`) — the spelling
//! `metering.rs::from_usage` expects for that block.
//!
//! Sizing uses allocated disk blocks (`st_blocks * 512`) on Unix so sparse and
//! partially-written files bill what they actually occupy; on other platforms
//! it falls back to logical file length. Reports are cached for
//! `STORAGE_CACHE_TTL` per process — the walk is `O(files)` and the metering
//! job polls hourly, so 60 s keeps repeated dashboard hits cheap without
//! letting bills go stale.
//!
//! Authorisation: both routes are gated by [`TeamScope::Audit`](super::team)
//! in the team auth middleware — same sensitivity class as `/v1/metrics` and
//! `/v1/savings/summary`, and the scope the control plane's audit-only token
//! carries.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Serialize;
use serde_json::json;

use super::team::TeamAppState;

/// How long a measured storage report may be served from cache.
pub(super) const STORAGE_CACHE_TTL: Duration = Duration::from_mins(1);

/// Env var through which a deployment can *override* the plan quota in bytes
/// (ops escape hatch). Normally the quota arrives as `storageQuotaBytes` in
/// `team.json`, rendered per plan by the control plane's provisioning bridge
/// (#282: Team 5 GiB, Enterprise 50 GiB).
pub(super) const QUOTA_ENV: &str = "LEANCTX_TEAM_STORAGE_QUOTA_BYTES";

/// Default quota when neither the env override nor `storageQuotaBytes` is
/// present: the Team tier's 5 GiB, per the provisioning contract ("the server
/// defaults to the Team tier when omitted",
/// `lean-ctx-cloud/src/provisioning/instance.rs`). Always resolving to a
/// concrete quota keeps the control plane's metering out of the degenerate
/// `quota = 0 ⇒ state "none"` path on hosted instances.
pub(super) const DEFAULT_TEAM_STORAGE_QUOTA_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// One measured storage component (a directory or file the server persists).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageComponent {
    /// Stable identifier: `server-data` or `workspace:<id>`.
    pub id: String,
    pub bytes: u64,
}

/// A measured (uncached) storage report.
#[derive(Debug, Clone)]
pub struct StorageReport {
    pub used_bytes: u64,
    pub components: Vec<StorageComponent>,
}

/// Cached report + when it was measured.
#[derive(Default)]
pub struct StorageCache(Option<(Instant, StorageReport)>);

/// The measurement inputs, fixed at server startup.
#[derive(Clone)]
pub struct StorageRoots {
    /// The server's data root (audit log, savings store, hosted indices) —
    /// `/data` on hosted instances.
    pub data_root: PathBuf,
    /// Per-workspace persistent state (`<root>/.lean-ctx`), skipped when it
    /// already lives under [`Self::data_root`] so nothing is counted twice.
    pub workspaces: Vec<(String, PathBuf)>,
    /// Plan quota in bytes, resolved once at startup
    /// (env override → `storageQuotaBytes` → Team-tier default).
    pub quota_bytes: u64,
}

impl StorageRoots {
    /// Measure every component now. `O(files)` — call through the cache.
    fn measure(&self) -> StorageReport {
        let mut components = Vec::new();
        let data_bytes = dir_allocated_bytes(&self.data_root);
        components.push(StorageComponent {
            id: "server-data".into(),
            bytes: data_bytes,
        });

        for (id, state_dir) in &self.workspaces {
            if state_dir.starts_with(&self.data_root) {
                continue; // already inside server-data
            }
            components.push(StorageComponent {
                id: format!("workspace:{id}"),
                bytes: dir_allocated_bytes(state_dir),
            });
        }

        let used_bytes = components
            .iter()
            .map(|c| c.bytes)
            .fold(0u64, u64::saturating_add);
        StorageReport {
            used_bytes,
            components,
        }
    }
}

/// `GET /v1/storage` — `camelCase`, served from the 60 s cache.
pub async fn v1_storage(State(state): State<TeamAppState>) -> impl IntoResponse {
    let (report, age) = cached_report(&state).await;
    let body = json!({
        "schemaVersion": 1,
        "measuredAt": chrono::Utc::now().to_rfc3339(),
        "usedBytes": report.used_bytes,
        "quotaBytes": state.team.storage_roots.quota_bytes,
        "components": report.components,
        "cacheAgeSeconds": age.as_secs(),
    });
    (StatusCode::OK, Json(body))
}

/// `GET /v1/usage` — savings roll-up + `snake_case` storage block.
pub async fn v1_usage(State(state): State<TeamAppState>) -> impl IntoResponse {
    let dir = state.team.savings_store_dir.lock().await.clone();
    let summary = tokio::task::spawn_blocking(move || super::savings_summary::aggregate(&dir))
        .await
        .unwrap_or_default();
    let (report, _) = cached_report(&state).await;

    let storage = json!({
        "used_bytes": report.used_bytes,
        "quota_bytes": state.team.storage_roots.quota_bytes,
    });

    // Managed-connector activity (#281/#283): a secret-free roll-up of each
    // connector's persisted run state, read off the runtime so the small file
    // walk never blocks the reactor. Empty when no connectors are configured.
    let connectors = state.team.connectors.clone();
    let connectors_dir = state.team.connectors_state_dir.as_ref().clone();
    let connectors_usage = tokio::task::spawn_blocking(move || {
        super::team::connectors::usage_rollup(&connectors, &connectors_dir)
    })
    .await
    .unwrap_or_else(|_| json!({}));

    let body = json!({
        "schemaVersion": 1,
        "generatedAt": chrono::Utc::now().to_rfc3339(),
        "savings": {
            "memberCount": summary.member_count,
            "savedTokens": summary.totals.saved_tokens,
            "netSavedTokens": summary.totals.net_saved_tokens,
            "savedUsd": summary.totals.saved_usd,
        },
        // Signed-ledger events are the team's measured agent actions — the
        // honest "tool calls" figure (each ledger entry is one measured call).
        "toolCalls": summary.totals.total_events,
        "storage": storage,
        "connectors": connectors_usage,
    });
    (StatusCode::OK, Json(body))
}

/// Serve from cache when fresh; otherwise re-measure off the async runtime.
async fn cached_report(state: &TeamAppState) -> (StorageReport, Duration) {
    {
        let cache = state.team.storage_cache.lock().await;
        if let Some((at, report)) = cache.0.as_ref() {
            let age = at.elapsed();
            if age < STORAGE_CACHE_TTL {
                return (report.clone(), age);
            }
        }
    }

    let roots = state.team.storage_roots.clone();
    let report = tokio::task::spawn_blocking(move || roots.measure())
        .await
        .unwrap_or(StorageReport {
            used_bytes: 0,
            components: Vec::new(),
        });

    let mut cache = state.team.storage_cache.lock().await;
    cache.0 = Some((Instant::now(), report.clone()));
    (report, Duration::ZERO)
}

fn quota_bytes_from_env() -> Option<u64> {
    std::env::var(QUOTA_ENV).ok()?.trim().parse::<u64>().ok()
}

/// Resolve the effective quota: env override (ops escape hatch) →
/// `storageQuotaBytes` from `team.json` (provisioning, #282) → Team-tier
/// default. Pure so the precedence is unit-testable without env races.
pub(super) fn resolve_quota_bytes(env_override: Option<u64>, config_quota: Option<u64>) -> u64 {
    env_override
        .or(config_quota)
        .unwrap_or(DEFAULT_TEAM_STORAGE_QUOTA_BYTES)
}

/// Build the measurement roots from the server config: the audit log's parent
/// is the server data root; each workspace contributes `<root>/.lean-ctx`.
/// `config_quota` is the `storageQuotaBytes` value from `team.json`.
pub(super) fn storage_roots_from_config(
    audit_log_path: &Path,
    workspaces: &[(String, PathBuf)],
    config_quota: Option<u64>,
) -> StorageRoots {
    let data_root = audit_log_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let workspaces = workspaces
        .iter()
        .map(|(id, root)| (id.clone(), root.join(".lean-ctx")))
        .collect();
    StorageRoots {
        data_root,
        workspaces,
        quota_bytes: resolve_quota_bytes(quota_bytes_from_env(), config_quota),
    }
}

/// Recursively sum a directory's allocated bytes. Missing paths are `0`
/// (a fresh server simply has no footprint yet). Symlinks are not followed
/// (`symlink_metadata`), so a link cannot inflate the bill or escape the root;
/// hard-linked files are deduplicated by (dev, inode) on Unix.
fn dir_allocated_bytes(path: &Path) -> u64 {
    let mut seen: BTreeSet<(u64, u64)> = BTreeSet::new();
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.is_symlink() {
            continue;
        }
        if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&p) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if !seen.insert((meta.dev(), meta.ino())) {
                continue; // hard link already counted
            }
            total = total.saturating_add(meta.blocks().saturating_mul(512));
        }
        #[cfg(not(unix))]
        {
            let _ = &mut seen;
            total = total.saturating_add(meta.len());
        }
    }
    total
}

/// Hosted-index quota backstop (#282): is the server's measured footprint at or
/// over the plan quota? The managed-connector scheduler ([`super::team::connectors`])
/// calls this once per tick to pause ingestion when full — it never deletes and
/// never gates reads. Measured with the same `dir_allocated_bytes` the billing
/// report uses, so the backstop and the bill agree. A `quota_bytes` of `0` (no
/// quota provisioned) never trips, so an unconfigured server keeps syncing.
#[must_use]
pub(crate) fn is_over_quota(data_root: &Path, quota_bytes: u64) -> bool {
    quota_bytes > 0 && dir_allocated_bytes(data_root) >= quota_bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("leanctx_team_billing_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn missing_dir_measures_zero() {
        let missing = std::env::temp_dir().join("leanctx_team_billing_does_not_exist_xyz");
        let _ = std::fs::remove_dir_all(&missing);
        assert_eq!(dir_allocated_bytes(&missing), 0);
    }

    /// Quota precedence (#282/#463): env override → `storageQuotaBytes` from
    /// `team.json` → Team-tier 5 GiB default. The report therefore always
    /// carries a concrete quota and hosted metering never degenerates into
    /// the `quota = 0 ⇒ "none"` state.
    #[test]
    fn quota_resolution_precedence() {
        assert_eq!(resolve_quota_bytes(Some(7), Some(9)), 7);
        assert_eq!(resolve_quota_bytes(None, Some(9)), 9);
        assert_eq!(
            resolve_quota_bytes(None, None),
            DEFAULT_TEAM_STORAGE_QUOTA_BYTES
        );
        assert_eq!(DEFAULT_TEAM_STORAGE_QUOTA_BYTES, 5_368_709_120);
    }

    /// Quota backstop (#282) for the managed-connector scheduler: a `0` quota
    /// (unprovisioned) never trips so syncing keeps working, a measured footprint
    /// at/over the quota does trip, and a missing data root measures zero.
    #[test]
    fn over_quota_only_trips_with_a_positive_quota() {
        let d = temp_dir("overquota");
        std::fs::write(d.join("a.bin"), vec![b'x'; 20_000]).unwrap();
        assert!(!is_over_quota(&d, 0), "0 quota must never trip");
        assert!(is_over_quota(&d, 1_000), "20 KiB must exceed a 1 KiB quota");
        assert!(
            !is_over_quota(&d, 10 * 1024 * 1024),
            "20 KiB must not exceed a 10 MiB quota"
        );
        let missing = std::env::temp_dir().join("leanctx_team_billing_overquota_missing_xyz");
        let _ = std::fs::remove_dir_all(&missing);
        assert!(
            !is_over_quota(&missing, 1),
            "missing dir is never over quota"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn allocated_bytes_cover_written_content() {
        let d = temp_dir("alloc");
        std::fs::write(d.join("a.jsonl"), vec![b'x'; 10_000]).unwrap();
        std::fs::create_dir_all(d.join("nested")).unwrap();
        std::fs::write(d.join("nested/b.bin"), vec![b'y'; 5_000]).unwrap();
        let measured = dir_allocated_bytes(&d);
        // Allocation granularity is FS-dependent; it must cover the logical
        // sizes without an absurd blow-up (factor 64 ≈ one 64 KiB cluster
        // per tiny file, far above any real FS we deploy on).
        assert!(measured >= 15_000, "measured {measured} < logical 15000");
        assert!(
            measured < 15_000 * 64,
            "measured {measured} implausibly large"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_count_once_and_symlinks_do_not_escape() {
        let d = temp_dir("links");
        std::fs::write(d.join("real.bin"), vec![b'z'; 8_192]).unwrap();
        std::fs::hard_link(d.join("real.bin"), d.join("hard.bin")).unwrap();
        let outside = temp_dir("links_outside");
        std::fs::write(outside.join("big.bin"), vec![b'w'; 100_000]).unwrap();
        std::os::unix::fs::symlink(outside.join("big.bin"), d.join("escape.bin")).unwrap();

        let measured = dir_allocated_bytes(&d);
        let single = dir_allocated_bytes(&outside); // ~100k for comparison
        assert!(measured < single, "symlink target must not be billed");
        // 8 KiB once, not twice (allow allocation slack below 2x).
        assert!(
            (8_192..16_384).contains(&measured),
            "hard link double-counted: {measured}"
        );

        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn workspace_state_dirs_under_data_root_are_not_double_counted() {
        let d = temp_dir("dedupe");
        let audit = d.join("audit.jsonl");
        std::fs::write(&audit, "x").unwrap();
        // Workspace lives under the data root — its .lean-ctx is part of
        // server-data and must be skipped as a separate component.
        let ws_root = d.join("ws1");
        std::fs::create_dir_all(ws_root.join(".lean-ctx")).unwrap();
        std::fs::write(ws_root.join(".lean-ctx/events.jsonl"), vec![b'e'; 4_096]).unwrap();
        // And one external workspace that must be counted.
        let ext = temp_dir("dedupe_ext");
        std::fs::create_dir_all(ext.join(".lean-ctx")).unwrap();
        std::fs::write(ext.join(".lean-ctx/k.jsonl"), vec![b'k'; 4_096]).unwrap();

        let roots = storage_roots_from_config(
            &audit,
            &[
                ("inside".into(), ws_root.clone()),
                ("outside".into(), ext.clone()),
            ],
            None,
        );
        let report = roots.measure();
        let ids: Vec<&str> = report.components.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"server-data"));
        assert!(
            !ids.contains(&"workspace:inside"),
            "nested workspace double-counted"
        );
        assert!(ids.contains(&"workspace:outside"));
        let sum: u64 = report.components.iter().map(|c| c.bytes).sum();
        assert_eq!(report.used_bytes, sum);

        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&ext);
    }
}
