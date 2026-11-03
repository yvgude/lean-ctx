// SPDX-License-Identifier: Apache-2.0
//! Hive requests a tool call hands off (connector rule "Hive never blocks a
//! tool call").
//!
//! A tool call that needs Hive (`ctx_tasks`, the Hive side of a
//! `ctx_agent` lease) checks what it can locally, queues the request here
//! with [`submit`] and answers at once that it is pending. One low-priority
//! thread per process runs the queue in order, so a later request of the
//! same caller (a lease release after its acquire, an update after a
//! claim) always goes out after the earlier one. What Hive answered is kept
//! per caller and put in front of that caller's next answer of the same
//! tool ([`with_answers`]); nothing is presented as confirmed before Hive
//! confirmed it.
//!
//! The queue lives in this process: requests of a process that ends before
//! they ran are never sent, and the agent's session ended with it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// A queued request: runs off the tool call, returns the line for the caller.
type Job = Box<dyn FnOnce() -> Option<String> + Send>;

#[derive(Default)]
struct Queue {
    jobs: VecDeque<(String, Option<String>, Job)>,
    /// Caller → answers not yet shown.
    answers: BTreeMap<String, Vec<String>>,
    /// Dedupe keys of queued or running jobs ([`submit_once`]).
    keys: BTreeSet<String>,
    running: bool,
    started: bool,
}

static QUEUE: Mutex<Option<Queue>> = Mutex::new(None);
static WAKE: Condvar = Condvar::new();

fn queue() -> MutexGuard<'static, Option<Queue>> {
    QUEUE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn with_queue<T>(change: impl FnOnce(&mut Queue) -> T) -> T {
    let mut guard = queue();
    change(guard.get_or_insert_with(Queue::default))
}

/// Queue `job` for `caller` (e.g. `ctx_agent:<agent id>`). Its line, if
/// any, is shown with the caller's next answer.
pub fn submit(caller: &str, job: impl FnOnce() -> Option<String> + Send + 'static) {
    push(caller, None, Box::new(job));
}

/// [`submit`], unless a job with `key` is already queued or running (a
/// cache refresh asked for twice is fetched once). `false` when skipped.
pub fn submit_once(
    caller: &str,
    key: &str,
    job: impl FnOnce() -> Option<String> + Send + 'static,
) -> bool {
    if with_queue(|q| q.keys.contains(key)) {
        return false;
    }
    push(caller, Some(key.to_string()), Box::new(job));
    true
}

fn push(caller: &str, key: Option<String>, job: Job) {
    let start = with_queue(|q| {
        if let Some(key) = &key {
            q.keys.insert(key.clone());
        }
        q.jobs.push_back((caller.to_string(), key, job));
        !std::mem::replace(&mut q.started, true)
    });
    WAKE.notify_all();
    if start {
        let spawned = std::thread::Builder::new()
            .name("hive-calls".into())
            .spawn(run);
        if spawned.is_err() {
            // No thread: the requests stay queued, never run in the tool call.
            with_queue(|q| q.started = false);
        }
    }
}

fn run() {
    loop {
        let (caller, key, job) = {
            let mut guard = queue();
            loop {
                let q = guard.get_or_insert_with(Queue::default);
                if let Some(next) = q.jobs.pop_front() {
                    q.running = true;
                    break next;
                }
                q.running = false;
                WAKE.notify_all();
                guard = WAKE.wait(guard).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let line = job();
        with_queue(|q| {
            if let Some(key) = key {
                q.keys.remove(&key);
            }
            if let Some(line) = line {
                q.answers.entry(caller).or_default().push(line);
            }
        });
    }
}

/// The answers waiting for `caller`, taken (shown once).
pub fn take(caller: &str) -> Vec<String> {
    with_queue(|q| q.answers.remove(caller).unwrap_or_default())
}

/// `text` with the answers waiting for `caller` in front of it.
pub fn with_answers(caller: &str, text: String) -> String {
    let answers = take(caller);
    if answers.is_empty() {
        return text;
    }
    let mut out = String::from("Hive answered your earlier requests:\n");
    for answer in answers {
        out.push_str("- ");
        out.push_str(&answer);
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&text);
    out
}

/// Wait until every queued request has run (tests and CLI commands that
/// end the process); `false` when `timeout` passed first.
pub fn wait_idle(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut guard = queue();
    loop {
        let idle = guard
            .as_ref()
            .is_none_or(|q| q.jobs.is_empty() && !q.running);
        if idle {
            return true;
        }
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return false;
        };
        guard = WAKE
            .wait_timeout(guard, left)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_run_in_order_off_the_caller_and_answer_once() {
        let caller = "test:requests_run_in_order";
        let order = std::sync::Arc::new(Mutex::new(Vec::new()));
        for n in 0..3 {
            let order = std::sync::Arc::clone(&order);
            submit(caller, move || {
                order.lock().unwrap().push(n);
                Some(format!("answer {n}"))
            });
        }
        assert!(wait_idle(Duration::from_secs(10)));
        assert_eq!(*order.lock().unwrap(), vec![0, 1, 2]);
        let shown = with_answers(caller, "now".into());
        assert_eq!(
            shown,
            "Hive answered your earlier requests:\n- answer 0\n- answer 1\n- answer 2\n\nnow"
        );
        assert_eq!(with_answers(caller, "again".into()), "again", "shown once");
    }

    #[test]
    fn a_keyed_request_is_queued_once() {
        let caller = "test:keyed";
        let gate = std::sync::Arc::new((Mutex::new(false), Condvar::new()));
        let held = std::sync::Arc::clone(&gate);
        assert!(submit_once(caller, "test:key", move || {
            let (open, cv) = &*held;
            let mut open = open.lock().unwrap();
            while !*open {
                open = cv.wait(open).unwrap();
            }
            None
        }));
        assert!(!submit_once(caller, "test:key", || None), "already queued");
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert!(wait_idle(Duration::from_secs(10)));
        assert!(submit_once(caller, "test:key", || None), "done, so again");
        assert!(wait_idle(Duration::from_secs(10)));
    }
}
