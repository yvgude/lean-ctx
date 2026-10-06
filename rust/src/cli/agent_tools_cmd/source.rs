// SPDX-License-Identifier: Apache-2.0
//! Explicit operator source selection for the shared SDK tool session.
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::core::providers::selected_gitlab::Selection;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GitLabSource {
    host: String,
    project: u64,
    namespace: String,
    glab: PathBuf,
    #[serde(default)]
    config_dir: Option<PathBuf>,
}

impl GitLabSource {
    fn selection(&self) -> Selection {
        Selection {
            host: self.host.clone(),
            project: self.project,
            namespace: self.namespace.clone(),
        }
    }

    pub(super) fn validate(&self) -> Result<(), &'static str> {
        self.selection().validate().map_err(|_| "invalid_source")?;
        if self.project > 9_007_199_254_740_991
            || !safe_absolute_path(&self.glab)
            || self
                .config_dir
                .as_deref()
                .is_some_and(|path| !safe_absolute_path(path))
        {
            return Err("invalid_source");
        }
        Ok(())
    }

    pub(super) fn initialize(&self, root: &Path) -> Result<String, &'static str> {
        self.validate()?;
        require_policy(root).map_err(|_| "source_policy_required")?;
        let authority = crate::core::policy::runtime::protected_policy_digest(root)
            .map_err(|_| "source_policy_required")?;
        crate::cli::codex_protected_cmd::initialize_agent_gitlab(
            self.selection(),
            &self.glab,
            self.config_dir.as_deref(),
            root,
        )
        .map_err(|_| "source_unavailable")?;
        verify_authority(root, &authority).map_err(|_| "source_policy_changed")?;
        Ok(authority)
    }

    pub(super) fn authorize_query(&self, args: &Map<String, Value>) -> Result<(), super::ErrorV1> {
        const KEYS: &[&str] = &[
            "action", "provider", "resource", "mode", "project", "limit", "state", "query",
        ];
        if args.keys().any(|key| !KEYS.contains(&key.as_str()))
            || args.get("action").and_then(Value::as_str) != Some("query")
            || args.get("provider").and_then(Value::as_str) != Some("gitlab")
            || args.get("mode").and_then(Value::as_str) != Some("snapshot")
            || !matches!(
                args.get("resource").and_then(Value::as_str),
                Some("issues" | "merge_requests" | "pipelines")
            )
            || !args
                .get("limit")
                .and_then(Value::as_u64)
                .is_some_and(|limit| (1..=100).contains(&limit))
            || ["state", "query"].iter().any(|key| {
                args.get(*key).is_some_and(|value| {
                    value
                        .as_str()
                        .is_none_or(|text| text.len() > 4096 || text.chars().any(char::is_control))
                })
            })
        {
            return Err(super::policy_denied());
        }
        let project = args
            .get("project")
            .and_then(Value::as_str)
            .ok_or_else(super::policy_denied)?;
        self.selection()
            .check_project(Some(project))
            .map_err(|_| super::policy_denied())
    }
}

fn safe_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .to_str()
            .is_some_and(|text| text.len() <= 4096 && !text.chars().any(char::is_control))
}

pub(super) fn require_policy(root: &Path) -> Result<(), super::ErrorV1> {
    match crate::core::policy::runtime::for_project(root) {
        Ok(Some(policy)) if policy.tool_allowed("ctx_provider") => Ok(()),
        _ => Err(super::policy_denied()),
    }
}

pub(super) fn verify_authority(root: &Path, expected: &str) -> Result<(), super::ErrorV1> {
    let current = crate::core::policy::runtime::protected_policy_digest(root)
        .map_err(|_| super::policy_denied())?;
    if current != expected {
        return Err(super::policy_denied());
    }
    require_policy(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn glab_path() -> &'static str {
        if cfg!(windows) {
            r"C:\Program Files\GitLab CLI\glab.exe"
        } else {
            "/usr/local/bin/glab"
        }
    }

    fn source() -> GitLabSource {
        serde_json::from_value(json!({"host":"gitlab.example.test", "project":5,
            "namespace":"group/project", "glab":glab_path()}))
        .unwrap()
    }

    #[test]
    fn selection_rejects_credential_fields_and_noncanonical_authority() {
        assert!(source().validate().is_ok());
        let mut value = json!({"host":"gitlab.example.test", "project":5,
            "namespace":"group/project", "glab":glab_path(), "token":"not-accepted"});
        assert!(serde_json::from_value::<GitLabSource>(value.clone()).is_err());
        value.as_object_mut().unwrap().remove("token");
        for (field, replacement) in [
            ("host", json!("https://gitlab.example.test")),
            ("project", json!(9_007_199_254_740_992_u64)),
            ("glab", json!("./glab")),
            ("config_dir", json!("relative")),
        ] {
            let mut invalid = value.clone();
            invalid[field] = replacement;
            assert!(
                serde_json::from_value::<GitLabSource>(invalid)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
    }

    #[test]
    fn query_is_selected_read_only_and_bounded() {
        let allowed = json!({"action":"query", "provider":"gitlab", "resource":"merge_requests",
            "mode":"snapshot", "project":"5", "limit":1});
        assert!(
            source()
                .authorize_query(allowed.as_object().unwrap())
                .is_ok()
        );
        for (field, replacement) in [
            ("action", json!("refresh")),
            ("provider", json!("github")),
            ("project", json!("other/project")),
            ("mode", json!("compact")),
            ("resource", json!("files")),
            ("limit", json!(101)),
            ("limit", json!(true)),
            ("query", json!(["unexpected"])),
            ("token", json!("unexpected")),
        ] {
            let mut invalid = allowed.clone();
            invalid[field] = replacement;
            assert!(
                source()
                    .authorize_query(invalid.as_object().unwrap())
                    .is_err()
            );
        }
        let mut missing = allowed;
        missing.as_object_mut().unwrap().remove("project");
        assert!(
            source()
                .authorize_query(missing.as_object().unwrap())
                .is_err()
        );
    }
}
