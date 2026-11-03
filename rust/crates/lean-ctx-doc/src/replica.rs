// SPDX-License-Identifier: Apache-2.0

//! Pages assembled from a place log (C0 2.2): the engine hands over its
//! verified events, in any order, and reads pages, sections, notes, each
//! node's head event (the base of a CAS write) and the derived doc state.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::fold::{self, DocEvent};
use crate::state::{self, Drift, NoteCovers, Verdict};

/// One verified place event as the engine's replica holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct LogEvent {
    pub seq: u64,
    pub event_id: String,
    pub op: String,
    pub target: Option<String>,
    pub node_type: Option<String>,
    /// `None` for a purged or withheld body.
    pub body: Option<Value>,
}

/// A doc node with its fields as folded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DocNode {
    pub id: String,
    pub node_type: String,
    pub fields: Map<String, Value>,
    pub retracted: bool,
    /// The event a CAS write names as `base_event`.
    pub head: String,
    /// The seq of the node's first `node.add`.
    pub added_seq: u64,
}

impl DocNode {
    pub fn str(&self, field: &str) -> Option<&str> {
        self.fields.get(field).and_then(Value::as_str)
    }
}

/// A section of a page, in reading order.
#[derive(Debug, Clone, PartialEq)]
pub struct SectionView {
    pub id: String,
    pub order: u32,
    pub body: String,
    pub head: String,
}

/// A note about a page or one of its sections.
#[derive(Debug, Clone, PartialEq)]
pub struct NoteView {
    pub id: String,
    pub about: String,
    pub body: String,
    pub open: bool,
    pub covers: Vec<Drift>,
    pub head: String,
}

/// A live page, assembled.
#[derive(Debug, Clone, PartialEq)]
pub struct PageView {
    pub page: DocNode,
    pub sections: Vec<SectionView>,
    pub notes: Vec<NoteView>,
    pub slug_conflict: bool,
    pub state: Verdict,
}

impl PageView {
    pub fn slug(&self) -> &str {
        self.page.str("slug").unwrap_or_default()
    }
    pub fn title(&self) -> &str {
        self.page.str("title").unwrap_or_default()
    }
    /// The page markdown: `# {title}\n` and the section bodies in order.
    pub fn markdown(&self) -> String {
        let mut out = format!("# {}\n", self.title());
        for s in &self.sections {
            out.push_str(&s.body);
        }
        out
    }
}

/// Every live doc page of a place.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DocSpace {
    /// Page id → page, live pages only.
    pub pages: BTreeMap<String, PageView>,
    /// Slug → page id (the slug owner).
    pub by_slug: BTreeMap<String, String>,
    /// Page id → its live `links_to` edges as `(edge id, target page id)`.
    pub links: BTreeMap<String, Vec<(String, String)>>,
}

impl DocSpace {
    pub fn page_by_slug(&self, slug: &str) -> Option<&PageView> {
        self.by_slug.get(slug).and_then(|id| self.pages.get(id))
    }
}

const DOC_TYPES: [&str; 3] = ["page", "section", "note"];
const HEAD_OPS: [&str; 4] = ["node.add", "field.set", "retract", "restore"];

/// Folds `events` into the doc pages of the place.
pub fn assemble(events: &[LogEvent]) -> DocSpace {
    let mut ordered: Vec<&LogEvent> = events.iter().collect();
    ordered.sort_by_key(|e| e.seq);
    let (nodes, doc_events) = fold_nodes(&ordered);
    let m = fold::fold(&doc_events);

    let mut sections: BTreeMap<&str, Vec<SectionView>> = BTreeMap::new();
    for (sec, page) in &m.section_page {
        let (Some(page), Some(n)) = (page, nodes.get(sec)) else {
            continue;
        };
        if n.retracted {
            continue;
        }
        sections.entry(page).or_default().push(SectionView {
            id: n.id.clone(),
            order: n.fields.get("order").and_then(Value::as_u64).unwrap_or(0) as u32,
            body: n.str("body").unwrap_or_default().to_owned(),
            head: n.head.clone(),
        });
    }
    let page_of = |about: &str| -> Option<String> {
        let n = nodes.get(about).filter(|n| !n.retracted)?;
        match n.node_type.as_str() {
            "page" => Some(about.to_owned()),
            "section" => m.section_page.get(about).cloned().flatten(),
            _ => None,
        }
    };
    let mut notes: BTreeMap<String, Vec<NoteView>> = BTreeMap::new();
    for (note, about) in &m.note_about {
        let (Some(about), Some(n)) = (about, nodes.get(note)) else {
            continue;
        };
        if n.retracted {
            continue;
        }
        if let Some(page) = page_of(about) {
            notes.entry(page).or_default().push(NoteView {
                id: n.id.clone(),
                about: about.clone(),
                body: n.str("body").unwrap_or_default().to_owned(),
                open: n.str("status") != Some("resolved"),
                covers: drift_list(n.fields.get("covers")),
                head: n.head.clone(),
            });
        }
    }

    let mut space = DocSpace::default();
    for n in nodes
        .values()
        .filter(|n| n.node_type == "page" && !n.retracted)
    {
        let mut secs = sections.remove(n.id.as_str()).unwrap_or_default();
        secs.sort_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)));
        let page_notes = notes.remove(&n.id).unwrap_or_default();
        let state = page_state(n, &page_notes);
        space.pages.insert(
            n.id.clone(),
            PageView {
                page: n.clone(),
                sections: secs,
                notes: page_notes,
                slug_conflict: m.slug_conflict.contains(&n.id),
                state,
            },
        );
    }
    space.by_slug = m.slug_owner;
    space.links = links_of(&ordered);
    space
}

/// The live `links_to` edges of the log, by their `from` page.
fn links_of(ordered: &[&LogEvent]) -> BTreeMap<String, Vec<(String, String)>> {
    let mut edges: BTreeMap<&str, (&str, &str, bool)> = BTreeMap::new();
    for e in ordered {
        let Some(target) = e.target.as_deref() else {
            continue;
        };
        let get = |k: &str| {
            e.body
                .as_ref()
                .and_then(|b| b.get(k))
                .and_then(Value::as_str)
        };
        match e.op.as_str() {
            "edge.add" if get("kind") == Some("links_to") => {
                if let (Some(from), Some(to)) = (get("from"), get("to")) {
                    edges.entry(target).or_insert((from, to, true));
                }
            }
            "retract" | "restore" => {
                if let Some(edge) = edges.get_mut(target) {
                    edge.2 = e.op == "restore";
                }
            }
            _ => {}
        }
    }
    let mut out: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (id, (from, to, live)) in edges {
        if live {
            out.entry(from.to_owned())
                .or_default()
                .push((id.to_owned(), to.to_owned()));
        }
    }
    out
}

/// The doc nodes (fields folded, head and first seq) and the membership
/// events of the log.
fn fold_nodes(ordered: &[&LogEvent]) -> (BTreeMap<String, DocNode>, Vec<DocEvent>) {
    let mut nodes: BTreeMap<String, DocNode> = BTreeMap::new();
    let mut doc_events = Vec::new();
    for e in ordered {
        let Some(target) = e.target.as_deref() else {
            continue;
        };
        let body = e.body.as_ref();
        match e.op.as_str() {
            "node.add" => {
                let Some(t) = e.node_type.as_deref().filter(|t| DOC_TYPES.contains(t)) else {
                    continue;
                };
                let mut fields = body.and_then(Value::as_object).cloned().unwrap_or_default();
                fields.remove("body_nonce");
                doc_events.push(DocEvent::NodeAdd {
                    seq: e.seq,
                    node: target.to_owned(),
                    node_type: t.to_owned(),
                    slug: fields
                        .get("slug")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                });
                nodes.entry(target.to_owned()).or_insert(DocNode {
                    id: target.to_owned(),
                    node_type: t.to_owned(),
                    fields,
                    added_seq: e.seq,
                    ..DocNode::default()
                });
            }
            "field.set" => {
                let Some(n) = nodes.get_mut(target) else {
                    continue;
                };
                if let Some(field) = body.and_then(|b| b.get("field")).and_then(Value::as_str) {
                    let value = body
                        .and_then(|b| b.get("value"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    if field == "slug" {
                        if let Some(s) = value.as_str() {
                            doc_events.push(DocEvent::SlugSet {
                                seq: e.seq,
                                node: target.to_owned(),
                                slug: s.to_owned(),
                            });
                        }
                    }
                    n.fields.insert(field.to_owned(), value);
                }
            }
            "edge.add" => {
                let get = |k: &str| {
                    body.and_then(|b| b.get(k))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                let kind = get("kind");
                if kind == "part_of" || kind == "about" {
                    doc_events.push(DocEvent::EdgeAdd {
                        seq: e.seq,
                        edge: target.to_owned(),
                        kind,
                        from: get("from"),
                        to: get("to"),
                    });
                }
            }
            "retract" | "restore" => {
                let retract = e.op == "retract";
                doc_events.push(if retract {
                    DocEvent::Retract {
                        seq: e.seq,
                        target: target.to_owned(),
                    }
                } else {
                    DocEvent::Restore {
                        seq: e.seq,
                        target: target.to_owned(),
                    }
                });
                if let Some(n) = nodes.get_mut(target) {
                    n.retracted = retract;
                }
            }
            _ => {}
        }
        if HEAD_OPS.contains(&e.op.as_str()) {
            if let Some(n) = nodes.get_mut(target) {
                n.head.clone_from(&e.event_id);
            }
        }
    }
    (nodes, doc_events)
}

fn drift_list(v: Option<&Value>) -> Vec<Drift> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|d| serde_json::from_value(d.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// The page's doc state from its own fields and its notes.
fn page_state(page: &DocNode, notes: &[NoteView]) -> Verdict {
    let derived: Vec<&str> = page
        .fields
        .get("anchors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|a| a.get("role").and_then(Value::as_str).unwrap_or("derived") == "derived")
        .filter_map(|a| a.get("ref").and_then(Value::as_str))
        .collect();
    let drift = drift_list(page.fields.get("drift"));
    let covers: Vec<NoteCovers> = notes
        .iter()
        .map(|n| NoteCovers {
            open: n.open,
            covers: n.covers.clone(),
        })
        .collect();
    state::of(page.str("anchor_state"), &derived, &drift, &covers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(seq: u64, op: &str, target: &str, node_type: Option<&str>, body: Value) -> LogEvent {
        LogEvent {
            seq,
            event_id: format!("ev{seq}"),
            op: op.into(),
            target: Some(target.into()),
            node_type: node_type.map(Into::into),
            body: Some(body),
        }
    }

    fn log() -> Vec<LogEvent> {
        vec![
            ev(
                1,
                "node.add",
                "n_p",
                Some("page"),
                json!({"slug": "guide/start", "title": "Start", "type": "guide", "status": "draft",
                "anchors": [{"ref": "file:a.rs", "digest": "d1", "commit": "c"}, {"ref": "file:b.md", "digest": "d2", "commit": "c", "role": "cites"}]}),
            ),
            ev(
                3,
                "node.add",
                "n_s2",
                Some("section"),
                json!({"body": "## Two\nb\n", "order": 2048}),
            ),
            ev(
                2,
                "node.add",
                "n_s1",
                Some("section"),
                json!({"body": "intro\n", "order": 1024}),
            ),
            ev(
                4,
                "edge.add",
                "e_1",
                None,
                json!({"kind": "part_of", "from": "n_s1", "to": "n_p"}),
            ),
            ev(
                5,
                "edge.add",
                "e_2",
                None,
                json!({"kind": "part_of", "from": "n_s2", "to": "n_p"}),
            ),
            ev(
                6,
                "field.set",
                "n_s1",
                None,
                json!({"field": "body", "value": "Intro.\n"}),
            ),
            ev(
                7,
                "node.add",
                "n_n",
                Some("note"),
                json!({"body": "a.rs moved", "status": "open", "covers": [{"ref": "file:a.rs", "digest": "d9"}]}),
            ),
            ev(
                8,
                "edge.add",
                "e_3",
                None,
                json!({"kind": "about", "from": "n_n", "to": "n_s2"}),
            ),
            ev(
                9,
                "field.set",
                "n_p",
                None,
                json!({"field": "anchor_state", "value": "to_verify"}),
            ),
            ev(
                10,
                "field.set",
                "n_p",
                None,
                json!({"field": "drift", "value": [{"ref": "file:a.rs", "observed": "d9"}]}),
            ),
            ev(
                11,
                "node.add",
                "n_k",
                Some("fact"),
                json!({"statement": "not a doc"}),
            ),
        ]
    }

    #[test]
    fn assembles_pages_in_order_with_heads_and_notes() {
        let space = assemble(&log());
        assert_eq!(space.pages.len(), 1);
        let p = space.page_by_slug("guide/start").expect("page");
        assert_eq!(p.markdown(), "# Start\nIntro.\n## Two\nb\n");
        assert_eq!(p.sections[0].head, "ev6", "a field.set moves the head");
        assert_eq!(p.page.head, "ev10");
        assert_eq!(p.notes.len(), 1);
        assert_eq!(
            p.notes[0].about, "n_s2",
            "a note about a section belongs to its page"
        );
        assert_eq!(p.state.state, crate::state::DocState::Noted);
    }

    #[test]
    fn a_retracted_section_or_resolved_note_changes_the_view() {
        let mut events = log();
        events.push(ev(12, "retract", "n_s2", None, json!({"reason": "gone"})));
        events.push(ev(
            13,
            "field.set",
            "n_n",
            None,
            json!({"field": "status", "value": "resolved"}),
        ));
        let space = assemble(&events);
        let p = space.page_by_slug("guide/start").expect("page");
        assert_eq!(p.markdown(), "# Start\nIntro.\n");
        assert!(p.notes.is_empty(), "the note's section is gone");
        assert_eq!(p.state.state, crate::state::DocState::Stale);
    }
}
