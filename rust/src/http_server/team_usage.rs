//! `GET /v1/storage` + `GET /v1/usage` — server-measured hosted-storage report
//! and combined usage snapshot (`billing-plane-v2`, GL #463).
//!
//! `/v1/storage` is the **camelCase** storage report the control plane's hourly
//! metering job consumes (Stripe meter events + threshold alerts, see
//! `lean-ctx-cloud/src/metering_job.rs`). `/v1/usage` is the **snake_case**
//! combined snapshot whose `storage` block the same metering tolerates and
//! whose `savings` block mirrors the signed savings roll-up. Both surfaces are
//! server-measured (no client signature involved), additive, and never gate a
//! local capability (Local-Free preserved).
//!
//! Authorisation: both paths are gated by `TeamScope::Audit` in the team auth
//! middleware — the control plane reads them with its audit-only
//! `control-plane` token.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use chrono::Utc;
use serde_json::{json, Value};

use super::team::TeamAppState;

/// Default hosted-index quota when `storageQuotaBytes` is omitted from
/// `team.json`: the Team tier's 5 GiB. Mirrors the control plane's
/// `TEAM_STORAGE_QUOTA_BYTES` (`lean-ctx-cloud/src/provisioning/mod.rs`).
pub const DEFAULT_TEAM_STORAGE_QUOTA_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// Re-measure at most once per this window. The hourly metering job, dashboard
/// polls and ad-hoc reads all share one cached filesystem walk.
const CACHE_TTL: Duration = Duration::from_mins(1);

/// One measured storage snapshot (logical byte sizes, symlinks not followed).
#[derive(Debug, Clone)]
struct Snapshot {
    workspaces_bytes: u64,
    audit_bytes: u64,
    savings_bytes: u64,
    /// RFC 3339 timestamp of the walk that produced this snapshot.
    measured_at: String,
    taken: Instant,
}

impl Snapshot {
    fn used_bytes(&self) -> u64 {
        self.workspaces_bytes
            .saturating_add(self.audit_bytes)
            .saturating_add(self.savings_bytes)
    }
}

/// Measures everything the hosted instance persists for the team: workspace
/// trees, the audit log, and the savings store. Cheap to clone (shared cache).
#[derive(Clone)]
pub struct StorageMeter {
    workspace_roots: Arc<Vec<PathBuf>>,
    audit_log: PathBuf,
    savings_dir: PathBuf,
    quota_bytes: u64,
    cache: Arc<tokio::sync::Mutex<Option<Snapshot>>>,
}

impl StorageMeter {
    #[must_use]
    pub fn new(
        workspace_roots: Vec<PathBuf>,
        audit_log: PathBuf,
        savings_dir: PathBuf,
        quota_bytes: u64,
    ) -> Self {
        Self {
            workspace_roots: Arc::new(workspace_roots),
            audit_log,
            savings_dir,
            quota_bytes,
            cache: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// The dedicated `/v1/storage` report (camelCase — the shape
    /// `lean-ctx-cloud` `StorageMetering::from_storage` reads).
    pub async fn storage_response(&self) -> Value {
        let snap = self.snapshot().await;
        json!({
            "schemaVersion": 1,
            "usedBytes": snap.used_bytes(),
            "quotaBytes": self.quota_bytes,
            "breakdown": {
                "workspacesBytes": snap.workspaces_bytes,
                "auditBytes": snap.audit_bytes,
                "savingsBytes": snap.savings_bytes,
            },
            "measuredAt": snap.measured_at,
        })
    }

    /// The `storage` block of `/v1/usage` (snake_case — the spelling
    /// `StorageMetering::from_usage` expects on usage snapshots).
    pub async fn usage_storage_block(&self) -> Value {
        let snap = self.snapshot().await;
        json!({
            "used_bytes": snap.used_bytes(),
            "quota_bytes": self.quota_bytes,
            "measured_at": snap.measured_at,
        })
    }

    /// Cached measurement: re-walks the filesystem at most once per
    /// [`CACHE_TTL`]; concurrent callers share the in-flight result.
    async fn snapshot(&self) -> Snapshot {
        let mut guard = self.cache.lock().await;
        if let Some(snap) = guard.as_ref() {
            if snap.taken.elapsed() < CACHE_TTL {
                return snap.clone();
            }
        }
        let roots = Arc::clone(&self.workspace_roots);
        let audit = self.audit_log.clone();
        let savings = self.savings_dir.clone();
        let measured = tokio::task::spawn_blocking(move || {
            let workspaces_bytes = roots.iter().map(|r| path_size(r)).sum();
            (workspaces_bytes, path_size(&audit), path_size(&savings))
        })
        .await
        .unwrap_or((0, 0, 0));
        let snap = Snapshot {
            workspaces_bytes: measured.0,
            audit_bytes: measured.1,
            savings_bytes: measured.2,
            measured_at: Utc::now().to_rfc3339(),
            taken: Instant::now(),
        };
        *guard = Some(snap.clone());
        snap
    }
}

/// Logical size of a file or directory tree in bytes. Symlinks are not
/// followed (loop-safe); unreadable entries count as `0` instead of failing —
/// the report degrades, it never errors the billing path.
fn path_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// `GET /v1/storage` — camelCase server-measured storage report.
pub async fn v1_storage(State(state): State<TeamAppState>) -> impl IntoResponse {
    let body = state.team.storage.storage_response().await;
    (StatusCode::OK, Json(body))
}

/// `GET /v1/usage` — snake_case combined usage snapshot: the storage block plus
/// the signed-savings roll-up (latest batch per signer, never double-counted).
pub async fn v1_usage(State(state): State<TeamAppState>) -> impl IntoResponse {
    let storage = state.team.storage.usage_storage_block().await;
    let workspaces = state.team.workspace_count();

    let dir = state.team.savings_store_dir.lock().await.clone();
    let savings = tokio::task::spawn_blocking(move || super::savings_summary::aggregate(&dir))
        .await
        .unwrap_or_default();

    let body = json!({
        "schema_version": 1,
        "generated_at": Utc::now().to_rfc3339(),
        "storage": storage,
        "savings": {
            "member_count": savings.member_count,
            "saved_tokens": savings.totals.saved_tokens,
            "net_saved_tokens": savings.totals.net_saved_tokens,
            "saved_usd": savings.totals.saved_usd,
            "total_events": savings.totals.total_events,
        },
        "workspaces": workspaces,
    });
    (StatusCode::OK, Json(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("leanctx_team_usage_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn meter_for(root: &Path) -> StorageMeter {
        StorageMeter::new(
            vec![root.join("ws")],
            root.join("audit.jsonl"),
            root.join("savings"),
            DEFAULT_TEAM_STORAGE_QUOTA_BYTES,
        )
    }

    #[test]
    fn path_size_sums_nested_files_and_tolerates_missing() {
        let d = temp_dir("size");
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("a/x.txt"), vec![0u8; 100]).unwrap();
        std::fs::write(d.join("a/b/y.txt"), vec![0u8; 50]).unwrap();
        assert_eq!(path_size(&d.join("a")), 150);
        assert_eq!(path_size(&d.join("a/x.txt")), 100);
        assert_eq!(path_size(&d.join("does-not-exist")), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn storage_report_is_camelcase_and_adds_up() {
        let d = temp_dir("camel");
        std::fs::create_dir_all(d.join("ws")).unwrap();
        std::fs::create_dir_all(d.join("savings")).unwrap();
        std::fs::write(d.join("ws/f.rs"), vec![0u8; 1000]).unwrap();
        std::fs::write(d.join("audit.jsonl"), vec![0u8; 10]).unwrap();
        std::fs::write(d.join("savings/savings_a.jsonl"), vec![0u8; 5]).unwrap();

        let report = meter_for(&d).storage_response().await;
        assert_eq!(report["schemaVersion"], 1);
        assert_eq!(report["usedBytes"], 1015);
        assert_eq!(report["quotaBytes"], DEFAULT_TEAM_STORAGE_QUOTA_BYTES);
        assert_eq!(report["breakdown"]["workspacesBytes"], 1000);
        assert_eq!(report["breakdown"]["auditBytes"], 10);
        assert_eq!(report["breakdown"]["savingsBytes"], 5);
        assert!(report["measuredAt"].as_str().is_some());
        // The metering consumer requires camelCase; snake_case must be absent.
        assert!(report.get("used_bytes").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn usage_storage_block_is_snake_case() {
        let d = temp_dir("snake");
        std::fs::create_dir_all(d.join("ws")).unwrap();
        std::fs::write(d.join("ws/f.rs"), vec![0u8; 7]).unwrap();

        let block = meter_for(&d).usage_storage_block().await;
        assert_eq!(block["used_bytes"], 7);
        assert_eq!(block["quota_bytes"], DEFAULT_TEAM_STORAGE_QUOTA_BYTES);
        assert!(block.get("usedBytes").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn snapshot_is_cached_within_ttl() {
        let d = temp_dir("cache");
        std::fs::create_dir_all(d.join("ws")).unwrap();
        std::fs::write(d.join("ws/f.rs"), vec![0u8; 3]).unwrap();

        let meter = meter_for(&d);
        let first = meter.storage_response().await;
        // Grow the tree; within the TTL the cached figure must still be served.
        std::fs::write(d.join("ws/g.rs"), vec![0u8; 1000]).unwrap();
        let second = meter.storage_response().await;
        assert_eq!(first["usedBytes"], second["usedBytes"]);
        assert_eq!(first["measuredAt"], second["measuredAt"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn default_quota_is_team_tier_5_gib() {
        assert_eq!(DEFAULT_TEAM_STORAGE_QUOTA_BYTES, 5_368_709_120);
    }
}
