// SPDX-License-Identifier: Apache-2.0
//! `ctx_doc` over a replica view: reads render from the assembled pages,
//! writes plan C0 2.2 events (create, revise, edit, note).

use lean_ctx_doc::replica::{LogEvent, assemble};
use serde_json::Value;

use super::*;

fn args(action: &'static str) -> Args<'static> {
    Args {
        action,
        ..Args::default()
    }
}

/// A space built by applying planned writes as if Hive accepted them.
fn apply(log: &mut Vec<LogEvent>, specs: &[write::EventSpec]) -> DocSpace {
    let first = log.len() as u64 + 1;
    for (i, s) in specs.iter().enumerate() {
        log.push(LogEvent {
            seq: first + i as u64,
            event_id: format!("ev{}", first + i as u64),
            op: s.op.into(),
            target: Some(s.target.clone()),
            node_type: s.node_type.map(Into::into),
            body: Some(Value::Object(s.body.clone())),
        });
    }
    assemble(log)
}

fn seeded() -> (Vec<LogEvent>, DocSpace) {
    let mut log = Vec::new();
    let a = Args {
        slug: Some("guide/retry"),
        content: Some("# Retry\nPayments retry.\n## Budget\nThree tries.\n## Backoff\nJittered.\n"),
        ..args("write")
    };
    let space = apply(
        &mut log,
        &plan_write(&DocSpace::default(), &a, &[]).unwrap(),
    );
    let b = Args {
        slug: Some("guide/index"),
        content: Some("# Index\nSee [retry](/guide/retry#budget).\n"),
        ..args("write")
    };
    let space = apply(&mut log, &plan_write(&space, &b, &[]).unwrap());
    (log, space)
}

#[test]
fn list_show_outline_section_and_search_read_the_replica() {
    let (_, space) = seeded();
    assert_eq!(
        read(&space, &args("list")),
        "2 page(s)\n/guide/index — Index [fresh]\n/guide/retry — Retry [fresh]\n"
    );
    let show = read(
        &space,
        &Args {
            slug: Some("guide/index"),
            ..args("show")
        },
    );
    assert!(
        show.contains("[retry](/guide/retry#budget)"),
        "links read as slugs: {show}"
    );
    let outline = read(
        &space,
        &Args {
            slug: Some("guide/retry"),
            outline: true,
            ..args("show")
        },
    );
    assert!(outline.contains("## Budget  (#budget)"), "{outline}");
    let section = read(
        &space,
        &Args {
            slug: Some("guide/retry"),
            section: Some("budget"),
            ..args("show")
        },
    );
    assert!(section.ends_with("## Budget\nThree tries.\n"), "{section}");
    assert_eq!(
        read(
            &space,
            &Args {
                query: Some("jittered"),
                ..args("search")
            }
        ),
        "/guide/retry — Retry (1)\n"
    );
    assert_eq!(read(&space, &args("status")), "2 page(s), all fresh");
}

#[test]
fn links_are_stored_by_id_and_kept_as_edges() {
    let (log, space) = seeded();
    let index = space.page_by_slug("guide/index").unwrap();
    let retry = space.page_by_slug("guide/retry").unwrap();
    assert!(
        index
            .markdown()
            .contains(&format!("hive:{}#budget", retry.page.id))
    );
    assert_eq!(space.links[&index.page.id].len(), 1);
    assert_eq!(space.links[&index.page.id][0].1, retry.page.id);
    drop(log);
}

#[test]
fn edit_and_write_revise_only_what_changed() {
    let (mut log, space) = seeded();
    let edit = Args {
        slug: Some("guide/retry"),
        find: Some("Three tries."),
        replace: Some("Five tries."),
        ..args("edit")
    };
    let specs = plan_write(&space, &edit, &[]).unwrap();
    assert_eq!(specs.len(), 1, "{specs:?}");
    assert!(specs[0].base_event.is_some());
    let space = apply(&mut log, &specs);
    let md = space.page_by_slug("guide/retry").unwrap().markdown();
    assert!(md.contains("Five tries.") && md.contains("Jittered."));

    let none = Args {
        slug: Some("guide/retry"),
        find: Some("nowhere"),
        replace: Some("x"),
        ..args("edit")
    };
    assert!(
        plan_write(&space, &none, &[])
            .unwrap_err()
            .to_string()
            .contains("matches nothing")
    );
    let bad = Args {
        slug: Some("Guide/X"),
        content: Some("# X\n"),
        ..args("write")
    };
    assert!(plan_write(&space, &bad, &[]).is_err());
    let untitled = Args {
        slug: Some("guide/x"),
        content: Some("no title\n"),
        ..args("write")
    };
    assert!(plan_write(&space, &untitled, &[]).is_err());
}

#[test]
fn a_note_shows_on_the_page() {
    let (mut log, space) = seeded();
    let note = Args {
        slug: Some("guide/retry"),
        content: Some("The budget moved to config."),
        ..args("note")
    };
    let space = apply(&mut log, &plan_write(&space, &note, &[]).unwrap());
    let show = read(
        &space,
        &Args {
            slug: Some("guide/retry"),
            ..args("show")
        },
    );
    assert!(show.contains("note: The budget moved to config."), "{show}");
    assert!(read(&space, &args("list")).contains("Retry [fresh, 1 note(s)]"));
}

#[test]
fn slugs_follow_the_c0_rule() {
    assert!(valid_slug("knowledge/wiki") && valid_slug("a-b/c1"));
    assert!(
        !valid_slug("A")
            && !valid_slug("a//b")
            && !valid_slug("-a")
            && !valid_slug("a--b")
            && !valid_slug("a/b/c/d/e/f/g/h/i")
    );
}
