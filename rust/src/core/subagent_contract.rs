//! Sub-agent context contracts (GL#450).
//!
//! A briefing pack is the *contract* between a parent agent and a sub-agent:
//! task, token budget, the facts the sub-agent needs, and the return format
//! it must produce. Packs are deterministic — same input, byte-identical
//! output — so they can be diffed, cached, and replayed. The return channel
//! is the inverse: a sub-agent reports `category/key: value` lines, which the
//! parent distills into recallable knowledge facts instead of raw transcript.

use serde::{Deserialize, Serialize};

use crate::core::knowledge::ProjectKnowledge;
use crate::core::tokens::count_tokens;

/// Versioned briefing pack. Field order is fixed by this struct; serialization
/// uses `serde_json::to_string_pretty` which preserves struct order, keeping
/// the bytes stable across runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentContractV1 {
    pub contract_version: u32,
    pub task: String,
    pub budget_tokens: usize,
    pub used_tokens: usize,
    pub project_hash: String,
    pub facts: Vec<ContractFact>,
    pub return_format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractFact {
    pub category: String,
    pub key: String,
    pub value: String,
}

/// Instruction embedded in every pack telling the sub-agent how to report.
pub const RETURN_FORMAT_V1: &str = "Report results as lines of 'category/key: value'. \
     Each line becomes a recallable fact in the parent's knowledge store. \
     Keep values self-contained; no transcript dumps.";

/// Build a deterministic briefing pack: task + the most relevant current
/// facts, greedily filled (in stable relevance order) until `budget_tokens`
/// is reached. The task itself is always included; the budget governs facts.
#[must_use]
pub fn build_briefing_pack(
    knowledge: &ProjectKnowledge,
    task: &str,
    budget_tokens: usize,
) -> SubAgentContractV1 {
    let mut used = count_tokens(task);

    // Deterministic relevance: term-coverage on the lexical index, quality
    // tie-break, then stable (category, key) ordering. recall() already
    // filters to current facts and sorts deterministically.
    let ranked = knowledge.recall(task);

    let mut facts: Vec<ContractFact> = Vec::new();
    for f in ranked {
        let line_tokens = count_tokens(&format!("{}/{}: {}", f.category, f.key, f.value));
        if used + line_tokens > budget_tokens {
            continue;
        }
        used += line_tokens;
        facts.push(ContractFact {
            category: f.category.clone(),
            key: f.key.clone(),
            value: f.value.clone(),
        });
    }

    SubAgentContractV1 {
        contract_version: 1,
        task: task.to_string(),
        budget_tokens,
        used_tokens: used,
        project_hash: knowledge.project_hash.clone(),
        facts,
        return_format: RETURN_FORMAT_V1.to_string(),
    }
}

/// Serialize a pack with stable formatting (struct field order, pretty JSON).
pub fn serialize_pack(pack: &SubAgentContractV1) -> Result<String, String> {
    serde_json::to_string_pretty(pack).map_err(|e| format!("contract serialization failed: {e}"))
}

/// Parse sub-agent return lines (`category/key: value`) into structured
/// facts. Lines that don't match the contract format are reported back as
/// rejects instead of being silently dropped.
#[must_use]
pub fn parse_return_lines(input: &str) -> (Vec<ContractFact>, Vec<String>) {
    let mut facts = Vec::new();
    let mut rejected = Vec::new();

    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parsed = line.split_once(": ").and_then(|(head, value)| {
            let (category, key) = head.split_once('/')?;
            let category = category.trim();
            let key = key.trim();
            let value = value.trim();
            if category.is_empty() || key.is_empty() || value.is_empty() {
                return None;
            }
            Some(ContractFact {
                category: category.to_string(),
                key: key.to_string(),
                value: value.to_string(),
            })
        });
        match parsed {
            Some(f) => facts.push(f),
            None => rejected.push(line.to_string()),
        }
    }

    (facts, rejected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::memory_policy::MemoryPolicy;

    fn knowledge_with_facts() -> ProjectKnowledge {
        let policy = MemoryPolicy::default();
        let mut k = ProjectKnowledge::new("/tmp/contract-test");
        k.remember(
            "architecture",
            "auth",
            "JWT RS256 authentication",
            "s1",
            0.9,
            &policy,
        );
        k.remember(
            "architecture",
            "db",
            "PostgreSQL 16 with pgvector",
            "s1",
            0.85,
            &policy,
        );
        k.remember("deploy", "host", "AWS eu-central-1", "s1", 0.8, &policy);
        k
    }

    #[test]
    fn briefing_pack_is_deterministic() {
        let k = knowledge_with_facts();
        let a = serialize_pack(&build_briefing_pack(&k, "fix authentication bug", 500)).unwrap();
        let b = serialize_pack(&build_briefing_pack(&k, "fix authentication bug", 500)).unwrap();
        assert_eq!(a, b, "same input must produce byte-identical packs");
    }

    #[test]
    fn briefing_pack_respects_budget() {
        let k = knowledge_with_facts();
        let tight = build_briefing_pack(&k, "authentication database deployment", 30);
        assert!(
            tight.used_tokens <= 30,
            "used {} > budget 30",
            tight.used_tokens
        );
        let roomy = build_briefing_pack(&k, "authentication database deployment", 5000);
        assert!(roomy.facts.len() >= tight.facts.len());
    }

    #[test]
    fn briefing_pack_includes_relevant_fact() {
        let k = knowledge_with_facts();
        let pack = build_briefing_pack(&k, "fix authentication bug", 500);
        assert!(
            pack.facts.iter().any(|f| f.key == "auth"),
            "auth fact must be selected for an authentication task: {:?}",
            pack.facts
        );
        assert_eq!(pack.contract_version, 1);
        assert_eq!(pack.return_format, RETURN_FORMAT_V1);
    }

    #[test]
    fn parse_return_accepts_contract_lines() {
        let (facts, rejected) = parse_return_lines(
            "finding/root-cause: race in session save\n\
             decision/fix: serialize via mutate_locked\n\
             \n\
             this line is not contract formatted",
        );
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].category, "finding");
        assert_eq!(facts[0].key, "root-cause");
        assert_eq!(rejected.len(), 1);
    }

    #[test]
    fn parse_return_rejects_empty_parts() {
        let (facts, rejected) = parse_return_lines("/key: value\ncat/: value\ncat/key:    ");
        assert!(facts.is_empty());
        assert_eq!(rejected.len(), 3);
    }
}
