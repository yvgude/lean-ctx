// SPDX-License-Identifier: Apache-2.0
//! Framed operator attestation; no caller credentials, paths or learning toggle.

use std::{
    io::{BufReader, Read},
    path::Path,
};

use lean_ctx_protocol::{EngineOutcomeRequestV1, MAX_ENGINE_OUTCOME_REQUEST_BYTES};

use super::{EngineCliError, host_error, read_bounded, read_framed_authority};

pub(super) fn run(args: &[String], input: &mut impl Read) -> Result<String, EngineCliError> {
    if !matches!(args.len(), 2 | 4)
        || args[1] != "--host-stdin"
        || (args.len() == 4 && args[2] != "--ledger-root")
    {
        return Err(EngineCliError::Usage);
    }
    let mut reader = BufReader::new(input);
    let authority = read_framed_authority(&mut reader, args.get(3).map(Path::new))?;
    let bytes = read_bounded(
        &mut reader,
        MAX_ENGINE_OUTCOME_REQUEST_BYTES,
        "invalid_host_outcome_request",
    )?;
    let request: EngineOutcomeRequestV1 =
        serde_json::from_slice(&bytes).map_err(|_| host_error("invalid_host_outcome_request"))?;
    request
        .validate()
        .map_err(|_| host_error("invalid_host_outcome_request"))?;
    let result = authority
        .observe_engine_outcome(&request)
        .map_err(|_| host_error("host_outcome_rejected"))?;
    serde_json::to_string(&result).map_err(|_| host_error("invalid_host_outcome_response"))
}
