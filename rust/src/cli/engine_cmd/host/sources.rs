// SPDX-License-Identifier: Apache-2.0

//! Operator-only source execution: bounded host settings and request over stdin.

use std::io::{BufReader, Read};
use std::path::Path;

use lean_ctx_protocol::{
    AcceptanceState, EngineContextSourceCanonicalReceiptV1, EngineContextSourceExecutionRequestV1,
    EngineContextSourceExecutionResponseV1, EngineContextSourceExecutionResponseV2,
    EngineContextSourceExecutionViewV1, ProtocolReference, ReceiptId, Sha256Digest,
};

use super::{
    EngineCliError, REQUEST_LIMIT, SemanticVersion, host_error, read_bounded, read_framed_authority,
};

pub(super) fn run(args: &[String], input: &mut impl Read) -> Result<String, EngineCliError> {
    if !matches!(args.len(), 4 | 6)
        || args[1] != "--project-root"
        || args[3] != "--host-stdin"
        || (args.len() == 6 && args[4] != "--ledger-root")
    {
        return Err(EngineCliError::Usage);
    }
    let ledger_root = args.get(5).map(Path::new);
    let mut reader = BufReader::new(input);
    let authority = read_framed_authority(&mut reader, ledger_root)?;
    authority
        .require_context_decision_signing()
        .map_err(EngineCliError::Host)?;
    let bytes = read_bounded(&mut reader, REQUEST_LIMIT, "invalid_host_source_request")?;
    let request: EngineContextSourceExecutionRequestV1 =
        serde_json::from_slice(&bytes).map_err(|_| host_error("invalid_host_source_request"))?;
    super::super::validate_header(
        request.schema_version,
        request.transport_version,
        &request.engine_interface_version,
    )?;
    // Preserve the existing materialization error code at the typed boundary.
    request
        .materialization
        .validate_payload()
        .map_err(|_| host_error("invalid_source_materialization_request"))?;
    request
        .validate()
        .map_err(|_| host_error("invalid_host_source_request"))?;
    let result = crate::core::engine_interface::source_execution::execute(
        Path::new(&args[2]),
        &request.task,
        &request.plan,
        &request.materialization,
        &authority,
    )
    .map_err(EngineCliError::Host)?;
    let receipt_document_json = if args[0] == "context-sources-receipt-v2" {
        Some(
            authority
                .published_receipt_json(&result.receipt)
                .map_err(|_| host_error("host_receipt_document_unavailable"))?,
        )
    } else {
        None
    };
    let invalid = |_| host_error("invalid_host_source_response");
    let response = EngineContextSourceExecutionResponseV1 {
        schema_version: 1,
        transport_version: 1,
        engine_interface_version: SemanticVersion::new("1.0.0").map_err(invalid)?,
        source_plan: result.source_plan,
        execution_plan: result.execution_plan,
        view: EngineContextSourceExecutionViewV1 {
            text: result.view.text,
            output_ref: result.view.output_ref,
            output_digest: result.view.output_digest,
        },
        invocation: result.invocation,
        observation: result.observation,
        canonical_receipt: EngineContextSourceCanonicalReceiptV1 {
            receipt_id: ReceiptId::new(result.receipt.receipt_id).map_err(invalid)?,
            receipt_ref: ProtocolReference::new(result.receipt.receipt_ref).map_err(invalid)?,
            receipt_digest: Sha256Digest::new(result.receipt.receipt_digest).map_err(invalid)?,
            outcome: AcceptanceState::Unknown,
        },
    };
    response.validate_against(&request).map_err(invalid)?;
    if let Some(receipt_document_json) = receipt_document_json {
        let response = EngineContextSourceExecutionResponseV2 {
            schema_version: 2,
            execution: response,
            receipt_document_json,
        };
        response.validate_against(&request).map_err(invalid)?;
        return serde_json::to_string(&response)
            .map_err(|_| host_error("invalid_host_source_response"));
    }
    serde_json::to_string(&response).map_err(|_| host_error("invalid_host_source_response"))
}
