// SPDX-License-Identifier: Apache-2.0
//! Test support for the delivered lineage of stored items, shared by the
//! stores that serve them, and the tests of `publish_served` itself.

use std::path::Path;

use super::{Lineage, SourceRef, StoredBinding, admit_and_bind, publish_served, serve_stored};
use crate::core::archive::delivered::{self, LineageRefusal};
use crate::core::policy::runtime::{REQUEST_PROJECT, TestPolicyOverride};

/// Global gateway governed, no project pack: protection is active.
pub(crate) fn governed() -> crate::core::data_dir::IsolatedDataDir {
    let data = crate::core::data_dir::isolated_data_dir();
    let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(
        dir.join("config.toml"),
        "[context_gateway]\nmode = \"governed\"\n",
    )
    .expect("config");
    data
}

pub(crate) fn project() -> (tempfile::TempDir, String) {
    let directory = tempfile::tempdir().expect("project");
    // Long names and no Windows verbatim prefix: callers join children with `/`.
    let root = crate::core::pathutil::safe_canonicalize(directory.path()).expect("canonical root");
    let root = root.to_str().expect("utf-8 root").to_owned();
    (directory, root)
}

pub(crate) fn scoped<T>(root: &str, operation: impl FnOnce() -> T) -> T {
    REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(Path::new(root).to_path_buf())),
        operation,
    )
}

/// Write `content` to `name` under `root` and bind it as the admitted reader
/// returns it for `tool`.
pub(crate) fn source(root: &str, name: &str, content: &str, tool: &str) -> SourceRef {
    let path = format!("{root}/{name}");
    std::fs::write(&path, content).expect("source");
    SourceRef::read(&path, root, tool)
        .expect("readable source")
        .1
}

/// A copy of `text` bound to `sources` (caller-supplied when there are none).
pub(crate) fn bound(text: &str, root: &str, sources: Vec<SourceRef>) -> (String, StoredBinding) {
    admit_and_bind(
        text,
        "test",
        Some(root),
        Lineage::of(sources).expect("bounded lineage"),
    )
    .expect("permitted content is stored")
}

/// Run `handler` as the delivery of a tool that declares every source: inside
/// the request project and the source capture, then the record every release
/// point makes. Returns the reply and the number of blocks recorded.
pub(crate) fn deliver(root: &str, handler: impl FnOnce() -> String) -> (String, usize) {
    scoped(root, || {
        let (reply, capture) = crate::core::archive::authority::capture_sync(handler);
        let recorded = delivered::record_tool_delivery(true, false, capture, &[reply.as_str()]);
        (reply, recorded)
    })
}

/// Whether `text` may be reused now: its recorded lineage under the current
/// rights of a request in `root`. No record is `Unknown`.
pub(crate) fn reusable(root: &str, text: &str) -> Result<(), LineageRefusal> {
    scoped(root, || {
        delivered::lineage_of(&delivered::digest(text))
            .ok_or(LineageRefusal::Unknown)?
            .still_admitted()
    })
}

/// The value of the fact [`two_facts`] binds to `auth.rs`.
pub(crate) const BOUND_FACT: &str = "Tokens are verified with RS256 in the auth module";
/// The value of the fact [`two_facts`] leaves without a binding.
pub(crate) const TYPED_FACT: &str = "Deployments are promoted by the release captain on Fridays";

/// Remember two facts of `category` in the knowledge store of `root`:
/// `token-check` ([`BOUND_FACT`]), bound to `auth.rs` as `ctx_knowledge
/// remember sources=[…]` binds it, and `release-duty` ([`TYPED_FACT`]) without
/// a binding. Deleting `auth.rs` ([`withdraw_source`]) withdraws the first.
pub(crate) fn two_facts(root: &str, category: &str) {
    bound_fact(root, category);
    remember(root, category, "release-duty", TYPED_FACT);
}

/// Remember one fact through `ctx_knowledge remember`, without a binding.
pub(crate) fn remember(root: &str, category: &str, key: &str, value: &str) {
    let reply = crate::tools::ctx_knowledge::handle(
        root,
        "remember",
        Some(category),
        Some(key),
        Some(value),
        None,
        "session-1",
        None,
        None,
        None,
        None,
        None,
    );
    assert!(reply.starts_with("Remembered"), "{reply}");
}

/// The first fact of [`two_facts`] alone: `token-check` ([`BOUND_FACT`]),
/// bound to `auth.rs`. A store that holds nothing else has no originless fact.
pub(crate) fn bound_fact(root: &str, category: &str) {
    use crate::core::archive::bindings::{self, Item};
    std::fs::write(format!("{root}/auth.rs"), "pub fn verify_rs256() {}\n").expect("source");
    remember(root, category, "token-check", BOUND_FACT);
    let item = Item::KnowledgeFact {
        project: root,
        category,
        key: "token-check",
    };
    let sources =
        bindings::sources_at(root, &["auth.rs"], "ctx_knowledge").expect("readable source");
    assert!(bindings::bind(&item, BOUND_FACT, root, sources));
}

/// Delete the file [`two_facts`] bound [`BOUND_FACT`] to.
pub(crate) fn withdraw_source(root: &str) {
    std::fs::remove_file(format!("{root}/auth.rs")).expect("delete");
}

/// The one publish function: a served source-bound item gives its delivery
/// the lineage of its files, which fails with any of them; a caller-supplied
/// item and an item without a binding leave the delivery unknown, also when a
/// source-bound item is delivered next to them.
#[test]
fn a_served_item_publishes_its_files_and_an_originless_one_nothing() {
    let _data = governed();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let a = source(&root, "a.rs", "pub fn alpha() {}\n", "ctx_read");
    let b = source(&root, "b.rs", "pub fn beta() {}\n", "ctx_read");
    let (text, binding) = bound("alpha and beta are exported", &root, vec![a, b]);
    let serve = |binding: Option<&StoredBinding>, text: &str| {
        let served = serve_stored(text, binding, Some(&root)).expect("served");
        publish_served(binding);
        served
    };

    let (reply, recorded) = deliver(&root, || format!("fact: {}", serve(Some(&binding), &text)));
    assert_eq!(recorded, 1, "a complete file lineage is recorded");
    assert_eq!(reusable(&root, &reply), Ok(()));
    assert_eq!(
        reusable(&root, &text),
        Err(LineageRefusal::Unknown),
        "the record is of the delivered block, not of the stored text"
    );

    // Typed by a caller: no source to re-check, so no lineage — alone, and
    // mixed with a source-bound item.
    let (typed, caller_supplied) = bound("a note typed by an agent", &root, Vec::new());
    let (reply, recorded) = deliver(&root, || serve(Some(&caller_supplied), &typed));
    assert_eq!(recorded, 0);
    assert_eq!(reusable(&root, &reply), Err(LineageRefusal::Unknown));
    let (mixed, recorded) = deliver(&root, || {
        format!(
            "{}\n{}",
            serve(Some(&binding), &text),
            serve(Some(&caller_supplied), &typed)
        )
    });
    assert_eq!(
        recorded, 0,
        "one originless item makes the whole reply unknown"
    );
    assert_eq!(reusable(&root, &mixed), Err(LineageRefusal::Unknown));
    // No binding at all publishes "unknown" as well.
    let (_, recorded) = deliver(&root, || {
        publish_served(None);
        "a record without a binding".to_owned()
    });
    assert_eq!(recorded, 0);

    // One of two sources is reclassified, then deleted: the block that was
    // delivered from both is refused although the other still holds.
    std::fs::write(
        format!("{root}/a.rs"),
        "*** TOP SECRET ***\npub fn alpha() {}\n",
    )
    .expect("mark");
    let reply = format!("fact: {text}");
    assert_eq!(
        reusable(&root, &reply),
        Err(LineageRefusal::SourceNoLongerAdmitted)
    );
    std::fs::remove_file(format!("{root}/a.rs")).expect("delete");
    assert_eq!(
        reusable(&root, &reply),
        Err(LineageRefusal::SourceNoLongerAdmitted)
    );
}

/// Without protection nothing is captured, exactly as for a file read.
#[test]
fn without_protection_nothing_is_published() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let (reply, recorded) = deliver(&root, || {
        publish_served(None);
        "developer mode".to_owned()
    });
    assert_eq!(recorded, 0);
    assert_eq!(reusable(&root, &reply), Err(LineageRefusal::Unknown));
}
