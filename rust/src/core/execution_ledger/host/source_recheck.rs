// SPDX-License-Identifier: Apache-2.0
//! Current receiver-owned file acquisition for authenticated transfer dependencies.
//! A successful check is an observation, never a lease or narrative promotion.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Result, ensure};
use lean_ctx_protocol::Sha256Digest;
use serde::Deserialize;

use crate::core::policy::runtime;

/// Supplied only through the receiving operator's host configuration. Foreign
/// package paths and caller-supplied source bodies never enter this mapping.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReceivingFileBinding {
    pub(super) source_ref: String,
    pub(super) path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReceivingProviderBinding {
    pub(super) source_ref: String,
    pub(super) origin: crate::core::providers::provenance::ProviderOrigin,
}

const MAX_SOURCES: usize = 128;
const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;

impl super::HostReceiptAuthority {
    pub(crate) fn recheck_checkpoint_sources(
        &self,
        request: &super::checkpoint_transfer::HostCheckpointImportRequest,
    ) -> Result<serde_json::Value> {
        use super::checkpoint_transfer::SourceKind;
        ensure!(
            self.checkpoint_source_files.is_some() || self.checkpoint_source_providers.is_some(),
            "receiving source check not authorized"
        );
        let verified = self.inspect_checkpoint_sources(request)?;
        let root = verified
            .root
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("unsafe receiving root"))?;
        if let Ok(Some(bound)) = runtime::REQUEST_PROJECT.try_with(|slot| slot.borrow().clone()) {
            ensure!(bound == verified.root, "receiving source scope mismatch");
        }
        runtime::with_project_source_view(root, || -> Result<serde_json::Value> {
            let current = self.inspect_checkpoint_sources(request)?;
            ensure!(
                current.root == verified.root && current.dependencies == verified.dependencies,
                "receiving source evidence changed"
            );
            let mut files = BTreeMap::new();
            let mut providers = BTreeMap::new();
            for (reference, dependency) in &current.dependencies {
                match dependency.kind {
                    SourceKind::File => {
                        files.insert(reference.clone(), dependency.digest.clone());
                    }
                    SourceKind::Provider => {
                        providers.insert(reference.clone(), dependency.digest.clone());
                    }
                    SourceKind::Unsupported => anyhow::bail!("receiving source type unsupported"),
                }
            }
            let file_bindings = self.checkpoint_source_files.as_deref().unwrap_or_default();
            let provider_bindings = self
                .checkpoint_source_providers
                .as_deref()
                .unwrap_or_default();
            ensure!(
                files.len() == file_bindings.len() && providers.len() == provider_bindings.len(),
                "receiving source mapping coverage mismatch"
            );
            if !files.is_empty() {
                check_files(&current.root, &files, file_bindings)?;
            }
            if !providers.is_empty() {
                check_providers(&current.root, &providers, provider_bindings)?;
            }
            let latest = self.inspect_checkpoint_sources(request)?;
            ensure!(
                latest.root == current.root && latest.dependencies == current.dependencies,
                "receiving source evidence changed"
            );
            self.validate_current().map_err(anyhow::Error::msg)?;
            Ok(serde_json::json!({
                "schema_version":"leanctx.checkpoint-source-check/v1",
                "package_digest":current.package_digest,
                "source_count":current.dependencies.len(),
                "current_source_reads_verified":true,
                "coverage":"recorded_invocations_only",
                "narrative_source_coverage":"unknown",
                "automatic_continuation_admitted":false,
                "authorization_lease":false,
            }))
        })
        .map_err(|_| anyhow::anyhow!("receiving source policy changed or unavailable"))?
    }
}

pub(super) fn check_providers(
    root: &Path,
    dependencies: &BTreeMap<String, Sha256Digest>,
    bindings: &[ReceivingProviderBinding],
) -> Result<()> {
    ensure!(
        !dependencies.is_empty()
            && dependencies.len() <= 16
            && bindings.len() == dependencies.len(),
        "receiving provider coverage unavailable"
    );
    let mut mapped = BTreeMap::new();
    for binding in bindings {
        ensure!(
            dependencies.contains_key(&binding.source_ref)
                && mapped
                    .insert(binding.source_ref.as_str(), &binding.origin)
                    .is_none(),
            "receiving provider mapping mismatch"
        );
    }
    let root_text = root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("unsafe receiving root"))?;
    if let Ok(Some(bound)) = runtime::REQUEST_PROJECT.try_with(|slot| slot.borrow().clone()) {
        ensure!(bound == root, "receiving source scope mismatch");
    }
    runtime::with_project_source_view(root_text, || -> Result<()> {
        ensure!(
            runtime::REQUEST_PROJECT
                .try_with(|slot| slot.borrow().as_deref() == Some(root))
                .unwrap_or(false),
            "receiving source scope mismatch"
        );
        crate::core::providers::provenance::with_reuse_deadline(|| -> Result<()> {
            for _ in 0..2 {
                let mut remaining = MAX_TOTAL_BYTES;
                for (reference, expected) in dependencies {
                    let origin = mapped
                        .get(reference.as_str())
                        .ok_or_else(|| anyhow::anyhow!("receiving provider mapping missing"))?;
                    let view = origin
                        .current_chunks(root_text)
                        .map_err(|_| anyhow::anyhow!("receiving provider unavailable"))?;
                    ensure!(
                        view.chunks.len() == 1,
                        "receiving provider result ambiguous"
                    );
                    let content = &view.chunks[0].content;
                    remaining = remaining.checked_sub(view.original_bytes).ok_or_else(|| {
                        anyhow::anyhow!("receiving provider content exceeds bound")
                    })?;
                    ensure!(
                        super::digest(content.as_bytes()).map_err(anyhow::Error::msg)? == *expected,
                        "receiving provider changed or requires transformation"
                    );
                }
            }
            Ok(())
        })
    })
    .map_err(|_| anyhow::anyhow!("receiving provider policy changed or unavailable"))?
}

pub(super) fn check_files(
    root: &Path,
    dependencies: &BTreeMap<String, Sha256Digest>,
    bindings: &[ReceivingFileBinding],
) -> Result<()> {
    ensure!(
        !dependencies.is_empty()
            && dependencies.len() <= MAX_SOURCES
            && bindings.len() == dependencies.len(),
        "receiving source coverage unavailable"
    );
    let mut mapped = BTreeMap::new();
    for binding in bindings {
        ensure!(
            dependencies.contains_key(&binding.source_ref)
                && mapped
                    .insert(binding.source_ref.as_str(), binding)
                    .is_none(),
            "receiving source mapping mismatch"
        );
        let path = Path::new(&binding.path);
        ensure!(
            !binding.path.is_empty()
                && binding.path.len() <= 4096
                && !binding.path.chars().any(char::is_control)
                && !path.is_absolute()
                && path
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_))),
            "receiving source mapping invalid"
        );
    }
    let root_text = root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("unsafe receiving root"))?;
    if let Ok(Some(bound)) = runtime::REQUEST_PROJECT.try_with(|slot| slot.borrow().clone()) {
        ensure!(bound == root, "receiving source scope mismatch");
    }
    runtime::with_project_source_view(root_text, || -> Result<()> {
        ensure!(
            runtime::REQUEST_PROJECT
                .try_with(|slot| slot.borrow().as_deref() == Some(root))
                .unwrap_or(false),
            "receiving source scope mismatch"
        );
        // Reacquire immediately before returning, using the same policy view.
        // Both passes charge original bytes before masking can shrink them.
        ensure!(
            crate::core::roles::active_role().is_tool_allowed("ctx_read")
                && runtime::active().is_none_or(|policy| policy.tool_allowed("ctx_read")),
            "receiving file access denied"
        );
        for _ in 0..2 {
            let mut remaining = MAX_TOTAL_BYTES;
            for (reference, expected) in dependencies {
                let binding = mapped
                    .get(reference.as_str())
                    .ok_or_else(|| anyhow::anyhow!("receiving source mapping missing"))?;
                let path = root.join(&binding.path);
                let path = path
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("receiving source path unavailable"))?;
                let read = crate::tools::ctx_read::read_file_for_tool_rooted_with_path(
                    path,
                    root_text,
                    "ctx_read",
                    &mut remaining,
                )
                .map_err(|_| anyhow::anyhow!("receiving source unavailable"))?;
                ensure!(
                    super::digest(read.content.as_bytes()).map_err(anyhow::Error::msg)?
                        == *expected,
                    "receiving source changed or requires transformation"
                );
            }
        }
        Ok(())
    })
    .map_err(|_| anyhow::anyhow!("receiving source policy changed or unavailable"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receiving_provider_rechecks_actual_access_revision_and_masking() {
        use crate::core::providers::{
            config_provider::{ConfigProvider, schema::ProviderConfig},
            hardened_http::redirect_tests::Server,
            provenance::BoundResult,
            provider_trait::ProviderParams,
        };
        let _data = crate::core::data_dir::isolated_data_dir();
        let _policy = runtime::lock_test_override();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let configs = root.join(".lean-ctx/providers");
        std::fs::create_dir_all(&configs).unwrap();
        let body = r#"[{"id":"1","title":"K-731842","body":"Investigate login"}]"#;
        let server = Server::new(200, None, body);
        let config_text = format!(
            "id='receiving-provider-fixture'\nname='Fixture'\nbase_url='{}'\n[auth]\ntype='none'\n[resources.issues]\nmethod='GET'\npath='/items'\n[resources.issues.response.mapping]\nid='id'\ntitle='title'\nbody='body'\n",
            server.url
        );
        let config_path = configs.join("fixture.toml");
        std::fs::write(&config_path, &config_text).unwrap();
        let config: ProviderConfig = toml::from_str(&config_text).unwrap();
        let provider = ConfigProvider::from_config(config).unwrap();
        let acquired =
            BoundResult::execute(&provider, "issues", &ProviderParams::default()).unwrap();
        let chunks = acquired.chunks(&acquired.result);
        assert_eq!(chunks.len(), 1);
        let origin = chunks[0].origin.clone().unwrap();
        let dependencies = BTreeMap::from([(
            "object:issue-1".into(),
            super::super::digest(chunks[0].content.as_bytes()).unwrap(),
        )]);
        let bindings = vec![ReceivingProviderBinding {
            source_ref: "object:issue-1".into(),
            origin,
        }];
        let before = server.count();
        assert!(check_providers(&root, &dependencies, &bindings).is_ok());
        assert_eq!(
            server.count() - before,
            2,
            "both checks must reacquire the original source"
        );
        server.respond(403, "denied");
        assert!(check_providers(&root, &dependencies, &bindings).is_err());
        server.respond(
            200,
            r#"[{"id":"1","title":"changed","body":"Investigate login"}]"#,
        );
        assert!(check_providers(&root, &dependencies, &bindings).is_err());
        server.respond(200, body);
        let policy_path = root.join(".lean-ctx/policy.toml");
        std::fs::write(&policy_path, "name='receiving'\nversion='1.0.0'\ndescription='test'\n[redaction]\ncustomer='K-731842'\n").unwrap();
        assert!(check_providers(&root, &dependencies, &bindings).is_err());
        std::fs::write(
            &policy_path,
            "name='receiving'\nversion='1.0.0'\ndescription='test'\n[redaction]\nall='(?s).+'\n",
        )
        .unwrap();
        let masked = runtime::with_project_source_view(root.to_str().unwrap(), || {
            crate::core::providers::provenance::with_reuse_deadline(|| {
                bindings[0].origin.current_chunks(root.to_str().unwrap())
            })
        })
        .unwrap()
        .unwrap();
        assert!(
            masked.original_bytes > masked.chunks[0].content.len(),
            "budget retains original item bytes even when policy shrinks all content"
        );
        std::fs::remove_file(policy_path).unwrap();
        assert!(check_providers(&root, &dependencies, &bindings).is_ok());
        std::fs::remove_file(config_path).unwrap();
        assert!(check_providers(&root, &dependencies, &bindings).is_err());
    }

    #[test]
    fn actual_receiving_file_must_match_and_remain_authorized() {
        let _policy = runtime::lock_test_override();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("source.txt"), "K-482193").unwrap();
        let dependencies = BTreeMap::from([(
            "source:fixture".into(),
            super::super::digest(b"K-482193").unwrap(),
        )]);
        let bindings = vec![ReceivingFileBinding {
            source_ref: "source:fixture".into(),
            path: "source.txt".into(),
        }];
        assert!(check_files(&root, &dependencies, &bindings).is_ok());
        std::fs::create_dir(root.join(".lean-ctx")).unwrap();
        let policy = root.join(".lean-ctx/policy.toml");
        let base = "name = 'receiving'\nversion = '1.0.0'\ndescription = 'test'\n";
        for rules in [
            "[context]\ndeny_tools = ['ctx_read']\n",
            "[redaction]\ncustomer = 'K-[0-9]{6}'\n",
            "[redaction]\ninvalid = '['\n",
        ] {
            std::fs::write(&policy, format!("{base}{rules}")).unwrap();
            assert!(check_files(&root, &dependencies, &bindings).is_err());
        }
        std::fs::remove_file(policy).unwrap();
        std::fs::write(root.join("source.txt"), "changed").unwrap();
        assert!(check_files(&root, &dependencies, &bindings).is_err());
        std::fs::remove_file(root.join("source.txt")).unwrap();
        assert!(check_files(&root, &dependencies, &bindings).is_err());
    }

    #[test]
    fn receiving_mappings_cannot_omit_duplicate_or_escape_sources() {
        let _policy = runtime::lock_test_override();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let dependencies = BTreeMap::from([(
            "source:fixture".into(),
            super::super::digest(b"safe").unwrap(),
        )]);
        assert!(check_files(&root, &dependencies, &[]).is_err());
        assert!(check_files(&root, &BTreeMap::new(), &[]).is_err());
        for path in ["../outside", "/etc/passwd", "", "./source.txt"] {
            let bindings = vec![ReceivingFileBinding {
                source_ref: "source:fixture".into(),
                path: path.into(),
            }];
            assert!(check_files(&root, &dependencies, &bindings).is_err());
        }
        let bindings = vec![ReceivingFileBinding {
            source_ref: "source:unknown".into(),
            path: "source.txt".into(),
        }];
        assert!(check_files(&root, &dependencies, &bindings).is_err());
        #[cfg(unix)]
        {
            let outside = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(outside.path(), "safe").unwrap();
            std::os::unix::fs::symlink(outside.path(), root.join("escape")).unwrap();
            let bindings = vec![ReceivingFileBinding {
                source_ref: "source:fixture".into(),
                path: "escape".into(),
            }];
            assert!(check_files(&root, &dependencies, &bindings).is_err());
        }
    }
}
