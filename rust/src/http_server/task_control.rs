// SPDX-License-Identifier: Apache-2.0

//! Signed remote task control over the existing authenticated A2A transport.

use axum::{Json, http::StatusCode};
use serde_json::Value;

use crate::core::a2a::{
    task::{
        TASK_ACTION_CANCEL, TASK_ACTION_GET, TaskAuthorityExpectationV1, TaskControlDescriptorV1,
        TaskStore,
    },
    task_control::{self, TaskControlError},
    task_response::AdmittedStatusSigner,
};

type Reply = (StatusCode, Json<Value>);

pub(super) fn is_control(payload: &str) -> bool {
    serde_json::from_str::<TaskControlDescriptorV1>(payload).is_ok_and(|request| {
        matches!(
            request.action.as_str(),
            TASK_ACTION_GET | TASK_ACTION_CANCEL
        )
    })
}

pub(super) fn accept(
    state: &super::AppState,
    request: &TaskControlDescriptorV1,
    expected: &TaskAuthorityExpectationV1<'_>,
) -> Result<Reply, Reply> {
    // Even a valid relay record cannot select another locally stored identity.
    if state.a2a_recipient_id.as_deref() != Some(expected.recipient) {
        return Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "task_response_identity_mismatch",
        ));
    }
    let policy = &state.a2a_task_authority;
    request
        .verify_authority(policy, expected)
        .map_err(|_| error(StatusCode::UNAUTHORIZED, "unauthorized_task_authority"))?;
    // Missing/revoked/wrong-scope receiver signing authority must reject before
    // cancellation changes durable state. No registry/key is created here.
    let signer = AdmittedStatusSigner::existing(request, policy, expected).map_err(|_| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "task_response_signer_unavailable",
        )
    })?;
    let path = TaskStore::scoped_path(&state.project_root).map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "task_storage_unavailable",
        )
    })?;
    let status = match request.action.as_str() {
        TASK_ACTION_GET => task_control::get_status(&path, request, policy, expected),
        TASK_ACTION_CANCEL => task_control::cancel(&path, request, policy, expected),
        _ => return Err(error(StatusCode::BAD_REQUEST, "invalid_task_control")),
    }
    .map_err(|failure| match failure {
        TaskControlError::Authority(_) => {
            error(StatusCode::UNAUTHORIZED, "unauthorized_task_authority")
        }
        TaskControlError::NotFound => error(StatusCode::NOT_FOUND, "task_not_found"),
        TaskControlError::InvalidTransition => {
            error(StatusCode::CONFLICT, "invalid_task_transition")
        }
        TaskControlError::Storage(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "task_storage_unavailable",
        ),
    })?;
    // An expired response is never disclosed. A post-persistence failure is an
    // uncertain acknowledgement, not rollback: retry/poll the same task safely.
    let reply = signer.sign(status, chrono::Utc::now()).map_err(|_| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "task_response_signer_unavailable",
        )
    })?;
    let value = serde_json::to_value(reply).map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "task_response_unavailable",
        )
    })?;
    Ok((StatusCode::OK, Json(value)))
}

fn error(status: StatusCode, code: &str) -> Reply {
    (status, Json(serde_json::json!({"error": code})))
}
