// SPDX-License-Identifier: Apache-2.0
//! Facts of the other checkouts of the same project, served by their
//! validity class (AP01 §4, D04; acceptance S05).
//!
//! Every checkout is its own partition with its own knowledge store. Only a
//! deliberate mapping (`lean-ctx project link`, D01) makes several checkouts
//! one project; a recall in one of them then also answers from the others:
//!
//! - `convention`: served regardless of branch or revision;
//! - `content`: served only when every file it is bound to is byte-identical
//!   in the reader's checkout (`bindings::holds_in_checkout`); otherwise it
//!   is withheld as `stale` and only counted;
//! - unclassified: not served outside its checkout, as before classes.

use crate::core::knowledge::{FactValidity, KnowledgeFact, ProjectKnowledge};

/// The linked-checkouts section of a recall in `reader_root`, or `None` when
/// the checkout is not linked or nothing matches.
pub(crate) fn recall_section(
    reader_root: &str,
    category: Option<&str>,
    query: Option<&str>,
    limit: usize,
) -> Option<String> {
    let (link, checkouts) = crate::core::project_link::linked_checkouts(reader_root)?;
    let terms: Vec<String> = query
        .map(str::to_lowercase)
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let matches = |fact: &KnowledgeFact| {
        category.is_none_or(|category| fact.category.eq_ignore_ascii_case(category))
            && (terms.is_empty() || {
                let text = format!("{} {} {}", fact.category, fact.key, fact.value).to_lowercase();
                terms.iter().any(|term| text.contains(term.as_str()))
            })
    };
    let mut served: Vec<(String, KnowledgeFact)> = Vec::new();
    let mut stale = 0usize;
    for checkout in checkouts {
        // Each store is read as its own checkout's (its project policy and
        // protection), never under the reader's binding; what is served
        // then passes the reader's gates with the whole reply.
        let Some(knowledge) = crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(std::path::PathBuf::from(&checkout))),
            || ProjectKnowledge::load(&checkout),
        ) else {
            continue;
        };
        for fact in knowledge
            .facts
            .into_iter()
            .filter(KnowledgeFact::is_current)
        {
            if !matches(&fact) {
                continue;
            }
            match fact.validity {
                Some(FactValidity::Convention) => {
                    // Not bound to a revision: no source to publish, so the
                    // reply's lineage is unknown, as for any unbound fact.
                    crate::core::context_admission::stored::publish_served(None);
                    served.push((knowledge.project_root.clone(), fact));
                }
                Some(FactValidity::Content) => {
                    let item = crate::core::archive::bindings::Item::KnowledgeFact {
                        project: &knowledge.project_root,
                        category: &fact.category,
                        key: &fact.key,
                    };
                    if crate::core::archive::bindings::holds_in_checkout(
                        &item,
                        &fact.value,
                        reader_root,
                    ) {
                        served.push((knowledge.project_root.clone(), fact));
                    } else {
                        stale += 1;
                    }
                }
                None => {}
            }
        }
    }
    if served.is_empty() && stale == 0 {
        return None;
    }
    served.sort_by(|a, b| {
        (a.0.as_str(), a.1.category.as_str(), a.1.key.as_str()).cmp(&(
            b.0.as_str(),
            b.1.category.as_str(),
            b.1.key.as_str(),
        ))
    });
    served.truncate(limit);
    let mut out = format!(
        "Linked checkouts of project '{}' (served by validity class):\n",
        link.team_project_id
    );
    for (checkout, fact) in &served {
        out.push_str(&format!(
            "  [{}/{}]: {} [{}] (checkout: {})\n",
            fact.category,
            fact.key,
            fact.as_delivered(fact.value.clone()),
            fact.validity.map_or("", FactValidity::as_str),
            super::short_path(checkout),
        ));
    }
    if stale > 0 {
        out.push_str(&format!(
            "  {stale} code-bound fact(s) withheld: stale — their sources differ in this checkout (next: refresh_source)\n"
        ));
    }
    Some(out)
}
