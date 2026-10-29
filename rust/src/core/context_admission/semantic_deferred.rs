// SPDX-License-Identifier: Apache-2.0
//! Deferred full semantic check for stored context (AP11 owner decision,
//! 2026-10-19, variant c).
//!
//! A live read keeps the semantic budget of one call (`ANSWER_DEADLINE_MS`)
//! and reports a longer text as `semantic.partial`. Stored context — what a
//! derived store admits on write and what a stored copy is re-admitted with
//! when it is served — is checked the same way at first, and a partial run
//! is queued here. While the MCP server is idle (`server::mcp_idle`, the
//! existing idle hook) the queue is drained: the whole text is asked for in
//! successive bounded calls, each with the unchanged per-call budget. The
//! complete answer is kept as a verdict (spans and examined categories, no
//! text) for exactly that text and runtime. The next admission of the same
//! text in a stored context uses it, so its findings take effect through the
//! existing vocabulary: `semantic.<category>.redacted` masks,
//! `semantic.<category>.blocked` withholds, an unexamined category stays
//! `semantic.<category>.not_inspected`, and `semantic.partial` disappears.
//!
//! Advisory as before: without a runtime nothing is queued; a run that fails
//! leaves no verdict, and the text stays as partially inspected as it was.
//! The queue and the verdicts are bounded and live in this process only.
//! Nothing here is recorded or sent anywhere.

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};

use super::semantic::{SemanticDetector, inspectable_prefix};
use crate::core::intelligence_runtime::semantic_detectors::{
    CategorySet, SemanticFinding, SemanticReport, continuation_start,
};

/// Texts waiting for a full check, and their bytes together.
const MAX_QUEUED: usize = 16;
const MAX_QUEUED_BYTES: usize = 1024 * 1024;
/// A longer stored text is not queued; it stays partially inspected.
const MAX_TEXT_BYTES: usize = 512 * 1024;
/// Verdicts kept; the oldest is dropped first.
const MAX_VERDICTS: usize = 256;
/// Bounded calls per text (each within the per-call budget).
const MAX_PIECES: usize = 256;

thread_local! {
    static STORED: Cell<bool> = const { Cell::new(false) };
}

/// Run `admit` as an admission of stored context: a partial semantic run is
/// queued for the full check, and a verdict already reached is applied.
pub(crate) fn for_stored_context<R>(admit: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            STORED.with(|stored| stored.set(self.0));
        }
    }
    let _restore = Restore(STORED.with(|stored| stored.replace(true)));
    admit()
}

pub(super) fn in_stored_context() -> bool {
    STORED.with(Cell::get)
}

/// One text under one runtime: a verdict never outlives a settings change.
pub(super) type Key = [u8; 32];

pub(super) fn key(detect: &SemanticDetector, text: &str) -> Key {
    let mut hasher = blake3::Hasher::new();
    hasher.update(detect.identity().to_string().as_bytes());
    hasher.update(&[0]);
    hasher.update(text.as_bytes());
    *hasher.finalize().as_bytes()
}

/// A complete answer over the whole text.
#[derive(Debug, Clone)]
pub(super) struct Verdict {
    pub(super) report: SemanticReport,
    pub(super) examined: CategorySet,
}

struct Job {
    key: Key,
    detect: SemanticDetector,
    text: String,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Job>,
    queued_bytes: usize,
    verdicts: HashMap<Key, Verdict>,
    order: VecDeque<Key>,
}

#[cfg(not(test))]
fn with_state<R>(work: impl FnOnce(&mut State) -> R) -> R {
    use std::sync::{Mutex, OnceLock, PoisonError};
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    work(
        &mut STATE
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
    )
}

/// Per test thread, so that parallel tests never share a queue.
#[cfg(test)]
fn with_state<R>(work: impl FnOnce(&mut State) -> R) -> R {
    thread_local! {
        static STATE: std::cell::RefCell<State> = std::cell::RefCell::default();
    }
    STATE.with(|state| work(&mut state.borrow_mut()))
}

pub(super) fn verdict(key: &Key) -> Option<Verdict> {
    with_state(|state| state.verdicts.get(key).cloned())
}

/// Queue `text` for a full check. A full queue or an oversized text is not
/// queued: the text then stays partially inspected, as it was.
pub(super) fn enqueue(key: Key, detect: &SemanticDetector, text: &str) {
    if text.len() > MAX_TEXT_BYTES {
        return;
    }
    with_state(|state| {
        if state.verdicts.contains_key(&key)
            || state.queue.iter().any(|job| job.key == key)
            || state.queue.len() >= MAX_QUEUED
            || state.queued_bytes + text.len() > MAX_QUEUED_BYTES
        {
            return;
        }
        state.queued_bytes += text.len();
        state.queue.push_back(Job {
            key,
            detect: detect.clone(),
            text: text.to_owned(),
        });
    });
}

/// Whether a full check is waiting.
pub(crate) fn pending() -> bool {
    with_state(|state| !state.queue.is_empty())
}

enum Stopped {
    /// `keep_going` said stop: the job goes back to the front of the queue.
    Yielded,
    /// The runtime failed or made no progress: no verdict (advisory).
    Failed,
}

/// Run up to `max_jobs` queued full checks. `keep_going` is asked before
/// every call to the runtime; the idle hook passes "no tool call in flight",
/// so a live read is never kept waiting by more than one bounded call. One
/// drain at a time per process. Returns the verdicts reached.
pub(crate) fn drain(max_jobs: usize, keep_going: impl Fn() -> bool) -> usize {
    static DRAINING: AtomicBool = AtomicBool::new(false);
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            DRAINING.store(false, Ordering::Release);
        }
    }
    if DRAINING.swap(true, Ordering::AcqRel) {
        return 0;
    }
    let _release = Release;
    let mut reached = 0;
    for _ in 0..max_jobs {
        let Some(job) = with_state(|state| {
            let job = state.queue.pop_front()?;
            state.queued_bytes -= job.text.len();
            Some(job)
        }) else {
            break;
        };
        match full_check(&job.detect, &job.text, &keep_going) {
            Ok(verdict) => {
                reached += 1;
                with_state(|state| {
                    if state.verdicts.insert(job.key, verdict).is_none() {
                        state.order.push_back(job.key);
                    }
                    while state.order.len() > MAX_VERDICTS {
                        if let Some(oldest) = state.order.pop_front() {
                            state.verdicts.remove(&oldest);
                        }
                    }
                });
            }
            Err(Stopped::Yielded) => {
                with_state(|state| {
                    state.queued_bytes += job.text.len();
                    state.queue.push_front(job);
                });
                break;
            }
            Err(Stopped::Failed) => {}
        }
    }
    reached
}

/// Ask for the whole text in successive line-aligned pieces. A piece after
/// the first starts a little earlier for context (as the runtime's own
/// continuation does); only findings from the resume point on are taken.
fn full_check(
    detect: &SemanticDetector,
    text: &str,
    keep_going: &impl Fn() -> bool,
) -> Result<Verdict, Stopped> {
    let mut offset = 0;
    let mut findings = Vec::new();
    let mut examined = CategorySet::ALL;
    for _ in 0..MAX_PIECES {
        if offset >= text.len() {
            return Ok(Verdict {
                report: SemanticReport {
                    bytes_inspected: text.len(),
                    findings,
                },
                examined,
            });
        }
        if !keep_going() {
            return Err(Stopped::Yielded);
        }
        let with_context = if offset == 0 {
            0
        } else {
            continuation_start(text, offset)
        };
        // A piece whose context alone uses up the call is asked again
        // from the resume point itself.
        let mut answer = None;
        let starts = if with_context == offset {
            vec![offset]
        } else {
            vec![with_context, offset]
        };
        for from in starts {
            if from != with_context && !keep_going() {
                return Err(Stopped::Yielded);
            }
            let piece = inspectable_prefix(&text[from..], usize::MAX);
            if piece.is_empty() {
                return Err(Stopped::Failed);
            }
            let (report, piece_examined) = detect.inspect(piece).map_err(|_| Stopped::Failed)?;
            let end = from + report.bytes_inspected;
            if end > text.len() || !text.is_char_boundary(end) {
                return Err(Stopped::Failed);
            }
            if end > offset {
                answer = Some((from, end, report, piece_examined));
                break;
            }
        }
        let Some((from, end, report, piece_examined)) = answer else {
            return Err(Stopped::Failed);
        };
        findings.extend(
            report
                .findings
                .into_iter()
                .filter(|finding| from + finding.start >= offset)
                .map(|finding| SemanticFinding {
                    start: from + finding.start,
                    end: from + finding.end,
                    ..finding
                }),
        );
        examined = examined.intersection(piece_examined);
        offset = end;
    }
    Err(Stopped::Failed)
}
