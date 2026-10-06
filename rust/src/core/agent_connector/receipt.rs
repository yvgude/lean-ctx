//! Canonical receipt linkage for provider-backed connector executions.
//!
//! Connector output is not itself a signed provider receipt.  This module
//! records the observed usage as child Engine evidence and delegates terminal
//! receipt construction, signing, and publication to the execution ledger.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::Utc;
use ed25519_dalek::{Signature, Verifier};
use lean_ctx_protocol::{
    AcceptanceState, ContextBalanceV1, EngineInvocationIdV1, EngineInvocationV1,
    EngineMeasurementV1, EngineObservationStatusV1, EngineObservationV1, EngineOperationV1,
    EnginePolicyAdmissionV1, EnginePolicyDecisionV1, EngineReceiptLinkV1,
    EngineValueClassificationV1, ExecutionPlanV1, ProjectId, ProtocolReference, ReceiptChainLinkV1,
    ReceiptDocumentV1, ReceiptEvidenceKindV1, ReceiptEvidenceRefV1, ReceiptId,
    ReceiptOutcomeLinkV1, ReceiptTerminalStatusV1, ReceiptValueV1, SemanticVersion, SessionId,
    Sha256Digest, SignatureStatus, TaskComplexity, TaskEnvelopeV1, TaskId, TraceId, UtcTimestamp,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::traits::{TaskRequest, TokenUsage};
use crate::core::agent_identity::{current_agent_id, get_or_create_keypair, get_stored_public_key};
use crate::core::canonical::canonical_serialize;
use crate::core::context_kernel::provider_normalization::{
    NormalizedUsage, decimal_value_to_micros, normalize_anthropic, normalize_openai,
};
use crate::core::execution_ledger::{
    ExecutionEvent, ExecutionLedgerStore, ReceiptSignerAdmissionV1, record_canonical_engine_receipt,
};

/// Link and measured values available only after a signed receipt is stored.
#[derive(Debug, Clone)]
pub(crate) struct ReceiptLink {
    pub(crate) reference: String,
    pub(crate) provider_cost_micros: u64,
    pub(crate) tokens_used: TokenUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ObservedProviderEvidence {
    schema_version: u32,
    connector: String,
    provider: String,
    task_id: String,
    requested_model: String,
    selected_model: String,
    fresh_input_tokens: u64,
    cached_input_tokens: Option<u64>,
    output_tokens: u64,
    reasoning_tokens: Option<u64>,
    provider_cost_micros: u64,
    stdout_digest: String,
}

impl ObservedProviderEvidence {
    fn tokens_used(&self) -> TokenUsage {
        TokenUsage {
            input_tokens: self.fresh_input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: self.cached_input_tokens.unwrap_or(0),
            cache_write_tokens: 0,
        }
    }
}

/// Record a receipt only for explicit provider JSON usage and cost evidence.
///
/// Failure to parse, sign, or persist evidence deliberately leaves the run
/// observed but unlinked; it never changes the connector's execution result.
pub(crate) fn record_provider_receipt(
    connector: &str,
    provider: &str,
    request: &TaskRequest,
    stdout: &str,
    duration_ms: u64,
) -> Option<ReceiptLink> {
    let evidence = observed_provider_evidence(connector, provider, request, stdout)?;
    record_canonical_provider_receipt(request, &evidence, duration_ms).ok()
}

/// Return the final human-visible agent response from a structured CLI stream.
/// Evidence parsing keeps the raw stream; task evaluation must score the answer,
/// not its JSON framing.
pub(crate) fn visible_output(raw_stdout: &str) -> String {
    let mut answer = None;
    for value in json_documents(raw_stdout) {
        for candidate in [
            value.get("result"),
            value.get("text"),
            value
                .get("message")
                .and_then(|message| message.get("content")),
            value.get("item").and_then(|item| item.get("text")),
            value.get("item").and_then(|item| item.get("content")),
        ] {
            if let Some(text) = candidate
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                answer = Some(text.to_owned());
            }
        }
    }
    answer.unwrap_or_else(|| raw_stdout.to_owned())
}

/// Verify the canonical receipt and every linked Engine evidence artifact.
pub(crate) fn verify_receipt_reference(reference: &str) -> anyhow::Result<bool> {
    let (receipt, _) = read_canonical_receipt(reference)?;
    if !verify_receipt_signature(&receipt)? {
        return Ok(false);
    }
    for evidence in &receipt.evidence_refs {
        let (directory, _) = evidence_artifact_location(evidence.uri.as_str())?;
        crate::core::engine_artifact::read_content(directory, evidence.digest.hex(), "json")
            .map_err(anyhow::Error::msg)?;
    }
    Ok(true)
}

fn verify_receipt_signature(receipt: &ReceiptDocumentV1) -> anyhow::Result<bool> {
    // ReceiptSignerV1 declares ExternalTrustStore: a verifier may only use an
    // already-pinned public key. Never call get_or_create_keypair here because
    // an unknown signer must fail closed without mutating identity state.
    let trust_store = LocalReceiptTrustStore::load()?;
    let verifying_key = trust_store.resolve(&receipt.signer.key_id)?;
    let signature_bytes = STANDARD.decode(&receipt.signature)?;
    let signature = Signature::from_slice(&signature_bytes)?;
    let signing_bytes = receipt.signing_bytes().map_err(anyhow::Error::msg)?;
    Ok(verifying_key.verify(&signing_bytes, &signature).is_ok())
}

/// Verify portable receipt payloads against the host's admitted signer, never
/// against a public key supplied by the evidence bundle itself.
pub(crate) fn verify_bundled_receipt(
    reference: &str,
    files: &std::collections::BTreeMap<String, Vec<u8>>,
) -> anyhow::Result<bool> {
    let digest = canonical_receipt_digest(reference)?;
    let prefix = format!("execution-receipts/receipt-{}", digest.hex());
    let Some(bytes) = files.get(&format!("{prefix}/receipt")) else {
        return Ok(false);
    };
    if sha256_digest(bytes) != digest {
        return Ok(false);
    }
    let receipt = ReceiptDocumentV1::from_canonical_bytes(bytes).map_err(anyhow::Error::msg)?;
    if !verify_receipt_signature(&receipt)? {
        return Ok(false);
    }
    for (index, evidence) in receipt.evidence_refs.iter().enumerate() {
        let (_, label) = evidence_artifact_location(evidence.uri.as_str())?;
        let Some(bytes) = files.get(&format!("{prefix}/{label}-{index}")) else {
            return Ok(false);
        };
        if sha256_digest(bytes) != evidence.digest {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Verify signature/evidence and bind the receipt to one exact connector task.
pub(crate) fn verify_receipt_reference_for_task(
    reference: &str,
    expected_task_id: &str,
) -> anyhow::Result<bool> {
    if !verify_receipt_reference(reference)? {
        return Ok(false);
    }
    let (receipt, _) = read_canonical_receipt(reference)?;
    Ok(receipt.lineage.task_id.as_str() == expected_task_id)
}

/// Return authoritative measured tokens and provider cost after full verification.
pub(crate) fn verified_receipt_usage_for_task(
    reference: &str,
    expected_task_id: &str,
) -> anyhow::Result<(u64, u64)> {
    if !verify_receipt_reference_for_task(reference, expected_task_id)? {
        anyhow::bail!("canonical receipt does not match task");
    }
    let (receipt, _) = read_canonical_receipt(reference)?;
    let value = |name: &str| -> anyhow::Result<u64> {
        let mut matches = receipt.values.iter().filter(|value| value.name == name);
        let first = matches
            .next()
            .ok_or_else(|| anyhow::anyhow!("canonical receipt lacks {name}"))?;
        if matches.next().is_some() {
            anyhow::bail!("canonical receipt duplicates {name}");
        }
        match (first.classification, first.value) {
            (lean_ctx_protocol::ReceiptValueClassificationV1::Measured, Some(value)) => Ok(value),
            _ => anyhow::bail!("canonical receipt omits measured {name}"),
        }
    };
    let fresh = value("fresh_input_tokens")?;
    let cached = value("cached_input_tokens")?;
    let output = value("output_tokens")?;
    let tokens = fresh
        .checked_add(cached)
        .and_then(|total| total.checked_add(output))
        .ok_or_else(|| anyhow::anyhow!("canonical receipt token total overflow"))?;
    Ok((tokens, value("provider_cost_micros")?))
}

/// Load the complete, locally persisted evidence for a receipt reference.
///
/// The caller receives only fixed, safe archive paths. It can include these
/// bytes in a portable evidence bundle without reopening the host data
/// directory to path traversal. A receipt that fails local verification is
/// never exported as evidence.
pub(crate) fn receipt_artifacts(reference: &str) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    let (receipt, receipt_bytes) = read_canonical_receipt(reference)?;
    let verified = verify_receipt_reference(reference)?;
    if !verified {
        anyhow::bail!("canonical receipt '{reference}' failed local verification");
    }
    let receipt_key = format!("receipt-{}", canonical_receipt_digest(reference)?.hex());
    let mut artifacts = vec![(
        format!("execution-receipts/{receipt_key}/receipt"),
        receipt_bytes,
    )];
    for (index, evidence) in receipt.evidence_refs.iter().enumerate() {
        let (directory, label) = evidence_artifact_location(evidence.uri.as_str())?;
        let contents =
            crate::core::engine_artifact::read_content(directory, evidence.digest.hex(), "json")
                .map_err(anyhow::Error::msg)?;
        artifacts.push((
            format!("execution-receipts/{receipt_key}/{label}-{index}"),
            contents,
        ));
    }
    Ok(artifacts)
}

fn read_canonical_receipt(reference: &str) -> anyhow::Result<(ReceiptDocumentV1, Vec<u8>)> {
    let digest = canonical_receipt_digest(reference)?;
    let bytes =
        crate::core::engine_artifact::read_content("execution/receipts", digest.hex(), "json")
            .map_err(anyhow::Error::msg)?;
    let actual = sha256_digest(&bytes);
    if actual != digest {
        anyhow::bail!("canonical receipt bytes do not match reference digest");
    }
    let receipt = ReceiptDocumentV1::from_canonical_bytes(&bytes).map_err(anyhow::Error::msg)?;
    Ok((receipt, bytes))
}

pub(crate) fn canonical_receipt_digest(reference: &str) -> anyhow::Result<Sha256Digest> {
    let digest = reference
        .strip_prefix("id:sha256:")
        .filter(|value| !value.is_empty() && !value.contains(['/', '\\']))
        .ok_or_else(|| anyhow::anyhow!("invalid canonical receipt reference"))?;
    Sha256Digest::new(format!("sha256:{digest}")).map_err(anyhow::Error::msg)
}

/// Read-only trust view for locally produced receipts. The current agent id is
/// the explicit admission allow-list; public key bytes are loaded, never
/// generated, by the resolver. Remote consumers provide their own equivalent
/// external trust store for other signer ids.
struct LocalReceiptTrustStore {
    key_id: String,
    verifying_key: ed25519_dalek::VerifyingKey,
}

impl LocalReceiptTrustStore {
    fn load() -> anyhow::Result<Self> {
        let key_id = current_agent_id().to_owned();
        let verifying_key = get_stored_public_key(&key_id).map_err(anyhow::Error::msg)?;
        Ok(Self {
            key_id,
            verifying_key,
        })
    }

    fn resolve(&self, key_id: &str) -> anyhow::Result<&ed25519_dalek::VerifyingKey> {
        if self.key_id != key_id {
            anyhow::bail!("receipt signer '{key_id}' is not admitted by local trust store");
        }
        Ok(&self.verifying_key)
    }
}

fn evidence_artifact_location(uri: &str) -> anyhow::Result<(&'static str, &'static str)> {
    if uri == "artifact://engine/observation" {
        Ok(("execution/evidence", "observation"))
    } else if uri.starts_with("artifact://engine/receipt/") {
        Ok(("engine-interface/v1/receipts", "engine-receipt"))
    } else if uri.starts_with("artifact://provider-run/quality/") {
        Ok(("execution/evidence", "provider-run-quality"))
    } else if uri.starts_with("artifact://provider-run/outcome/") {
        Ok(("execution/evidence", "provider-run-outcome"))
    } else if uri.starts_with("artifact://provider-run/measurement/") {
        Ok(("execution/evidence", "provider-run-measurement"))
    } else {
        anyhow::bail!("unsupported canonical receipt evidence URI '{uri}'");
    }
}

fn observed_provider_evidence(
    connector: &str,
    provider: &str,
    request: &TaskRequest,
    stdout: &str,
) -> Option<ObservedProviderEvidence> {
    let stdout_digest = format!("blake3:{}", blake3::hash(stdout.as_bytes()).to_hex());
    json_documents(stdout).into_iter().find_map(|value| {
        let usage = normalize_connector_usage(connector, &value, request.model.as_deref());
        let fresh_input_tokens = usage.fresh_input_tokens?;
        let output_tokens = usage.output_tokens?;
        let provider_cost_micros = explicit_provider_cost_micros(&value)?;
        let selected_model = non_empty(&usage.model)
            .or_else(|| {
                request
                    .model
                    .as_deref()
                    .filter(|model| !model.trim().is_empty())
            })?
            .to_owned();
        let requested_model = request
            .model
            .as_deref()
            .filter(|model| !model.trim().is_empty())
            .unwrap_or(&selected_model)
            .to_owned();

        Some(ObservedProviderEvidence {
            schema_version: 1,
            connector: connector.to_owned(),
            provider: provider.to_owned(),
            task_id: request.id.clone(),
            requested_model,
            selected_model,
            fresh_input_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            output_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            provider_cost_micros,
            stdout_digest: stdout_digest.clone(),
        })
    })
}

fn json_documents(stdout: &str) -> Vec<Value> {
    let mut values = serde_json::from_str(stdout)
        .ok()
        .into_iter()
        .collect::<Vec<_>>();
    values.extend(
        stdout
            .lines()
            .filter_map(|line| serde_json::from_str(line.trim()).ok()),
    );
    values
}

fn normalize_connector_usage(
    connector: &str,
    value: &Value,
    requested_model: Option<&str>,
) -> NormalizedUsage {
    match connector {
        "claude-code" => normalize_anthropic(value, requested_model, 0),
        // Codex and Cursor CLI JSON both use OpenAI-style `usage` names when
        // they expose token evidence.  A cursor receipt still identifies
        // Cursor as its provider below; no upstream provider is inferred.
        _ => normalize_openai(value, requested_model, 0),
    }
}

fn explicit_provider_cost_micros(value: &Value) -> Option<u64> {
    let root = value.get("response").unwrap_or(value);
    [
        root.get("total_cost_usd"),
        root.get("cost_usd"),
        root.get("usage")
            .and_then(|usage| usage.get("total_cost_usd")),
        root.get("usage").and_then(|usage| usage.get("cost_usd")),
        root.get("usage").and_then(|usage| usage.get("cost")),
    ]
    .into_iter()
    .flatten()
    .find_map(decimal_value_to_micros)
}

fn record_canonical_provider_receipt(
    request: &TaskRequest,
    evidence: &ObservedProviderEvidence,
    duration_ms: u64,
) -> anyhow::Result<ReceiptLink> {
    let task_id = TaskId::try_from(request.id.clone()).map_err(anyhow::Error::msg)?;
    let trace_id =
        TraceId::try_from(format!("trace:connector:{}", request.id)).map_err(anyhow::Error::msg)?;
    let project_id =
        ProjectId::try_from("project:agent-connector".to_owned()).map_err(anyhow::Error::msg)?;
    let session_id = SessionId::try_from(format!("session:connector:{}", request.id))
        .map_err(anyhow::Error::msg)?;
    let billed_input_tokens = evidence
        .fresh_input_tokens
        .saturating_add(evidence.cached_input_tokens.unwrap_or(0));
    let signing_key = get_or_create_keypair(current_agent_id()).map_err(anyhow::Error::msg)?;
    let ledger = ExecutionLedgerStore::from_default().map_err(anyhow::Error::msg)?;
    let capability_id = lean_ctx_protocol::CapabilityId::try_from(format!(
        "capability://provider-observation/{}",
        evidence.connector
    ))
    .map_err(anyhow::Error::msg)?;
    let capability_version = SemanticVersion::new("1.0.0").map_err(anyhow::Error::msg)?;
    let policy_ref = ProtocolReference::new("policy:provider-observation-v1:record-only")
        .map_err(anyhow::Error::msg)?;
    let input_ref = ProtocolReference::new(format!("input:connector:{}", request.id))
        .map_err(anyhow::Error::msg)?;
    let input_digest = sha256_digest(&canonical_serialize(evidence));
    let invocation_id = EngineInvocationIdV1::new(format!(
        "invocation:connector:{}:{}",
        request.id,
        short_digest(&sha256_digest(canonical_serialize(evidence).as_slice()))
    ))
    .map_err(anyhow::Error::msg)?;
    if let Some(reference) =
        existing_canonical_receipt_reference(&ledger, request.id.as_str(), invocation_id.as_str())?
    {
        read_canonical_receipt(&reference)?;
        return Ok(ReceiptLink {
            reference,
            provider_cost_micros: evidence.provider_cost_micros,
            tokens_used: evidence.tokens_used(),
        });
    }
    // Admit local observation recording, not a retrospective provider execution.
    let plan_id = lean_ctx_protocol::PlanId::try_from(format!("plan:connector:{}", request.id))
        .map_err(anyhow::Error::msg)?;
    let plan = ExecutionPlanV1 {
        schema_version: 1,
        plan_id: plan_id.clone(),
        task_id: task_id.clone(),
        context_budget_tokens: billed_input_tokens,
        context_budget_policy: None,
        context_strategy: lean_ctx_protocol::ContextStrategy::Balanced,
        knowledge_refs: Vec::new(),
        capability_ids: vec![capability_id.clone()],
        model: evidence.selected_model.clone(),
        provider: evidence.provider.clone(),
        reasoning_allocation_milli: 0,
        max_retries: 0,
        fallback_refs: Vec::new(),
        stop_condition: lean_ctx_protocol::StopCondition::OnCompletion,
        expected_cost_micros: evidence.provider_cost_micros,
        expected_quality_milli: 0,
        expected_latency_ms: duration_ms,
        policy_decision_ref: Some(policy_ref.as_str().to_owned()),
        scheduler_decision_ref: None,
        estimates: None,
        executor_agent_id: None,
        context_plan_id: None,
        capability_bindings: vec![lean_ctx_protocol::CapabilityBindingV1 {
            capability_id: capability_id.clone(),
            version: capability_version.as_str().to_owned(),
            manifest_digest: None,
        }],
        extensions: Default::default(),
    };
    let issued_at = retry_timestamp(&ledger, task_id.as_str(), invocation_id.as_str())?;
    let task = TaskEnvelopeV1 {
        schema_version: 1,
        task_id,
        trace_id,
        project_id,
        session_id,
        agent_id: lean_ctx_protocol::AgentId::try_from(current_agent_id())
            .map_err(anyhow::Error::msg)?,
        complexity: TaskComplexity::Low,
        created_at: issued_at.as_str().to_owned(),
        parent_task_id: None,
        tenant_id: None,
        intent: None,
        task_class: Some("provider-usage-observation".to_owned()),
        risk_class: None,
        quality_requirement_milli: None,
        cost_budget_micros: None,
        latency_budget_ms: Some(request.timeout_ms),
        data_classification: None,
        region_policy_ref: None,
        model_policy_ref: None,
        context_state_ref: None,
        outcome_contract_ref: None,
        extensions: Default::default(),
    };
    let mut source_refs = vec![input_ref.clone()];
    source_refs.extend(
        crate::core::engine_interface::planning::binding_refs(&task, &plan)
            .map_err(anyhow::Error::msg)?,
    );
    let invocation = EngineInvocationV1 {
        schema_version: 1,
        invocation_id: invocation_id.clone(),
        engine: lean_ctx_protocol::ResolvedLocalEngineIdentityV1 {
            engine_id: format!("agent-connector:{}", evidence.connector),
            engine_version: capability_version.clone(),
        },
        operation: EngineOperationV1 {
            capability_id: capability_id.clone(),
            capability_version: capability_version.clone(),
        },
        input_ref: input_ref.clone(),
        input_digest,
        source_refs,
        policy_admission: EnginePolicyAdmissionV1 {
            policy_ref: policy_ref.clone(),
            decision: EnginePolicyDecisionV1::Admitted,
        },
    };
    let output_digest = sha256_digest(evidence.stdout_digest.as_bytes());
    let mut observation_without_link = EngineObservationV1 {
        schema_version: 1,
        invocation_id: invocation_id.clone(),
        status: EngineObservationStatusV1::Succeeded,
        output_ref: Some(
            ProtocolReference::new(format!("artifact://connector/stdout/{}", request.id))
                .map_err(anyhow::Error::msg)?,
        ),
        output_digest: Some(output_digest),
        source_lineage: invocation.source_refs.clone(),
        measurements: vec![
            EngineMeasurementV1 {
                name: "fresh_input_tokens".to_owned(),
                unit: "tokens".to_owned(),
                classification: EngineValueClassificationV1::Measured,
                value: Some(evidence.fresh_input_tokens),
            },
            EngineMeasurementV1 {
                name: "cached_input_tokens".to_owned(),
                unit: "tokens".to_owned(),
                classification: if evidence.cached_input_tokens.is_some() {
                    EngineValueClassificationV1::Measured
                } else {
                    EngineValueClassificationV1::Unavailable
                },
                value: evidence.cached_input_tokens,
            },
            EngineMeasurementV1 {
                name: "output_tokens".to_owned(),
                unit: "tokens".to_owned(),
                classification: EngineValueClassificationV1::Measured,
                value: Some(evidence.output_tokens),
            },
            EngineMeasurementV1 {
                name: "provider_cost_micros".to_owned(),
                unit: "microdollars".to_owned(),
                classification: EngineValueClassificationV1::Measured,
                value: Some(evidence.provider_cost_micros),
            },
            EngineMeasurementV1 {
                name: "latency_ms".to_owned(),
                unit: "milliseconds".to_owned(),
                classification: EngineValueClassificationV1::Measured,
                value: Some(duration_ms),
            },
        ],
        failure: None,
        receipt_link: None,
    };
    let engine_bytes = crate::core::engine_interface::canonical_engine_receipt_artifact_bytes(
        &invocation,
        &observation_without_link,
    );
    let engine_digest = sha256_digest(&engine_bytes);
    crate::core::engine_interface::persist_engine_artifact_content(
        "engine-interface/v1/receipts",
        engine_digest.hex(),
        "json",
        &engine_bytes,
    )
    .map_err(anyhow::Error::msg)?;
    observation_without_link.receipt_link = Some(EngineReceiptLinkV1 {
        schema_version: 1,
        receipt_id: ReceiptId::try_from(format!("engine-{}", engine_digest.hex()))
            .map_err(anyhow::Error::msg)?,
        receipt_ref: ProtocolReference::new(format!("receipt:{}", engine_digest.as_str()))
            .map_err(anyhow::Error::msg)?,
        receipt_digest: engine_digest.clone(),
        invocation_id: invocation_id.clone(),
    });
    let observation_digest = sha256_digest(&crate::core::canonical::canonical_serialize(
        &observation_without_link,
    ));
    let record = crate::core::execution_ledger::CanonicalReceiptRecordV1 {
        context_balance: ContextBalanceV1 {
            original_tokens: billed_input_tokens,
            materialized_tokens: billed_input_tokens,
            delivered_tokens: billed_input_tokens,
            provider_billed_tokens: billed_input_tokens,
        },
        status: ReceiptTerminalStatusV1::Succeeded,
        values: vec![
            measured_value(
                "fresh_input_tokens",
                evidence.fresh_input_tokens,
                observation_digest.clone(),
            ),
            optional_measured_value(
                "cached_input_tokens",
                evidence.cached_input_tokens,
                observation_digest.clone(),
            ),
            measured_value(
                "output_tokens",
                evidence.output_tokens,
                observation_digest.clone(),
            ),
            measured_value(
                "provider_cost_micros",
                evidence.provider_cost_micros,
                observation_digest.clone(),
            ),
            measured_value("latency_ms", duration_ms, observation_digest.clone()),
        ],
        outcome: ReceiptOutcomeLinkV1 {
            state: AcceptanceState::Unknown,
            outcome_id: None,
            outcome_ref: None,
            acceptance_evidence_digest: None,
        },
        evidence_refs: vec![ReceiptEvidenceRefV1 {
            kind: ReceiptEvidenceKindV1::Runtime,
            uri: ProtocolReference::new(format!(
                "artifact://engine/receipt/{}",
                engine_digest.hex()
            ))
            .map_err(anyhow::Error::msg)?,
            digest: engine_digest,
            media_type: "application/json".to_owned(),
            signature_status: SignatureStatus::NotSigned,
        }],
        chain: ReceiptChainLinkV1 {
            chain_id: format!("connector:{}", request.id),
            sequence_number: 1,
            previous_receipt_id: None,
            previous_signature_digest: None,
        },
        issued_at,
        signer_admission: ReceiptSignerAdmissionV1 {
            key_id: current_agent_id().to_owned(),
            public_key_digest: sha256_digest(signing_key.verifying_key().as_bytes()),
            admitted_at: UtcTimestamp::new("2000-01-01T00:00:00Z").map_err(anyhow::Error::msg)?,
            expires_at: UtcTimestamp::new("9999-12-31T23:59:59Z").map_err(anyhow::Error::msg)?,
            revoked_at: None,
        },
    };
    let published = record_canonical_engine_receipt(
        &task,
        &plan,
        &invocation,
        &observation_without_link,
        record,
        &signing_key,
        &ledger,
    )?;
    Ok(ReceiptLink {
        reference: published.receipt_ref,
        provider_cost_micros: evidence.provider_cost_micros,
        tokens_used: evidence.tokens_used(),
    })
}

fn measured_value(name: &str, value: u64, evidence_digest: Sha256Digest) -> ReceiptValueV1 {
    ReceiptValueV1 {
        name: name.to_owned(),
        unit: "count".to_owned(),
        classification: lean_ctx_protocol::ReceiptValueClassificationV1::Measured,
        value: Some(value),
        evidence_digests: vec![evidence_digest],
        formula_digest: None,
        price_table_digest: None,
        reconciliation_digest: None,
    }
}

fn optional_measured_value(
    name: &str,
    value: Option<u64>,
    evidence_digest: Sha256Digest,
) -> ReceiptValueV1 {
    match value {
        Some(value) => measured_value(name, value, evidence_digest),
        None => ReceiptValueV1 {
            name: name.to_owned(),
            unit: "count".to_owned(),
            classification: lean_ctx_protocol::ReceiptValueClassificationV1::Unavailable,
            value: None,
            evidence_digests: Vec::new(),
            formula_digest: None,
            price_table_digest: None,
            reconciliation_digest: None,
        },
    }
}

fn sha256_digest(bytes: &[u8]) -> Sha256Digest {
    let digest = crate::core::agent_identity::hex_encode(&Sha256::digest(bytes));
    Sha256Digest::new(format!("sha256:{digest}")).expect("SHA-256 digest is canonical")
}

fn short_digest(digest: &Sha256Digest) -> &str {
    &digest.hex()[..16]
}

fn retry_timestamp(
    ledger: &ExecutionLedgerStore,
    task_id: &str,
    invocation_id: &str,
) -> anyhow::Result<UtcTimestamp> {
    if let Some(timestamp) = ledger
        .by_task_verified(task_id)
        .map_err(anyhow::Error::msg)?
        .into_iter()
        .rev()
        .find_map(|event| match event {
            ExecutionEvent::CanonicalReceiptRecorded {
                invocation_id: existing,
                timestamp,
                ..
            } if existing == invocation_id => Some(timestamp),
            _ => None,
        })
    {
        return UtcTimestamp::new(timestamp).map_err(anyhow::Error::msg);
    }
    UtcTimestamp::new(Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .map_err(anyhow::Error::msg)
}

fn existing_canonical_receipt_reference(
    ledger: &ExecutionLedgerStore,
    task_id: &str,
    invocation_id: &str,
) -> anyhow::Result<Option<String>> {
    Ok(ledger
        .by_task_verified(task_id)
        .map_err(anyhow::Error::msg)?
        .into_iter()
        .rev()
        .find_map(|event| match event {
            ExecutionEvent::CanonicalReceiptRecorded {
                invocation_id: existing,
                receipt_ref,
                ..
            } if existing == invocation_id => Some(receipt_ref),
            _ => None,
        }))
}

fn non_empty(value: &str) -> Option<&str> {
    (!value.trim().is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn request() -> TaskRequest {
        TaskRequest {
            id: "task-1".to_owned(),
            prompt: "test".to_owned(),
            working_dir: PathBuf::from("."),
            timeout_ms: 1_000,
            model: Some("claude-test".to_owned()),
            max_turns: None,
            profile_name: None,
            profile_hash: None,
            delivery_profile: None,
        }
    }

    #[test]
    fn explicit_usage_and_cost_create_a_verifiable_receipt_link() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let output = json!({
            "model": "claude-test",
            "usage": {
                "input_tokens": 100,
                "cache_read_input_tokens": 25,
                "output_tokens": 40
            },
            "total_cost_usd": "0.000123"
        })
        .to_string();

        let evidence = observed_provider_evidence("claude-code", "anthropic", &request(), &output)
            .expect("explicit provider evidence should parse");
        let link = record_canonical_provider_receipt(&request(), &evidence, 42)
            .expect("canonical publisher should accept explicit provider evidence");
        assert!(link.reference.starts_with("id:sha256:"));

        let (receipt, bytes) = read_canonical_receipt(&link.reference).expect("canonical receipt");
        let expected_reference = format!("id:sha256:{}", sha256_digest(&bytes).hex());
        assert_eq!(link.reference, expected_reference);
        assert_eq!(
            receipt.receipt_id,
            receipt.derived_receipt_id().expect("receipt id")
        );
        assert_eq!(receipt.outcome.state, AcceptanceState::Unknown);
        assert_eq!(receipt.chain.sequence_number, 1);
        assert!(
            receipt
                .evidence_refs
                .iter()
                .any(|evidence| evidence.kind == ReceiptEvidenceKindV1::Runtime)
        );
        assert!(
            receipt
                .evidence_refs
                .iter()
                .any(|evidence| evidence.kind == ReceiptEvidenceKindV1::Measurement)
        );

        let retry = record_provider_receipt("claude-code", "anthropic", &request(), &output, 99)
            .expect("retry should remain idempotent");
        assert_eq!(retry.reference, link.reference);

        assert_eq!(link.provider_cost_micros, 123);
        assert_eq!(link.tokens_used.input_tokens, 100);
        assert_eq!(link.tokens_used.cache_read_tokens, 25);
        assert!(verify_receipt_reference(&link.reference).expect("offline verifier should run"));
        assert_eq!(
            verified_receipt_usage_for_task(&link.reference, "task-1").unwrap(),
            (165, 123)
        );
        assert!(verified_receipt_usage_for_task(&link.reference, "another-task").is_err());
        assert!(
            receipt.lineage.capabilities[0]
                .capability_id
                .as_str()
                .starts_with("capability://provider-observation/")
        );
        let artifacts = receipt_artifacts(&link.reference).expect("receipt artifacts export");
        assert_eq!(artifacts.len(), 3);
        assert!(artifacts.iter().all(|(path, bytes)| {
            path.starts_with("execution-receipts/receipt-") && !bytes.is_empty()
        }));
    }

    #[test]
    fn missing_cost_or_usage_never_creates_a_receipt() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let missing_cost = json!({
            "model": "claude-test",
            "usage": {"input_tokens": 100, "output_tokens": 40}
        })
        .to_string();
        let missing_usage = json!({"model": "claude-test", "total_cost_usd": "0.01"}).to_string();

        assert!(
            record_provider_receipt("claude-code", "anthropic", &request(), &missing_cost, 1)
                .is_none()
        );
        assert!(
            record_provider_receipt("claude-code", "anthropic", &request(), &missing_usage, 1)
                .is_none()
        );
    }

    #[test]
    fn omitted_cached_usage_is_unavailable_not_zero_measured() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let output = json!({
            "model": "claude-test",
            "usage": {"input_tokens": 100, "output_tokens": 40},
            "total_cost_usd": "0.000123"
        })
        .to_string();

        let link = record_provider_receipt("claude-code", "anthropic", &request(), &output, 42)
            .expect("explicit provider evidence should produce a receipt");
        let (receipt, _) = read_canonical_receipt(&link.reference).expect("canonical receipt");
        let cached = receipt
            .values
            .iter()
            .find(|value| value.name == "cached_input_tokens")
            .expect("cached metric");
        assert_eq!(
            cached.classification,
            lean_ctx_protocol::ReceiptValueClassificationV1::Unavailable
        );
        assert!(cached.value.is_none());
        assert!(
            verified_receipt_usage_for_task(&link.reference, "task-1").is_err(),
            "unknown cached usage must not authorize a zero-usage retry"
        );
    }

    #[test]
    fn cursor_openai_style_json_is_supported_without_pricing_estimate() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let output = json!({
            "model": "cursor-model",
            "usage": {
                "prompt_tokens": 20,
                "prompt_tokens_details": {"cached_tokens": 5},
                "completion_tokens": 8,
                "cost": "0.000004"
            }
        })
        .to_string();

        let link = record_provider_receipt("cursor", "cursor", &request(), &output, 7)
            .expect("explicit cursor evidence should produce a receipt");

        assert_eq!(link.provider_cost_micros, 4);
        assert_eq!(link.tokens_used.input_tokens, 15);
        assert!(verify_receipt_reference(&link.reference).expect("offline verifier should run"));
    }

    #[test]
    fn unknown_signer_fails_closed_without_creating_identity_state() {
        let data_dir = crate::core::data_dir::isolated_data_dir();
        let output = json!({
            "model": "claude-test",
            "usage": {"input_tokens": 1, "output_tokens": 1},
            "total_cost_usd": "0.000001"
        })
        .to_string();
        let link = record_provider_receipt("claude-code", "anthropic", &request(), &output, 1)
            .expect("explicit provider evidence should produce a receipt");
        let key_dir = data_dir.path().join("keys");
        let key_path = key_dir.join(format!("{}.key", current_agent_id()));
        let public_path = key_dir.join(format!("{}.pub", current_agent_id()));
        assert!(key_path.exists());
        assert!(public_path.exists());
        std::fs::remove_file(&key_path).expect("remove signing key fixture");
        std::fs::remove_file(&public_path).expect("remove trusted key fixture");

        assert!(verify_receipt_reference(&link.reference).is_err());
        assert!(!key_path.exists());
        assert!(!public_path.exists());
    }

    #[test]
    fn wrong_or_invalid_trusted_key_never_verifies_receipt() {
        let data_dir = crate::core::data_dir::isolated_data_dir();
        let output = json!({
            "model": "claude-test",
            "usage": {"input_tokens": 1, "output_tokens": 1},
            "total_cost_usd": "0.000001"
        })
        .to_string();
        let link = record_provider_receipt("claude-code", "anthropic", &request(), &output, 1)
            .expect("explicit provider evidence should produce a receipt");
        let public_path = data_dir
            .path()
            .join("keys")
            .join(format!("{}.pub", current_agent_id()));
        std::fs::write(
            &public_path,
            ed25519_dalek::SigningKey::from_bytes(&[17_u8; 32])
                .verifying_key()
                .to_bytes(),
        )
        .expect("replace trusted key fixture");
        assert!(!verify_receipt_reference(&link.reference).expect("wrong key is parseable"));

        let invalid_key = (0_u8..=u8::MAX)
            .find_map(|prefix| {
                let mut candidate = [0_u8; 32];
                candidate[0] = prefix;
                std::fs::write(&public_path, candidate).expect("replace key fixture");
                verify_receipt_reference(&link.reference)
                    .is_err()
                    .then_some(candidate)
            })
            .expect("find malformed 32-byte public key");
        std::fs::write(&public_path, invalid_key).expect("persist malformed key fixture");
        assert!(verify_receipt_reference(&link.reference).is_err());
    }

    #[test]
    fn tampered_canonical_bytes_fail_before_signature_verification() {
        let data_dir = crate::core::data_dir::isolated_data_dir();
        let output = json!({
            "model": "claude-test",
            "usage": {"input_tokens": 1, "output_tokens": 1},
            "total_cost_usd": "0.000001"
        })
        .to_string();
        let link = record_provider_receipt("claude-code", "anthropic", &request(), &output, 1)
            .expect("explicit provider evidence should produce a receipt");
        let path = data_dir.path().join("execution/receipts").join(format!(
            "{}.json",
            canonical_receipt_digest(&link.reference).unwrap().hex()
        ));
        let mut bytes = std::fs::read(&path).expect("canonical receipt bytes");
        bytes[0] = b'!';
        std::fs::write(path, bytes).expect("tamper canonical receipt fixture");

        assert!(verify_receipt_reference(&link.reference).is_err());
    }

    #[test]
    fn visible_output_uses_final_structured_answer() {
        let stream = [
            r#"{"item":{"text":"draft"}}"#,
            r#"{"result":"final answer"}"#,
        ]
        .join("\n");

        assert_eq!(visible_output(&stream), "final answer");
    }
}
