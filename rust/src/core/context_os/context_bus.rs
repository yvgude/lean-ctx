use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

const MAX_READ_CONNS: usize = 4;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextEventKindV1 {
    ToolCallRecorded,
    SessionMutated,
    KnowledgeRemembered,
    ArtifactStored,
    GraphBuilt,
    ProofAdded,
}

impl ContextEventKindV1 {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ToolCallRecorded => "tool_call_recorded",
            Self::SessionMutated => "session_mutated",
            Self::KnowledgeRemembered => "knowledge_remembered",
            Self::ArtifactStored => "artifact_stored",
            Self::GraphBuilt => "graph_built",
            Self::ProofAdded => "proof_added",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "tool_call_recorded" => Self::ToolCallRecorded,
            "session_mutated" => Self::SessionMutated,
            "knowledge_remembered" => Self::KnowledgeRemembered,
            "artifact_stored" => Self::ArtifactStored,
            "graph_built" => Self::GraphBuilt,
            "proof_added" => Self::ProofAdded,
            other => {
                tracing::warn!(
                    "unknown ContextEventKind '{other}', defaulting to ToolCallRecorded"
                );
                Self::ToolCallRecorded
            }
        }
    }

    /// Classifies the consistency requirement for this event kind.
    ///
    /// - `Local`: Agent-local, never shared (tool reads, cache hits).
    /// - `Eventual`: Broadcast via bus, other agents see it "soon" (knowledge, artifacts).
    /// - `Strong`: Critical decisions that require acknowledgment before proceeding.
    #[must_use]
    pub fn consistency_level(&self) -> ConsistencyLevel {
        match self {
            Self::ToolCallRecorded | Self::GraphBuilt => ConsistencyLevel::Local,
            Self::KnowledgeRemembered | Self::ArtifactStored => ConsistencyLevel::Eventual,
            Self::SessionMutated | Self::ProofAdded => ConsistencyLevel::Strong,
        }
    }
}

/// Consistency requirement for shared context events.
/// Ordered from least to most strict for filtering comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsistencyLevel {
    /// Agent-local, authoritative: session task, local cache, current file set.
    Local = 0,
    /// Shared, eventually consistent: knowledge facts, gotchas, artifact refs.
    Eventual = 1,
    /// Shared, strongly consistent: workspace config, critical decisions.
    Strong = 2,
}

impl ConsistencyLevel {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Eventual => "eventual",
            Self::Strong => "strong",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEventV1 {
    pub id: i64,
    pub workspace_id: String,
    pub channel_id: String,
    pub kind: String,
    pub actor: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<i64>,
    pub consistency_level: String,
    pub payload: Value,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub target_agents: Option<Vec<String>>,
}

impl ContextEventV1 {
    #[must_use]
    pub fn consistency(&self) -> ConsistencyLevel {
        ContextEventKindV1::parse(&self.kind).consistency_level()
    }

    #[must_use]
    pub fn is_visible_to_agent(&self, agent_id: &str) -> bool {
        match &self.target_agents {
            None => true,
            Some(targets) => targets.iter().any(|t| t == agent_id),
        }
    }
}

/// Filter for selective event subscriptions.
/// All fields are optional; `None` means "accept all".
#[derive(Debug, Clone, Default)]
pub struct TopicFilter {
    pub kinds: Option<Vec<ContextEventKindV1>>,
    pub actors: Option<Vec<String>>,
    pub min_consistency: Option<ConsistencyLevel>,
    pub agent_id: Option<String>,
}

impl TopicFilter {
    /// Convenience constructor: filter by event kind strings.
    #[must_use]
    pub fn kinds(kind_strs: &[&str]) -> Self {
        Self {
            kinds: Some(
                kind_strs
                    .iter()
                    .map(|s| ContextEventKindV1::parse(s))
                    .collect(),
            ),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn matches(&self, event: &ContextEventV1) -> bool {
        if let Some(ref kinds) = self.kinds {
            let parsed = ContextEventKindV1::parse(&event.kind);
            if !kinds.contains(&parsed) {
                return false;
            }
        }
        if let Some(ref actors) = self.actors {
            match &event.actor {
                Some(actor) if actors.iter().any(|a| a == actor) => {}
                Some(_) | None => return false,
            }
        }
        if let Some(min) = self.min_consistency
            && event.consistency() < min
        {
            return false;
        }
        if let Some(ref aid) = self.agent_id
            && !event.is_visible_to_agent(aid)
        {
            return false;
        }
        true
    }
}

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContextEventV1> {
    let ts_str: String = row.get(5)?;
    let ts = DateTime::parse_from_rfc3339(&ts_str)
        .map_or_else(|_| Utc::now(), |d| d.with_timezone(&Utc));
    let payload_str: String = row.get(6)?;
    let payload: Value = serde_json::from_str(&payload_str).unwrap_or(Value::Null);
    let kind_str: String = row.get(3)?;
    let cl = ContextEventKindV1::parse(&kind_str)
        .consistency_level()
        .as_str()
        .to_string();
    Ok(ContextEventV1 {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        channel_id: row.get(2)?,
        kind: kind_str,
        actor: row.get::<_, Option<String>>(4)?,
        timestamp: ts,
        version: row.get::<_, i64>(7).unwrap_or(0),
        parent_id: row.get::<_, Option<i64>>(8).ok().flatten(),
        consistency_level: cl,
        payload,
        target_agents: None,
    })
}

#[derive(Clone)]
pub struct ContextBus {
    inner: Arc<Inner>,
}

const STREAM_CHANNEL_SIZE: usize = 256;
const MAX_SUBSCRIBERS_PER_CHANNEL: usize = 64;
/// Bound the write-side version cache. Each entry is re-derivable from the DB via
/// `MAX(version)`, so when the map exceeds this (e.g. a client cycling workspace/channel
/// ids) it is simply cleared — costing at most one extra `MAX()` query per active channel.
const MAX_VERSION_CACHE_ENTRIES: usize = 4096;

struct Inner {
    write_conn: Mutex<Connection>,
    read_pool: Mutex<Vec<Connection>>,
    streams: Mutex<HashMap<String, broadcast::Sender<ContextEventV1>>>,
    version_cache: Mutex<HashMap<String, i64>>,
    db_path: PathBuf,
}

impl Inner {
    fn open_read_conn(path: &PathBuf) -> Connection {
        let conn = Connection::open(path).expect("open read context-os db");
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA query_only=ON;");
        conn
    }

    fn take_read_conn(&self) -> Connection {
        self.read_pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop()
            .unwrap_or_else(|| Self::open_read_conn(&self.db_path))
    }

    fn return_read_conn(&self, conn: Connection) {
        let mut pool = self
            .read_pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pool.len() < MAX_READ_CONNS {
            pool.push(conn);
        }
    }

    fn stream_key(workspace_id: &str, channel_id: &str) -> String {
        format!("{workspace_id}\0{channel_id}")
    }

    fn next_version(&self, workspace_id: &str, channel_id: &str) -> i64 {
        let key = Self::stream_key(workspace_id, channel_id);

        {
            let mut cache = self
                .version_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(v) = cache.get_mut(&key) {
                *v += 1;
                return *v;
            }
        }

        let conn = self.take_read_conn();
        let v: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM context_events WHERE workspace_id = ?1 AND channel_id = ?2",
                params![workspace_id, channel_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        self.return_read_conn(conn);

        let mut cache = self
            .version_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cache.len() > MAX_VERSION_CACHE_ENTRIES {
            // Re-derivable from the DB; drop the whole cache rather than grow unbounded.
            cache.clear();
        }
        let entry = cache.entry(key).or_insert(v);
        *entry = (*entry).max(v) + 1;
        *entry
    }
}

impl Default for ContextBus {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextBus {
    #[must_use]
    pub fn new() -> Self {
        let path = default_db_path();
        Self::open_at(path)
    }

    fn open_at(path: PathBuf) -> Self {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(&path).expect("open context-os db");
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS context_events (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               workspace_id TEXT NOT NULL,
               channel_id TEXT NOT NULL,
               kind TEXT NOT NULL,
               actor TEXT,
               timestamp TEXT NOT NULL,
               payload_json TEXT NOT NULL,
               version INTEGER NOT NULL DEFAULT 0,
               parent_id INTEGER
             );
             CREATE INDEX IF NOT EXISTS idx_context_events_stream
               ON context_events(workspace_id, channel_id, id);",
        )
        .expect("init context-os db");

        let _ = conn.execute_batch(
            "ALTER TABLE context_events ADD COLUMN version INTEGER NOT NULL DEFAULT 0;",
        );
        let _ = conn.execute_batch("ALTER TABLE context_events ADD COLUMN parent_id INTEGER;");

        let _ = conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS context_events_fts USING fts5(
               payload_text,
               content=context_events,
               content_rowid=id
             );",
        );

        let mut read_conns = Vec::with_capacity(MAX_READ_CONNS);
        for _ in 0..MAX_READ_CONNS {
            read_conns.push(Inner::open_read_conn(&path));
        }

        Self {
            inner: Arc::new(Inner {
                write_conn: Mutex::new(conn),
                read_pool: Mutex::new(read_conns),
                streams: Mutex::new(HashMap::new()),
                version_cache: Mutex::new(HashMap::new()),
                db_path: path,
            }),
        }
    }

    pub fn subscribe(
        &self,
        workspace_id: &str,
        channel_id: &str,
    ) -> Option<broadcast::Receiver<ContextEventV1>> {
        let key = Inner::stream_key(workspace_id, channel_id);
        let mut streams = self
            .inner
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Reap senders left behind by departed clients (receiver_count() == 0). A removed
        // key is transparently recreated below; the DB is the durability source so this
        // loses no deliverable events. Bounds `streams` to ~live connection count even
        // under client-cycled workspace/channel ids.
        streams.retain(|_, tx| tx.receiver_count() > 0);
        let tx = streams
            .entry(key)
            .or_insert_with(|| broadcast::channel(STREAM_CHANNEL_SIZE).0);
        if tx.receiver_count() >= MAX_SUBSCRIBERS_PER_CHANNEL {
            tracing::warn!(
                "SSE subscriber cap ({MAX_SUBSCRIBERS_PER_CHANNEL}) reached for {workspace_id}/{channel_id} — rejecting"
            );
            return None;
        }
        Some(tx.subscribe())
    }

    /// Subscribe with a filter — only events matching the filter are delivered.
    /// Returns `(Receiver, TopicFilter)` for use in filtered receive loops.
    #[must_use]
    pub fn subscribe_filtered(
        &self,
        workspace_id: &str,
        channel_id: &str,
        filter: TopicFilter,
    ) -> Option<FilteredSubscription> {
        let rx = self.subscribe(workspace_id, channel_id)?;
        Some(FilteredSubscription { rx, filter })
    }

    #[must_use]
    pub fn append(
        &self,
        workspace_id: &str,
        channel_id: &str,
        kind: &ContextEventKindV1,
        actor: Option<&str>,
        payload: Value,
    ) -> Option<ContextEventV1> {
        self.append_with_parent(workspace_id, channel_id, kind, actor, payload, None)
    }

    #[must_use]
    pub fn append_with_parent(
        &self,
        workspace_id: &str,
        channel_id: &str,
        kind: &ContextEventKindV1,
        actor: Option<&str>,
        payload: Value,
        parent_id: Option<i64>,
    ) -> Option<ContextEventV1> {
        let ev = self.insert_event(
            workspace_id,
            channel_id,
            kind,
            actor,
            payload,
            parent_id,
            None,
        )?;
        self.broadcast_event(&ev);
        Some(ev)
    }

    /// Append an event directed at specific agents only.
    /// Only subscribers whose `TopicFilter.agent_id` matches a target will see it.
    #[must_use]
    pub fn append_directed(
        &self,
        workspace_id: &str,
        channel_id: &str,
        kind: &ContextEventKindV1,
        actor: Option<&str>,
        payload: Value,
        target_agents: Vec<String>,
    ) -> Option<ContextEventV1> {
        let ev = self.insert_event(
            workspace_id,
            channel_id,
            kind,
            actor,
            payload,
            None,
            Some(target_agents),
        )?;
        self.broadcast_event(&ev);
        Some(ev)
    }

    fn insert_event(
        &self,
        workspace_id: &str,
        channel_id: &str,
        kind: &ContextEventKindV1,
        actor: Option<&str>,
        payload: Value,
        parent_id: Option<i64>,
        target_agents: Option<Vec<String>>,
    ) -> Option<ContextEventV1> {
        let ts = Utc::now();
        let payload_json = payload.to_string();

        let (id, version) = {
            let Ok(conn) = self.inner.write_conn.lock() else {
                return None;
            };
            let version = self.inner.next_version(workspace_id, channel_id);

            let result: Result<(i64, i64), rusqlite::Error> = conn
                .execute_batch("BEGIN IMMEDIATE")
                .and_then(|()| {
                    conn.execute(
                        "INSERT INTO context_events (workspace_id, channel_id, kind, actor, timestamp, payload_json, version, parent_id)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            workspace_id,
                            channel_id,
                            kind.as_str(),
                            actor.map(str::to_string),
                            ts.to_rfc3339(),
                            payload_json,
                            version,
                            parent_id,
                        ],
                    )?;
                    let rowid = conn.last_insert_rowid();
                    if let Err(e) = conn.execute(
                        "INSERT INTO context_events_fts(rowid, payload_text) VALUES (?1, ?2)",
                        params![rowid, payload_json],
                    ) {
                        tracing::warn!("FTS insert failed for event {rowid}: {e}");
                    }
                    conn.execute_batch("COMMIT")?;
                    Ok((rowid, version))
                });

            match result {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!("context bus append failed: {e}");
                    let _ = conn.execute_batch("ROLLBACK");
                    return None;
                }
            }
        };

        Some(ContextEventV1 {
            id,
            workspace_id: workspace_id.to_string(),
            channel_id: channel_id.to_string(),
            consistency_level: kind.consistency_level().as_str().to_string(),
            kind: kind.as_str().to_string(),
            actor: actor.map(str::to_string),
            timestamp: ts,
            version,
            parent_id,
            payload,
            target_agents,
        })
    }

    fn broadcast_event(&self, ev: &ContextEventV1) {
        let key = Inner::stream_key(&ev.workspace_id, &ev.channel_id);
        let tx = self
            .inner
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .cloned();
        if let Some(tx) = tx {
            let _ = tx.send(ev.clone());
        }
    }

    #[must_use]
    pub fn read(
        &self,
        workspace_id: &str,
        channel_id: &str,
        since: i64,
        limit: usize,
    ) -> Vec<ContextEventV1> {
        let limit = limit.clamp(1, 1000) as i64;
        let conn = self.inner.take_read_conn();
        let result = (|| {
            let mut stmt = conn.prepare(
                "SELECT id, workspace_id, channel_id, kind, actor, timestamp, payload_json, version, parent_id
                 FROM context_events
                 WHERE workspace_id = ?1 AND channel_id = ?2 AND id > ?3
                 ORDER BY id ASC
                 LIMIT ?4",
            ).ok()?;
            let rows = stmt
                .query_map(
                    params![workspace_id, channel_id, since, limit],
                    event_from_row,
                )
                .ok()?;
            Some(rows.flatten().collect::<Vec<_>>())
        })();
        self.inner.return_read_conn(conn);
        result.unwrap_or_default()
    }

    /// Query recent events of a specific kind (for conflict detection).
    #[must_use]
    pub fn recent_by_kind(
        &self,
        workspace_id: &str,
        channel_id: &str,
        kind: &str,
        limit: usize,
    ) -> Vec<ContextEventV1> {
        let limit = limit.clamp(1, 100) as i64;
        let conn = self.inner.take_read_conn();
        let result = (|| {
            let mut stmt = conn.prepare(
                "SELECT id, workspace_id, channel_id, kind, actor, timestamp, payload_json, version, parent_id
                 FROM context_events
                 WHERE workspace_id = ?1 AND channel_id = ?2 AND kind = ?3
                 ORDER BY id DESC
                 LIMIT ?4",
            ).ok()?;
            let rows = stmt
                .query_map(
                    params![workspace_id, channel_id, kind, limit],
                    event_from_row,
                )
                .ok()?;
            Some(rows.flatten().collect::<Vec<_>>())
        })();
        self.inner.return_read_conn(conn);
        result.unwrap_or_default()
    }

    /// Full-text search over event payloads via FTS5.
    #[must_use]
    pub fn search(
        &self,
        workspace_id: &str,
        channel_id: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Vec<ContextEventV1> {
        let limit = limit.clamp(1, 100) as i64;
        let conn = self.inner.take_read_conn();
        let result =
            if let Some(ch) = channel_id {
                (|| {
                    let mut stmt = conn.prepare(
                    "SELECT e.id, e.workspace_id, e.channel_id, e.kind, e.actor, e.timestamp,
                            e.payload_json, e.version, e.parent_id
                     FROM context_events e
                     JOIN context_events_fts f ON e.id = f.rowid
                     WHERE f.payload_text MATCH ?1 AND e.workspace_id = ?2 AND e.channel_id = ?3
                     ORDER BY f.rank
                     LIMIT ?4",
                ).ok()?;
                    let rows = stmt
                        .query_map(params![query, workspace_id, ch, limit], event_from_row)
                        .ok()?;
                    Some(rows.flatten().collect::<Vec<_>>())
                })()
            } else {
                (|| {
                    let mut stmt = conn.prepare(
                    "SELECT e.id, e.workspace_id, e.channel_id, e.kind, e.actor, e.timestamp,
                            e.payload_json, e.version, e.parent_id
                     FROM context_events e
                     JOIN context_events_fts f ON e.id = f.rowid
                     WHERE f.payload_text MATCH ?1 AND e.workspace_id = ?2
                     ORDER BY f.rank
                     LIMIT ?3",
                ).ok()?;
                    let rows = stmt
                        .query_map(params![query, workspace_id, limit], event_from_row)
                        .ok()?;
                    Some(rows.flatten().collect::<Vec<_>>())
                })()
            };
        self.inner.return_read_conn(conn);
        result.unwrap_or_default()
    }

    /// Trace the causal lineage of an event by following `parent_id` chains.
    /// Only returns events belonging to the given workspace (tenant isolation).
    pub fn lineage(
        &self,
        event_id: i64,
        workspace_id: &str,
        max_depth: usize,
    ) -> Vec<ContextEventV1> {
        let max_depth = max_depth.clamp(1, 50);
        let conn = self.inner.take_read_conn();
        let mut chain = Vec::new();
        let mut current_id = Some(event_id);

        for _ in 0..max_depth {
            let Some(id) = current_id else {
                break;
            };
            let ev = conn.query_row(
                "SELECT id, workspace_id, channel_id, kind, actor, timestamp, payload_json, version, parent_id
                 FROM context_events WHERE id = ?1 AND workspace_id = ?2",
                params![id, workspace_id],
                event_from_row,
            );
            match ev {
                Ok(ev) => {
                    current_id = ev.parent_id;
                    chain.push(ev);
                }
                Err(_) => break,
            }
        }
        self.inner.return_read_conn(conn);
        chain
    }

    /// Returns the highest event id for a workspace/channel pair, or 0 if none.
    #[must_use]
    pub fn latest_id(&self, workspace_id: &str, channel_id: &str) -> i64 {
        let conn = self.inner.take_read_conn();
        let result = conn
            .query_row(
                "SELECT COALESCE(MAX(id), 0) FROM context_events WHERE workspace_id = ?1 AND channel_id = ?2",
                params![workspace_id, channel_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        self.inner.return_read_conn(conn);
        result
    }
}

/// A subscription wrapper that applies a [`TopicFilter`] to received events.
pub struct FilteredSubscription {
    pub rx: broadcast::Receiver<ContextEventV1>,
    pub filter: TopicFilter,
}

impl FilteredSubscription {
    /// Receive the next event that matches the filter.
    /// Skips non-matching events silently.
    pub async fn recv_filtered(&mut self) -> Result<ContextEventV1, broadcast::error::RecvError> {
        loop {
            let ev = self.rx.recv().await?;
            if self.filter.matches(&ev) {
                return Ok(ev);
            }
        }
    }
}

fn default_db_path() -> PathBuf {
    let data = crate::core::data_dir::lean_ctx_data_dir().unwrap_or_else(|_| PathBuf::from("."));
    data.join("context-os").join("context-os.db")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_bus() -> (ContextBus, tempfile::TempDir) {
        let td = tempdir().expect("tempdir");
        let bus = ContextBus::open_at(td.path().join("test-context-os.db"));
        (bus, td)
    }

    #[test]
    fn append_and_read_roundtrip() {
        let (bus, _td) = test_bus();
        let ev = bus
            .append(
                "ws",
                "ch",
                &ContextEventKindV1::ToolCallRecorded,
                Some("agent"),
                serde_json::json!({"tool":"ctx_read"}),
            )
            .expect("append");
        let got = bus.read("ws", "ch", ev.id - 1, 10);
        assert!(got.iter().any(|e| e.id == ev.id));
    }

    #[test]
    fn multi_client_concurrent_appends_have_deterministic_ordering() {
        let (bus, _td) = test_bus();
        let bus = Arc::new(bus);
        let n_clients = 5;
        let n_events_per_client = 20;
        let ws = format!("ws-concurrent-{}", std::process::id());
        let ch = format!("ch-concurrent-{}", std::process::id());

        let mut handles = vec![];
        for client_idx in 0..n_clients {
            let bus = Arc::clone(&bus);
            let ws = ws.clone();
            let ch = ch.clone();
            handles.push(std::thread::spawn(move || {
                let agent = format!("agent-{client_idx}");
                for event_idx in 0..n_events_per_client {
                    bus.append(
                        &ws,
                        &ch,
                        &ContextEventKindV1::ToolCallRecorded,
                        Some(&agent),
                        serde_json::json!({"client": client_idx, "seq": event_idx}),
                    );
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let all = bus.read(&ws, &ch, 0, 1000);
        assert_eq!(
            all.len(),
            n_clients * n_events_per_client,
            "all events should be persisted"
        );

        let ids: Vec<i64> = all.iter().map(|e| e.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "events must be in strictly ascending ID order");

        for win in ids.windows(2) {
            assert!(
                win[1] > win[0],
                "IDs must be strictly monotonic (no gaps from concurrent access)"
            );
        }
    }

    #[test]
    fn workspace_channel_isolation() {
        let (bus, _td) = test_bus();
        let pid = std::process::id();
        let ws_a = format!("ws-iso-a-{pid}");
        let ws_b = format!("ws-iso-b-{pid}");
        let ws_c = format!("ws-iso-c-{pid}");
        let ch1 = format!("ch-iso-1-{pid}");
        let ch2 = format!("ch-iso-2-{pid}");

        bus.append(
            &ws_a,
            &ch1,
            &ContextEventKindV1::SessionMutated,
            Some("agent-a"),
            serde_json::json!({"ws":"a","ch":"1"}),
        );
        bus.append(
            &ws_a,
            &ch2,
            &ContextEventKindV1::KnowledgeRemembered,
            Some("agent-a"),
            serde_json::json!({"ws":"a","ch":"2"}),
        );
        bus.append(
            &ws_b,
            &ch1,
            &ContextEventKindV1::ArtifactStored,
            Some("agent-b"),
            serde_json::json!({"ws":"b","ch":"1"}),
        );

        let ws_a_ch_1 = bus.read(&ws_a, &ch1, 0, 100);
        assert_eq!(ws_a_ch_1.len(), 1);
        assert_eq!(ws_a_ch_1[0].kind, "session_mutated");

        let ws_a_ch_2 = bus.read(&ws_a, &ch2, 0, 100);
        assert_eq!(ws_a_ch_2.len(), 1);
        assert_eq!(ws_a_ch_2[0].kind, "knowledge_remembered");

        let ws_b_ch_1 = bus.read(&ws_b, &ch1, 0, 100);
        assert_eq!(ws_b_ch_1.len(), 1);
        assert_eq!(ws_b_ch_1[0].kind, "artifact_stored");

        let ws_c_ch_1 = bus.read(&ws_c, &ch1, 0, 100);
        assert!(ws_c_ch_1.is_empty(), "non-existent workspace returns empty");
    }

    #[test]
    fn replay_from_cursor_returns_only_newer_events() {
        let (bus, _td) = test_bus();
        let pid = std::process::id();
        let ws = &format!("ws-replay-{pid}");
        let ch = &format!("ch-replay-{pid}");

        let ev1 = bus
            .append(
                ws,
                ch,
                &ContextEventKindV1::ToolCallRecorded,
                None,
                serde_json::json!({"seq":1}),
            )
            .unwrap();
        let ev2 = bus
            .append(
                ws,
                ch,
                &ContextEventKindV1::SessionMutated,
                None,
                serde_json::json!({"seq":2}),
            )
            .unwrap();
        let _ev3 = bus
            .append(
                ws,
                ch,
                &ContextEventKindV1::GraphBuilt,
                None,
                serde_json::json!({"seq":3}),
            )
            .unwrap();

        let from_cursor = bus.read(ws, ch, ev2.id, 100);
        assert_eq!(from_cursor.len(), 1, "only events after cursor");
        assert_eq!(from_cursor[0].kind, "graph_built");

        let from_first = bus.read(ws, ch, ev1.id, 100);
        assert_eq!(from_first.len(), 2, "events after first");

        let from_zero = bus.read(ws, ch, 0, 100);
        assert_eq!(from_zero.len(), 3, "all events from zero");
    }

    #[test]
    fn broadcast_subscriber_receives_events() {
        let (bus, _td) = test_bus();
        let mut rx = bus.subscribe("ws", "ch").expect("subscribe should succeed");

        let ev = bus
            .append(
                "ws",
                "ch",
                &ContextEventKindV1::ProofAdded,
                Some("verifier"),
                serde_json::json!({"proof":"hash"}),
            )
            .unwrap();

        let received = rx.try_recv().expect("subscriber should receive event");
        assert_eq!(received.id, ev.id);
        assert_eq!(received.kind, "proof_added");
        assert_eq!(received.actor.as_deref(), Some("verifier"));
    }
}
