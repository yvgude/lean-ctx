use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct AgentInfo {
    pub name: String,
    pub version: Option<String>,
    pub path: PathBuf,
    pub capabilities: Vec<String>,
    pub available: bool,
}

/// Shared task input for CLI connectors and the OCLA agent adapter.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskRequest {
    pub id: String,
    pub prompt: String,
    pub working_dir: PathBuf,
    pub timeout_ms: u64,
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    /// Profile selected for this isolated agent invocation.
    ///
    /// Connectors pass this through as `LEAN_CTX_PROFILE` so the child agent
    /// and every LeanCTX tool it launches resolve the same profile.
    #[serde(default)]
    pub profile_name: Option<String>,
    pub profile_hash: Option<String>,
    /// Explicit host-provided child identity; never inherited from the parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_profile: Option<Box<ChildDeliveryProfileV1>>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildDeliveryProfileV1 {
    pub task_id: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub profile: crate::core::a2a::task::delivery_authority::DeliverySigningProfileV1,
}

impl ChildDeliveryProfileV1 {
    fn environment_for(&self, request: &TaskRequest) -> Option<String> {
        if self.task_id != request.id
            || self.expires_at <= chrono::Utc::now()
            || self.profile.schema_version != 1
            || !self.profile.project_root.is_absolute()
        {
            return None;
        }
        let root = self.profile.project_root.canonicalize().ok()?;
        if self.profile.delegation.as_ref().is_some_and(|certificate| {
            certificate.execution.task_id != request.id
                || self.expires_at > certificate.expires_at
                || certificate.schema_version != 1
                || certificate.issued_at > chrono::Utc::now()
                || certificate.project_root != root
                || certificate.privacy != self.profile.privacy
                || certificate.child_agent != self.profile.agent_id
                || certificate.child_key_id != self.profile.key_id
                || certificate.recipient != self.profile.recipient
                || certificate.tenant_id != self.profile.tenant_id
                || certificate.project_id != self.profile.project_id
                || certificate.read_grant_id != self.profile.read_grant_id
                || certificate
                    .write_grant_id
                    .as_ref()
                    .is_some_and(|grant| grant != &self.profile.write_grant_id)
                || !crate::core::agent_identity::get_stored_public_key(&self.profile.agent_id)
                    .is_ok_and(|key| {
                        crate::core::agent_identity::hex_encode(key.as_bytes())
                            == certificate.child_public_key
                    })
        }) {
            return None;
        }
        if !root.is_dir() || root != request.working_dir.canonicalize().ok()? {
            return None;
        }
        let raw = serde_json::to_string(&self.profile).ok()?;
        (raw.len() <= 16_384).then_some(raw)
    }
}

pub(crate) fn apply_profile_environment(command: &mut Command, request: &TaskRequest) {
    // CLI children are distinct agents. Only an explicit, task/project-bound
    // host profile may replace the fail-closed default.
    // An invalid explicit profile disables delivery without legacy fallback;
    // removing the variable would silently re-enable unscoped delivery.
    command.env("LEAN_CTX_DELIVERY_PROFILE", "{}");
    // Child delivery permission never includes the parent's issuer configuration.
    command.env_remove("LEAN_CTX_DELIVERY_ISSUER_PROFILE");
    if let Some(profile) = &request.delivery_profile {
        if let Some(raw) = profile.environment_for(request) {
            command.env("LEAN_CTX_DELIVERY_PROFILE", raw);
        } else {
            tracing::warn!(
                event = "invalid_child_delivery_profile",
                "Child context sharing disabled: explicit profile failed launch validation"
            );
        }
    }
    if let Some(profile_name) = request
        .profile_name
        .as_deref()
        .filter(|profile_name| !profile_name.trim().is_empty())
    {
        command.env("LEAN_CTX_PROFILE", profile_name);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaskResult {
    pub task_id: String,
    pub agent: String,
    pub model: String,
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub tokens_used: Option<TokenUsage>,
    /// Explicit provider-reported charge, never a table-derived estimate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_cost_micros: Option<u64>,
    /// Canonical receipt emitted from explicit provider usage and cost evidence.
    /// It remains absent when CLI output does not carry both observations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_receipt_ref: Option<String>,
    /// Process outcome, independent of stderr text and provider task quality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination: Option<TaskTermination>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskTermination {
    Exited,
    Signalled,
    TimedOut,
}

impl TaskTermination {
    pub(crate) fn from_process(status: std::process::ExitStatus, timed_out: bool) -> Self {
        if timed_out {
            Self::TimedOut
        } else if status.code().is_none() {
            Self::Signalled
        } else {
            Self::Exited
        }
    }
}

/// Reject controls the chosen CLI cannot enforce before any process is started.
pub(crate) fn validate_request(
    request: &TaskRequest,
    supports_turn_limit: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(!request.id.trim().is_empty(), "task ID is required");
    anyhow::ensure!(!request.prompt.trim().is_empty(), "task prompt is required");
    anyhow::ensure!(request.timeout_ms > 0, "task timeout must be positive");
    anyhow::ensure!(
        request
            .model
            .as_ref()
            .is_none_or(|model| !model.trim().is_empty()),
        "model must not be empty"
    );
    anyhow::ensure!(request.max_turns != Some(0), "turn limit must be positive");
    anyhow::ensure!(
        supports_turn_limit || request.max_turns.is_none(),
        "connector does not support a turn limit"
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub(crate) struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

pub(crate) trait AgentConnector: Send + Sync {
    fn info(&self) -> AgentInfo;
    fn health_check(&self) -> anyhow::Result<bool> {
        self.health_check_with_timeout(2_000)
    }
    fn health_check_with_timeout(&self, timeout_ms: u64) -> anyhow::Result<bool>;
    fn execute(&self, request: &TaskRequest) -> anyhow::Result<TaskResult>;
    fn name(&self) -> &'static str;
    fn supports_model_selection(&self) -> bool {
        false
    }
    fn supports_turn_limit(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_request_roundtrip() {
        let req = TaskRequest {
            id: "t1".into(),
            prompt: "Explore codebase".into(),
            working_dir: PathBuf::from("/tmp/test"),
            timeout_ms: 60_000,
            model: Some("gpt-4".into()),
            max_turns: Some(10),
            profile_name: Some("benchmark-candidate".into()),
            profile_hash: Some("abc".into()),
            delivery_profile: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let restored: TaskRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.id, "t1");
        assert_eq!(
            restored.profile_name.as_deref(),
            Some("benchmark-candidate")
        );
    }

    #[test]
    fn profile_environment_is_set_only_for_named_profiles() {
        let mut command = Command::new("echo");
        let request = TaskRequest {
            id: "t1".into(),
            prompt: "Explore codebase".into(),
            working_dir: PathBuf::from("/tmp/test"),
            timeout_ms: 60_000,
            model: None,
            max_turns: None,
            profile_name: Some("benchmark-candidate".into()),
            profile_hash: None,
            delivery_profile: None,
        };

        apply_profile_environment(&mut command, &request);
        let profile = command
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("LEAN_CTX_PROFILE"))
            .and_then(|(_, value)| value);
        assert_eq!(profile, Some(std::ffi::OsStr::new("benchmark-candidate")));
        let delivery_profile = command
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("LEAN_CTX_DELIVERY_PROFILE"))
            .and_then(|(_, value)| value);
        assert_eq!(delivery_profile, Some(std::ffi::OsStr::new("{}")));
    }

    #[test]
    fn child_cannot_inherit_parent_delivery_authority() {
        let mut command = Command::new("echo");
        command.env("LEAN_CTX_DELIVERY_PROFILE", "parent-authority");
        let request = TaskRequest {
            id: "child-task".into(),
            prompt: "Inspect".into(),
            working_dir: PathBuf::from("/tmp/test"),
            timeout_ms: 1000,
            model: None,
            max_turns: None,
            profile_name: None,
            profile_hash: None,
            delivery_profile: None,
        };
        apply_profile_environment(&mut command, &request);
        let (_, value) = command
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("LEAN_CTX_DELIVERY_PROFILE"))
            .unwrap();
        let value = value.unwrap().to_str().unwrap();
        assert_eq!(value, "{}");
        assert!(
            serde_json::from_str::<
                crate::core::a2a::task::delivery_authority::DeliverySigningProfileV1,
            >(value)
            .is_err()
        );
    }

    #[test]
    fn task_result_roundtrip() {
        let result = TaskResult {
            task_id: "t1".into(),
            agent: "codex".into(),
            model: "gpt-4".into(),
            success: true,
            exit_code: 0,
            stdout: "done".into(),
            stderr: String::new(),
            duration_ms: 5000,
            tokens_used: Some(TokenUsage {
                input_tokens: 1000,
                output_tokens: 200,
                cache_read_tokens: 500,
                cache_write_tokens: 100,
            }),
            provider_cost_micros: Some(1_500),
            execution_receipt_ref: Some("receipt:test".into()),
            termination: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        let restored: TaskResult = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.agent, "codex");
        assert_eq!(restored.provider_cost_micros, Some(1_500));
        assert!(restored.termination.is_none());
        assert!(!json.contains("termination"));
    }

    #[test]
    fn child_delivery_profile_is_bound_to_task_project_and_expiry() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let binding = ChildDeliveryProfileV1 {
            task_id: "child-task".into(),
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(2),
            profile: crate::core::a2a::task::delivery_authority::DeliverySigningProfileV1 {
                schema_version: 1,
                agent_id: "child-agent".into(),
                delegation: None,
                recipient: "host".into(),
                tenant_id: "tenant".into(),
                project_id: "project".into(),
                project_root: root.path().to_path_buf(),
                key_id: "child-key".into(),
                read_grant_id: "child-read".into(),
                write_grant_id: "child-write".into(),
                privacy: lean_ctx_ocla::delivery_scope::DeliveryPrivacyV1::Project,
            },
        };
        let mut request = TaskRequest {
            id: "child-task".into(),
            prompt: "Inspect".into(),
            working_dir: root.path().to_path_buf(),
            timeout_ms: 1000,
            model: None,
            max_turns: None,
            profile_name: None,
            profile_hash: None,
            delivery_profile: Some(Box::new(binding.clone())),
        };
        let profile_value = |request: &TaskRequest| {
            let mut command = Command::new("echo");
            command.env("LEAN_CTX_DELIVERY_PROFILE", "parent-authority");
            command.env("LEAN_CTX_DELIVERY_ISSUER_PROFILE", "parent-issuer");
            apply_profile_environment(&mut command, request);
            assert!(command.get_envs().any(|(key, value)| key
                == std::ffi::OsStr::new("LEAN_CTX_DELIVERY_ISSUER_PROFILE")
                && value.is_none()));
            command
                .get_envs()
                .find(|(key, _)| *key == std::ffi::OsStr::new("LEAN_CTX_DELIVERY_PROFILE"))
                .unwrap()
                .1
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned()
        };
        let raw = profile_value(&request);
        let profile: crate::core::a2a::task::delivery_authority::DeliverySigningProfileV1 =
            serde_json::from_str(&raw).unwrap();
        assert_eq!(profile.agent_id, "child-agent");
        assert_eq!(profile, binding.profile);
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(
            serde_json::from_value::<TaskRequest>(encoded.clone()).unwrap(),
            request
        );
        let mut legacy = encoded;
        legacy.as_object_mut().unwrap().remove("delivery_profile");
        assert!(
            serde_json::from_value::<TaskRequest>(legacy)
                .unwrap()
                .delivery_profile
                .is_none()
        );

        let (mut certificate, _, host_key) =
            crate::core::a2a::task::delivery_delegation::tests::fixture();
        certificate.execution.task_id = request.id.clone();
        certificate.child_agent = binding.profile.agent_id.clone();
        certificate.child_key_id = binding.profile.key_id.clone();
        certificate.recipient = binding.profile.recipient.clone();
        certificate.tenant_id = binding.profile.tenant_id.clone();
        certificate.project_id = binding.profile.project_id.clone();
        certificate.project_root = root.path().canonicalize().unwrap();
        certificate.privacy = binding.profile.privacy;
        certificate.read_grant_id = binding.profile.read_grant_id.clone();
        let child_key =
            crate::core::agent_identity::get_or_create_keypair(&certificate.child_agent).unwrap();
        certificate.child_public_key =
            crate::core::agent_identity::hex_encode(child_key.verifying_key().as_bytes());
        certificate.sign(&host_key);
        request
            .delivery_profile
            .as_mut()
            .unwrap()
            .profile
            .delegation = Some(Box::new(certificate.clone()));
        assert_ne!(profile_value(&request), "{}");
        let delegated_profile: crate::core::a2a::task::delivery_authority::DeliverySigningProfileV1 =
            serde_json::from_str(&profile_value(&request)).unwrap();
        assert_eq!(delegated_profile.delegation.as_deref(), Some(&certificate));
        for field in [
            "child_agent",
            "child_key_id",
            "recipient",
            "tenant_id",
            "project_id",
            "read_grant_id",
            "child_public_key",
        ] {
            let mut value = serde_json::to_value(&certificate).unwrap();
            value[field] = serde_json::json!("mismatch");
            request
                .delivery_profile
                .as_mut()
                .unwrap()
                .profile
                .delegation = Some(Box::new(serde_json::from_value(value).unwrap()));
            assert_eq!(profile_value(&request), "{}", "{field}");
        }
        certificate.execution.task_id = "other-attempt".into();
        certificate.sign(&host_key);
        request
            .delivery_profile
            .as_mut()
            .unwrap()
            .profile
            .delegation = Some(Box::new(certificate.clone()));
        assert_eq!(profile_value(&request), "{}");
        certificate.execution.task_id = request.id.clone();
        certificate.expires_at = binding.expires_at - chrono::Duration::seconds(1);
        certificate.sign(&host_key);
        request
            .delivery_profile
            .as_mut()
            .unwrap()
            .profile
            .delegation = Some(Box::new(certificate));
        assert_eq!(profile_value(&request), "{}");
        request.delivery_profile = Some(Box::new(binding.clone()));

        request.delivery_profile.as_mut().unwrap().profile.recipient = "x".repeat(16_384);
        assert_eq!(profile_value(&request), "{}");
        request.delivery_profile = Some(Box::new(binding.clone()));
        request.id = "different-task".into();
        assert_eq!(profile_value(&request), "{}");
        request.id = binding.task_id.clone();
        request.working_dir = other.path().to_path_buf();
        assert_eq!(profile_value(&request), "{}");
        request.working_dir = root.path().to_path_buf();
        request.delivery_profile.as_mut().unwrap().expires_at =
            chrono::Utc::now() - chrono::Duration::seconds(1);
        assert_eq!(profile_value(&request), "{}");
        request.delivery_profile = Some(Box::new(binding));
        request
            .delivery_profile
            .as_mut()
            .unwrap()
            .profile
            .schema_version = 2;
        assert_eq!(profile_value(&request), "{}");
    }
}
