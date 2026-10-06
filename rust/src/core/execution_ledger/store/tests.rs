use super::*;

fn task_started() -> ExecutionEvent {
    ExecutionEvent::TaskStarted {
        task_id: "task-1".to_owned(),
        trace_id: "trace-1".to_owned(),
        envelope_ref: "task:1".to_owned(),
        timestamp: "2026-08-23T12:00:00Z".to_owned(),
        sequence_number: 0,
        prev_hash: String::new(),
    }
}

fn plan_created() -> ExecutionEvent {
    ExecutionEvent::PlanCreated {
        task_id: "task-1".to_owned(),
        trace_id: "trace-1".to_owned(),
        plan_id: "plan-1".to_owned(),
        plan_ref: "plan:1".to_owned(),
        timestamp: "2026-08-23T12:00:01Z".to_owned(),
        sequence_number: 0,
        prev_hash: String::new(),
    }
}

fn stage_prepared_append(
    store: &ExecutionLedgerStore,
    event: ExecutionEvent,
    omitted_suffix: usize,
) -> Vec<u8> {
    let previous = fs::read(store.path()).unwrap_or_default();
    store.append(event).unwrap();
    let complete = fs::read(store.path()).unwrap();
    let record = complete[previous.len()..].to_vec();
    assert!(record.ends_with(b"\n"));
    OpenOptions::new()
        .write(true)
        .open(store.path())
        .unwrap()
        .set_len(u64::try_from(previous.len()).unwrap())
        .unwrap();
    let line = std::str::from_utf8(&record[..record.len() - 1]).unwrap();
    write_append_journal(
        store.path(),
        u64::try_from(previous.len()).unwrap(),
        &sha256(&previous),
        line,
    )
    .unwrap();
    let written = record.len().saturating_sub(omitted_suffix);
    let mut file = OpenOptions::new().append(true).open(store.path()).unwrap();
    file.write_all(&record[..written]).unwrap();
    file.sync_all().unwrap();
    record
}

#[test]
fn prepared_complete_record_without_newline_is_finalized_before_retry() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    stage_prepared_append(&store, task_started(), 1);

    assert!(store.load_verified().is_err());
    assert!(!store.append_if_new(task_started()).unwrap());
    assert_eq!(store.load_verified().unwrap().len(), 1);
    assert!(fs::read(&path).unwrap().ends_with(b"\n"));
    assert!(!append_journal_path(&path).exists());
}

#[test]
fn prepared_partial_record_is_completed_before_retry() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    stage_prepared_append(&store, plan_created(), 7);

    assert!(!store.append_if_new(plan_created()).unwrap());
    assert_eq!(store.load_verified().unwrap().len(), 2);
    assert!(store.verify_chain().unwrap());
    assert!(!append_journal_path(&path).exists());
}

#[test]
fn prepared_record_is_completed_when_no_ledger_byte_was_written() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    stage_prepared_append(&store, plan_created(), usize::MAX);

    assert!(!store.append_if_new(plan_created()).unwrap());
    assert_eq!(store.load_verified().unwrap().len(), 2);
}

#[test]
fn prepared_first_record_is_completed_from_empty_ledger() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    stage_prepared_append(&store, task_started(), usize::MAX);

    assert!(!store.append_if_new(task_started()).unwrap());
    assert_eq!(store.load_verified().unwrap().len(), 1);
}

#[test]
fn completed_prepared_record_only_clears_stale_journal() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    stage_prepared_append(&store, plan_created(), 0);

    assert!(store.load_verified().is_err());
    assert!(!store.append_if_new(plan_created()).unwrap());
    assert_eq!(store.load_verified().unwrap().len(), 2);
    assert!(!append_journal_path(&path).exists());
}

#[test]
fn newline_terminated_corruption_remains_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"garbage\n").unwrap();
    file.sync_all().unwrap();
    let before = fs::read(&path).unwrap();

    assert!(store.append(plan_created()).is_err());
    assert!(store.by_task_verified("task-1").is_err());
    assert!(store.last_sequence_verified().is_err());
    assert!(store.canonical_receipt_for_task_verified("task-1").is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn unmatched_unterminated_corruption_remains_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"garbage").unwrap();
    file.sync_all().unwrap();
    let before = fs::read(&path).unwrap();

    assert!(store.append(plan_created()).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn tampered_prefix_prevents_prepared_tail_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    stage_prepared_append(&store, plan_created(), 7);
    let bytes = fs::read(&path).unwrap();
    let tampered = String::from_utf8(bytes)
        .unwrap()
        .replace("task-1", "task-X");
    fs::write(&path, tampered.as_bytes()).unwrap();
    let before = fs::read(&path).unwrap();

    assert!(store.append(plan_created()).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn prepared_tail_mismatch_remains_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    stage_prepared_append(&store, plan_created(), 7);
    let mut bytes = fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(&path, &bytes).unwrap();
    let before = fs::read(&path).unwrap();

    assert!(store.append(plan_created()).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn bytes_after_prepared_record_remain_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    stage_prepared_append(&store, plan_created(), 0);
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"extra").unwrap();
    file.sync_all().unwrap();
    let before = fs::read(&path).unwrap();

    assert!(store.append(plan_created()).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn prepared_lifecycle_violation_is_rejected_before_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    let previous = fs::read(&path).unwrap();
    let previous_event = store.load_verified().unwrap().pop().unwrap();
    let mut event = task_started();
    event.set_chain_fields(2, hash_event(&previous_event).unwrap());
    let entry_hash = hash_event(&event).unwrap();
    event.set_entry_hash(entry_hash.clone());
    let line = serde_json::to_string(&LedgerRecordV1 {
        schema: LEDGER_RECORD_SCHEMA.to_owned(),
        kind: LEDGER_RECORD_KIND.to_owned(),
        event,
        entry_hash,
    })
    .unwrap();
    write_append_journal(
        &path,
        u64::try_from(previous.len()).unwrap(),
        &sha256(&previous),
        &line,
    )
    .unwrap();

    assert!(store.append(plan_created()).is_err());
    assert_eq!(fs::read(&path).unwrap(), previous);
}

#[test]
fn crlf_terminated_record_remains_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    let mut bytes = fs::read(&path).unwrap();
    bytes.pop();
    bytes.extend_from_slice(b"\r\n");
    fs::write(&path, &bytes).unwrap();

    assert!(store.append(plan_created()).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn incomplete_utf8_tail_without_journal_remains_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(&[0xe2, 0x82]).unwrap();
    file.sync_all().unwrap();

    let before = fs::read(&path).unwrap();
    assert!(store.append(plan_created()).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn incomplete_first_record_without_journal_remains_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    fs::write(&path, b"{\"schema\":").unwrap();
    let store = ExecutionLedgerStore::new(&path);
    let before = fs::read(&path).unwrap();

    assert!(store.append(task_started()).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn valid_unterminated_record_without_journal_remains_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    let file = OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(file.metadata().unwrap().len() - 1).unwrap();
    let before = fs::read(&path).unwrap();

    assert!(store.load_verified().is_err());
    assert!(store.append(plan_created()).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn every_pending_journal_state_is_hidden_until_exclusive_recovery() {
    for omitted_suffix in [usize::MAX, 7, 1, 0] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ledger.jsonl");
        let store = ExecutionLedgerStore::new(&path);
        stage_prepared_append(&store, task_started(), omitted_suffix);

        assert!(store.load().is_err());
        assert!(store.load_verified().is_err());
        assert!(store.verify_chain().is_err());
        assert!(store.by_task_verified("task-1").is_err());
        assert!(store.last_sequence_verified().is_err());
        assert!(store.canonical_receipt_for_task_verified("task-1").is_err());
        assert!(!store.append_if_new(task_started()).unwrap());
        assert_eq!(store.load_verified().unwrap().len(), 1);
        assert_eq!(store.by_task_verified("task-1").unwrap().len(), 1);
        assert_eq!(store.last_sequence_verified().unwrap(), 1);
    }
}

#[test]
fn pending_journal_without_ledger_remains_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    fs::write(append_journal_path(&path), b"{}").unwrap();
    let store = ExecutionLedgerStore::new(&path);

    assert!(store.load().is_err());
    assert!(store.load_verified().is_err());
    assert!(store.verify_chain().is_err());
    assert!(store.append(task_started()).is_err());
    assert!(!path.exists());
}

#[test]
fn temporary_journal_is_hidden_then_cleaned_by_next_append() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    fs::write(append_journal_temp_path(&path), b"stale prepared bytes").unwrap();

    assert!(store.load_verified().is_err());
    assert!(!store.append_if_new(task_started()).unwrap());
    assert_eq!(store.load_verified().unwrap().len(), 1);
    assert!(!append_journal_temp_path(&path).exists());
}

#[test]
fn oversized_journal_is_rejected_without_ledger_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    let before = fs::read(&path).unwrap();
    fs::write(
        append_journal_path(&path),
        vec![b' '; MAX_APPEND_JOURNAL_BYTES + 1],
    )
    .unwrap();

    assert!(store.append(plan_created()).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn journal_symlink_is_rejected_without_touching_target_or_ledger() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let target = directory.path().join("target");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    let before = fs::read(&path).unwrap();
    fs::write(&target, b"target-bytes").unwrap();
    symlink(&target, append_journal_path(&path)).unwrap();

    assert!(store.append(plan_created()).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::read(&target).unwrap(), b"target-bytes");
}

#[test]
fn journal_publish_never_replaces_existing_destination() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    fs::write(append_journal_path(&path), b"existing").unwrap();

    assert!(write_append_journal(&path, 0, &sha256(b""), "{}").is_err());
    assert_eq!(fs::read(append_journal_path(&path)).unwrap(), b"existing");
}

#[cfg(unix)]
#[test]
fn crash_after_journal_link_before_temp_unlink_recovers_prepared_state() {
    use std::fs::hard_link;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    stage_prepared_append(&store, plan_created(), 7);
    let journal = append_journal_path(&path);
    let temporary = append_journal_temp_path(&path);
    hard_link(&journal, &temporary).unwrap();

    assert!(store.load_verified().is_err());
    assert!(!store.append_if_new(plan_created()).unwrap());
    assert_eq!(store.load_verified().unwrap().len(), 2);
    assert!(!journal.exists());
    assert!(!temporary.exists());
}

#[test]
fn journal_error_cleans_unpublished_temporary_only() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let journal = append_journal_path(&path);
    let temporary = append_journal_temp_path(&path);
    fs::write(&journal, b"existing").unwrap();

    assert!(write_append_journal(&path, 0, &sha256(b""), "{}").is_err());
    assert_eq!(fs::read(&journal).unwrap(), b"existing");
    assert!(!temporary.exists());
}

#[test]
fn oversized_record_is_rejected_before_journal_or_ledger_write() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    let mut event = task_started();
    if let ExecutionEvent::TaskStarted { envelope_ref, .. } = &mut event {
        *envelope_ref = "x".repeat(MAX_LEDGER_RECORD_BYTES + 1);
    }

    assert!(store.append(event).is_err());
    assert_eq!(fs::read(&path).unwrap(), Vec::<u8>::new());
    assert!(!append_journal_path(&path).exists());
    assert!(!append_journal_temp_path(&path).exists());
}

#[test]
fn oversized_prepared_record_is_rejected_before_recovery_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    fs::write(&path, b"").unwrap();
    let store = ExecutionLedgerStore::new(&path);
    let mut event = task_started();
    if let ExecutionEvent::TaskStarted { envelope_ref, .. } = &mut event {
        *envelope_ref = "x".repeat(MAX_LEDGER_RECORD_BYTES + 1);
    }
    event.set_chain_fields(1, GENESIS.to_owned());
    let entry_hash = hash_event(&event).unwrap();
    event.set_entry_hash(entry_hash.clone());
    let record = serde_json::to_string(&LedgerRecordV1 {
        schema: LEDGER_RECORD_SCHEMA.to_owned(),
        kind: LEDGER_RECORD_KIND.to_owned(),
        event,
        entry_hash,
    })
    .unwrap();
    let journal = AppendJournalV1 {
        schema: APPEND_JOURNAL_SCHEMA.to_owned(),
        previous_len: 0,
        previous_sha256: sha256(b""),
        record_sha256: sha256(record.as_bytes()),
        record,
    };
    fs::write(
        append_journal_path(&path),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();

    assert!(store.append(task_started()).is_err());
    assert_eq!(fs::read(&path).unwrap(), Vec::<u8>::new());
}

#[test]
fn oversized_on_disk_record_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    fs::write(&path, vec![b'x'; MAX_LEDGER_RECORD_BYTES + 2]).unwrap();
    let store = ExecutionLedgerStore::new(&path);

    assert!(store.load().is_err());
}

#[test]
fn corrupt_append_journal_remains_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    stage_prepared_append(&store, plan_created(), 7);
    fs::write(append_journal_path(&path), b"{\"schema\":").unwrap();
    let before = fs::read(&path).unwrap();

    assert!(store.append(plan_created()).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn ledger_symlink_is_rejected_by_every_read_projection() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("target.jsonl");
    let path = directory.path().join("ledger.jsonl");
    let target_store = ExecutionLedgerStore::new(&target);
    target_store.append(task_started()).unwrap();
    symlink(&target, &path).unwrap();
    let store = ExecutionLedgerStore::new(&path);

    assert!(store.load().is_err());
    assert!(store.load_verified().is_err());
    assert!(store.verify_chain().is_err());
}

#[cfg(unix)]
#[test]
fn parent_symlink_is_rejected_before_ledger_open() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let target_directory = tempfile::tempdir().unwrap();
    let target = target_directory.path().join("ledger.jsonl");
    let target_store = ExecutionLedgerStore::new(&target);
    target_store.append(task_started()).unwrap();
    let parent = directory.path().join("execution");
    symlink(target_directory.path(), &parent).unwrap();
    let store = ExecutionLedgerStore::new(parent.join("ledger.jsonl"));

    assert!(store.load().is_err());
    assert!(store.load_verified().is_err());
    assert!(store.verify_chain().is_err());
}

#[cfg(unix)]
#[test]
fn hardlink_alias_is_rejected_after_descriptor_identity_check() {
    use std::fs::hard_link;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let alias = directory.path().join("ledger-alias.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    hard_link(&path, &alias).unwrap();

    assert!(store.load().is_err());
    assert!(store.load_verified().is_err());
    assert!(store.verify_chain().is_err());
}

#[cfg(unix)]
#[test]
fn opened_descriptor_is_not_redirected_by_path_swap() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let moved = directory.path().join("ledger-moved.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    let (_, relative) = path_parts(&path).unwrap();
    let operation = store.open_operation(false).unwrap();
    let file = open_regular_nofollow_operation(&operation, &relative)
        .unwrap()
        .unwrap();

    fs::rename(&path, &moved).unwrap();
    assert!(read_events_from_file(&file).is_ok());
    assert!(
        validate_operation_file(&operation, &relative, &file, "execution ledger", true).is_err()
    );
}

#[cfg(unix)]
#[test]
fn opened_descriptor_rejects_parent_swap_after_acquisition() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("execution");
    fs::create_dir(&parent).unwrap();
    let path = parent.join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    let (_, relative) = path_parts(&path).unwrap();
    let operation = store.open_operation(false).unwrap();
    let file = open_regular_nofollow_operation(&operation, &relative)
        .unwrap()
        .unwrap();

    let moved = directory.path().join("execution-moved");
    fs::rename(&parent, &moved).unwrap();
    symlink(&moved, &parent).unwrap();
    assert!(read_events_from_file(&file).is_ok());
    assert!(
        validate_operation_file(&operation, &relative, &file, "execution ledger", true).is_err()
    );
}

#[cfg(unix)]
#[test]
fn hardlink_created_after_open_is_rejected_before_commit() {
    use std::fs::hard_link;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let alias = directory.path().join("ledger-race-alias.jsonl");
    let store = ExecutionLedgerStore::new(&path);
    store.append(task_started()).unwrap();
    let operation = store.open_operation(false).unwrap();
    let file = open_regular_nofollow_operation(&operation, Path::new("ledger.jsonl"))
        .unwrap()
        .unwrap();
    hard_link(&path, &alias).unwrap();

    assert!(
        validate_operation_file(
            &operation,
            Path::new("ledger.jsonl"),
            &file,
            "execution ledger",
            true,
        )
        .is_err()
    );
    assert!(store.append(plan_created()).is_err());
}

#[test]
fn verified_constructor_rejects_parent_traversal() {
    let directory = tempfile::tempdir().unwrap();
    assert!(
        ExecutionLedgerStore::new_verified(directory.path(), Path::new("nested/../ledger.jsonl"))
            .is_err()
    );
    assert!(
        ExecutionLedgerStore::new_verified(directory.path(), Path::new("/outside/ledger.jsonl"))
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn verified_constructor_rejects_symlink_root() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let real_root = directory.path().join("real-root");
    let alias_root = directory.path().join("alias-root");
    fs::create_dir(&real_root).unwrap();
    symlink(&real_root, &alias_root).unwrap();

    assert!(ExecutionLedgerStore::new_verified(&alias_root, "ledger.jsonl").is_err());
}

#[cfg(unix)]
#[test]
fn verified_root_replacement_is_rejected_before_append_side_effect() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    let moved = directory.path().join("root-moved");
    fs::create_dir(&root).unwrap();
    let store = ExecutionLedgerStore::new_verified(&root, "ledger.jsonl").unwrap();

    fs::rename(&root, &moved).unwrap();
    fs::create_dir(&root).unwrap();

    assert!(store.append(task_started()).is_err());
    assert!(!root.join("ledger.jsonl").exists());
    assert!(!moved.join("ledger.jsonl").exists());
}

#[cfg(unix)]
#[test]
fn verified_parent_replacement_is_rejected_before_append_side_effect() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    let parent = root.join("execution");
    let moved = root.join("execution-moved");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&parent).unwrap();
    let store = ExecutionLedgerStore::new_verified(&root, "execution/ledger.jsonl").unwrap();

    fs::rename(&parent, &moved).unwrap();
    fs::create_dir(&parent).unwrap();

    assert!(store.append(task_started()).is_err());
    assert!(!parent.join("ledger.jsonl").exists());
    assert!(!moved.join("ledger.jsonl").exists());
}

#[cfg(unix)]
#[test]
fn created_parent_identity_is_pinned_for_later_operations() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    let parent = root.join("execution");
    let moved = root.join("execution-moved");
    fs::create_dir(&root).unwrap();
    let store = ExecutionLedgerStore::new_verified(&root, "execution/ledger.jsonl").unwrap();

    store.append(task_started()).unwrap();
    let before = fs::read(parent.join("ledger.jsonl")).unwrap();
    fs::rename(&parent, &moved).unwrap();
    fs::create_dir(&parent).unwrap();

    assert!(store.append(plan_created()).is_err());
    assert!(!parent.join("ledger.jsonl").exists());
    assert_eq!(fs::read(moved.join("ledger.jsonl")).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn default_data_root_creation_is_descriptor_relative_and_private() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("missing").join("data");

    ensure_default_data_root(&root).unwrap();

    assert!(root.is_dir());
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn legacy_relative_constructor_resolves_current_directory() {
    let store = ExecutionLedgerStore::new(Path::new("ledger.jsonl"));
    assert!(store.path().is_absolute());
    assert_eq!(
        store.path().file_name(),
        Some(std::ffi::OsStr::new("ledger.jsonl"))
    );
}

#[cfg(windows)]
#[test]
fn windows_ledger_preserves_verified_append_and_load() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    let store = ExecutionLedgerStore::new(&path);

    store.append(task_started()).unwrap();
    assert_eq!(store.load_verified().unwrap().len(), 1);
    assert!(store.verify_chain().unwrap());
}
