//! Production `ctx_read` bridge for the first receipt-backed Engine operation.

use std::sync::{Arc, Mutex};

use lean_ctx_protocol::{
    EngineObservationStatusV1, EnginePolicyAdmissionV1, EnginePolicyDecisionV1, ProtocolReference,
};
use rmcp::ErrorData;
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::core::engine_interface::NativeContextEngine;
use crate::server::context_gate::PreDispatchResult;

#[derive(Clone, Default)]
pub(super) struct SourceSnapshot(Arc<Mutex<Option<CapturedSource>>>);

struct CapturedSource {
    input: String,
    canonical_path: String,
}

impl SourceSnapshot {
    pub(super) fn code_hint(&self, path: &str) -> Option<String> {
        let snapshot = self.0.lock().ok()?;
        let source = snapshot.as_ref()?;
        crate::core::intelligence_runtime::code_security::note(path, &source.input)
    }

    fn capture_rooted(&self, source: crate::tools::ctx_read::RootedRead) {
        let mut snapshot = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *snapshot = Some(CapturedSource {
            input: source.content,
            canonical_path: source.canonical_path,
        });
    }

    pub(super) fn record(
        self,
        project_root: &str,
        policy_admission: EnginePolicyAdmissionV1,
    ) -> Result<String, String> {
        let snapshot = self
            .0
            .lock()
            .map_err(|_| "Engine source snapshot lock poisoned".to_owned())?
            .take();
        record_aggressive_snapshot(snapshot, project_root, policy_admission)
    }

    pub(super) fn record_if_enabled(
        self,
        project_root: &str,
        policy_admission: Option<EnginePolicyAdmissionV1>,
        output: &mut String,
    ) -> Option<String> {
        policy_admission.and_then(|admission| match self.record(project_root, admission) {
            Ok(verified_output) => {
                *output = verified_output;
                None
            }
            Err(error) => Some(stable_warning(&error)),
        })
    }
}

pub(super) fn read_source(
    enabled: bool,
    path: &str,
    project_root: &str,
    snapshot: &SourceSnapshot,
) -> Result<String, std::io::Error> {
    if !enabled {
        let content = crate::tools::ctx_read::read_file_lossy(path)?;
        if crate::core::intelligence_runtime::code_security::enabled_for(path) {
            // A transient copy of the admitted input only. Hints never enter
            // the source/cache/receipt payload and cached reads are not rescanned.
            snapshot.capture_rooted(crate::tools::ctx_read::RootedRead {
                content: content.clone(),
                canonical_path: path.to_owned(),
            });
        }
        return Ok(content);
    }
    let source = crate::tools::ctx_read::read_file_lossy_rooted(path, project_root)?;
    let content = source.content.clone();
    snapshot.capture_rooted(source);
    Ok(content)
}

pub(super) fn interface_schema() -> Value {
    json!({
        "type": "string",
        "enum": ["v1"],
        "description": "Opt into one receipt-backed Engine v1 invocation (single-path aggressive reads only)"
    })
}

pub(super) fn interface_v1_requested(args: &Map<String, Value>) -> Result<bool, ErrorData> {
    match args.get("engine_interface") {
        None => Ok(false),
        Some(Value::String(version)) if version == "v1" => Ok(true),
        Some(_) => Err(ErrorData::invalid_params(
            "engine_interface must be the string \"v1\" when provided",
            None,
        )),
    }
}

pub(super) fn validate_v1_request_shape(
    args: &Map<String, Value>,
    enabled: bool,
) -> Result<(), ErrorData> {
    if !enabled {
        return Ok(());
    }
    if args.contains_key("paths") {
        return Err(ErrorData::invalid_params(
            "engine_interface=\"v1\" supports only single-path ctx_read",
            None,
        ));
    }
    if args.get("mode").and_then(Value::as_str) != Some("aggressive") {
        return Err(ErrorData::invalid_params(
            "engine_interface=\"v1\" requires mode=\"aggressive\"",
            None,
        ));
    }
    for unsupported in [
        "raw",
        "start_line",
        "offset",
        "limit",
        "aggressiveness",
        "protect",
    ] {
        if args.contains_key(unsupported) {
            return Err(ErrorData::invalid_params(
                format!("engine_interface=\"v1\" does not support the {unsupported} parameter"),
                None,
            ));
        }
    }
    Ok(())
}

pub(super) fn require_aggressive(
    enabled: bool,
    mode: &str,
    project_root: &str,
    path: &str,
    policy_admission: &mut Option<EnginePolicyAdmissionV1>,
) -> Result<(), ErrorData> {
    if enabled && mode != "aggressive" {
        return Err(reject_after_admission(
            project_root,
            path,
            policy_admission,
            "effective_mode_not_aggressive",
            mode,
        ));
    }
    Ok(())
}

pub(super) fn reject_non_text_extension(
    enabled: bool,
    project_root: &str,
    path: &str,
    policy_admission: &mut Option<EnginePolicyAdmissionV1>,
) -> Result<(), ErrorData> {
    if enabled && crate::core::binary_detect::is_llm_viewable_image(path) {
        return Err(reject_after_admission(
            project_root,
            path,
            policy_admission,
            "unsupported_input_image",
            "image",
        ));
    }
    if enabled && crate::core::binary_detect::has_binary_extension(path) {
        return Err(reject_after_admission(
            project_root,
            path,
            policy_admission,
            "unsupported_input_binary",
            "binary_extension",
        ));
    }
    Ok(())
}

pub(super) fn reject_read_failure_if_enabled(
    enabled: bool,
    resolved_mode: &str,
    project_root: &str,
    path: &str,
    policy_admission: &mut Option<EnginePolicyAdmissionV1>,
) -> Result<(), ErrorData> {
    if enabled && resolved_mode == "error" {
        return Err(reject_after_admission(
            project_root,
            path,
            policy_admission,
            "source_read_failed",
            resolved_mode,
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct GateAdmissionIdentityV1<'a> {
    schema_version: u32,
    requested_mode: &'a str,
    overridden_mode: Option<&'a str>,
    reason: Option<&'a str>,
    pressure_downgraded: bool,
    budget_blocked: bool,
    triage_filter_level: u8,
}

#[derive(Serialize)]
struct PostAdmissionRejectionIdentityV1<'a> {
    schema_version: u32,
    admitted_policy_ref: &'a str,
    reason_code: &'a str,
    detail: &'a str,
}

/// Convert the actual pre-dispatch decision into the Engine admission record.
pub(super) fn admission_from_gate(
    gate: &PreDispatchResult,
    requested_mode: &str,
) -> Result<EnginePolicyAdmissionV1, String> {
    let identity = GateAdmissionIdentityV1 {
        schema_version: 1,
        requested_mode,
        overridden_mode: gate.overridden_mode.as_deref(),
        reason: gate.reason,
        pressure_downgraded: gate.pressure_downgraded,
        budget_blocked: gate.budget_blocked,
        triage_filter_level: gate.triage_filter_level,
    };
    let bytes = crate::core::canonical::canonical_serialize(&identity);
    let digest = crate::core::agent_identity::hex_encode(&Sha256::digest(bytes));
    Ok(EnginePolicyAdmissionV1 {
        policy_ref: ProtocolReference::new(format!(
            "policy:ctx-read-context-gate-v1:sha256:{digest}"
        ))
        .map_err(|error| error.to_string())?,
        decision: if gate.budget_blocked
            || gate.pressure_downgraded
            || gate.overridden_mode.is_some()
            || gate.triage_filter_level > 0
        {
            EnginePolicyDecisionV1::Rejected
        } else {
            EnginePolicyDecisionV1::Admitted
        },
    })
}

pub(super) fn admission_or_reject(
    enabled: bool,
    gate: &PreDispatchResult,
    requested_mode: &str,
    project_root: &str,
    path: &str,
) -> Result<Option<EnginePolicyAdmissionV1>, ErrorData> {
    if !enabled {
        if gate.budget_blocked {
            return Err(ErrorData::invalid_params(
                gate.budget_warning
                    .clone()
                    .unwrap_or_else(|| "Agent token budget exceeded".to_owned()),
                None,
            ));
        }
        return Ok(None);
    }
    let admission = admission_from_gate(gate, requested_mode)
        .map_err(|error| ErrorData::internal_error(error, None))?;
    if admission.decision == EnginePolicyDecisionV1::Rejected {
        let receipt_ref = record_policy_rejection(project_root, path, admission)
            .map_err(|error| ErrorData::internal_error(stable_warning(&error), None))?;
        return Err(ErrorData::invalid_params(
            format!("engine_interface=\"v1\" rejected by context gate; receipt_ref={receipt_ref}"),
            None,
        ));
    }
    Ok(Some(admission))
}

fn reject_after_admission(
    project_root: &str,
    path: &str,
    policy_admission: &mut Option<EnginePolicyAdmissionV1>,
    reason_code: &'static str,
    detail: &str,
) -> ErrorData {
    let Some(admitted) = policy_admission.take() else {
        return ErrorData::internal_error(
            "Engine v1 post-admission rejection has no admission identity",
            None,
        );
    };
    let identity = PostAdmissionRejectionIdentityV1 {
        schema_version: 1,
        admitted_policy_ref: admitted.policy_ref.as_str(),
        reason_code,
        detail,
    };
    let digest = crate::core::agent_identity::hex_encode(&Sha256::digest(
        crate::core::canonical::canonical_serialize(&identity),
    ));
    let rejected = EnginePolicyAdmissionV1 {
        policy_ref: match ProtocolReference::new(format!(
            "policy:ctx-read-runtime-rejection-v1:sha256:{digest}"
        )) {
            Ok(reference) => reference,
            Err(error) => return ErrorData::internal_error(error.to_string(), None),
        },
        decision: EnginePolicyDecisionV1::Rejected,
    };
    match record_policy_rejection(project_root, path, rejected) {
        Ok(receipt_ref) => ErrorData::invalid_params(
            format!(
                "engine_interface=\"v1\" rejected after admission; reason={reason_code}; receipt_ref={receipt_ref}"
            ),
            None,
        ),
        Err(error) => ErrorData::internal_error(stable_warning(&error), None),
    }
}

/// Keep user-visible fallback deterministic and free of OS/path error text.
pub(super) fn stable_warning(error: &str) -> String {
    let mut warning = "[ENGINE RECEIPT WARNING] code=engine_record_unavailable".to_owned();
    for key in ["receipt_ref", "recovery_ref"] {
        let marker = format!("{key}=");
        let Some(value) = error
            .split_ascii_whitespace()
            .find_map(|token| token.strip_prefix(&marker))
            .map(|value| value.trim_end_matches([';', ',']))
        else {
            continue;
        };
        if ProtocolReference::new(value.to_owned()).is_ok() {
            warning.push(' ');
            warning.push_str(key);
            warning.push('=');
            warning.push_str(value);
        }
    }
    warning
}

/// Record the exact input snapshot captured by the cold read worker.
/// Omitted Engine calls may hit legacy cache; explicit v1 calls force fresh.
fn record_aggressive_snapshot(
    snapshot: Option<CapturedSource>,
    project_root: &str,
    policy_admission: EnginePolicyAdmissionV1,
) -> Result<String, String> {
    let source = snapshot.ok_or_else(|| "Engine v1 source snapshot unavailable".to_owned())?;
    record_aggressive_invocation(
        project_root,
        &source.canonical_path,
        &source.input,
        policy_admission,
    )
}

pub(super) fn record_policy_rejection(
    project_root: &str,
    path: &str,
    policy_admission: EnginePolicyAdmissionV1,
) -> Result<String, String> {
    let engine = NativeContextEngine::with_root(project_root)?;
    let (_, observation) = engine.execute_ctx_read_rejection(path, policy_admission)?;
    if observation.status != EngineObservationStatusV1::Rejected {
        return Err("native Engine policy rejection returned a non-rejected status".to_owned());
    }
    observation
        .receipt_link
        .map(|link| link.receipt_ref.as_str().to_owned())
        .ok_or_else(|| "native Engine policy rejection omitted its receipt link".to_owned())
}

/// Record the native aggressive-compression capability against the source
/// snapshot already acquired by `ctx_read`; Engine applies active redaction.
fn record_aggressive_invocation(
    project_root: &str,
    path: &str,
    input: &str,
    policy_admission: EnginePolicyAdmissionV1,
) -> Result<String, String> {
    let engine = NativeContextEngine::with_root(project_root)?;
    let capture = crate::server::native_receipts::current_capture();
    let (invocation, observation) = if let Some(task) =
        crate::core::task_spine::TaskSpine::scoped_current()
    {
        let plan = crate::core::engine_interface::planning::native_plan(&task, &policy_admission)?;
        if let Some(capture) = &capture {
            capture.begin(&task, &plan)?;
        }
        engine.execute_ctx_read_rooted_snapshot_with_plan(
            path,
            input,
            policy_admission,
            &task,
            &plan,
        )?
    } else {
        // Legacy unscoped callers remain explicitly unplanned.
        if capture.is_some() {
            return Err("mcp_receipt_scoped_task_required".into());
        }
        engine.execute_ctx_read_rooted_snapshot(path, input, policy_admission)?
    };
    let receipt_link = observation
        .receipt_link
        .as_ref()
        .ok_or_else(|| "native Engine terminal observation omitted its receipt link".to_owned())?;
    if observation.status != EngineObservationStatusV1::Succeeded {
        let recovery_ref = observation
            .failure
            .as_ref()
            .and_then(|failure| failure.recovery_ref.as_ref())
            .map_or("none", ProtocolReference::as_str);
        return Err(format!(
            "Engine status={:?}; receipt_ref={}; recovery_ref={recovery_ref}",
            observation.status,
            receipt_link.receipt_ref.as_str()
        ));
    }
    tracing::debug!(
        status = ?observation.status,
        receipt_ref = receipt_link.receipt_ref.as_str(),
        "ctx_read Engine observation recorded"
    );
    let view = crate::core::engine_interface::verified_output_view(&invocation, &observation)
        .map_err(|error| format!("Engine verified output unavailable: {error:?}"))?;
    if let Some(capture) = capture {
        capture.complete(invocation, observation)?;
    }
    Ok(view.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(blocked: bool, overridden_mode: Option<&str>) -> PreDispatchResult {
        PreDispatchResult {
            overridden_mode: overridden_mode.map(str::to_owned),
            reason: overridden_mode.map(|_| "fixture-override"),
            pressure_downgraded: overridden_mode.is_some(),
            budget_blocked: blocked,
            budget_warning: None,
            triage_filter_level: u8::from(overridden_mode.is_some()),
        }
    }

    #[test]
    fn gate_admission_preserves_decision_and_exact_policy_identity() {
        let admitted = admission_from_gate(&gate(false, None), "aggressive").unwrap();
        let rejected = admission_from_gate(&gate(true, None), "aggressive").unwrap();
        let overridden =
            admission_from_gate(&gate(false, Some("signatures")), "aggressive").unwrap();

        assert_eq!(admitted.decision, EnginePolicyDecisionV1::Admitted);
        assert_eq!(rejected.decision, EnginePolicyDecisionV1::Rejected);
        assert_eq!(overridden.decision, EnginePolicyDecisionV1::Rejected);
        assert_ne!(admitted.policy_ref, rejected.policy_ref);
        assert_ne!(admitted.policy_ref, overridden.policy_ref);
        assert!(
            admitted
                .policy_ref
                .as_str()
                .starts_with("policy:ctx-read-context-gate-v1:sha256:")
        );
    }

    #[test]
    fn rejected_gate_persists_a_receipt_without_native_output() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rejected.rs");
        std::fs::write(&file, "fn must_not_execute() {}").unwrap();
        let error = admission_or_reject(
            true,
            &gate(true, None),
            "aggressive",
            &dir.path().to_string_lossy(),
            &file.to_string_lossy(),
        )
        .unwrap_err();
        assert!(error.message.contains("receipt_ref=receipt:sha256:"));
        let data_dir = crate::core::data_dir::lean_ctx_data_dir().unwrap();
        assert!(!data_dir.join("engine-interface/v1/outputs").exists());
        let receipt = std::fs::read_dir(data_dir.join("engine-interface/v1/receipts"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(receipt).unwrap()).unwrap();
        assert_eq!(receipt["observation"]["status"], "rejected");
        assert_eq!(
            receipt["invocation"]["policy_admission"]["decision"],
            "rejected"
        );
    }

    #[test]
    fn enabled_recording_never_silently_accepts_a_missing_snapshot() {
        let admission = admission_from_gate(&gate(false, None), "aggressive").unwrap();
        let error = record_aggressive_snapshot(None, "/tmp", admission).unwrap_err();
        assert_eq!(error, "Engine v1 source snapshot unavailable");
    }

    #[test]
    fn warning_redacts_nondeterministic_storage_error_details() {
        let warning = stable_warning(
            "persist Engine receipt: /tmp/private: errno 13; recovery_ref=recovery:sha256:abc",
        );
        assert_eq!(
            warning,
            "[ENGINE RECEIPT WARNING] code=engine_record_unavailable recovery_ref=recovery:sha256:abc"
        );
        assert!(!warning.contains("/tmp/private"));
        assert!(!warning.contains("errno"));
    }

    #[test]
    fn engine_v1_rejects_non_aggressive_effective_mode() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("mode.rs");
        std::fs::write(&file, "fn mode_fixture() {}").unwrap();
        let mut admitted = Some(admission_from_gate(&gate(false, None), "aggressive").unwrap());
        require_aggressive(
            true,
            "aggressive",
            &dir.path().to_string_lossy(),
            &file.to_string_lossy(),
            &mut admitted,
        )
        .unwrap();
        let error = require_aggressive(
            true,
            "full",
            &dir.path().to_string_lossy(),
            &file.to_string_lossy(),
            &mut admitted,
        )
        .unwrap_err();
        assert!(
            error
                .message
                .contains("reason=effective_mode_not_aggressive")
        );
        assert!(error.message.contains("receipt_ref=receipt:sha256:"));
        let mut legacy = None;
        require_aggressive(
            false,
            "full",
            &dir.path().to_string_lossy(),
            &file.to_string_lossy(),
            &mut legacy,
        )
        .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn production_bridge_uses_cache_snapshot_without_second_disk_read() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("snapshot.rs");
        let source = "fn cache_snapshot_marker() {}\n".repeat(40);
        std::fs::write(&file, &source).unwrap();
        let path = file.to_string_lossy().into_owned();
        let admission = admission_from_gate(&gate(false, None), "aggressive").unwrap();
        let snapshot = SourceSnapshot::default();

        let captured = read_source(true, &path, &dir.path().to_string_lossy(), &snapshot).unwrap();
        assert_eq!(captured, source);
        std::fs::write(&file, "fn changed_disk_marker() {}\n").unwrap();
        snapshot
            .record(&dir.path().to_string_lossy(), admission)
            .unwrap();

        let output_dir = crate::core::data_dir::lean_ctx_data_dir()
            .unwrap()
            .join("engine-interface/v1/outputs");
        let output_path = std::fs::read_dir(output_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let output = std::fs::read_to_string(output_path).unwrap();
        assert!(output.contains("cache_snapshot_marker"));
        assert!(!output.contains("changed_disk_marker"));
    }

    #[test]
    fn scoped_bridge_delivers_verified_output_and_never_promotes_legacy_task_identity() {
        use crate::core::task_spine::TaskSpine;
        let _data = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bound.rs");
        let source = "fn bound_bridge_marker() {}\n".repeat(40);
        std::fs::write(&file, &source).unwrap();
        let task = TaskSpine::create_envelope("read", "bridge-session", "bridge-agent");
        assert!(TaskSpine::scoped_current().is_none());
        let run = || {
            let snapshot = SourceSnapshot::default();
            read_source(
                true,
                &file.to_string_lossy(),
                &dir.path().to_string_lossy(),
                &snapshot,
            )
            .unwrap();
            let mut output = "unrelated legacy output".to_owned();
            let warning = snapshot.record_if_enabled(
                &dir.path().to_string_lossy(),
                Some(admission_from_gate(&gate(false, None), "aggressive").unwrap()),
                &mut output,
            );
            assert!(warning.is_none());
            assert!(output.contains("bound_bridge_marker"));
            assert!(!output.contains("unrelated legacy output"));
            output
        };
        let planned_output = TaskSpine::sync_scope(Some(task.clone()), run);
        assert_eq!(run(), planned_output);
        let data = crate::core::data_dir::lean_ctx_data_dir().unwrap();
        let artifacts: Vec<serde_json::Value> =
            std::fs::read_dir(data.join("engine-interface/v1/receipts"))
                .unwrap()
                .map(|entry| {
                    serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap()
                })
                .collect();
        assert_eq!(artifacts.len(), 2);
        let task_digest = crate::core::canonical::canonical_serialize(&task);
        let expected = format!(
            "task:sha256:{}",
            crate::core::agent_identity::hex_encode(&Sha256::digest(task_digest))
        );
        let bound: Vec<_> = artifacts
            .iter()
            .filter(|artifact| {
                artifact["invocation"]["source_refs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r.as_str() == Some(&expected))
            })
            .collect();
        assert_eq!(
            bound.len(),
            1,
            "legacy thread-local identity must not create a canonical plan"
        );
        let output_digest = bound[0]["observation"]["output_digest"]
            .as_str()
            .unwrap()
            .strip_prefix("sha256:")
            .unwrap();
        let persisted = std::fs::read_to_string(
            data.join("engine-interface/v1/outputs")
                .join(format!("{output_digest}.txt")),
        )
        .unwrap();
        assert_eq!(planned_output, persisted);
    }
}
