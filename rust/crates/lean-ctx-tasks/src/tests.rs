// SPDX-License-Identifier: Apache-2.0
use chrono::{DateTime, Duration, TimeZone as _, Utc};

use lean_ctx_protocol::hive::tasks::{
    EvidenceKind, EvidenceV1, TaskOutcome, TaskState, TaskVerdict,
};

use crate::board::{Proposal, Submission};
use crate::{Actor, Board, Store, TaskError, TaskEvent};

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 4, 9, 0, 0)
        .single()
        .expect("fixed time")
}

fn proposal() -> Proposal {
    Proposal {
        title: "Fix the flaky login test".into(),
        goal: None,
        acceptance: "The test passes 50 runs in a row".into(),
        role: None,
    }
}

fn result(outcome: TaskOutcome) -> Submission {
    Submission {
        summary: "Fixed the race in the session fixture.".into(),
        evidence: vec![EvidenceV1 {
            kind: EvidenceKind::Commit,
            reference: Some("7be16a9596".into()),
            anchor: None,
        }],
        usage: None,
        outcome,
    }
}

fn refused(result: Result<impl std::fmt::Debug, TaskError>) -> &'static str {
    result
        .expect_err("the write is refused")
        .reason()
        .unwrap_or("invalid")
}

#[test]
fn a_task_runs_proposal_to_done_and_records_every_transition() {
    let (queen, worker, person) = (
        Actor::agent("queen"),
        Actor::agent("w1"),
        Actor::person("Yves"),
    );
    let mut board = Board::default();
    let n = board
        .propose(&worker, proposal(), t0())
        .expect("proposed")
        .number;
    // Anyone accepts, the proposer included; the proposer claims it too.
    board
        .decide(n, &worker, true, None, t0())
        .expect("accepted");
    assert!(
        board
            .claim(n, &worker, None, t0())
            .expect("claimed")
            .is_some()
    );
    board
        .update(n, &worker, "Reproduced it.", None, t0())
        .expect("update");
    let submitted = board
        .submit(n, &worker, result(TaskOutcome::Completed), t0())
        .expect("submitted");
    assert_eq!(submitted.worker.as_deref(), Some("w1"));
    // Never the worker; another agent rejects (open again), a person accepts.
    assert_eq!(
        refused(board.verify(n, &worker, TaskVerdict::Accept, None, t0())),
        "self_verify"
    );
    assert!(
        board
            .verify(n, &queen, TaskVerdict::Reject, None, t0())
            .is_err(),
        "a reject names its reason"
    );
    let rejected = board
        .verify(
            n,
            &queen,
            TaskVerdict::Reject,
            Some("The test still flakes."),
            t0(),
        )
        .expect("rejected");
    assert_eq!(rejected.state, TaskState::Open);
    board.claim(n, &worker, None, t0()).expect("claimed again");
    board
        .submit(n, &worker, result(TaskOutcome::Completed), t0())
        .expect("submitted again");
    let done = board
        .verify(n, &person, TaskVerdict::Accept, None, t0())
        .expect("verified");
    assert_eq!(done.state, TaskState::Done);

    let task = &board.tasks[&n];
    let events: Vec<_> = task.transitions.iter().map(|t| (t.event, t.to)).collect();
    assert_eq!(
        events,
        [
            (TaskEvent::Proposed, TaskState::Proposed),
            (TaskEvent::Accepted, TaskState::Open),
            (TaskEvent::Claimed, TaskState::Claimed),
            (TaskEvent::Updated, TaskState::Claimed),
            (TaskEvent::Submitted, TaskState::Submitted),
            (TaskEvent::Rejected, TaskState::Open),
            (TaskEvent::Claimed, TaskState::Claimed),
            (TaskEvent::Submitted, TaskState::Submitted),
            (TaskEvent::Verified, TaskState::Done),
        ]
    );
    assert_eq!(task.transitions[5].by.agent.as_deref(), Some("queen"));
    assert_eq!(
        task.task.verified_by.as_ref().map(|a| a.label.as_str()),
        Some("Yves")
    );
    assert_eq!(
        task.task
            .approved_by
            .as_ref()
            .and_then(|a| a.agent.as_deref()),
        Some("w1")
    );
    // A closed task takes no further write.
    assert_eq!(refused(board.claim(n, &queen, None, t0())), "task_closed");
}

#[test]
fn the_rules_refuse_who_may_not_act() {
    let (creator, other, person) = (
        Actor::agent("a1"),
        Actor::agent("a2"),
        Actor::person("Yves"),
    );
    let mut board = Board::default();
    let n = board
        .propose(&creator, proposal(), t0())
        .expect("proposed")
        .number;
    assert_eq!(refused(board.claim(n, &other, None, t0())), "task_not_open");
    board
        .decide(n, &person, true, None, t0())
        .expect("a person accepts");
    assert_eq!(
        refused(board.decide(n, &other, false, None, t0())),
        "not_proposed"
    );
    assert_eq!(refused(board.claim(n, &person, None, t0())), "agents_claim");
    board.claim(n, &creator, None, t0()).expect("claimed");
    assert_eq!(refused(board.claim(n, &other, None, t0())), "task_claimed");
    assert_eq!(
        refused(board.update(n, &other, "Mine now.", None, t0())),
        "not_holder"
    );
    assert_eq!(refused(board.release(n, &other, None, t0())), "not_holder");
    // Only a person stops a task, never an agent (its creator neither),
    // and only with a reason.
    assert_eq!(
        refused(board.stop(n, &creator, Some("Not needed."), t0())),
        "agents_stop"
    );
    assert!(matches!(
        board.stop(n, &person, None, t0()),
        Err(crate::TaskError::Invalid(_))
    ));
    // A person may release a stuck claim and stop the task.
    board
        .release(n, &person, Some("Stuck."), t0())
        .expect("released by a person");
    board
        .stop(n, &person, Some("Not needed."), t0())
        .expect("stopped by a person");

    let m = board
        .propose(&person, proposal(), t0())
        .expect("proposed")
        .number;
    let declined = board
        .decide(m, &other, false, Some("Duplicate of #1."), t0())
        .expect("declined");
    assert_eq!(declined.state, TaskState::Rejected);

    let f = board
        .propose(&creator, proposal(), t0())
        .expect("proposed")
        .number;
    board
        .decide(f, &creator, true, None, t0())
        .expect("accepted");
    board.claim(f, &other, None, t0()).expect("claimed");
    let failed = board
        .submit(f, &other, result(TaskOutcome::Failed), t0())
        .expect("failed");
    assert_eq!(
        (failed.event, failed.state),
        (TaskEvent::Failed, TaskState::Failed)
    );
    // Hive's text rules hold: a two-line summary is no summary.
    let g = board
        .propose(&creator, proposal(), t0())
        .expect("proposed")
        .number;
    board
        .decide(g, &creator, true, None, t0())
        .expect("accepted");
    board.claim(g, &other, None, t0()).expect("claimed");
    assert!(matches!(
        board.update(g, &other, "line one\nline two", None, t0()),
        Err(TaskError::Invalid(_))
    ));
}

#[test]
fn a_claim_that_runs_out_opens_the_task_and_work_renews_it() {
    let worker = Actor::agent("w1");
    let mut board = Board::default();
    let n = board
        .propose(&worker, proposal(), t0())
        .expect("proposed")
        .number;
    board
        .decide(n, &worker, true, None, t0())
        .expect("accepted");
    board.claim(n, &worker, Some(10), t0()).expect("claimed");
    // An update at minute 8 renews the claim for 10 more minutes.
    board
        .update(n, &worker, "Halfway.", None, t0() + Duration::minutes(8))
        .expect("update");
    assert!(board.expire(t0() + Duration::minutes(15)).is_empty());
    let expired = board.expire(t0() + Duration::minutes(19));
    assert_eq!(expired.len(), 1);
    assert_eq!(
        (expired[0].event, expired[0].state),
        (TaskEvent::Released, TaskState::Open)
    );
    assert!(board.tasks[&n].task.claim.is_none());
}

#[test]
fn concurrent_writers_lose_no_transition() {
    let dir = tempfile::tempdir().expect("temp dir");
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let store = Store::new(dir.path());
            std::thread::spawn(move || {
                for _ in 0..5 {
                    store
                        .write(t0(), |board| {
                            board.propose(&Actor::agent(format!("a{i}")), proposal(), t0())
                        })
                        .expect("proposed");
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().expect("writer");
    }
    let store = Store::new(dir.path());
    let board = store.load().expect("board");
    assert_eq!(board.tasks.len(), 40);
    assert_eq!(
        board.tasks.keys().copied().collect::<Vec<_>>(),
        (1..=40).collect::<Vec<_>>()
    );
    let seqs: Vec<u64> = store.journal().iter().map(|t| t.seq).collect();
    assert_eq!(seqs, (1..=40).collect::<Vec<_>>());
}

/// Hive task #39: a failed bus post leaves its notices due, the next access
/// posts them, and none is lost or posted twice. Posts run outside the
/// lock, so two writers' posts can overlap; each order is run here
/// deterministically by writing the second inside the first one's post.
#[test]
fn a_failed_post_is_retried_and_no_notice_is_lost() {
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::new(dir.path());
    let posted = RefCell::new(BTreeSet::new());
    let ok = |changes: &[crate::Change]| {
        posted.borrow_mut().extend(changes.iter().map(|c| c.seq));
        true
    };
    let fail = |_: &[crate::Change]| false;
    let agent = Actor::agent("a");
    let propose = |b: &mut Board| b.propose(&agent, proposal(), t0()).map(|_| ());
    let seqs = |to: u64| (1..=to).collect::<BTreeSet<u64>>();

    // A failed post: the next access posts its notice.
    store.write_announced(t0(), propose, fail).expect("1");
    assert!(posted.borrow().is_empty());
    store.read_announced(t0(), ok).expect("read");
    assert_eq!(*posted.borrow(), seqs(1));

    // A fails while B succeeds: B posts A's notice with its own.
    store
        .write_announced(t0(), propose, |_| {
            store.write_announced(t0(), propose, ok).expect("3");
            false
        })
        .expect("2");
    assert_eq!(*posted.borrow(), seqs(3));

    // A succeeds after B failed: A does not move the cursor past B.
    store
        .write_announced(t0(), propose, |changes| {
            store.write_announced(t0(), propose, fail).expect("5");
            ok(changes)
        })
        .expect("4");
    assert_eq!(*posted.borrow(), seqs(4));
    store.read_announced(t0(), ok).expect("read");
    assert_eq!(*posted.borrow(), seqs(5));

    // B succeeds before A: A never moves the cursor back, nothing is due.
    store
        .write_announced(t0(), propose, |changes| {
            store.write_announced(t0(), propose, ok).expect("7");
            ok(changes)
        })
        .expect("6");
    store
        .read_announced(t0(), |changes| panic!("posted twice: {changes:?}"))
        .expect("read");
    let journal: BTreeSet<u64> = store.journal().iter().map(|t| t.seq).collect();
    assert_eq!((&*posted.borrow(), &journal), (&seqs(7), &seqs(7)));
}

/// The bus is posted to after `tasks.lock` is released, so a slow or
/// contended bus never holds up the next task writer.
#[test]
fn notices_are_posted_outside_the_task_lock() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::new(dir.path());
    let mut free = None;
    store
        .write_announced(
            t0(),
            |b| b.propose(&Actor::agent("a"), proposal(), t0()),
            |_| {
                let lock = std::fs::File::open(dir.path().join("tasks.lock")).expect("lock file");
                free = Some(fs2::FileExt::try_lock_exclusive(&lock).is_ok());
                let _ = fs2::FileExt::unlock(&lock);
                true
            },
        )
        .expect("proposed");
    assert_eq!(free, Some(true), "tasks.lock was held while posting");
}

/// A notice cursor that cannot be saved refuses the write: the transition
/// is never journaled without a cursor a failed post could retry from.
#[test]
fn a_write_whose_notice_cursor_cannot_be_saved_is_refused() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join("notices.json")).expect("block the cursor");
    let store = Store::new(dir.path());
    let written = store.write_announced(
        t0(),
        |b| b.propose(&Actor::agent("a"), proposal(), t0()),
        |_| true,
    );
    assert!(written.is_err());
    assert!(store.journal().is_empty());
    assert!(store.load().expect("board").tasks.is_empty());
}

#[test]
fn a_read_gives_back_expired_claims_and_a_task_is_shared_once() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::new(dir.path());
    let worker = Actor::agent("w1");
    let (n, _) = store
        .write(t0(), |board| {
            let n = board.propose(&worker, proposal(), t0())?.number;
            board.decide(n, &worker, true, None, t0())?;
            board.claim(n, &worker, Some(5), t0())?;
            Ok(n)
        })
        .expect("claimed");
    let (board, expired) = store.read(t0() + Duration::minutes(6)).expect("read");
    assert_eq!(board.tasks[&n].task.state, TaskState::Open);
    assert_eq!(expired.len(), 1);
    assert!(
        store
            .read(t0() + Duration::minutes(7))
            .expect("read")
            .1
            .is_empty()
    );

    let person = Actor::person("Yves");
    store
        .write(t0(), |board| {
            board.mark_shared(n, &person, "acme/app", 7, t0())
        })
        .expect("shared");
    let again = store.write(t0(), |board| {
        board.mark_shared(n, &person, "acme/app", 8, t0())
    });
    assert_eq!(refused(again), "already_shared");
    let shared = store.load().expect("board").tasks[&n]
        .shared
        .clone()
        .expect("shared ref");
    assert_eq!((shared.vault.as_str(), shared.number), ("acme/app", 7));
}

#[test]
fn priority_and_labels_count_their_version_and_a_closed_task_keeps_them() {
    use lean_ctx_protocol::hive::tasks::TaskPriority;
    let (person, worker) = (Actor::person("Yves"), Actor::agent("w1"));
    let mut board = Board::default();
    let n = board
        .propose(&worker, proposal(), t0())
        .expect("proposed")
        .number;
    let change = board
        .set_fields(
            n,
            &person,
            Some(TaskPriority::High),
            Some(vec!["core".into(), "p1".into()]),
            t0(),
        )
        .expect("set");
    assert_eq!(change.event, TaskEvent::Fields);
    assert_eq!(change.state, TaskState::Proposed, "no state change");
    let task = &board.tasks[&n].task;
    assert_eq!(task.priority, Some(TaskPriority::High));
    assert_eq!(task.labels, ["core", "p1"]);
    assert_eq!(task.fields_version, Some(1));
    // Labels only: the priority stays, the list is replaced.
    board
        .set_fields(n, &person, None, Some(Vec::new()), t0())
        .expect("cleared");
    let task = &board.tasks[&n].task;
    assert_eq!(
        (task.priority, task.labels.len(), task.fields_version),
        (Some(TaskPriority::High), 0, Some(2))
    );
    let last = board.tasks[&n].transitions.last().expect("journaled");
    assert_eq!(last.reason.as_deref(), Some("priority high, labels none"));
    // A wrong value or nothing to set is refused before anything changes.
    assert_eq!(
        refused(board.set_fields(n, &person, None, None, t0())),
        "invalid"
    );
    assert_eq!(
        refused(board.set_fields(n, &person, None, Some(vec!["Core".into()]), t0())),
        "invalid"
    );
    assert_eq!(board.tasks[&n].task.fields_version, Some(2));
    assert_eq!(
        refused(board.set_fields(99, &person, Some(TaskPriority::Normal), None, t0())),
        "invalid",
        "an unknown task"
    );
    board
        .decide(n, &person, false, Some("not now"), t0())
        .expect("declined");
    assert_eq!(
        refused(board.set_fields(n, &person, Some(TaskPriority::Normal), None, t0())),
        "task_closed"
    );
}

fn commit_of(sha: &str) -> EvidenceV1 {
    EvidenceV1 {
        kind: EvidenceKind::Commit,
        reference: Some(sha.into()),
        anchor: None,
    }
}

fn merge_of(sha: &str) -> crate::board::Amendment {
    crate::board::Amendment {
        merge_commit: Some(sha.into()),
        ..crate::board::Amendment::default()
    }
}

/// A task `w1` submitted (7be16a9596) and `rev` accepted: `done`.
fn done_board(worker: &Actor, reviewer: &Actor) -> (Board, u64) {
    let mut board = Board::default();
    let n = board
        .propose(worker, proposal(), t0())
        .expect("proposed")
        .number;
    board.decide(n, worker, true, None, t0()).expect("accepted");
    board.claim(n, worker, None, t0()).expect("claimed");
    board
        .submit(n, worker, result(TaskOutcome::Completed), t0())
        .expect("submitted");
    board
        .verify(n, reviewer, TaskVerdict::Accept, None, t0())
        .expect("done");
    (board, n)
}

#[test]
fn only_the_submitter_amends_a_result_and_the_history_keeps_old_and_new() {
    use crate::board::Amendment;
    use lean_ctx_protocol::hive::tasks::TaskUpdateKind;
    let amend = |summary: Option<&str>, shas: &[&str]| Amendment {
        summary: summary.map(str::to_owned),
        evidence: shas.iter().map(|sha| commit_of(sha)).collect(),
        ..Amendment::default()
    };
    let (worker, other, reviewer, person) = (
        Actor::agent("w1"),
        Actor::agent("w2"),
        Actor::agent("rev"),
        Actor::person("Yves"),
    );
    let mut board = Board::default();
    let n = board
        .propose(&worker, proposal(), t0())
        .expect("proposed")
        .number;
    board
        .decide(n, &worker, true, None, t0())
        .expect("accepted");
    // A claimed task has no result to amend yet.
    board.claim(n, &worker, None, t0()).expect("claimed");
    assert_eq!(
        refused(board.amend(n, &worker, amend(None, &["409c9eec05"]), t0())),
        "not_submitter"
    );
    board
        .submit(n, &worker, result(TaskOutcome::Completed), t0())
        .expect("submitted");
    // A correction: never another agent, a person or a Queen session.
    let queen_fix = Amendment {
        queen: true,
        ..amend(None, &["409c9eec05"])
    };
    for (who, amendment) in [
        (&other, amend(None, &["409c9eec05"])),
        (&person, amend(None, &["409c9eec05"])),
        (&other, queen_fix),
    ] {
        assert_eq!(
            refused(board.amend(n, who, amendment, t0())),
            "not_submitter"
        );
    }
    assert_eq!(board.tasks[&n].task.updates.len(), 0);
    // A submitted task is not merged yet.
    assert_eq!(
        refused(board.amend(n, &worker, merge_of("0802f8f633"), t0())),
        "not_merged_yet"
    );
    // Submitted: the submitter replaces evidence and summary, no claim needed.
    let change = board
        .amend(
            n,
            &worker,
            amend(Some("Ported to the new head."), &["409c9eec05"]),
            t0(),
        )
        .expect("amended");
    assert_eq!(
        (change.event, change.state),
        (TaskEvent::Amended, TaskState::Submitted)
    );
    let task = &board.tasks[&n].task;
    let result_now = task.result.as_ref().expect("result");
    assert_eq!(
        result_now.summary.as_deref(),
        Some("Ported to the new head.")
    );
    assert_eq!(
        result_now.evidence[0].reference.as_deref(),
        Some("409c9eec05")
    );
    let update = task.updates.last().expect("amendment in the history");
    assert_eq!(update.kind, Some(TaskUpdateKind::Amendment));
    assert_eq!(
        (
            update.amended_from[0].reference.as_deref(),
            update.amended_to[0].reference.as_deref()
        ),
        (Some("7be16a9596"), Some("409c9eec05")),
        "old → new"
    );
    // Done: only a merge record.
    board
        .verify(n, &reviewer, TaskVerdict::Accept, None, t0())
        .expect("accepted");
    assert_eq!(
        refused(board.amend(n, &worker, amend(None, &["409c9eec05", "0802f8f633"]), t0())),
        "record_merge_only"
    );
    board
        .amend(n, &worker, merge_of("0802f8f633"), t0())
        .expect("the submitter records the merge");
    let task = &board.tasks[&n].task;
    assert_eq!(task.state, TaskState::Done);
    let result_now = task.result.as_ref().expect("result");
    assert_eq!(
        result_now.merge.as_ref().map(|m| m.commit.as_str()),
        Some("0802f8f633")
    );
    assert_eq!(result_now.evidence.len(), 1, "the evidence stays");
    let update = task.updates.last().expect("merge record in the history");
    assert_eq!(
        (
            update.amended_from[0].reference.as_deref(),
            update.amended_to[0].reference.as_deref()
        ),
        (Some("409c9eec05"), Some("0802f8f633")),
        "head → merge"
    );
    assert_eq!(
        board.tasks[&n].transitions.last().map(|t| t.event),
        Some(TaskEvent::Amended)
    );
    // Once: a second record changes nothing.
    let recorded = board.tasks[&n].task.updates.len();
    assert_eq!(
        refused(board.amend(n, &person, merge_of("1234567abc"), t0())),
        "merge_recorded"
    );
    assert_eq!(board.tasks[&n].task.updates.len(), recorded);
}

/// The Queen's rule: a merge record comes from the submitter, a person or
/// a registered Queen session; never the reviewer (checker and merger stay
/// separate), never another agent.
#[test]
fn a_merge_is_recorded_by_the_submitter_a_person_or_a_queen_never_the_reviewer() {
    let (worker, reviewer, other) = (Actor::agent("w1"), Actor::agent("rev"), Actor::agent("w2"));
    let queen_merge = |sha: &str| crate::board::Amendment {
        queen: true,
        ..merge_of(sha)
    };
    for (who, amendment) in [
        (&worker, merge_of("6aded74fcf")),
        (&Actor::person("Yves"), merge_of("6aded74fcf")),
        (&Actor::agent("queen"), queen_merge("6aded74fcf")),
    ] {
        let (mut board, n) = done_board(&worker, &reviewer);
        board
            .amend(n, who, amendment, t0())
            .expect("the merge is recorded");
        let task = &board.tasks[&n].task;
        assert_eq!(
            task.result
                .as_ref()
                .and_then(|r| r.merge.as_ref())
                .map(|m| m.commit.as_str()),
            Some("6aded74fcf")
        );
        assert_eq!(task.updates.len(), 2, "the verdict, then one merge entry");
    }
    let (mut board, n) = done_board(&worker, &reviewer);
    assert_eq!(
        refused(board.amend(n, &reviewer, merge_of("6aded74fcf"), t0())),
        "not_merger"
    );
    // A Queen session that reviewed the task stays the reviewer.
    assert_eq!(
        refused(board.amend(n, &reviewer, queen_merge("6aded74fcf"), t0())),
        "not_merger"
    );
    assert_eq!(
        refused(board.amend(n, &other, merge_of("6aded74fcf"), t0())),
        "not_submitter"
    );
    // A person who reviewed is the reviewer too; another person is not.
    let person = Actor::person("Yves");
    let (mut by_person, m) = done_board(&worker, &person);
    assert_eq!(
        refused(by_person.amend(m, &person, merge_of("6aded74fcf"), t0())),
        "not_merger"
    );
    by_person
        .amend(m, &Actor::person("Ada"), merge_of("6aded74fcf"), t0())
        .expect("another person records it");
    // A merge record is a commit SHA.
    assert_eq!(
        refused(board.amend(n, &worker, merge_of("not a sha"), t0())),
        "invalid"
    );
    assert_eq!(
        board.tasks[&n].task.updates.len(),
        1,
        "only the verdict: nothing written"
    );
}
