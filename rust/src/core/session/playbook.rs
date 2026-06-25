//! Incremental delta playbook for checkpoints (#541, EFF-4 — ACE principle).
//!
//! ACE (Agentic Context Engineering, 2510.04618) showed that monolithic
//! checkpoint rewrites cause *brevity bias* (repeated summarization loses
//! detail) and *context collapse* (an observed 18k -> 122 token implosion,
//! −29% accuracy). The cure: contexts grow as structured, itemized delta
//! entries with stable IDs that are never rewritten — only appended, bumped
//! (dedup-confirm), voted on, and locally evicted.
//!
//! lean-ctx wires this into `ctx_compress`: every checkpoint distills the
//! session into playbook deltas instead of re-summarizing prior summaries.
//! Renders are ordered by stable ID, so unchanged prefixes stay prefix-cache
//! friendly across checkpoints.

use serde::{Deserialize, Serialize};

use crate::core::memory_consolidation::token_jaccard;

/// Entries with this token-Jaccard similarity to an existing entry are
/// duplicates: the existing entry gets confirmed instead of inserting.
const DEDUP_JACCARD: f64 = 0.7;
/// Entries unconfirmed for this many turns are evicted (locally).
const STALE_TURNS: u32 = 50;
/// Hard cap on entry content length (chars).
const MAX_CONTENT_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// An approach that worked (decisions, successful tactics).
    Strategy,
    /// Something that bit us (gotchas, bounces, failed edits).
    Pitfall,
    /// A stable observation about the codebase or domain.
    Fact,
    /// A file worth remembering, with why.
    FileRef,
}

impl EntryKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            EntryKind::Strategy => "Strategy",
            EntryKind::Pitfall => "Pitfall",
            EntryKind::Fact => "Fact",
            EntryKind::FileRef => "FileRef",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybookEntry {
    /// Stable, monotonically assigned — never reused, never rewritten.
    pub id: u32,
    pub kind: EntryKind,
    pub content: String,
    pub created_turn: u32,
    pub last_confirmed_turn: u32,
    pub helpful_votes: u32,
    pub harmful_votes: u32,
}

impl PlaybookEntry {
    fn salience(&self) -> i64 {
        i64::from(self.helpful_votes) - i64::from(self.harmful_votes)
    }
}

/// Outcome of a delta insert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaOutcome {
    Added(u32),
    Confirmed(u32),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Playbook {
    pub entries: Vec<PlaybookEntry>,
    pub next_id: u32,
}

impl Playbook {
    /// Grow-and-refine insert: near-duplicates confirm the existing entry
    /// (bump + helpful vote) instead of creating drift. Existing entries are
    /// NEVER rewritten — that is the ACE anti-collapse invariant.
    pub fn add_delta(&mut self, kind: EntryKind, content: &str, turn: u32) -> DeltaOutcome {
        let content: String = content.trim().chars().take(MAX_CONTENT_CHARS).collect();
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|e| e.kind == kind && token_jaccard(&e.content, &content) >= DEDUP_JACCARD)
        {
            existing.last_confirmed_turn = turn;
            existing.helpful_votes += 1;
            return DeltaOutcome::Confirmed(existing.id);
        }
        self.next_id += 1;
        let id = self.next_id;
        self.entries.push(PlaybookEntry {
            id,
            kind,
            content,
            created_turn: turn,
            last_confirmed_turn: turn,
            helpful_votes: 0,
            harmful_votes: 0,
        });
        DeltaOutcome::Added(id)
    }

    /// Vote on an entry by stable ID (agent feedback via `ctx_session`).
    pub fn vote(&mut self, id: u32, helpful: bool) -> bool {
        match self.entries.iter_mut().find(|e| e.id == id) {
            Some(e) => {
                if helpful {
                    e.helpful_votes += 1;
                } else {
                    e.harmful_votes += 1;
                }
                true
            }
            None => false,
        }
    }

    /// Local eviction only: net-harmful entries and entries unconfirmed for
    /// `STALE_TURNS` die. No global re-summarization ever happens.
    pub fn evict(&mut self, current_turn: u32) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| {
            let net_harmful = e.harmful_votes > e.helpful_votes;
            let stale = current_turn.saturating_sub(e.last_confirmed_turn) > STALE_TURNS;
            !(net_harmful || stale)
        });
        before - self.entries.len()
    }

    /// Render ordered by stable ID (prefix-cache friendly: old entries keep
    /// their byte positions). When `top_k` is exceeded, the lowest-salience
    /// entries are elided — never rewritten.
    #[must_use]
    pub fn render(&self, top_k: usize) -> String {
        if self.entries.is_empty() {
            return String::new();
        }
        let mut selected: Vec<&PlaybookEntry> = self.entries.iter().collect();
        let elided = if selected.len() > top_k {
            selected.sort_by_key(|e| std::cmp::Reverse((e.salience(), e.last_confirmed_turn)));
            let n = selected.len() - top_k;
            selected.truncate(top_k);
            n
        } else {
            0
        };
        selected.sort_by_key(|e| e.id);

        let mut out = String::from("PLAYBOOK (delta log, stable IDs):\n");
        for e in selected {
            let votes = if e.helpful_votes > 0 || e.harmful_votes > 0 {
                format!(" (+{}/-{})", e.helpful_votes, e.harmful_votes)
            } else {
                String::new()
            };
            out.push_str(&format!(
                "[P{}] {}: {}{votes}\n",
                e.id,
                e.kind.as_str(),
                e.content
            ));
        }
        if elided > 0 {
            out.push_str(&format!(
                "… {elided} low-salience entries elided (recall via ctx_session)\n"
            ));
        }
        out
    }

    /// Total content volume (entries × chars) — used by the brevity-bias
    /// regression test: repeated checkpoints must never shrink this.
    #[must_use]
    pub fn information_volume(&self) -> usize {
        self.entries.iter().map(|e| e.content.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_assigns_stable_monotonic_ids() {
        let mut p = Playbook::default();
        let a = p.add_delta(EntryKind::Fact, "billing service uses separate database", 1);
        let b = p.add_delta(EntryKind::Strategy, "deploy via rsync then script", 1);
        assert_eq!(a, DeltaOutcome::Added(1));
        assert_eq!(b, DeltaOutcome::Added(2));
    }

    #[test]
    fn near_duplicate_confirms_instead_of_inserting() {
        let mut p = Playbook::default();
        p.add_delta(
            EntryKind::Fact,
            "the webhook parses cancel_at from stripe",
            1,
        );
        let out = p.add_delta(EntryKind::Fact, "webhook parses cancel_at from stripe", 5);
        assert_eq!(out, DeltaOutcome::Confirmed(1));
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].last_confirmed_turn, 5);
        assert_eq!(p.entries[0].helpful_votes, 1);
    }

    #[test]
    fn eviction_is_local_only() {
        let mut p = Playbook::default();
        p.add_delta(EntryKind::Strategy, "good strategy that keeps working", 1);
        p.add_delta(EntryKind::Pitfall, "bad advice that hurt us twice", 1);
        p.vote(2, false);
        let evicted = p.evict(2);
        assert_eq!(evicted, 1);
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].id, 1, "untouched entry survives verbatim");
    }

    #[test]
    fn stale_entries_evicted_after_50_turns() {
        let mut p = Playbook::default();
        p.add_delta(EntryKind::Fact, "old fact from early in the session", 1);
        p.add_delta(EntryKind::Fact, "recent fact still being confirmed", 60);
        let evicted = p.evict(60);
        assert_eq!(evicted, 1);
        assert_eq!(p.entries[0].created_turn, 60);
    }

    #[test]
    fn render_is_stable_across_checkpoints() {
        let mut p = Playbook::default();
        p.add_delta(EntryKind::Fact, "fact one about the billing database", 1);
        p.add_delta(EntryKind::Strategy, "strategy two for safe deploys", 1);
        let r1 = p.render(10);
        let r2 = p.render(10);
        assert_eq!(r1, r2, "no drift without new deltas");
        // A new delta only appends — existing lines keep their bytes.
        p.add_delta(EntryKind::Pitfall, "pitfall three with the file lock", 2);
        let r3 = p.render(10);
        assert!(r3.contains("[P1]") && r3.contains("[P2]") && r3.contains("[P3]"));
        for line in r1.lines().filter(|l| l.starts_with("[P")) {
            assert!(r3.contains(line), "old line rewritten: {line}");
        }
    }

    #[test]
    fn brevity_bias_regression_volume_never_shrinks() {
        let mut p = Playbook::default();
        let mut last_volume = 0;
        for turn in 1..=10 {
            p.add_delta(
                EntryKind::Fact,
                &format!("distinct finding number {turn} about module {turn}"),
                turn,
            );
            p.evict(turn);
            let vol = p.information_volume();
            assert!(
                vol >= last_volume,
                "checkpoint {turn} shrank information volume: {last_volume} -> {vol}"
            );
            last_volume = vol;
        }
    }

    #[test]
    fn render_caps_at_top_k_by_salience() {
        let mut p = Playbook::default();
        let topics = [
            "webhook parses stripe cancellation timestamps",
            "dashboard heatmap aggregates bounce counters",
            "scent field decays claims exponentially",
            "playbook entries keep stable identifiers",
            "thresholds learn from edit failures",
            "litm calibration shifts begin share",
            "billing purge runs inside one transaction",
            "goodbye email sends after account deletion",
            "entropy mode rescues task keywords",
            "bm25 index rebuilds on provider sync",
        ];
        for (i, t) in topics.iter().enumerate() {
            p.add_delta(EntryKind::Fact, t, i as u32 + 1);
        }
        assert_eq!(p.entries.len(), topics.len(), "fixtures must not dedup");
        p.vote(7, true);
        p.vote(7, true);
        let r = p.render(5);
        let entry_lines = r.lines().filter(|l| l.starts_with("[P")).count();
        assert_eq!(entry_lines, 5);
        assert!(r.contains("[P7]"), "high-salience entry survives the cut");
        assert!(r.contains("elided"));
    }

    #[test]
    fn content_capped_at_200_chars() {
        let mut p = Playbook::default();
        let long = "x".repeat(500);
        p.add_delta(EntryKind::Fact, &long, 1);
        assert_eq!(p.entries[0].content.len(), MAX_CONTENT_CHARS);
    }
}
