// SPDX-License-Identifier: Apache-2.0
//! Relevance floor for explicit-source plans (gateway acceptance scenario 24).
//!
//! The field ranks candidates and the compiler fills the budget, so a source
//! that has nothing to do with the task still gets selected when budget is
//! left. Within a plan of caller-supplied sources, a candidate is kept when
//! it is relevant to the task directly (it shares a term with the query) or
//! one hop away (it shares a term with a directly relevant candidate, e.g. the
//! module a relevant file calls into, or the log that names its setting).
//! Everything else is left out. Terms ignore stop words and split
//! identifiers (`SESSION_TTL`, `requireLogin` → `session`, `ttl`, `require`,
//! `login`). When no candidate is directly relevant the floor does nothing:
//! it never empties a plan the query cannot speak to.

use std::collections::HashSet;

/// Words that carry no topic: English function words and ubiquitous code
/// keywords.
const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "from", "this", "that", "are", "was", "were", "not", "but", "you",
    "your", "all", "any", "can", "has", "have", "had", "use", "using", "into", "onto", "via",
    "per", "its", "our", "out", "off", "too", "how", "why", "what", "when", "who", "fix", "def",
    "return", "self", "none", "true", "false", "int", "str", "let", "var", "const", "pub", "new",
    "get", "set", "import", "export", "class", "type", "else", "then", "raise", "throw", "async",
    "await", "null", "void",
];

/// Topic terms of a text: identifiers split on separators and case changes,
/// lowercased, without stop words and without terms shorter than three.
pub(crate) fn terms(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for word in text.split(|character: char| !character.is_alphanumeric()) {
        let mut current = String::new();
        let mut previous_lower = false;
        for character in word.chars() {
            if character.is_uppercase() && previous_lower && !current.is_empty() {
                push_term(&mut out, &current);
                current.clear();
            }
            previous_lower = character.is_lowercase() || character.is_ascii_digit();
            current.extend(character.to_lowercase());
        }
        push_term(&mut out, &current);
    }
    out
}

fn push_term(out: &mut HashSet<String>, term: &str) {
    if term.chars().count() >= 3
        && !term.chars().all(|character| character.is_ascii_digit())
        && !STOP_WORDS.contains(&term)
    {
        out.insert(term.to_owned());
    }
}

/// Which candidates (`title`, `content`) clear the floor for `query`.
pub(crate) fn kept(candidates: &[(&str, Option<&str>)], query: &str) -> Vec<bool> {
    let query_terms = terms(query);
    let candidate_terms: Vec<HashSet<String>> = candidates
        .iter()
        .map(|(title, content)| {
            let mut all = terms(title);
            if let Some(content) = content {
                all.extend(terms(content));
            }
            all
        })
        .collect();
    let direct: Vec<bool> = candidate_terms
        .iter()
        .map(|terms| !terms.is_disjoint(&query_terms))
        .collect();
    if !direct.iter().any(|relevant| *relevant) {
        return vec![true; candidates.len()];
    }
    let mut neighbourhood: HashSet<&String> = HashSet::new();
    for (terms, relevant) in candidate_terms.iter().zip(&direct) {
        if *relevant {
            neighbourhood.extend(terms.iter());
        }
    }
    candidate_terms
        .iter()
        .zip(&direct)
        .map(|(terms, relevant)| *relevant || terms.iter().any(|term| neighbourhood.contains(term)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_split_and_stop_words_drop() {
        let found = terms("SESSION_TTL = requireLogin(the user) + 14:05");
        for term in ["session", "ttl", "require", "login", "user"] {
            assert!(found.contains(term), "{term} missing from {found:?}");
        }
        assert!(!found.contains("the") && !found.contains("14"));
    }

    #[test]
    fn related_sources_stay_and_unrelated_ones_are_left_out() {
        let candidates = [
            (
                "src/auth/login.py",
                Some("def login(request):\n    sessions.create(user, ttl=SESSION_TTL)"),
            ),
            (
                "src/auth/session.py",
                Some("SESSION_TTL = int(os.environ['SESSION_TTL'])"),
            ),
            (
                "logs/deploy.log",
                Some("deploy: SESSION_TTL unset, session expired (ttl=0)"),
            ),
            (
                "docs/holiday-calendar.md",
                Some("holiday calendar: office closures for the winter break"),
            ),
            (
                "docs/press-kit.md",
                Some("press kit: founder bios and product screenshots"),
            ),
        ];
        assert_eq!(
            kept(&candidates, "Fix the production login issue"),
            vec![true, true, true, false, false]
        );
    }

    #[test]
    fn a_query_that_matches_nothing_keeps_everything() {
        let candidates = [("a.md", Some("alpha beta")), ("b.md", Some("gamma delta"))];
        assert_eq!(kept(&candidates, "unrelated question"), vec![true, true]);
    }
}
