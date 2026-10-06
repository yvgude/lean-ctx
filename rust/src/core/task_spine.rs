// SPDX-License-Identifier: Apache-2.0
//! Canonical task lineage with immutable async/blocking execution scopes.

use chrono::Utc;
use lean_ctx_protocol::{
    AgentId, ProjectId, SessionId, TaskComplexity, TaskEnvelopeV1, TaskId, TaskProfileV1, TenantId,
    TraceId,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

pub type TaskProfileLocal = TaskProfileV1;

thread_local! {
    static CURRENT: RefCell<Option<TaskEnvelopeV1>> = const { RefCell::new(None) };
}

tokio::task_local! {
    // Explicit None masks stale legacy thread state in unbound worker scopes.
    static SCOPED: Option<TaskEnvelopeV1>;
}

/// Parent task id retained for the lifetime of each active MCP session.
type SessionLineage = (String, String);
type SessionLineages = Arc<Mutex<HashMap<(ProjectId, Option<TenantId>, String), SessionLineage>>>;
static SESSION_LINEAGES: OnceLock<SessionLineages> = OnceLock::new();

/// Logical identities supplied by an admitting host, never by tool arguments.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct AdmittedTaskIdentity {
    pub(crate) project_id: ProjectId,
    pub(crate) tenant_id: TenantId,
}

#[derive(Debug, Clone, Default)]
/// Maintains task-envelope lineage for the current execution thread.
pub struct TaskSpine {
    /// Root task of the current session, absent when creating that root task.
    pub parent_id: Option<String>,
    /// Stable trace id shared by the root task and every true child in the session.
    pub trace_id: String,
}

impl TaskSpine {
    pub fn create_envelope(query: &str, session_id: &str, agent_id: &str) -> TaskEnvelopeV1 {
        Self::create_envelope_in_project(query, session_id, agent_id, None)
    }

    /// Only the admitting host supplies workspace identity. The latest saved
    /// session belongs to no particular caller and must never supply this scope.
    pub(crate) fn create_envelope_in_project(
        query: &str,
        session_id: &str,
        agent_id: &str,
        project_root: Option<&str>,
    ) -> TaskEnvelopeV1 {
        Self::create_envelope_with_identity(query, session_id, agent_id, project_root, None)
    }

    pub(crate) fn create_envelope_with_identity(
        query: &str,
        session_id: &str,
        agent_id: &str,
        project_root: Option<&str>,
        identity: Option<&AdmittedTaskIdentity>,
    ) -> TaskEnvelopeV1 {
        let project_id = identity
            .map(|identity| identity.project_id.clone())
            .or_else(|| project_root.and_then(|project| ProjectId::try_from(project).ok()))
            .unwrap_or_else(|| {
                ProjectId::try_from("unknown-project").expect("fallback project id is valid")
            });
        let task_id = TaskId::try_from(format!("mcp-task-{}", uuid::Uuid::new_v4()))
            .expect("generated task id is valid");
        let initial_trace_id = format!("trace-{}", uuid::Uuid::new_v4());
        let tenant_id = identity.map(|identity| identity.tenant_id.clone());
        let spine = Self::for_session(
            &project_id,
            tenant_id.as_ref(),
            session_id,
            task_id.as_str(),
            &initial_trace_id,
        );
        let envelope = TaskEnvelopeV1 {
            schema_version: TaskEnvelopeV1::SCHEMA_VERSION,
            task_id,
            trace_id: TraceId::try_from(spine.trace_id.clone())
                .expect("generated trace id is valid"),
            project_id,
            session_id: SessionId::try_from(session_id.to_owned())
                .expect("MCP session id is valid"),
            agent_id: AgentId::try_from(agent_id.to_owned()).expect("MCP agent id is valid"),
            complexity: TaskComplexity::Unknown,
            created_at: Utc::now().to_rfc3339(),
            parent_task_id: spine
                .parent_id
                .clone()
                .map(TaskId::try_from)
                .transpose()
                .expect("stored parent task id is valid"),
            tenant_id,
            intent: (!query.trim().is_empty()).then(|| query.to_owned()),
            task_class: None,
            risk_class: None,
            quality_requirement_milli: None,
            cost_budget_micros: None,
            latency_budget_ms: None,
            data_classification: None,
            region_policy_ref: None,
            model_policy_ref: None,
            context_state_ref: None,
            outcome_contract_ref: None,
            extensions: Default::default(),
        };
        Self::set(envelope.clone());
        envelope
    }

    fn for_session(
        project_id: &ProjectId,
        tenant_id: Option<&TenantId>,
        session_id: &str,
        task_id: &str,
        initial_trace_id: &str,
    ) -> Self {
        if session_id.trim().is_empty() {
            return Self {
                parent_id: None,
                trace_id: initial_trace_id.to_owned(),
            };
        }

        let mut lineages = Self::session_lineages()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (parent_id, trace_id) = match lineages.entry((
            project_id.clone(),
            tenant_id.cloned(),
            session_id.to_owned(),
        )) {
            std::collections::hash_map::Entry::Occupied(parent) => {
                let (root_task_id, trace_id) = parent.get();
                (Some(root_task_id.clone()), trace_id.clone())
            }
            std::collections::hash_map::Entry::Vacant(session) => {
                session.insert((task_id.to_owned(), initial_trace_id.to_owned()));
                (None, initial_trace_id.to_owned())
            }
        };
        Self {
            parent_id,
            trace_id,
        }
    }

    fn session_lineages() -> &'static SessionLineages {
        SESSION_LINEAGES.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
    }

    pub fn enrich_from_triage(envelope: &mut TaskEnvelopeV1, profile: &TaskProfileLocal) {
        envelope.intent = Some(profile.primary_intent.clone());
        envelope.task_class = Some(profile.task_class.clone());
        envelope.complexity = profile.complexity;
        envelope.risk_class = Some(profile.risk_signal);
        Self::set(envelope.clone());
    }

    pub fn task_id() -> Option<String> {
        Self::current().map(|envelope| envelope.task_id.as_str().to_owned())
    }

    pub fn current() -> Option<TaskEnvelopeV1> {
        SCOPED
            .try_with(Clone::clone)
            .unwrap_or_else(|_| CURRENT.with(|current| current.borrow().clone()))
    }

    /// Only an explicitly bound lifecycle may supply canonical execution identity.
    pub(crate) fn scoped_current() -> Option<TaskEnvelopeV1> {
        SCOPED.try_with(Clone::clone).ok().flatten()
    }

    /// Legacy synchronous callers may set a fallback, never overwrite a bound task.
    pub fn set(envelope: TaskEnvelopeV1) {
        if SCOPED.try_with(|_| ()).is_err() {
            CURRENT.with(|current| *current.borrow_mut() = Some(envelope));
        }
    }

    pub(crate) async fn scope<F: std::future::Future>(
        envelope: Option<TaskEnvelopeV1>,
        future: F,
    ) -> F::Output {
        SCOPED.scope(envelope, future).await
    }

    /// Callers must capture the owning async scope before spawning a blocking job.
    pub(crate) fn sync_scope<T>(
        envelope: Option<TaskEnvelopeV1>,
        operation: impl FnOnce() -> T,
    ) -> T {
        SCOPED.sync_scope(envelope, operation)
    }

    /// Preserve the owner of a detached read/telemetry worker, including no owner.
    pub(crate) fn spawn_thread<T: Send + 'static>(
        operation: impl FnOnce() -> T + Send + 'static,
    ) -> std::thread::JoinHandle<T> {
        let envelope = Self::current();
        let project = crate::core::policy::runtime::REQUEST_PROJECT
            .try_with(|slot| slot.borrow().clone())
            .ok();
        let source_view = crate::core::policy::runtime::inherited_source_view();
        // Admissions made by the worker belong to the call that spawned it.
        let admissions = crate::core::context_admission::capture::current();
        std::thread::spawn(move || {
            crate::core::context_admission::capture::ADMISSIONS.sync_scope(admissions, || {
                match project {
                    Some(project) => crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
                        std::cell::RefCell::new(project),
                        || {
                            crate::core::policy::runtime::with_inherited_source_view(
                                source_view,
                                || Self::sync_scope(envelope, operation),
                            )
                        },
                    ),
                    None => crate::core::policy::runtime::with_inherited_source_view(
                        source_view,
                        || Self::sync_scope(envelope, operation),
                    ),
                }
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detached_read_workers_isolate_overlapping_policy_projects() {
        use crate::core::policy::runtime::REQUEST_PROJECT;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let workers = ["first-request-project", "second-request-project"].map(|name| {
            let root = std::path::PathBuf::from(name);
            let worker_barrier = barrier.clone();
            let worker =
                REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(Some(root.clone())), || {
                    TaskSpine::spawn_thread(move || {
                        worker_barrier.wait();
                        REQUEST_PROJECT.with(|slot| slot.borrow().clone())
                    })
                });
            (root, worker)
        });
        barrier.wait();
        for (root, worker) in workers {
            assert_eq!(worker.join().unwrap(), Some(root));
        }
    }

    #[test]
    fn detached_worker_inherits_pinned_source_policy() {
        use crate::core::policy::runtime::{self, REQUEST_PROJECT};
        use std::fs;

        let _override_guard = runtime::lock_test_override();
        let _data = crate::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().expect("create project");
        let policy_dir = root.path().join(".lean-ctx");
        fs::create_dir_all(&policy_dir).expect("create policy directory");
        let policy_path = policy_dir.join("policy.toml");
        fs::write(
            &policy_path,
            "name = \"thread-a\"\nversion = \"1.0.0\"\ndescription = \"thread test\"\n",
        )
        .expect("write policy A");

        let result =
            REQUEST_PROJECT.sync_scope(RefCell::new(Some(root.path().to_path_buf())), || {
                runtime::with_source_view(|| {
                    let worker = TaskSpine::spawn_thread(|| {
                        runtime::active().map(|policy| policy.resolved.name.clone())
                    });
                    fs::write(
                        &policy_path,
                        "name = \"thread-b\"\nversion = \"1.0.0\"\ndescription = \"thread test\"\n",
                    )
                    .expect("write policy B");
                    assert_eq!(worker.join().unwrap().as_deref(), Some("thread-a"));
                })
            });

        assert!(result.is_err());
    }

    #[test]
    fn detached_threads_preserve_bound_and_explicitly_unbound_identity() {
        let task = TaskSpine::create_envelope("thread", "scoped-thread", "agent");
        let worker = TaskSpine::sync_scope(Some(task.clone()), || {
            TaskSpine::spawn_thread(TaskSpine::current)
        });
        assert_eq!(worker.join().unwrap(), Some(task));
        let unbound = TaskSpine::sync_scope(None, || {
            TaskSpine::spawn_thread(|| {
                TaskSpine::create_envelope("decoy", "detached-decoy", "agent");
                TaskSpine::current()
            })
        });
        assert!(unbound.join().unwrap().is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_scopes_isolate_interleaved_tasks_and_ignore_legacy_setters() {
        let first = TaskSpine::create_envelope("first", "scoped-a", "agent-a");
        let second = TaskSpine::create_envelope("second", "scoped-b", "agent-b");
        let legacy = TaskSpine::current();
        let check = |expected: TaskEnvelopeV1, other: TaskEnvelopeV1| async move {
            TaskSpine::scope(Some(expected.clone()), async {
                for _ in 0..5 {
                    TaskSpine::set(other.clone());
                    tokio::task::yield_now().await;
                    assert_eq!(TaskSpine::current(), Some(expected.clone()));
                }
                TaskSpine::scope(None, async {
                    assert!(TaskSpine::current().is_none());
                    TaskSpine::set(other);
                    assert!(TaskSpine::current().is_none());
                })
                .await;
                assert_eq!(TaskSpine::current(), Some(expected));
            })
            .await;
        };
        tokio::join!(check(first.clone(), second.clone()), check(second, first));
        assert_eq!(TaskSpine::current(), legacy);
    }

    #[test]
    fn sync_scope_restores_identity_after_panic_and_masks_unbound_workers() {
        let legacy = TaskSpine::create_envelope("legacy", "legacy-worker", "agent");
        let scoped = TaskSpine::create_envelope("scoped", "scoped-worker", "agent");
        TaskSpine::set(legacy.clone());
        assert!(TaskSpine::scoped_current().is_none());
        let panic = std::panic::catch_unwind(|| {
            TaskSpine::sync_scope(Some(scoped.clone()), || {
                assert_eq!(TaskSpine::scoped_current(), Some(scoped.clone()));
                assert_eq!(TaskSpine::current(), Some(scoped));
                panic!("fixture unwind");
            })
        });
        assert!(panic.is_err());
        assert_eq!(TaskSpine::current(), Some(legacy.clone()));
        TaskSpine::sync_scope(None, || assert!(TaskSpine::current().is_none()));
        assert_eq!(TaskSpine::current(), Some(legacy));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_scope_does_not_replace_parent_identity() {
        let parent = TaskSpine::create_envelope("parent", "cancel-parent", "agent");
        let child = TaskSpine::create_envelope("child", "cancel-child", "agent");
        TaskSpine::scope(Some(parent.clone()), async {
            let task = TaskSpine::scope(Some(child), async {
                std::future::pending::<()>().await;
            });
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(10), task)
                    .await
                    .is_err()
            );
            assert_eq!(TaskSpine::current(), Some(parent));
        })
        .await;
    }

    #[test]
    fn test_envelope_created_on_dispatch() {
        let envelope =
            TaskSpine::create_envelope("query", "session-task-spine", "agent-task-spine");
        assert_eq!(
            TaskSpine::task_id().as_deref(),
            Some(envelope.task_id.as_str())
        );
    }

    #[test]
    fn test_envelope_has_valid_task_id() {
        let first = TaskSpine::create_envelope("one", "session-task-spine", "agent-task-spine");
        let second = TaskSpine::create_envelope("two", "session-task-spine", "agent-task-spine");
        assert!(!first.task_id.as_str().is_empty());
        assert_ne!(first.task_id, second.task_id);
    }

    #[test]
    fn test_enrich_from_triage() {
        let mut envelope =
            TaskSpine::create_envelope("query", "session-task-spine", "agent-task-spine");
        let profile = TaskProfileV1 {
            primary_intent: "implement".into(),
            task_class: "coding".into(),
            complexity: TaskComplexity::High,
            scope: Default::default(),
            context_need_milli: 0,
            reasoning_need_milli: 0,
            risk_signal: lean_ctx_protocol::RiskClass::High,
            confidence_milli: 0,
            capability_id: None,
            capability_version: None,
            keywords: vec![],
            language_hints: vec![],
        };
        TaskSpine::enrich_from_triage(&mut envelope, &profile);
        assert_eq!(envelope.intent.as_deref(), Some("implement"));
        assert_eq!(envelope.task_class.as_deref(), Some("coding"));
        assert_eq!(envelope.complexity, TaskComplexity::High);
        assert_eq!(
            envelope.risk_class,
            Some(lean_ctx_protocol::RiskClass::High)
        );
    }

    #[test]
    fn first_session_task_is_parent_of_subsequent_tasks() {
        let session_id = format!("lineage-session-{}", uuid::Uuid::new_v4());
        let parent = TaskSpine::create_envelope("first", &session_id, "agent");
        let child = TaskSpine::create_envelope("second", &session_id, "agent");

        assert!(parent.parent_task_id.is_none());
        assert_eq!(child.trace_id, parent.trace_id);
        child.validate_child_of(&parent).unwrap();
        assert_eq!(
            child
                .parent_task_id
                .as_ref()
                .map(lean_ctx_protocol::TaskId::as_str),
            Some(parent.task_id.as_str())
        );
        assert_eq!(
            TaskSpine::current()
                .as_ref()
                .map(|envelope| envelope.task_id.as_str()),
            Some(child.task_id.as_str())
        );
    }

    #[test]
    fn session_traces_are_isolated_and_shared_by_all_children() {
        let first_session = format!("lineage-first-{}", uuid::Uuid::new_v4());
        let second_session = format!("lineage-second-{}", uuid::Uuid::new_v4());
        let first = TaskSpine::create_envelope("root", &first_session, "agent");
        let second = TaskSpine::create_envelope("root", &second_session, "agent");
        assert_ne!(first.trace_id, second.trace_id);
        assert!(second.parent_task_id.is_none());
        for _ in 0..3 {
            let child = TaskSpine::create_envelope("child", &first_session, "agent");
            child.validate_child_of(&first).unwrap();
            assert_ne!(child.task_id, first.task_id);
        }
    }
}
