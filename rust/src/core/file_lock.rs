use std::fs::File;
use std::io::{Error, ErrorKind};
use std::time::{Duration, Instant};

const RETRY_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) fn is_contended(error: &Error) -> bool {
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
pub(crate) fn acquire_exclusive_timeout(file: &File, timeout: Duration) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::{acquire_exclusive_timeout, is_contended};
    use std::io::{Error, ErrorKind};
    use std::time::{Duration, Instant};

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
