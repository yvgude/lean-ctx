//! memory adapter (#1100): Mem0 / OpenMemory / Cognee / Letta search output →
//! `ctx_knowledge` facts, so recalled memories become first-class lean-ctx
//! knowledge (searchable, ranked, deduplicated) instead of an opaque blob.
//!
//! Defensive JSON parsing accepts the common shapes:
//!   - `{ "results":  [ {memory|text|content, id?, score?}, … ] }` (Mem0)
//!   - `{ "memories": [ … ] }` / a top-level array
//! Each memory is remembered under the `addon_memory` category, keyed by the
//! provider id (or a stable content hash) so re-ingest updates in place. Runs as
//! a side-channel; never panics.

use crate::core::knowledge::ProjectKnowledge;
use crate::core::memory_policy::MemoryPolicy;

struct ParsedMemory {
    key: String,
    value: String,
    confidence: f32,
}

/// Default confidence when the provider returns no relevance score.
const DEFAULT_CONFIDENCE: f32 = 0.7;

/// Parse memories from `text` and remember them as knowledge facts.
pub fn ingest(server: &str, _tool: &str, text: &str, project_root: &str) {
    let memories = parse_memories(text);
    if memories.is_empty() {
        return;
    }
    let mut knowledge =
        ProjectKnowledge::load(project_root).unwrap_or_else(|| ProjectKnowledge::new(project_root));
    let policy = MemoryPolicy::default();
    let session = format!("addon:{server}");
    for m in &memories {
        knowledge.remember(
            "addon_memory",
            &m.key,
            &m.value,
            &session,
            m.confidence,
            &policy,
        );
    }
    if knowledge.save().is_err() {
        tracing::warn!("[memory adapter] knowledge save failed for `{server}`");
    }
}

fn parse_memories(text: &str) -> Vec<ParsedMemory> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim()) else {
        return Vec::new();
    };
    let items = v
        .get("results")
        .or_else(|| v.get("memories"))
        .or_else(|| v.get("data"))
        .and_then(serde_json::Value::as_array)
        .or_else(|| v.as_array());
    let Some(items) = items else {
        return Vec::new();
    };
    items.iter().filter_map(parse_one).collect()
}

fn parse_one(item: &serde_json::Value) -> Option<ParsedMemory> {
    let obj = item.as_object()?;
    let value = ["memory", "text", "content", "fact", "data"]
        .iter()
        .filter_map(|k| obj.get(*k).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .find(|s| !s.is_empty())?
        .to_string();

    let key = ["id", "memory_id", "uuid", "hash"]
        .iter()
        .filter_map(|k| obj.get(*k).and_then(serde_json::Value::as_str))
        .find(|s| !s.is_empty())
        .map_or_else(|| stable_key(&value), String::from);

    let confidence = ["score", "relevance", "similarity"]
        .iter()
        .filter_map(|k| obj.get(*k).and_then(serde_json::Value::as_f64))
        .next()
        .map_or(DEFAULT_CONFIDENCE, |s| (s as f32).clamp(0.0, 1.0));

    Some(ParsedMemory {
        key,
        value,
        confidence,
    })
}

/// Stable, collision-resistant key derived from the memory text, so the same
/// memory re-ingested updates rather than duplicates.
fn stable_key(value: &str) -> String {
    format!("mem_{}", &blake3::hash(value.as_bytes()).to_hex()[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mem0_results_shape() {
        let payload = r#"{"results":[
            {"memory":"prefers tabs over spaces","id":"m1","score":0.91},
            {"text":"deploy runs on fridays"}
        ]}"#;
        let mems = parse_memories(payload);
        assert_eq!(mems.len(), 2);
        assert_eq!(mems[0].key, "m1");
        assert!((mems[0].confidence - 0.91).abs() < 1e-5);
        // No id → stable content-derived key, default confidence.
        assert!(mems[1].key.starts_with("mem_"));
        assert!((mems[1].confidence - DEFAULT_CONFIDENCE).abs() < 1e-5);
    }

    #[test]
    fn stable_key_is_deterministic() {
        assert_eq!(stable_key("same"), stable_key("same"));
        assert_ne!(stable_key("a"), stable_key("b"));
    }

    #[test]
    fn ingest_writes_knowledge_facts() {
        let _lock = crate::core::data_dir::test_env_lock();
        let proj = tempfile::tempdir().unwrap();
        let root = proj.path().to_str().unwrap();

        ingest(
            "mem0",
            "search_memories",
            r#"{"results":[{"memory":"the user prefers Rust","id":"u1","score":0.8}]}"#,
            root,
        );

        let knowledge = ProjectKnowledge::load(root).expect("knowledge persisted");
        assert!(
            knowledge
                .facts
                .iter()
                .any(|f| f.category == "addon_memory" && f.value.contains("prefers Rust")),
            "memory must become an addon_memory knowledge fact"
        );
    }
}
