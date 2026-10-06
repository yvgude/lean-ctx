//! Independent verifier for canonical signed ReceiptDocumentV1 artifacts.

use std::collections::{BTreeMap, BTreeSet};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signature, Verifier};
use serde_json::{Map, Value};

use super::v2::{
    array, canonical_bytes, check_fields, field, is_content_id, is_digest,
    is_rfc3339_utc_timestamp, object, parse_canonical_json, string, trusted_receipt_key, unsigned,
    TrustStore,
};
use crate::verify::sha256_hex;

const MAX_ITEMS: usize = 64;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

pub(crate) struct VerifiedReceipt {
    pub chain_id: String,
    pub sequence_number: u64,
    pub receipt_id: String,
    pub previous_receipt_id: Option<String>,
    pub previous_signature_digest: Option<String>,
    pub signature_digest: String,
}

pub(crate) struct InventoryArtifact {
    pub kind: String,
    pub bytes: Vec<u8>,
}

pub(crate) fn verify_receipt_document(
    raw: &[u8],
    inventory: &BTreeMap<String, InventoryArtifact>,
    trust: &TrustStore,
) -> Result<VerifiedReceipt, String> {
    let document = parse_canonical_json(raw, "receipt document")?;
    let root = object(&document, "receipt")?;
    check_fields(
        root,
        "receipt",
        &[
            "schema_version",
            "receipt_id",
            "lineage",
            "chain",
            "status",
            "values",
            "outcome",
            "evidence_refs",
            "issued_at",
            "signer",
            "signature",
        ],
        &[],
    )?;
    if unsigned(root, "schema_version", "receipt", 1)? != 1 {
        return Err("receipt.schema_version must be 1".to_string());
    }
    let receipt_id = string(root, "receipt_id", "receipt")?.to_owned();
    if !is_digest(&receipt_id) {
        return Err("receipt.receipt_id is malformed".to_string());
    }
    let issued_at = string(root, "issued_at", "receipt")?;
    if !is_rfc3339_utc_timestamp(issued_at) {
        return Err("receipt.issued_at is not an RFC3339 UTC timestamp".to_string());
    }
    let status = string(root, "status", "receipt")?;
    one_of(
        status,
        &["succeeded", "failed", "rejected", "cancelled", "timed_out"],
        "receipt.status",
    )?;
    let (task_id, invocation_id, invocation_ref) =
        verify_lineage(field(root, "lineage", "receipt")?, inventory)?;
    let (chain_id, sequence_number, previous_receipt_id, previous_signature_digest) =
        verify_chain(field(root, "chain", "receipt")?, &receipt_id)?;
    let evidence = verify_evidence(field(root, "evidence_refs", "receipt")?, inventory)?;
    verify_engine_observation(&evidence, inventory, &invocation_id, &invocation_ref)?;
    verify_values(field(root, "values", "receipt")?, &evidence)?;
    let outcome = verify_outcome(
        field(root, "outcome", "receipt")?,
        &evidence,
        inventory,
        &task_id,
    )?;
    match (status, outcome) {
        ("rejected", "rejected") | ("succeeded", "accepted") => {}
        ("rejected", _) => return Err("rejected status requires rejected outcome".to_string()),
        (_, "rejected") => return Err("rejected outcome requires rejected status".to_string()),
        (_, "accepted") => return Err("accepted outcome requires succeeded status".to_string()),
        (_, "unknown") => {}
        _ => unreachable!(),
    }

    let mut identity = document.clone();
    let identity = identity.as_object_mut().expect("validated receipt object");
    identity.remove("receipt_id");
    identity.remove("signature");
    let derived = format!(
        "sha256:{}",
        sha256_hex(&canonical_bytes(&Value::Object(identity.clone())))
    );
    if derived != receipt_id {
        return Err("receipt_id does not match canonical identity bytes".to_string());
    }

    let signer = object(field(root, "signer", "receipt")?, "receipt.signer")?;
    check_fields(
        signer,
        "receipt.signer",
        &["algorithm", "key_id", "key_admission"],
        &[],
    )?;
    if string(signer, "algorithm", "receipt.signer")? != "ed25519"
        || string(signer, "key_admission", "receipt.signer")? != "external_trust_store"
    {
        return Err("receipt signer admission is unsupported".to_string());
    }
    let key_id = signer_key_id(
        string(signer, "key_id", "receipt.signer")?,
        "receipt.signer.key_id",
    )?;
    let key = trusted_receipt_key(key_id, issued_at, trust)?;

    let encoded = string(root, "signature", "receipt")?;
    let signature = STANDARD
        .decode(encoded)
        .map_err(|_| "receipt signature is not standard Base64".to_string())?;
    if signature.len() != 64 || STANDARD.encode(&signature) != encoded {
        return Err("receipt signature is not canonical Ed25519 Base64".to_string());
    }
    let signature_digest = format!("sha256:{}", sha256_hex(&signature));
    let signature = Signature::from_slice(&signature)
        .map_err(|_| "receipt signature length is invalid".to_string())?;
    let mut signing = document;
    signing
        .as_object_mut()
        .expect("validated receipt object")
        .remove("signature");
    key.verify(&canonical_bytes(&signing), &signature)
        .map_err(|_| "receipt signature does not verify under external trust".to_string())?;
    Ok(VerifiedReceipt {
        chain_id,
        sequence_number,
        receipt_id,
        previous_receipt_id,
        previous_signature_digest,
        signature_digest,
    })
}

fn verify_lineage(
    value: &Value,
    inventory: &BTreeMap<String, InventoryArtifact>,
) -> Result<(String, String, String), String> {
    let lineage = object(value, "receipt.lineage")?;
    check_fields(
        lineage,
        "receipt.lineage",
        &[
            "task_id",
            "task_ref",
            "plan_id",
            "plan_ref",
            "invocation_id",
            "invocation_ref",
            "identity_ref",
            "policy_refs",
            "capabilities",
        ],
        &[],
    )?;
    let task_id = bounded(
        string(lineage, "task_id", "receipt.lineage")?,
        "receipt.lineage.task_id",
    )?;
    let plan_id = bounded(
        string(lineage, "plan_id", "receipt.lineage")?,
        "receipt.lineage.plan_id",
    )?;
    let invocation_id = bounded(
        string(lineage, "invocation_id", "receipt.lineage")?,
        "receipt.lineage.invocation_id",
    )?;
    for (key, kind) in [
        ("task_ref", "task_envelope"),
        ("plan_ref", "execution_plan"),
        ("invocation_ref", "engine_invocation"),
        ("identity_ref", "run_metadata"),
    ] {
        require_artifact_kind(
            string(lineage, key, "receipt.lineage")?,
            inventory,
            key,
            kind,
        )?;
    }
    let policies = array(
        field(lineage, "policy_refs", "receipt.lineage")?,
        "policy_refs",
    )?;
    if policies.is_empty() || policies.len() > MAX_ITEMS {
        return Err("receipt lineage requires 1..=64 policy refs".to_string());
    }
    let mut policy_refs = BTreeSet::new();
    for policy in policies {
        let digest = policy
            .as_str()
            .ok_or_else(|| "policy ref must be a digest string".to_string())?;
        require_artifact_kind(digest, inventory, "policy_ref", "claim_basis")?;
        if !policy_refs.insert(digest.to_owned()) {
            return Err("receipt policy refs must be unique".to_string());
        }
    }
    let invocation_ref = string(lineage, "invocation_ref", "receipt.lineage")?;
    let capabilities = array(
        field(lineage, "capabilities", "receipt.lineage")?,
        "receipt.lineage.capabilities",
    )?;
    if capabilities.is_empty() || capabilities.len() > MAX_ITEMS {
        return Err("receipt lineage requires 1..=64 capabilities".to_string());
    }
    let mut capability_bindings = BTreeSet::new();
    for capability in capabilities {
        let capability = object(capability, "receipt capability")?;
        check_fields(
            capability,
            "receipt capability",
            &["capability_id", "capability_version", "invocation_ref"],
            &[],
        )?;
        let id = bounded(
            string(capability, "capability_id", "receipt capability")?,
            "capability_id",
        )?;
        let version = string(capability, "capability_version", "receipt capability")?;
        if !semantic_version(version)
            || string(capability, "invocation_ref", "receipt capability")? != invocation_ref
            || !capability_bindings.insert((id.to_owned(), version.to_owned()))
        {
            return Err("receipt capability binding is invalid or duplicate".to_string());
        }
    }
    verify_lineage_artifacts(
        inventory,
        task_id,
        string(lineage, "task_ref", "receipt.lineage")?,
        plan_id,
        string(lineage, "plan_ref", "receipt.lineage")?,
        invocation_id,
        invocation_ref,
        string(lineage, "identity_ref", "receipt.lineage")?,
        &policy_refs,
        &capability_bindings,
    )?;
    Ok((
        task_id.to_owned(),
        invocation_id.to_owned(),
        invocation_ref.to_owned(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn verify_lineage_artifacts(
    inventory: &BTreeMap<String, InventoryArtifact>,
    task_id: &str,
    task_ref: &str,
    plan_id: &str,
    plan_ref: &str,
    invocation_id: &str,
    invocation_ref: &str,
    identity_ref: &str,
    policy_refs: &BTreeSet<String>,
    capability_bindings: &BTreeSet<(String, String)>,
) -> Result<(), String> {
    let task_document = artifact_document(inventory, task_ref, "task envelope")?;
    let task = object(&task_document, "task envelope")?;
    check_fields(
        task,
        "task envelope",
        &[
            "schema_version",
            "task_id",
            "trace_id",
            "project_id",
            "session_id",
            "agent_id",
            "complexity",
            "created_at",
        ],
        &[
            "parent_task_id",
            "tenant_id",
            "intent",
            "task_class",
            "risk_class",
            "quality_requirement_milli",
            "cost_budget_micros",
            "latency_budget_ms",
            "data_classification",
            "region_policy_ref",
            "model_policy_ref",
            "context_state_ref",
            "outcome_contract_ref",
        ],
    )?;
    require_schema_v1(task, "task envelope")?;
    if string(task, "task_id", "task envelope")? != task_id {
        return Err("task envelope task_id disagrees with receipt lineage".to_string());
    }
    for key in [
        "task_id",
        "trace_id",
        "project_id",
        "session_id",
        "agent_id",
    ] {
        bounded(
            string(task, key, "task envelope")?,
            &format!("task envelope.{key}"),
        )?;
    }
    one_of(
        string(task, "complexity", "task envelope")?,
        &["unknown", "low", "medium", "high", "critical"],
        "task envelope.complexity",
    )?;
    if !is_rfc3339_utc_timestamp(string(task, "created_at", "task envelope")?) {
        return Err("task envelope created_at is invalid".to_string());
    }
    validate_task_optionals(task)?;
    let agent_id = string(task, "agent_id", "task envelope")?;

    let plan_document = artifact_document(inventory, plan_ref, "execution plan")?;
    let plan = object(&plan_document, "execution plan")?;
    check_fields(
        plan,
        "execution plan",
        &[
            "schema_version",
            "plan_id",
            "task_id",
            "context_budget_tokens",
            "context_strategy",
            "knowledge_refs",
            "capability_ids",
            "model",
            "provider",
            "reasoning_allocation_milli",
            "max_retries",
            "fallback_refs",
            "stop_condition",
            "expected_cost_micros",
            "expected_quality_milli",
            "expected_latency_ms",
        ],
        &[
            "policy_decision_ref",
            "scheduler_decision_ref",
            "executor_agent_id",
            "context_plan_id",
            "capability_bindings",
            "context_budget_policy",
            "estimates",
        ],
    )?;
    require_schema_v1(plan, "execution plan")?;
    if string(plan, "plan_id", "execution plan")? != plan_id
        || string(plan, "task_id", "execution plan")? != task_id
    {
        return Err("execution plan IDs disagree with receipt lineage".to_string());
    }
    validate_execution_plan(plan)?;
    if nullable_optional_string(plan, "executor_agent_id", "execution plan")?
        .is_some_and(|executor| executor != agent_id)
    {
        return Err("execution plan executor disagrees with task agent".to_string());
    }
    let pinned = plan_capability_bindings(plan)?;
    if capability_bindings.iter().any(|(id, version)| {
        pinned
            .iter()
            .any(|(pinned_id, pinned_version)| id == pinned_id && version != pinned_version)
    }) {
        return Err("receipt capability version disagrees with execution plan".to_string());
    }
    let plan_capabilities = array(
        field(plan, "capability_ids", "execution plan")?,
        "execution plan capability_ids",
    )?;
    let plan_capabilities: BTreeSet<&str> = plan_capabilities
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| "execution plan capability_ids must be strings".to_string())
        })
        .collect::<Result<_, _>>()?;
    if capability_bindings
        .iter()
        .any(|(capability_id, _)| !plan_capabilities.contains(capability_id.as_str()))
    {
        return Err("receipt capability was not admitted by execution plan".to_string());
    }

    let invocation_document = artifact_document(inventory, invocation_ref, "engine invocation")?;
    let invocation = object(&invocation_document, "engine invocation")?;
    check_fields(
        invocation,
        "engine invocation",
        &[
            "schema_version",
            "invocation_id",
            "engine",
            "operation",
            "input_ref",
            "input_digest",
            "source_refs",
            "policy_admission",
        ],
        &[],
    )?;
    require_schema_v1(invocation, "engine invocation")?;
    if string(invocation, "invocation_id", "engine invocation")? != invocation_id {
        return Err("engine invocation ID disagrees with receipt lineage".to_string());
    }
    let engine = object(
        field(invocation, "engine", "engine invocation")?,
        "engine invocation engine",
    )?;
    check_fields(
        engine,
        "engine invocation engine",
        &["engine_id", "engine_version"],
        &[],
    )?;
    bounded(
        string(engine, "engine_id", "engine invocation engine")?,
        "engine invocation engine.engine_id",
    )?;
    if !semantic_version(string(
        engine,
        "engine_version",
        "engine invocation engine",
    )?) {
        return Err("engine invocation engine_version is invalid".to_string());
    }
    let operation = object(
        field(invocation, "operation", "engine invocation")?,
        "engine invocation operation",
    )?;
    check_fields(
        operation,
        "engine invocation operation",
        &["capability_id", "capability_version"],
        &[],
    )?;
    let operation_binding = (
        string(operation, "capability_id", "engine invocation operation")?.to_owned(),
        string(
            operation,
            "capability_version",
            "engine invocation operation",
        )?
        .to_owned(),
    );
    if !capability_bindings.contains(&operation_binding) {
        return Err("engine invocation operation disagrees with receipt capability".to_string());
    }
    let input_ref = string(invocation, "input_ref", "engine invocation")?;
    bounded(input_ref, "engine invocation.input_ref")?;
    let input_digest = string(invocation, "input_digest", "engine invocation")?;
    if !is_digest(input_digest) {
        return Err("engine invocation input_digest is malformed".to_string());
    }
    require_artifact_kind(input_digest, inventory, "engine input", "replay_input")?;
    let source_refs = array(
        field(invocation, "source_refs", "engine invocation")?,
        "engine invocation source_refs",
    )?;
    if source_refs.is_empty() || source_refs.len() > 32 {
        return Err("engine invocation source_refs must contain 1..=32 entries".to_string());
    }
    let mut unique_sources = BTreeSet::new();
    for reference in source_refs {
        let reference = reference
            .as_str()
            .ok_or_else(|| "engine invocation source_refs must be strings".to_string())?;
        bounded(reference, "engine invocation source_ref")?;
        if is_content_id(reference) {
            require_artifact_ref(reference, inventory, "engine invocation source_ref")?;
        }
        if !unique_sources.insert(reference) {
            return Err("engine invocation source_refs must be unique".to_string());
        }
    }
    if !unique_sources.contains(input_ref) {
        return Err(
            "engine invocation source_refs must contain input_ref exactly once".to_string(),
        );
    }
    let admission = object(
        field(invocation, "policy_admission", "engine invocation")?,
        "engine invocation policy_admission",
    )?;
    check_fields(
        admission,
        "engine invocation policy_admission",
        &["policy_ref", "decision"],
        &[],
    )?;
    bounded(
        string(
            admission,
            "policy_ref",
            "engine invocation policy_admission",
        )?,
        "engine invocation policy_admission.policy_ref",
    )?;
    if string(admission, "decision", "engine invocation policy_admission")? != "admitted" {
        return Err("engine invocation policy admission disagrees with receipt".to_string());
    }
    let admission_value = Value::Object(admission.clone());
    if !policy_refs.iter().any(|digest| {
        artifact_document(inventory, digest, "policy admission")
            .is_ok_and(|value| value == admission_value)
    }) {
        return Err("receipt policy artifact disagrees with engine admission".to_string());
    }
    if artifact_document(inventory, identity_ref, "run identity")?
        != Value::String(agent_id.to_owned())
    {
        return Err("receipt identity artifact disagrees with task agent_id".to_string());
    }
    Ok(())
}

fn verify_chain(
    value: &Value,
    receipt_id: &str,
) -> Result<(String, u64, Option<String>, Option<String>), String> {
    let chain = object(value, "receipt.chain")?;
    check_fields(
        chain,
        "receipt.chain",
        &["chain_id", "sequence_number"],
        &["previous_receipt_id", "previous_signature_digest"],
    )?;
    let chain_id = bounded(
        string(chain, "chain_id", "receipt.chain")?,
        "receipt.chain.chain_id",
    )?;
    let sequence = unsigned(chain, "sequence_number", "receipt.chain", MAX_SAFE_INTEGER)?;
    if sequence == 0 {
        return Err("receipt chain sequence must start at one".to_string());
    }
    let previous_id = optional_string(chain, "previous_receipt_id", "receipt.chain")?;
    let previous_signature = optional_string(chain, "previous_signature_digest", "receipt.chain")?;
    if (sequence == 1 && (previous_id.is_some() || previous_signature.is_some()))
        || (sequence > 1 && (previous_id.is_none() || previous_signature.is_none()))
        || previous_id.is_some_and(|value| !is_digest(value) || value == receipt_id)
        || previous_signature.is_some_and(|value| !is_digest(value))
    {
        return Err("receipt chain predecessor binding is invalid".to_string());
    }
    Ok((
        chain_id.to_owned(),
        sequence,
        previous_id.map(str::to_owned),
        previous_signature.map(str::to_owned),
    ))
}

fn verify_evidence(
    value: &Value,
    inventory: &BTreeMap<String, InventoryArtifact>,
) -> Result<BTreeMap<String, String>, String> {
    let references = array(value, "receipt.evidence_refs")?;
    if references.len() > MAX_ITEMS {
        return Err("receipt evidence collection exceeds 64".to_string());
    }
    let mut evidence = BTreeMap::new();
    for reference in references {
        let reference = object(reference, "receipt evidence")?;
        check_fields(
            reference,
            "receipt evidence",
            &["kind", "uri", "digest", "media_type", "signature_status"],
            &[],
        )?;
        let kind = string(reference, "kind", "receipt evidence")?;
        one_of(
            kind,
            &[
                "measurement",
                "assumption",
                "formula",
                "price_table",
                "invoice",
                "outcome",
                "runtime",
                "methodology",
            ],
            "receipt evidence kind",
        )?;
        let digest = string(reference, "digest", "receipt evidence")?;
        require_artifact(digest, inventory, "receipt evidence")?;
        let inventory_kind = inventory.get(digest).map(|artifact| artifact.kind.as_str());
        let kind_matches = match kind {
            "measurement" => matches!(
                inventory_kind,
                Some("measurement" | "quality_measurement" | "engine_observation")
            ),
            "outcome" => matches!(inventory_kind, Some("accepted_outcome")),
            "runtime" => matches!(inventory_kind, Some("run_metadata")),
            "methodology" => matches!(inventory_kind, Some("claim_basis")),
            expected => inventory_kind == Some(expected),
        };
        if !kind_matches {
            return Err("receipt evidence kind disagrees with inventory".to_string());
        }
        if evidence
            .insert(digest.to_owned(), kind.to_owned())
            .is_some()
        {
            return Err("receipt evidence digests must be unique".to_string());
        }
        let uri = string(reference, "uri", "receipt evidence")?;
        if !safe_evidence_uri(uri) {
            return Err("receipt evidence URI is unsafe".to_string());
        }
        let media = string(reference, "media_type", "receipt evidence")?;
        let media_parts: Vec<&str> = media.split('/').collect();
        if media.len() > 128
            || media_parts.len() != 2
            || media_parts.iter().any(|part| {
                part.is_empty()
                    || !part.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric()
                            || matches!(
                                byte,
                                b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                            )
                    })
            })
        {
            return Err("receipt evidence media type is invalid".to_string());
        }
        one_of(
            string(reference, "signature_status", "receipt evidence")?,
            &["Verified", "Unverified", "NotSigned"],
            "receipt evidence signature_status",
        )?;
    }
    Ok(evidence)
}

fn verify_engine_observation(
    evidence: &BTreeMap<String, String>,
    inventory: &BTreeMap<String, InventoryArtifact>,
    invocation_id: &str,
    invocation_ref: &str,
) -> Result<(), String> {
    let observations: Vec<(&str, &InventoryArtifact)> = evidence
        .keys()
        .filter_map(|digest| {
            inventory
                .get(digest)
                .filter(|artifact| artifact.kind == "engine_observation")
                .map(|artifact| (digest.as_str(), artifact))
        })
        .collect();
    if observations.is_empty() {
        return Ok(());
    }
    let [(observation_digest, observation_artifact)] = observations.as_slice() else {
        return Err("receipt permits at most one engine_observation artifact".to_string());
    };
    let observation = parse_canonical_json(&observation_artifact.bytes, "engine observation")?;
    let observation = object(&observation, "engine observation")?;
    check_fields(
        observation,
        "engine observation",
        &[
            "schema_version",
            "invocation_id",
            "status",
            "source_lineage",
            "measurements",
        ],
        &["output_ref", "output_digest", "failure", "receipt_link"],
    )?;
    require_schema_v1(observation, "engine observation")?;
    if string(observation, "invocation_id", "engine observation")? != invocation_id {
        return Err("engine observation invocation_id disagrees with receipt lineage".to_string());
    }

    let invocation_document = artifact_document(inventory, invocation_ref, "engine invocation")?;
    let invocation = object(&invocation_document, "engine invocation")?;
    let invocation_sources = array(
        field(invocation, "source_refs", "engine invocation")?,
        "engine invocation.source_refs",
    )?
    .iter()
    .map(|value| {
        value
            .as_str()
            .ok_or_else(|| "engine invocation source_refs must be strings".to_string())
    })
    .collect::<Result<BTreeSet<_>, _>>()?;
    let source_lineage = array(
        field(observation, "source_lineage", "engine observation")?,
        "engine observation.source_lineage",
    )?;
    if source_lineage.is_empty() || source_lineage.len() > 32 {
        return Err("engine observation source_lineage must contain 1..=32 refs".to_string());
    }
    let mut unique_sources = BTreeSet::new();
    for source in source_lineage {
        let source = source
            .as_str()
            .ok_or_else(|| "engine observation source_lineage must be strings".to_string())?;
        bounded(source, "engine observation source_lineage")?;
        if !invocation_sources.contains(source) || !unique_sources.insert(source) {
            return Err(
                "engine observation source_lineage is duplicate or absent from invocation"
                    .to_string(),
            );
        }
    }

    let status = string(observation, "status", "engine observation")?;
    one_of(
        status,
        &["succeeded", "degraded", "rejected", "failed"],
        "engine observation.status",
    )?;
    let output_ref = observation.get("output_ref").and_then(Value::as_str);
    let output_digest = observation.get("output_digest").and_then(Value::as_str);
    if output_ref.is_some() != output_digest.is_some() {
        return Err(
            "engine observation output_ref and output_digest must appear together".to_string(),
        );
    }
    if let Some(digest) = output_digest {
        if !is_digest(digest) || output_ref != Some(format!("output:{}", &digest[7..]).as_str()) {
            return Err("engine observation output reference is invalid".to_string());
        }
        require_artifact(digest, inventory, "engine observation output")?;
    }

    let failure = observation.get("failure");
    match (status, output_ref.is_some(), failure) {
        ("succeeded", true, None)
        | ("degraded", true, Some(_))
        | ("failed", false, Some(_))
        | ("rejected", false, Some(_)) => {}
        _ => return Err("engine observation terminal semantics are inconsistent".to_string()),
    }
    if let Some(failure) = failure {
        let failure = object(failure, "engine observation.failure")?;
        check_fields(
            failure,
            "engine observation.failure",
            &["code", "retryable_by_host"],
            &["recovery_ref"],
        )?;
        let code = string(failure, "code", "engine observation.failure")?;
        one_of(
            code,
            &[
                "policy_rejected",
                "source_unavailable",
                "source_integrity_mismatch",
                "resource_limit",
                "unsupported_operation",
                "internal",
            ],
            "engine observation.failure.code",
        )?;
        let retryable = field(failure, "retryable_by_host", "engine observation.failure")?
            .as_bool()
            .ok_or_else(|| {
                "engine observation.failure.retryable_by_host must be boolean".to_string()
            })?;
        let recovery = failure
            .get("recovery_ref")
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| {
                        "engine observation.failure.recovery_ref must be a string".to_string()
                    })
                    .and_then(|value| bounded(value, "engine observation.failure.recovery_ref"))
            })
            .transpose()?;
        if (code == "policy_rejected" && (retryable || recovery.is_some()))
            || (matches!(code, "source_unavailable" | "source_integrity_mismatch")
                && recovery.is_none())
            || (status == "rejected" && code != "policy_rejected")
        {
            return Err("engine observation failure semantics are inconsistent".to_string());
        }
    }
    if string(
        object(
            field(invocation, "policy_admission", "engine invocation")?,
            "engine invocation.policy_admission",
        )?,
        "decision",
        "engine invocation.policy_admission",
    )? == "admitted"
        && status == "rejected"
    {
        return Err("admitted invocation cannot have a rejected observation".to_string());
    }

    let measurements = array(
        field(observation, "measurements", "engine observation")?,
        "engine observation.measurements",
    )?;
    if measurements.len() > MAX_ITEMS {
        return Err("engine observation measurements exceed 64".to_string());
    }
    let mut names = BTreeSet::new();
    for measurement in measurements {
        let measurement = object(measurement, "engine observation measurement")?;
        check_fields(
            measurement,
            "engine observation measurement",
            &["name", "unit", "classification"],
            &["value"],
        )?;
        let name = bounded(
            string(measurement, "name", "engine observation measurement")?,
            "engine observation measurement.name",
        )?;
        bounded(
            string(measurement, "unit", "engine observation measurement")?,
            "engine observation measurement.unit",
        )?;
        if !names.insert(name) {
            return Err("engine observation measurement names must be unique".to_string());
        }
        let classification = string(
            measurement,
            "classification",
            "engine observation measurement",
        )?;
        one_of(
            classification,
            &["measured", "estimated", "unavailable"],
            "engine observation measurement.classification",
        )?;
        let value = measurement.get("value");
        if classification == "unavailable" {
            if value.is_some() {
                return Err("unavailable engine measurement must omit value".to_string());
            }
        } else {
            let Some(value) = value else {
                return Err("measured or estimated engine measurement requires value".to_string());
            };
            if value.as_u64().is_none() {
                return Err("engine observation measurement value must be unsigned".to_string());
            }
        }
    }

    let receipt_link = object(
        field(observation, "receipt_link", "engine observation")?,
        "engine observation.receipt_link",
    )?;
    check_fields(
        receipt_link,
        "engine observation.receipt_link",
        &[
            "schema_version",
            "receipt_id",
            "receipt_ref",
            "receipt_digest",
            "invocation_id",
        ],
        &[],
    )?;
    require_schema_v1(receipt_link, "engine observation.receipt_link")?;
    if string(
        receipt_link,
        "invocation_id",
        "engine observation.receipt_link",
    )? != invocation_id
    {
        return Err("engine observation receipt link invocation_id disagrees".to_string());
    }
    bounded(
        string(
            receipt_link,
            "receipt_id",
            "engine observation.receipt_link",
        )?,
        "engine observation.receipt_link.receipt_id",
    )?;
    let receipt_digest = string(
        receipt_link,
        "receipt_digest",
        "engine observation.receipt_link",
    )?;
    if !is_digest(receipt_digest)
        || string(
            receipt_link,
            "receipt_ref",
            "engine observation.receipt_link",
        )? != format!("receipt:{receipt_digest}")
        || !evidence.contains_key(receipt_digest)
    {
        return Err("engine observation receipt link is not signed evidence".to_string());
    }
    let receipt_document = artifact_document(inventory, receipt_digest, "engine receipt artifact")?;
    let receipt = object(&receipt_document, "engine receipt artifact")?;
    check_fields(
        receipt,
        "engine receipt artifact",
        &["schema_version", "invocation", "observation"],
        &[],
    )?;
    require_schema_v1(receipt, "engine receipt artifact")?;
    if field(receipt, "invocation", "engine receipt artifact")?
        != &Value::Object(invocation.clone())
    {
        return Err("engine receipt invocation disagrees with engine invocation".to_string());
    }
    let receipt_observation = object(
        field(receipt, "observation", "engine receipt artifact")?,
        "engine receipt observation",
    )?;
    if receipt_observation.contains_key("receipt_link") {
        return Err("engine receipt observation must omit receipt_link".to_string());
    }
    let mut expected_observation = observation.clone();
    expected_observation.remove("receipt_link");
    if receipt_observation != &expected_observation {
        return Err("engine receipt observation disagrees with engine observation".to_string());
    }
    if !is_digest(observation_digest) {
        return Err("engine observation evidence digest is malformed".to_string());
    }
    Ok(())
}

fn verify_values(value: &Value, evidence: &BTreeMap<String, String>) -> Result<(), String> {
    let values = array(value, "receipt.values")?;
    if values.len() > MAX_ITEMS {
        return Err("receipt value collection exceeds 64".to_string());
    }
    let mut names = BTreeSet::new();
    for value in values {
        let value = object(value, "receipt value")?;
        check_fields(
            value,
            "receipt value",
            &["name", "unit", "classification"],
            &[
                "value",
                "evidence_digests",
                "formula_digest",
                "price_table_digest",
                "reconciliation_digest",
            ],
        )?;
        let value_name = name(string(value, "name", "receipt value")?)?;
        name(string(value, "unit", "receipt value")?)?;
        if !names.insert(value_name) {
            return Err("receipt value names must be unique".to_string());
        }
        let classification = string(value, "classification", "receipt value")?;
        let numeric = value
            .get("value")
            .map(|_| unsigned(value, "value", "receipt value", MAX_SAFE_INTEGER))
            .transpose()?;
        let direct = value
            .get("evidence_digests")
            .map(|value| array(value, "receipt value evidence"))
            .transpose()?
            .unwrap_or(&[]);
        let mut direct_kinds = Vec::new();
        let mut unique = BTreeSet::new();
        for digest in direct {
            let digest = digest
                .as_str()
                .ok_or_else(|| "value evidence must be digest strings".to_string())?;
            let kind = evidence
                .get(digest)
                .ok_or_else(|| "value evidence is absent from receipt".to_string())?;
            if !unique.insert(digest) {
                return Err("value evidence digests must be unique".to_string());
            }
            direct_kinds.push(kind.as_str());
        }
        let formula = optional_digest(value, "formula_digest", evidence)?;
        let price = optional_digest(value, "price_table_digest", evidence)?;
        let reconciliation = optional_digest(value, "reconciliation_digest", evidence)?;
        let derived: Vec<&str> = [
            "formula_digest",
            "price_table_digest",
            "reconciliation_digest",
        ]
        .iter()
        .filter_map(|key| value.get(*key).and_then(Value::as_str))
        .collect();
        let mut all_digests = unique;
        if derived.iter().any(|digest| !all_digests.insert(*digest)) {
            return Err("value provenance digests must occur exactly once".to_string());
        }
        match classification {
            "unavailable"
                if numeric.is_none()
                    && direct.is_empty()
                    && formula.is_none()
                    && price.is_none()
                    && reconciliation.is_none() => {}
            "measured"
                if numeric.is_some()
                    && direct_kinds.contains(&"measurement")
                    && formula.is_none()
                    && price.is_none()
                    && reconciliation.is_none() => {}
            "estimated"
                if numeric.is_some()
                    && direct_kinds.contains(&"assumption")
                    && formula.is_none()
                    && price.is_none()
                    && reconciliation.is_none() => {}
            "calculated"
                if numeric.is_some()
                    && !direct.is_empty()
                    && direct_kinds
                        .iter()
                        .all(|kind| matches!(*kind, "measurement" | "assumption"))
                    && formula == Some("formula")
                    && price == Some("price_table")
                    && reconciliation.is_none() => {}
            "reconciled"
                if numeric.is_some()
                    && !direct.is_empty()
                    && direct_kinds
                        .iter()
                        .all(|kind| matches!(*kind, "measurement" | "assumption"))
                    && formula == Some("formula")
                    && price == Some("price_table")
                    && reconciliation == Some("invoice") => {}
            _ => return Err("receipt value classification/provenance is inconsistent".to_string()),
        }
    }
    Ok(())
}

fn verify_outcome<'a>(
    value: &'a Value,
    evidence: &BTreeMap<String, String>,
    inventory: &BTreeMap<String, InventoryArtifact>,
    task_id: &str,
) -> Result<&'a str, String> {
    let outcome = object(value, "receipt.outcome")?;
    check_fields(
        outcome,
        "receipt.outcome",
        &["state"],
        &["outcome_id", "outcome_ref", "acceptance_evidence_digest"],
    )?;
    let state = string(outcome, "state", "receipt.outcome")?;
    let id = optional_string(outcome, "outcome_id", "receipt.outcome")?;
    let reference = optional_string(outcome, "outcome_ref", "receipt.outcome")?;
    let acceptance = optional_string(outcome, "acceptance_evidence_digest", "receipt.outcome")?;
    match state {
        "unknown" if id.is_none() && reference.is_none() && acceptance.is_none() => {}
        "rejected"
            if id.is_some_and(|value| bounded(value, "outcome_id").is_ok())
                && reference
                    .and_then(|digest| evidence.get(digest))
                    .map(String::as_str)
                    == Some("outcome")
                && acceptance.is_none() => {}
        "accepted"
            if id.is_some_and(|value| bounded(value, "outcome_id").is_ok())
                && reference
                    .and_then(|digest| evidence.get(digest))
                    .map(String::as_str)
                    == Some("outcome")
                && acceptance.is_some_and(|digest| evidence.contains_key(digest))
                && acceptance != reference => {}
        _ => return Err("receipt outcome binding is invalid".to_string()),
    }
    if state != "unknown" {
        verify_outcome_artifact(
            inventory,
            evidence,
            reference.expect("validated terminal outcome reference"),
            id.expect("validated terminal outcome ID"),
            task_id,
            state,
            acceptance,
        )?;
    }
    Ok(state)
}

fn verify_outcome_artifact(
    inventory: &BTreeMap<String, InventoryArtifact>,
    evidence: &BTreeMap<String, String>,
    outcome_ref: &str,
    outcome_id: &str,
    task_id: &str,
    state: &str,
    acceptance_evidence: Option<&str>,
) -> Result<(), String> {
    let document = artifact_document(inventory, outcome_ref, "accepted outcome")?;
    let outcome = object(&document, "accepted outcome")?;
    check_fields(
        outcome,
        "accepted outcome",
        &[
            "schema_version",
            "outcome_id",
            "task_id",
            "accepted",
            "signals",
            "evidence_refs",
            "observed_at",
        ],
        &["quality_score_milli", "contract_ref"],
    )?;
    require_schema_v1(outcome, "accepted outcome")?;
    if string(outcome, "outcome_id", "accepted outcome")? != outcome_id
        || string(outcome, "task_id", "accepted outcome")? != task_id
        || string(outcome, "accepted", "accepted outcome")? != state
    {
        return Err("accepted outcome payload disagrees with receipt outcome".to_string());
    }
    bounded(
        string(outcome, "outcome_id", "accepted outcome")?,
        "accepted outcome.outcome_id",
    )?;
    bounded(
        string(outcome, "task_id", "accepted outcome")?,
        "accepted outcome.task_id",
    )?;
    if !is_rfc3339_utc_timestamp(string(outcome, "observed_at", "accepted outcome")?) {
        return Err("accepted outcome observed_at is invalid".to_string());
    }
    if outcome
        .get("quality_score_milli")
        .is_some_and(|value| !value.is_null())
    {
        unsigned(outcome, "quality_score_milli", "accepted outcome", 1_000)?;
    }
    if let Some(contract_ref) =
        nullable_optional_string(outcome, "contract_ref", "accepted outcome")?
    {
        bounded(contract_ref, "accepted outcome.contract_ref")?;
    }
    let signals = object(
        field(outcome, "signals", "accepted outcome")?,
        "accepted outcome signals",
    )?;
    check_fields(
        signals,
        "accepted outcome signals",
        &[
            "build",
            "tests",
            "lint",
            "typecheck",
            "completion",
            "pr",
            "correction",
            "rollback",
            "retry",
        ],
        &[],
    )?;
    for (key, value) in signals {
        if !value.is_null() {
            one_of(
                value
                    .as_str()
                    .ok_or_else(|| format!("accepted outcome signal {key} is invalid"))?,
                &["passed", "failed", "unknown", "not_run"],
                &format!("accepted outcome signal {key}"),
            )?;
        }
    }
    let payload_evidence = array(
        field(outcome, "evidence_refs", "accepted outcome")?,
        "accepted outcome evidence_refs",
    )?;
    if payload_evidence.len() > MAX_ITEMS {
        return Err("accepted outcome evidence_refs exceeds 64 entries".to_string());
    }
    let mut payload_digests = BTreeSet::new();
    for reference in payload_evidence {
        let reference = object(reference, "accepted outcome evidence ref")?;
        check_fields(
            reference,
            "accepted outcome evidence ref",
            &["kind", "uri", "digest", "signature_status"],
            &[],
        )?;
        one_of(
            string(reference, "kind", "accepted outcome evidence ref")?,
            &[
                "ProviderReceipt",
                "RuntimeLog",
                "SignedBatch",
                "QualityMeasurement",
                "ExperimentOutcome",
            ],
            "accepted outcome evidence kind",
        )?;
        bounded(
            string(reference, "uri", "accepted outcome evidence ref")?,
            "accepted outcome evidence uri",
        )?;
        let digest = string(reference, "digest", "accepted outcome evidence ref")?;
        if !is_digest(digest) {
            return Err("accepted outcome evidence digest is malformed".to_string());
        }
        require_artifact(digest, inventory, "accepted outcome evidence")?;
        if !evidence.contains_key(digest) {
            return Err("accepted outcome evidence digest is not listed by receipt".to_string());
        }
        if !payload_digests.insert(digest) {
            return Err("accepted outcome evidence digests must be unique".to_string());
        }
        one_of(
            string(
                reference,
                "signature_status",
                "accepted outcome evidence ref",
            )?,
            &["Verified", "Unverified", "NotSigned"],
            "accepted outcome evidence signature_status",
        )?;
    }
    if let Some(expected) = acceptance_evidence {
        if !payload_evidence.iter().any(|reference| {
            reference
                .as_object()
                .and_then(|reference| reference.get("digest"))
                .and_then(Value::as_str)
                == Some(expected)
        }) {
            return Err("accepted outcome payload omits receipt acceptance evidence".to_string());
        }
    }
    Ok(())
}

fn optional_digest<'a>(
    value: &'a Map<String, Value>,
    key: &str,
    evidence: &'a BTreeMap<String, String>,
) -> Result<Option<&'a str>, String> {
    let Some(digest) = value.get(key) else {
        return Ok(None);
    };
    let digest = digest
        .as_str()
        .ok_or_else(|| format!("{key} must be a digest string"))?;
    evidence
        .get(digest)
        .map(String::as_str)
        .map(Some)
        .ok_or_else(|| format!("{key} is absent from receipt evidence"))
}

fn require_artifact(
    digest: &str,
    inventory: &BTreeMap<String, InventoryArtifact>,
    label: &str,
) -> Result<(), String> {
    let Some(artifact) = inventory.get(digest).filter(|_| is_digest(digest)) else {
        return Err(format!(
            "{label} does not resolve to one inventory artifact"
        ));
    };
    if format!("sha256:{}", sha256_hex(&artifact.bytes)) != digest {
        return Err(format!("{label} bytes disagree with inventory digest"));
    }
    Ok(())
}

fn require_artifact_ref(
    reference: &str,
    inventory: &BTreeMap<String, InventoryArtifact>,
    label: &str,
) -> Result<(), String> {
    let digest = reference
        .strip_prefix("id:")
        .ok_or_else(|| format!("{label} must be a content ID"))?;
    if !is_digest(digest) {
        return Err(format!("{label} must contain a SHA-256 digest"));
    }
    require_artifact(digest, inventory, label)
}

fn artifact_document(
    inventory: &BTreeMap<String, InventoryArtifact>,
    digest: &str,
    label: &str,
) -> Result<Value, String> {
    let artifact = inventory
        .get(digest)
        .ok_or_else(|| format!("{label} does not resolve to one inventory artifact"))?;
    if format!("sha256:{}", sha256_hex(&artifact.bytes)) != digest {
        return Err(format!("{label} bytes disagree with inventory digest"));
    }
    parse_canonical_json(&artifact.bytes, label)
}

fn require_schema_v1(object: &Map<String, Value>, label: &str) -> Result<(), String> {
    if unsigned(object, "schema_version", label, 1)? != 1 {
        return Err(format!("{label}.schema_version must be 1"));
    }
    Ok(())
}

fn validate_task_optionals(task: &Map<String, Value>) -> Result<(), String> {
    for key in [
        "parent_task_id",
        "tenant_id",
        "intent",
        "task_class",
        "region_policy_ref",
        "model_policy_ref",
        "context_state_ref",
        "outcome_contract_ref",
    ] {
        if let Some(value) = nullable_optional_string(task, key, "task envelope")? {
            bounded(value, &format!("task envelope.{key}"))?;
        }
    }
    if let Some(value) = nullable_optional_string(task, "risk_class", "task envelope")? {
        one_of(
            value,
            &["low", "medium", "high", "critical"],
            "task envelope.risk_class",
        )?;
    }
    if let Some(value) = nullable_optional_string(task, "data_classification", "task envelope")? {
        one_of(
            value,
            &["Public", "Internal", "Confidential", "Restricted"],
            "task envelope.data_classification",
        )?;
    }
    if task
        .get("quality_requirement_milli")
        .is_some_and(|value| !value.is_null())
    {
        unsigned(task, "quality_requirement_milli", "task envelope", 1_000)?;
    }
    for key in ["cost_budget_micros", "latency_budget_ms"] {
        if task.get(key).is_some_and(|value| !value.is_null()) {
            unsigned(task, key, "task envelope", u64::MAX)?;
        }
    }
    Ok(())
}

fn validate_execution_plan(plan: &Map<String, Value>) -> Result<(), String> {
    bounded(
        string(plan, "plan_id", "execution plan")?,
        "execution plan.plan_id",
    )?;
    bounded(
        string(plan, "task_id", "execution plan")?,
        "execution plan.task_id",
    )?;
    for key in ["model", "provider"] {
        bounded(
            string(plan, key, "execution plan")?,
            &format!("execution plan.{key}"),
        )?;
    }
    one_of(
        string(plan, "context_strategy", "execution plan")?,
        &["minimal", "balanced", "comprehensive", "cached_first"],
        "execution plan.context_strategy",
    )?;
    one_of(
        string(plan, "stop_condition", "execution plan")?,
        &[
            "on_completion",
            "on_acceptance",
            "on_budget_exhaustion",
            "on_error",
            "manual",
        ],
        "execution plan.stop_condition",
    )?;
    for (key, max) in [
        ("context_budget_tokens", u64::MAX),
        ("reasoning_allocation_milli", 1_000),
        ("max_retries", u64::from(u32::MAX)),
        ("expected_cost_micros", u64::MAX),
        ("expected_quality_milli", 1_000),
        ("expected_latency_ms", u64::MAX),
    ] {
        unsigned(plan, key, "execution plan", max)?;
    }
    for key in ["knowledge_refs", "capability_ids", "fallback_refs"] {
        let values = array(field(plan, key, "execution plan")?, key)?;
        if values.len() > MAX_ITEMS {
            return Err(format!("execution plan.{key} exceeds 64 entries"));
        }
        let mut unique = BTreeSet::new();
        for value in values {
            let value = value
                .as_str()
                .ok_or_else(|| format!("execution plan.{key} must contain strings"))?;
            bounded(value, &format!("execution plan.{key}"))?;
            if !unique.insert(value) {
                return Err(format!("execution plan.{key} must be unique"));
            }
        }
    }
    for key in [
        "policy_decision_ref",
        "scheduler_decision_ref",
        "executor_agent_id",
        "context_plan_id",
    ] {
        if let Some(value) = nullable_optional_string(plan, key, "execution plan")? {
            bounded(value, &format!("execution plan.{key}"))?;
        }
    }
    validate_plan_additives(plan)?;
    plan_capability_bindings(plan)?;
    Ok(())
}

fn validate_plan_additives(plan: &Map<String, Value>) -> Result<(), String> {
    if let Some(value) = plan
        .get("context_budget_policy")
        .filter(|value| !value.is_null())
    {
        let policy = object(value, "context budget policy")?;
        let expected = match string(policy, "kind", "context budget policy")? {
            "no_token_limit" => {
                check_fields(policy, "context budget policy", &["kind"], &[])?;
                0
            }
            "token_limit" => {
                check_fields(policy, "context budget policy", &["kind", "tokens"], &[])?;
                unsigned(policy, "tokens", "context budget policy", u64::MAX)?
            }
            _ => return Err("unknown context budget policy".into()),
        };
        if unsigned(plan, "context_budget_tokens", "execution plan", u64::MAX)? != expected {
            return Err("context budget policy disagrees with legacy scalar".into());
        }
    }
    if let Some(value) = plan.get("estimates").filter(|value| !value.is_null()) {
        let estimates = object(value, "plan estimates")?;
        check_fields(
            estimates,
            "plan estimates",
            &[],
            &["cost_micros", "quality_milli", "latency_ms"],
        )?;
        for (key, scalar, maximum) in [
            ("cost_micros", "expected_cost_micros", u64::MAX),
            ("quality_milli", "expected_quality_milli", 1000),
            ("latency_ms", "expected_latency_ms", u64::MAX),
        ] {
            let projected = match estimates.get(key) {
                None | Some(Value::Null) => 0,
                Some(_) => unsigned(estimates, key, "plan estimates", maximum)?,
            };
            if unsigned(plan, scalar, "execution plan", maximum)? != projected {
                return Err("plan estimates disagree with legacy scalars".into());
            }
        }
    }
    Ok(())
}

fn plan_capability_bindings(
    plan: &Map<String, Value>,
) -> Result<BTreeSet<(String, String)>, String> {
    let mut bindings = BTreeSet::new();
    let Some(value) = plan.get("capability_bindings") else {
        return Ok(bindings);
    };
    let entries = array(value, "plan capability bindings")?;
    if entries.len() > MAX_ITEMS {
        return Err("too many plan capability bindings".into());
    }
    let capabilities = array(
        field(plan, "capability_ids", "execution plan")?,
        "plan capabilities",
    )?;
    let mut ids = BTreeSet::new();
    for entry in entries {
        let entry = object(entry, "plan capability binding")?;
        check_fields(
            entry,
            "plan capability binding",
            &["capability_id", "version"],
            &["manifest_digest"],
        )?;
        let id = bounded(
            string(entry, "capability_id", "plan capability binding")?,
            "capability_id",
        )?;
        let version = bounded(
            string(entry, "version", "plan capability binding")?,
            "plan capability version",
        )?;
        if !ids.insert(id) || !capabilities.iter().any(|value| value.as_str() == Some(id)) {
            return Err("invalid or duplicate plan capability binding".into());
        }
        if let Some(digest) =
            nullable_optional_string(entry, "manifest_digest", "plan capability binding")?
        {
            let digest = digest
                .strip_prefix("sha256:")
                .or_else(|| digest.strip_prefix("blake3:"))
                .unwrap_or(digest);
            if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err("invalid plan manifest digest".into());
            }
        }
        bindings.insert((id.to_owned(), version.to_owned()));
    }
    Ok(bindings)
}

fn require_artifact_kind(
    digest: &str,
    inventory: &BTreeMap<String, InventoryArtifact>,
    label: &str,
    expected_kind: &str,
) -> Result<(), String> {
    require_artifact(digest, inventory, label)?;
    if inventory.get(digest).map(|artifact| artifact.kind.as_str()) != Some(expected_kind) {
        return Err(format!("{label} resolves to the wrong inventory kind"));
    }
    Ok(())
}

fn bounded<'a>(value: &'a str, label: &str) -> Result<&'a str, String> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(format!("{label} is not a bounded opaque identifier"));
    }
    Ok(value)
}

fn signer_key_id<'a>(value: &'a str, label: &str) -> Result<&'a str, String> {
    let mut bytes = value.bytes();
    if value.len() > 128
        || !bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !bytes.all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-')
        })
        || value.starts_with("base64:")
        || value.starts_with("hex:")
    {
        return Err(format!("{label} is not a valid external key identifier"));
    }
    Ok(value)
}

fn optional_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    label: &str,
) -> Result<Option<&'a str>, String> {
    object
        .get(key)
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| format!("{label}.{key} must be a string when present"))
        })
        .transpose()
}

fn nullable_optional_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    label: &str,
) -> Result<Option<&'a str>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(format!(
            "{label}.{key} must be a string or null when present"
        )),
    }
}

fn name(value: &str) -> Result<&str, String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err("receipt name/unit is invalid".to_string());
    }
    Ok(value)
}

fn safe_evidence_uri(value: &str) -> bool {
    if value.len() > 1024
        || !value.bytes().all(|byte| (b'!'..=b'~').contains(&byte))
        || value.contains("..")
        || value.contains(['?', '#', '@'])
    {
        return false;
    }
    ["artifact://", "evidence://", "bundle://", "source://"]
        .iter()
        .any(|prefix| {
            value
                .strip_prefix(prefix)
                .is_some_and(|locator| !locator.is_empty() && !locator.starts_with('/'))
        })
        || value
            .strip_prefix("urn:")
            .is_some_and(|locator| !locator.is_empty())
}

fn semantic_version(value: &str) -> bool {
    if value.len() > 128 {
        return false;
    }
    let (without_build, build) = value
        .split_once('+')
        .map_or((value, None), |(core, build)| (core, Some(build)));
    if build.is_some_and(|value| !semver_identifiers(value, false)) {
        return false;
    }
    let (core, prerelease) = without_build
        .split_once('-')
        .map_or((without_build, None), |(core, pre)| (core, Some(pre)));
    if prerelease.is_some_and(|value| !semver_identifiers(value, true)) {
        return false;
    }
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part == &"0" || !part.starts_with('0'))
        })
}

fn semver_identifiers(value: &str, reject_numeric_leading_zero: bool) -> bool {
    !value.is_empty()
        && value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && !(reject_numeric_leading_zero
                    && part.len() > 1
                    && part.starts_with('0')
                    && part.bytes().all(|byte| byte.is_ascii_digit()))
        })
}

fn one_of(value: &str, allowed: &[&str], label: &str) -> Result<(), String> {
    allowed
        .contains(&value)
        .then_some(())
        .ok_or_else(|| format!("{label} has unsupported value '{value}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn additive_plan() -> Map<String, Value> {
        serde_json::json!({
            "capability_ids":["capability:local"], "context_budget_tokens":0,
            "expected_cost_micros":0, "expected_quality_milli":0, "expected_latency_ms":0
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn plan_additives_preserve_legacy_and_validate_explicit_unknowns() {
        let mut plan = additive_plan();
        validate_plan_additives(&plan).unwrap();
        assert!(plan_capability_bindings(&plan).unwrap().is_empty());
        plan.insert(
            "context_budget_policy".into(),
            serde_json::json!({"kind":"no_token_limit"}),
        );
        plan.insert(
            "estimates".into(),
            serde_json::json!({"cost_micros":null,"quality_milli":null,"latency_ms":0}),
        );
        validate_plan_additives(&plan).unwrap();
        plan.insert("context_budget_tokens".into(), Value::from(20));
        assert!(validate_plan_additives(&plan).is_err());
        plan.insert(
            "context_budget_policy".into(),
            serde_json::json!({"kind":"token_limit","tokens":20}),
        );
        validate_plan_additives(&plan).unwrap();
        plan.insert("expected_cost_micros".into(), Value::from(1));
        assert!(validate_plan_additives(&plan).is_err());
    }

    #[test]
    fn plan_additives_reject_unknown_fields_types_and_out_of_range_estimates() {
        for policy in [
            serde_json::json!({"kind":"no_token_limit","tokens":0}),
            serde_json::json!({"kind":"token_limit","tokens":-1}),
            serde_json::json!({"kind":"invented"}),
        ] {
            let mut plan = additive_plan();
            plan.insert("context_budget_policy".into(), policy);
            assert!(validate_plan_additives(&plan).is_err());
        }
        for estimates in [
            serde_json::json!({"invented":0}),
            serde_json::json!({"quality_milli":1001}),
            serde_json::json!({"latency_ms":"0"}),
        ] {
            let mut plan = additive_plan();
            plan.insert("estimates".into(), estimates);
            assert!(validate_plan_additives(&plan).is_err());
        }
    }

    #[test]
    fn plan_bindings_validate_selected_identity_unique_versions_and_digest() {
        let mut plan = additive_plan();
        let binding = serde_json::json!({"capability_id":"capability:local","version":"1.0.0"});
        plan.insert(
            "capability_bindings".into(),
            serde_json::json!([binding.clone()]),
        );
        assert_eq!(plan_capability_bindings(&plan).unwrap().len(), 1);
        for prefix in ["sha256:", "blake3:", ""] {
            let mut value = binding.clone();
            value["manifest_digest"] = Value::from(format!("{prefix}{}", "a".repeat(64)));
            plan.insert("capability_bindings".into(), serde_json::json!([value]));
            plan_capability_bindings(&plan).unwrap();
        }
        for values in [
            serde_json::json!([binding.clone(), binding.clone()]),
            serde_json::json!([{"capability_id":"capability:foreign","version":"1.0.0"}]),
            serde_json::json!([{"capability_id":"capability:local","version":"1.0.0","manifest_digest":"bad"}]),
            serde_json::json!([{"capability_id":"capability:local","version":"1.0.0","extra":true}]),
        ] {
            plan.insert("capability_bindings".into(), values);
            assert!(plan_capability_bindings(&plan).is_err());
        }
    }

    fn artifact(kind: &str, bytes: Vec<u8>) -> (String, InventoryArtifact) {
        (
            format!("sha256:{}", sha256_hex(&bytes)),
            InventoryArtifact {
                kind: kind.to_owned(),
                bytes,
            },
        )
    }

    fn observation_fixture(
        observation_invocation_id: &str,
        status: &str,
    ) -> (
        BTreeMap<String, String>,
        BTreeMap<String, InventoryArtifact>,
        String,
        String,
    ) {
        let (output_digest, output) = artifact("replay_input", b"real engine output".to_vec());
        let invocation_value = serde_json::json!({
            "schema_version": 1,
            "invocation_id": "invocation-real",
            "engine": {"engine_id": "lean-ctx-local", "engine_version": "3.9.20"},
            "operation": {
                "capability_id": "capability://leanctx/context",
                "capability_version": "1.0.0"
            },
            "input_ref": "input:fixture",
            "input_digest": output_digest.clone(),
            "source_refs": ["input:fixture", "source:fixture"],
            "policy_admission": {"policy_ref": "policy:fixture", "decision": "admitted"}
        });
        let (invocation_ref, invocation) =
            artifact("engine_invocation", canonical_bytes(&invocation_value));
        let observation_without_link = serde_json::json!({
            "schema_version": 1,
            "invocation_id": observation_invocation_id,
            "status": status,
            "output_ref": format!("output:{}", &output_digest[7..]),
            "output_digest": output_digest.clone(),
            "source_lineage": ["input:fixture", "source:fixture"],
            "measurements": [{
                "name": "output_tokens",
                "unit": "token",
                "classification": "measured",
                "value": 4
            }],
        });
        let (engine_receipt_digest, engine_receipt) = artifact(
            "measurement",
            canonical_bytes(&serde_json::json!({
                "schema_version": 1,
                "invocation": invocation_value,
                "observation": observation_without_link,
            })),
        );
        let observation = canonical_bytes(&serde_json::json!({
            "schema_version": 1,
            "invocation_id": observation_invocation_id,
            "status": status,
            "output_ref": format!("output:{}", &output_digest[7..]),
            "output_digest": output_digest.clone(),
            "source_lineage": ["input:fixture", "source:fixture"],
            "measurements": [{
                "name": "output_tokens",
                "unit": "token",
                "classification": "measured",
                "value": 4
            }],
            "receipt_link": {
                "schema_version": 1,
                "receipt_id": "engine-receipt-real",
                "receipt_ref": format!("receipt:{engine_receipt_digest}"),
                "receipt_digest": engine_receipt_digest.clone(),
                "invocation_id": "invocation-real"
            }
        }));
        let (observation_digest, observation) = artifact("engine_observation", observation);
        let evidence = BTreeMap::from([
            (observation_digest.clone(), "measurement".to_owned()),
            (engine_receipt_digest.clone(), "measurement".to_owned()),
        ]);
        let inventory = BTreeMap::from([
            (output_digest, output),
            (engine_receipt_digest, engine_receipt),
            (invocation_ref.clone(), invocation),
            (observation_digest, observation),
        ]);
        (
            evidence,
            inventory,
            "invocation-real".to_owned(),
            invocation_ref,
        )
    }

    fn engine_observation_digest(inventory: &BTreeMap<String, InventoryArtifact>) -> String {
        inventory
            .iter()
            .find_map(|(digest, artifact)| {
                (artifact.kind == "engine_observation").then_some(digest.clone())
            })
            .unwrap()
    }

    fn engine_receipt_digest(inventory: &BTreeMap<String, InventoryArtifact>) -> String {
        let observation_digest = engine_observation_digest(inventory);
        let observation = artifact_document(inventory, &observation_digest, "observation").unwrap();
        let observation = object(&observation, "observation").unwrap();
        let link = object(
            field(observation, "receipt_link", "observation").unwrap(),
            "link",
        )
        .unwrap();
        string(link, "receipt_digest", "link").unwrap().to_owned()
    }

    fn replace_engine_receipt_bytes(
        evidence: &mut BTreeMap<String, String>,
        inventory: &mut BTreeMap<String, InventoryArtifact>,
        bytes: Vec<u8>,
    ) {
        let observation_digest = engine_observation_digest(inventory);
        let old_receipt_digest = engine_receipt_digest(inventory);
        let mut observation =
            artifact_document(inventory, &observation_digest, "observation").unwrap();
        let link = observation
            .as_object_mut()
            .unwrap()
            .get_mut("receipt_link")
            .unwrap()
            .as_object_mut()
            .unwrap();
        let new_receipt_digest = format!("sha256:{}", sha256_hex(&bytes));
        link.insert(
            "receipt_ref".to_owned(),
            Value::String(format!("receipt:{new_receipt_digest}")),
        );
        link.insert(
            "receipt_digest".to_owned(),
            Value::String(new_receipt_digest.clone()),
        );
        let observation_bytes = canonical_bytes(&observation);
        let new_observation_digest = format!("sha256:{}", sha256_hex(&observation_bytes));

        let mut receipt_artifact = inventory.remove(&old_receipt_digest).unwrap();
        receipt_artifact.bytes = bytes;
        inventory.insert(new_receipt_digest.clone(), receipt_artifact);

        let mut observation_artifact = inventory.remove(&observation_digest).unwrap();
        observation_artifact.bytes = observation_bytes;
        inventory.insert(new_observation_digest.clone(), observation_artifact);

        let receipt_kind = evidence.remove(&old_receipt_digest).unwrap();
        evidence.insert(new_receipt_digest, receipt_kind);
        let observation_kind = evidence.remove(&observation_digest).unwrap();
        evidence.insert(new_observation_digest, observation_kind);
    }

    fn replace_engine_receipt_value<F>(
        evidence: &mut BTreeMap<String, String>,
        inventory: &mut BTreeMap<String, InventoryArtifact>,
        mutate: F,
    ) where
        F: FnOnce(&mut Value),
    {
        let receipt_digest = engine_receipt_digest(inventory);
        let mut receipt = artifact_document(inventory, &receipt_digest, "engine receipt").unwrap();
        mutate(&mut receipt);
        replace_engine_receipt_bytes(evidence, inventory, canonical_bytes(&receipt));
    }

    #[test]
    fn direct_artifact_resolution_rehashes_bytes() {
        let advertised = format!("sha256:{}", "0".repeat(64));
        let inventory = BTreeMap::from([(
            advertised.clone(),
            InventoryArtifact {
                kind: "task_envelope".to_string(),
                bytes: b"{}".to_vec(),
            },
        )]);

        assert!(artifact_document(&inventory, &advertised, "task").is_err());
        assert!(require_artifact(&advertised, &inventory, "task").is_err());
    }

    #[test]
    fn engine_observation_semantics_bind_invocation_and_receipt_evidence() {
        let (evidence, inventory, invocation_id, invocation_ref) =
            observation_fixture("invocation-real", "succeeded");
        verify_engine_observation(&evidence, &inventory, &invocation_id, &invocation_ref).unwrap();

        let (evidence, inventory, invocation_id, invocation_ref) =
            observation_fixture("invocation-other", "succeeded");
        assert!(
            verify_engine_observation(&evidence, &inventory, &invocation_id, &invocation_ref)
                .is_err()
        );

        let (evidence, inventory, invocation_id, invocation_ref) =
            observation_fixture("invocation-real", "unknown");
        assert!(
            verify_engine_observation(&evidence, &inventory, &invocation_id, &invocation_ref)
                .is_err()
        );

        let (mut evidence, inventory, invocation_id, invocation_ref) =
            observation_fixture("invocation-real", "succeeded");
        let engine_receipt_digest = inventory
            .iter()
            .find_map(|(digest, artifact)| {
                (artifact.kind == "measurement").then_some(digest.clone())
            })
            .unwrap();
        evidence.remove(&engine_receipt_digest);
        assert!(
            verify_engine_observation(&evidence, &inventory, &invocation_id, &invocation_ref)
                .is_err()
        );
    }

    #[test]
    fn engine_receipt_artifact_rejects_noncanonical_bytes() {
        let (mut evidence, mut inventory, invocation_id, invocation_ref) =
            observation_fixture("invocation-real", "succeeded");
        let receipt_digest = engine_receipt_digest(&inventory);
        let mut noncanonical = vec![b' '];
        noncanonical.extend_from_slice(&inventory[&receipt_digest].bytes);
        replace_engine_receipt_bytes(&mut evidence, &mut inventory, noncanonical);

        assert!(
            verify_engine_observation(&evidence, &inventory, &invocation_id, &invocation_ref)
                .is_err()
        );
    }

    #[test]
    fn engine_receipt_artifact_rejects_mismatched_invocation() {
        let (mut evidence, mut inventory, invocation_id, invocation_ref) =
            observation_fixture("invocation-real", "succeeded");
        replace_engine_receipt_value(&mut evidence, &mut inventory, |receipt| {
            receipt
                .as_object_mut()
                .unwrap()
                .get_mut("invocation")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(
                    "invocation_id".to_owned(),
                    Value::String("invocation-other".to_owned()),
                );
        });

        assert!(
            verify_engine_observation(&evidence, &inventory, &invocation_id, &invocation_ref)
                .is_err()
        );
    }

    #[test]
    fn engine_receipt_artifact_rejects_mismatched_observation() {
        let (mut evidence, mut inventory, invocation_id, invocation_ref) =
            observation_fixture("invocation-real", "succeeded");
        replace_engine_receipt_value(&mut evidence, &mut inventory, |receipt| {
            receipt
                .as_object_mut()
                .unwrap()
                .get_mut("observation")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .get_mut("measurements")
                .unwrap()
                .as_array_mut()
                .unwrap()[0]
                .as_object_mut()
                .unwrap()
                .insert("value".to_owned(), Value::from(5));
        });

        assert!(
            verify_engine_observation(&evidence, &inventory, &invocation_id, &invocation_ref)
                .is_err()
        );
    }

    #[test]
    fn engine_receipt_artifact_rejects_self_linked_observation() {
        let (mut evidence, mut inventory, invocation_id, invocation_ref) =
            observation_fixture("invocation-real", "succeeded");
        let receipt_digest = engine_receipt_digest(&inventory);
        replace_engine_receipt_value(&mut evidence, &mut inventory, |receipt| {
            receipt
                .as_object_mut()
                .unwrap()
                .get_mut("observation")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(
                    "receipt_link".to_owned(),
                    serde_json::json!({
                        "schema_version": 1,
                        "receipt_id": "self-linked",
                        "receipt_ref": format!("receipt:{receipt_digest}"),
                        "receipt_digest": receipt_digest,
                        "invocation_id": "invocation-real"
                    }),
                );
        });

        assert!(
            verify_engine_observation(&evidence, &inventory, &invocation_id, &invocation_ref)
                .is_err()
        );
    }
}
