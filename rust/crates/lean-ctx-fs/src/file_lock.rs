// SPDX-License-Identifier: Apache-2.0
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

const RETRY_INTERVAL: Duration = Duration::from_millis(10);

pub fn is_contended(error: &Error) -> bool {
    error.kind() == ErrorKind::WouldBlock
        || error
            .raw_os_error()
            .zip(fs2::lock_contended_error().raw_os_error())
            .is_some_and(|(error_code, contended_code)| error_code == contended_code)
}

/// Takes an exclusive advisory lock, giving up after `timeout` rather than
/// waiting for as long as the other holder cares to keep it.
///
/// `fs2::FileExt::lock_exclusive` has no deadline at all. On a thread whose only
/// job is that one write, an unbounded wait is merely slow. Anywhere else it is
/// a wedge: the waiter keeps everything it already owns — a `tokio` guard, an
/// executor worker — for the entire wait, and nothing in the process can end it
/// (#1783). A caller that cannot afford to wait forever could not express that
/// before this; now the bound is the default and the failure is an ordinary
/// `Err` the caller can retry on its own schedule.
pub fn acquire_exclusive_timeout(file: &File, timeout: Duration) -> Result<(), String> {
    use fs2::FileExt;

    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(error) if is_contended(&error) => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "still held after {}ms — giving up rather than blocking",
                        timeout.as_millis()
                    ));
                }
                std::thread::sleep(RETRY_INTERVAL);
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

/// One thread's turn at a lock path within this process; the next waiter in
/// line gets it when this drops.
///
/// An advisory file lock polled with a deadline is no queue: threads of one
/// process that take the same lock back to back make a waiter's deadline count
/// every critical section ahead of it, so eight writers whose writes each take
/// a second (an `fsync` plus a Windows replace that waits out a scanner) fail
/// the last one after five seconds although every holder made progress. The
/// turn queues the threads of a process in arrival order, and a waiter's
/// `timeout` counts only time in which no turn was handed over: a holder that
/// never returns still fails every waiter after `timeout`, a queue that moves
/// fails nobody. Other processes are still bounded by the file lock itself.
pub struct Turn {
    path: PathBuf,
    queue: Arc<Queue>,
}

#[derive(Default)]
struct Queue {
    line: Mutex<Line>,
    moved: Condvar,
}

#[derive(Default)]
struct Line {
    /// The next ticket handed out, and the ticket whose turn it is.
    next: u64,
    serving: u64,
    /// Tickets whose waiters gave up before their turn came.
    abandoned: BTreeSet<u64>,
}

impl Line {
    fn advance(&mut self) {
        self.serving += 1;
        while self.abandoned.remove(&self.serving) {
            self.serving += 1;
        }
    }

    fn idle(&self) -> bool {
        self.serving == self.next
    }
}

fn queues() -> &'static Mutex<HashMap<PathBuf, Arc<Queue>>> {
    static QUEUES: OnceLock<Mutex<HashMap<PathBuf, Arc<Queue>>>> = OnceLock::new();
    QUEUES.get_or_init(Mutex::default)
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Wait for this thread's turn at `path` among the threads of this process.
/// `None` once `timeout` passed without any turn being handed over.
///
/// The queue is keyed on `path` exactly as spelled, not canonicalized: two
/// spellings of one lock file (relative and absolute, through a symlink) get
/// two queues, and their callers fall back to contending on the file lock
/// alone, as before the turn existed. Callers pass one derived path per lock.
#[must_use]
pub fn take_turn(path: &Path, timeout: Duration) -> Option<Turn> {
    // Lock order everywhere: the map, then a queue's line. The ticket is drawn
    // under the map lock so a releasing holder never drops a queue in use.
    let (queue, ticket) = {
        let mut map = locked(queues());
        let queue = Arc::clone(map.entry(path.to_path_buf()).or_default());
        let mut line = locked(&queue.line);
        let ticket = line.next;
        line.next += 1;
        drop(line);
        drop(map);
        (queue, ticket)
    };
    let mut line = locked(&queue.line);
    let mut seen = line.serving;
    let mut deadline = Instant::now() + timeout;
    while line.serving != ticket {
        let now = Instant::now();
        if line.serving != seen {
            seen = line.serving;
            deadline = now + timeout;
        }
        let left = deadline.saturating_duration_since(now);
        if left.is_zero() {
            line.abandoned.insert(ticket);
            return None;
        }
        line = queue
            .moved
            .wait_timeout(line, left)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
    drop(line);
    Some(Turn {
        path: path.to_path_buf(),
        queue,
    })
}

impl Drop for Turn {
    fn drop(&mut self) {
        let mut map = locked(queues());
        let mut line = locked(&self.queue.line);
        line.advance();
        if line.idle() {
            map.remove(&self.path);
        }
        drop(line);
        drop(map);
        self.queue.moved.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::{acquire_exclusive_timeout, is_contended, locked, queues, take_turn};
    use std::io::{Error, ErrorKind};
    use std::path::Path;
    use std::time::{Duration, Instant};

    /// The next ticket `take_turn` would hand out at `path`, 0 without a queue.
    fn tickets_drawn(path: &Path) -> u64 {
        locked(queues())
            .get(path)
            .map_or(0, |queue| locked(&queue.line).next)
    }

    fn wait_for_tickets(path: &Path, drawn: u64) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while tickets_drawn(path) < drawn {
            assert!(Instant::now() < deadline, "ticket {drawn} never drawn");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A waiter that gives up in the middle of the line leaves an abandoned
    /// ticket. The holder's release must skip it and hand the turn to the
    /// waiter behind it at once; without the skip that waiter would wait out
    /// its own timeout. The idle queue is then removed from the map.
    #[test]
    fn an_abandoned_ticket_is_skipped_and_the_idle_queue_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("abandoned.lock");

        let holder = take_turn(&path, Duration::from_secs(1)).expect("free turn");
        let quitter = {
            let path = path.clone();
            std::thread::spawn(move || take_turn(&path, Duration::from_millis(100)).is_none())
        };
        wait_for_tickets(&path, 2);
        let patient = {
            let path = path.clone();
            std::thread::spawn(move || {
                let turn = take_turn(&path, Duration::from_secs(30));
                (turn.is_some(), Instant::now())
            })
        };
        wait_for_tickets(&path, 3);

        assert!(
            quitter.join().expect("quitter"),
            "the middle waiter must give up while the holder keeps its turn"
        );
        let released = Instant::now();
        drop(holder);
        let (got_turn, served) = patient.join().expect("patient");

        assert!(
            got_turn,
            "the waiter behind an abandoned ticket gets its turn"
        );
        let handover = served.saturating_duration_since(released);
        assert!(
            handover < Duration::from_secs(5),
            "the abandoned ticket must be skipped, handover took {handover:?}"
        );
        assert!(
            !locked(queues()).contains_key(&path),
            "an idle queue must leave the map"
        );
    }

    #[test]
    fn recognizes_fs2_contention_sentinel() {
        assert!(is_contended(&fs2::lock_contended_error()));
    }

    #[test]
    fn rejects_unrelated_errors() {
        assert!(!is_contended(&Error::new(
            ErrorKind::PermissionDenied,
            "unrelated",
        )));
    }

    #[test]
    fn gives_up_instead_of_waiting_for_a_held_lock() {
        use fs2::FileExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("contended.lock");
        let holder = std::fs::File::create(&path).expect("create holder");
        holder.lock_exclusive().expect("hold the lock");

        let waiter = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open waiter");
        let started = Instant::now();
        let result = acquire_exclusive_timeout(&waiter, Duration::from_millis(120));
        let waited = started.elapsed();

        assert!(
            result.is_err(),
            "must not acquire a lock someone else holds"
        );
        assert!(
            waited < Duration::from_secs(5),
            "must return on its own deadline, waited {waited:?}"
        );
        FileExt::unlock(&holder).expect("release");
    }

    #[test]
    fn takes_a_free_lock_immediately() {
        use fs2::FileExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let file = std::fs::File::create(dir.path().join("free.lock")).expect("create");

        acquire_exclusive_timeout(&file, Duration::from_millis(100)).expect("uncontended");
        FileExt::unlock(&file).expect("release");
    }
}
