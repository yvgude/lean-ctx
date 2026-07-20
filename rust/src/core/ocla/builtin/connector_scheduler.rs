//! BuiltinConnectorScheduler — queues provider connector jobs.
//!
//! Wraps `core/providers/provider_trait.rs` behind the OCLA trait.
//! Jobs are queued locally with bounded capacity; actual execution
//! remains with the provider pipeline. This adapter provides the
//! scheduling interface and deterministic job-ref generation.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::ocla::traits::{ConnectorScheduler, OclaService};
use crate::core::ocla::types::{
    ConnectorJob, OclaCapability, OclaCapabilityKind, OclaResult, ScheduledJob,
};

const MAX_QUEUED_JOBS: usize = 128;

pub struct BuiltinConnectorScheduler {
    queue: Mutex<VecDeque<ConnectorJob>>,
    next_seq: AtomicU64,
}

impl BuiltinConnectorScheduler {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(MAX_QUEUED_JOBS)),
            next_seq: AtomicU64::new(1),
        }
    }

    pub fn pending_count(&self) -> usize {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

impl Default for BuiltinConnectorScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinConnectorScheduler {
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::ConnectorScheduler)
    }
}

impl ConnectorScheduler for BuiltinConnectorScheduler {
    fn schedule_connector(&self, job: ConnectorJob) -> OclaResult<ScheduledJob> {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let job_ref = format!("job:{}:{seq}", job.connector_id);
        let queue_ref = format!("queue:{}", job.connector_id);

        let mut queue = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if queue.len() >= MAX_QUEUED_JOBS {
            queue.pop_front();
        }
        queue.push_back(job);

        Ok(ScheduledJob { job_ref, queue_ref })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::types::OclaRequestContext;

    fn job(connector: &str) -> ConnectorJob {
        ConnectorJob {
            context: OclaRequestContext {
                request_id: "r1".into(),
                session_id: "s1".into(),
                agent_id: "agent-test".into(),
                content_ref: "ref:test".into(),
                tenant_id: None,
            },
            connector_id: connector.into(),
            payload_ref: "payload:abc".into(),
            deadline_ms: Some(5000),
        }
    }

    #[test]
    fn schedule_returns_unique_refs() {
        let scheduler = BuiltinConnectorScheduler::new();
        let j1 = scheduler.schedule_connector(job("github")).unwrap();
        let j2 = scheduler.schedule_connector(job("github")).unwrap();
        assert_ne!(j1.job_ref, j2.job_ref);
        assert_eq!(scheduler.pending_count(), 2);
    }

    #[test]
    fn bounded_capacity() {
        let scheduler = BuiltinConnectorScheduler::new();
        for _ in 0..150 {
            scheduler.schedule_connector(job("test")).unwrap();
        }
        assert_eq!(scheduler.pending_count(), MAX_QUEUED_JOBS);
    }
}
