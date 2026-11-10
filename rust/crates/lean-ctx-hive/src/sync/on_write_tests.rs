// SPDX-License-Identifier: Apache-2.0
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::*;

fn ms(base: Instant, millis: u64) -> Instant {
    base + Duration::from_millis(millis)
}

/// #270: a fact remembered on Instance A has to be on Instance B within
/// 10 s; the daemon pushed it only on its next pass, 32 s later on
/// staging. The writing process now pushes it within `COALESCE`.
#[test]
fn a_write_is_pushed_from_the_writing_process_within_a_second() {
    let t0 = Instant::now();
    let queue = Mutex::new(Queue::default());
    queue.lock().unwrap().request("/repo", t0);
    let mut pushed = Vec::new();
    let mut push = |root: &str| {
        pushed.push(root.to_string());
        Outcome::Done
    };
    let next = step(&queue, ms(t0, 100), &mut push);
    assert_eq!(next, Some(ms(t0, 300)), "coalescing");
    step(&queue, ms(t0, 300), &mut push);
    assert_eq!(pushed, ["/repo"]);
    assert!(COALESCE < Duration::from_secs(1));
}

#[test]
fn a_burst_of_writes_is_one_push() {
    let t0 = Instant::now();
    let queue = Mutex::new(Queue::default());
    for i in 0..20 {
        queue.lock().unwrap().request("/repo", ms(t0, i * 10));
    }
    let mut pushes = 0;
    step(&queue, ms(t0, 300), &mut |_| {
        pushes += 1;
        Outcome::Done
    });
    assert_eq!(pushes, 1);
    assert_eq!(queue.lock().unwrap().due(ms(t0, 300)), (Vec::new(), None));
}

#[test]
fn a_checkout_is_pushed_at_most_once_per_spacing() {
    let t0 = Instant::now();
    let queue = Mutex::new(Queue::default());
    let mut pushed = Vec::new();
    let mut push = |root: &str| {
        pushed.push(root.to_string());
        Outcome::Done
    };
    queue.lock().unwrap().request("/a", t0);
    step(&queue, ms(t0, 300), &mut push);
    // A write right after the push waits for the spacing; another
    // checkout does not.
    queue.lock().unwrap().request("/a", ms(t0, 400));
    queue.lock().unwrap().request("/b", ms(t0, 400));
    let next = step(&queue, ms(t0, 700), &mut push);
    assert_eq!(next, Some(ms(t0, 2_300)));
    step(&queue, ms(t0, 2_300), &mut push);
    assert_eq!(pushed, ["/a", "/b", "/a"]);
}

#[test]
fn a_locked_vault_is_tried_again_a_bounded_number_of_times() {
    let t0 = Instant::now();
    let queue = Mutex::new(Queue::default());
    queue.lock().unwrap().request("/repo", t0);
    let mut tries = 0u64;
    let mut now = ms(t0, 300);
    while let Some(next) = step(&queue, now, &mut |_| {
        tries += 1;
        Outcome::Busy
    }) {
        assert_eq!(next, now + SPACING, "no busy loop");
        now = next;
    }
    assert_eq!(tries, u64::from(LOCK_RETRIES) + 1);
    // A later write starts over.
    queue.lock().unwrap().request("/repo", now + SPACING);
    let mut done = 0;
    step(&queue, now + SPACING + COALESCE, &mut |_| {
        done += 1;
        Outcome::Done
    });
    assert_eq!(done, 1);
}

#[test]
fn a_skipped_push_is_not_retried() {
    let t0 = Instant::now();
    let queue = Mutex::new(Queue::default());
    queue.lock().unwrap().request("/repo", t0);
    let next = step(&queue, ms(t0, 300), &mut |_| Outcome::Skipped);
    assert_eq!(next, None);
}

/// Review of #270: a worker that panicked left `STARTED` set, and push on
/// write stayed off for the rest of the process without a word.
#[test]
fn a_worker_that_ends_by_panic_lets_the_next_write_start_one() {
    STARTED.store(true, Ordering::SeqCst);
    let ended = std::thread::spawn(|| {
        let _running = Running;
        panic!("a push panicked");
    })
    .join();
    assert!(ended.is_err());
    assert!(!STARTED.load(Ordering::SeqCst));
}
