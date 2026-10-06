// SPDX-License-Identifier: Apache-2.0
//! Relevance ranking: which candidates deserve the budget first.
//!
//! Three signals, all deterministic:
//! - **lexical** — intent terms in the path (weighted) and in the content;
//! - **structural** — personalized PageRank over the import graph, seeded
//!   with the best lexical hits so neighbours of relevant files rise too;
//! - **intent** — debug/review favour changed files and tests, explore
//!   favours READMEs, docs and manifests.

use std::collections::{HashMap, HashSet};

use crate::core::ib::intent::TaskIntent;

use super::collect::Candidate;

const LEXICAL_WEIGHT: f64 = 0.55;
const STRUCTURAL_WEIGHT: f64 = 0.35;
const PATH_HIT_WEIGHT: f64 = 3.0;
/// Lexical seeds for personalized PageRank.
const MAX_SEEDS: usize = 5;
/// Occurrence counting stops here; a term repeated 500 times is not 5× as relevant.
const MAX_OCCURRENCES: usize = 100;

const STOPWORDS: &[&str] = &[
    "the",
    "and",
    "for",
    "with",
    "from",
    "into",
    "that",
    "this",
    "these",
    "those",
    "what",
    "when",
    "where",
    "which",
    "why",
    "how",
    "are",
    "was",
    "were",
    "has",
    "have",
    "had",
    "not",
    "but",
    "all",
    "any",
    "can",
    "does",
    "should",
    "would",
    "could",
    "make",
    "use",
    "using",
    "code",
    "file",
    "files",
    "about",
    "there",
    "their",
    "then",
    "than",
    "its",
    "our",
    "your",
    // Task verbs say what to do, not where: they classify intent instead.
    "fix",
    "debug",
    "implement",
    "add",
    "create",
    "build",
    "feature",
    "new",
    "review",
    "check",
    "audit",
    "verify",
    "inspect",
    "understand",
    "explore",
    "analyze",
    "investigate",
    "find",
    "refactor",
    "rename",
    "move",
    "extract",
    "restructure",
    "clean",
    "auto",
];

const MANIFESTS: &[&str] = &[
    "cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "gemfile",
    "composer.json",
    "makefile",
];

/// Everything ranking needs, gathered by the caller.
pub(crate) struct RankInput<'a> {
    pub candidates: &'a [Candidate],
    /// Free-text task description (may be empty).
    pub intent_text: &'a str,
    pub intent: TaskIntent,
    /// Import edges `(from, to)` between root-relative paths.
    pub edges: &'a [(String, String)],
    /// Root-relative paths changed in the working tree.
    pub changed: &'a HashSet<String>,
}

/// A readable candidate and its score in `[0, 1]`-ish space.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Ranked {
    /// Index into [`RankInput::candidates`].
    pub index: usize,
    pub score: f64,
}

/// Intent terms: lowercase words of three or more characters, minus stopwords.
pub(crate) fn terms(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() >= 3 && !STOPWORDS.contains(&w.as_str()))
        .filter(|w| seen.insert(w.clone()))
        .collect()
}

/// Rank readable candidates, best first. Skipped candidates are not ranked.
pub(crate) fn rank(input: &RankInput<'_>) -> Vec<Ranked> {
    let terms = terms(input.intent_text);
    let readable: Vec<usize> = input
        .candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.content.is_some())
        .map(|(i, _)| i)
        .collect();

    let lexical: HashMap<usize, f64> = readable
        .iter()
        .map(|&i| (i, lexical_score(&input.candidates[i], &terms)))
        .collect();
    let structural = structural_scores(input, &readable, &lexical);

    let max_lex = lexical.values().copied().fold(0.0, f64::max);
    let max_pr = structural.values().copied().fold(0.0, f64::max);

    let mut ranked: Vec<Ranked> = readable
        .iter()
        .map(|&i| {
            let lex = normalize(lexical[&i], max_lex);
            let pr = normalize(structural.get(&i).copied().unwrap_or(0.0), max_pr);
            let bonus = intent_bonus(
                input,
                &input.candidates[i].path,
                lex > 0.0,
                terms.is_empty(),
            );
            Ranked {
                index: i,
                score: LEXICAL_WEIGHT * lex + STRUCTURAL_WEIGHT * pr + bonus,
            }
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score.total_cmp(&a.score).then_with(|| {
            input.candidates[a.index]
                .path
                .cmp(&input.candidates[b.index].path)
        })
    });
    ranked
}

fn normalize(value: f64, max: f64) -> f64 {
    if max > 0.0 { value / max } else { 0.0 }
}

fn lexical_score(candidate: &Candidate, terms: &[String]) -> f64 {
    if terms.is_empty() {
        return 0.0;
    }
    let path = candidate.path.to_lowercase();
    let content = candidate
        .content
        .as_deref()
        .map(str::to_lowercase)
        .unwrap_or_default();
    terms
        .iter()
        .map(|term| {
            let path_hit = if path.contains(term.as_str()) {
                PATH_HIT_WEIGHT
            } else {
                0.0
            };
            let occurrences = content.matches(term.as_str()).take(MAX_OCCURRENCES).count();
            #[allow(clippy::cast_precision_loss)]
            let content_hit = (occurrences as f64).ln_1p();
            path_hit + content_hit
        })
        .sum()
}

/// Personalized PageRank restricted to readable candidates, keyed by index.
fn structural_scores(
    input: &RankInput<'_>,
    readable: &[usize],
    lexical: &HashMap<usize, f64>,
) -> HashMap<usize, f64> {
    let by_path: HashMap<&str, usize> = readable
        .iter()
        .map(|&i| (input.candidates[i].path.as_str(), i))
        .collect();
    let mut forward: HashMap<String, Vec<String>> = HashMap::new();
    for (from, to) in input.edges {
        if from != to && by_path.contains_key(from.as_str()) && by_path.contains_key(to.as_str()) {
            forward.entry(from.clone()).or_default().push(to.clone());
        }
    }
    if forward.is_empty() {
        return HashMap::new();
    }
    for targets in forward.values_mut() {
        targets.sort();
        targets.dedup();
    }

    let mut seeds: Vec<(usize, f64)> = lexical
        .iter()
        .filter(|(_, score)| **score > 0.0)
        .map(|(&i, &score)| (i, score))
        .collect();
    seeds.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| input.candidates[a.0].path.cmp(&input.candidates[b.0].path))
    });
    let mut seed_paths: Vec<String> = seeds
        .iter()
        .take(MAX_SEEDS)
        .map(|(i, _)| input.candidates[*i].path.clone())
        .collect();
    if seed_paths.is_empty() {
        let mut changed: Vec<String> = input
            .changed
            .iter()
            .filter(|p| by_path.contains_key(p.as_str()))
            .cloned()
            .collect();
        changed.sort();
        seed_paths = changed;
    }

    let graph = crate::core::pagerank::PageRankInput {
        files: by_path.keys().map(|p| (*p).to_string()).collect(),
        forward,
    };
    crate::core::pagerank::compute_personalized(&graph, 0.85, 30, &seed_paths)
        .into_iter()
        .filter_map(|(path, score)| by_path.get(path.as_str()).map(|&i| (i, score)))
        .collect()
}

fn intent_bonus(input: &RankInput<'_>, path: &str, lexical_hit: bool, no_terms: bool) -> f64 {
    let changed = input.changed.contains(path);
    match input.intent {
        TaskIntent::Review => {
            if changed {
                0.30
            } else {
                0.0
            }
        }
        TaskIntent::Debug => {
            let mut bonus = if changed { 0.15 } else { 0.0 };
            if lexical_hit && is_test(path) {
                bonus += 0.10;
            }
            bonus
        }
        TaskIntent::Explore => {
            if is_orientation(path) {
                0.20
            } else {
                0.0
            }
        }
        TaskIntent::Refactor | TaskIntent::Implement => {
            if changed {
                0.05
            } else {
                0.0
            }
        }
        TaskIntent::Unknown => {
            if no_terms && is_orientation(path) {
                0.10
            } else {
                0.0
            }
        }
    }
}

fn is_test(path: &str) -> bool {
    let lower = path.to_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    lower
        .split('/')
        .any(|seg| matches!(seg, "test" | "tests" | "spec" | "__tests__"))
        || name.starts_with("test_")
        || name.contains("_test.")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.contains("_spec.")
        || name.ends_with("_tests.rs")
}

/// READMEs, top-level docs and build manifests: where a newcomer starts.
fn is_orientation(path: &str) -> bool {
    let lower = path.to_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let depth = lower.matches('/').count();
    let markdown = std::path::Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
    name.starts_with("readme")
        || (depth == 0 && (MANIFESTS.contains(&name) || markdown))
        || (lower.starts_with("docs/") && markdown && depth <= 1)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{RankInput, rank, terms};
    use crate::core::context_bundle::collect::Candidate;
    use crate::core::ib::intent::TaskIntent;

    fn file(path: &str, body: &str) -> Candidate {
        Candidate {
            path: path.to_string(),
            content: Some(body.to_string()),
            skipped: None,
        }
    }

    fn order(input: &RankInput<'_>) -> Vec<String> {
        rank(input)
            .into_iter()
            .map(|r| input.candidates[r.index].path.clone())
            .collect()
    }

    #[test]
    fn terms_drop_stopwords_short_words_and_duplicates() {
        assert_eq!(
            terms("Fix the auth token refresh in the Auth module, id=7"),
            ["auth", "token", "refresh", "module"]
        );
    }

    #[test]
    fn path_and_content_hits_outrank_unrelated_files() {
        let candidates = [
            file("src/db.rs", "fn connect() {}"),
            file("src/auth/login.rs", "fn login() { refresh_token() }"),
            file("src/util.rs", "// token helpers\nfn token() {}"),
        ];
        let changed = HashSet::new();
        let input = RankInput {
            candidates: &candidates,
            intent_text: "auth token refresh",
            intent: TaskIntent::Unknown,
            edges: &[],
            changed: &changed,
        };
        assert_eq!(
            order(&input),
            ["src/auth/login.rs", "src/util.rs", "src/db.rs"]
        );
    }

    #[test]
    fn graph_neighbours_of_relevant_files_rise() {
        let candidates = [
            file("src/a.rs", "fn a() {}"),
            file("src/b.rs", "fn b() {}"),
            file("src/billing.rs", "fn billing() {}"),
        ];
        let edges = [("src/billing.rs".to_string(), "src/b.rs".to_string())];
        let changed = HashSet::new();
        let input = RankInput {
            candidates: &candidates,
            intent_text: "billing",
            intent: TaskIntent::Unknown,
            edges: &edges,
            changed: &changed,
        };
        assert_eq!(order(&input), ["src/billing.rs", "src/b.rs", "src/a.rs"]);
    }

    #[test]
    fn review_intent_puts_changed_files_first() {
        let candidates = [file("src/a.rs", "fn a() {}"), file("src/z.rs", "fn z() {}")];
        let changed: HashSet<String> = ["src/z.rs".to_string()].into();
        let input = RankInput {
            candidates: &candidates,
            intent_text: "",
            intent: TaskIntent::Review,
            edges: &[],
            changed: &changed,
        };
        assert_eq!(order(&input), ["src/z.rs", "src/a.rs"]);
    }

    #[test]
    fn explore_intent_favours_readme_and_manifests() {
        let candidates = [
            file("src/deep/x.rs", "fn x() {}"),
            file("README.md", "# Project"),
            file("Cargo.toml", "[package]"),
        ];
        let changed = HashSet::new();
        let input = RankInput {
            candidates: &candidates,
            intent_text: "",
            intent: TaskIntent::Explore,
            edges: &[],
            changed: &changed,
        };
        assert_eq!(order(&input), ["Cargo.toml", "README.md", "src/deep/x.rs"]);
    }

    #[test]
    fn skipped_candidates_are_not_ranked_and_order_is_deterministic() {
        let candidates = [
            file("b.rs", ""),
            Candidate {
                path: "logo.png".to_string(),
                content: None,
                skipped: Some("binary"),
            },
            file("a.rs", ""),
        ];
        let changed = HashSet::new();
        let input = RankInput {
            candidates: &candidates,
            intent_text: "",
            intent: TaskIntent::Unknown,
            edges: &[],
            changed: &changed,
        };
        assert_eq!(order(&input), ["a.rs", "b.rs"]);
    }
}
