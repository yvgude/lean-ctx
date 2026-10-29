// SPDX-License-Identifier: Apache-2.0
//! The lineage of a ledger's findings and facts: joined at creation, held as
//! a whole at every use, marked by the binding's origin.

use std::path::{Path, PathBuf};

use super::tests::{make_minimal_ledger, show, write_bound, write_marked, write_recorded};
use super::{
    BoundEntry, CreateLedgerInput, CuratedRef, HandoffLedgerV1, KnowledgeExcerpt,
    KnowledgeFactMini, LedgerAuthority, LedgerPart, ORIGIN_ALL_BOUND, ORIGIN_ORIGINLESS_PART,
    ORIGIN_UNCHECKED, PartKind, SessionExcerpt, authority_path, create_ledger, list_ledgers,
    load_ledger_bound,
};
use crate::core::archive::delivered::LineageRefusal;
use crate::core::context_admission::stored::StoredBinding;
use crate::core::context_admission::stored::lineage_support::{
    BOUND_FACT, TYPED_FACT, bound_fact, deliver, governed, project, remember, reusable, scoped,
    source, two_facts,
};
use crate::core::policy::runtime::TestPolicyOverride;
use crate::core::session::SessionState;

const POOL: &str = "pub fn lease() {}\n";
const AUTH: &str = "pub fn verify_rs256() {}\n";
const FINDING: &str = "the pool leaks one lease";
const TYPED_FINDING: &str = "noted without a file";

/// A session of `root` with one finding bound to `pool.rs`.
fn bound_session(root: &str) -> SessionState {
    std::fs::write(format!("{root}/pool.rs"), POOL).expect("source");
    let mut session = SessionState::new();
    session.project_root = Some(root.to_owned());
    session.add_finding_for_file(Some("pool.rs"), Some(1), FINDING);
    session
}

fn create(root: &str, session: &SessionState) -> Result<(HandoffLedgerV1, PathBuf), String> {
    scoped(root, || {
        create_ledger(CreateLedgerInput {
            agent_id: None,
            client_name: None,
            project_root: Some(root.to_owned()),
            session: session.clone(),
            tool_calls: Vec::new(),
            workflow: None,
            curated_refs: Vec::new(),
            request_id: None,
        })
    })
}

/// `ctx_handoff show` as a delivery: the reply and the blocks registered.
fn shown(root: &str, path: &Path) -> (String, usize) {
    deliver(root, || show(path))
}

fn binding_of(path: &Path) -> StoredBinding {
    let authority = std::fs::read_to_string(authority_path(path)).expect("sidecar");
    serde_json::from_str(&authority).expect("binding")
}

/// A ledger without a session snapshot, as only a hand-built one is.
fn snapshot_free(root: &str, name: &str, findings: &[&str]) -> HandoffLedgerV1 {
    HandoffLedgerV1 {
        created_at: name.to_owned(),
        project_root: Some(root.to_owned()),
        session_snapshot: String::new(),
        session: SessionExcerpt {
            findings: findings
                .iter()
                .map(|finding| (*finding).to_owned())
                .collect(),
            ..SessionExcerpt::default()
        },
        ..make_minimal_ledger()
    }
}

/// A ledger created with a bound finding and a bound fact carries their
/// sources: it is served while both hold and withheld as a whole — typed, no
/// content — when one is deleted, changed or reclassified; with the same
/// bytes back it is served again.
#[test]
fn a_ledger_is_withheld_while_a_source_of_a_bound_part_does_not_admit() {
    let _data = governed();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let session = bound_session(&root);
    bound_fact(&root, "architecture");
    let (ledger, path) = create(&root, &session).expect("create ledger");
    assert_eq!(ledger.session.findings, [FINDING]);
    assert_eq!(ledger.knowledge.facts.len(), 1);
    let binding = binding_of(&path);
    // Both parts are bound, but a created ledger carries a session snapshot:
    // the marker describes the stored ledger as a whole.
    assert!(!ledger.session_snapshot.is_empty());
    assert_eq!(binding.origin(), ORIGIN_ORIGINLESS_PART);
    assert_eq!(
        binding.file_sources().map(|(_, files)| files.len()),
        Some(2),
        "the finding's file and the fact's file are the ledger's lineage"
    );

    let served = |reply: &str| reply.contains(FINDING) && reply.contains(BOUND_FACT);
    let withheld = |reply: &str, code: &str| {
        assert!(reply.contains(code), "{reply}");
        assert!(
            !reply.contains(FINDING) && !reply.contains("RS256"),
            "{reply}"
        );
    };
    assert!(served(&shown(&root, &path).0));

    // The finding's source is deleted, comes back with the same bytes, then
    // comes back changed.
    std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
    let (refusal, recorded) = shown(&root, &path);
    withheld(&refusal, "store.source_unavailable");
    assert_eq!(recorded, 0);
    std::fs::write(format!("{root}/pool.rs"), POOL).expect("same bytes");
    assert!(served(&shown(&root, &path).0));
    std::fs::write(format!("{root}/pool.rs"), "pub fn lease() -> u8 { 1 }\n").expect("changed");
    withheld(&shown(&root, &path).0, "store.source_changed");
    std::fs::write(format!("{root}/pool.rs"), POOL).expect("same bytes");
    assert!(served(&shown(&root, &path).0));

    // The fact's source is reclassified.
    std::fs::write(
        format!("{root}/auth.rs"),
        format!("*** TOP SECRET ***\n{AUTH}"),
    )
    .expect("reclassify");
    withheld(&shown(&root, &path).0, "store.");
    std::fs::write(format!("{root}/auth.rs"), AUTH).expect("same bytes");
    assert!(served(&shown(&root, &path).0));
}

/// A new ledger with a finding and a fact that have no binding says so in
/// its marker. It is served under the originless rule — project authority
/// and content rules — and its delivery is never registered; its bound parts
/// still withhold it when their source goes.
#[test]
fn a_new_ledger_with_an_originless_part_is_served_and_never_registered() {
    let _data = governed();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let mut session = bound_session(&root);
    session.add_finding_for_file(None, None, TYPED_FINDING);
    two_facts(&root, "architecture");
    let (_, path) = create(&root, &session).expect("create ledger");
    assert_eq!(binding_of(&path).origin(), ORIGIN_ORIGINLESS_PART);
    let (reply, recorded) = shown(&root, &path);
    for text in [FINDING, TYPED_FINDING, BOUND_FACT, TYPED_FACT] {
        assert!(reply.contains(text), "{text}: {reply}");
    }
    assert_eq!(recorded, 0);
    assert_eq!(reusable(&root, &reply), Err(LineageRefusal::Unknown));

    // Also without any session text: the marker alone keeps it unregistered.
    let pool = source(&root, "pool.rs", POOL, "ctx_session");
    let free = snapshot_free(&root, "originless-part", &[FINDING, TYPED_FINDING]);
    let first_bound = |_: PartKind, index: usize| index == 0;
    let free_path = write_recorded(
        &free,
        &root,
        vec![pool],
        ORIGIN_ORIGINLESS_PART,
        first_bound,
    );
    let (free_reply, recorded) = shown(&root, &free_path);
    assert!(free_reply.contains(TYPED_FINDING), "{free_reply}");
    assert_eq!(recorded, 0);
    assert_eq!(reusable(&root, &free_reply), Err(LineageRefusal::Unknown));

    std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
    for ledger in [&path, &free_path] {
        let (refusal, _) = shown(&root, ledger);
        assert!(refusal.contains("store.source_unavailable"), "{refusal}");
        assert!(!refusal.contains(TYPED_FINDING), "{refusal}");
    }
}

/// A ledger whose findings and facts were all bound publishes the joined
/// set: its delivery is registered under the files of the excerpt, the
/// finding and the fact, and fails with any one of them. A ledger of curated
/// excerpts alone still publishes its files, whatever its marker.
#[test]
fn an_all_bound_ledger_publishes_the_joined_set() {
    let _data = governed();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let sources = || {
        vec![
            source(&root, "lib.rs", "pub fn alpha() {}\n", "ctx_handoff"),
            source(&root, "pool.rs", POOL, "ctx_session"),
            source(&root, "auth.rs", AUTH, "ctx_knowledge"),
        ]
    };
    let excerpt = CuratedRef {
        path: format!("{root}/lib.rs"),
        mode: "signatures".to_owned(),
        content_md5: "digest".to_owned(),
        content: "fn alpha()".to_owned(),
    };
    let all_bound = HandoffLedgerV1 {
        knowledge: KnowledgeExcerpt {
            project_hash: None,
            facts: vec![KnowledgeFactMini {
                category: "architecture".to_owned(),
                key: "token-check".to_owned(),
                value: BOUND_FACT.to_owned(),
                confidence: 0.9,
            }],
        },
        curated_refs: vec![excerpt.clone()],
        ..snapshot_free(&root, "all-bound", &[FINDING])
    };
    let path = write_bound(&all_bound, &root, sources());
    let (reply, recorded) = shown(&root, &path);
    assert!(
        reply.contains(FINDING) && reply.contains(BOUND_FACT),
        "{reply}"
    );
    assert_eq!(recorded, 1);
    assert_eq!(reusable(&root, &reply), Ok(()));

    for marker in [ORIGIN_UNCHECKED, ORIGIN_ALL_BOUND] {
        let curated_only = HandoffLedgerV1 {
            curated_refs: vec![excerpt.clone()],
            ..snapshot_free(&root, &format!("curated-{}", marker.len()), &[])
        };
        // Only the excerpt's own source: nothing else was bound here.
        let lib = sources()[..1].to_vec();
        let curated_path = write_marked(&curated_only, &root, lib, marker);
        let (curated_reply, recorded) = shown(&root, &curated_path);
        assert!(curated_reply.contains("fn alpha()"), "{curated_reply}");
        assert_eq!(recorded, 1, "{marker}");
    }

    // One file of the joined set goes — the fact's, not the excerpt's.
    std::fs::remove_file(format!("{root}/auth.rs")).expect("delete");
    assert_eq!(
        reusable(&root, &reply),
        Err(LineageRefusal::SourceNoLongerAdmitted)
    );
    let (refusal, recorded) = shown(&root, &path);
    assert!(
        !refusal.contains(FINDING) && !refusal.contains("alpha"),
        "{refusal}"
    );
    assert_eq!(recorded, 0);
}

/// A ledger whose sidecar carries the marker of ledgers written before
/// findings and facts were checked: its findings were never bound, so under
/// protection it is withheld, whatever its curated sources say. The same
/// content under a marker of this version is not caught by that rule, and an
/// unchecked ledger without findings and facts is served as before.
#[test]
fn an_unchecked_sidecar_ledger_with_findings_is_withheld_under_protection() {
    let _data = governed();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let lib = || source(&root, "lib.rs", "pub fn alpha() {}\n", "ctx_handoff");
    let with_findings = |name: &str| HandoffLedgerV1 {
        session_snapshot: "snapshot".to_owned(),
        ..snapshot_free(&root, name, &["an unchecked finding"])
    };

    let legacy = write_marked(
        &with_findings("legacy"),
        &root,
        vec![lib()],
        ORIGIN_UNCHECKED,
    );
    let (refusal, recorded) = shown(&root, &legacy);
    assert!(refusal.contains("store.no_source_authority"), "{refusal}");
    assert!(!refusal.contains("unchecked finding"), "{refusal}");
    assert_eq!(recorded, 0);

    let unknown = write_marked(
        &with_findings("unknown"),
        &root,
        vec![lib()],
        "handoff:other",
    );
    assert!(
        shown(&root, &unknown)
            .0
            .contains("store.no_source_authority")
    );

    let checked = with_findings("checked");
    let checked = write_marked(&checked, &root, vec![lib()], ORIGIN_ORIGINLESS_PART);
    assert!(shown(&root, &checked).0.contains("an unchecked finding"));

    let plain = HandoffLedgerV1 {
        session_snapshot: "snapshot".to_owned(),
        ..snapshot_free(&root, "legacy-plain", &[])
    };
    let plain = write_marked(&plain, &root, vec![lib()], ORIGIN_UNCHECKED);
    assert!(shown(&root, &plain).0.contains("snapshot"));
}

/// Developer mode is unchanged: the unchecked ledger with findings is served,
/// and a ledger created there is marked unchecked.
#[test]
fn an_unchecked_sidecar_ledger_with_findings_is_served_in_developer_mode() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let ledger = snapshot_free(&root, "legacy", &["an unchecked finding"]);
    let path = write_marked(&ledger, &root, Vec::new(), ORIGIN_UNCHECKED);
    let (reply, recorded) = shown(&root, &path);
    assert!(reply.contains("an unchecked finding"), "{reply}");
    assert_eq!(recorded, 0);

    let session = bound_session(&root);
    let (created, created_path) = create(&root, &session).expect("create ledger");
    assert_eq!(created.session.findings, [FINDING]);
    assert_eq!(binding_of(&created_path).origin(), ORIGIN_UNCHECKED);
    std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
    assert!(shown(&root, &created_path).0.contains(FINDING));
}

/// The joined set is bounded like every lineage: more sources than a lineage
/// may name refuse the ledger — typed, content-free, nothing written — and
/// are never cut down to the first ones.
#[test]
fn a_joined_set_above_the_lineage_bound_refuses_the_ledger() {
    use crate::core::archive::authority::MAX_SOURCES;
    use crate::core::archive::bindings::{self, Item};
    let _data = governed();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let per_fact = MAX_SOURCES / 2 + 1;
    for (key, value) in [("token-check", BOUND_FACT), ("release-duty", TYPED_FACT)] {
        remember(&root, "architecture", key, value);
        let stem = key.len();
        let names: Vec<String> = (0..per_fact)
            .map(|n| format!("part{stem}_{n}.rs"))
            .collect();
        for name in &names {
            std::fs::write(format!("{root}/{name}"), "pub fn part() {}\n").expect("source");
        }
        let item = Item::KnowledgeFact {
            project: &root,
            category: "architecture",
            key,
        };
        let sources = bindings::sources_at(&root, &names, "ctx_knowledge").expect("readable");
        assert!(bindings::bind(&item, value, &root, sources));
    }
    let mut session = SessionState::new();
    session.project_root = Some(root.clone());

    let refusal = create(&root, &session).expect_err("a lineage is never truncated");
    assert!(refusal.contains("store.lineage_unbounded"), "{refusal}");
    assert!(
        !refusal.contains("RS256") && !refusal.contains("captain"),
        "{refusal}"
    );
    assert!(list_ledgers(&root).is_empty(), "nothing was written");
}

/// A mixed ledger records WHICH of its parts were bound: the sidecar keeps
/// their digests beside the binding, no content. The lineage it hands on
/// binds exactly those parts; an unbound part, a text that is not the bound
/// text and the same value under another key get none. A ledger whose text
/// was edited is refused as a whole, and a sidecar that is one serialized
/// binding has no bound parts.
#[test]
fn a_mixed_ledger_records_which_parts_were_bound() {
    use crate::core::archive::bindings::Item;
    let _data = governed();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let mut session = bound_session(&root);
    session.add_finding_for_file(None, None, TYPED_FINDING);
    two_facts(&root, "architecture");
    let (_, path) = create(&root, &session).expect("create ledger");

    let sidecar = std::fs::read_to_string(authority_path(&path)).expect("sidecar");
    let authority: LedgerAuthority = serde_json::from_str(&sidecar).expect("authority");
    assert_eq!(authority.binding.origin(), ORIGIN_ORIGINLESS_PART);
    let record = authority
        .bound_parts
        .expect("always written under protection");
    assert_eq!((record.count, record.entries.len()), (2, 2));
    assert_eq!(
        record.entries[0],
        BoundEntry {
            kind: PartKind::Finding,
            index: 0,
            digest: LedgerPart::Finding(FINDING).digest(),
        }
    );
    let token_check = LedgerPart::Fact {
        category: "architecture",
        key: "token-check",
        value: BOUND_FACT,
    };
    assert_eq!(record.entries[1].kind, PartKind::Fact);
    assert_eq!(record.entries[1].digest, token_check.digest());
    assert!(
        !sidecar.contains(FINDING) && !sidecar.contains("RS256"),
        "{sidecar}"
    );

    // Consistent: served, and the lineage binds exactly the recorded places.
    assert!(shown(&root, &path).0.contains(TYPED_FINDING));
    let (_, lineage) = scoped(&root, || load_ledger_bound(&path)).expect("served");
    let lineage = lineage.expect("a mixed ledger hands its lineage on");
    let item = Item::SessionFinding {
        session: "reader",
        file: None,
    };
    let bound = |part: LedgerPart<'_>, index: usize| {
        scoped(&root, || lineage.carry(&part, index, &item)).map(|carried| carried.is_some())
    };
    assert_eq!(bound(LedgerPart::Finding(FINDING), 0), Ok(true));
    assert_eq!(bound(token_check, record.entries[1].index), Ok(true));
    assert_eq!(bound(LedgerPart::Finding(TYPED_FINDING), 1), Ok(false));
    assert_eq!(
        bound(LedgerPart::Finding(FINDING), 1),
        Ok(false),
        "not at this place"
    );
    let other_text = bound(LedgerPart::Finding("the pool leaks two leases"), 0);
    assert!(
        other_text.is_err(),
        "a recorded place with another text is refused"
    );

    // Every disagreement between the stored ledger and its record withholds
    // the whole ledger with the one typed, content-free reason.
    let ledger_json = std::fs::read_to_string(&path).expect("ledger");
    let refused = |what: &str| {
        let (refusal, recorded) = shown(&root, &path);
        assert!(
            refusal.contains("store.ledger_authority_inconsistent"),
            "{what}: {refusal}"
        );
        for text in [FINDING, TYPED_FINDING, "two leases", "RS256"] {
            assert!(!refusal.contains(text), "{what}: {refusal}");
        }
        assert_eq!(recorded, 0, "{what}");
        std::fs::write(&path, &ledger_json).expect("restore the ledger");
        std::fs::write(authority_path(&path), &sidecar).expect("restore the sidecar");
        assert!(
            shown(&root, &path).0.contains(FINDING),
            "{what}: consistent again"
        );
    };

    std::fs::write(
        &path,
        ledger_json.replace(FINDING, "the pool leaks two leases"),
    )
    .expect("edit");
    refused("the text of a bound element was edited");

    let mut swapped: serde_json::Value = serde_json::from_str(&ledger_json).expect("json");
    let findings = swapped["session"]["findings"]
        .as_array_mut()
        .expect("findings");
    findings.swap(0, 1);
    std::fs::write(&path, serde_json::to_string_pretty(&swapped).expect("json")).expect("swap");
    refused("two entries were swapped in the stored list");

    let mut stripped: serde_json::Value = serde_json::from_str(&sidecar).expect("json");
    stripped
        .as_object_mut()
        .expect("object")
        .remove("bound_parts");
    std::fs::write(authority_path(&path), stripped.to_string()).expect("strip");
    refused("the bound record was removed");

    let mut truncated: serde_json::Value = serde_json::from_str(&sidecar).expect("json");
    let entries = truncated["bound_parts"]["entries"]
        .as_array_mut()
        .expect("entries");
    entries.pop();
    std::fs::write(authority_path(&path), truncated.to_string()).expect("truncate");
    refused("an entry is missing from the record");
}

/// Two findings with the same summary, one bound and one not, are two
/// elements: only the place that was bound is recorded, and only that place
/// hands a binding on. The text alone identifies nothing.
#[test]
fn elements_with_the_same_text_are_told_apart_by_their_place() {
    use crate::core::archive::bindings::Item;
    let _data = governed();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let mut session = bound_session(&root);
    session.add_finding_for_file(None, None, FINDING);
    let (ledger, path) = create(&root, &session).expect("create ledger");
    assert_eq!(ledger.session.findings, [FINDING, FINDING]);

    let sidecar = std::fs::read_to_string(authority_path(&path)).expect("sidecar");
    let authority: LedgerAuthority = serde_json::from_str(&sidecar).expect("authority");
    assert_eq!(authority.binding.origin(), ORIGIN_ORIGINLESS_PART);
    let record = authority.bound_parts.expect("record");
    assert_eq!(record.count, 1);
    assert_eq!(
        (record.entries[0].kind, record.entries[0].index),
        (PartKind::Finding, 0)
    );

    let (_, lineage) = scoped(&root, || load_ledger_bound(&path)).expect("served");
    let lineage = lineage.expect("lineage");
    let item = Item::SessionFinding {
        session: "reader",
        file: None,
    };
    let bound = |index: usize| {
        let part = LedgerPart::Finding(FINDING);
        scoped(&root, || lineage.carry(&part, index, &item)).map(|carried| carried.is_some())
    };
    assert_eq!(bound(0), Ok(true), "the bound element");
    assert_eq!(
        bound(1),
        Ok(false),
        "the same text, never bound: no upgrade"
    );
}

/// A session with more findings than a ledger keeps: the ledger is cut to
/// the last ones BEFORE any is asked about, so the record, the marker, the
/// joined sources and the snapshot all describe the stored list.
/// - oldest unbound, the kept ones bound: created and served; all stored
///   findings are recorded and hand their binding on; the cut finding is in
///   no part of the ledger;
/// - oldest bound, the kept ones unbound: created and served with an empty
///   record, and the cut finding's source is not in the lineage.
#[test]
fn the_record_and_the_marker_describe_the_stored_window() {
    use crate::core::archive::bindings::Item;
    let _data = governed();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    std::fs::write(format!("{root}/pool.rs"), POOL).expect("source");
    let kept = super::MAX_LEDGER_FINDINGS;

    let mut bound_window = SessionState::new();
    bound_window.project_root = Some(root.clone());
    for n in 0..kept {
        bound_window.add_finding_for_file(Some("pool.rs"), None, &format!("lease path {n} leaks"));
    }
    let mut oldest = bound_window.findings[0].clone();
    oldest.file = None;
    oldest.summary = "an older note without a file".to_owned();
    bound_window.findings.insert(0, oldest);
    assert_eq!(bound_window.findings.len(), kept + 1);

    let (ledger, path) = create(&root, &bound_window).expect("a correct ledger is created");
    assert_eq!(ledger.session.findings.len(), kept);
    let stored = serde_json::to_string(&ledger).expect("json");
    assert!(
        !stored.contains("an older note"),
        "cut from list and snapshot: {stored}"
    );
    let sidecar = std::fs::read_to_string(authority_path(&path)).expect("sidecar");
    let authority: LedgerAuthority = serde_json::from_str(&sidecar).expect("authority");
    assert_eq!(authority.bound_parts.map(|record| record.count), Some(kept));
    assert!(
        shown(&root, &path).0.contains("lease path 0 leaks"),
        "served"
    );
    let (_, lineage) = scoped(&root, || load_ledger_bound(&path)).expect("served");
    let lineage = lineage.expect("lineage");
    let item = Item::SessionFinding {
        session: "reader",
        file: None,
    };
    for (index, summary) in ledger.session.findings.iter().enumerate() {
        let part = LedgerPart::Finding(summary);
        let carried = scoped(&root, || lineage.carry(&part, index, &item));
        assert_eq!(
            carried.map(|carried| carried.is_some()),
            Ok(true),
            "{index}"
        );
    }

    let mut unbound_window = SessionState::new();
    unbound_window.project_root = Some(root.clone());
    unbound_window.add_finding_for_file(Some("pool.rs"), None, "the oldest finding is bound");
    for n in 1..kept {
        unbound_window.add_finding_for_file(None, None, &format!("typed note {n}"));
    }
    let mut newest = unbound_window.findings[1].clone();
    newest.summary = "the newest typed note".to_owned();
    unbound_window.findings.push(newest);
    assert_eq!(unbound_window.findings.len(), kept + 1);

    let (ledger, path) = create(&root, &unbound_window).expect("create ledger");
    assert_eq!(ledger.session.findings.len(), kept);
    let stored = serde_json::to_string(&ledger).expect("json");
    assert!(!stored.contains("the oldest finding is bound"), "{stored}");
    let sidecar = std::fs::read_to_string(authority_path(&path)).expect("sidecar");
    let authority: LedgerAuthority = serde_json::from_str(&sidecar).expect("authority");
    assert!(
        authority.binding.file_sources().is_none(),
        "no source of a cut finding"
    );
    assert_eq!(authority.bound_parts.map(|record| record.count), Some(0));
    assert!(
        shown(&root, &path).0.contains("the newest typed note"),
        "served"
    );
    std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
    assert!(
        shown(&root, &path).0.contains("the newest typed note"),
        "not bound to it"
    );
}

/// A ledger created under protection without any bound element still has
/// its record, with count 0: it is served, and it hands no lineage on —
/// every item restored from it is originless.
#[test]
fn a_ledger_without_bound_parts_records_that_and_is_served() {
    let _data = governed();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let mut session = SessionState::new();
    session.project_root = Some(root.clone());
    session.add_finding_for_file(None, None, TYPED_FINDING);
    remember(&root, "architecture", "release-duty", TYPED_FACT);
    let (_, path) = create(&root, &session).expect("create ledger");

    let sidecar = std::fs::read_to_string(authority_path(&path)).expect("sidecar");
    let authority: LedgerAuthority = serde_json::from_str(&sidecar).expect("authority");
    let record = authority
        .bound_parts
        .expect("written also when nothing was bound");
    assert_eq!((record.count, record.entries.len()), (0, 0));
    let (reply, recorded) = shown(&root, &path);
    assert!(
        reply.contains(TYPED_FINDING) && reply.contains(TYPED_FACT),
        "{reply}"
    );
    assert_eq!(recorded, 0);
    let (_, lineage) = scoped(&root, || load_ledger_bound(&path)).expect("served");
    assert!(lineage.is_none());
}

/// `ctx_handoff merge` under protection: two agents' ledgers become one that
/// is a byte-for-byte subset of both, carries each part's binding from the
/// input it came from, is the same ledger in any argument order, and is
/// withheld — like its inputs — while a source of a bound part is gone.
#[test]
fn merged_ledgers_keep_their_parts_bindings_and_merge_deterministically() {
    let _data = governed();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let (first, first_path) = create(&root, &bound_session(&root)).expect("first ledger");
    bound_fact(&root, "architecture");
    let mut second_session = SessionState::new();
    second_session.project_root = Some(root.clone());
    second_session.add_finding_for_file(None, None, TYPED_FINDING);
    second_session.add_finding_for_file(Some("pool.rs"), Some(1), FINDING);
    let (second, second_path) = create(&root, &second_session).expect("second ledger");

    let merge = |paths: &[&PathBuf]| {
        let paths: Vec<PathBuf> = paths.iter().map(|path| (*path).clone()).collect();
        scoped(&root, || super::merge_ledgers(&paths))
    };
    let merged = merge(&[&second_path, &first_path]).expect("merged");
    let again = merge(&[&first_path, &second_path, &first_path]).expect("merged again");
    assert_eq!(merged.ledger.content_md5, again.ledger.content_md5);
    assert_eq!(merged.path, again.path);

    let ledger = &merged.ledger;
    let mut inputs = vec![
        super::ledger_content_md5(&first),
        super::ledger_content_md5(&second),
    ];
    if first.created_at > second.created_at {
        inputs.reverse();
    }
    assert_eq!(ledger.merged_from, inputs);
    // Each finding once; every part is one of an input's.
    assert_eq!(ledger.session.findings, [FINDING, TYPED_FINDING]);
    assert_eq!(ledger.knowledge.facts.len(), 1);
    assert_eq!(ledger.knowledge.facts[0].value, BOUND_FACT);
    assert!(ledger.session_snapshot.is_empty());

    // Bound where an input recorded it bound, originless where none did.
    let (_, lineage) = scoped(&root, || load_ledger_bound(&merged.path)).expect("served");
    let bound: Vec<(PartKind, usize)> = lineage
        .expect("bound parts")
        .bound
        .iter()
        .map(|entry| (entry.kind, entry.index))
        .collect();
    assert_eq!(bound, [(PartKind::Finding, 0), (PartKind::Fact, 0)]);
    assert_eq!(binding_of(&merged.path).origin(), ORIGIN_ORIGINLESS_PART);
    assert!(shown(&root, &merged.path).0.contains(FINDING));

    // A bound part's source is gone: the merged ledger is withheld, and so
    // is a new merge — typed, without the text of any input.
    std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
    let (refusal, recorded) = shown(&root, &merged.path);
    assert!(refusal.contains("store.source_unavailable"), "{refusal}");
    assert!(!refusal.contains(FINDING), "{refusal}");
    assert_eq!(recorded, 0);
    let refused = merge(&[&first_path, &second_path]).expect_err("input withheld");
    assert!(refused.contains("store.source_unavailable"), "{refused}");
    assert!(!refused.contains(FINDING), "{refused}");
}

/// A fact key with different values in two ledgers keeps both values and
/// is reported; an excerpt is carried only with its source; a merge needs
/// two different ledgers of one project.
#[test]
fn a_merge_keeps_conflicting_facts_and_only_excerpts_with_their_source() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let fact = |value: &str, confidence: f32| KnowledgeFactMini {
        category: "architecture".to_owned(),
        key: "auth".to_owned(),
        value: value.to_owned(),
        confidence,
    };
    let ledger = |name: &str, facts: Vec<KnowledgeFactMini>, root: &str| {
        let mut ledger = HandoffLedgerV1 {
            created_at: name.to_owned(),
            project_root: Some(root.to_owned()),
            knowledge: KnowledgeExcerpt {
                project_hash: None,
                facts,
            },
            ..make_minimal_ledger()
        };
        ledger.content_md5 = super::ledger_content_md5(&ledger);
        ledger
    };
    // An excerpt travels only with its file's source: `lib.rs` is bound in
    // `b`; `notes.rs` was kept unbound by a Community-mode `a` and is left
    // out — a merged ledger has no snapshot that would mark it unbound.
    let excerpt = |name: &str, content: &str| CuratedRef {
        path: format!("{root}/{name}"),
        mode: "signatures".to_owned(),
        content_md5: crate::core::hasher::hash_hex(content.as_bytes()),
        content: content.to_owned(),
    };
    let lib = source(&root, "lib.rs", "pub fn alpha() {}\n", "ctx_handoff");
    let mut first = ledger("a", vec![fact("RS256", 0.6), fact("same", 0.2)], &root);
    first.curated_refs = vec![excerpt("notes.rs", "fn notes() {}")];
    first.content_md5 = super::ledger_content_md5(&first);
    let mut second = ledger("b", vec![fact("HS256", 0.9), fact("same", 0.7)], &root);
    second.curated_refs = vec![excerpt("lib.rs", "pub fn alpha() {}\n")];
    second.content_md5 = super::ledger_content_md5(&second);
    let a = write_marked(&first, &root, Vec::new(), ORIGIN_UNCHECKED);
    let b = write_marked(&second, &root, vec![lib], ORIGIN_UNCHECKED);
    let merged = scoped(&root, || super::merge_ledgers(&[a.clone(), b.clone()])).expect("merged");
    let kept: Vec<&str> = merged
        .ledger
        .curated_refs
        .iter()
        .map(|excerpt| excerpt.path.as_str())
        .collect();
    assert_eq!(kept, [format!("{root}/lib.rs")]);
    assert_eq!(merged.dropped.unbound_curated, 1);
    assert_eq!(
        binding_of(&merged.path)
            .file_sources()
            .map(|(_, files)| files.len()),
        Some(1)
    );
    let values: Vec<(&str, f32)> = merged
        .ledger
        .knowledge
        .facts
        .iter()
        .map(|fact| (fact.value.as_str(), fact.confidence))
        .collect();
    assert_eq!(values, [("RS256", 0.6), ("same", 0.7), ("HS256", 0.9)]);
    assert_eq!(
        merged.conflicts,
        [super::merge::FactConflict {
            category: "architecture".to_owned(),
            key: "auth".to_owned(),
            values: 3,
        }]
    );

    let alone = scoped(&root, || super::merge_ledgers(&[a.clone(), a.clone()]));
    assert!(alone.is_err());
    let (_other_directory, other_root) = project();
    let elsewhere = write_marked(
        &ledger("c", Vec::new(), &other_root),
        &other_root,
        Vec::new(),
        ORIGIN_UNCHECKED,
    );
    // A ledger of another project is not served to this reader at all.
    assert!(scoped(&root, || super::merge_ledgers(&[a, elsewhere])).is_err());
}
