// SPDX-License-Identifier: Apache-2.0

//! Host-owned native receipt handoff; never populated from MCP arguments.

use std::{
    path::Path,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use lean_ctx_protocol::{EngineInvocationV1, EngineObservationV1, ExecutionPlanV1, TaskEnvelopeV1};
use rmcp::model::CallToolResult;
use serde_json::{Map, Value};

use crate::core::execution_ledger::host::{HostReceiptAttempt, HostReceiptAuthority};

tokio::task_local! {
    pub(crate) static NATIVE_RECEIPT_CAPTURE: Option<Arc<NativeReceiptCapture>>;
}

pub(crate) fn current_capture() -> Option<Arc<NativeReceiptCapture>> {
    NATIVE_RECEIPT_CAPTURE.try_with(Clone::clone).ok().flatten()
}

/// One immutable lifecycle task, one attempt, one Engine result, one publisher.
pub(crate) struct NativeReceiptCapture {
    authority: Arc<HostReceiptAuthority>,
    task: TaskEnvelopeV1,
    plan: OnceLock<ExecutionPlanV1>,
    attempt: OnceLock<HostReceiptAttempt>,
    execution: OnceLock<(EngineInvocationV1, EngineObservationV1)>,
    publication_claimed: AtomicBool,
}

impl NativeReceiptCapture {
    pub(crate) fn new(authority: Arc<HostReceiptAuthority>, task: TaskEnvelopeV1) -> Self {
        Self {
            authority,
            task,
            plan: OnceLock::new(),
            attempt: OnceLock::new(),
            execution: OnceLock::new(),
            publication_claimed: AtomicBool::new(false),
        }
    }

    pub(crate) fn begin(
        &self,
        task: &TaskEnvelopeV1,
        plan: &ExecutionPlanV1,
    ) -> Result<(), String> {
        if task != &self.task || self.plan.set(plan.clone()).is_err() {
            return Err("mcp_receipt_task_mismatch_or_duplicate".into());
        }
        let handoff = crate::core::context_kernel::bridge::runtime::current_handoff();
        let context = match &handoff {
            crate::core::context_kernel::bridge::runtime::KernelPlanningHandoff::Prepared(
                prepared,
            ) => Some(prepared.decision()),
            _ => None,
        };
        let attempt = self.authority.begin_with_context(task, plan, context)?;
        self.attempt
            .set(attempt)
            .map_err(|_| "mcp_receipt_duplicate_attempt".into())
    }

    pub(crate) fn complete(
        &self,
        invocation: EngineInvocationV1,
        observation: EngineObservationV1,
    ) -> Result<(), String> {
        if self.attempt.get().is_none() {
            return Err("mcp_receipt_attempt_missing".into());
        }
        self.execution
            .set((invocation, observation))
            .map_err(|_| "mcp_receipt_duplicate_execution".into())
    }

    pub(crate) fn publish(&self, result: &CallToolResult) -> Result<Value, String> {
        if self.publication_claimed.swap(true, Ordering::AcqRel) {
            return Err("mcp_receipt_publication_already_claimed".into());
        }
        if result.is_error == Some(true) || result.content.len() != 1 {
            return Err("mcp_receipt_delivery_unavailable".into());
        }
        let text = result.content[0]
            .as_text()
            .ok_or("mcp_receipt_delivery_unavailable")?;
        let attempt = self.attempt.get().ok_or("mcp_receipt_attempt_missing")?;
        let (invocation, observation) = self
            .execution
            .get()
            .ok_or("mcp_receipt_execution_missing")?;
        // The shared publisher checks byte equality with the verified Engine view.
        let published = self
            .authority
            .publish(attempt, invocation, observation, &text.text)?;
        let mut metadata = serde_json::json!({
            "schema_version":1, "receipt_id":published.receipt_id,
            "receipt_ref":published.receipt_ref, "receipt_digest":published.receipt_digest,
            "outcome":"unknown", "delivery":"native_engine_view"
        });
        if let Some(decision) = attempt.context_decision() {
            let bytes = crate::core::canonical::canonical_serialize(decision);
            let digest = crate::core::execution_ledger::host::digest(&bytes)?;
            metadata["context_decision_ref"] =
                format!("artifact://execution/evidence/{}", digest.hex()).into();
        }
        if let Some(protocol) = self.authority.published_context_protocol(
            attempt,
            invocation,
            observation,
            &published,
        )? {
            // Persist the real evaluator's Unknown observation, not acceptance proof.
            let bytes = crate::core::canonical::canonical_serialize(&protocol.accepted_outcome);
            let digest = crate::core::execution_ledger::host::digest(&bytes)?;
            drop(
                crate::core::engine_interface::persist_engine_artifact_content(
                    "execution/evidence",
                    digest.hex(),
                    "json",
                    &bytes,
                )?,
            );
            metadata["outcome_observation_ref"] =
                format!("artifact://execution/evidence/{}", digest.hex()).into();
        }
        Ok(metadata)
    }
}

pub(crate) fn requested(name: &str, args: Option<&Map<String, Value>>) -> bool {
    let Ok((name, args)) = crate::tools::registered::ctx_call::resolve(name, args) else {
        return false;
    };
    name == "ctx_read"
        && args
            .as_ref()
            .and_then(|args| args.get("engine_interface"))
            .and_then(Value::as_str)
            == Some("v1")
}

/// Snapshot explicit host authority once during transport startup, never per request.
pub(crate) fn load_configured_host_authority() -> Result<Option<Arc<HostReceiptAuthority>>, String>
{
    let path = std::env::var_os("LEAN_CTX_RECEIPT_HOST_CONFIG");
    load_host_authority(path.as_deref().map(Path::new))
}

/// Read only the explicitly configured host file, before the MCP transport starts.
/// Never consult tool/session arguments or discover a signing key implicitly.
pub(crate) fn load_host_authority(
    path: Option<&Path>,
) -> Result<Option<Arc<HostReceiptAuthority>>, String> {
    let Some(path) = path else {
        return Ok(None);
    };
    if !path.is_absolute() {
        return Err("mcp_receipt_host_path_invalid".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(not(any(unix, windows)))]
    return Err("mcp_receipt_host_platform_unsupported".into());

    let mut file = options
        .open(path)
        .map_err(|_| "mcp_receipt_host_unavailable")?;
    let metadata = file
        .metadata()
        .map_err(|_| "mcp_receipt_host_unavailable")?;
    if !metadata.is_file() || metadata.len() > 16 * 1024 {
        return Err("mcp_receipt_host_file_invalid".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid takes no pointers and only reads the process credential.
        let effective_uid = unsafe { libc::geteuid() };
        if metadata.mode() & 0o077 != 0 || metadata.uid() != effective_uid {
            return Err("mcp_receipt_host_permissions_invalid".into());
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err("mcp_receipt_host_file_invalid".into());
        }
    }
    HostReceiptAuthority::from_reader(&mut file)
        .map(|host| Some(Arc::new(host)))
        .map_err(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ed25519_dalek::{Signature, SigningKey};
    use lean_ctx_protocol::{AcceptanceState, ReceiptDocumentV1};
    use sha2::{Digest, Sha256};

    fn host_config(ledger: &Path) -> Vec<u8> {
        let key = SigningKey::from_bytes(&[53; 32]);
        let now = chrono::Utc::now();
        serde_json::to_vec(&serde_json::json!({
            "schema_version":1,
            "signing_key_hex":crate::core::agent_identity::hex_encode(&key.to_bytes()),
            "signer":{
                "key_id":"mcp-native-test-key",
                "public_key_digest":format!("sha256:{}", crate::core::agent_identity::hex_encode(&Sha256::digest(key.verifying_key().as_bytes()))),
                "admitted_at":(now-chrono::Duration::days(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs,true),
                "expires_at":(now+chrono::Duration::days(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs,true),
                "revoked_at":null
            },
            "ledger_path":ledger
        })).unwrap()
    }

    fn authority(ledger: &Path) -> Arc<HostReceiptAuthority> {
        Arc::new(HostReceiptAuthority::from_reader(&mut host_config(ledger).as_slice()).unwrap())
    }

    #[test]
    fn receipt_selection_uses_semantic_v1_identity_only() {
        let args = serde_json::json!({"engine_interface":"v1"})
            .as_object()
            .unwrap()
            .clone();
        assert!(requested("ctx_read", Some(&args)));
        assert!(!requested("ctx_read", None));
        assert!(!requested("ctx_shell", Some(&args)));
        let wrapped = serde_json::json!({"name":"ctx_read","arguments":args})
            .as_object()
            .unwrap()
            .clone();
        assert!(requested("ctx_call", Some(&wrapped)));
    }

    #[test]
    fn explicit_loader_rejects_unsafe_files_without_discovering_authority() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("receipt-host.json");
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        options
            .open(&path)
            .unwrap()
            .write_all(&host_config(&directory.path().join("ledger.jsonl")))
            .unwrap();
        assert!(load_host_authority(None).unwrap().is_none());
        assert!(load_host_authority(Some(Path::new("relative.json"))).is_err());
        assert!(load_host_authority(Some(directory.path())).is_err());
        assert!(load_host_authority(Some(&path)).unwrap().is_some());
        let oversized = directory.path().join("oversized.json");
        std::fs::write(&oversized, vec![b' '; 16 * 1024 + 1]).unwrap();
        assert_eq!(
            load_host_authority(Some(&oversized)).err().unwrap(),
            "mcp_receipt_host_file_invalid"
        );
        #[cfg(unix)]
        {
            use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
            let fifo = directory.path().join("fifo");
            let fifo_name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
            // SAFETY: the live CString is NUL-terminated and names only this fixture's FIFO.
            let created = unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) };
            assert_eq!(created, 0);
            assert_eq!(
                load_host_authority(Some(&fifo)).err().unwrap(),
                "mcp_receipt_host_file_invalid"
            );
            let link = directory.path().join("linked.json");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(load_host_authority(Some(&link)).is_err());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(load_host_authority(Some(&path)).is_err());
        }
    }

    fn request(path: &Path, id: &str) -> rmcp::model::CallToolRequestParams {
        serde_json::from_value(serde_json::json!({
            "name":"ctx_read", "arguments":{"path":path,"mode":"aggressive","engine_interface":"v1"},
            "_meta":{"requestId":id}
        })).unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_mcp_path_publishes_unknown_for_exact_body_and_keeps_task_replays_scoped() {
        // Preserve real guards while isolating other tests' process-global role
        // and budget state; this runs the test harness, not MCP startup cleanup.
        if std::env::var_os("LEAN_CTX_NATIVE_RECEIPT_SUBPROCESS").is_none() {
            // Snapshot the environment/cwd only while other tests cannot lend
            // this child their temporary policy, configuration or path settings.
            let _environment = crate::core::data_dir::test_env_lock();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "server::native_receipts::tests::real_mcp_path_publishes_unknown_for_exact_body_and_keeps_task_replays_scoped", "--nocapture"])
                .env("LEAN_CTX_NATIVE_RECEIPT_SUBPROCESS", "1")
                .env("LEAN_CTX_ROLE", "coder")
                .output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let _data = crate::core::data_dir::isolated_data_dir();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("native.rs");
        std::fs::write(
            &path,
            "pub fn native_receipt_marker() { let value = 42; }\n".repeat(40),
        )
        .unwrap();
        let ledger_path = directory.path().join("ledger.jsonl");
        let mut server =
            crate::tools::LeanCtxServer::new_with_project_root(directory.path().to_str());
        server.native_receipt_authority = Some(authority(&ledger_path));
        let response = server
            .call_tool_guarded(request(&path, "receipt-call-1"))
            .await
            .unwrap();
        assert_ne!(response.is_error, Some(true), "{response:?}");
        let metadata = &response.meta.as_ref().unwrap().0["canonical_receipt"];
        assert_eq!(metadata["outcome"], "unknown");
        let digest = metadata["receipt_digest"]
            .as_str()
            .unwrap()
            .strip_prefix("sha256:")
            .unwrap();
        let data = crate::core::data_dir::lean_ctx_data_dir().unwrap();
        let bytes = std::fs::read(
            data.join("execution/receipts")
                .join(format!("{digest}.json")),
        )
        .unwrap();
        let receipt = ReceiptDocumentV1::from_canonical_bytes(&bytes).unwrap();
        let signature =
            Signature::from_slice(&STANDARD.decode(&receipt.signature).unwrap()).unwrap();
        SigningKey::from_bytes(&[53; 32])
            .verifying_key()
            .verify_strict(&receipt.signing_bytes().unwrap(), &signature)
            .unwrap();
        assert_eq!(receipt.outcome.state, AcceptanceState::Unknown);
        let body = &response.content[0].as_text().unwrap().text;
        assert!(body.contains("native_receipt_marker"));
        let output_tokens = crate::core::tokens::count_tokens(body) as u64;
        assert!(
            receipt
                .values
                .iter()
                .any(|value| value.name == "output_tokens" && value.value == Some(output_tokens))
        );
        let replay = server
            .call_tool_guarded(request(&path, "receipt-call-1"))
            .await
            .unwrap();
        assert_eq!(
            replay.meta.as_ref().unwrap().0["canonical_receipt"],
            *metadata
        );
        let next = server
            .call_tool_guarded(request(&path, "receipt-call-2"))
            .await
            .unwrap();
        assert_ne!(
            next.meta.as_ref().unwrap().0["canonical_receipt"]["receipt_digest"],
            metadata["receipt_digest"]
        );
        let mut wrapped = request(&path, "receipt-call-3");
        let arguments = wrapped.arguments.take().unwrap();
        wrapped.name = "ctx_call".into();
        wrapped.arguments = Some(
            serde_json::from_value(serde_json::json!({
                "name":"ctx_read", "arguments":arguments
            }))
            .unwrap(),
        );
        let wrapped_response = server.call_tool_guarded(wrapped).await.unwrap();
        assert_eq!(
            wrapped_response.meta.as_ref().unwrap().0["canonical_receipt"]["outcome"],
            "unknown"
        );
        assert_eq!(&wrapped_response.content[0].as_text().unwrap().text, body);
        let serialized = serde_json::to_string(&response).unwrap();
        assert!(!serialized.contains(&crate::core::agent_identity::hex_encode(&[53; 32])));
        use crate::core::execution_ledger::{ExecutionEvent, ExecutionLedgerStore};
        let events = ExecutionLedgerStore::new(ledger_path)
            .load_verified()
            .unwrap();
        let recorded: std::collections::BTreeSet<_> = events
            .iter()
            .filter_map(|event| {
                if let ExecutionEvent::CanonicalReceiptRecorded { receipt_digest, .. } = event {
                    Some(receipt_digest.as_str())
                } else {
                    None
                }
            })
            .collect();
        let expected: std::collections::BTreeSet<_> = [
            metadata,
            &next.meta.as_ref().unwrap().0["canonical_receipt"],
            &wrapped_response.meta.as_ref().unwrap().0["canonical_receipt"],
        ]
        .into_iter()
        .map(|value| value["receipt_digest"].as_str().unwrap())
        .collect();
        assert_eq!(expected.len(), 3);
        assert_eq!(recorded, expected);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ExecutionEvent::CanonicalReceiptRecorded { .. }))
                .count(),
            3
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ExecutionEvent::OutcomeRecorded { .. }))
        );
    }

    #[test]
    fn terminal_capture_refuses_changed_delivery_and_duplicate_publication() {
        use crate::core::engine_interface::{NativeContextEngine, planning};
        let _data = crate::core::data_dir::isolated_data_dir();
        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        let ledger_path = root.join("ledger.jsonl");
        let task: TaskEnvelopeV1 = serde_json::from_value(serde_json::json!({
            "schema_version":1,"task_id":"capture-task","trace_id":"capture-trace",
            "project_id":"capture-project","session_id":"capture-session","agent_id":"capture-agent",
            "complexity":"unknown","created_at":"2026-01-01T00:00:00Z"
        })).unwrap();
        let admission = lean_ctx_protocol::EnginePolicyAdmissionV1 {
            policy_ref: lean_ctx_protocol::ProtocolReference::new("policy:capture-test").unwrap(),
            decision: lean_ctx_protocol::EnginePolicyDecisionV1::Admitted,
        };
        let plan = planning::native_plan(&task, &admission).unwrap();
        let capture = NativeReceiptCapture::new(authority(&ledger_path), task.clone());
        capture.begin(&task, &plan).unwrap();
        let (invocation, observation) = NativeContextEngine::with_root(&root)
            .unwrap()
            .execute_ctx_read_rooted_snapshot_with_plan(
                root.join("captured.rs").to_str().unwrap(),
                "fn original() {}",
                admission,
                &task,
                &plan,
            )
            .unwrap();
        capture.complete(invocation, observation).unwrap();
        let changed = CallToolResult::success(vec![rmcp::model::ContentBlock::text(
            "changed post-processing",
        )]);
        assert!(capture.publish(&changed).is_err());
        assert_eq!(
            capture.publish(&changed).unwrap_err(),
            "mcp_receipt_publication_already_claimed"
        );
        use crate::core::execution_ledger::{ExecutionEvent, ExecutionLedgerStore};
        let events = ExecutionLedgerStore::new(ledger_path)
            .load_verified()
            .unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ExecutionEvent::TaskStarted { .. }));
    }
}
