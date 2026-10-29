// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::core::data_dir::test_env_lock;
use tempfile::tempdir;

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        crate::test_env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(ref previous) = self.previous {
            crate::test_env::set_var(self.key, previous);
        } else {
            crate::test_env::remove_var(self.key);
        }
    }
}

fn sample_session() -> SessionState {
    let mut session = SessionState::new();
    session.set_task("stabilize handoff ledger tests", None);
    session.add_decision("Keep ledger deterministic", None);
    session.add_finding(
        Some("rust/src/core/handoff_ledger.rs"),
        Some(42),
        "Edge case coverage extended",
    );
    session.record_manual_evidence("tool:ctx_read", Some("handoff_ledger.rs"));
    session
}

fn sample_tool_calls() -> Vec<ToolCallRecord> {
    vec![
        ToolCallRecord {
            tool: "ctx_read".to_string(),
            original_tokens: 900,
            saved_tokens: 700,
            mode: Some("map".to_string()),
            duration_ms: 12,
            timestamp: chrono::Utc::now().to_rfc3339(),
        },
        ToolCallRecord {
            tool: "ctx_shell".to_string(),
            original_tokens: 120,
            saved_tokens: 60,
            mode: None,
            duration_ms: 8,
            timestamp: chrono::Utc::now().to_rfc3339(),
        },
    ]
}

/// A ledger is bound to the sources of its curated excerpts. Before, a
/// ledger was written without admission and `show`/`pull` read it back
/// plainly, also after the excerpt's source was gone.
#[test]
fn ledger_is_withheld_once_the_source_of_a_curated_excerpt_is_gone() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let project = tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical root");
    let source = root.join("lib.rs");
    std::fs::write(&source, "pub fn alpha() {}\n").expect("source");
    let input = CreateLedgerInput {
        agent_id: Some("agent-1".to_string()),
        client_name: Some("cursor".to_string()),
        project_root: Some(root.to_string_lossy().to_string()),
        session: sample_session(),
        tool_calls: sample_tool_calls(),
        workflow: None,
        curated_refs: vec![(
            source.to_string_lossy().to_string(),
            "fn alpha()".to_string(),
        )],
        request_id: None,
    };
    let (ledger, path) = create_ledger(input).expect("create ledger");
    assert_eq!(ledger.curated_refs.len(), 1);
    let in_root = || {
        crate::core::policy::runtime::REQUEST_PROJECT
            .sync_scope(std::cell::RefCell::new(Some(root.clone())), || {
                load_ledger(&path)
            })
    };
    let loaded = in_root().expect("an authorized ledger loads unchanged");
    assert_eq!(loaded.content_md5, ledger.content_md5);

    std::fs::remove_file(&source).expect("delete the source");
    let refusal = in_root().expect_err("old code returned the ledger");
    assert!(refusal.contains("store.source_unavailable"), "{refusal}");
    assert!(!refusal.contains("alpha"), "the refusal is content-free");
}

pub(super) fn make_minimal_ledger() -> HandoffLedgerV1 {
    HandoffLedgerV1 {
        schema_version: SCHEMA_VERSION,
        created_at: "20260429T000000".to_string(),
        content_md5: String::new(),
        manifest_md5: "manifest".to_string(),
        project_root: None,
        agent_id: Some("agent-test".to_string()),
        client_name: Some("cursor".to_string()),
        workflow: None,
        session_snapshot: "snapshot".to_string(),
        session: SessionExcerpt::default(),
        tool_calls: ToolCallsSummary::default(),
        evidence_keys: Vec::new(),
        knowledge: KnowledgeExcerpt::default(),
        curated_refs: Vec::new(),
        active_overlays: Vec::new(),
        merged_from: Vec::new(),
        request_id: None,
        request_digest: None,
    }
}

/// Write `ledger` and its binding the way `create_ledger` does, with a
/// chosen content (a real ledger always carries a session snapshot).
pub(super) fn write_bound(
    ledger: &HandoffLedgerV1,
    root: &str,
    sources: Vec<crate::core::context_admission::stored::SourceRef>,
) -> PathBuf {
    // Every element recorded as bound; the marker as creation decides it.
    let origin = ledger.checked_origin(&ledger.bound_record(|_, _| true));
    write_recorded(ledger, root, sources, origin, |_, _| true)
}

/// [`write_bound`] with a chosen origin marker and no bound element.
pub(super) fn write_marked(
    ledger: &HandoffLedgerV1,
    root: &str,
    sources: Vec<crate::core::context_admission::stored::SourceRef>,
    origin: &str,
) -> PathBuf {
    write_recorded(ledger, root, sources, origin, |_, _| false)
}

/// The sidecar as `create_ledger` writes it: under a marker of a ledger
/// checked at creation, the bound record of the elements `bound` names;
/// under any other marker, the binding alone.
pub(super) fn write_recorded(
    ledger: &HandoffLedgerV1,
    root: &str,
    sources: Vec<crate::core::context_admission::stored::SourceRef>,
    origin: &str,
    bound: impl Fn(PartKind, usize) -> bool,
) -> PathBuf {
    // A stored ledger names its project, as `create_ledger` writes it.
    let mut ledger = ledger.clone();
    ledger.project_root.get_or_insert_with(|| root.to_owned());
    let ledger = &ledger;
    let json = serde_json::to_string_pretty(ledger).expect("serialize");
    let text = ledger_text(&json).expect("text");
    let binding = admit_ledger(&text, Some(root), sources, origin).expect("admitted ledger");
    let authority = LedgerAuthority {
        binding,
        bound_parts: checked_at_creation(origin).then(|| ledger.bound_record(bound)),
    };
    let path = Path::new(root).join(format!("{}.json", ledger.created_at));
    std::fs::write(&path, &json).expect("ledger");
    std::fs::write(
        authority_path(&path),
        serde_json::to_string(&authority).expect("authority"),
    )
    .expect("authority");
    path
}

pub(super) fn show(path: &Path) -> String {
    match load_ledger(path) {
        Ok(ledger) => crate::tools::ctx_handoff::format_show(path, &ledger),
        Err(refusal) => refusal,
    }
}

/// `show` of a ledger made of curated excerpts alone is registered under
/// the files of those excerpts and fails with them. A ledger that also
/// carries session text, and one without any source, are not registered.
#[test]
fn only_a_curated_only_ledger_gives_its_reply_a_file_lineage() {
    use crate::core::archive::delivered::LineageRefusal;
    use crate::core::context_admission::stored::lineage_support::{
        deliver, governed, project, reusable, source,
    };
    let _data = governed();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let excerpt = || CuratedRef {
        path: format!("{root}/lib.rs"),
        mode: "signatures".to_string(),
        content_md5: "digest".to_string(),
        content: "fn alpha()".to_string(),
    };
    let lib = || source(&root, "lib.rs", "pub fn alpha() {}\n", "ctx_handoff");

    let curated_only = HandoffLedgerV1 {
        created_at: "curated".to_string(),
        project_root: Some(root.clone()),
        session_snapshot: String::new(),
        curated_refs: vec![excerpt()],
        ..make_minimal_ledger()
    };
    let path = write_bound(&curated_only, &root, vec![lib()]);
    let (reply, recorded) = deliver(&root, || show(&path));
    assert!(reply.contains("fn alpha()"), "{reply}");
    assert_eq!(recorded, 1);
    assert_eq!(reusable(&root, &reply), Ok(()));

    // The same excerpt next to a session snapshot: unknown.
    let with_session = HandoffLedgerV1 {
        created_at: "with-session".to_string(),
        project_root: Some(root.clone()),
        curated_refs: vec![excerpt()],
        ..make_minimal_ledger()
    };
    let session_path = write_bound(&with_session, &root, vec![lib()]);
    let (session_reply, recorded) = deliver(&root, || show(&session_path));
    assert!(session_reply.contains("snapshot"), "{session_reply}");
    assert_eq!(recorded, 0);
    assert_eq!(
        reusable(&root, &session_reply),
        Err(LineageRefusal::Unknown)
    );

    // No curated source at all (caller-supplied): unknown.
    let originless = HandoffLedgerV1 {
        created_at: "originless".to_string(),
        project_root: Some(root.clone()),
        session_snapshot: String::new(),
        ..make_minimal_ledger()
    };
    let originless_path = write_bound(&originless, &root, Vec::new());
    let (originless_reply, recorded) = deliver(&root, || show(&originless_path));
    assert!(
        originless_reply.contains("ctx_handoff show"),
        "{originless_reply}"
    );
    assert_eq!(recorded, 0);
    assert_eq!(
        reusable(&root, &originless_reply),
        Err(LineageRefusal::Unknown)
    );

    // The source is reclassified, then deleted.
    std::fs::write(
        format!("{root}/lib.rs"),
        "*** TOP SECRET ***\npub fn alpha() {}\n",
    )
    .expect("reclassify");
    assert_eq!(
        reusable(&root, &reply),
        Err(LineageRefusal::SourceNoLongerAdmitted)
    );
    std::fs::remove_file(format!("{root}/lib.rs")).expect("delete");
    assert_eq!(
        reusable(&root, &reply),
        Err(LineageRefusal::SourceNoLongerAdmitted)
    );
    let (refusal, recorded) = deliver(&root, || show(&path));
    assert!(!refusal.contains("alpha"), "the refusal is content-free");
    assert_eq!(recorded, 0);
}

/// A new ledger copies only findings and facts that are served now: a
/// finding and a fact bound to files that are gone are absent from the
/// ledger, from `show` and from the compact handoff block; the finding
/// and the fact without a binding stay.
#[test]
fn a_new_ledger_leaves_out_findings_and_facts_that_are_not_served() {
    use crate::core::archive::bindings::{self, Item};
    use crate::core::context_admission::stored::lineage_support::{governed, project, scoped};
    let _data = governed();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let (_directory, root) = project();
    std::fs::write(format!("{root}/pool.rs"), "pub fn lease() {}\n").expect("source");
    std::fs::write(format!("{root}/auth.rs"), "pub fn verify_rs256() {}\n").expect("source");
    let mut session = SessionState::new();
    session.project_root = Some(root.clone());
    session.add_finding_for_file(Some("pool.rs"), Some(1), "the pool leaks one lease");
    session.add_finding_for_file(None, None, "noted without a file");
    let bound_fact = "Tokens are verified with RS256 in the auth module";
    let typed_fact = "Deployments are promoted by the release captain on Fridays";
    for (key, value) in [("token-check", bound_fact), ("release-duty", typed_fact)] {
        let reply = crate::tools::ctx_knowledge::handle(
            &root,
            "remember",
            Some("architecture"),
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
    let item = Item::KnowledgeFact {
        project: &root,
        category: "architecture",
        key: "token-check",
    };
    let sources =
        bindings::sources_at(&root, &["auth.rs"], "ctx_knowledge").expect("readable source");
    assert!(bindings::bind(&item, bound_fact, &root, sources));

    // Everything a new ledger hands on: its stored form, `show`, and the
    // compact block `ctx_handoff create` appends.
    let handed_on = || {
        scoped(&root, || {
            let (ledger, path) = create_ledger(CreateLedgerInput {
                agent_id: None,
                client_name: None,
                project_root: Some(root.clone()),
                session: session.clone(),
                tool_calls: Vec::new(),
                workflow: None,
                curated_refs: Vec::new(),
                request_id: None,
            })
            .expect("create ledger");
            format!(
                "{}\n{}\n{}",
                serde_json::to_string(&ledger).expect("json"),
                crate::tools::ctx_handoff::format_show(&path, &ledger),
                HandoffPackage::build(ledger, None, None).format_compact(),
            )
        })
    };

    let before = handed_on();
    for text in [
        "the pool leaks one lease",
        "noted without a file",
        "RS256",
        "captain",
    ] {
        assert!(before.contains(text), "{text}: {before}");
    }
    std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
    std::fs::remove_file(format!("{root}/auth.rs")).expect("delete");
    let after = handed_on();
    for text in ["the pool leaks one lease", "RS256", "token-check"] {
        assert!(!after.contains(text), "{text}: {after}");
    }
    for text in ["noted without a file", "captain"] {
        assert!(after.contains(text), "{text}: {after}");
    }
}

#[test]
fn create_load_list_clear_ledger_roundtrip() {
    let _env_lock = test_env_lock();
    let tmp = tempdir().expect("tempdir");
    let data_dir = tmp.path().join("leanctx-data");
    let _env = EnvVarGuard::set(
        "LEAN_CTX_DATA_DIR",
        data_dir.to_str().expect("data dir utf8"),
    );

    let input = CreateLedgerInput {
        agent_id: Some("agent-1".to_string()),
        client_name: Some("cursor".to_string()),
        project_root: Some(tmp.path().to_string_lossy().to_string()),
        session: sample_session(),
        tool_calls: sample_tool_calls(),
        workflow: None,
        curated_refs: vec![("src/lib.rs".to_string(), "fn alpha() {}".to_string())],
        request_id: None,
    };

    let (ledger, path) = create_ledger(input).expect("create ledger");
    assert!(path.exists(), "ledger file should be created");
    assert_eq!(ledger.tool_calls.total, 2);
    assert_eq!(ledger.tool_calls.by_tool.get("ctx_read"), Some(&1));
    assert_eq!(ledger.tool_calls.by_ctx_read_mode.get("map"), Some(&1));
    assert_eq!(ledger.curated_refs.len(), 1);
    assert!(
        !ledger.content_md5.is_empty(),
        "`ledger.content_md5` is empty"
    );

    // AP05-G7: list and clear spanned every project, and a ledger of
    // another project or of none was served in Community mode.
    let root = tmp.path().to_string_lossy().to_string();
    let other = tempdir().expect("other project");
    let foreign = handoffs_dir().unwrap().join("19990101000000-foreign.json");
    let projectless = handoffs_dir().unwrap().join("19990101000000-none.json");
    for (file, project_root) in [
        (&foreign, Some(other.path().to_string_lossy().to_string())),
        (&projectless, None),
    ] {
        let ledger = HandoffLedgerV1 {
            project_root,
            ..make_minimal_ledger()
        };
        std::fs::write(file, serde_json::to_string(&ledger).unwrap()).unwrap();
    }
    let in_root = |path: &Path| {
        crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(tmp.path().to_path_buf())),
            || load_ledger(path),
        )
    };

    assert_eq!(list_ledgers(&root), vec![path.clone()]);

    let loaded = in_root(&path).expect("load ledger");
    assert_eq!(loaded.content_md5, ledger.content_md5);
    assert_eq!(loaded.session.decisions.len(), 1);
    assert!(loaded.evidence_keys.iter().any(|k| k == "tool:ctx_read"));
    // Missing, foreign, project-less and corrupt read the same: no oracle.
    let missing = handoffs_dir().unwrap().join("19990101000000-missing.json");
    let corrupt = handoffs_dir().unwrap().join("19990101000000-corrupt.json");
    std::fs::write(&corrupt, "{not json").unwrap();
    let refusal = |path: &Path| {
        in_root(path)
            .expect_err("not served")
            .replace(&path.display().to_string(), "<path>")
    };
    for other in [&foreign, &projectless, &corrupt] {
        assert_eq!(refusal(other), refusal(&missing));
    }

    let removed = clear_ledgers(&root).expect("clear ledgers");
    assert_eq!(removed, 1);
    assert!(foreign.exists() && projectless.exists());
    {
        let found = &list_ledgers(&root);
        assert!(found.is_empty(), "{found:?}");
    }
}

#[test]
fn create_ledger_truncates_curated_refs_to_max() {
    let _env_lock = test_env_lock();
    let tmp = tempdir().expect("tempdir");
    let data_dir = tmp.path().join("leanctx-data");
    let _env = EnvVarGuard::set(
        "LEAN_CTX_DATA_DIR",
        data_dir.to_str().expect("data dir utf8"),
    );

    let curated_refs: Vec<(String, String)> = (0..(MAX_CURATED_REFS + 5))
        .map(|idx| (format!("src/file_{idx}.rs"), format!("fn f_{idx}() {{}}")))
        .collect();

    let input = CreateLedgerInput {
        agent_id: Some("agent-1".to_string()),
        client_name: Some("cursor".to_string()),
        project_root: Some(tmp.path().to_string_lossy().to_string()),
        session: sample_session(),
        tool_calls: sample_tool_calls(),
        workflow: None,
        curated_refs,
        request_id: None,
    };

    let (ledger, _) = create_ledger(input).expect("create ledger");
    assert_eq!(ledger.curated_refs.len(), MAX_CURATED_REFS);
}

/// AP07-G9: `create` retried after a lost response wrote a second
/// ledger. With a key the retry serves the first one; the key reused for
/// other paths is refused; without a key nothing changes; a ledger
/// written before keys existed still loads and never matches a key.
#[test]
fn keyed_create_stores_one_ledger_and_refuses_a_changed_request() {
    let _env_lock = test_env_lock();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let tmp = tempdir().expect("tempdir");
    let data_dir = tmp.path().join("leanctx-data");
    let _env = EnvVarGuard::set(
        "LEAN_CTX_DATA_DIR",
        data_dir.to_str().expect("data dir utf8"),
    );
    let root = tmp.path().to_string_lossy().to_string();
    let in_root = |run: &dyn Fn() -> Result<(HandoffLedgerV1, PathBuf), String>| {
        crate::core::policy::runtime::REQUEST_PROJECT
            .sync_scope(std::cell::RefCell::new(Some(tmp.path().to_path_buf())), run)
    };
    let create = |key: Option<&str>, path: &str| {
        in_root(&|| {
            create_ledger(CreateLedgerInput {
                agent_id: Some("agent-test".to_string()),
                client_name: Some("cursor".to_string()),
                project_root: Some(root.clone()),
                session: sample_session(),
                tool_calls: sample_tool_calls(),
                workflow: None,
                curated_refs: vec![(path.to_string(), "fn alpha() {}".to_string())],
                request_id: key.map(str::to_string),
            })
        })
    };
    // A ledger of this project and agent stored before keys existed.
    let dir = handoffs_dir().expect("handoffs dir");
    std::fs::create_dir_all(&dir).expect("create handoffs dir");
    let legacy = dir.join("19990101000000-legacy.json");
    let mut old = HandoffLedgerV1 {
        project_root: Some(root.clone()),
        ..make_minimal_ledger()
    };
    old.content_md5 = ledger_content_md5(&old);
    let json = serde_json::to_string(&old).expect("serialize legacy ledger");
    std::fs::write(&legacy, json).expect("write legacy ledger");

    let (first, path) = create(Some("k1"), "src/lib.rs").expect("first create");
    let (again, again_path) = create(Some("k1"), "src/lib.rs").expect("replay");
    assert_eq!(again_path, path);
    assert_eq!(again.content_md5, first.content_md5);
    let conflict = create(Some("k1"), "src/other.rs").expect_err("changed request");
    assert!(conflict.contains("idempotency conflict"), "{conflict}");
    assert_eq!(list_ledgers(&root).len(), 2, "legacy and one keyed ledger");

    create(None, "src/lib.rs").expect("unkeyed");
    create(None, "src/lib.rs").expect("unkeyed");
    assert_eq!(list_ledgers(&root).len(), 4);
    let loaded = in_root(&|| load_ledger(&legacy).map(|ledger| (ledger, legacy.clone())))
        .expect("a ledger without a key still loads");
    assert_eq!(loaded.0.content_md5, old.content_md5);
    assert!(loaded.0.request_id.is_none());

    // Two FIRST creates with one key, held between the missed lookup and
    // the write until both are there (`keyed_race`): one ledger is
    // written, the other call is served that ledger, never a second one.
    let _race = keyed_race::arm(2);
    let gate = std::sync::Barrier::new(2);
    let racing = || {
        gate.wait();
        create(Some("k2"), "src/lib.rs").expect("racing keyed create")
    };
    let (left, right) = std::thread::scope(|scope| {
        let left = scope.spawn(racing);
        let right = scope.spawn(racing);
        (left.join().expect("left"), right.join().expect("right"))
    });
    assert_eq!(left.1, right.1, "both callers are served one ledger");
    assert_eq!(left.0.content_md5, right.0.content_md5);
    assert_eq!(list_ledgers(&root).len(), 5, "one ledger for the raced key");
}

#[test]
fn canonicalization_and_content_md5_are_deterministic() {
    let value = serde_json::json!({"b": 1, "a": {"d": 2, "c": 3}});
    let canonical = crate::core::canonical::canonical_value(&value);
    assert_eq!(canonical.to_string(), r#"{"a":{"c":3,"d":2},"b":1}"#);

    let mut ledger = make_minimal_ledger();
    ledger.content_md5 = "old-value".to_string();
    let md5_a = ledger_content_md5(&ledger);
    ledger.content_md5 = "new-value".to_string();
    let md5_b = ledger_content_md5(&ledger);
    assert_eq!(md5_a, md5_b);
}
