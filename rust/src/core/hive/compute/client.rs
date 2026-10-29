// SPDX-License-Identifier: Apache-2.0
//! E47–E52 (C0 2.x `schemas/compute.schema.json`): requests are validated
//! before they leave, answers before they are used, refusals worded for
//! people ([`refusal`]).

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::core::hive::HiveError;
use crate::core::hive::transport::HiveClient;
use lean_ctx_protocol::hive::compute::{
    ComputeBundleResponseV1, ComputeJobResponseV1, ComputeJobV1, ComputeRefusalV1,
    ComputeResultResponseV1, ComputeSubmitRequestV1, MAX_BUNDLE_BYTES, is_digest, is_job_id,
};

/// Largest JSON answer of these routes (a job or a result is a few KiB).
const MAX_ANSWER: usize = 256 * 1024;

/// Which route answered: a plain `404` means something else on each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    Submit,
    Job,
    Bundle,
    Blob,
}

/// The refusal of a compute route, worded. `feature_disabled`,
/// `credit_exhausted` and `cap_reached` are shown with their numbers.
pub(crate) fn refusal(route: Route, status: u16, answer: &Value) -> HiveError {
    match ComputeRefusalV1::parse(answer) {
        Some(ComputeRefusalV1::FeatureDisabled) => {
            return HiveError::Refused(
                "feature_disabled: Task Compute is not enabled for this workspace.".into(),
            );
        }
        Some(ComputeRefusalV1::CreditExhausted { balance, needed }) => {
            return HiveError::Refused(format!(
                "credit_exhausted: the workspace's prepaid compute credit ({balance}) does not \
                 cover this job's reserve ({needed} credits). Nothing was charged or queued; a \
                 workspace owner tops up on the web."
            ));
        }
        Some(ComputeRefusalV1::CapReached {
            cap,
            limit,
            requested,
        }) => {
            return HiveError::Refused(format!(
                "cap_reached: this job would pass the workspace cap {} ({}: limit {limit}, \
                 requested {requested}). Nothing was queued; ask less, or a workspace admin \
                 raises the cap on the web.",
                cap.as_str(),
                cap.describe()
            ));
        }
        None => {}
    }
    let error = &answer["error"];
    let code = error["code"].as_str().unwrap_or_default();
    let detail = |key: &str| error[key].as_str().unwrap_or_default().to_owned();
    match (status, code) {
        (404, _) if matches!(route, Route::Submit | Route::Bundle) => {
            HiveError::Refused("not_available: this Hive does not offer Task Compute.".into())
        }
        (404, _) => HiveError::Refused(
            "not_found: no such compute job (or artifact) in this workspace.".into(),
        ),
        (409, "invalid_state") => {
            let state = detail("state");
            if state == "not_finished" {
                HiveError::Refused(
                    "not_finished: the job is still queued or running; check it with `status`."
                        .into(),
                )
            } else {
                HiveError::Refused(format!(
                    "invalid_state: {}",
                    if state.is_empty() { code } else { &state }
                ))
            }
        }
        (400, "invalid_request") => {
            let field = detail("field");
            let reason = detail("reason");
            HiveError::Refused(
                format!("invalid_request: {field} {reason}")
                    .trim_end()
                    .to_owned(),
            )
        }
        _ => HiveError::from_response(status, answer),
    }
}

fn answer(
    client: &HiveClient,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<(u16, Value), HiveError> {
    let (status, answer) = client.send_answer(method, path, body, MAX_ANSWER)?;
    if status == 401 {
        return Err(HiveError::Unauthorized);
    }
    Ok((status, answer))
}

fn decode<T: DeserializeOwned>(value: Value, what: &str) -> Result<T, HiveError> {
    serde_json::from_value(value).map_err(|e| HiveError::Protocol(format!("{what}: {e}")))
}

fn job_path(job_id: &str, rest: &str) -> Result<String, HiveError> {
    if !is_job_id(job_id) {
        return Err(HiveError::Config(format!(
            "not a compute job id (cj_ + 32 hex): {job_id}"
        )));
    }
    Ok(format!("/compute/jobs/{job_id}{rest}"))
}

fn job(route: Route, status: u16, value: Value, what: &str) -> Result<ComputeJobV1, HiveError> {
    if !(200..300).contains(&status) {
        return Err(refusal(route, status, &value));
    }
    let response: ComputeJobResponseV1 = decode(value, what)?;
    response
        .validate()
        .map_err(|e| HiveError::Protocol(format!("{what}: {e}")))?;
    Ok(response.job)
}

/// E47: submit a validated request.
pub(crate) fn submit(
    client: &HiveClient,
    request: &ComputeSubmitRequestV1,
) -> Result<ComputeJobV1, HiveError> {
    request
        .validate()
        .map_err(|e| HiveError::Config(format!("invalid compute job: {e}")))?;
    let body = serde_json::to_value(request).map_err(|e| HiveError::Local(e.to_string()))?;
    let (status, value) = answer(client, "POST", "/compute/jobs", Some(&body))?;
    let job = job(Route::Submit, status, value, "compute job")?;
    if job.vault_id != request.vault_id
        || job.job_type != request.job_type
        || job.inputs != request.inputs
    {
        return Err(HiveError::Protocol(
            "compute job: the answer is not the submitted job".into(),
        ));
    }
    Ok(job)
}

/// E48: the job as it is now.
pub(crate) fn get(client: &HiveClient, job_id: &str) -> Result<ComputeJobV1, HiveError> {
    let (status, value) = answer(client, "GET", &job_path(job_id, "")?, None)?;
    let job = job(Route::Job, status, value, "compute job")?;
    same_job(&job.job_id, job_id)?;
    Ok(job)
}

/// E49: ask for the job to be cancelled (idempotent).
pub(crate) fn cancel(client: &HiveClient, job_id: &str) -> Result<ComputeJobV1, HiveError> {
    let (status, value) = answer(client, "POST", &job_path(job_id, "/cancel")?, None)?;
    let job = job(Route::Job, status, value, "compute job")?;
    same_job(&job.job_id, job_id)?;
    Ok(job)
}

/// E50: a terminal job's result and cost.
pub(crate) fn result(
    client: &HiveClient,
    job_id: &str,
) -> Result<ComputeResultResponseV1, HiveError> {
    let (status, value) = answer(client, "GET", &job_path(job_id, "/result")?, None)?;
    if !(200..300).contains(&status) {
        return Err(refusal(Route::Job, status, &value));
    }
    let response: ComputeResultResponseV1 = decode(value, "compute result")?;
    response
        .validate()
        .map_err(|e| HiveError::Protocol(format!("compute result: {e}")))?;
    same_job(&response.job_id, job_id)?;
    Ok(response)
}

/// E51: upload one bundle; `created` is `false` when Hive had it already.
pub(crate) fn put_bundle(
    client: &HiveClient,
    digest: &str,
    bytes: &[u8],
) -> Result<ComputeBundleResponseV1, HiveError> {
    if !is_digest(digest) || bytes.is_empty() || bytes.len() as u64 > MAX_BUNDLE_BYTES {
        return Err(HiveError::Config(
            "a compute bundle is 1 byte to 64 MiB with a sha256: digest".into(),
        ));
    }
    let (status, value) =
        client.put_bytes_answer(&format!("/compute/bundles/{digest}"), bytes, MAX_ANSWER)?;
    if status == 401 {
        return Err(HiveError::Unauthorized);
    }
    if !(200..300).contains(&status) {
        return Err(refusal(Route::Bundle, status, &value));
    }
    let response: ComputeBundleResponseV1 = decode(value, "compute bundle")?;
    response
        .validate_for(digest, bytes.len() as u64)
        .map_err(|e| HiveError::Protocol(format!("compute bundle: {e}")))?;
    Ok(response)
}

/// E52: the bytes of one artifact or log of `job_id`, at most `max` bytes;
/// the caller checks their digest.
pub(crate) fn blob(
    client: &HiveClient,
    job_id: &str,
    digest: &str,
    max: u64,
) -> Result<Vec<u8>, HiveError> {
    if !is_digest(digest) {
        return Err(HiveError::Config(format!("not a sha256 digest: {digest}")));
    }
    let path = job_path(job_id, &format!("/blobs/{digest}"))?;
    let max = max.min(MAX_BUNDLE_BYTES);
    // Room for an error body even when the blob itself is tiny.
    let limit = usize::try_from(max).unwrap_or(usize::MAX).max(MAX_ANSWER);
    let (status, bytes, value) = client.get_bytes_answer(&path, limit)?;
    if status == 401 {
        return Err(HiveError::Unauthorized);
    }
    if !(200..300).contains(&status) {
        return Err(refusal(Route::Blob, status, &value));
    }
    if bytes.len() as u64 > max {
        return Err(HiveError::Protocol(format!(
            "compute blob: larger than its {max} bytes"
        )));
    }
    Ok(bytes)
}

fn same_job(answered: &str, asked: &str) -> Result<(), HiveError> {
    if answered == asked {
        Ok(())
    } else {
        Err(HiveError::Protocol(
            "compute job: Hive answered for another job".into(),
        ))
    }
}
