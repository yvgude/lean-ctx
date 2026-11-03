// SPDX-License-Identifier: Apache-2.0
//! Doc writes: the queued events are signed class-W C0 2.2 events, CAS
//! writes carry `base_event` (chained inside a batch), a screened text
//! queues nothing, and once Hive has them the replica assembles the page.

use ed25519_dalek::SigningKey;
use lean_ctx_doc::model::{Page, PageStatus};
use lean_ctx_doc::replica::LogEvent;
use serde_json::{Value, json};

use super::*;
use crate::events::build::{Place, Signer};
use crate::replica::log::StoredEvent;

const NOW: u64 = 1_792_800_020_000;

fn place() -> Place {
    Place {
        kind: "vault".into(),
        id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
        pk_id: [9; 32],
        causal_base: 0,
    }
}

fn screen() -> Screen {
    Screen::with_rules(
        Vec::new(),
        std::sync::Arc::new(Vec::new()),
        vec![regex::Regex::new("project-aurora").unwrap()],
    )
}

fn pending(replica: &Replica) -> Vec<Value> {
    replica
        .pending()
        .unwrap()
        .iter()
        .map(|(_, b)| serde_json::from_slice(b).unwrap())
        .collect()
}

/// Stores the pending events as Hive would serve them back, from `first`.
fn accept(replica: &mut Replica, events: &[Value], first: u64) {
    let stored: Vec<StoredEvent> = events
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let mut e = e.clone();
            let seq = first + i as u64;
            e["seq"] = json!(seq);
            e["provenance"] = json!("own");
            e["review_state"] = json!("confirmed");
            StoredEvent {
                seq,
                event: e,
                sig_invalid: false,
            }
        })
        .collect();
    replica.insert_events(&stored).unwrap();
    replica.refold().unwrap();
}

fn page(body: &str) -> Page {
    let (title, sections) = lean_ctx_doc::sections::split(body);
    Page {
        slug: "guide/start".into(),
        title,
        kind: "guide".into(),
        status: PageStatus::Draft,
        sections,
        anchors: Vec::new(),
        notes: Vec::new(),
    }
}

#[test]
fn a_page_is_queued_signed_and_reads_back_from_the_replica() {
    let data = tempfile::tempdir().unwrap();
    let (mut replica, _) = Replica::open(data.path(), "pl_docs", &[5; 32]).unwrap();
    let key = SigningKey::from_bytes(&[0x49; 32]);
    let signer = Signer::workspace(
        &key,
        "inst_6f2b9c4e1a3d4e5f8a7b0c1d2e3f4a5b",
        "wkr_vk4oelpv5lzjmev44hun",
    );
    let place = place();
    let target = Target {
        replica: &replica,
        place: &place,
        signer: &signer,
    };

    let mut ids = new_id;
    let (specs, page_id) =
        doc_write::create_page(&page("# Start\nintro\n## One\na\n"), &[], &mut ids);
    let queued = queue_specs(&target, specs, &mut screen(), NOW).unwrap();
    let events = pending(&replica);
    assert_eq!(events.len(), queued.len());
    let public = key.verifying_key().to_bytes();
    for e in &events {
        assert!(sign::verify(e, &public), "{}", e["op"]);
        assert!(e["body"]["body_nonce"].is_string());
        assert!(e.get("base_event").is_none(), "adds carry no base");
    }
    assert_eq!(events[0]["node_type"], "page");
    assert_eq!(events[0]["provenance_claim"], "own");

    accept(&mut replica, &events, 1);
    let space = space(&replica).unwrap();
    let p = space.page_by_slug("guide/start").expect("assembled");
    assert_eq!(p.page.id, page_id);
    assert_eq!(p.markdown(), "# Start\nintro\n## One\na\n");

    // Two edits of one section in one batch: the second names the first.
    let s = &p.sections[1];
    let specs = vec![
        doc_write::edit_section(s, "## One\nb\n"),
        doc_write::edit_section(s, "## One\nc\n"),
    ];
    let target = Target {
        replica: &replica,
        place: &place,
        signer: &signer,
    };
    let ids2 = queue_specs(&target, specs, &mut screen(), NOW + 1).unwrap();
    let edits: Vec<Value> = pending(&replica)
        .into_iter()
        .filter(|e| e["op"] == "field.set")
        .collect();
    assert_eq!(edits[0]["base_event"], s.head.as_str());
    assert_eq!(edits[1]["base_event"], ids2[0].as_str());
    assert_eq!(edits[0]["provenance_claim"], "workspace");

    // The next write, before Hive has the edits: the page reads them and
    // the CAS bases on the last queued one, not on Hive's older head.
    let space = super::space(&replica).unwrap();
    let p = space.page_by_slug("guide/start").unwrap();
    assert_eq!(p.markdown(), "# Start\nintro\n## One\nc\n");
    assert_eq!(p.sections[1].head, ids2[1]);
    let ids3 = queue_specs(
        &target,
        vec![doc_write::edit_section(&p.sections[1], "## One\nd\n")],
        &mut screen(),
        NOW + 2,
    )
    .unwrap();
    let last = pending(&replica)
        .into_iter()
        .find(|e| e["event_id"] == ids3[0].as_str())
        .unwrap();
    assert_eq!(last["base_event"], ids2[1].as_str());
}

#[test]
fn anchors_pin_the_checkout_and_the_recheck_marks_a_moved_page() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path().to_str().unwrap().to_owned();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q"]);
    std::fs::create_dir_all(repo.path().join("src/x")).unwrap();
    std::fs::write(repo.path().join("src/lib.rs"), "pub mod x;\n").unwrap();
    std::fs::write(repo.path().join("src/x/y.rs"), "pub fn y() {}\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);

    let files = tracked_files(&root).unwrap();
    assert_eq!(files, ["src/lib.rs", "src/x/y.rs"]);
    // The C0 2.2 vectors: file and glob digests over the same tree.
    let file = pin(&root, &files, "file:src/lib.rs").unwrap();
    assert_eq!(
        file.digest,
        "1497ba2ae9c58431b70c98c925ac011cdfc28b8b74ce9e0e5ae54c0261ec5fb9"
    );
    let glob = pin(&root, &files, "glob:src/**/*.rs").unwrap();
    assert_eq!(
        glob.digest,
        "e5ba8609e0f1153e71e6015814700e24ddf89abe7b5953363ccc53f79aff32f6"
    );
    assert!(pin(&root, &files, "file:../etc/passwd").is_err());
    assert!(pin(&root, &files, "file:src/gone.rs").is_err());

    let mut p = page("# Start\nintro\n");
    p.anchors = vec![file, pin(&root, &files, "dir:src/x").unwrap()];
    let mut ids = new_id;
    let (specs, _) = doc_write::create_page(&p, &[], &mut ids);
    let log: Vec<LogEvent> = specs
        .iter()
        .enumerate()
        .map(|(i, s)| LogEvent {
            seq: i as u64 + 1,
            event_id: format!("ev{i}"),
            op: s.op.into(),
            target: Some(s.target.clone()),
            node_type: s.node_type.map(Into::into),
            body: Some(Value::Object(s.body.clone())),
        })
        .collect();
    let space = lean_ctx_doc::replica::assemble(&log);
    assert!(recheck(&root, &space).is_empty(), "nothing moved");

    std::fs::write(repo.path().join("src/lib.rs"), "pub mod x;\npub mod z;\n").unwrap();
    std::fs::remove_file(repo.path().join("src/x/y.rs")).unwrap();
    let marks = recheck(&root, &space);
    assert_eq!(marks.len(), 2);
    assert_eq!(marks[0].body["value"], "to_verify");
    let drift = marks[1].body["value"].as_array().unwrap();
    assert_eq!(drift.len(), 2, "{drift:?}");
    assert!(
        marks.iter().all(|m| m.base_event.is_none()),
        "machine verdicts are CAS-exempt"
    );
}

#[test]
fn one_screened_text_queues_nothing() {
    let data = tempfile::tempdir().unwrap();
    let (replica, _) = Replica::open(data.path(), "pl_docs", &[5; 32]).unwrap();
    let key = SigningKey::from_bytes(&[0x49; 32]);
    let signer = Signer::workspace(
        &key,
        "inst_6f2b9c4e1a3d4e5f8a7b0c1d2e3f4a5b",
        "wkr_vk4oelpv5lzjmev44hun",
    );
    let place = place();
    let target = Target {
        replica: &replica,
        place: &place,
        signer: &signer,
    };
    let mut ids = new_id;
    let (specs, _) = doc_write::create_page(
        &page("# Start\nfine\n## Two\nproject-aurora ships\n"),
        &[],
        &mut ids,
    );
    let err = queue_specs(&target, specs, &mut screen(), NOW).unwrap_err();
    assert!(err.to_string().contains("screened"), "{err}");
    assert!(pending(&replica).is_empty());
}
