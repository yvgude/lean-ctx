// SPDX-License-Identifier: Apache-2.0
//! Operator-owned logical identity for an exact, safe local project root.

use std::path::{Path, PathBuf};

use lean_ctx_protocol::{ProjectId, TenantId};
use serde::Deserialize;

use super::HostReceiptAuthority;
use crate::core::task_spine::AdmittedTaskIdentity;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HostTaskScope {
    project_root: PathBuf,
    project_id: ProjectId,
    tenant_id: TenantId,
}

impl HostReceiptAuthority {
    /// Recheck signer and exact workspace on every call, including cached replay.
    /// Saved sessions and MCP arguments cannot grant or change this identity.
    pub(crate) fn admit_task_identity(
        &self,
        project_root: Option<&str>,
    ) -> Result<Option<AdmittedTaskIdentity>, &'static str> {
        let Some(scope) = &self.task_scope else {
            return Ok(None);
        };
        self.validate_current()?;
        let receiving = project_root
            .map(Path::new)
            .ok_or("host_task_scope_root_missing")?;
        if !scope.project_root.is_absolute() || !receiving.is_absolute() {
            return Err("host_task_scope_root_invalid");
        }
        let expected = std::fs::canonicalize(&scope.project_root)
            .map_err(|_| "host_task_scope_root_invalid")?;
        let actual =
            std::fs::canonicalize(receiving).map_err(|_| "host_task_scope_root_invalid")?;
        if !expected.is_dir()
            || crate::core::pathutil::is_broad_or_unsafe_root(&expected)
            || expected != actual
        {
            return Err("host_task_scope_root_mismatch");
        }
        Ok(Some(AdmittedTaskIdentity {
            project_id: scope.project_id.clone(),
            tenant_id: scope.tenant_id.clone(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use serde_json::{Value, json};

    fn settings(root: &Path) -> Value {
        let key = SigningKey::from_bytes(&[61; 32]);
        json!({"schema_version":1, "signing_key_hex":"3d".repeat(32),
            "ledger_path":root.join("ledger.jsonl"),
            "signer":{"key_id":"task-scope-test",
                "public_key_digest":super::super::digest(key.verifying_key().as_bytes()).unwrap(),
                "admitted_at":"2020-01-01T00:00:00Z", "expires_at":"9999-01-01T00:00:00Z", "revoked_at":null},
            "task_scope":{"project_root":root,"project_id":"portable-project","tenant_id":"local-account"}})
    }

    fn authority(settings: &Value) -> HostReceiptAuthority {
        HostReceiptAuthority::from_reader(&mut serde_json::to_vec(settings).unwrap().as_slice())
            .unwrap()
    }

    #[test]
    fn host_task_scope_requires_exact_safe_root_and_explicit_operator_binding() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        let config = settings(&root);
        let host = authority(&config);
        let identity = host.admit_task_identity(root.to_str()).unwrap().unwrap();
        assert_eq!(identity.project_id.as_str(), "portable-project");
        assert_eq!(identity.tenant_id.as_str(), "local-account");
        let foreign = tempfile::tempdir().unwrap();
        for receiving in [None, Some("relative"), foreign.path().to_str()] {
            assert!(host.admit_task_identity(receiving).is_err());
        }
        for configured in [Path::new("relative"), Path::new("/")] {
            let mut invalid = config.clone();
            invalid["task_scope"]["project_root"] = json!(configured);
            assert!(
                authority(&invalid)
                    .admit_task_identity(root.to_str())
                    .is_err()
            );
        }
        let mut absent = config.clone();
        absent.as_object_mut().unwrap().remove("task_scope");
        assert!(
            authority(&absent)
                .admit_task_identity(root.to_str())
                .unwrap()
                .is_none()
        );
        let mut malformed = config;
        malformed["task_scope"]["caller_granted"] = json!(true);
        assert!(
            HostReceiptAuthority::from_reader(
                &mut serde_json::to_vec(&malformed).unwrap().as_slice()
            )
            .is_err()
        );
        assert!(!root.join("ledger.jsonl").exists());
    }
}
