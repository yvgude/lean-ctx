//! A2A-compatible envelope for handoff transfer bundles (GL#449).
//!
//! Wraps the proprietary `HandoffTransferBundleV1` in a spec-shaped A2A Task
//! object so a foreign agent can consume a lean-ctx handoff with a plain A2A
//! parser: `id` + `status` + one `data` artifact part carrying the bundle.
//! Deterministic: every field derives from the bundle itself (no wall clock).

use serde_json::Value;

use crate::core::handoff_transfer_bundle::HandoffTransferBundleV1;

/// Media type identifying the embedded bundle payload.
pub const BUNDLE_MIME_V1: &str = "application/vnd.leanctx.handoff-bundle.v1+json";

/// Wrap a transfer bundle in an A2A Task envelope.
///
/// The task id is derived from the embedded ledger's session id and content
/// hash, so re-exporting the same handoff yields the same task id.
pub fn wrap_bundle_as_a2a_task(bundle: &HandoffTransferBundleV1) -> Result<Value, String> {
    let bundle_json =
        serde_json::to_value(bundle).map_err(|e| format!("bundle serialization failed: {e}"))?;

    let session_id = &bundle.ledger.session.id;
    let task_id = format!(
        "handoff-{session_id}-{}",
        &bundle.ledger.content_md5[..bundle.ledger.content_md5.len().min(8)]
    );

    Ok(serde_json::json!({
        "id": task_id,
        "status": {
            "state": "completed",
            "timestamp": bundle.exported_at.to_rfc3339(),
        },
        "messages": [],
        "artifacts": [{
            "type": "data",
            "mimeType": BUNDLE_MIME_V1,
            "data": bundle_json,
        }],
        "history": [],
        "metadata": {
            "producer": "lean-ctx",
            "bundleSchemaVersion": bundle.schema_version,
            "privacy": bundle.privacy,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::handoff_ledger::HandoffLedgerV1;
    use crate::core::handoff_transfer_bundle::{ArtifactsExcerptV1, ProjectIdentityV1};

    fn sample_bundle() -> HandoffTransferBundleV1 {
        let mut ledger = HandoffLedgerV1::default();
        ledger.session.id = "sess-42".to_string();
        ledger.content_md5 = "abcdef0123456789".to_string();
        HandoffTransferBundleV1 {
            schema_version: 1,
            exported_at: chrono::DateTime::parse_from_rfc3339("2026-06-01T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            privacy: "redacted".to_string(),
            project: ProjectIdentityV1 {
                project_root_hash: None,
                project_identity_hash: None,
            },
            ledger,
            artifacts: ArtifactsExcerptV1::default(),
            signature: None,
            signer_public_key: None,
            signer_agent_id: None,
            signature_scheme: None,
        }
    }

    #[test]
    fn wraps_bundle_with_required_a2a_task_fields() {
        let task = wrap_bundle_as_a2a_task(&sample_bundle()).unwrap();
        assert_eq!(task["id"], "handoff-sess-42-abcdef01");
        assert_eq!(task["status"]["state"], "completed");
        assert_eq!(task["artifacts"][0]["type"], "data");
        assert_eq!(task["artifacts"][0]["mimeType"], BUNDLE_MIME_V1);
        assert_eq!(task["artifacts"][0]["data"]["schema_version"], 1);
        assert_eq!(task["metadata"]["producer"], "lean-ctx");
    }

    #[test]
    fn wrap_is_deterministic() {
        let a = wrap_bundle_as_a2a_task(&sample_bundle()).unwrap();
        let b = wrap_bundle_as_a2a_task(&sample_bundle()).unwrap();
        assert_eq!(a, b);
    }
}
