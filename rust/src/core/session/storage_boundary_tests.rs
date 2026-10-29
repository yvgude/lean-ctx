// SPDX-License-Identifier: Apache-2.0
//! The session storage boundary (Q14): `prepare_save` is memory work that may
//! run under the live session lock; storage admission runs after the lock is
//! released; the admitting authority travels with the save and is verified
//! again at the commit.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use super::{
    ProjectSessionIndex, SaveError, SessionState, normalized_safe_project_root, project_index_path,
};
use crate::core::policy::runtime::{TestPolicyOverride, lock_test_override};
use crate::core::session::admission::{self, StorageAdmissionProbe};
use crate::core::session::paths::sessions_dir;
use crate::core::session::{Finding, TaskInfo};

const CUSTOMER: &str = "CUSTOMER-482193";
const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
const BLOCK_CUSTOMER: &str = "name = \"session-boundary\"\nversion = \"1.0.0\"\ndescription = \"fixture\"\n[filters.blocked_patterns]\ncustomer = 'CUSTOMER-482193'\n";

fn fixture(project: &Path, id: &str) -> SessionState {
    let root = project.to_string_lossy().into_owned();
    let mut session = SessionState::new();
    session.id = id.to_string();
    session.project_root = Some(root.clone());
    session.shell_cwd = Some(root);
    session.task = Some(TaskInfo {
        description: "storage boundary fixture".to_string(),
        intent: None,
        progress_pct: None,
    });
    session.next_steps.push(format!("publish {CUSTOMER}"));
    // A recorded tool call makes the save carry a compaction snapshot too.
    session.stats.total_tool_calls = 3;
    session.stats.unsaved_changes = 3;
    session
}

fn project_policy(project: &Path, pack: &str) {
    std::fs::create_dir_all(project.join(".lean-ctx")).expect("policy directory");
    std::fs::write(project.join(".lean-ctx/policy.toml"), pack).expect("write project policy");
}

fn gateway_config(value: &str) {
    let dir = crate::core::paths::config_dir_read_only().expect("isolated config directory");
    std::fs::create_dir_all(&dir).expect("config directory");
    std::fs::write(dir.join("config.toml"), value).expect("write gateway config");
}

fn collect(base: &Path, dir: &Path, found: &mut BTreeMap<String, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(base, &path, found);
            continue;
        }
        let name = path
            .strip_prefix(base)
            .expect("entry below the session store")
            .to_string_lossy()
            .into_owned();
        // Advisory lock files are coordination state, never published content.
        if Path::new(&name)
            .extension()
            .is_some_and(|extension| extension == "lock")
        {
            continue;
        }
        found.insert(name, std::fs::read(&path).expect("read store entry"));
    }
}

/// Every file below the session store with its bytes: primary session files,
/// `latest.json`, compaction snapshots, the project index and any temporary
/// file a writer left behind. An absent store is an empty map.
fn published() -> BTreeMap<String, Vec<u8>> {
    let sessions = sessions_dir().expect("sessions dir");
    let mut found = BTreeMap::new();
    collect(&sessions, &sessions, &mut found);
    found
}

/// The names of the store's files — never their bytes.
fn names(store: &BTreeMap<String, Vec<u8>>) -> Vec<&String> {
    store.keys().collect()
}

/// The WHOLE session store is empty; a failure names the files found.
fn assert_nothing_published(what: &str) {
    let store = published();
    assert!(store.is_empty(), "{what}: {:?}", names(&store));
}

const CHILD: &str = "LEAN_CTX_STORAGE_BOUNDARY_SUBPROCESS";
const CHILD_LIMIT: std::time::Duration = std::time::Duration::from_mins(2);

/// These tests assert on EVERY file of the session store, and the store is
/// selected by the process environment: any thread of this test binary that
/// saves a session meanwhile writes into the directory under test. Each body
/// therefore runs in a process of its own (the pattern of
/// `http_server/cancellation_tests.rs`), which contains nothing else.
///
/// In the test binary's own process this starts that child for exactly the
/// test `name`, waits a bounded time, fails with the child's output when it
/// failed, did not end or ran no test, and returns `true`: the body has run.
/// In the child it returns `false` and the body runs.
fn ran_in_its_own_process(name: &str) -> bool {
    use std::io::{Read, Seek};

    if std::env::var_os(CHILD).is_some() {
        return false;
    }
    // The child inherits this process's environment: held, so no other
    // test's temporary variable is in it.
    let _env = crate::core::data_dir::test_env_lock();
    let environment = tempfile::tempdir().expect("child environment");
    let mut output = tempfile::tempfile().expect("child output");
    let exact = format!("core::session::persistence::storage_boundary_tests::{name}");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    command
        .args(["--exact", &exact, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .env_remove("LEAN_CTX_PROJECT_ROOT")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("WORKSPACE_FOLDER_PATHS")
        .stdin(std::process::Stdio::null())
        .stdout(output.try_clone().expect("child stdout"))
        .stderr(output.try_clone().expect("child stderr"));
    for directory in [
        "LEAN_CTX_DATA_DIR",
        "LEAN_CTX_CONFIG_DIR",
        "LEAN_CTX_STATE_DIR",
        "LEAN_CTX_CACHE_DIR",
    ] {
        command.env(directory, environment.path());
    }
    let mut child = command.spawn().expect("start the isolated test");
    let deadline = std::time::Instant::now() + CHILD_LIMIT;
    let status = loop {
        match child.try_wait().expect("wait for the isolated test") {
            Some(status) => break Some(status),
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    };
    let mut bytes = Vec::new();
    output.rewind().expect("child output");
    output.read_to_end(&mut bytes).expect("child output");
    let text = String::from_utf8_lossy(&bytes);
    let status = status.unwrap_or_else(|| panic!("{exact} did not end in {CHILD_LIMIT:?}\n{text}"));
    assert!(
        status.success(),
        "{exact} failed in its own process\n{text}"
    );
    // A name that selects nothing would also exit 0.
    assert!(
        text.contains("test result: ok. 1 passed"),
        "{exact} did not run in its own process\n{text}"
    );
    true
}

fn assert_absent_everywhere(store: &BTreeMap<String, Vec<u8>>, needle: &str) {
    for (name, bytes) in store {
        assert!(
            !String::from_utf8_lossy(bytes).contains(needle),
            "{name} carries content the current policy withholds"
        );
    }
}

/// Installs a probe that records, at the start of every storage admission on
/// this thread, whether the live session write lock could be taken.
fn lock_probe(
    owner: &Arc<tokio::sync::RwLock<SessionState>>,
) -> (StorageAdmissionProbe, Rc<RefCell<Vec<bool>>>) {
    let lock_free = Rc::new(RefCell::new(Vec::new()));
    let probe = StorageAdmissionProbe::install({
        let owner = Arc::clone(owner);
        let lock_free = Rc::clone(&lock_free);
        move || lock_free.borrow_mut().push(owner.try_write().is_ok())
    });
    (probe, lock_free)
}

#[test]
fn prepare_under_the_session_write_lock_runs_no_storage_admission() {
    if ran_in_its_own_process("prepare_under_the_session_write_lock_runs_no_storage_admission") {
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    let project = tempfile::tempdir().expect("project root");
    let owner = Arc::new(tokio::sync::RwLock::new(fixture(
        project.path(),
        "boundary-lock",
    )));
    let (_probe, lock_free) = lock_probe(&owner);

    // The shape of `server/call_tool/policy.rs`: prepare under the write
    // guard, drop the guard, publish afterwards.
    let prepared = {
        let mut live = owner.try_write().expect("uncontested session");
        let prepared = live.prepare_save().expect("prepare under the write guard");
        assert!(
            lock_free.borrow().is_empty(),
            "prepare_save ran storage admission (configuration and policy refresh) while the session write lock was held"
        );
        prepared
    };
    prepared
        .publish()
        .expect("publish after the guard is released");

    let lock_free = lock_free.borrow();
    assert!(
        !lock_free.is_empty(),
        "publication must run storage admission"
    );
    assert!(
        lock_free.iter().all(|free| *free),
        "storage admission must only run while the session write lock is free"
    );
}

#[test]
fn unavailable_project_policy_is_reported_after_the_session_lock_is_released() {
    if ran_in_its_own_process(
        "unavailable_project_policy_is_reported_after_the_session_lock_is_released",
    ) {
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    let project = tempfile::tempdir().expect("project root");
    let owner = Arc::new(tokio::sync::RwLock::new(fixture(
        project.path(),
        "boundary-refresh-unavailable",
    )));
    // The policy refresh for this project can no longer succeed.
    std::fs::remove_dir_all(project.path()).expect("delete project root");
    let (_probe, lock_free) = lock_probe(&owner);

    let prepared = {
        let mut live = owner.try_write().expect("uncontested session");
        let prepared = live
            .prepare_save()
            .expect("prepare_save must not consult the project root or its policy");
        assert!(lock_free.borrow().is_empty());
        prepared
    };
    let error = prepared
        .publish()
        .expect_err("a session of a missing project root is withheld");

    assert!(
        matches!(error, SaveError::AdmissionRefused(_)),
        "unexpected refusal: {error:?}"
    );
    assert!(!lock_free.borrow().is_empty() && lock_free.borrow().iter().all(|free| *free));
    assert_nothing_published("a refused save writes nothing");
}

#[test]
fn prepared_save_is_not_published_after_its_project_root_is_deleted() {
    if ran_in_its_own_process("prepared_save_is_not_published_after_its_project_root_is_deleted") {
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    let project = tempfile::tempdir().expect("project root");
    let mut session = fixture(project.path(), "boundary-root-deleted");
    let index_path = project_index_path(
        &sessions_dir().expect("sessions dir"),
        &normalized_safe_project_root(&project.path().to_string_lossy()).expect("safe root"),
    );

    let prepared = session
        .prepare_save()
        .expect("prepare while the root exists");
    std::fs::remove_dir_all(project.path()).expect("delete project root");

    let outcome = prepared.write_to_disk_with_policy(false);
    assert!(
        matches!(outcome.result, Err(SaveError::AdmissionRefused(_))),
        "unexpected outcome: {:?}",
        outcome.result
    );
    let sessions = sessions_dir().expect("sessions dir");
    assert!(!sessions.join("boundary-root-deleted.json").exists());
    assert!(!sessions.join("boundary-root-deleted_snapshot.txt").exists());
    assert!(!sessions.join("latest.json").exists());
    assert!(!index_path.exists());
    assert_nothing_published("nothing at all may be written");

    // Pending changes stay pending: the refusal is never an acknowledgement.
    assert!(session.acknowledge_save(outcome).is_err());
    assert!(session.stats.unsaved_changes >= 3);
    assert!(session.last_save_failed);
    assert!(session.should_save());
}

#[test]
fn admitted_save_is_refused_at_commit_after_its_project_root_is_deleted() {
    if ran_in_its_own_process(
        "admitted_save_is_refused_at_commit_after_its_project_root_is_deleted",
    ) {
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    let project = tempfile::tempdir().expect("project root");
    let mut session = fixture(project.path(), "boundary-root-revoked");
    session.save().expect("first save under the existing root");
    let before = published();
    assert!(before.contains_key("boundary-root-revoked.json"));
    assert!(before.contains_key("latest.json"));

    session.next_steps.push("second revision".to_string());
    session.increment();
    let admitted = session
        .prepare_save()
        .expect("prepare")
        .admit()
        .expect("admitted while the root exists");
    std::fs::remove_dir_all(project.path()).expect("delete project root");

    let error = admitted
        .write_to_disk()
        .expect_err("the admitting authority no longer holds");
    assert!(
        matches!(error, SaveError::AuthorityChanged(_)),
        "unexpected refusal: {error:?}"
    );
    assert_eq!(
        published(),
        before,
        "primary file, pointer, snapshot and index must be unchanged"
    );
}

#[test]
fn admitted_save_is_refused_at_commit_after_the_project_policy_is_tightened() {
    if ran_in_its_own_process(
        "admitted_save_is_refused_at_commit_after_the_project_policy_is_tightened",
    ) {
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    // The live project pack is the authority here, not a fixed test override.
    let _live = lock_test_override();
    let project = tempfile::tempdir().expect("project root");
    let mut session = fixture(project.path(), "boundary-policy-tightened");

    let admitted = session
        .prepare_save()
        .expect("prepare")
        .admit()
        .expect("admitted while no pack withholds the content");
    project_policy(project.path(), BLOCK_CUSTOMER);

    let error = admitted
        .write_to_disk()
        .expect_err("content admitted under the previous policy must not be published");
    assert!(
        matches!(error, SaveError::AuthorityChanged(_)),
        "unexpected refusal: {error:?}"
    );
    assert_nothing_published("nothing may be written");
}

#[test]
fn prepared_save_never_publishes_content_a_tightened_project_policy_withholds() {
    if ran_in_its_own_process(
        "prepared_save_never_publishes_content_a_tightened_project_policy_withholds",
    ) {
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let _live = lock_test_override();
    let project = tempfile::tempdir().expect("project root");
    let mut session = fixture(project.path(), "boundary-policy-late");

    let prepared = session.prepare_save().expect("prepare");
    project_policy(project.path(), BLOCK_CUSTOMER);
    let result = prepared.publish();

    // Admission runs at publication, under the tightened pack: the withheld
    // record is gone from every artifact, or the whole save is refused.
    let store = published();
    assert_absent_everywhere(&store, CUSTOMER);
    if result.is_ok() {
        let stored = SessionState::load_by_id("boundary-policy-late")
            .expect("the admitted remainder is published");
        assert!(stored.next_steps.is_empty());
    } else {
        assert!(
            store.is_empty(),
            "{}: {:?}",
            "a refused save writes nothing",
            names(&store)
        );
    }
}

#[test]
fn admitted_save_is_refused_at_commit_after_the_gateway_is_tightened() {
    if ran_in_its_own_process("admitted_save_is_refused_at_commit_after_the_gateway_is_tightened") {
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    gateway_config("[context_gateway]\nenabled = false\n");
    let project = tempfile::tempdir().expect("project root");
    let mut session = fixture(project.path(), "boundary-gateway-tightened");
    session.findings.push(Finding {
        file: None,
        line: None,
        summary: format!("key is {AWS_KEY}"),
        timestamp: chrono::Utc::now(),
    });

    let admitted = session
        .prepare_save()
        .expect("prepare")
        .admit()
        .expect("admitted while the Gateway is disabled");
    let late = session.prepare_save().expect("prepare a second copy");
    gateway_config("[context_gateway]\nmode = \"governed\"\n");

    let error = admitted
        .write_to_disk()
        .expect_err("content admitted under the previous Gateway policy must not be published");
    assert!(
        matches!(error, SaveError::AuthorityChanged(_)),
        "unexpected refusal: {error:?}"
    );
    assert_nothing_published("nothing may be written");

    // The copy that was only prepared is admitted under the governed policy.
    let result = late.publish();
    let store = published();
    assert_absent_everywhere(&store, AWS_KEY);
    if result.is_err() {
        assert!(
            store.is_empty(),
            "{}: {:?}",
            "a refused save writes nothing",
            names(&store)
        );
    }
}

/// The authority check runs inside the project-index lock, so a slow policy
/// refresh there can make a competing save of the same project miss the
/// lock's acquisition deadline. The competitor must report that as a storage
/// failure, write nothing and keep its changes pending for the next batch.
/// The lock is held for the whole call, so the outcome does not depend on
/// timing; the call only takes as long as the acquisition deadline.
#[test]
fn competitor_that_misses_the_project_index_lock_writes_nothing_and_stays_pending() {
    if ran_in_its_own_process(
        "competitor_that_misses_the_project_index_lock_writes_nothing_and_stays_pending",
    ) {
        return;
    }
    use fs2::FileExt;

    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    let project = tempfile::tempdir().expect("project root");
    let mut session = fixture(project.path(), "boundary-index-lock-busy");
    let index_path = project_index_path(
        &sessions_dir().expect("sessions dir"),
        &normalized_safe_project_root(&project.path().to_string_lossy()).expect("safe root"),
    );
    std::fs::create_dir_all(index_path.parent().expect("index directory"))
        .expect("create index directory");
    // Stands in for another save of this project that is still inside its
    // commit (for example waiting on the policy refresh of its authority check).
    let holder = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(index_path.with_extension("lock"))
        .expect("project index lock file");
    holder
        .lock_exclusive()
        .expect("hold the project index lock");

    let outcome = session
        .prepare_save()
        .expect("prepare")
        .write_to_disk_with_policy(false);

    assert!(
        matches!(
            &outcome.result,
            Err(SaveError::Storage(message)) if message.contains("project index lock")
        ),
        "unexpected outcome: {:?}",
        outcome.result
    );
    assert_nothing_published("the competitor writes nothing");
    assert!(session.acknowledge_save(outcome).is_err());
    assert!(session.stats.unsaved_changes >= 3, "changes stay pending");
    assert!(session.last_save_failed);
    assert!(session.should_save());

    FileExt::unlock(&holder).expect("release");
    session
        .save()
        .expect("the retry publishes once the lock is free");
    assert!(published().contains_key("boundary-index-lock-busy.json"));
    assert_eq!(session.stats.unsaved_changes, 0);
}

#[test]
fn unchanged_authority_publishes_the_pre_change_bytes() {
    if ran_in_its_own_process("unchanged_authority_publishes_the_pre_change_bytes") {
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    let project = tempfile::tempdir().expect("project root");
    let mut session = fixture(project.path(), "boundary-positive");
    let canonical_root =
        normalized_safe_project_root(&project.path().to_string_lossy()).expect("safe root");

    // What the previous `prepare_save` serialized under the lock: the storage
    // projection, its storage JSON and its compaction snapshot.
    let projection = admission::for_storage(&session).expect("storage projection");
    let (expected_json, _) = projection.storage_json().expect("storage json");
    let expected_snapshot = projection.build_compaction_snapshot();
    assert!(expected_json.contains(CUSTOMER));

    session
        .prepare_save()
        .expect("prepare")
        .publish()
        .expect("unchanged authority publishes");

    let sessions = sessions_dir().expect("sessions dir");
    let read = |name: &str| std::fs::read(sessions.join(name)).expect("published artifact");
    assert_eq!(read("boundary-positive.json"), expected_json.as_bytes());
    assert_eq!(read("latest.json"), br#"{"id":"boundary-positive"}"#);
    assert_eq!(
        read("boundary-positive_snapshot.txt"),
        expected_snapshot.as_bytes()
    );
    let index: ProjectSessionIndex = serde_json::from_slice(
        &std::fs::read(project_index_path(&sessions, &canonical_root)).expect("project index"),
    )
    .expect("valid project index");
    assert_eq!(index.version, 1);
    assert_eq!(index.project_root, canonical_root);
    assert_eq!(index.session_ids, ["boundary-positive"]);

    // The explicit two-step form commits the same bytes.
    let after_first = published();
    session
        .prepare_save()
        .expect("prepare")
        .admit()
        .expect("admit")
        .write_to_disk()
        .expect("commit under the unchanged authority");
    assert_eq!(published(), after_first);

    // And the acknowledged synchronous save still clears the pending counter.
    session.save().expect("synchronous save");
    assert_eq!(session.stats.unsaved_changes, 0);
    assert!(!session.last_save_failed);
}

#[test]
fn background_save_adapter_publishes_then_keeps_changes_pending_without_a_root() {
    if ran_in_its_own_process(
        "background_save_adapter_publishes_then_keeps_changes_pending_without_a_root",
    ) {
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    let project = tempfile::tempdir().expect("project root");
    let owner = Arc::new(tokio::sync::RwLock::new(fixture(
        project.path(),
        "boundary-background",
    )));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    // Admission and commit run on the blocking pool, which has no request
    // scope of its own; the save still publishes under the session's project.
    runtime
        .block_on(SessionState::save_shared(Arc::clone(&owner)))
        .expect("background save under the existing root");
    let before = published();
    assert!(before.contains_key("boundary-background.json"));
    assert_eq!(
        owner.try_read().expect("uncontested").stats.unsaved_changes,
        0
    );

    {
        let mut live = owner.try_write().expect("uncontested");
        live.next_steps.push("second revision".to_string());
        live.increment();
    }
    std::fs::remove_dir_all(project.path()).expect("delete project root");

    let error = runtime
        .block_on(SessionState::save_shared(Arc::clone(&owner)))
        .expect_err("a session of a deleted project root is withheld");
    assert!(!error.is_empty());
    assert_eq!(published(), before, "the refused save changes nothing");
    let live = owner.try_read().expect("uncontested");
    assert_ne!(live.stats.unsaved_changes, 0, "changes stay pending");
    assert!(live.last_save_failed);
}
