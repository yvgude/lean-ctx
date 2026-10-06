// SPDX-License-Identifier: Apache-2.0
//! Immutable source authority for an explicitly selected protected MCP session.
//! This is acquisition authorization, not authorization of previously stored context.
use super::config::GitLabConfig;
#[cfg(any(unix, windows))]
use std::sync::Arc;
use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Selection {
    pub host: String,
    pub project: u64,
    pub namespace: String,
}

impl Selection {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let url = reqwest::Url::parse(&format!("https://{}", self.host))
            .map_err(|_| "invalid selected GitLab host")?;
        if self.host.is_empty()
            || self.host.len() > 253
            || self.host.chars().any(char::is_control)
            || url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || url.origin().ascii_serialization() != format!("https://{}", self.host)
        {
            return Err("selected GitLab host must be a canonical HTTPS authority".into());
        }
        if self.project == 0
            || self.namespace.len() > 1024
            || !self.namespace.contains('/')
            || self.namespace.split('/').any(|segment| {
                segment.is_empty()
                    || matches!(segment, "." | "..")
                    || !segment
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
        {
            return Err("selected GitLab needs a positive project ID and a valid namespace".into());
        }
        Ok(())
    }

    pub(crate) fn check_project(&self, project: Option<&str>) -> Result<(), String> {
        if project.is_some_and(|p| p != self.project.to_string() && p != self.namespace) {
            return Err("request does not match the selected GitLab project".into());
        }
        Ok(())
    }

    fn check_config(&self, selected: &GitLabConfig, config: &GitLabConfig) -> Result<(), String> {
        if config.host != self.host
            || config.project_path.as_deref() != Some(self.project.to_string().as_str())
            || config.token != selected.token
        {
            return Err("GitLab configuration cannot replace the protected source".into());
        }
        Ok(())
    }

    fn check_identity(&self, body: &str) -> Result<(), String> {
        let value: serde_json::Value =
            serde_json::from_str(body).map_err(|_| "invalid GitLab project identity response")?;
        if value["id"].as_u64() != Some(self.project)
            || value["path_with_namespace"].as_str() != Some(self.namespace.as_str())
        {
            return Err("GitLab project identity no longer matches the selected source".into());
        }
        Ok(())
    }
}

struct Selected {
    source: Selection,
    config: GitLabConfig,
}

static SELECTED: OnceLock<Selected> = OnceLock::new();

#[cfg(any(unix, windows))]
pub(crate) fn install(source: Selection, token: String) -> Result<(), String> {
    source.validate()?;
    if token.is_empty()
        || token.len() > 16 * 1024
        || !token.bytes().all(|b| (33..=126).contains(&b))
    {
        return Err("invalid selected GitLab credential".into());
    }
    let config = GitLabConfig {
        host: source.host.clone(),
        token,
        project_path: Some(source.project.to_string()),
    };
    SELECTED
        .set(Selected {
            source,
            config: config.clone(),
        })
        .map_err(|_| "selected GitLab source was already installed")?;
    super::registry::global_registry()
        .pin(Arc::new(super::gitlab::GitLabProvider::with_config(config)))
}

pub(crate) fn config() -> Option<GitLabConfig> {
    SELECTED.get().map(|selected| selected.config.clone())
}

pub(crate) fn reuse_binding(config: &GitLabConfig) -> Option<String> {
    let selected = SELECTED.get()?;
    selected
        .source
        .check_config(&selected.config, config)
        .ok()?;
    super::provenance::digest(&serde_json::json!({
        "version": 1, "source": selected.source, "credential": config.token,
    }))
}

pub(crate) fn check_project(project: Option<&str>) -> Result<(), String> {
    SELECTED
        .get()
        .map_or(Ok(()), |selected| selected.source.check_project(project))
}

/// Re-check identity using the selected credential on every resource acquisition.
/// The subsequent resource GET still enforces that resource's current permission.
pub(crate) fn authorize(config: &GitLabConfig) -> Result<(), String> {
    let Some(selected) = SELECTED.get() else {
        return Ok(());
    };
    selected.source.check_config(&selected.config, config)?;
    let body = super::hardened_http::provider_get_with_headers(
        "gitlab",
        &config.api_url(&format!("/projects/{}", selected.source.project)),
        &[("PRIVATE-TOKEN", config.token.as_str())],
    )
    .into_body()
    .map_err(|_| "selected GitLab identity could not be authorized")?;
    selected.source.check_identity(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn selection() -> Selection {
        Selection {
            host: "gitlab.example.test".into(),
            project: 5,
            namespace: "group/project".into(),
        }
    }
    #[test]
    fn selection_rejects_host_and_namespace_reinterpretation() {
        assert!(selection().validate().is_ok());
        for host in [
            "https://gitlab.example.test",
            "user@gitlab.example.test",
            "gitlab.example.test/x",
            "gitlab.example.test?x",
            "gitlab.example.test#x",
            "GitLab.example.test",
            "gitlab.example.test/",
        ] {
            assert!(
                Selection {
                    host: host.into(),
                    ..selection()
                }
                .validate()
                .is_err(),
                "{host}"
            );
        }
        for namespace in ["", "project", "../project", "g//p", "g/p?x", "g/p\n"] {
            assert!(
                Selection {
                    namespace: namespace.into(),
                    ..selection()
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            Selection {
                project: 0,
                ..selection()
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn project_parameters_and_fresh_identity_must_match() {
        let source = selection();
        for project in [None, Some("5"), Some("group/project")] {
            assert!(source.check_project(project).is_ok());
        }
        assert!(source.check_project(Some("other/project")).is_err());
        assert!(
            source
                .check_identity(r#"{"id":5,"path_with_namespace":"group/project"}"#)
                .is_ok()
        );
        for body in [
            r#"{"id":6,"path_with_namespace":"group/project"}"#,
            r#"{"id":5,"path_with_namespace":"moved/project"}"#,
            "{}",
            "not json",
        ] {
            assert!(source.check_identity(body).is_err());
        }
    }
}
