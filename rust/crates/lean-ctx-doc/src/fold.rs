// SPDX-License-Identifier: Apache-2.0

//! Lowest-seq membership of doc nodes (C0 2.2, shared with Hive's
//! `hive-docs`): a section's page is the `to` of its live `part_of` edge with
//! the lowest seq; a note's subject the `to` of its live `about` edge with the
//! lowest seq; a slug belongs to the live page with the lowest `node.add` seq
//! that holds it now, and every other live page holding it is in conflict.

use std::collections::{BTreeMap, BTreeSet};

/// The doc-relevant part of one place event, in log order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocEvent {
    /// `node.add` of a `page` (with its slug), `section` or `note`.
    NodeAdd {
        seq: u64,
        node: String,
        node_type: String,
        slug: Option<String>,
    },
    /// `field.set slug` of a page (other fields do not change membership).
    SlugSet {
        seq: u64,
        node: String,
        slug: String,
    },
    /// `edge.add` of kind `part_of` or `about`.
    EdgeAdd {
        seq: u64,
        edge: String,
        kind: String,
        from: String,
        to: String,
    },
    /// `retract` of a node or an edge.
    Retract { seq: u64, target: String },
    /// `restore` of a node or an edge.
    Restore { seq: u64, target: String },
}

/// Who belongs where after a fold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Membership {
    /// Section → its page; `None` while unattached.
    pub section_page: BTreeMap<String, Option<String>>,
    /// Note → the page or section it is about; `None` while it has none.
    pub note_about: BTreeMap<String, Option<String>>,
    /// Slug → the page that holds it.
    pub slug_owner: BTreeMap<String, String>,
    /// Live pages that hold a slug another page owns, in id order.
    pub slug_conflict: Vec<String>,
}

struct Edge<'a> {
    seq: u64,
    kind: &'a str,
    from: &'a str,
    to: &'a str,
}

/// Folds `events` (any order; seq decides) into the doc membership.
pub fn fold(events: &[DocEvent]) -> Membership {
    let mut ordered: Vec<&DocEvent> = events.iter().collect();
    ordered.sort_by_key(|e| seq(e));

    let mut types: BTreeMap<&str, (&str, u64)> = BTreeMap::new();
    let mut slugs: BTreeMap<&str, &str> = BTreeMap::new();
    let mut edges: BTreeMap<&str, Edge<'_>> = BTreeMap::new();
    let mut retracted: BTreeSet<&str> = BTreeSet::new();
    for e in ordered {
        match e {
            DocEvent::NodeAdd {
                seq,
                node,
                node_type,
                slug,
            } => {
                types.entry(node).or_insert((node_type, *seq));
                if let Some(s) = slug {
                    slugs.insert(node, s);
                }
            }
            DocEvent::SlugSet { node, slug, .. } => {
                slugs.insert(node, slug);
            }
            DocEvent::EdgeAdd {
                seq,
                edge,
                kind,
                from,
                to,
            } => {
                edges.entry(edge).or_insert(Edge {
                    seq: *seq,
                    kind,
                    from,
                    to,
                });
            }
            DocEvent::Retract { target, .. } => {
                retracted.insert(target);
            }
            DocEvent::Restore { target, .. } => {
                retracted.remove(target.as_str());
            }
        }
    }

    let mut m = Membership::default();
    let live = |id: &str| !retracted.contains(id);
    // Edges name their ends even when the nodes' adds are elsewhere in the log.
    for e in edges.values() {
        let map = match e.kind {
            "part_of" => &mut m.section_page,
            "about" => &mut m.note_about,
            _ => continue,
        };
        map.entry(e.from.to_owned()).or_insert(None);
    }
    let mut best: BTreeMap<(&str, &str), (u64, &str)> = BTreeMap::new();
    for (id, e) in &edges {
        if !live(id) {
            continue;
        }
        let slot = best.entry((e.kind, e.from)).or_insert((e.seq, e.to));
        if e.seq < slot.0 {
            *slot = (e.seq, e.to);
        }
    }
    for ((kind, from), (_, to)) in best {
        let map = if kind == "part_of" {
            &mut m.section_page
        } else {
            &mut m.note_about
        };
        map.insert(from.to_owned(), Some(to.to_owned()));
    }

    let mut holders: BTreeMap<&str, Vec<(u64, &str)>> = BTreeMap::new();
    for (node, slug) in &slugs {
        // No let-chain: rust-version is 1.87.
        if let Some(("page", first)) = types.get(node) {
            if live(node) {
                holders.entry(slug).or_default().push((*first, node));
            }
        }
    }
    for (slug, mut pages) in holders {
        pages.sort_unstable();
        m.slug_owner.insert(slug.to_owned(), pages[0].1.to_owned());
        m.slug_conflict
            .extend(pages[1..].iter().map(|(_, p)| (*p).to_owned()));
    }
    m.slug_conflict.sort();
    m
}

fn seq(e: &DocEvent) -> u64 {
    match e {
        DocEvent::NodeAdd { seq, .. }
        | DocEvent::SlugSet { seq, .. }
        | DocEvent::EdgeAdd { seq, .. }
        | DocEvent::Retract { seq, .. }
        | DocEvent::Restore { seq, .. } => *seq,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: &str = "n_aaaaaaaaaaaaaaaaaaaaaadsea";
    const PA: &str = "n_aaaaaaaaaaaaaaaaaaaaaadpga";
    const PB: &str = "n_aaaaaaaaaaaaaaaaaaaaaadpgb";
    const NOTE: &str = "n_aaaaaaaaaaaaaaaaaaaaaadnta";

    fn edge(seq: u64, edge: &str, kind: &str, from: &str, to: &str) -> DocEvent {
        DocEvent::EdgeAdd {
            seq,
            edge: edge.into(),
            kind: kind.into(),
            from: from.into(),
            to: to.into(),
        }
    }
    fn page(seq: u64, node: &str, slug: &str) -> DocEvent {
        DocEvent::NodeAdd {
            seq,
            node: node.into(),
            node_type: "page".into(),
            slug: Some(slug.into()),
        }
    }
    fn retract(seq: u64, target: &str) -> DocEvent {
        DocEvent::Retract {
            seq,
            target: target.into(),
        }
    }

    // C0 2.2 vectors (hive-v2 vectors/doc_fold).
    #[test]
    fn part_of_and_about_vectors() {
        let two = [
            edge(5, "e_a", "part_of", SEC, PA),
            edge(7, "e_b", "part_of", SEC, PB),
        ];
        assert_eq!(
            fold(&two).section_page[SEC].as_deref(),
            Some(PA),
            "part_of_lowest_seq"
        );
        let mut after = two.to_vec();
        after.push(retract(9, "e_a"));
        assert_eq!(
            fold(&after).section_page[SEC].as_deref(),
            Some(PB),
            "part_of_after_retract"
        );
        let gone = [edge(5, "e_a", "part_of", SEC, PA), retract(6, "e_a")];
        assert_eq!(fold(&gone).section_page[SEC], None, "unattached");
        let about = [
            edge(4, "e_c", "about", NOTE, SEC),
            edge(8, "e_d", "about", NOTE, PA),
        ];
        assert_eq!(
            fold(&about).note_about[NOTE].as_deref(),
            Some(SEC),
            "about_lowest_seq"
        );
    }

    #[test]
    fn slug_vectors() {
        let both = [page(3, PA, "a"), page(4, PB, "a")];
        let m = fold(&both);
        assert_eq!(m.slug_owner["a"], PA);
        assert_eq!(m.slug_conflict, [PB]);

        let mut freed = both.to_vec();
        freed.push(retract(6, PA));
        let m = fold(&freed);
        assert_eq!(m.slug_owner["a"], PB);
        assert!(m.slug_conflict.is_empty());

        let mut renamed = both.to_vec();
        renamed.push(DocEvent::SlugSet {
            seq: 6,
            node: PA.into(),
            slug: "b".into(),
        });
        let m = fold(&renamed);
        assert_eq!(
            (m.slug_owner["a"].as_str(), m.slug_owner["b"].as_str()),
            (PB, PA)
        );
        assert!(m.slug_conflict.is_empty());
    }
}
