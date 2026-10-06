// SPDX-License-Identifier: Apache-2.0

//! Explicit local-host boundary: credentials arrive only on operator-owned stdin.

use std::{
    io::{BufRead, Read},
    path::Path,
};

use lean_ctx_protocol::{ExecutionPlanV1, TaskEnvelopeV1};
use serde::Deserialize;
use zeroize::Zeroizing;

#[cfg(test)]
use crate::core::engine_interface::execute_transport_context_view_with_budget;

use super::{EngineCliError, SemanticVersion};
use crate::core::{
    engine_interface::{
        CAPABILITY_ID, CAPABILITY_VERSION, ENGINE_TRANSPORT_POLICY_REF,
        MAX_TRANSPORT_BUDGET_TOKENS, execute_transport_context_view_with_plan,
    },
    execution_ledger::host::{HostReceiptAuthority, digest, parse_key},
};

const REQUEST_LIMIT: usize = 1024 * 1024;

mod outcome;
mod personal_sync;
mod sources;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostRequest {
    schema_version: u32,
    transport_version: u32,
    engine_interface_version: SemanticVersion,
    path: String,
    mode: String,
    task: TaskEnvelopeV1,
    plan: ExecutionPlanV1,
}

pub(super) fn run(args: &[String], input: &mut impl Read) -> Result<String, EngineCliError> {
    if args == ["context-personal-sync", "--host-stdin"] {
        return personal_sync::run(input);
    }
    if args == ["context-checkpoint-continue", "--host-stdin"] {
        let mut reader = std::io::BufReader::new(input);
        let authority = read_framed_authority(&mut reader, None)?;
        let bytes = read_bounded(&mut reader, REQUEST_LIMIT, "invalid_host_request")?;
        let request = serde_json::from_slice::<
            crate::core::execution_ledger::host::HostCheckpointImportRequest,
        >(&bytes)
        .map_err(|_| host_error("invalid_host_request"))?;
        return authority
            .continue_checkpoint(&request)
            .map(|value| value.to_string())
            .map_err(|_| host_error("host_checkpoint_continuation_rejected"));
    }
    if args.first().map(String::as_str) == Some("context-outcome-receipt") {
        return outcome::run(args, input);
    }
    if matches!(
        args.first().map(String::as_str),
        Some("context-sources-receipt" | "context-sources-receipt-v2")
    ) {
        return sources::run(args, input);
    }
    if args.first().map(String::as_str) == Some("context-checkpoint-resume") {
        if args.len() != 4 || args[1] != "--json" || args[3] != "--host-stdin" {
            return Err(EngineCliError::Usage);
        }
        let request = read_request::<
            crate::core::execution_ledger::host::HostCheckpointResumeRequest,
        >(Path::new(&args[2]))?;
        let authority = HostReceiptAuthority::from_reader(input).map_err(EngineCliError::Host)?;
        return authority
            .resume_checkpoint(&request)
            .map(|value| value.to_string())
            .map_err(|_| host_error("host_checkpoint_resume_rejected"));
    }
    if args.first().map(String::as_str) == Some("context-checkpoint-package") {
        if args.len() != 4 || args[1] != "--json" || args[3] != "--host-stdin" {
            return Err(EngineCliError::Usage);
        }
        let request = read_request::<
            crate::core::execution_ledger::host::HostCheckpointPackageRequest,
        >(Path::new(&args[2]))?;
        let authority = HostReceiptAuthority::from_reader(input).map_err(EngineCliError::Host)?;
        return authority
            .package_checkpoint(&request)
            .map(|value| value.to_string())
            .map_err(|_| host_error("host_checkpoint_package_rejected"));
    }
    if args.first().map(String::as_str) == Some("context-checkpoint-sources") {
        if args.len() != 4 || args[1] != "--json" || args[3] != "--host-stdin" {
            return Err(EngineCliError::Usage);
        }
        let request = read_request::<
            crate::core::execution_ledger::host::HostCheckpointImportRequest,
        >(Path::new(&args[2]))?;
        let authority = HostReceiptAuthority::from_reader(input).map_err(EngineCliError::Host)?;
        return authority
            .recheck_checkpoint_sources(&request)
            .map(|value| value.to_string())
            .map_err(|_| host_error("host_checkpoint_sources_rejected"));
    }
    if matches!(
        args.first().map(String::as_str),
        Some("context-checkpoint-import" | "context-checkpoint-continue")
    ) {
        if args.len() != 4 || args[1] != "--json" || args[3] != "--host-stdin" {
            return Err(EngineCliError::Usage);
        }
        let request = read_request::<
            crate::core::execution_ledger::host::HostCheckpointImportRequest,
        >(Path::new(&args[2]))?;
        let authority = HostReceiptAuthority::from_reader(input).map_err(EngineCliError::Host)?;
        if args[0] == "context-checkpoint-continue" {
            return authority
                .continue_checkpoint(&request)
                .map(|value| value.to_string())
                .map_err(|_| host_error("host_checkpoint_continuation_rejected"));
        }
        return authority
            .import_checkpoint(&request)
            .map(|value| value.to_string())
            .map_err(|_| host_error("host_checkpoint_import_rejected"));
    }
    if args.first().map(String::as_str) == Some("context-checkpoint") {
        if args.len() != 4 || args[1] != "--json" || args[3] != "--host-stdin" {
            return Err(EngineCliError::Usage);
        }
        let request = read_request::<crate::core::execution_ledger::host::HostCheckpointRequest>(
            Path::new(&args[2]),
        )?;
        let authority = HostReceiptAuthority::from_reader(input).map_err(EngineCliError::Host)?;
        return authority
            .export_checkpoint(&request)
            .map(|value| value.to_string())
            .map_err(|_| host_error("host_checkpoint_rejected"));
    }
    if args.first().map(String::as_str) == Some("context-outcome") {
        if args.len() != 4 || args[1] != "--json" || args[3] != "--host-stdin" {
            return Err(EngineCliError::Usage);
        }
        let request = read_request::<crate::core::execution_ledger::host::HostOutcomeRequest>(
            Path::new(&args[2]),
        )?;
        let authority = HostReceiptAuthority::from_reader(input).map_err(EngineCliError::Host)?;
        return authority
            .observe_outcome(&request)
            .map(|value| value.to_string())
            .map_err(|_| host_error("host_outcome_rejected"));
    }
    if args == ["signer-info", "--key-stdin"] {
        let bytes = read_bounded(input, 66, "invalid_signing_key")?;
        let text = std::str::from_utf8(&bytes).map_err(|_| host_error("invalid_signing_key"))?;
        let key = parse_key(text.trim()).map_err(EngineCliError::Host)?;
        return Ok(serde_json::json!({
            "schema_version":1, "algorithm":"ed25519",
            "public_key_hex":crate::core::agent_identity::hex_encode(key.verifying_key().as_bytes()),
            "public_key_digest":digest(key.verifying_key().as_bytes()).map_err(EngineCliError::Host)?.as_str(),
        }).to_string());
    }
    if args.first().map(String::as_str) != Some("context-view-receipt")
        || args
            .iter()
            .filter(|arg| arg.as_str() == "--host-stdin")
            .count()
            != 1
    {
        return Err(EngineCliError::Usage);
    }
    let mut view_args: Vec<String> = args
        .iter()
        .filter(|arg| arg.as_str() != "--host-stdin")
        .cloned()
        .collect();
    view_args[0] = "context-view".into();
    let cli = super::parse_cli_args(&view_args)?;
    let request: HostRequest = read_request(&cli.json_file)?;
    let authority = HostReceiptAuthority::from_reader(input).map_err(EngineCliError::Host)?;
    validate_request(&request)?;
    let attempt = authority
        .begin(&request.task, &request.plan)
        .map_err(EngineCliError::Host)?;
    let result = execute_transport_context_view_with_plan(
        &cli.project_root,
        &request.path,
        &request.task,
        &request.plan,
    )
    .map_err(EngineCliError::Engine)?;
    let invocation = result
        .invocation
        .as_ref()
        .ok_or_else(|| host_error("host_engine_evidence_unavailable"))?;
    let observation = result
        .observation
        .as_ref()
        .ok_or_else(|| host_error("host_engine_evidence_unavailable"))?;
    let published = authority
        .publish(&attempt, invocation, observation, &result.view.text)
        .map_err(EngineCliError::Host)?;
    let view: serde_json::Value = serde_json::from_str(&super::encode_response(result)?)
        .map_err(|_| host_error("host_response_unavailable"))?;
    Ok(serde_json::json!({
        "schema_version":1, "engine":view,
        "canonical_receipt":{
            "receipt_id":published.receipt_id, "receipt_ref":published.receipt_ref,
            "receipt_digest":published.receipt_digest, "outcome":"unknown",
        },
    })
    .to_string())
}

fn validate_request(request: &HostRequest) -> Result<(), EngineCliError> {
    super::validate_header(
        request.schema_version,
        request.transport_version,
        &request.engine_interface_version,
    )?;
    request
        .task
        .validate()
        .map_err(|_| host_error("invalid_host_task"))?;
    request
        .plan
        .validate()
        .map_err(|_| host_error("invalid_host_plan"))?;
    if request.path.trim().is_empty()
        || request.mode != "aggressive"
        || request.plan.task_id != request.task.task_id
        || request.plan.model != "local-native"
        || request.plan.provider != "local-native"
        || request.plan.context_plan_id.is_some()
        || request
            .plan
            .context_token_limit()
            .is_none_or(|limit| limit == 0 || limit > MAX_TRANSPORT_BUDGET_TOKENS)
        || !request
            .plan
            .capability_ids
            .iter()
            .any(|id| id.as_str() == CAPABILITY_ID)
        || request.plan.policy_decision_ref.as_deref() != Some(ENGINE_TRANSPORT_POLICY_REF)
        || request.plan.capability_bindings.iter().any(|binding| {
            binding.capability_id.as_str() == CAPABILITY_ID && binding.version != CAPABILITY_VERSION
        })
    {
        return Err(host_error("invalid_host_plan"));
    }
    crate::core::engine_interface::planning::validate_native_plan(
        &request.task,
        &request.plan,
        &lean_ctx_protocol::EnginePolicyAdmissionV1 {
            policy_ref: lean_ctx_protocol::ProtocolReference::new(ENGINE_TRANSPORT_POLICY_REF)
                .map_err(|_| host_error("invalid_host_plan"))?,
            decision: lean_ctx_protocol::EnginePolicyDecisionV1::Admitted,
        },
    )
    .map_err(|_| host_error("invalid_host_plan"))?;
    Ok(())
}

fn read_framed_authority(
    reader: &mut impl BufRead,
    ledger_root: Option<&Path>,
) -> Result<HostReceiptAuthority, EngineCliError> {
    // Compact settings are operator-owned, bounded even without a newline,
    // zeroized after loading and never part of the untrusted request DTO.
    let mut settings = Zeroizing::new(Vec::new());
    reader
        .by_ref()
        .take(16 * 1024 + 1)
        .read_until(b'\n', &mut settings)
        .map_err(|_| host_error("invalid_host_configuration"))?;
    if settings.len() > 16 * 1024 || !settings.ends_with(b"\n") {
        return Err(host_error("invalid_host_configuration"));
    }
    HostReceiptAuthority::from_reader_scoped(&mut settings.as_slice(), ledger_root)
        .map_err(EngineCliError::Host)
}

fn read_request<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, EngineCliError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(path)
        .map_err(|_| host_error("host_request_unavailable"))?;
    if !file
        .metadata()
        .map_err(|_| host_error("host_request_unavailable"))?
        .is_file()
    {
        return Err(host_error("host_request_unavailable"));
    }
    serde_json::from_slice(&read_bounded(
        &mut file,
        REQUEST_LIMIT,
        "host_request_unavailable",
    )?)
    .map_err(|_| host_error("invalid_host_request"))
}

fn read_bounded(
    input: &mut impl Read,
    limit: usize,
    code: &'static str,
) -> Result<Zeroizing<Vec<u8>>, EngineCliError> {
    let mut bytes = Zeroizing::new(Vec::new());
    input
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| host_error(code))?;
    if bytes.len() > limit {
        return Err(host_error(code));
    }
    Ok(bytes)
}

fn host_error(code: &'static str) -> EngineCliError {
    EngineCliError::Host(code)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod transfer_tests;
