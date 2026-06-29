//! Managed Connectors for the hosted team server (#281).
//!
//! A *connector* is a scheduled, in-process sync from an external source
//! (GitLab / GitHub) into a team workspace's long-term stores (BM25 + graph +
//! knowledge). Once a connector has run, `ctx_semantic_search` and
//! `ctx_knowledge` surface the source's issues / PRs / pipelines to every seat —
//! no per-call credential transport, no manual `ctx_provider` invocation.
//!
//! **Where credentials live.** A connector's credential is only ever present in
//! the injected `team.json` (a private Coolify env var, `LEAN_CTX_TEAM_CONFIG`).
//! The control plane keeps the secret encrypted at rest and decrypts it solely
//! to render that env var; it is never written to disk by the server and never
//! returned by [`v1_connectors`].
//!
//! **Local-Free Invariant.** Connectors are a hosted convenience: they only add
//! context to a hosted workspace and gate nothing locally.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::core::consolidation;
use crate::core::providers::config::GitLabConfig;
use crate::core::providers::github::{GitHubConfig, GitHubProvider};
use crate::core::providers::gitlab::GitLabProvider;
use crate::core::providers::provider_trait::{ContextProvider, ProviderParams};
use crate::core::providers::{ProviderResult, registry};

use super::super::team_billing;
use super::TeamAppState;

/// Smallest sync cadence we accept (defends external APIs from a hot loop).
const MIN_INTERVAL_SECS: u64 = 300;
/// Default sync cadence when a connector omits one (hourly).
const DEFAULT_INTERVAL_SECS: u64 = 3_600;
/// How many items a single sync pulls when the connector omits a limit.
const DEFAULT_LIMIT: usize = 50;

fn default_interval_secs() -> u64 {
    DEFAULT_INTERVAL_SECS
}
fn default_true() -> bool {
    true
}

/// One configured connector, deserialized from `team.json` (`connectors[]`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorConfig {
    /// Stable, DNS/file-safe id (unique within the instance).
    pub id: String,
    /// Source kind: `gitlab` | `github`.
    pub provider: String,
    #[serde(default)]
    pub display_name: Option<String>,
    /// Target workspace; the instance default when omitted.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Resource to pull: gitlab `issues|merge_requests|pipelines`,
    /// github `issues|pull_requests|actions`.
    pub resource: String,
    /// `group/project` (GitLab) or `owner/repo` (GitHub).
    #[serde(default)]
    pub project: Option<String>,
    /// GitLab host (default `gitlab.com`) or GitHub API base
    /// (default `https://api.github.com`).
    #[serde(default)]
    pub host: Option<String>,
    /// Optional state filter passed through to the provider (e.g. `opened`).
    #[serde(default)]
    pub state: Option<String>,
    /// Max items per sync.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Desired sync cadence in seconds (clamped to a 5-minute floor).
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    /// Provider credential (plaintext only inside the private team.json).
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl ConnectorConfig {
    /// Effective cadence, never below the floor.
    #[must_use]
    pub fn effective_interval(&self) -> u64 {
        self.interval_secs.max(MIN_INTERVAL_SECS)
    }

    fn has_secret(&self) -> bool {
        self.secret.as_deref().is_some_and(|s| !s.trim().is_empty())
    }

    fn limit(&self) -> usize {
        self.limit.unwrap_or(DEFAULT_LIMIT)
    }
}

/// Persisted outcome of a connector's most recent sync (one file per connector
/// under `<state_dir>/<id>.json`). Never contains the credential.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorRunState {
    /// RFC3339 timestamp of the last attempt (human-facing).
    pub last_run_at: Option<String>,
    /// Epoch seconds of the last attempt (scheduling).
    #[serde(default)]
    pub last_run_secs: Option<u64>,
    /// `ok` | `error`.
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub last_item_count: Option<usize>,
    #[serde(default)]
    pub total_runs: u64,
    #[serde(default)]
    pub total_items: u64,
}

/// Pure scheduler decision: is a connector due to run now?
///
/// First run (`last_run` is `None`) is always due; afterwards a connector is due
/// once at least `interval` seconds have elapsed since the last *attempt*. The
/// `interval` is floored at one second so a misconfigured `0` never busy-loops.
#[must_use]
pub fn is_due(now: u64, last_run: Option<u64>, interval: u64) -> bool {
    match last_run {
        None => true,
        Some(last) => now.saturating_sub(last) >= interval.max(1),
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Keep only file-safe characters so a connector id can never escape the state
/// directory (defence in depth — ids are control-plane minted).
fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn state_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{}.json", sanitize_id(id)))
}

fn load_state(dir: &Path, id: &str) -> ConnectorRunState {
    std::fs::read_to_string(state_path(dir, id))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(dir: &Path, id: &str, st: &ConnectorRunState) {
    let _ = std::fs::create_dir_all(dir);
    if let Ok(s) = serde_json::to_string_pretty(st) {
        let _ = std::fs::write(state_path(dir, id), s);
    }
}

/// Fetch the connector's source data as a `ProviderResult`, constructing a
/// provider with the connector's own credential (no global env mutation).
fn fetch(cfg: &ConnectorConfig) -> Result<ProviderResult, String> {
    let secret = cfg
        .secret
        .clone()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "connector has no credential configured".to_string())?;
    let params = ProviderParams {
        state: cfg.state.clone(),
        limit: Some(cfg.limit()),
        ..Default::default()
    };

    match cfg.provider.as_str() {
        "gitlab" => {
            let gl = GitLabConfig {
                host: cfg
                    .host
                    .clone()
                    .filter(|h| !h.trim().is_empty())
                    .unwrap_or_else(|| "gitlab.com".to_string()),
                token: secret,
                project_path: cfg.project.clone(),
            };
            GitLabProvider::with_config(gl).execute(&cfg.resource, &params)
        }
        "github" => {
            let (owner, repo) = split_owner_repo(cfg.project.as_deref());
            let gh = GitHubConfig {
                token: secret,
                owner,
                repo,
                api_base: cfg
                    .host
                    .clone()
                    .filter(|h| !h.trim().is_empty())
                    .unwrap_or_else(|| "https://api.github.com".to_string()),
            };
            GitHubProvider::with_config(gh).execute(&cfg.resource, &params)
        }
        other => Err(format!(
            "unsupported provider '{other}' (expected gitlab|github)"
        )),
    }
}

fn split_owner_repo(project: Option<&str>) -> (Option<String>, Option<String>) {
    match project.and_then(|p| p.split_once('/')) {
        Some((o, r)) => (Some(o.to_string()), Some(r.to_string())),
        None => (None, None),
    }
}

/// Run one sync: fetch → chunk → consolidate → persist into the workspace's
/// BM25 / graph / knowledge stores. Returns the number of items ingested.
fn run_once(cfg: &ConnectorConfig, workspace_root: &Path) -> Result<usize, String> {
    let result = fetch(cfg)?;
    let chunks = registry::result_to_chunks(&result);
    let n = chunks.len();
    if !chunks.is_empty() {
        let artifacts = consolidation::consolidate(&chunks);
        if !artifacts.is_empty() {
            crate::tools::ctx_provider::apply_artifacts_to_stores(
                &artifacts,
                &workspace_root.to_string_lossy(),
            );
        }
    }
    Ok(n)
}

/// Spawn the background scheduler. Ticks every `tick`, runs each due connector
/// once (blocking work on the blocking pool), and records its outcome. A no-op
/// when no connectors are configured.
pub fn spawn_scheduler(
    connectors: Arc<Vec<ConnectorConfig>>,
    roots: Arc<HashMap<String, String>>,
    default_workspace_id: String,
    state_dir: PathBuf,
    data_dir: PathBuf,
    quota_bytes: u64,
    tick: Duration,
) {
    if connectors.iter().all(|c| !c.enabled) {
        return;
    }
    tokio::spawn(async move {
        // Let the server finish binding before the first sync.
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            // Quota backstop (#282): once the hosted index hits quota we pause
            // ingestion (never delete, never gate reads). Checked once per tick.
            let over_quota = team_billing::is_over_quota(&data_dir, quota_bytes);
            for c in connectors.iter().filter(|c| c.enabled) {
                let st = load_state(&state_dir, &c.id);
                if !is_due(now_secs(), st.last_run_secs, c.effective_interval()) {
                    continue;
                }
                if over_quota {
                    let mut st = st;
                    st.last_status = Some("error".to_string());
                    st.last_error = Some("storage quota exceeded — hosted sync paused".to_string());
                    st.last_run_secs = Some(now_secs());
                    st.last_run_at = Some(chrono::Utc::now().to_rfc3339());
                    st.total_runs = st.total_runs.saturating_add(1);
                    save_state(&state_dir, &c.id, &st);
                    tracing::warn!(
                        connector = %c.id,
                        "skipping connector sync: storage quota exceeded"
                    );
                    continue;
                }
                let ws = c
                    .workspace_id
                    .clone()
                    .unwrap_or_else(|| default_workspace_id.clone());
                let Some(root) = roots.get(&ws).cloned() else {
                    tracing::warn!(
                        connector = %c.id,
                        workspace = %ws,
                        "connector references unknown workspace; skipping"
                    );
                    continue;
                };

                let cfg = c.clone();
                let dir = state_dir.clone();
                // Provider HTTP + store writes are blocking.
                let _ = tokio::task::spawn_blocking(move || {
                    let started = now_secs();
                    let mut st = load_state(&dir, &cfg.id);
                    match run_once(&cfg, Path::new(&root)) {
                        Ok(n) => {
                            st.last_status = Some("ok".to_string());
                            st.last_error = None;
                            st.last_item_count = Some(n);
                            st.total_items = st.total_items.saturating_add(n as u64);
                            tracing::info!(connector = %cfg.id, items = n, "connector sync ok");
                        }
                        Err(e) => {
                            st.last_status = Some("error".to_string());
                            st.last_error = Some(e.clone());
                            tracing::warn!(connector = %cfg.id, error = %e, "connector sync failed");
                        }
                    }
                    // Record the attempt time even on error so a failing
                    // connector waits its interval before retrying.
                    st.last_run_secs = Some(started);
                    st.last_run_at = Some(chrono::Utc::now().to_rfc3339());
                    st.total_runs = st.total_runs.saturating_add(1);
                    save_state(&dir, &cfg.id, &st);
                })
                .await;
            }
            tokio::time::sleep(tick).await;
        }
    });
}

/// Public, secret-free view of a connector and its latest run.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectorView {
    id: String,
    provider: String,
    display_name: Option<String>,
    workspace_id: String,
    resource: String,
    project: Option<String>,
    interval_secs: u64,
    enabled: bool,
    /// Whether a credential is configured (the secret itself is never exposed).
    has_secret: bool,
    status: ConnectorRunState,
}

/// `GET /v1/connectors` — secret-free roster + per-connector sync status. Gated
/// on the `audit` scope (read by the control plane via its audit-only token, the
/// same path the savings roll-up uses).
pub async fn v1_connectors(State(state): State<TeamAppState>) -> impl IntoResponse {
    let default_ws = state.team.engine.server.default_workspace_id.clone();
    let dir = state.team.connectors_state_dir.as_ref().clone();

    let views: Vec<ConnectorView> = state
        .team
        .connectors
        .iter()
        .map(|c| {
            let workspace_id = c.workspace_id.clone().unwrap_or_else(|| default_ws.clone());
            ConnectorView {
                id: c.id.clone(),
                provider: c.provider.clone(),
                display_name: c.display_name.clone(),
                workspace_id,
                resource: c.resource.clone(),
                project: c.project.clone(),
                interval_secs: c.effective_interval(),
                enabled: c.enabled,
                has_secret: c.has_secret(),
                status: load_state(&dir, &c.id),
            }
        })
        .collect();

    (
        StatusCode::OK,
        Json(json!({
            "schema_version": 1,
            "generated_at": chrono::Utc::now().to_rfc3339(),
            "connector_count": views.len(),
            "connectors": views,
        })),
    )
}

/// Aggregate connector activity for the usage snapshot (#283). Reads each
/// connector's persisted run state only; never touches credentials.
#[must_use]
pub fn usage_rollup(connectors: &[ConnectorConfig], state_dir: &Path) -> serde_json::Value {
    let mut total_runs = 0u64;
    let mut total_items = 0u64;
    let mut ok = 0u64;
    let mut errored = 0u64;
    let mut last_run_at: Option<String> = None;
    for c in connectors {
        let st = load_state(state_dir, &c.id);
        total_runs = total_runs.saturating_add(st.total_runs);
        total_items = total_items.saturating_add(st.total_items);
        match st.last_status.as_deref() {
            Some("ok") => ok += 1,
            Some("error") => errored += 1,
            _ => {}
        }
        if let Some(ts) = st.last_run_at
            && last_run_at.as_deref().is_none_or(|cur| ts.as_str() > cur)
        {
            last_run_at = Some(ts);
        }
    }
    json!({
        "configured": connectors.len(),
        "enabled": connectors.iter().filter(|c| c.enabled).count(),
        "total_runs": total_runs,
        "total_items_ingested": total_items,
        "last_status_ok": ok,
        "last_status_error": errored,
        "last_run_at": last_run_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_is_always_due() {
        assert!(is_due(1_000, None, 3_600));
    }

    #[test]
    fn due_only_after_interval_elapses() {
        // 100s elapsed, 300s interval → not due yet.
        assert!(!is_due(1_100, Some(1_000), 300));
        // exactly interval → due.
        assert!(is_due(1_300, Some(1_000), 300));
        // well past → due.
        assert!(is_due(5_000, Some(1_000), 300));
    }

    #[test]
    fn zero_interval_never_busy_loops() {
        // A misconfigured 0 is floored to 1s, so a same-second re-check is not due.
        assert!(!is_due(1_000, Some(1_000), 0));
        assert!(is_due(1_001, Some(1_000), 0));
    }

    #[test]
    fn interval_is_floored_to_minimum() {
        let c = ConnectorConfig {
            id: "x".into(),
            provider: "gitlab".into(),
            display_name: None,
            workspace_id: None,
            resource: "issues".into(),
            project: Some("g/p".into()),
            host: None,
            state: None,
            limit: None,
            interval_secs: 5,
            secret: Some("t".into()),
            enabled: true,
        };
        assert_eq!(c.effective_interval(), MIN_INTERVAL_SECS);
    }

    #[test]
    fn split_owner_repo_parses_slug() {
        assert_eq!(
            split_owner_repo(Some("octocat/hello")),
            (Some("octocat".to_string()), Some("hello".to_string()))
        );
        assert_eq!(split_owner_repo(Some("noseparator")), (None, None));
        assert_eq!(split_owner_repo(None), (None, None));
    }

    #[test]
    fn sanitize_id_blocks_traversal() {
        assert_eq!(sanitize_id("../../etc/passwd"), "______etc_passwd");
        assert_eq!(sanitize_id("conn-1_ok"), "conn-1_ok");
    }

    #[test]
    fn unsupported_provider_is_rejected() {
        let c = ConnectorConfig {
            id: "x".into(),
            provider: "bitbucket".into(),
            display_name: None,
            workspace_id: None,
            resource: "issues".into(),
            project: Some("g/p".into()),
            host: None,
            state: None,
            limit: None,
            interval_secs: 3_600,
            secret: Some("t".into()),
            enabled: true,
        };
        let err = fetch(&c).unwrap_err();
        assert!(err.contains("unsupported provider"));
    }

    #[test]
    fn missing_secret_is_rejected() {
        let c = ConnectorConfig {
            id: "x".into(),
            provider: "gitlab".into(),
            display_name: None,
            workspace_id: None,
            resource: "issues".into(),
            project: Some("g/p".into()),
            host: None,
            state: None,
            limit: None,
            interval_secs: 3_600,
            secret: None,
            enabled: true,
        };
        let err = fetch(&c).unwrap_err();
        assert!(err.contains("no credential"));
    }

    /// End-to-end: a real sync against a live HTTP source must land in the target
    /// workspace's BM25 store and be searchable afterwards. A tiny axum server
    /// stands in for the GitHub REST API and answers with a fixture in GitHub's
    /// exact wire shape; the production sync path
    /// (HTTP fetch → parse → chunk → consolidate → store) runs for real against
    /// it. Nothing in the code under test is mocked — only the remote endpoint is
    /// local so the test is hermetic and needs no credentials or network.
    #[tokio::test]
    async fn sync_lands_in_searchable_store_end_to_end() {
        use axum::Router;
        use axum::routing::get;

        let issues = serde_json::json!([
            {
                "number": 1,
                "title": "Zephyr crash on cold start",
                "state": "open",
                "user": { "login": "alice" },
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-02T00:00:00Z",
                "html_url": "http://example.test/1",
                "labels": [{ "name": "bug" }],
                "body": "Service panics in the Zephyr boot path on a cold start."
            },
            {
                "number": 2,
                "title": "Add Borealis dashboard",
                "state": "open",
                "user": { "login": "bob" },
                "created_at": "2026-01-03T00:00:00Z",
                "updated_at": "2026-01-04T00:00:00Z",
                "html_url": "http://example.test/2",
                "labels": [],
                "body": "A Borealis analytics panel for the team overview."
            }
        ]);

        let app = Router::new().route(
            "/repos/acme/widgets/issues",
            get(move || {
                let body = issues.clone();
                async move { Json(body) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let workspace = tempfile::tempdir().unwrap();
        let ws_root = workspace.path().to_path_buf();

        let cfg = ConnectorConfig {
            id: "gh-e2e".into(),
            provider: "github".into(),
            display_name: None,
            workspace_id: None,
            resource: "issues".into(),
            project: Some("acme/widgets".into()),
            // `host` becomes the GitHub `api_base`, so we point the real provider
            // at our local fixture server.
            host: Some(format!("http://{addr}")),
            state: Some("open".into()),
            limit: Some(50),
            interval_secs: 3_600,
            secret: Some("test-token".into()),
            enabled: true,
        };

        // `run_once` does blocking HTTP (ureq) + store writes; keep it off the
        // async reactor so the fixture server can serve the request.
        let ws_sync = ws_root.clone();
        let ingested = tokio::task::spawn_blocking(move || run_once(&cfg, &ws_sync))
            .await
            .unwrap()
            .expect("sync against the local source must succeed");
        assert_eq!(ingested, 2, "both fixture issues must be ingested");

        // The sync must have persisted a real, searchable BM25 index for the
        // workspace. `load` reads the persisted artifact directly; the workspace
        // safety/staleness guards in `load_or_build` are a separate concern of the
        // workspace lifecycle, not of the connector's write path.
        let hits = tokio::task::spawn_blocking(move || {
            crate::core::bm25_index::BM25Index::load(&ws_root)
                .expect("the sync must persist a BM25 index")
                .search("Zephyr", 5)
        })
        .await
        .unwrap();
        assert!(
            !hits.is_empty(),
            "the synced GitHub issue must be findable in the persisted BM25 index"
        );

        server.abort();
    }
}
