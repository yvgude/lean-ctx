mod core;
mod fact;
mod format;
mod import_export;
mod persist;
mod query;
mod ranking;
mod types;

pub use import_export::{parse_import_data, ImportMerge, ImportResult, SimpleFactEntry};
pub use ranking::{find_cross_key_similar, SimilarFact};
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::memory_boundary::FactPrivacy;
    use crate::core::memory_policy::MemoryPolicy;
    use chrono::Utc;

    fn default_policy() -> MemoryPolicy {
        MemoryPolicy::default()
    }

    #[test]
    fn remember_and_recall() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test-project");
        k.remember(
            "architecture",
            "auth",
            "JWT RS256",
            "session-1",
            0.9,
            &policy,
        );
        k.remember("api", "rate-limit", "100/min", "session-1", 0.8, &policy);

        let results = k.recall("auth");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].value, "JWT RS256");

        let results = k.recall("api rate");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "rate-limit");
    }

    #[test]
    fn facts_evict_down_to_cap_not_double() {
        // Regression: remember() must keep the fact count at or below max_facts.
        // Previously the lifecycle only fired above 2 * max_facts, so a store
        // could silently grow to twice its configured budget before reclaiming.
        let mut policy = default_policy();
        policy.knowledge.max_facts = 5;
        let mut k = ProjectKnowledge::new("/tmp/test-evict");
        for i in 0..40 {
            k.remember(
                "finding",
                &format!("key-{i}"),
                &format!("value number {i}"),
                "s1",
                0.7,
                &policy,
            );
        }
        assert!(
            k.facts.len() <= policy.knowledge.max_facts,
            "expected <= {} facts after eviction, got {}",
            policy.knowledge.max_facts,
            k.facts.len()
        );
    }

    #[test]
    fn upsert_existing_fact() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.7, &policy);
        k.remember(
            "arch",
            "db",
            "PostgreSQL 16 with pgvector",
            "s2",
            0.95,
            &policy,
        );

        let current: Vec<_> = k.facts.iter().filter(|f| f.is_current()).collect();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].value, "PostgreSQL 16 with pgvector");
    }

    #[test]
    fn contradiction_detection() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95, &policy);
        k.facts[0].confirmation_count = 3;

        let contradiction = k.check_contradiction("arch", "db", "MySQL", &policy);
        assert!(contradiction.is_some());
        let c = contradiction.unwrap();
        assert_eq!(c.severity, ContradictionSeverity::High);
    }

    #[test]
    fn temporal_validity() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95, &policy);
        k.facts[0].confirmation_count = 3;

        k.remember("arch", "db", "MySQL", "s2", 0.9, &policy);

        let current: Vec<_> = k.facts.iter().filter(|f| f.is_current()).collect();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].value, "MySQL");

        let all_db: Vec<_> = k.facts.iter().filter(|f| f.key == "db").collect();
        assert_eq!(all_db.len(), 2);
    }

    #[test]
    fn confirmation_count() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.9, &policy);
        assert_eq!(k.facts[0].confirmation_count, 1);

        k.remember("arch", "db", "PostgreSQL", "s2", 0.9, &policy);
        assert_eq!(k.facts[0].confirmation_count, 2);
    }

    #[test]
    fn remove_fact() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.9, &policy);
        assert!(k.remove_fact("arch", "db"));
        assert!(k.facts.is_empty());
        assert!(!k.remove_fact("arch", "db"));
    }

    #[test]
    fn list_rooms() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("architecture", "auth", "JWT", "s1", 0.9, &policy);
        k.remember("architecture", "db", "PG", "s1", 0.9, &policy);
        k.remember("deploy", "host", "AWS", "s1", 0.8, &policy);

        let rooms = k.list_rooms();
        assert_eq!(rooms.len(), 2);
    }

    #[test]
    fn aaak_format() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("architecture", "auth", "JWT RS256", "s1", 0.95, &policy);
        k.remember("architecture", "db", "PostgreSQL", "s1", 0.7, &policy);

        let aaak = k.format_aaak();
        assert!(aaak.contains("ARCHITECTURE:"));
        assert!(aaak.contains("auth=JWT RS256"));
    }

    #[test]
    fn consolidate_history() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.consolidate(
            "Migrated from REST to GraphQL",
            vec!["s1".into(), "s2".into()],
            &policy,
        );
        assert_eq!(k.history.len(), 1);
        assert_eq!(k.history[0].from_sessions.len(), 2);
    }

    #[test]
    fn format_summary_output() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("architecture", "auth", "JWT RS256", "s1", 0.9, &policy);
        k.add_pattern(
            "naming",
            "snake_case for functions",
            vec!["get_user()".into()],
            "s1",
            &policy,
        );
        let summary = k.format_summary();
        assert!(summary.contains("PROJECT KNOWLEDGE:"));
        assert!(summary.contains("auth: JWT RS256"));
        assert!(summary.contains("PROJECT PATTERNS:"));
    }

    #[test]
    fn temporal_recall_at_time() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95, &policy);
        k.facts[0].confirmation_count = 3;

        let before_change = Utc::now();
        std::thread::sleep(std::time::Duration::from_millis(10));

        k.remember("arch", "db", "MySQL", "s2", 0.9, &policy);

        let results = k.recall_at_time("db", before_change);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].value, "PostgreSQL");

        let results_now = k.recall_at_time("db", Utc::now());
        assert_eq!(results_now.len(), 1);
        assert_eq!(results_now[0].value, "MySQL");
    }

    #[test]
    fn timeline_shows_history() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95, &policy);
        k.facts[0].confirmation_count = 3;
        k.remember("arch", "db", "MySQL", "s2", 0.9, &policy);

        let timeline = k.timeline("arch");
        assert_eq!(timeline.len(), 2);
        assert!(!timeline[0].is_current());
        assert!(timeline[1].is_current());
    }

    #[test]
    fn wakeup_format() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "auth", "JWT", "s1", 0.95, &policy);
        k.remember("arch", "db", "PG", "s1", 0.8, &policy);

        let wakeup = k.format_wakeup();
        assert!(wakeup.contains("FACTS:"));
        assert!(wakeup.contains("arch/auth=JWT"));
        assert!(wakeup.contains("arch/db=PG"));
    }

    #[test]
    fn salience_prioritizes_decisions_over_findings_at_similar_confidence() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("finding", "f1", "some thing", "s1", 0.9, &policy);
        k.remember("decision", "d1", "important", "s1", 0.85, &policy);

        let wakeup = k.format_wakeup();
        let items = wakeup
            .strip_prefix("FACTS:")
            .unwrap_or(&wakeup)
            .split('|')
            .collect::<Vec<_>>();
        assert!(
            items
                .first()
                .is_some_and(|s| s.contains("decision/d1=important")),
            "expected decision first in wakeup: {wakeup}"
        );
    }

    #[test]
    fn low_confidence_contradiction() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.4, &policy);

        let c = k.check_contradiction("arch", "db", "MySQL", &policy);
        assert!(c.is_some());
        assert_eq!(c.unwrap().severity, ContradictionSeverity::Low);
    }

    #[test]
    fn no_contradiction_for_same_value() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95, &policy);

        let c = k.check_contradiction("arch", "db", "PostgreSQL", &policy);
        assert!(c.is_none());
    }

    #[test]
    fn no_contradiction_for_similar_values() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember(
            "arch",
            "db",
            "PostgreSQL 16 production database server",
            "s1",
            0.95,
            &policy,
        );

        let c = k.check_contradiction(
            "arch",
            "db",
            "PostgreSQL 16 production database server config",
            &policy,
        );
        assert!(c.is_none());
    }

    #[test]
    fn import_skip_existing() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95, &policy);

        let incoming = vec![KnowledgeFact {
            category: "arch".into(),
            key: "db".into(),
            value: "MySQL".into(),
            source_session: "import".into(),
            confidence: 0.8,
            created_at: Utc::now(),
            last_confirmed: Utc::now(),
            retrieval_count: 0,
            last_retrieved: None,
            valid_from: Some(Utc::now()),
            valid_until: None,
            supersedes: None,
            confirmation_count: 1,
            feedback_up: 0,
            feedback_down: 0,
            last_feedback: None,
            privacy: FactPrivacy::default(),
            sensitivity: crate::core::sensitivity::SensitivityLevel::default(),
            imported_from: None,
            archetype: KnowledgeArchetype::default(),
            fidelity: None,
            revision_count: 0,
        }];

        let result = k.import_facts(incoming, ImportMerge::SkipExisting, "imp-1", &policy);
        assert_eq!(result.skipped, 1);
        assert_eq!(result.added, 0);
        assert_eq!(k.facts.iter().filter(|f| f.is_current()).count(), 1);
    }

    #[test]
    fn import_replace_existing() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95, &policy);

        let incoming = vec![KnowledgeFact {
            category: "arch".into(),
            key: "db".into(),
            value: "MySQL".into(),
            source_session: "import".into(),
            confidence: 0.8,
            created_at: Utc::now(),
            last_confirmed: Utc::now(),
            retrieval_count: 0,
            last_retrieved: None,
            valid_from: Some(Utc::now()),
            valid_until: None,
            supersedes: None,
            confirmation_count: 1,
            feedback_up: 0,
            feedback_down: 0,
            last_feedback: None,
            privacy: FactPrivacy::default(),
            sensitivity: crate::core::sensitivity::SensitivityLevel::default(),
            imported_from: None,
            archetype: KnowledgeArchetype::default(),
            fidelity: None,
            revision_count: 0,
        }];

        let result = k.import_facts(incoming, ImportMerge::Replace, "imp-1", &policy);
        assert_eq!(result.replaced, 1);
        let current: Vec<_> = k.facts.iter().filter(|f| f.is_current()).collect();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].value, "MySQL");
    }

    #[test]
    fn import_adds_new_facts() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95, &policy);

        let incoming = vec![KnowledgeFact {
            category: "security".into(),
            key: "auth".into(),
            value: "JWT".into(),
            source_session: "import".into(),
            confidence: 0.9,
            created_at: Utc::now(),
            last_confirmed: Utc::now(),
            retrieval_count: 0,
            last_retrieved: None,
            valid_from: Some(Utc::now()),
            valid_until: None,
            supersedes: None,
            confirmation_count: 1,
            feedback_up: 0,
            feedback_down: 0,
            last_feedback: None,
            privacy: FactPrivacy::default(),
            sensitivity: crate::core::sensitivity::SensitivityLevel::default(),
            imported_from: None,
            archetype: KnowledgeArchetype::default(),
            fidelity: None,
            revision_count: 0,
        }];

        let result = k.import_facts(incoming, ImportMerge::SkipExisting, "imp-1", &policy);
        assert_eq!(result.added, 1);
        assert_eq!(k.facts.iter().filter(|f| f.is_current()).count(), 2);
    }

    #[test]
    fn parse_simple_json_array() {
        let data = r#"[
            {"category": "arch", "key": "db", "value": "PostgreSQL"},
            {"category": "security", "key": "auth", "value": "JWT", "confidence": 0.9}
        ]"#;
        let facts = parse_import_data(data).unwrap();
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].category, "arch");
        assert_eq!(facts[1].confidence, 0.9);
    }

    #[test]
    fn parse_jsonl_format() {
        let data = "{\"category\":\"arch\",\"key\":\"db\",\"value\":\"PG\"}\n\
                    {\"category\":\"security\",\"key\":\"auth\",\"value\":\"JWT\"}";
        let facts = parse_import_data(data).unwrap();
        assert_eq!(facts.len(), 2);
    }

    #[test]
    fn export_simple_only_current() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95, &policy);
        k.remember("arch", "db", "MySQL", "s2", 0.9, &policy);

        let exported = k.export_simple();
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].value, "MySQL");
    }

    #[test]
    fn import_merge_parse() {
        assert_eq!(ImportMerge::parse("replace"), Some(ImportMerge::Replace));
        assert_eq!(ImportMerge::parse("append"), Some(ImportMerge::Append));
        assert_eq!(
            ImportMerge::parse("skip-existing"),
            Some(ImportMerge::SkipExisting)
        );
        assert_eq!(
            ImportMerge::parse("skip_existing"),
            Some(ImportMerge::SkipExisting)
        );
        assert_eq!(ImportMerge::parse("skip"), Some(ImportMerge::SkipExisting));
        assert!(ImportMerge::parse("invalid").is_none());
    }

    #[test]
    fn revision_count_on_new_fact() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.9, &policy);
        let cur = k.facts.iter().find(|f| f.is_current()).unwrap();
        assert_eq!(cur.revision_count, 1);
    }

    #[test]
    fn revision_count_increments_on_confirm() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.9, &policy);
        k.remember("arch", "db", "PostgreSQL", "s2", 0.9, &policy);
        k.remember("arch", "db", "PostgreSQL", "s3", 0.9, &policy);
        let cur = k.facts.iter().find(|f| f.is_current()).unwrap();
        assert_eq!(cur.revision_count, 3);
        assert_eq!(cur.confirmation_count, 3);
    }

    #[test]
    fn revision_count_carries_over_on_supersede() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95, &policy);
        k.remember("arch", "db", "PostgreSQL", "s2", 0.9, &policy);
        assert_eq!(
            k.facts
                .iter()
                .find(|f| f.is_current())
                .unwrap()
                .revision_count,
            2
        );
        k.facts[0].confirmation_count = 3;
        k.remember("arch", "db", "MySQL", "s3", 0.9, &policy);
        let cur: Vec<_> = k.facts.iter().filter(|f| f.is_current()).collect();
        assert_eq!(cur.len(), 1);
        assert_eq!(cur[0].value, "MySQL");
        assert_eq!(cur[0].revision_count, 3);
        assert!(cur[0].supersedes.is_some());
    }

    #[test]
    fn revision_count_default_zero_for_legacy() {
        let json = r#"{
            "category": "test", "key": "k", "value": "v",
            "source_session": "s", "confidence": 0.8,
            "created_at": "2024-01-01T00:00:00Z",
            "last_confirmed": "2024-01-01T00:00:00Z"
        }"#;
        let fact: KnowledgeFact = serde_json::from_str(json).unwrap();
        assert_eq!(fact.revision_count, 0);
    }

    #[test]
    fn judged_pairs_default_empty_for_legacy() {
        let json = r#"{
            "project_root": "/test", "project_hash": "abc",
            "facts": [], "patterns": [], "history": [],
            "updated_at": "2024-01-01T00:00:00Z"
        }"#;
        let pk: ProjectKnowledge = serde_json::from_str(json).unwrap();
        assert!(pk.judged_pairs.is_empty());
    }

    #[test]
    fn cross_key_similar_finds_related_facts() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember(
            "architecture",
            "auth",
            "JWT RS256 token based authentication with Redis session store",
            "s1",
            0.9,
            &policy,
        );
        k.remember(
            "decision",
            "session-model",
            "JWT token authentication stored in Redis for session management",
            "s1",
            0.85,
            &policy,
        );
        k.remember("deploy", "host", "AWS us-east-1", "s1", 0.8, &policy);

        let similar = find_cross_key_similar(
            "architecture",
            "auth",
            "JWT RS256 token based authentication with Redis session store",
            &k.facts,
            &k.judged_pairs,
            3,
        );
        assert!(!similar.is_empty(), "should find session-model as similar");
        assert_eq!(similar[0].category, "decision");
        assert_eq!(similar[0].key, "session-model");
        assert!(similar[0].similarity > 0.35);
    }

    #[test]
    fn cross_key_similar_excludes_same_key() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL 16", "s1", 0.9, &policy);

        let similar =
            find_cross_key_similar("arch", "db", "PostgreSQL 16", &k.facts, &k.judged_pairs, 3);
        assert!(similar.is_empty());
    }

    #[test]
    fn cross_key_similar_excludes_judged_pairs() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember(
            "architecture",
            "auth",
            "JWT RS256 token based authentication with Redis",
            "s1",
            0.9,
            &policy,
        );
        k.remember(
            "decision",
            "session-model",
            "JWT token authentication stored in Redis",
            "s1",
            0.85,
            &policy,
        );

        k.judged_pairs.push(JudgedPair {
            key_a: "architecture/auth".into(),
            key_b: "decision/session-model".into(),
            verdict: "compatible".into(),
            judged_at: Utc::now(),
        });

        let similar = find_cross_key_similar(
            "architecture",
            "auth",
            "JWT RS256 token based authentication with Redis",
            &k.facts,
            &k.judged_pairs,
            3,
        );
        assert!(similar.is_empty(), "judged pairs should be excluded");
    }

    #[test]
    fn cross_key_similar_ignores_unrelated_facts() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember(
            "arch",
            "db",
            "PostgreSQL 16 with pgvector",
            "s1",
            0.9,
            &policy,
        );
        k.remember("deploy", "host", "AWS us-east-1 region", "s1", 0.8, &policy);

        let similar = find_cross_key_similar(
            "arch",
            "db",
            "PostgreSQL 16 with pgvector",
            &k.facts,
            &k.judged_pairs,
            3,
        );
        assert!(similar.is_empty(), "unrelated facts should not match");
    }

    #[test]
    fn judge_supersedes_archives_target() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("architecture", "auth", "JWT RS256", "s1", 0.9, &policy);
        k.remember("decision", "session", "JWT tokens", "s1", 0.85, &policy);

        assert!(k.facts.iter().all(KnowledgeFact::is_current));

        if let Some(tf) = k
            .facts
            .iter_mut()
            .find(|f| f.category == "decision" && f.key == "session" && f.is_current())
        {
            tf.valid_until = Some(Utc::now());
        }
        k.judged_pairs.push(JudgedPair {
            key_a: "architecture/auth".into(),
            key_b: "decision/session".into(),
            verdict: "supersedes".into(),
            judged_at: Utc::now(),
        });

        let cur: Vec<_> = k.facts.iter().filter(|f| f.is_current()).collect();
        assert_eq!(cur.len(), 1);
        assert_eq!(cur[0].category, "architecture");
    }
}
