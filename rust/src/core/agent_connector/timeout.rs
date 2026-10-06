use anyhow::Result;
use std::collections::BTreeSet;
use std::process::{Command, Output};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

pub(crate) mod durable;

const MAX_CAPTURED_OUTPUT_BYTES: usize = 1_048_576;
const MAX_CANCELLATION_KEYS: usize = 1_024;
static CANCELLATIONS: LazyLock<Mutex<BTreeSet<String>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));

pub(crate) struct TimedOutput {
    pub(crate) output: Output,
    pub(crate) timed_out: bool,
    pub(crate) cancelled: bool,
}

pub(crate) fn run_with_timeout(command: &mut Command, timeout_ms: u64) -> Result<TimedOutput> {
    run_with_timeout_cancellable(command, timeout_ms, None)
}

pub(crate) fn run_with_timeout_cancellable(
    command: &mut Command,
    timeout_ms: u64,
    cancellation_key: Option<&str>,
) -> Result<TimedOutput> {
    let capture = durable::capture_started();
    let mut observation_error = None;
    let captured = crate::core::process_capture::run_with_output_limits_cancellable(
        command,
        Duration::from_millis(timeout_ms),
        MAX_CAPTURED_OUTPUT_BYTES,
        MAX_CAPTURED_OUTPUT_BYTES,
        || {
            let Some(key) = cancellation_key else {
                return false;
            };
            match durable::poll(key) {
                Ok(stopped) => stopped || cancellation_requested(key),
                Err(error) => {
                    observation_error = Some(error);
                    true
                }
            }
        },
    );
    if captured.is_ok()
        && let Some(capture) = capture
    {
        capture.confirm_reaped();
    }
    if let Some(error) = observation_error {
        return Err(anyhow::anyhow!(
            "durable cancellation observation failed: {error}; capture: {}",
            captured
                .as_ref()
                .map_or_else(String::as_str, |_| "child reaped")
        ));
    }
    let captured = captured.map_err(anyhow::Error::msg)?;
    Ok(TimedOutput {
        output: captured.output,
        timed_out: captured.timed_out,
        cancelled: captured.cancelled,
    })
}

pub(crate) fn request_cancellation(task_prefix: &str) -> Result<()> {
    request_cancellations(std::iter::once(task_prefix))
}

pub(crate) fn request_cancellations<'a>(
    task_prefixes: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let mut cancellations = CANCELLATIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let requested = task_prefixes
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    insert_cancellations(&mut cancellations, requested)
}

fn insert_cancellations(
    cancellations: &mut BTreeSet<String>,
    requested: BTreeSet<String>,
) -> Result<()> {
    let additional = requested.difference(&cancellations).count();
    if cancellations.len().saturating_add(additional) > MAX_CANCELLATION_KEYS {
        anyhow::bail!("cancellation registry capacity exceeded");
    }
    cancellations.extend(requested);
    Ok(())
}

pub(crate) fn clear_cancellation(task_prefix: &str) {
    CANCELLATIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(task_prefix);
}

pub(crate) fn cancellation_requested(task_id: &str) -> bool {
    CANCELLATIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|prefix| {
            task_id == prefix
                || task_id
                    .strip_prefix(prefix)
                    .is_some_and(|suffix| suffix.starts_with(":attempt-"))
        })
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[cfg(unix)]
    #[test]
    fn kills_child_when_timeout_expires() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 1"]);

        let output = run_with_timeout(&mut command, 20).unwrap();

        assert!(output.timed_out);
    }

    #[cfg(unix)]
    #[test]
    fn collects_output_when_child_completes() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf done"]);

        let output = run_with_timeout(&mut command, 1_000).unwrap();

        assert!(!output.timed_out);
        assert_eq!(output.output.stdout, b"done");
    }

    #[cfg(unix)]
    #[test]
    fn inherited_output_does_not_extend_timeout() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 2 & wait"]);
        let start = std::time::Instant::now();
        let output = run_with_timeout(&mut command, 50).expect("timed process capture");
        assert!(output.timed_out);
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn inherited_output_does_not_delay_completed_parent() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 2 & printf done; exit 0"]);
        let start = std::time::Instant::now();
        let output = run_with_timeout(&mut command, 500).expect("completed process capture");
        assert!(!output.timed_out);
        assert!(output.output.status.success());
        assert_eq!(output.output.stdout, b"done");
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn timeout_stops_descendant_work() {
        let directory = tempfile::tempdir().expect("isolated descendant marker");
        let marker = directory.path().join("survived");
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "(sleep 1; printf leaked > \"$1\") & wait",
            "timeout-test",
        ]);
        command.arg(&marker);
        let output = run_with_timeout(&mut command, 50).expect("descendant cleanup");
        assert!(output.timed_out);
        std::thread::sleep(Duration::from_millis(1200));
        assert!(!marker.exists(), "descendant continued work after timeout");
    }

    #[cfg(unix)]
    #[test]
    fn capture_preserves_both_streams_and_nonzero_exit() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf out; printf err >&2; exit 7"]);
        let output = run_with_timeout(&mut command, 1000).expect("nonzero child capture");
        assert!(!output.timed_out);
        assert_eq!(output.output.status.code(), Some(7));
        assert_eq!(output.output.stdout, b"out");
        assert_eq!(output.output.stderr, b"err");
    }
    #[cfg(unix)]
    #[test]
    fn cancellation_key_kills_attempt_process_tree_before_timeout() {
        let key = "graph:node";
        clear_cancellation(key);
        request_cancellation(key).unwrap();
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5"]);

        let output =
            run_with_timeout_cancellable(&mut command, 5_000, Some("graph:node:attempt-1"))
                .unwrap();

        assert!(output.cancelled);
        assert!(!output.timed_out);
        clear_cancellation(key);
    }

    #[test]
    fn cancellation_capacity_never_evicts_an_active_key() {
        let mut cancellations = (0..MAX_CANCELLATION_KEYS)
            .map(|index| format!("active:{index}"))
            .collect::<BTreeSet<_>>();
        let before = cancellations.clone();
        assert!(insert_cancellations(&mut cancellations, BTreeSet::from(["new".into()])).is_err());
        assert_eq!(cancellations, before);
    }

    #[test]
    fn work_graph_cancellation_is_project_and_execution_scoped() {
        use crate::core::work_graph_executor::execution_key;
        let first = tempfile::tempdir().expect("first project");
        let second = tempfile::tempdir().expect("second project");
        let old = execution_key(first.path(), "graph", "node", "old-fence").unwrap();
        let replacement = execution_key(first.path(), "graph", "node", "new-fence").unwrap();
        let other = execution_key(second.path(), "graph", "node", "old-fence").unwrap();
        assert_eq!(
            old,
            crate::core::work_graph_executor::execution_key_for_scope(
                &crate::core::project_hash::hash_project_root(first.path().to_str().unwrap()),
                "graph",
                "node",
                "old-fence"
            )
            .unwrap()
        );
        assert_ne!(old, replacement);
        assert_ne!(old, other);
        assert_ne!(
            execution_key(first.path(), "graph:child", "node", "fence").unwrap(),
            execution_key(first.path(), "graph", "child:node", "fence").unwrap(),
        );
        request_cancellation(&old).unwrap();
        assert!(cancellation_requested(&format!("{old}:attempt-1")));
        assert!(!cancellation_requested(&format!("{replacement}:attempt-1")));
        assert!(!cancellation_requested(&format!("{other}:attempt-1")));
        request_cancellation(&replacement).unwrap();
        clear_cancellation(&old);
        assert!(
            cancellation_requested(&format!("{replacement}:attempt-2")),
            "late cleanup must not clear a replacement execution's stop"
        );
        clear_cancellation(&replacement);
        assert!(execution_key(first.path(), "graph", "node", "").is_err());
    }
    #[cfg(unix)]
    #[test]
    fn oversized_capture_fails_closed_instead_of_accepting_truncated_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "yes x | head -c 1100000"]);
        assert!(run_with_timeout(&mut command, 5_000).is_err());
    }
}
