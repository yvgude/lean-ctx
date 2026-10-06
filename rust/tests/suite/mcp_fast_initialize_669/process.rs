// SPDX-License-Identifier: Apache-2.0

//! Bounded cleanup for the real stdio test, including assertion unwinding.

use std::io;
use std::process::{Child, ExitStatus};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(super) struct ChildGuard<'a> {
    child: &'a mut Child,
}

impl<'a> ChildGuard<'a> {
    pub(super) fn new(child: &'a mut Child) -> Self {
        Self { child }
    }

    pub(super) fn child_mut(&mut self) -> &mut Child {
        self.child
    }

    pub(super) fn wait_for_exit(&mut self, timeout: Duration) -> io::Result<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "child exit timeout",
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for ChildGuard<'_> {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        // Kill only this owned child; never touch installed runtimes or other tests.
        let _ = self.child.kill();
        // Reap normally, but do not replace an assertion failure with an endless wait.
        if let Err(error) = self.wait_for_exit(Duration::from_secs(2)) {
            eprintln!("stdio test child cleanup failed: {error}");
        }
    }
}

pub(super) fn join_bounded<T>(reader: JoinHandle<T>, timeout: Duration) -> io::Result<T> {
    let deadline = Instant::now() + timeout;
    while !reader.is_finished() {
        if Instant::now() >= deadline {
            // Dropping the handle detaches it; a descendant retaining stdout must
            // not hang this test. The ChildGuard still owns server cleanup.
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "reader exit timeout",
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
    reader
        .join()
        .map_err(|_| io::Error::other("reader panicked"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;

    #[test]
    fn child_fixture() {
        if std::env::var_os("LEAN_CTX_STDIO_GUARD_FIXTURE").is_some() {
            let mut input = Vec::new();
            io::stdin().read_to_end(&mut input).unwrap();
        }
    }

    fn child() -> Child {
        let module = module_path!().split_once("::").unwrap().1;
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &format!("{module}::child_fixture")])
            .env("LEAN_CTX_STDIO_GUARD_FIXTURE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[test]
    fn normal_eof_exit_is_reaped() {
        let mut child = child();
        let mut guard = ChildGuard::new(&mut child);
        drop(guard.child_mut().stdin.take());
        assert!(
            guard
                .wait_for_exit(Duration::from_secs(5))
                .unwrap()
                .success()
        );
    }

    #[test]
    fn timeout_then_drop_kills_and_reaps_child() {
        let mut child = child();
        {
            let mut guard = ChildGuard::new(&mut child);
            assert_eq!(
                guard.wait_for_exit(Duration::ZERO).unwrap_err().kind(),
                io::ErrorKind::TimedOut
            );
        }
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn panic_unwinding_kills_and_reaps_child() {
        let mut child = child();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = ChildGuard::new(&mut child);
            panic!("deliberate test assertion failure");
        }));
        assert!(result.is_err());
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn completed_reader_returns_its_value() {
        assert_eq!(
            join_bounded(thread::spawn(|| 42), Duration::from_secs(2)).unwrap(),
            42
        );
    }

    #[test]
    fn blocked_reader_times_out_without_waiting_for_join() {
        let (release, pending) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let reader = thread::spawn(move || {
            let _ = pending.recv();
            finished.send(()).unwrap();
        });
        let result = join_bounded(reader, Duration::ZERO);
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }
}
