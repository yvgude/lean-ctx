use super::ranking::{confidence_stars, sort_fact_for_output};
use super::types::{ConsolidatedInsight, KnowledgeFact, ProjectKnowledge};
use crate::core::memory_policy::MemoryPolicy;

impl ProjectKnowledge {
    pub fn consolidate(&mut self, summary: &str, session_ids: Vec<String>, policy: &MemoryPolicy) {
        self.history.push(ConsolidatedInsight {
            summary: summary.to_string(),
            from_sessions: session_ids,
            timestamp: chrono::Utc::now(),
        });

        if self.history.len() > policy.knowledge.max_history {
            self.history
                .drain(0..self.history.len() - policy.knowledge.max_history);
        }
        self.updated_at = chrono::Utc::now();
    }

    #[must_use]
    pub fn format_summary(&self) -> String {
        let mut out = String::new();
        let current_facts: Vec<&KnowledgeFact> =
            self.facts.iter().filter(|f| f.is_current()).collect();

        if !current_facts.is_empty() {
            out.push_str("PROJECT KNOWLEDGE:\n");
            let mut rooms: Vec<(String, usize)> = self.list_rooms();
            rooms.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

            let total_rooms = rooms.len();
            rooms.truncate(crate::core::budgets::KNOWLEDGE_SUMMARY_ROOMS_LIMIT);

            for (cat, _count) in rooms {
                out.push_str(&format!("  [{cat}]\n"));

                let mut facts_in_cat: Vec<&KnowledgeFact> = current_facts
                    .iter()
                    .copied()
                    .filter(|f| f.category == cat)
                    .collect();
                facts_in_cat.sort_by(|a, b| sort_fact_for_output(a, b));

                let total_in_cat = facts_in_cat.len();
                facts_in_cat.truncate(crate::core::budgets::KNOWLEDGE_SUMMARY_FACTS_PER_ROOM_LIMIT);

                for f in facts_in_cat {
                    let key = crate::core::sanitize::neutralize_metadata(&f.key);
                    let val = crate::core::sanitize::neutralize_metadata(&f.value);
                    out.push_str(&format!(
                        "    {}: {} (confidence: {:.0}%)\n",
                        key,
                        val,
                        f.confidence * 100.0
                    ));
                }
                if total_in_cat > crate::core::budgets::KNOWLEDGE_SUMMARY_FACTS_PER_ROOM_LIMIT {
                    out.push_str(&format!(
                        "    … +{} more\n",
                        total_in_cat - crate::core::budgets::KNOWLEDGE_SUMMARY_FACTS_PER_ROOM_LIMIT
                    ));
                }
            }

            if total_rooms > crate::core::budgets::KNOWLEDGE_SUMMARY_ROOMS_LIMIT {
                out.push_str(&format!(
                    "  … +{} more rooms\n",
                    total_rooms - crate::core::budgets::KNOWLEDGE_SUMMARY_ROOMS_LIMIT
                ));
            }
        }

        if !self.patterns.is_empty() {
            out.push_str("PROJECT PATTERNS:\n");
            let mut patterns = self.patterns.clone();
            patterns.sort_by(|a, b| {
                b.created_at
                    .cmp(&a.created_at)
                    .then_with(|| a.pattern_type.cmp(&b.pattern_type))
                    .then_with(|| a.description.cmp(&b.description))
            });
            let total = patterns.len();
            patterns.truncate(crate::core::budgets::KNOWLEDGE_PATTERNS_LIMIT);
            for p in &patterns {
                let ty = crate::core::sanitize::neutralize_metadata(&p.pattern_type);
                let desc = crate::core::sanitize::neutralize_metadata(&p.description);
                out.push_str(&format!("  [{ty}] {desc}\n"));
            }
            if total > crate::core::budgets::KNOWLEDGE_PATTERNS_LIMIT {
                out.push_str(&format!(
                    "  … +{} more\n",
                    total - crate::core::budgets::KNOWLEDGE_PATTERNS_LIMIT
                ));
            }
        }

        if out.is_empty() {
            out
        } else {
            crate::core::sanitize::fence_content("project_knowledge", out.trim_end())
        }
    }

    #[must_use]
    pub fn format_aaak(&self) -> String {
        // #212 — pre-prompt sensitivity floor: never inject facts whose stored or
        // freshly-classified sensitivity meets/exceeds the configured floor. The
        // short-circuit means zero classification cost when disabled (default).
        let sens = crate::core::config::Config::load().sensitivity;
        let current_facts: Vec<&KnowledgeFact> = self
            .facts
            .iter()
            .filter(|f| f.is_current())
            .filter(|f| {
                !sens.enabled_effective()
                    || !crate::core::sensitivity::floor_blocks(
                        f.sensitivity
                            .max(crate::core::sensitivity::classify_content(&f.value)),
                        &sens,
                    )
            })
            .collect();

        if current_facts.is_empty() && self.patterns.is_empty() {
            return String::new();
        }

        let mut out = String::new();

        let mut rooms: Vec<(String, usize)> = self.list_rooms();
        rooms.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        rooms.truncate(crate::core::budgets::KNOWLEDGE_AAAK_ROOMS_LIMIT);

        for (cat, _count) in rooms {
            let mut facts_in_cat: Vec<&KnowledgeFact> = current_facts
                .iter()
                .copied()
                .filter(|f| f.category == cat)
                .collect();
            facts_in_cat.sort_by(|a, b| sort_fact_for_output(a, b));
            facts_in_cat.truncate(crate::core::budgets::KNOWLEDGE_AAAK_FACTS_PER_ROOM_LIMIT);

            let items: Vec<String> = facts_in_cat
                .iter()
                .map(|f| {
                    let stars = confidence_stars(f.confidence);
                    let key = crate::core::sanitize::neutralize_metadata(&f.key);
                    let val = crate::core::sanitize::neutralize_metadata(&f.value);
                    format!("{key}={val}{stars}")
                })
                .collect();
            out.push_str(&format!(
                "{}:{}\n",
                crate::core::sanitize::neutralize_metadata(&cat.to_uppercase()),
                items.join("|")
            ));
        }

        if !self.patterns.is_empty() {
            let mut patterns = self.patterns.clone();
            patterns.sort_by(|a, b| {
                b.created_at
                    .cmp(&a.created_at)
                    .then_with(|| a.pattern_type.cmp(&b.pattern_type))
                    .then_with(|| a.description.cmp(&b.description))
            });
            patterns.truncate(crate::core::budgets::KNOWLEDGE_PATTERNS_LIMIT);
            let pat_items: Vec<String> = patterns
                .iter()
                .map(|p| {
                    let ty = crate::core::sanitize::neutralize_metadata(&p.pattern_type);
                    let desc = crate::core::sanitize::neutralize_metadata(&p.description);
                    format!("{ty}.{desc}")
                })
                .collect();
            out.push_str(&format!("PAT:{}\n", pat_items.join("|")));
        }

        if out.is_empty() {
            out
        } else {
            crate::core::sanitize::fence_content("project_memory_aaak", out.trim_end())
        }
    }

    #[must_use]
    pub fn format_wakeup(&self) -> String {
        let current_facts: Vec<&KnowledgeFact> = self
            .facts
            .iter()
            .filter(|f| f.is_current() && f.confidence >= 0.7)
            .collect();

        if current_facts.is_empty() {
            return String::new();
        }

        // Theta-gamma chunking (#543): salience-ordered top-K facts grouped
        // into 4±1-sized thematic chunks; shared headers amortize category
        // prefixes (token savings) and prime related facts (recall).
        let mut top_facts: Vec<&KnowledgeFact> = current_facts;
        top_facts.sort_by(|a, b| sort_fact_for_output(a, b));
        top_facts.truncate(20);

        let clusters = super::chunking::cluster_facts(&top_facts);
        crate::core::sanitize::fence_content(
            "project_facts_wakeup",
            &super::chunking::render_chunked(&clusters),
        )
    }
}
