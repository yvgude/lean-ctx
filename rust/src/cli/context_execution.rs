// SPDX-License-Identifier: Apache-2.0

//! CLI context command results and their single local lifecycle owner.

use std::time::Duration;

use crate::core::execution_lifecycle::{
    CompletionObservation, LifecycleStage, TaskContext, ToolSurface,
};
use crate::core::task_spine::TaskSpine;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ContextCommand {
    Read,
    Diff,
    Grep,
    Glob,
    Find,
    Ls,
    Deps,
}

/// Actual primitive observations, retained for the owning lifecycle stages.
/// These values do not assert that any persistence or acceptance occurred.
#[derive(Debug)]
pub(crate) enum Recording {
    Read {
        path: String,
        mode: String,
        input_tokens: usize,
        output_tokens: usize,
        cache_hit: bool,
        elapsed: Duration,
        excerpt: String,
    },
    Search {
        modeled_baseline: usize,
        observed_tokens: usize,
        output_tokens: usize,
        pattern: String,
        path: String,
        elapsed: Duration,
        excerpt: String,
    },
    Tree {
        input_tokens: usize,
        output_tokens: usize,
    },
    Stats {
        tool: &'static str,
        input_tokens: usize,
        output_tokens: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(unix), allow(dead_code))] // `Daemon` is only constructed on the cfg(unix) route
pub(crate) enum ExecutionRoute {
    NotDispatched,
    Daemon,
    Local,
}

#[derive(Debug)]
pub(crate) struct CommandOutput {
    pub exit_code: i32,
    pub route: ExecutionRoute,
    pub recording: Option<Recording>,
    authority: Option<crate::core::policy::runtime::PublicationAuthority>,
}

impl CommandOutput {
    pub(crate) fn rejected(exit_code: i32) -> Self {
        Self {
            exit_code,
            route: ExecutionRoute::NotDispatched,
            recording: None,
            authority: None,
        }
    }

    #[cfg_attr(not(unix), allow(dead_code))] // called only from the cfg(unix) daemon route
    pub(crate) fn daemon(exit_code: i32) -> Self {
        Self {
            exit_code,
            route: ExecutionRoute::Daemon,
            recording: None,
            authority: None,
        }
    }

    pub(crate) fn local(exit_code: i32, recording: Option<Recording>) -> Self {
        Self {
            exit_code,
            route: ExecutionRoute::Local,
            recording,
            authority: None,
        }
    }

    pub(super) fn protected(
        exit_code: i32,
        recording: Option<Recording>,
        authority: crate::core::policy::runtime::PublicationAuthority,
    ) -> Self {
        Self {
            exit_code,
            route: ExecutionRoute::Local,
            recording,
            authority: Some(authority),
        }
    }
}

/// The adapter supplies the real standalone primitive; the runner owns its
/// task scope, recording stages and terminalization. Daemon routes never call it.
pub(crate) type LocalOperation<'a> = Box<dyn FnOnce() -> CommandOutput + 'a>;

pub(crate) fn excerpt(text: &str) -> String {
    crate::core::tool_lifecycle::ir_excerpt(text).to_owned()
}

impl ContextCommand {
    fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Diff => "diff",
            Self::Grep => "grep",
            Self::Glob => "glob",
            Self::Find => "find",
            Self::Ls => "ls",
            Self::Deps => "deps",
        }
    }
}

pub(crate) fn execute(command: ContextCommand, args: &[String]) -> CommandOutput {
    // A library caller's task must not leak into daemon ingress or a new CLI task.
    TaskSpine::sync_scope(None, || {
        match super::context_policy::prepare(command, args) {
            Ok(Some(prepared)) => {
                return run_protected(command, Box::new(move || prepared.publish()));
            }
            Ok(None) => {}
            Err(()) => {
                eprintln!("Context command withheld by the active policy or invalid arguments.");
                return CommandOutput::rejected(1);
            }
        }
        let local = |primitive| run_local(command, args, primitive).0;
        match command {
            ContextCommand::Read => super::read_cmd::observe_read(args, local),
            ContextCommand::Diff => super::read_cmd::observe_diff(args, local),
            ContextCommand::Grep => super::read_cmd::observe_grep(args, local),
            ContextCommand::Glob => super::read_cmd::observe_glob(args, local),
            ContextCommand::Find => super::read_cmd::observe_find(args, local),
            ContextCommand::Ls => super::read_cmd::observe_ls(args, local),
            ContextCommand::Deps => super::read_cmd::observe_deps(args, local),
        }
    })
}

fn completion(exit_code: i32) -> CompletionObservation {
    let mut observation = CompletionObservation::tool_result(0, 0, "cli-context", exit_code == 0);
    "local".clone_into(&mut observation.provider);
    // These are local command observations, not compiler or task acceptance evidence.
    observation.outcome_signals.clear();
    observation
}

fn skip_artifacts(context: &TaskContext, reason: &'static str) {
    for stage in [
        LifecycleStage::ReversiblePostProcess,
        LifecycleStage::RecordContextIr,
        LifecycleStage::RecordLedger,
        LifecycleStage::RecordEvidence,
    ] {
        let _ = context.skip(stage, reason);
    }
}

fn run_local(
    command: ContextCommand,
    args: &[String],
    primitive: LocalOperation<'_>,
) -> (CommandOutput, TaskContext) {
    let query = args.join(" ");
    let guard =
        super::dispatch::external_lifecycle_guard(command.name(), Some(&query), ToolSurface::Cli);
    run_admitted_local(guard, primitive)
}

pub(super) fn run_protected(
    command: ContextCommand,
    primitive: LocalOperation<'_>,
) -> CommandOutput {
    // Protected observations already carry admitted metadata. Never put raw
    // CLI arguments in lifecycle state, even if policy changes before publish.
    run_local(command, &[], primitive).0
}

fn run_admitted_local(
    guard: crate::core::execution_lifecycle::LifecycleGuard<'static>,
    primitive: LocalOperation<'_>,
) -> (CommandOutput, TaskContext) {
    let context = guard.context().clone();
    TaskSpine::sync_scope(Some(context.envelope.clone()), move || {
        crate::core::execution_lifecycle::begin_sync_heatmap_capture();
        let _ = context.advance(LifecycleStage::DispatchPrimitive);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut output = primitive();
            debug_assert_eq!(output.route, ExecutionRoute::Local);
            if output
                .authority
                .as_ref()
                .is_some_and(|authority| authority.verify().is_err())
            {
                output.recording = None;
                output.exit_code = 1;
                eprintln!("Context recording withheld because its authority changed.");
            }
            let _ = context.skip(
                LifecycleStage::ReversiblePostProcess,
                "CLI primitive already renders its final output",
            );
            match output.recording.as_ref().map(Recording::record_ir) {
                Some(Ok(true)) => {
                    let _ = context.advance(LifecycleStage::RecordContextIr);
                }
                Some(Err(error)) => {
                    tracing::warn!("CLI Context IR persistence failed: {error}");
                    let _ = context.skip(
                        LifecycleStage::RecordContextIr,
                        "Context IR persistence failed",
                    );
                }
                Some(Ok(false)) | None => {
                    let _ =
                        context.skip(LifecycleStage::RecordContextIr, "no Context IR observation");
                }
            }
            if let Some(recording) = &output.recording {
                recording.record_accounting();
            }
            // Keep existing counters/context pressure bookkeeping, but do not
            // represent their best-effort saves as a signed execution receipt.
            let _ = context.skip(
                LifecycleStage::RecordLedger,
                "legacy accounting only; canonical receipt unavailable",
            );
            let _ = context.skip(
                LifecycleStage::RecordEvidence,
                "no authoritative task outcome evidence",
            );
            output
        }));
        match result {
            Ok(output) => {
                let _ = guard.complete(completion(output.exit_code));
                (output, context)
            }
            Err(payload) => {
                skip_artifacts(&context, "CLI operation panicked");
                // Terminalize without the compatibility guard's synthetic CompileError.
                let _ = guard.complete(completion(1));
                std::panic::resume_unwind(payload)
            }
        }
    })
}

impl Recording {
    fn record_ir(&self) -> anyhow::Result<bool> {
        use crate::core::context_ir::{ContextIrSourceKindV1, ContextIrV1, RecordIrInput};
        let input = match self {
            Self::Read {
                path,
                mode,
                input_tokens,
                output_tokens,
                elapsed,
                excerpt,
                ..
            } => RecordIrInput {
                kind: ContextIrSourceKindV1::Read,
                tool: "ctx_read",
                client_name: Some("lean-ctx-cli".to_owned()),
                agent_id: Some("cli".to_owned()),
                path: Some(path),
                command: None,
                pattern: Some(mode),
                input_tokens: *input_tokens,
                output_tokens: *output_tokens,
                duration: *elapsed,
                content_excerpt: excerpt,
            },
            Self::Search {
                observed_tokens,
                output_tokens,
                pattern,
                path,
                elapsed,
                excerpt,
                ..
            } => RecordIrInput {
                kind: ContextIrSourceKindV1::Search,
                tool: "ctx_search",
                client_name: Some("lean-ctx-cli".to_owned()),
                agent_id: Some("cli".to_owned()),
                path: Some(path),
                command: None,
                pattern: Some(pattern),
                input_tokens: *observed_tokens,
                output_tokens: *output_tokens,
                duration: *elapsed,
                content_excerpt: excerpt,
            },
            Self::Tree { .. } | Self::Stats { .. } => return Ok(false),
        };
        let mut ir = ContextIrV1::load();
        ir.record(input);
        ir.try_save()?;
        Ok(true)
    }

    fn record_accounting(&self) {
        use crate::core::tool_lifecycle;
        match self {
            Self::Read {
                path,
                mode,
                input_tokens,
                output_tokens,
                cache_hit,
                elapsed,
                ..
            } => {
                tool_lifecycle::record_file_read_accounting(
                    path,
                    mode,
                    *input_tokens,
                    *output_tokens,
                    *cache_hit,
                    *elapsed,
                );
                tool_lifecycle::record_file_read_projection(
                    path,
                    mode,
                    *input_tokens,
                    *output_tokens,
                );
            }
            Self::Search {
                modeled_baseline,
                observed_tokens,
                output_tokens,
                path,
                elapsed,
                ..
            } => {
                tool_lifecycle::record_search_accounting(
                    *modeled_baseline,
                    *observed_tokens,
                    *output_tokens,
                    path,
                    *elapsed,
                );
            }
            Self::Tree {
                input_tokens,
                output_tokens,
            } => tool_lifecycle::record_tree(*input_tokens, *output_tokens),
            Self::Stats {
                tool,
                input_tokens,
                output_tokens,
            } => crate::core::stats::record(tool, *input_tokens, *output_tokens),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CommandOutput, ContextCommand, ExecutionRoute, Recording, run_admitted_local, run_local,
    };
    use crate::core::context_ir::ContextIrV1;
    use crate::core::execution_lifecycle::{
        LIFECYCLE_STAGE_ORDER, LifecycleStage, StageDisposition, ToolSurface,
    };
    use crate::core::task_spine::TaskSpine;
    use lean_ctx_protocol::AcceptanceState;
    use std::time::Duration;

    struct RestoreEnv(&'static str, Option<std::ffi::OsString>);

    impl RestoreEnv {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            crate::test_env::set_var(key, value);
            Self(key, previous)
        }
    }

    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            if let Some(value) = self.1.take() {
                crate::test_env::set_var(self.0, value);
            } else {
                crate::test_env::remove_var(self.0);
            }
        }
    }

    #[test]
    fn aggressive_and_cached_full_reads_record_once_and_preserve_ocla_projection() {
        use crate::core::ocla::OclaRegistry;
        use crate::core::ocla::builtin::savings_ledger::BuiltinSavingsLedger;
        use crate::core::savings_ledger::store;
        use std::fmt::Write;
        use std::sync::Arc;

        let data = crate::core::data_dir::isolated_data_dir();
        let _daemon = RestoreEnv::set("__LEAN_CTX_NO_DAEMON", "1");
        let _ledger = RestoreEnv::set("LEAN_CTX_SAVINGS_LEDGER", "on");
        let savings = Arc::new(BuiltinSavingsLedger::new());
        let mut registry = OclaRegistry::with_builtins();
        registry.savings_ledger = savings.clone();
        let _registry = crate::core::ocla::registry::with_test_registry(registry);
        let path = data.path().join("aggressive.rs");
        let mut source = String::new();
        for index in 0..60 {
            writeln!(
                source,
                "// Detailed implementation note for sample {index}: this explanatory comment is removable while the executable statements remain unchanged.\npub fn sample_{index}() -> usize {{\n    let buffer = [0; 128];\n    buffer.iter().map(|value| value + {index}).sum()\n}}"
            )
            .unwrap();
        }
        std::fs::write(&path, source).unwrap();
        let output = super::execute(
            ContextCommand::Read,
            &[
                path.to_string_lossy().into_owned(),
                "--mode".to_owned(),
                "aggressive".to_owned(),
                "--fresh".to_owned(),
            ],
        );
        assert_eq!(output.exit_code, 0);
        assert_eq!(output.route, ExecutionRoute::Local);
        let Some(Recording::Read {
            input_tokens,
            output_tokens,
            mode,
            ..
        }) = output.recording
        else {
            panic!("real read observation missing");
        };
        assert_eq!(mode, "aggressive");
        let saved = input_tokens.saturating_sub(output_tokens) as u64;
        assert!(
            saved > 0,
            "fixture must exercise actual compression savings"
        );
        assert_eq!(ContextIrV1::load().items.len(), 1);
        assert_eq!(savings.total_tokens_saved(), saved);
        let events = store::load(&store::default_path().unwrap());
        assert_eq!(
            events.len(),
            1,
            "one read must not be booked twice: {events:?}"
        );
        assert_eq!(events[0].tool, "ctx_read");
        assert_eq!(events[0].saved_tokens, saved);

        let full_args = [
            path.to_string_lossy().into_owned(),
            "--mode".to_owned(),
            "full".to_owned(),
        ];
        let warm = super::execute(ContextCommand::Read, &full_args);
        assert_eq!(warm.exit_code, 0);
        assert!(matches!(
            warm.recording,
            Some(Recording::Read {
                cache_hit: false,
                ..
            })
        ));
        let ledger_path = store::default_path().unwrap();
        let before_events = store::load(&ledger_path).len();
        let before_saved = savings.total_tokens_saved();
        let cached = super::execute(ContextCommand::Read, &full_args);
        assert_eq!(cached.exit_code, 0);
        assert_eq!(cached.route, ExecutionRoute::Local);
        let Some(Recording::Read {
            input_tokens,
            output_tokens,
            mode,
            cache_hit: true,
            ..
        }) = cached.recording
        else {
            panic!("real full-mode cache hit missing");
        };
        assert_eq!(mode, "full");
        let cached_saved = input_tokens.saturating_sub(output_tokens) as u64;
        assert!(cached_saved > 0);
        let events = store::load(&ledger_path);
        assert_eq!(events.len(), before_events + 1);
        assert_eq!(events.last().unwrap().tool, "ctx_read");
        assert_eq!(events.last().unwrap().saved_tokens, cached_saved);
        assert_eq!(savings.total_tokens_saved(), before_saved + cached_saved);
    }

    #[test]
    fn standalone_ocla_record_and_caller_projection_each_have_one_accounting_owner() {
        use crate::core::ocla::builtin::savings_ledger::BuiltinSavingsLedger;
        use crate::core::ocla::traits::SavingsLedger;
        use crate::core::ocla::types::{OclaRequestContext, SavingsEvidence};
        use crate::core::savings_ledger::{record_read_event, store};

        let _data = crate::core::data_dir::isolated_data_dir();
        let _ledger = RestoreEnv::set("LEAN_CTX_SAVINGS_LEDGER", "on");
        let ledger = BuiltinSavingsLedger::new();
        let mut evidence = SavingsEvidence {
            context: OclaRequestContext {
                request_id: "standalone-savings".to_owned(),
                session_id: "test-session".to_owned(),
                agent_id: "test-agent".to_owned(),
                content_ref: "file:test.rs".to_owned(),
                tenant_id: None,
                trace_id: "test-trace".to_owned(),
                task_id: None,
                parent_task_id: None,
            },
            original_tokens: 1_000,
            delivered_tokens: 400,
            quality_ref: None,
            evidence_ref: "standalone-observation".to_owned(),
        };
        ledger.record_savings(evidence.clone()).unwrap();
        let path = store::default_path().unwrap();
        let direct = store::load(&path);
        assert_eq!(direct.len(), 1);
        assert_eq!(direct[0].tool, "ocla_savings");
        assert_eq!(direct[0].saved_tokens, 600);

        record_read_event(500, 200, None, None);
        evidence.original_tokens = 500;
        evidence.delivered_tokens = 300;
        "caller-projection".clone_into(&mut evidence.context.request_id);
        "caller-observation".clone_into(&mut evidence.evidence_ref);
        ledger.project_savings(evidence).unwrap();
        let events = store::load(&path);
        assert_eq!(events.len(), 2, "projection must not append a third event");
        assert_eq!(events[1].tool, "ctx_read");
        assert_eq!(events[1].saved_tokens, 200);
        assert_eq!(ledger.total_tokens_saved(), 800);
    }

    #[test]
    fn real_standalone_family_runs_through_the_shared_owner() {
        let data = crate::core::data_dir::isolated_data_dir();
        let _daemon = RestoreEnv::set("__LEAN_CTX_NO_DAEMON", "1");
        let project = data.path().join("fixture");
        std::fs::create_dir(&project).unwrap();
        let first = project.join("needle.rs");
        let second = project.join("second.rs");
        std::fs::write(&first, "fn needle() {}\n").unwrap();
        std::fs::write(&second, "fn second() {}\n").unwrap();
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n[dependencies]\nserde = \"1\"\n",
        )
        .unwrap();
        let first = first.to_string_lossy().into_owned();
        let second = second.to_string_lossy().into_owned();
        let project = project.to_string_lossy().into_owned();
        let before = TaskSpine::current().map(|task| task.task_id);
        let cases = [
            (
                ContextCommand::Read,
                vec![first.clone(), "--fresh".to_owned()],
            ),
            (
                ContextCommand::Grep,
                vec!["needle".to_owned(), first.clone()],
            ),
            (ContextCommand::Diff, vec![first, second]),
            (
                ContextCommand::Glob,
                vec!["*.rs".to_owned(), project.clone()],
            ),
            (
                ContextCommand::Find,
                vec!["needle".to_owned(), project.clone()],
            ),
            (ContextCommand::Ls, vec![project.clone()]),
            (ContextCommand::Deps, vec![project]),
        ];
        for (command, args) in cases {
            let output = super::execute(command, &args);
            assert_eq!(output.exit_code, 0, "{command:?}");
            assert_eq!(output.route, ExecutionRoute::Local, "{command:?}");
            assert!(output.recording.is_some(), "{command:?}");
            assert_eq!(TaskSpine::current().map(|task| task.task_id), before);
        }
        // Read and grep each publish one IR item; the other primitives have no
        // IR contract and must not manufacture an item from a successful exit.
        assert_eq!(ContextIrV1::load().items.len(), 2);
    }

    #[test]
    fn every_local_context_command_owns_one_dispatch_and_unknown_terminal_status() {
        let _data = crate::core::data_dir::isolated_data_dir();
        for command in [
            ContextCommand::Read,
            ContextCommand::Diff,
            ContextCommand::Grep,
            ContextCommand::Glob,
            ContextCommand::Find,
            ContextCommand::Ls,
            ContextCommand::Deps,
        ] {
            for code in [0, 1, 127] {
                let calls = std::cell::Cell::new(0);
                let (output, context) = TaskSpine::sync_scope(None, || {
                    run_local(
                        command,
                        &[],
                        Box::new(|| {
                            calls.set(calls.get() + 1);
                            assert!(TaskSpine::current().is_some());
                            CommandOutput::local(code, None)
                        }),
                    )
                });
                assert_eq!(calls.get(), 1);
                assert_eq!(output.exit_code, code);
                assert_eq!(output.route, ExecutionRoute::Local);
                assert_eq!(context.completed_stages(), LIFECYCLE_STAGE_ORDER);
                assert_eq!(
                    context.outcome().unwrap().accepted_outcome.accepted,
                    AcceptanceState::Unknown
                );
                assert!(context.outcome().unwrap().assessment.is_none());
                for stage in [
                    LifecycleStage::DispatchPrimitive,
                    LifecycleStage::FlushState,
                ] {
                    assert!(
                        context
                            .stage_executions()
                            .iter()
                            .any(|entry| entry.stage == stage
                                && entry.disposition == StageDisposition::Applied)
                    );
                }
                assert!(ContextIrV1::load().items.is_empty());
            }
        }
    }

    #[test]
    fn actual_ir_is_persisted_once_and_parent_task_is_restored() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let parent = TaskSpine::create_envelope("parent", "caller task", "caller");
        TaskSpine::sync_scope(Some(parent.clone()), || {
            let (_, context) = run_local(
                ContextCommand::Grep,
                &[],
                Box::new(|| {
                    assert_ne!(TaskSpine::current().unwrap().task_id, parent.task_id);
                    CommandOutput::local(
                        0,
                        Some(Recording::Search {
                            modeled_baseline: 3,
                            observed_tokens: 3,
                            output_tokens: 2,
                            pattern: "needle".to_owned(),
                            path: "source.rs".to_owned(),
                            elapsed: Duration::ZERO,
                            excerpt: "needle".to_owned(),
                        }),
                    )
                }),
            );
            assert_eq!(TaskSpine::current().unwrap().task_id, parent.task_id);
            assert_eq!(ContextIrV1::load().items.len(), 1);
            assert!(
                context
                    .stage_executions()
                    .iter()
                    .any(|entry| entry.stage == LifecycleStage::RecordContextIr
                        && entry.disposition == StageDisposition::Applied)
            );
            assert!(
                context
                    .stage_executions()
                    .iter()
                    .any(|entry| entry.stage == LifecycleStage::RecordLedger
                        && matches!(entry.disposition, StageDisposition::Skipped(_)))
            );
            assert_eq!(
                context.outcome().unwrap().accepted_outcome.accepted,
                AcceptanceState::Unknown
            );
        });
    }

    #[test]
    fn failed_ir_write_is_not_acknowledged_or_retried_as_an_artifact() {
        let data = crate::core::data_dir::isolated_data_dir();
        std::fs::create_dir(data.path().join("context_ir_v1.json")).unwrap();
        let (_, context) = TaskSpine::sync_scope(None, || {
            run_local(
                ContextCommand::Grep,
                &[],
                Box::new(|| {
                    CommandOutput::local(
                        0,
                        Some(Recording::Search {
                            modeled_baseline: 3,
                            observed_tokens: 3,
                            output_tokens: 2,
                            pattern: "needle".to_owned(),
                            path: "source.rs".to_owned(),
                            elapsed: Duration::ZERO,
                            excerpt: "needle".to_owned(),
                        }),
                    )
                }),
            )
        });
        assert!(data.path().join("context_ir_v1.json").is_dir());
        assert!(context.stage_executions().iter().any(|entry| entry.stage
            == LifecycleStage::RecordContextIr
            && entry.disposition == StageDisposition::Skipped("Context IR persistence failed")));
        assert_eq!(
            context.outcome().unwrap().accepted_outcome.accepted,
            AcceptanceState::Unknown
        );
        assert_eq!(context.completed_stages(), LIFECYCLE_STAGE_ORDER);
    }

    #[test]
    fn primitive_panic_finalizes_without_synthetic_quality_and_restores_parent() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let parent = TaskSpine::create_envelope("parent", "panic caller", "caller");
        TaskSpine::sync_scope(Some(parent.clone()), || {
            let guard =
                crate::cli::dispatch::external_lifecycle_guard("read", None, ToolSurface::Cli);
            let context = guard.context().clone();
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_admitted_local(guard, Box::new(|| panic!("primitive failed")))
            }));
            assert!(panic.is_err());
            assert_eq!(TaskSpine::current().unwrap().task_id, parent.task_id);
            assert_eq!(context.completed_stages(), LIFECYCLE_STAGE_ORDER);
            assert_eq!(
                context.outcome().unwrap().accepted_outcome.accepted,
                AcceptanceState::Unknown
            );
            assert!(context.outcome().unwrap().assessment.is_none());
            assert!(
                context.stage_executions().iter().any(|entry| entry.stage
                    == LifecycleStage::RecordContextIr
                    && entry.disposition == StageDisposition::Skipped("CLI operation panicked"))
            );
        });
    }
}
