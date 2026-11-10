// SPDX-License-Identifier: Apache-2.0
//! Local docs: the Local Read v1 shapes Hive Desktop builds against, and the
//! revision history behind them.

use serde_json::{Value, json};

use super::*;

fn doc(kind: &str, body: &impl Serialize) -> Value {
    serde_json::from_str(&local_read::document(kind, body)).expect("one JSON object")
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn local_pages_print_list_page_history_and_saved_documents() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_str().expect("utf-8").to_owned();
    let as_yves = args(&["--author", "Yves"]);

    let saved = save(
        &root,
        "write",
        "guide/retry",
        None,
        Some("# Retry\n\nOnce, see [setup](/setup).\n\n## Limits\n\nThree.\n"),
        &as_yves,
    )
    .expect("write");
    let saved = doc("docs.saved", &saved);
    assert_eq!(saved["contract"], "local-read/1");
    assert_eq!(
        (saved["source"].clone(), saved["rev"].clone()),
        (json!("local"), json!(1))
    );
    save(
        &root,
        "write",
        "setup",
        None,
        Some("# Setup\n\nInstall.\n"),
        &[],
    )
    .expect("second page");
    // The link to /setup is stored by id, so it follows a move.
    save(
        &root,
        "write",
        "guide/retry",
        None,
        Some("# Retry\n\nOnce, see [setup](/setup).\n\n## Limits\n\nFour.\n"),
        &as_yves,
    )
    .expect("revise");
    let moved = save(&root, "move", "setup", Some("guide/setup"), None, &[]).expect("move");
    assert_eq!(moved.slug, "guide/setup");

    let list = doc("docs.list", &list_document(&root, None).expect("list"));
    assert_eq!(list["kind"], "docs.list");
    assert_eq!(list["source"], "local");
    assert_eq!(list["vault"], Value::Null);
    // The vault document keeps its object and names its source.
    let vault = doc(
        "docs.list",
        &super::super::local_read::docs::DocsList {
            project_root: root.clone(),
            source: "vault",
            vault: VaultRef {
                address: "acme/app".into(),
                linked: true,
            },
            pages: Vec::new(),
        },
    );
    assert_eq!(
        (vault["source"].clone(), vault["vault"]["address"].clone()),
        (json!("vault"), json!("acme/app"))
    );
    let ids: Vec<&str> = list["pages"]
        .as_array()
        .expect("pages")
        .iter()
        .map(|p| p["id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, ["guide/retry", "guide/setup"]);
    let retry = &list["pages"][0];
    for key in [
        "title",
        "page_kind",
        "status",
        "anchor_state",
        "sections",
        "notes",
        "anchors",
    ] {
        assert!(retry.get(key).is_some(), "vault row key {key}");
    }
    assert_eq!(
        retry["updated_by"],
        json!({ "kind": "person", "label": "Yves" })
    );
    assert_eq!(retry["shared"], Value::Null);

    let page = doc(
        "docs.page",
        &page_document(&root, "guide/retry", None).expect("show"),
    );
    assert_eq!(page["page"]["links"], json!(["guide/setup"]));
    assert_eq!(page["page"]["sections"][1]["heading"], "Limits");
    assert!(
        page["page"]["sections"][0]["text"]
            .as_str()
            .expect("text")
            .contains("(/guide/setup)")
    );
    let then = doc(
        "docs.page",
        &page_document(&root, "guide/retry", Some(1)).expect("show rev 1"),
    );
    assert_eq!(then["page"]["sections"][1]["text"], "## Limits\n\nThree.\n");

    save(&root, "delete", "guide/retry", None, None, &[]).expect("delete");
    assert!(matches!(
        page_document(&root, "guide/retry", None),
        Err(Failure {
            code: Code::NotFound,
            ..
        })
    ));
    let history = doc(
        "docs.history",
        &history_document(&root, "guide/retry").expect("history"),
    );
    assert_eq!(history["page"]["live"], false);
    let actions: Vec<(&str, &str)> = history["revisions"]
        .as_array()
        .expect("revisions")
        .iter()
        .map(|r| {
            (
                r["action"].as_str().expect("a"),
                r["author"]["label"].as_str().expect("l"),
            )
        })
        .collect();
    assert_eq!(
        actions,
        [("write", "Yves"), ("write", "Yves"), ("delete", "person")]
    );

    // A bad slug is the caller's error, not the store's.
    let bad = save(&root, "write", "Bad Slug", None, Some("# X\n"), &[]).expect_err("bad slug");
    assert_eq!(bad.code, Code::InvalidArgument);
}

/// Writes `body` as this machine's vault map (`<hive data>/projects.json`).
fn write_vault_map(body: &[u8]) {
    let store = lean_ctx_hive::HiveStore::open().expect("hive store");
    std::fs::create_dir_all(store.data_dir()).expect("hive dir");
    std::fs::write(
        store
            .data_dir()
            .join(lean_ctx::core::vault_map::VAULTS_FILE),
        body,
    )
    .expect("a vault map");
}

/// What `docs`, `hive docs` and `ctx_doc` answer for `root`, and whether
/// a local doc store was created for it.
fn refusals(root: &str) -> (Vec<(&'static str, Option<Code>)>, bool, String) {
    let mut out = Vec::new();
    for action in ["list", "show", "write"] {
        let answer = json_answer(
            root,
            action,
            Some("guide"),
            None,
            &args(&["--content", "# Guide\n"]),
            local_read::Via::Cli,
        );
        out.push((action, answer.failed));
    }
    for action in ["list", "show"] {
        let answer = local_read::docs::answer(
            &args(&[action, "guide", "--json", "--project-root", root]),
            local_read::Via::Socket,
        );
        out.push((action, answer.failed));
    }
    let tool = ctx_doc::handle(
        root,
        &ctx_doc::Args {
            action: "write",
            slug: Some("guide"),
            content: Some("# Guide\n"),
            ..ctx_doc::Args::default()
        },
    );
    let store = local_docs::store(root).expect("store");
    (out, store.dir().exists(), tool)
}

/// A vault map that cannot be read is neither "unlinked" nor the vault:
/// every docs answer is `unavailable`, ctx_doc refuses, and nothing is
/// written locally (a linked checkout's pages never go local silently).
#[test]
fn an_unreadable_link_answers_unavailable_and_writes_nothing_locally() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().expect("project");
    let root = dir.path().to_str().expect("utf-8").to_owned();
    write_vault_map(b"not json");
    assert!(local_docs::is_linked(&root).is_err());

    let (answers, created, tool) = refusals(&root);
    for (action, failed) in answers {
        assert_eq!(failed, Some(Code::Unavailable), "{action}");
    }
    assert!(tool.starts_with("ctx_doc: "), "{tool}");
    assert!(tool.contains("cannot be read"), "{tool}");
    assert!(
        !created,
        "no local doc store for a checkout whose link is unreadable"
    );
}

/// Signed out, a linked checkout stays linked: its reads are
/// `not_connected`, its writes are refused, and nothing is written locally
/// to be shared later.
#[test]
fn a_linked_checkout_signed_out_refuses_and_writes_nothing_locally() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().expect("project");
    let root = dir.path().to_str().expect("utf-8").to_owned();
    let map = json!({"projects": {"stable": {
        "project_id": "v_1", "ns": "yves", "name": "lean-ctx", "sealed": false, "roots": [root],
    }}});
    write_vault_map(&serde_json::to_vec(&map).expect("json"));
    assert_eq!(local_docs::is_linked(&root), Ok(true));

    let (answers, created, tool) = refusals(&root);
    assert_eq!(
        answers,
        [
            ("list", Some(Code::NotConnected)),
            ("show", Some(Code::NotConnected)),
            ("write", Some(Code::Forbidden)),
            ("list", Some(Code::NotConnected)),
            ("show", Some(Code::NotConnected)),
        ]
    );
    assert!(tool.starts_with("ctx_doc: "), "{tool}");
    assert!(
        !created,
        "a signed-out linked checkout writes no local pages"
    );
}
