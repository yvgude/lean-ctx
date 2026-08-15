//! Persistent outcome attribution for context supplied to an agent.
//!
//! Scores are correlations, not proof of causality: every chunk supplied to a
//! successful session receives +1.0, and every chunk supplied to a failed
//! session receives -0.5. The deliberately simple model is a durable baseline
//! for deciding which context earns its token cost.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const ATTRIBUTIONS_FILE: &str = "attributions.jsonl";
const SUMMARY_CHARS: usize = 100;

/// A context chunk supplied to an agent during a conversation turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextChunkRecord {
    /// BLAKE3 digest of the complete chunk content.
    pub id: String,
    /// Tool and arguments that produced the chunk, for example `ctx_read src/main.rs`.
    pub source: String,
    /// Tokens consumed by this chunk.
    pub token_cost: usize,
    /// Conversation turn that received the chunk.
    pub turn_provided: u64,
    /// Bounded diagnostic preview; full content is never persisted here.
    pub content_summary: String,
}

impl ContextChunkRecord {
    /// Creates a record while deriving its stable content identifier and preview.
    pub fn new(
        content: &str,
        source: impl Into<String>,
        token_cost: usize,
        turn_provided: u64,
    ) -> Self {
        Self {
            id: crate::core::hasher::hash_str(content),
            source: source.into(),
            token_cost,
            turn_provided,
            content_summary: content.chars().take(SUMMARY_CHARS).collect(),
        }
    }
}

/// The observed result for a session.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure,
    Partial,
    Unknown,
}

/// A durable, human-readable outcome signal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutcomeSignal {
    pub session_id: String,
    pub outcome: Outcome,
    pub evidence: String,
}

/// Aggregated usefulness of one content-identical context chunk.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkAttribution {
    pub source: String,
    pub avg_score: f64,
    pub times_provided: u64,
    pub times_helpful: u64,
    /// Net outcome score per token spent supplying the chunk.
    pub efficiency_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum AttributionEvent {
    Chunk {
        session_id: String,
        chunk: ContextChunkRecord,
    },
    Outcome {
        outcome: OutcomeSignal,
    },
}

/// JSONL-backed context/outcome recorder.
#[derive(Debug)]
pub struct CausalAttributor {
    path: PathBuf,
    events: Mutex<Vec<AttributionEvent>>,
}

impl CausalAttributor {
    /// Opens an attribution store, retaining all historical observations.
    pub fn open(path: PathBuf) -> Result<Self, String> {
        Ok(Self {
            events: Mutex::new(load_events(&path)?),
            path,
        })
    }

    /// Opens a store, treating an unreadable prior store as empty.
    pub fn new(path: PathBuf) -> Self {
        Self::open(path.clone()).unwrap_or_else(|_| Self {
            path,
            events: Mutex::new(Vec::new()),
        })
    }

    /// Default XDG data path: `~/.local/share/lean-ctx/attributions.jsonl`.
    pub fn default_path() -> Result<PathBuf, String> {
        crate::core::data_dir::lean_ctx_data_dir().map(|dir| dir.join(ATTRIBUTIONS_FILE))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Records one chunk supplied to `session_id`.
    pub fn record_chunk(&self, session_id: &str, chunk: ContextChunkRecord) -> Result<(), String> {
        let event = AttributionEvent::Chunk {
            session_id: session_id.to_owned(),
            chunk,
        };
        self.append(event)
    }

    /// Records a terminal session outcome. Repeated terminal signals are ignored.
    pub fn record_outcome(&self, session_id: &str, outcome: OutcomeSignal) -> Result<(), String> {
        if outcome.session_id != session_id {
            return Err("outcome session_id must match record_outcome session_id".to_owned());
        }

        let mut events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        if events.iter().any(|event| {
            matches!(event, AttributionEvent::Outcome { outcome } if outcome.session_id == session_id)
        }) {
            return Ok(());
        }

        let event = AttributionEvent::Outcome { outcome };
        append_event(&self.path, &event)?;
        events.push(event);
        Ok(())
    }

    /// Calculates attribution from every durable chunk and terminal outcome.
    pub fn compute_attributions(&self) -> Vec<ChunkAttribution> {
        let events = self
            .events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        aggregate(&events)
    }

    /// Returns the exact context chunks supplied before this session's outcome.
    pub fn chunks_for_session(&self, session_id: &str) -> Vec<ContextChunkRecord> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter_map(|event| match event {
                AttributionEvent::Chunk {
                    session_id: recorded_session,
                    chunk,
                } if recorded_session == session_id => Some(chunk.clone()),
                AttributionEvent::Outcome { .. } | AttributionEvent::Chunk { .. } => None,
            })
            .collect()
    }

    /// Returns sources whose repeated observations have no positive outcome value.
    pub fn suggest_removals(&self) -> Vec<String> {
        let mut candidates: Vec<_> = self
            .compute_attributions()
            .into_iter()
            .filter(|attribution| {
                attribution.times_provided >= 2
                    && attribution.times_helpful == 0
                    && attribution.avg_score <= 0.0
            })
            .collect();
        candidates.sort_by(|left, right| {
            left.avg_score
                .total_cmp(&right.avg_score)
                .then_with(|| left.source.cmp(&right.source))
        });
        candidates
            .into_iter()
            .map(|candidate| candidate.source)
            .collect()
    }

    fn append(&self, event: AttributionEvent) -> Result<(), String> {
        let mut events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        append_event(&self.path, &event)?;
        events.push(event);
        Ok(())
    }
}

fn load_events(path: &Path) -> Result<Vec<AttributionEvent>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    BufReader::new(file)
        .lines()
        .filter_map(|line| match line {
            Ok(line) if line.trim().is_empty() => None,
            Ok(line) => Some(Ok(line)),
            Err(error) => Some(Err(format!("read {}: {error}", path.display()))),
        })
        .map(|line| {
            let line = line?;
            serde_json::from_str(&line)
                .map_err(|error| format!("parse {}: {error}", path.display()))
        })
        .collect()
}

fn append_event(path: &Path, event: &AttributionEvent) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("open {}: {error}", path.display()))?;
    serde_json::to_writer(&mut file, event).map_err(|error| format!("serialize event: {error}"))?;
    file.write_all(b"\n")
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    file.flush()
        .map_err(|error| format!("flush {}: {error}", path.display()))
}

#[derive(Default)]
struct Aggregate {
    source: String,
    times_provided: u64,
    times_helpful: u64,
    total_score: f64,
    total_token_cost: u64,
}

fn aggregate(events: &[AttributionEvent]) -> Vec<ChunkAttribution> {
    let mut sessions: HashMap<&str, Vec<&ContextChunkRecord>> = HashMap::new();
    let mut completed = HashSet::new();
    let mut chunks: BTreeMap<&str, Aggregate> = BTreeMap::new();

    for event in events {
        match event {
            AttributionEvent::Chunk { session_id, chunk } => {
                sessions.entry(session_id).or_default().push(chunk);
                let aggregate = chunks.entry(&chunk.id).or_insert_with(|| Aggregate {
                    source: chunk.source.clone(),
                    ..Aggregate::default()
                });
                aggregate.times_provided = aggregate.times_provided.saturating_add(1);
                aggregate.total_token_cost = aggregate
                    .total_token_cost
                    .saturating_add(chunk.token_cost as u64);
            }
            AttributionEvent::Outcome { outcome } if completed.insert(&outcome.session_id) => {
                let (score, helpful) = score(outcome.outcome);
                let provided = sessions
                    .remove(outcome.session_id.as_str())
                    .unwrap_or_default();
                for chunk in provided {
                    if let Some(aggregate) = chunks.get_mut(chunk.id.as_str()) {
                        aggregate.total_score += score;
                        if helpful {
                            aggregate.times_helpful = aggregate.times_helpful.saturating_add(1);
                        }
                    }
                }
            }
            AttributionEvent::Outcome { .. } => {}
        }
    }

    let mut attributions: Vec<_> = chunks
        .into_values()
        .map(|aggregate| ChunkAttribution {
            source: aggregate.source,
            avg_score: average(aggregate.total_score, aggregate.times_provided),
            times_provided: aggregate.times_provided,
            times_helpful: aggregate.times_helpful,
            efficiency_ratio: average(aggregate.total_score, aggregate.total_token_cost),
        })
        .collect();
    attributions.sort_by(|left, right| {
        right
            .avg_score
            .total_cmp(&left.avg_score)
            .then_with(|| left.source.cmp(&right.source))
    });
    attributions
}

fn score(outcome: Outcome) -> (f64, bool) {
    match outcome {
        Outcome::Success => (1.0, true),
        Outcome::Failure => (-0.5, false),
        Outcome::Partial => (0.5, true),
        Outcome::Unknown => (0.0, false),
    }
}

fn average(total: f64, count: u64) -> f64 {
    if count == 0 {
        0.0
    } else {
        total / count as f64
    }
}

fn global() -> &'static CausalAttributor {
    static ATTRIBUTOR: OnceLock<CausalAttributor> = OnceLock::new();
    ATTRIBUTOR.get_or_init(|| {
        CausalAttributor::default_path()
            .map(CausalAttributor::new)
            .unwrap_or_else(|_| CausalAttributor::new(PathBuf::from(ATTRIBUTIONS_FILE)))
    })
}

/// Records a chunk in the process-wide persistent attribution store.
pub fn record_chunk(session_id: &str, chunk: ContextChunkRecord) -> Result<(), String> {
    global().record_chunk(session_id, chunk)
}

/// Records a terminal session outcome in the process-wide persistent store.
pub fn record_outcome(session_id: &str, outcome: OutcomeSignal) -> Result<(), String> {
    global().record_outcome(session_id, outcome)
}

/// Computes process-wide durable attributions.
pub fn compute_attributions() -> Vec<ChunkAttribution> {
    global().compute_attributions()
}

/// Returns the chunks supplied to one session for outcome-level attribution.
pub fn chunks_for_session(session_id: &str) -> Vec<ContextChunkRecord> {
    global().chunks_for_session(session_id)
}

/// Returns process-wide candidates for removal from future context.
pub fn suggest_removals() -> Vec<String> {
    global().suggest_removals()
}

/// Records textual context carried by a proxied request.
///
/// This is intentionally best-effort: attribution must never block a provider
/// request if local state storage is unavailable.
pub fn record_proxy_context(
    session_id: &str,
    request: &Value,
    turn_provided: u64,
) -> Result<usize, String> {
    let mut values = Vec::new();
    collect_request_context(request, &mut values);
    let mut seen = HashSet::new();
    let mut recorded = 0;
    for (source, content) in values {
        let chunk =
            ContextChunkRecord::new(&content, source, estimate_tokens(&content), turn_provided);
        if seen.insert(chunk.id.clone()) {
            record_chunk(session_id, chunk)?;
            recorded += 1;
        }
    }
    Ok(recorded)
}

fn collect_request_context(request: &Value, values: &mut Vec<(String, String)>) {
    if let Some(system) = request.get("system") {
        collect_text(system, "proxy system", values);
    }
    for key in ["messages", "input"] {
        if let Some(Value::Array(messages)) = request.get(key) {
            for message in messages {
                collect_message(message, key, values);
            }
        } else if let Some(Value::String(text)) = request.get(key) {
            values.push((format!("proxy {key}"), text.clone()));
        }
    }
}

fn collect_message(message: &Value, field: &str, values: &mut Vec<(String, String)>) {
    let Some(object) = message.as_object() else {
        collect_text(message, &format!("proxy {field}"), values);
        return;
    };
    let role = object
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("context");
    let source = if role == "tool" {
        let name = object
            .get("name")
            .or_else(|| object.get("tool_call_id"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        format!("tool_output {name}")
    } else {
        format!("proxy {role}")
    };
    for key in ["content", "output"] {
        if let Some(content) = object.get(key) {
            collect_text(content, &source, values);
        }
    }
}

fn collect_text(value: &Value, source: &str, values: &mut Vec<(String, String)>) {
    match value {
        Value::String(text) if !text.is_empty() => values.push((source.to_owned(), text.clone())),
        Value::Array(items) => {
            for item in items {
                collect_text(item, source, values);
            }
        }
        Value::Object(object) => {
            for key in ["text", "input_text", "content"] {
                if let Some(text) = object.get(key) {
                    collect_text(text, source, values);
                }
            }
        }
        _ => {}
    }
}

fn estimate_tokens(content: &str) -> usize {
    content.len().saturating_add(3) / 4
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> CausalAttributor {
        let path = std::env::temp_dir().join(format!(
            "lean-ctx-causal-attribution-{name}-{}.jsonl",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        CausalAttributor::open(path).expect("open test store")
    }

    fn record(store: &CausalAttributor, session: &str, content: &str, source: &str) {
        store
            .record_chunk(session, ContextChunkRecord::new(content, source, 10, 1))
            .expect("record chunk");
    }

    fn outcome(store: &CausalAttributor, session: &str, outcome: Outcome) {
        store
            .record_outcome(
                session,
                OutcomeSignal {
                    session_id: session.to_owned(),
                    outcome,
                    evidence: "test signal".to_owned(),
                },
            )
            .expect("record outcome");
    }

    #[test]
    fn successful_outcome_increases_chunk_score() {
        let store = store("success");
        record(&store, "session", "useful context", "ctx_read src/main.rs");
        outcome(&store, "session", Outcome::Success);

        let attribution = store.compute_attributions().pop().expect("attribution");
        assert_eq!(attribution.avg_score, 1.0);
        assert_eq!(attribution.times_helpful, 1);
    }

    #[test]
    fn failed_outcome_decreases_chunk_score() {
        let store = store("failure");
        record(&store, "session", "noisy context", "ctx_read src/noise.rs");
        outcome(&store, "session", Outcome::Failure);

        assert_eq!(store.compute_attributions()[0].avg_score, -0.5);
    }

    #[test]
    fn compute_attributions_sorts_by_score() {
        let store = store("sorted");
        record(&store, "success", "useful", "ctx_read src/useful.rs");
        outcome(&store, "success", Outcome::Success);
        record(&store, "failure", "noise", "ctx_read src/noise.rs");
        outcome(&store, "failure", Outcome::Failure);

        let attributions = store.compute_attributions();
        assert_eq!(attributions[0].source, "ctx_read src/useful.rs");
        assert_eq!(attributions[1].source, "ctx_read src/noise.rs");
    }

    #[test]
    fn suggest_removals_identifies_negative_chunks() {
        let store = store("removals");
        for session in ["first", "second"] {
            record(&store, session, "repeated noise", "ctx_read src/noise.rs");
            outcome(&store, session, Outcome::Failure);
        }

        assert_eq!(store.suggest_removals(), vec!["ctx_read src/noise.rs"]);
    }

    #[test]
    fn persistence_round_trip_works() {
        let store = store("round-trip");
        let path = store.path().to_path_buf();
        record(&store, "session", "durable context", "ctx_read src/lib.rs");
        outcome(&store, "session", Outcome::Success);
        drop(store);

        let reopened = CausalAttributor::open(path).expect("reopen store");
        let attribution = reopened.compute_attributions().pop().expect("attribution");
        assert_eq!(attribution.source, "ctx_read src/lib.rs");
        assert_eq!(attribution.avg_score, 1.0);
    }

    #[test]
    fn chunks_for_session_excludes_other_sessions_and_outcomes() {
        let store = store("session-chunks");
        record(&store, "accepted", "useful context", "ctx_read src/lib.rs");
        record(
            &store,
            "other",
            "unrelated context",
            "ctx_search regex other",
        );
        outcome(&store, "accepted", Outcome::Success);

        let chunks = store.chunks_for_session("accepted");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].source, "ctx_read src/lib.rs");
    }

    #[test]
    fn proxy_context_records_tool_output_with_bounded_summary() {
        let mut values = Vec::new();
        collect_request_context(
            &serde_json::json!({
                "messages": [{
                    "role": "tool",
                    "name": "ctx_read src/main.rs",
                    "content": "x".repeat(SUMMARY_CHARS + 1)
                }]
            }),
            &mut values,
        );

        let chunk = ContextChunkRecord::new(&values[0].1, &values[0].0, 1, 1);
        assert_eq!(chunk.source, "tool_output ctx_read src/main.rs");
        assert_eq!(chunk.content_summary.chars().count(), SUMMARY_CHARS);
    }
}
