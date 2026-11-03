// SPDX-License-Identifier: Apache-2.0

//! Doc writes as unsigned place events (C0 2.2). The engine fills in the
//! envelope (event id, nonce, commit, HLC, signature) and queues them in
//! order; CAS-checked `field.set`s carry the head they were built on, so a
//! batch can chain edits: each event's base is the one before it.

use serde_json::{Map, Value, json};

use crate::anchor::{Anchor, AnchorMode, AnchorRole};
use crate::model::{Page, PageStatus};
use crate::replica::{PageView, SectionView};
use crate::sections;

/// One event to sign and queue.
#[derive(Debug, Clone, PartialEq)]
pub struct EventSpec {
    /// `node.add`, `field.set`, `edge.add` or `retract`.
    pub op: &'static str,
    pub target: String,
    /// For `node.add`: `page`, `section` or `note`.
    pub node_type: Option<&'static str>,
    /// The body without `body_nonce`.
    pub body: Map<String, Value>,
    /// The head this write was built on (CAS-checked fields only).
    pub base_event: Option<String>,
}

/// Makes ids: `"n"` for nodes, `"e"` for edges (`n_…`, `e_…`).
pub type NewId<'a> = &'a mut dyn FnMut(&str) -> String;

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// The wire form of an anchor: `{ref, digest, commit, mode?, role?}`.
pub fn anchor_value(a: &Anchor) -> Value {
    let mut m = obj(json!({ "ref": a.reference(), "digest": a.digest }));
    if let Some(c) = &a.commit {
        m.insert("commit".into(), Value::from(c.as_str()));
    }
    if a.mode == AnchorMode::Shape {
        m.insert("mode".into(), Value::from("shape"));
    }
    if a.role == AnchorRole::Cites {
        m.insert("role".into(), Value::from("cites"));
    }
    Value::Object(m)
}

fn status_str(s: PageStatus) -> &'static str {
    match s {
        PageStatus::Draft => "draft",
        PageStatus::Canonical => "canonical",
        PageStatus::Retired => "retired",
    }
}

fn node_add(node_type: &'static str, target: String, body: Value) -> EventSpec {
    EventSpec {
        op: "node.add",
        target,
        node_type: Some(node_type),
        body: obj(body),
        base_event: None,
    }
}

fn edge_add(kind: &str, from: &str, to: &str, new_id: NewId<'_>) -> EventSpec {
    EventSpec {
        op: "edge.add",
        target: new_id("e"),
        node_type: None,
        body: obj(json!({ "kind": kind, "from": from, "to": to })),
        base_event: None,
    }
}

/// The events that create `page`: the page node, its sections with their
/// `part_of` edges, its notes with their `about` edges (to the page) and a
/// `links_to` edge per linked page id. Bodies must already be in storage
/// form ([`crate::links::to_storage`]). Returns the events and the page id.
pub fn create_page(
    page: &Page,
    links_to: &[String],
    new_id: NewId<'_>,
) -> (Vec<EventSpec>, String) {
    let id = new_id("n");
    let mut body = obj(json!({
        "slug": page.slug,
        "title": page.title,
        "type": page.kind,
        "status": status_str(page.status),
    }));
    if !page.anchors.is_empty() {
        body.insert(
            "anchors".into(),
            page.anchors.iter().map(anchor_value).collect(),
        );
    }
    let mut out = vec![node_add("page", id.clone(), Value::Object(body))];
    for s in &page.sections {
        let sid = new_id("n");
        out.push(node_add(
            "section",
            sid.clone(),
            json!({ "body": s.text, "order": s.order }),
        ));
        out.push(edge_add("part_of", &sid, &id, new_id));
    }
    for n in &page.notes {
        let nid = new_id("n");
        let covers: Vec<Value> = n
            .covers
            .iter()
            .map(|r| json!({ "ref": r, "digest": Value::Null }))
            .collect();
        let mut nb = obj(json!({ "body": n.body, "status": "open" }));
        if !covers.is_empty() {
            nb.insert("covers".into(), Value::Array(covers));
        }
        out.push(node_add("note", nid.clone(), Value::Object(nb)));
        out.push(edge_add("about", &nid, &id, new_id));
    }
    for to in links_to {
        out.push(edge_add("links_to", &id, to, new_id));
    }
    (out, id)
}

/// A CAS-checked `field.set` of `field` on a node whose head is `base`.
pub fn field_set(target: &str, field: &str, value: Value, base: &str) -> EventSpec {
    let mut body = Map::new();
    body.insert("field".into(), Value::from(field));
    body.insert("value".into(), value);
    EventSpec {
        op: "field.set",
        target: target.to_owned(),
        node_type: None,
        body,
        base_event: Some(base.to_owned()),
    }
}

/// Replaces the body of `section`.
pub fn edit_section(section: &SectionView, body: &str) -> EventSpec {
    field_set(&section.id, "body", Value::from(body), &section.head)
}

/// Inserts a section with `text` after the section at `after` (`None`: at
/// the front). When no order fits between the neighbours the page is
/// renumbered first, one CAS `field.set order` per section.
pub fn insert_section(
    page: &PageView,
    after: Option<usize>,
    text: &str,
    new_id: NewId<'_>,
) -> Vec<EventSpec> {
    let secs = &page.sections;
    let before = after.and_then(|i| secs.get(i)).map(|s| s.order);
    let next = after.map_or(0, |i| i + 1);
    let behind = secs.get(next).map(|s| s.order);
    let mut out = Vec::new();
    let order = sections::order_between(before, behind).unwrap_or_else(|| {
        // No gap left: every section moves to `(i + 1) * ORDER_GAP`, the new
        // one taking the slot at `next`.
        let fresh = sections::renumber(secs.len() + 1);
        for (i, s) in secs.iter().enumerate() {
            let new = fresh[if i >= next { i + 1 } else { i }];
            if new != s.order {
                out.push(field_set(&s.id, "order", Value::from(new), &s.head));
            }
        }
        fresh[next]
    });
    let sid = new_id("n");
    out.push(node_add(
        "section",
        sid.clone(),
        json!({ "body": text, "order": order }),
    ));
    out.push(edge_add("part_of", &sid, &page.page.id, new_id));
    out
}

/// A note about `about` (a page or section id); `covers` are `(ref, digest)`
/// pairs the note accounts for.
pub fn add_note(
    about: &str,
    body: &str,
    covers: &[(String, Option<String>)],
    new_id: NewId<'_>,
) -> Vec<EventSpec> {
    let id = new_id("n");
    let mut nb = obj(json!({ "body": body, "status": "open" }));
    if !covers.is_empty() {
        let list: Vec<Value> = covers
            .iter()
            .map(|(r, d)| json!({ "ref": r, "digest": d }))
            .collect();
        nb.insert("covers".into(), Value::Array(list));
    }
    vec![
        node_add("note", id.clone(), Value::Object(nb)),
        edge_add("about", &id, about, new_id),
    ]
}

/// Confirms a page against the checkout: new anchors (CAS), then the
/// machine verdicts `anchor_state: fresh` and an empty `drift` (exempt).
/// A re-anchor carries `verified_at` (ms) in its body: C0 refuses one
/// without it.
pub fn confirm(page: &PageView, anchors: &[Anchor], verified_at_ms: u64) -> Vec<EventSpec> {
    let value: Vec<Value> = anchors.iter().map(anchor_value).collect();
    let exempt = |field: &str, value: Value| EventSpec {
        op: "field.set",
        target: page.page.id.clone(),
        node_type: None,
        body: obj(json!({ "field": field, "value": value })),
        base_event: None,
    };
    let mut anchors = field_set(
        &page.page.id,
        "anchors",
        Value::Array(value),
        &page.page.head,
    );
    anchors
        .body
        .insert("verified_at".into(), Value::from(verified_at_ms));
    vec![
        anchors,
        exempt("anchor_state", Value::from("fresh")),
        exempt("drift", Value::Array(Vec::new())),
    ]
}

/// `retract` of a node or an edge with `reason` (2–500 characters).
pub fn retract(target: &str, reason: &str) -> EventSpec {
    EventSpec {
        op: "retract",
        target: target.to_owned(),
        node_type: None,
        body: obj(json!({ "reason": reason })),
        base_event: None,
    }
}

/// The events that turn `page` into `markdown` (storage form: links
/// already `hive:`). Unchanged sections stay; a changed stretch becomes edits
/// of the sections it replaces, then inserts or retractions for the rest. A
/// new `# ` title is a CAS `field.set title`. `links_want` are the page ids
/// the new body links to; `links_now` the live `links_to` edges.
pub fn revise_page(
    page: &PageView,
    markdown: &str,
    links_now: &[(String, String)],
    links_want: &[String],
    new_id: NewId<'_>,
) -> Vec<EventSpec> {
    let (title, wanted) = sections::split(markdown);
    let mut out = Vec::new();
    if !title.is_empty() && title != page.title() {
        out.push(field_set(
            &page.page.id,
            "title",
            Value::from(title),
            &page.page.head,
        ));
    }
    let old: Vec<&str> = page.sections.iter().map(|s| s.body.as_str()).collect();
    let new: Vec<&str> = wanted.iter().map(|s| s.text.as_str()).collect();
    // Each new section either keeps an old one (same text, `Keep`), takes an
    // old one's place (`Edit`) or is added (`Add`); unmatched old ones go.
    let plan = align(&old, &new);
    let mut orders: Vec<Option<u32>> = plan
        .iter()
        .map(|step| match step {
            Step::Keep(i) | Step::Edit(i) => Some(page.sections[*i].order),
            Step::Add => None,
        })
        .collect();
    let renumber = !fill_orders(&mut orders);
    let fresh = sections::renumber(plan.len());
    for (k, step) in plan.iter().enumerate() {
        let order = if renumber {
            fresh[k]
        } else {
            orders[k].unwrap_or(fresh[k])
        };
        match step {
            Step::Keep(i) | Step::Edit(i) => {
                let s = &page.sections[*i];
                if matches!(step, Step::Edit(_)) {
                    out.push(edit_section(s, new[k]));
                }
                if order != s.order {
                    out.push(field_set(&s.id, "order", Value::from(order), &s.head));
                }
            }
            Step::Add => {
                let sid = new_id("n");
                out.push(node_add(
                    "section",
                    sid.clone(),
                    json!({ "body": new[k], "order": order }),
                ));
                out.push(edge_add("part_of", &sid, &page.page.id, new_id));
            }
        }
    }
    let used: std::collections::BTreeSet<usize> = plan
        .iter()
        .filter_map(|s| match s {
            Step::Keep(i) | Step::Edit(i) => Some(*i),
            Step::Add => None,
        })
        .collect();
    for (i, s) in page.sections.iter().enumerate() {
        if !used.contains(&i) {
            out.push(retract(&s.id, "removed from the page"));
        }
    }
    for (edge, to) in links_now {
        if !links_want.contains(to) {
            out.push(retract(edge, "the page no longer links there"));
        }
    }
    for to in links_want {
        if !links_now.iter().any(|(_, t)| t == to) && *to != page.page.id {
            out.push(edge_add("links_to", &page.page.id, to, new_id));
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Keep(usize),
    Edit(usize),
    Add,
}

/// Aligns `new` sections to `old` ones: the longest common subsequence of
/// equal texts is kept; between two kept anchors the old sections are
/// re-used in order for edits, the rest added or dropped.
fn align(old: &[&str], new: &[&str]) -> Vec<Step> {
    let (n, m) = (old.len(), new.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut steps = Vec::with_capacity(m);
    let (mut i, mut j) = (0, 0);
    let mut spare: Vec<usize> = Vec::new();
    while j < m {
        if i < n && old[i] == new[j] {
            spare.clear();
            steps.push(Step::Keep(i));
            i += 1;
            j += 1;
        } else if i < n && lcs[i + 1][j] >= lcs[i][j + 1] {
            spare.push(i);
            i += 1;
        } else {
            steps.push(if spare.is_empty() {
                Step::Add
            } else {
                Step::Edit(spare.remove(0))
            });
            j += 1;
        }
    }
    steps
}

/// Fills the `None` orders strictly between their known neighbours; false
/// when some gap is too small (or the known orders are not increasing).
fn fill_orders(orders: &mut [Option<u32>]) -> bool {
    let mut k = 0;
    let mut prev: Option<u32> = None;
    while k < orders.len() {
        if let Some(o) = orders[k] {
            if prev.is_some_and(|p| o <= p) {
                return false;
            }
            prev = Some(o);
            k += 1;
            continue;
        }
        let start = k;
        while k < orders.len() && orders[k].is_none() {
            k += 1;
        }
        let next = orders.get(k).copied().flatten();
        let lo = prev.unwrap_or(0);
        let count = (k - start) as u32;
        let Some(span) = next.map_or(Some(crate::ORDER_GAP.saturating_mul(count + 1)), |hi| {
            hi.checked_sub(lo)
        }) else {
            return false;
        };
        let step = span / (count + 1);
        if step == 0 {
            return false;
        }
        for (n, slot) in orders[start..k].iter_mut().enumerate() {
            *slot = Some(lo + step * (n as u32 + 1));
        }
        prev = orders[k - 1];
    }
    true
}

/// Chains the bases of a batch: a CAS write on a node another event of the
/// batch already wrote names that event, so the batch applies in order.
/// `event_ids` are the ids the engine gave each event, in batch order.
pub fn chain(specs: &mut [EventSpec], event_ids: &[String]) {
    let mut last: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (spec, id) in specs.iter_mut().zip(event_ids) {
        if spec.base_event.is_some() {
            if let Some(prev) = last.get(&spec.target) {
                spec.base_event = Some(prev.clone());
            }
        }
        if spec.op != "edge.add" {
            last.insert(spec.target.clone(), id.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replica::{LogEvent, assemble};

    fn ids() -> impl FnMut(&str) -> String {
        let mut n = 0;
        move |p: &str| {
            n += 1;
            format!("{p}_{n:026}")
        }
    }

    /// Applies specs as if the server accepted them, `seq` from `first`.
    fn apply(specs: &[EventSpec], first: u64) -> Vec<LogEvent> {
        specs
            .iter()
            .enumerate()
            .map(|(i, s)| LogEvent {
                seq: first + i as u64,
                event_id: format!("ev{}", first + i as u64),
                op: s.op.into(),
                target: Some(s.target.clone()),
                node_type: s.node_type.map(Into::into),
                body: Some(Value::Object(s.body.clone())),
            })
            .collect()
    }

    fn page() -> Page {
        let (title, secs) = sections::split("# Start\nintro\n## One\na\n## Two\nb\n");
        Page {
            slug: "guide/start".into(),
            title,
            kind: "guide".into(),
            status: PageStatus::Draft,
            sections: secs,
            anchors: Vec::new(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn a_created_page_assembles_back_to_its_markdown() {
        let mut new_id = ids();
        let (specs, id) = create_page(&page(), &[], &mut new_id);
        assert_eq!(specs.len(), 1 + 2 * 3);
        let space = assemble(&apply(&specs, 1));
        let p = &space.pages[&id];
        assert_eq!(p.markdown(), "# Start\nintro\n## One\na\n## Two\nb\n");
    }

    #[test]
    fn edits_and_inserts_keep_the_order_and_name_their_base() {
        let mut new_id = ids();
        let (specs, id) = create_page(&page(), &[], &mut new_id);
        let mut log = apply(&specs, 1);
        let space = assemble(&log);
        let p = &space.pages[&id];
        let edit = edit_section(&p.sections[1], "## One\nA!\n");
        assert_eq!(
            edit.base_event.as_deref(),
            Some(p.sections[1].head.as_str())
        );
        let ins = insert_section(p, Some(0), "## Half\nh\n", &mut new_id);
        let mut batch = vec![edit];
        batch.extend(ins);
        log.extend(apply(&batch, 100));
        let p = &assemble(&log).pages[&id];
        assert_eq!(
            p.markdown(),
            "# Start\nintro\n## Half\nh\n## One\nA!\n## Two\nb\n"
        );
    }

    #[test]
    fn a_full_gap_renumbers_before_the_insert() {
        let mut new_id = ids();
        let (mut specs, id) = create_page(&page(), &[], &mut new_id);
        // Squeeze the first two sections together: no integer fits between.
        if let Some(b) = specs
            .iter_mut()
            .find(|s| s.body.get("order") == Some(&Value::from(2048)))
        {
            b.body.insert("order".into(), Value::from(1025));
        }
        let mut log = apply(&specs, 1);
        let p = assemble(&log).pages[&id].clone();
        let ins = insert_section(&p, Some(0), "## Mid\nm\n", &mut new_id);
        assert!(ins.iter().filter(|s| s.op == "field.set").count() >= 2);
        log.extend(apply(&ins, 100));
        let p = &assemble(&log).pages[&id];
        assert_eq!(
            p.markdown(),
            "# Start\nintro\n## Mid\nm\n## One\na\n## Two\nb\n"
        );
    }

    fn revised(old: &str, new: &str) -> (Vec<EventSpec>, String) {
        let mut new_id = ids();
        let (title, secs) = sections::split(old);
        let p = Page {
            title,
            sections: secs,
            ..page()
        };
        let (specs, id) = create_page(&p, &[], &mut new_id);
        let mut log = apply(&specs, 1);
        let view = assemble(&log).pages[&id].clone();
        let rev = revise_page(&view, new, &[], &[], &mut new_id);
        log.extend(apply(&rev, 100));
        (rev, assemble(&log).pages[&id].markdown())
    }

    #[test]
    fn revise_keeps_unchanged_sections_and_edits_only_what_moved() {
        let old = "# T\nlead\n## A\na\n## B\nb\n## C\nc\n";
        let new = "# T\nlead\n## A\na\n## B\nB changed\n## C\nc\n";
        let (rev, md) = revised(old, new);
        assert_eq!(md, new);
        assert_eq!(rev.len(), 1, "one edit: {rev:?}");
        assert_eq!(rev[0].op, "field.set");
    }

    #[test]
    fn revise_inserts_removes_and_retitles() {
        let old = "# T\nlead\n## A\na\n## B\nb\n";
        let new = "# New\nlead\n## X\nx\n## B\nb\n## Z\nz\n";
        let (rev, md) = revised(old, new);
        assert_eq!(md, new);
        assert!(
            rev.iter()
                .any(|s| s.body.get("field") == Some(&Value::from("title")))
        );
        assert!(
            !rev.iter().any(|s| s.op == "retract"),
            "A became X by an edit"
        );
        let (rev, md) = revised(old, "# T\nlead\n## B\nb\n");
        assert_eq!(md, "# T\nlead\n## B\nb\n");
        assert_eq!(rev.iter().filter(|s| s.op == "retract").count(), 1);
    }

    #[test]
    fn revise_renumbers_when_inserts_do_not_fit() {
        let old = "# T\nlead\n## A\na\n";
        let many = (0..1100).fold(String::new(), |mut acc, i| {
            acc.push_str("## S");
            acc.push_str(&i.to_string());
            acc.push_str("\ns\n");
            acc
        });
        let new = format!("# T\nlead\n{many}## A\na\n");
        let (_, md) = revised(old, &new);
        assert_eq!(md, new);
    }

    #[test]
    fn revise_keeps_links_to_in_step() {
        let mut new_id = ids();
        let (specs, id) = create_page(&page(), &[], &mut new_id);
        let view = assemble(&apply(&specs, 1)).pages[&id].clone();
        let now = vec![
            ("e_old".to_owned(), "n_gone".to_owned()),
            ("e_keep".to_owned(), "n_b".to_owned()),
        ];
        let rev = revise_page(
            &view,
            &view.markdown(),
            &now,
            &["n_b".into(), "n_c".into()],
            &mut new_id,
        );
        assert_eq!(rev.len(), 2);
        assert_eq!((rev[0].op, rev[0].target.as_str()), ("retract", "e_old"));
        assert_eq!(rev[1].body.get("to"), Some(&Value::from("n_c")));
    }

    #[test]
    fn chain_names_the_previous_write_of_the_same_node() {
        let mut specs = vec![
            field_set("n_a", "body", Value::from("1"), "h0"),
            field_set("n_a", "body", Value::from("2"), "h0"),
            field_set("n_b", "body", Value::from("x"), "hb"),
        ];
        chain(&mut specs, &["e1".into(), "e2".into(), "e3".into()]);
        let bases: Vec<_> = specs
            .iter()
            .map(|s| s.base_event.clone().unwrap_or_default())
            .collect();
        assert_eq!(bases, ["h0", "e1", "hb"]);
    }

    #[test]
    fn confirm_sends_anchors_with_base_and_exempt_verdicts() {
        let mut new_id = ids();
        let (specs, id) = create_page(&page(), &[], &mut new_id);
        let p = assemble(&apply(&specs, 1)).pages[&id].clone();
        let c = confirm(&p, &[], 1_792_800_020_000);
        assert_eq!(c[0].base_event.as_deref(), Some(p.page.head.as_str()));
        assert_eq!(c[0].body["verified_at"], 1_792_800_020_000_u64);
        assert!(c[1].base_event.is_none() && c[2].base_event.is_none());
    }
}
