use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde::Serialize;

const WORKSPACE_ACTIVE_TTL_SECS: u64 = 600;

/// Process-local metrics for Context OS observability.
pub struct ContextOsMetrics {
    events_appended: AtomicU64,
    events_broadcast: AtomicU64,
    events_replayed: AtomicU64,
    sse_connections_opened: AtomicU64,
    sse_connections_closed: AtomicU64,
    shared_sessions_loaded: AtomicU64,
    shared_sessions_persisted: AtomicU64,
    active_workspaces: Mutex<std::collections::HashMap<String, Instant>>,
}

impl Default for ContextOsMetrics {
    fn default() -> Self {
        Self {
            events_appended: AtomicU64::new(0),
            events_broadcast: AtomicU64::new(0),
            events_replayed: AtomicU64::new(0),
            sse_connections_opened: AtomicU64::new(0),
            sse_connections_closed: AtomicU64::new(0),
            shared_sessions_loaded: AtomicU64::new(0),
            shared_sessions_persisted: AtomicU64::new(0),
            active_workspaces: Mutex::new(std::collections::HashMap::new()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsSnapshot {
    pub events_appended: u64,
    pub events_broadcast: u64,
    pub events_replayed: u64,
    pub sse_connections_active: u64,
    pub sse_connections_total: u64,
    pub shared_sessions_loaded: u64,
    pub shared_sessions_persisted: u64,
    pub active_workspace_count: usize,
}

impl ContextOsMetrics {
    pub fn record_event_appended(&self) {
        self.events_appended.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_event_broadcast(&self) {
        self.events_broadcast.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_events_replayed(&self, count: u64) {
        self.events_replayed.fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_sse_connect(&self) {
        self.sse_connections_opened.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_sse_disconnect(&self) {
        self.sse_connections_closed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_session_loaded(&self) {
        self.shared_sessions_loaded.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_session_persisted(&self) {
        self.shared_sessions_persisted
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_workspace_active(&self, workspace_id: &str) {
        if let Ok(mut map) = self.active_workspaces.lock() {
            map.insert(workspace_id.to_string(), Instant::now());
        }
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let opened = self.sse_connections_opened.load(Ordering::Relaxed);
        let closed = self.sse_connections_closed.load(Ordering::Relaxed);
        let active_workspace_count = match self.active_workspaces.lock() {
            Ok(mut map) => {
                if let Some(cutoff) = Instant::now()
                    .checked_sub(std::time::Duration::from_secs(WORKSPACE_ACTIVE_TTL_SECS))
                {
                    map.retain(|_, last_seen| *last_seen > cutoff);
                }
                map.len()
            }
            _ => 0,
        };
        MetricsSnapshot {
            events_appended: self.events_appended.load(Ordering::Relaxed),
            events_broadcast: self.events_broadcast.load(Ordering::Relaxed),
            events_replayed: self.events_replayed.load(Ordering::Relaxed),
            sse_connections_active: opened.saturating_sub(closed),
            sse_connections_total: opened,
            shared_sessions_loaded: self.shared_sessions_loaded.load(Ordering::Relaxed),
            shared_sessions_persisted: self.shared_sessions_persisted.load(Ordering::Relaxed),
            active_workspace_count,
        }
    }
}
