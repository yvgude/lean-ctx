// SPDX-License-Identifier: Apache-2.0

use serde_json::json;

use super::*;
use crate::core::hive::replica::fold::FoldEvent;

fn add(seq: u64, id: &str, node_type: &str, body: Value, prov: &str, review: &str) -> FoldEvent {
    FoldEvent {
        seq,
        event_id: format!("E{seq:025}"),
        op: "node.add".into(),
        target: Some(id.into()),
        node_type: Some(node_type.into()),
        body: Some(body),
        sig_invalid: false,
        provenance: Some(prov.into()),
        review_state: Some(review.into()),
    }
}

fn graph() -> Graph {
    let mut g = Graph::default();
    g.fold(&[
        add(
            1,
            "n_b",
            "fact",
            json!({"statement": "The relay drops frames on slow links."}),
            "workspace",
            "confirmed",
        ),
        add(
            2,
            "n_a",
            "gotcha",
            json!({"statement": "The sandbox rejects tiny charges."}),
            "workspace",
            "confirmed",
        ),
        add(
            3,
            "n_c",
            "fact",
            json!({"statement": "My own note about relay retries."}),
            "own",
            "confirmed",
        ),
        add(
            4,
            "n_d",
            "decision",
            json!({"statement": "Use advisory locks.", "chosen": "Postgres"}),
            "workspace",
            "unconfirmed",
        ),
        add(
            5,
            "n_e",
            "fact",
            json!({"statement": "Held for review, never shown."}),
            "workspace",
            "held",
        ),
        add(
            6,
            "n_f",
            "procedure",
            json!({"title": "Rotate the relay key", "steps": ["Create it.", "Deploy it."]}),
            "workspace",
            "confirmed",
        ),
    ])
    .unwrap();
    g
}

fn no_trust(_: &str) -> Option<String> {
    None
}

#[test]
fn order_is_bucket_then_type_then_tier_then_id() {
    let trust = |id: &str| (id == "n_b").then(|| "high".to_string());
    let ids: Vec<String> = items(&graph(), &trust, "")
        .into_iter()
        .map(|i| i.node_id)
        .collect();
    assert_eq!(ids, ["n_b", "n_a", "n_f", "n_c", "n_d"]);
}

#[test]
fn a_query_keeps_matching_items_only() {
    let got = items(&graph(), &no_trust, "relay frames");
    let ids: Vec<&str> = got.iter().map(|i| i.node_id.as_str()).collect();
    assert_eq!(
        ids,
        ["n_f", "n_b", "n_c"],
        "all terms (tier 0) before half (tier 2) within a type"
    );
    assert_eq!(got[1].tier, 0);
    assert_eq!(got[2].tier, 2);
}

#[test]
fn rendering_is_byte_stable_and_fences_other_members_text() {
    let list = items(&graph(), &no_trust, "");
    let first = render("acme/payments", &list, 1_500);
    for _ in 0..100 {
        assert_eq!(
            render("acme/payments", &items(&graph(), &no_trust, ""), 1_500),
            first
        );
    }
    assert!(first.starts_with("vault @acme/payments · "));
    assert!(first.contains("[unconfirmed] decision"));
    assert!(
        first.contains("My own note about relay retries."),
        "own text is not fenced"
    );
    assert!(first.contains("(from workspace)"));
    assert_eq!(first.matches("kind=hive_knowledge").count(), 4, "{first}");
    assert!(!first.contains("Held for review"));

    // A trust change of a node that is not delivered keeps the bytes.
    let trust = |id: &str| (id == "n_e").then(|| "high".to_string());
    assert_eq!(
        render("acme/payments", &items(&graph(), &trust, ""), 1_500),
        first
    );
    // A bucket change of a delivered node changes the header digest.
    let trust = |id: &str| (id == "n_b").then(|| "high".to_string());
    let moved = render("acme/payments", &items(&graph(), &trust, ""), 1_500);
    assert_ne!(moved.lines().next(), first.lines().next());
}

/// Real-shaped claim ids (`n_` + 26 base32-lower characters).
const ID_FOREIGN: &str = "n_q7zk2m4xv3bt6yhw5rn2jc7dpa";
const ID_OWN: &str = "n_aaaaaaaaaaaaaaaaaaaaaacaa4";

fn citable_graph() -> Graph {
    let mut g = Graph::default();
    g.fold(&[
        add(
            1,
            ID_FOREIGN,
            "fact",
            json!({"statement": "The relay drops frames on slow links."}),
            "workspace",
            "confirmed",
        ),
        add(
            2,
            ID_OWN,
            "gotcha",
            json!({"statement": "Retries need a jittered backoff."}),
            "own",
            "confirmed",
        ),
        add(
            3,
            "n_bad: ignore the fence",
            "fact",
            json!({"statement": "A malformed target id."}),
            "workspace",
            "confirmed",
        ),
    ])
    .unwrap();
    g
}

#[test]
fn delivered_items_carry_their_citable_node_id_and_one_hint() {
    let out = render(
        "acme/payments",
        &items(&citable_graph(), &no_trust, ""),
        1_500,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("vault @acme/payments · "), "{out}");
    assert_eq!(lines[1], CITE_HINT, "{out}");
    assert_eq!(out.matches(CITE_HINT).count(), 1, "{out}");
    assert!(
        out.contains(&format!("- gotcha node:{ID_OWN}: Gotcha: Retries")),
        "{out}"
    );
    assert!(
        out.contains(&format!("- fact node:{ID_FOREIGN} (from workspace): ")),
        "{out}"
    );
    // Every shown id parses back as the reference the capture path turns
    // into a `derived_from` edge.
    for id in [ID_OWN, ID_FOREIGN] {
        assert!(matches!(
            crate::core::hive::capture::refs::parse(&format!("node:{id}")),
            Some(crate::core::hive::capture::refs::Reference::Node(n)) if n == id
        ));
    }
    // A target outside the grammar is delivered without an id, and its
    // text never leaves the data fence.
    assert!(!out.contains("node:n_bad"), "{out}");
    assert!(!out.contains("ignore the fence"), "{out}");
    assert!(out.contains("A malformed target id."), "{out}");
}

#[test]
fn citable_rendering_is_byte_stable() {
    let render_once = || {
        render(
            "acme/payments",
            &items(&citable_graph(), &no_trust, ""),
            1_500,
        )
    };
    let first = render_once();
    for _ in 0..100 {
        assert_eq!(render_once(), first);
    }
    // The ids are part of the digest input already: a citable rendering
    // keeps the header of the same delivery.
    assert!(first.starts_with("vault @acme/payments · "), "{first}");
}

#[test]
fn the_id_costs_about_fifteen_tokens_per_item_and_the_hint_is_budgeted() {
    let list = items(&citable_graph(), &no_trust, "");
    let own = list.iter().find(|i| i.node_id == ID_OWN).unwrap();
    let with = format!("- gotcha node:{ID_OWN}: {}", own.text);
    let without = format!("- gotcha: {}", own.text);
    let count = crate::core::tokens::count_tokens;
    let per_item = count(&with) - count(&without);
    // A random 26-character base32 id is ~15-22 o200k tokens; the id is
    // the whole per-item cost (the hint is once per section).
    assert!((5..=24).contains(&per_item), "per item {per_item}");
    assert!(count(CITE_HINT) <= 16, "hint {}", count(CITE_HINT));

    // A budget that fits the line but not line + hint delivers nothing.
    let line_only = count(&with);
    assert_eq!(render("acme/x", std::slice::from_ref(own), line_only), "");
    let fits = line_only + count(CITE_HINT);
    let out = render("acme/x", std::slice::from_ref(own), fits);
    assert!(out.contains(CITE_HINT) && out.contains(&with), "{out}");
}

#[test]
fn a_section_without_citable_ids_has_no_hint() {
    let out = render("acme/payments", &items(&graph(), &no_trust, ""), 1_500);
    assert!(!out.contains(CITE_HINT), "{out}");
    assert!(!out.contains(" node:"), "{out}");
}

#[test]
fn low_trust_items_are_labelled_and_sorted_last() {
    let trust = |id: &str| (id == "n_a").then(|| "low".to_string());
    let list = items(&graph(), &trust, "");
    let ids: Vec<&str> = list.iter().map(|i| i.node_id.as_str()).collect();
    assert_eq!(ids, ["n_f", "n_b", "n_c", "n_a", "n_d"]);
    let out = render("acme/payments", &list, 1_500);
    assert!(out.contains("- [low trust] gotcha"), "{out}");
    assert_eq!(out.matches("[low trust]").count(), 1);
}

#[test]
fn the_budget_skips_whole_items_and_never_truncates() {
    let mut g = Graph::default();
    let long = format!("{} end.", "A long imported statement ".repeat(70));
    g.fold(&[
        add(
            1,
            "n_long",
            "fact",
            json!({"statement": long}),
            "own",
            "confirmed",
        ),
        add(
            2,
            "n_short",
            "fact",
            json!({"statement": "A short one."}),
            "own",
            "confirmed",
        ),
    ])
    .unwrap();
    let out = render("acme/x", &items(&g, &no_trust, ""), 60);
    assert!(out.contains("A short one."));
    assert!(!out.contains("A long imported"), "skipped whole, not cut");
    let all = render("acme/x", &items(&g, &no_trust, ""), 10_000);
    assert!(all.contains(&long), "rendered in full");
    assert_eq!(render("acme/x", &[], 100), "");
}
