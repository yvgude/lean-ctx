use super::receipt::{record_provider_receipt, visible_output};
use super::timeout::run_with_timeout_cancellable;
use super::traits::{
    AgentConnector, AgentInfo, TaskRequest, TaskResult, TokenUsage, apply_profile_environment,
};
use std::process::Command;
use std::time::Instant;

pub(crate) struct ClaudeConnector {
    info: AgentInfo,
}
impl ClaudeConnector {
    pub(crate) fn new(info: AgentInfo) -> Self {
        Self { info }
    }
}

impl AgentConnector for ClaudeConnector {
    fn supports_model_selection(&self) -> bool {
        true
    }
    fn supports_turn_limit(&self) -> bool {
        true
    }
    fn info(&self) -> AgentInfo {
        self.info.clone()
    }
    fn health_check_with_timeout(&self, timeout_ms: u64) -> anyhow::Result<bool> {
        if !self.info.available {
            return Ok(false);
        }
        Ok(
            super::detection::probe_version_with_timeout(&self.info.path, timeout_ms)
                .is_some_and(|version| self.info.version.as_ref() == Some(&version)),
        )
    }
    fn execute(&self, request: &TaskRequest) -> anyhow::Result<TaskResult> {
        super::traits::validate_request(request, true)?;
        let start = Instant::now();
        let mut cmd = Command::new(&self.info.path);
        cmd.arg("-p")
            .arg("--output-format")
            .arg("json")
            .current_dir(&request.working_dir);
        if let Some(turns) = request.max_turns {
            cmd.arg("--max-turns").arg(turns.to_string());
        }
        if let Some(model) = &request.model {
            cmd.arg("--model").arg(model);
        }
        cmd.arg("--").arg(&request.prompt);
        apply_profile_environment(&mut cmd, request);
        let timed_output =
            run_with_timeout_cancellable(&mut cmd, request.timeout_ms, Some(&request.id))?;
        let output = timed_output.output;
        let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if timed_output.timed_out {
            stderr.push_str(&format!("task timed out after {}ms", request.timeout_ms));
        }
        if timed_output.cancelled {
            stderr.push_str("task cancelled by work graph");
        }
        let raw_stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let duration_ms = start.elapsed().as_millis() as u64;
        let receipt = record_provider_receipt(
            "claude-code",
            "anthropic",
            request,
            &raw_stdout,
            duration_ms,
        );
        let tokens_used = receipt
            .as_ref()
            .map(|link| link.tokens_used)
            .or_else(|| parse_claude_usage(&output.stdout));
        let provider_cost_micros = receipt.as_ref().map(|link| link.provider_cost_micros);
        let execution_receipt_ref = receipt.map(|link| link.reference);
        let stdout = visible_output(&raw_stdout);
        Ok(TaskResult {
            task_id: request.id.clone(),
            agent: "claude-code".into(),
            model: request.model.clone().unwrap_or_default(),
            success: !timed_output.timed_out && !timed_output.cancelled && output.status.success(),
            exit_code: if timed_output.timed_out || timed_output.cancelled {
                -1
            } else {
                output.status.code().unwrap_or(-1)
            },
            stdout,
            stderr,
            duration_ms,
            tokens_used,
            provider_cost_micros,
            execution_receipt_ref,
            termination: Some(super::traits::TaskTermination::from_process(
                output.status,
                timed_output.timed_out,
            )),
        })
    }
    fn name(&self) -> &'static str {
        "claude-code"
    }
}

fn parse_claude_usage(stdout: &[u8]) -> Option<TokenUsage> {
    let text = String::from_utf8_lossy(stdout);
    let val: serde_json::Value = serde_json::from_str(&text).ok()?;
    let usage = val.get("usage")?;
    Some(TokenUsage {
        input_tokens: usage["input_tokens"].as_u64()?,
        output_tokens: usage["output_tokens"].as_u64()?,
        cache_read_tokens: usage["cache_read_input_tokens"].as_u64().unwrap_or(0),
        cache_write_tokens: usage["cache_creation_input_tokens"].as_u64().unwrap_or(0),
    })
}
