// SPDX-License-Identifier: Apache-2.0

//! Thread-bound observer for synchronous connector capture, not a scheduler.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::core::work_graph_store::WorkGraphStore;

thread_local! {
    static WATCH: RefCell<Option<Watch>> = const { RefCell::new(None) };
}

struct Watch {
    key: String,
    project: String,
    graph: String,
    node: String,
    fence: String,
    last_poll: Option<Instant>,
    stopped: bool,
    captures: CaptureTracker,
}

/// Not Send: the observer must be removed on the same connector thread.
pub(crate) struct Guard(CaptureTracker);

/// Private construction prevents a caller from inventing a successful reap.
#[derive(Clone)]
pub(crate) struct CaptureTracker(Rc<Cell<usize>>);

impl CaptureTracker {
    fn new() -> Self {
        Self(Rc::new(Cell::new(0)))
    }

    pub(crate) fn all_reaped(&self) -> bool {
        self.0.get() == 0
    }
}

/// Dropping without confirmation conservatively retains the unresolved count.
pub(super) struct Capture(CaptureTracker);

impl Capture {
    pub(super) fn confirm_reaped(self) {
        self.0.0.set(self.0.0.get() - 1);
    }
}

pub(super) fn capture_started() -> Option<Capture> {
    WATCH.with(|cell| {
        let slot = cell.borrow();
        let watch = slot.as_ref()?;
        let tracker = watch.captures.clone();
        tracker.0.set(tracker.0.get().saturating_add(1));
        Some(Capture(tracker))
    })
}

impl Guard {
    pub(crate) fn install(
        project: &str,
        graph: &str,
        node: &str,
        fence: &str,
    ) -> Result<Self, String> {
        let key = crate::core::work_graph_executor::execution_key(
            std::path::Path::new(project),
            graph,
            node,
            fence,
        )?;
        if WorkGraphStore::execution_stop_requested(project, graph, node, fence)? {
            return Err("execution was cancelled before connector dispatch".into());
        }
        WATCH.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_some() {
                return Err("nested durable execution observers are not supported".into());
            }
            let captures = CaptureTracker::new();
            *slot = Some(Watch {
                key,
                project: project.into(),
                graph: graph.into(),
                node: node.into(),
                fence: fence.into(),
                last_poll: None,
                stopped: false,
                captures: captures.clone(),
            });
            Ok(Self(captures))
        })
    }

    pub(crate) fn captures(&self) -> CaptureTracker {
        self.0.clone()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        WATCH.with(|cell| {
            cell.borrow_mut().take();
        });
    }
}

pub(super) fn poll(task_id: &str) -> Result<bool, String> {
    WATCH.with(|cell| {
        let mut slot = cell.borrow_mut();
        let Some(watch) = slot.as_mut() else {
            return Ok(false);
        };
        if task_id != watch.key
            && !task_id
                .strip_prefix(&watch.key)
                .is_some_and(|suffix| suffix.starts_with(":attempt-"))
        {
            return Err("connector task does not match its durable execution observer".into());
        }
        if watch.stopped {
            return Ok(true);
        }
        if watch
            .last_poll
            .is_some_and(|last| last.elapsed() < Duration::from_millis(100))
        {
            return Ok(false);
        }
        watch.stopped = WorkGraphStore::execution_stop_requested(
            &watch.project,
            &watch.graph,
            &watch.node,
            &watch.fence,
        )?;
        watch.last_poll = Some(Instant::now());
        Ok(watch.stopped)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unresolved_capture_cannot_be_cleared_by_later_success() {
        let tracker = CaptureTracker::new();
        assert!(tracker.all_reaped());
        tracker.0.set(1);
        {
            let _unresolved = Capture(tracker.clone());
        }
        assert!(!tracker.all_reaped());
        tracker.0.set(2);
        Capture(tracker.clone()).confirm_reaped();
        assert!(!tracker.all_reaped());
        assert_eq!(tracker.0.get(), 1);
    }

    #[test]
    fn all_started_captures_need_individual_confirmation() {
        let tracker = CaptureTracker::new();
        tracker.0.set(2);
        Capture(tracker.clone()).confirm_reaped();
        assert!(!tracker.all_reaped());
        Capture(tracker.clone()).confirm_reaped();
        assert!(tracker.all_reaped());
    }
}
