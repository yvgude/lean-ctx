// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::time::{Duration, Instant};

struct Fixture {
    directory: tempfile::TempDir,
    executable: PathBuf,
}

impl Fixture {
    fn new(body: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("agent");
        fs::write(&executable, format!(
            "#!/bin/sh\nprintf 'call\\n' >> \"$0.calls\"\nif [ \"$1\" = '--version' ]; then printf 'fixture 1.0\\n'; exit 0; fi\nprintf '%s\\n' \"$@\" > \"$0.args\"\npwd > \"$0.cwd\"\nprintf '%s' \"$LEAN_CTX_PROFILE\" > \"$0.profile\"\n{body}\n"
        )).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            directory,
            executable,
        }
    }

    fn artifact(&self, suffix: &str) -> PathBuf {
        self.executable.with_file_name(format!("agent.{suffix}"))
    }

    fn connector(&self, name: &str) -> Arc<dyn AgentConnector> {
        let info = AgentInfo {
            name: name.into(),
            version: Some("fixture 1.0".into()),
            path: self.executable.clone(),
            capabilities: vec!["execute".into()],
            available: true,
        };
        match name {
            "codex" => Arc::new(super::super::codex::CodexConnector::new(info)),
            "claude-code" => Arc::new(super::super::claude::ClaudeConnector::new(info)),
            "cursor" => Arc::new(super::super::cursor::CursorConnector::new(info)),
            _ => unreachable!(),
        }
    }

    fn adapter(&self, name: &str, policy: PolicyConstraints) -> Arc<AgentConnectorAdapter> {
        Arc::new(
            AgentConnectorAdapter::new(
                self.connector(name),
                AgentExecutionPolicy::new(self.directory.path(), policy, 5_000).unwrap(),
            )
            .unwrap(),
        )
    }

    fn request(&self) -> TaskRequest {
        TaskRequest {
            id: "fixture-task".into(),
            prompt: "literal prompt; $(touch should-not-exist)".into(),
            working_dir: self.directory.path().to_owned(),
            // Functional fixtures should not test host scheduling latency.
            timeout_ms: 5_000,
            model: Some("selected-model".into()),
            max_turns: None,
            profile_name: Some("isolated-profile".into()),
            profile_hash: None,
            delivery_profile: None,
        }
    }
}

fn admitted() -> PolicyConstraints {
    PolicyConstraints {
        allow_remote: true,
        ..PolicyConstraints::default()
    }
}

#[test]
fn real_connectors_share_task_input_and_forward_argv_cwd_profile() {
    for name in ["codex", "claude-code", "cursor"] {
        let fixture = Fixture::new("printf '{\"result\":\"done\"}'");
        let adapter = fixture.adapter(name, admitted());
        let registry = AdapterRegistry::new();
        registry.register_arc(adapter.clone()).unwrap();
        let mut request = fixture.request();
        if name == "claude-code" {
            request.max_turns = Some(3);
        }
        let invocation = adapter.invocation(request.clone());
        let json = serde_json::to_value(&invocation).unwrap();
        assert_eq!(
            json["input"]["agent_task"],
            serde_json::to_value(&request).unwrap()
        );
        let (raw, common) = adapter.execute_registered(&registry, &invocation).unwrap();
        assert_eq!(raw.stdout, "done");
        assert!(common.success);
        assert_eq!(raw.termination, Some(TaskTermination::Exited));
        let args = fs::read_to_string(fixture.artifact("args")).unwrap();
        assert!(args.lines().any(|arg| arg == request.prompt));
        assert!(args.contains(&format!("--\n{}", request.prompt)));
        assert!(args.contains("selected-model"));
        if name == "codex" {
            assert!(args.contains("--sandbox\nread-only"));
            assert!(!args.contains("danger-full-access"));
        }
        if name == "claude-code" {
            assert!(args.contains("--model\nselected-model"));
            assert!(args.contains("--max-turns\n3"));
        }
        assert_eq!(
            fs::read_to_string(fixture.artifact("profile")).unwrap(),
            "isolated-profile"
        );
        assert_eq!(
            PathBuf::from(fs::read_to_string(fixture.artifact("cwd")).unwrap().trim())
                .canonicalize()
                .unwrap(),
            fixture.directory.path().canonicalize().unwrap()
        );
        assert!(!fixture.directory.path().join("should-not-exist").exists());
        assert_eq!(common.observation.task_id, request.id);
        assert_eq!(common.observation.output_tokens, common.output_tokens);
        assert_eq!(common.observation.latency_ms, common.latency_ms);
        assert_eq!(common.observation.output_ref, Some(evidence_ref("done")));
        assert!(common.evidence_ref.is_none());
        assert!(
            !common
                .observation
                .metrics
                .contains_key("provider_reported_cost_micros")
        );
        assert!(
            !common
                .observation
                .metrics
                .contains_key("provider_reported_input_tokens")
        );
        assert!(adapter.manifest().supported_classifications.is_empty());
        assert_eq!(adapter.manifest().provider, "unknown");
    }
}

#[test]
fn invocation_cannot_relax_host_policy_and_rejection_never_spawns() {
    let fixture = Fixture::new("exit 0");
    let policies = [
        PolicyConstraints::default(),
        PolicyConstraints {
            allowed_models: vec!["other-model".into()],
            ..admitted()
        },
        PolicyConstraints {
            allowed_paths: vec!["/not-admitted".into()],
            ..admitted()
        },
        PolicyConstraints {
            require_deterministic: true,
            ..admitted()
        },
        PolicyConstraints {
            require_reversible: true,
            ..admitted()
        },
        PolicyConstraints {
            max_input_tokens: Some(50),
            ..admitted()
        },
        PolicyConstraints {
            max_output_tokens: Some(50),
            ..admitted()
        },
        PolicyConstraints {
            max_latency_ms: Some(1),
            ..admitted()
        },
        PolicyConstraints {
            allowed_data_classifications: vec!["public".into()],
            ..admitted()
        },
    ];
    for policy in policies {
        let adapter = fixture.adapter("codex", policy);
        let mut invocation = adapter.invocation(fixture.request());
        invocation.policy_constraints = admitted();
        assert!(adapter.invoke(invocation).is_err());
        assert!(
            !fixture.artifact("calls").exists(),
            "rejection ran a subprocess"
        );
    }
}

#[test]
fn rejects_identity_input_deadline_and_unsupported_turns_before_spawn() {
    let fixture = Fixture::new("exit 0");
    let adapter = fixture.adapter("codex", admitted());
    let valid = adapter.invocation(fixture.request());
    let mut cases = Vec::new();
    let mut changed = valid.clone();
    changed.capability_id.push_str("-wrong");
    cases.push(changed);
    let mut changed = valid.clone();
    changed.capability_version = "2.0.0".into();
    cases.push(changed);
    let mut changed = valid.clone();
    changed.task_id = "wrong-task".into();
    cases.push(changed);
    let mut changed = valid.clone();
    changed.timeout_ms = 1;
    cases.push(changed);
    let mut changed = valid.clone();
    changed.policy_constraints.allow_remote = false;
    cases.push(changed);
    let mut changed = valid.clone();
    changed.input = CapabilityInput::ShellCommand {
        command: "exit 0".into(),
        workdir: None,
    };
    cases.push(changed);
    for timeout_ms in [0, 5_001] {
        let mut request = fixture.request();
        request.timeout_ms = timeout_ms;
        cases.push(adapter.invocation(request));
    }
    let mut request = fixture.request();
    request.max_turns = Some(1);
    cases.push(adapter.invocation(request));
    for invocation in cases {
        assert!(adapter.invoke(invocation).is_err());
    }
    assert!(!fixture.artifact("calls").exists());
    for name in ["codex", "cursor"] {
        let mut request = fixture.request();
        request.max_turns = Some(1);
        assert!(fixture.connector(name).execute(&request).is_err());
    }
    assert!(!fixture.artifact("calls").exists());
}

#[test]
fn symlink_and_parent_directory_escape_are_rejected_before_spawn() {
    let fixture = Fixture::new("exit 0");
    let outside = tempfile::tempdir().unwrap();
    let link = fixture.directory.path().join("escape");
    symlink(outside.path(), &link).unwrap();
    let adapter = fixture.adapter("codex", admitted());
    for working_dir in [link, fixture.directory.path().join("..")] {
        let mut request = fixture.request();
        request.working_dir = working_dir;
        assert!(adapter.invoke(adapter.invocation(request)).is_err());
    }
    assert!(!fixture.artifact("calls").exists());
}

#[test]
fn registry_requires_same_runtime_not_descriptor_or_identical_replacement() {
    let fixture = Fixture::new("printf done");
    let adapter = fixture.adapter("codex", admitted());
    let registry = AdapterRegistry::new();
    let invocation = adapter.invocation(fixture.request());
    assert!(adapter.execute_registered(&registry, &invocation).is_err());
    registry
        .register_descriptor(adapter.manifest().clone())
        .unwrap();
    assert!(adapter.execute_registered(&registry, &invocation).is_err());
    let replacement = fixture.adapter("codex", admitted());
    registry.register_arc(replacement).unwrap();
    assert!(adapter.execute_registered(&registry, &invocation).is_err());
    assert!(!fixture.artifact("calls").exists());
}

#[test]
fn registered_trait_invocation_runs_the_same_real_connector() {
    let fixture = Fixture::new("printf done");
    let adapter = fixture.adapter("codex", admitted());
    let registry = AdapterRegistry::new();
    registry.register_arc(adapter.clone()).unwrap();
    let manifest = adapter.manifest();
    let lookup = registry
        .lookup(manifest.capability_id.as_str(), &manifest.version)
        .unwrap();
    let expected: Arc<dyn CapabilityAdapter> = adapter.clone();
    assert!(Arc::ptr_eq(&expected, &lookup));
    assert!(
        lookup
            .invoke(adapter.invocation(fixture.request()))
            .unwrap()
            .success
    );
    assert!(fixture.artifact("args").exists());
}

#[test]
fn health_failure_removes_runtime_without_spawning_task() {
    let fixture = Fixture::new("printf done");
    let adapter = fixture.adapter("codex", admitted());
    let registry = AdapterRegistry::new();
    registry.register_arc(adapter.clone()).unwrap();
    assert_eq!(registry.list_available().len(), 1);
    fs::write(&fixture.executable, "#!/bin/sh\nexit 1\n").unwrap();
    assert!(registry.list_available().is_empty());
    assert!(
        adapter
            .execute_registered(&registry, &adapter.invocation(fixture.request()))
            .is_err()
    );
    assert!(!fixture.artifact("args").exists());
}

#[test]
fn timeout_and_signal_are_typed_not_inferred_from_stderr() {
    // This tests terminal classification, not probe scheduling under a busy
    // full-suite run. Leave room for preflight; the child still exceeds budget.
    for (body, timeout, expected) in [
        ("sleep 10", 5_000, CapabilityFailureMode::Timeout),
        (
            "printf 'task timed out' >&2; exit 7",
            5_000,
            CapabilityFailureMode::Internal,
        ),
        ("kill -TERM $$", 5_000, CapabilityFailureMode::Internal),
    ] {
        let fixture = Fixture::new(body);
        let adapter = fixture.adapter("codex", admitted());
        let mut request = fixture.request();
        request.timeout_ms = timeout;
        let common = adapter.invoke(adapter.invocation(request)).unwrap();
        assert!(!common.success);
        assert_eq!(common.observation.failure_mode, Some(expected));
    }
}

#[test]
fn hung_version_probe_is_bounded_and_not_available() {
    let fixture = Fixture::new("exit 0");
    fs::write(&fixture.executable, "#!/bin/sh\nsleep 10\n").unwrap();
    let start = Instant::now();
    assert!(!fixture.connector("codex").health_check().unwrap());
    assert!(start.elapsed() < Duration::from_secs(4));
}

#[test]
fn unknown_usage_stays_unknown_but_explicit_counts_are_retained() {
    for (body, known) in [
        ("printf '{\"usage\":{},\"result\":\"done\"}'", false),
        (
            "printf '{\"usage\":{\"input_tokens\":0,\"output_tokens\":7},\"result\":\"done\"}'",
            true,
        ),
    ] {
        for name in ["codex", "claude-code"] {
            let fixture = Fixture::new(body);
            let adapter = fixture.adapter(name, admitted());
            let common = adapter
                .invoke(adapter.invocation(fixture.request()))
                .unwrap();
            assert_eq!(
                common
                    .observation
                    .metrics
                    .get("provider_reported_input_tokens"),
                known.then_some(&0)
            );
            assert_eq!(
                common
                    .observation
                    .metrics
                    .get("provider_reported_output_tokens"),
                known.then_some(&7)
            );
            assert!(
                !common
                    .observation
                    .metrics
                    .contains_key("provider_reported_cost_micros")
            );
            assert!(common.evidence_ref.is_none());
        }
    }
}
