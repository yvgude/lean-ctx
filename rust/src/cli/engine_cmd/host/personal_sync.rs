// SPDX-License-Identifier: Apache-2.0
//! Operator-only framed IPC for selected-project personal session sync.

use std::io::{BufRead, Read};

use serde::Deserialize;
use serde_json::Value;
use zeroize::Zeroizing;

use crate::core::execution_ledger::host::{
    PersonalCarrierV1, PersonalSyncAdmission, receive_personal_sync, snapshot_personal_sync,
};

use super::{EngineCliError, host_error};

const SETTINGS_LIMIT: usize = 16 * 1024;

#[derive(Deserialize)]
struct PersonalSyncHost {
    personal_sync: PersonalSyncAdmission,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersonalSyncRequest {
    schema_version: u32,
    action: String,
    #[serde(default)]
    expected_head: Option<String>,
    #[serde(default)]
    payload: Option<PersonalCarrierV1>,
    #[serde(default)]
    payload_sha256: Option<String>,
}

pub(super) fn run(input: &mut impl Read) -> Result<String, EngineCliError> {
    let mut reader = std::io::BufReader::new(input);
    let admission = read_admission(&mut reader)?;
    admission
        .validate()
        .map_err(|_| host_error("invalid_personal_sync_admission"))?;

    let request_bytes = super::read_bounded(
        &mut reader,
        super::REQUEST_LIMIT,
        "invalid_personal_sync_request",
    )?;
    let request = serde_json::from_slice::<PersonalSyncRequest>(&request_bytes)
        .map_err(|_| host_error("invalid_personal_sync_request"))?;
    if request.schema_version != 1 {
        return Err(host_error("unsupported_personal_sync_schema"));
    }

    // Check the exact action-specific field set as well as deny_unknown_fields;
    // snapshot must not smuggle receive data, including explicit null fields.
    let request_value: Value = serde_json::from_slice(&request_bytes)
        .map_err(|_| host_error("invalid_personal_sync_request"))?;
    let object = request_value
        .as_object()
        .ok_or_else(|| host_error("invalid_personal_sync_request"))?;
    let exact_keys = |expected: &[&str]| {
        object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
    };

    let project_root = selected_project_root()?;
    let output = match request.action.as_str() {
        "snapshot" if exact_keys(&["schema_version", "action"]) => {
            snapshot_personal_sync(&admission, &project_root)
                .map_err(|_| host_error("personal_sync_rejected"))?
        }
        "receive"
            if exact_keys(&[
                "schema_version",
                "action",
                "expected_head",
                "payload",
                "payload_sha256",
            ]) =>
        {
            let payload = request
                .payload
                .ok_or_else(|| host_error("invalid_personal_sync_request"))?;
            let payload_sha256 = request
                .payload_sha256
                .as_deref()
                .ok_or_else(|| host_error("invalid_personal_sync_request"))?;
            receive_personal_sync(
                &admission,
                &project_root,
                request.expected_head.as_deref(),
                &payload,
                payload_sha256,
            )
            .map_err(|_| host_error("personal_sync_rejected"))?
        }
        _ => return Err(host_error("invalid_personal_sync_request")),
    };
    serde_json::to_string(&output).map_err(|_| host_error("personal_sync_response_unavailable"))
}

fn read_admission(reader: &mut impl BufRead) -> Result<PersonalSyncAdmission, EngineCliError> {
    // The host frame can contain unrelated operator settings; deserialize only
    // its short-lived personal_sync bridge field and never load signing keys.
    let mut settings = Zeroizing::new(Vec::new());
    reader
        .by_ref()
        .take(SETTINGS_LIMIT as u64 + 1)
        .read_until(b'\n', &mut settings)
        .map_err(|_| host_error("invalid_personal_sync_admission"))?;
    if settings.len() > SETTINGS_LIMIT || !settings.ends_with(b"\n") {
        return Err(host_error("invalid_personal_sync_admission"));
    }
    serde_json::from_slice::<PersonalSyncHost>(&settings)
        .map(|host| host.personal_sync)
        .map_err(|_| host_error("invalid_personal_sync_admission"))
}

fn selected_project_root() -> Result<String, EngineCliError> {
    let root = std::env::current_dir()
        .and_then(std::fs::canonicalize)
        .map_err(|_| host_error("personal_sync_project_unavailable"))?;
    if crate::core::pathutil::is_broad_or_unsafe_root(&root) {
        return Err(host_error("personal_sync_project_unavailable"));
    }
    root.to_str()
        .map(str::to_owned)
        .ok_or_else(|| host_error("personal_sync_project_unavailable"))
}
