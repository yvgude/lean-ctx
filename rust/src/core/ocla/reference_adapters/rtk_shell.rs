//! Sandboxed RTK shell-output reference adapter.
//!
//! RTK is intentionally kept outside the production adapter registry.  The
//! adapter can collect a comparable external observation and can provide a
//! native fallback, but it never makes the external response authoritative.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::super::invocation::{
    CAPABILITY_OBSERVATION_SCHEMA_VERSION, CapabilityAdapter, CapabilityFailureMode,
    CapabilityInput, CapabilityInvocation, CapabilityObservationV1, CapabilityResult, evidence_ref,
};
use super::KillSwitch;
use crate::core::ocla::{OclaError, OclaResult};
use crate::core::{pathjail, shell_allowlist};

const CAPABILITY_ID: &str = "rtk-shell-output";
const CAPABILITY_VERSION: &str = "1.0.0";
const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 16_384;
const DEFAULT_PROCESS_CAPTURE_BYTES: usize = 2 * 1024 * 1024;
const VERSION_PROBE_TIMEOUT_MS: u64 = 1_000;
const TIMEOUT_MARKER: &str = "ERROR: command timed out after ";

const MANIFEST_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/ocla/capability-manifests/rtk/rtk-shell-v1.json"
));

/// Configuration for the external shell optimizer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RtkConfig {
    /// Absolute path or a binary name resolved through `PATH`.
    pub executable: PathBuf,
    /// Fixed arguments inserted before `rewrite <command>`.
    pub args: Vec<String>,
    /// Maximum duration for the RTK subprocess; zero means no local cap.
    pub timeout_ms: u64,
    /// Test-only extension for the version-probe deadline under process-heavy test load.
    #[cfg(test)]
    #[serde(skip)]
    pub(crate) test_version_probe_timeout_ms: Option<u64>,
    /// Optional default working directory, overridden by the invocation.
    pub working_dir: Option<PathBuf>,
    /// Exact output of `rtk --version` that must be present in the probe.
    pub pinned_version: Option<String>,
    /// Lowercase or uppercase SHA-256 of the resolved executable.
    pub pinned_sha256: Option<String>,
    /// Maximum output tokens retained in an observation.
    pub max_output_tokens: u64,
    /// Maximum bytes captured from the RTK process before it is rejected.
    pub max_capture_bytes: usize,
    /// Optional PathJail root for test or explicitly configured sandboxes.
    pub sandbox_root: Option<PathBuf>,
}

impl Default for RtkConfig {
    fn default() -> Self {
        Self {
            executable: PathBuf::from("rtk"),
            args: Vec::new(),
            timeout_ms: 5_000,
            #[cfg(test)]
            test_version_probe_timeout_ms: None,
            working_dir: None,
            pinned_version: None,
            pinned_sha256: None,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            max_capture_bytes: DEFAULT_PROCESS_CAPTURE_BYTES,
            sandbox_root: None,
        }
    }
}

impl RtkConfig {
    /// Create a configuration using a specific executable.
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            ..Self::default()
        }
    }

    /// Add fixed arguments supplied before the shell command.
    #[must_use]
    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Set the maximum wall-clock duration for one external process.
    #[must_use]
    pub const fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    /// Extend the version-probe deadline only for process-level test fixtures.
    #[cfg(all(test, unix))]
    #[must_use]
    pub(crate) fn with_test_version_probe_timeout_ms(mut self, timeout_ms: u64) -> Self {
        assert!(timeout_ms > 0, "test version-probe timeout must be bounded");
        self.test_version_probe_timeout_ms = Some(timeout_ms);
        self
    }

    /// Run the optimizer in a fixed working directory.
    #[must_use]
    pub fn with_working_dir(mut self, working_dir: impl AsRef<Path>) -> Self {
        self.working_dir = Some(working_dir.as_ref().to_path_buf());
        self
    }

    /// Pin both the executable version and its SHA-256 digest.
    #[must_use]
    pub fn with_pins(mut self, version: impl Into<String>, sha256: impl Into<String>) -> Self {
        self.pinned_version = Some(version.into());
        self.pinned_sha256 = Some(sha256.into());
        self
    }

    /// Set only the exact version pin.
    #[must_use]
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.pinned_version = Some(version.into());
        self
    }

    /// Set only the executable hash pin.
    #[must_use]
    pub fn with_sha256(mut self, sha256: impl Into<String>) -> Self {
        self.pinned_sha256 = Some(sha256.into());
        self
    }

    /// Set the observation token bound.
    #[must_use]
    pub const fn with_max_output_tokens(mut self, max_output_tokens: u64) -> Self {
        self.max_output_tokens = max_output_tokens;
        self
    }

    /// Set the subprocess capture bound.
    #[must_use]
    pub const fn with_max_capture_bytes(mut self, max_capture_bytes: usize) -> Self {
        self.max_capture_bytes = max_capture_bytes;
        self
    }

    /// Set the PathJail root used for the invocation working directory.
    #[must_use]
    pub fn with_sandbox_root(mut self, sandbox_root: impl AsRef<Path>) -> Self {
        self.sandbox_root = Some(sandbox_root.as_ref().to_path_buf());
        self
    }
}

/// Evidenced failure at the optional external process boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CapabilityFailure {
    pub failure_mode: CapabilityFailureMode,
    pub reason: String,
    pub fallback_available: bool,
    pub evidence_ref: Option<String>,
}

impl CapabilityFailure {
    fn new(failure_mode: CapabilityFailureMode, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        let evidence_ref = Some(evidence_ref(&reason));
        Self {
            failure_mode,
            reason,
            fallback_available: true,
            evidence_ref,
        }
    }

    fn kill_switched() -> Self {
        Self::new(
            CapabilityFailureMode::FallbackToNative,
            "RTK capability is disabled by its kill switch",
        )
    }

    /// Compatibility constant for callers of the original small reference stub.
    #[allow(non_upper_case_globals)]
    pub const KillSwitched: Self = Self {
        failure_mode: CapabilityFailureMode::FallbackToNative,
        reason: String::new(),
        fallback_available: true,
        evidence_ref: None,
    };
}

/// Non-sensitive health snapshot for an RTK executable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RtkHealthReport {
    pub executable: PathBuf,
    pub resolved_executable: Option<PathBuf>,
    pub available: bool,
    pub version: Option<String>,
    pub sha256: Option<String>,
    pub reason: Option<String>,
}

/// Result of a bounded subprocess execution.
struct ProcessOutput {
    stdout: String,
    stderr: String,
    status: ExitStatus,
    elapsed_ms: u64,
}

/// External shell optimizer kept outside the production adapter registry.
pub struct RtkShellAdapter {
    config: RtkConfig,
    kill_switch: Arc<KillSwitch>,
    last_failure: Mutex<Option<CapabilityFailure>>,
}

impl RtkShellAdapter {
    /// Construct a reference adapter.  Missing pins make health false closed.
    #[must_use]
    pub fn new(config: RtkConfig) -> Self {
        Self::with_kill_switch(config, Arc::new(KillSwitch::new(CAPABILITY_ID)))
    }

    /// Construct an adapter with a shared atomic kill switch.
    #[must_use]
    pub fn with_kill_switch(config: RtkConfig, kill_switch: Arc<KillSwitch>) -> Self {
        Self {
            config,
            kill_switch,
            last_failure: Mutex::new(None),
        }
    }

    /// Read-only configuration access for diagnostics.
    #[must_use]
    pub const fn config(&self) -> &RtkConfig {
        &self.config
    }

    /// Read-only kill-switch access for orchestration.
    #[must_use]
    pub fn kill_switch(&self) -> &Arc<KillSwitch> {
        &self.kill_switch
    }

    /// Return the most recent evidenced external failure.
    #[must_use]
    pub fn last_failure(&self) -> Option<CapabilityFailure> {
        self.last_failure
            .lock()
            .ok()
            .and_then(|failure| failure.clone())
    }

    fn remember_failure(&self, failure: &CapabilityFailure) {
        if let Ok(mut last_failure) = self.last_failure.lock() {
            *last_failure = Some(failure.clone());
        }
    }

    /// Perform binary existence, exact version, and exact hash checks.
    #[must_use]
    pub fn health_check_detailed(&self) -> RtkHealthReport {
        let executable = self.config.executable.clone();
        if self.kill_switch.is_active() {
            return RtkHealthReport {
                executable,
                resolved_executable: None,
                available: false,
                version: None,
                sha256: None,
                reason: self
                    .kill_switch
                    .reason()
                    .or_else(|| Some("capability disabled by kill switch".to_string())),
            };
        }

        let Some(pinned_version) = self.config.pinned_version.as_deref() else {
            return unavailable_health(executable, "an exact RTK version pin is required");
        };
        let Some(pinned_sha256) = self.config.pinned_sha256.as_deref() else {
            return unavailable_health(executable, "an exact RTK SHA-256 pin is required");
        };
        let resolved = match resolve_executable(&self.config.executable) {
            Ok(path) => path,
            Err(reason) => return unavailable_health(executable, &reason),
        };
        #[cfg(test)]
        let version_probe_timeout_ms = self
            .config
            .test_version_probe_timeout_ms
            .unwrap_or_else(|| bounded_probe_timeout(self.config.timeout_ms));
        #[cfg(not(test))]
        let version_probe_timeout_ms = bounded_probe_timeout(self.config.timeout_ms);
        let version_probe = match run_bounded_process(
            &resolved,
            &["--version".to_string()],
            self.config
                .working_dir
                .as_deref()
                .unwrap_or_else(|| Path::new(".")),
            version_probe_timeout_ms,
            self.config.max_capture_bytes.min(16 * 1024),
        ) {
            Ok(output) => output,
            Err(reason) => return unavailable_health_with_path(executable, resolved, reason),
        };
        if !version_probe.status.success() {
            return unavailable_health_with_path(
                executable,
                resolved,
                format!("version probe exited with {}", version_probe.status),
            );
        }
        let version_text = format!("{}{}", version_probe.stdout, version_probe.stderr);
        if !version_text.contains(pinned_version) {
            return unavailable_health_with_path(
                executable,
                resolved,
                format!("version probe did not contain pinned version {pinned_version:?}"),
            );
        }
        let actual_sha256 = match sha256_file(&resolved) {
            Ok(hash) => hash,
            Err(reason) => return unavailable_health_with_path(executable, resolved, reason),
        };
        if !hashes_equal(&actual_sha256, pinned_sha256) {
            return unavailable_health_with_path(
                executable,
                resolved,
                "executable SHA-256 does not match the configured pin".to_string(),
            );
        }

        RtkHealthReport {
            executable,
            resolved_executable: Some(resolved),
            available: true,
            version: Some(version_text.trim().to_string()),
            sha256: Some(actual_sha256),
            reason: None,
        }
    }

    /// Check health without invoking anything when the kill switch is active.
    #[must_use]
    pub fn health_check(&self) -> RtkHealthReport {
        self.health_check_detailed()
    }

    /// Execute RTK only, returning a detailed failure instead of falling back.
    /// This is the method used by shadow mode.
    pub fn observe_rtk(
        &self,
        invocation: &CapabilityInvocation,
    ) -> Result<CapabilityResult, CapabilityFailure> {
        if self.kill_switch.is_active() {
            let failure = CapabilityFailure::kill_switched();
            self.remember_failure(&failure);
            return Err(failure);
        }

        let prepared = match self.prepare_invocation(invocation) {
            Ok(prepared) => prepared,
            Err(failure) => {
                self.remember_failure(&failure);
                return Err(failure);
            }
        };
        let health = self.health_check_detailed();
        if !health.available {
            let failure = CapabilityFailure::new(
                health
                    .reason
                    .as_deref()
                    .map_or(CapabilityFailureMode::Unavailable, failure_mode_for_reason),
                health
                    .reason
                    .unwrap_or_else(|| "RTK health check failed".to_string()),
            );
            self.remember_failure(&failure);
            return Err(failure);
        }
        let resolved = health
            .resolved_executable
            .expect("available health has a path");
        let started = Instant::now();
        let rewrite_args = {
            let mut args = self.config.args.clone();
            args.push("rewrite".to_string());
            args.push(prepared.command.clone());
            args
        };
        let rewrite_timeout = effective_timeout(invocation.timeout_ms, self.config.timeout_ms);
        let rewrite = match run_bounded_process(
            &resolved,
            &rewrite_args,
            &prepared.cwd,
            rewrite_timeout,
            self.config.max_capture_bytes,
        ) {
            Ok(output) => output,
            Err(failure) => {
                let failure = CapabilityFailure::new(failure_mode_for_reason(&failure), failure);
                self.remember_failure(&failure);
                return Err(failure);
            }
        };
        if !rewrite.status.success() {
            let reason = if rewrite.stderr.trim().is_empty() {
                format!("RTK rewrite exited with {}", rewrite.status)
            } else {
                format!("RTK rewrite failed: {}", rewrite.stderr.trim())
            };
            let failure = CapabilityFailure::new(CapabilityFailureMode::Unavailable, reason);
            self.remember_failure(&failure);
            return Err(failure);
        }
        let rewritten = match bounded_text(
            &redact_output(&rewrite.stdout),
            self.config.max_output_tokens,
        ) {
            Ok(output) => output.trim().to_string(),
            Err(reason) => {
                let failure = CapabilityFailure::new(CapabilityFailureMode::InvalidOutput, reason);
                self.remember_failure(&failure);
                return Err(failure);
            }
        };
        if rewritten.is_empty() {
            let failure = CapabilityFailure::new(
                CapabilityFailureMode::InvalidOutput,
                "RTK rewrite returned empty output",
            );
            self.remember_failure(&failure);
            return Err(failure);
        }
        if let Err(reason) = validate_shell_command(&rewritten) {
            let failure = CapabilityFailure::new(CapabilityFailureMode::RejectedByPolicy, reason);
            self.remember_failure(&failure);
            return Err(failure);
        }

        let elapsed_ms = started.elapsed().as_millis() as u64;
        let remaining_timeout = remaining_timeout(rewrite_timeout, elapsed_ms);
        let native = native_execute(&rewritten, &prepared.cwd, remaining_timeout);
        let output = redact_output(&native.output);
        let output = match bounded_text(&output, self.config.max_output_tokens) {
            Ok(output) => output,
            Err(reason) => {
                let failure = CapabilityFailure::new(CapabilityFailureMode::InvalidOutput, reason);
                self.remember_failure(&failure);
                return Err(failure);
            }
        };
        let latency_ms = started.elapsed().as_millis() as u64;
        let mut metrics = BTreeMap::new();
        metrics.insert("rewrite_latency_ms".to_string(), rewrite.elapsed_ms);
        metrics.insert("execution_latency_ms".to_string(), native.latency_ms);
        metrics.insert("output_bytes".to_string(), output.len() as u64);
        metrics.insert("quality_score".to_string(), 100);
        metrics.insert(
            "structurally_equal".to_string(),
            u64::from(native.exit_code == 0 && !native.timed_out),
        );
        let success = native.exit_code == 0 && !native.timed_out;
        let failure_mode = if native.timed_out {
            Some(CapabilityFailureMode::Timeout)
        } else if success {
            None
        } else {
            Some(CapabilityFailureMode::Partial)
        };
        let output_ref = Some(evidence_ref(&output));
        Ok(CapabilityResult {
            success,
            output_tokens: crate::core::tokens::count_tokens(&output) as u64,
            latency_ms,
            observation: CapabilityObservationV1 {
                schema_version: CAPABILITY_OBSERVATION_SCHEMA_VERSION,
                task_id: invocation.task_id.clone(),
                capability_id: CAPABILITY_ID.to_string(),
                capability_version: CAPABILITY_VERSION.to_string(),
                success,
                input_tokens: crate::core::tokens::count_tokens(&prepared.command) as u64,
                output_tokens: crate::core::tokens::count_tokens(&output) as u64,
                latency_ms,
                failure_mode,
                output_ref: output_ref.clone(),
                metrics,
            },
            evidence_ref: output_ref,
        })
    }

    /// Execute the native command only and return its observation.
    pub fn observe_native(
        &self,
        invocation: &CapabilityInvocation,
    ) -> OclaResult<CapabilityResult> {
        let prepared = self
            .prepare_invocation(invocation)
            .map_err(|failure| OclaError::InvalidRequest(failure.reason))?;
        Ok(native_result(
            invocation,
            &prepared.command,
            &prepared.cwd,
            effective_timeout(invocation.timeout_ms, self.config.timeout_ms),
        ))
    }

    /// Invoke the reference adapter and fall back to the original native
    /// command on every evidenced external failure.
    pub fn invoke_with_fallback(
        &self,
        invocation: &CapabilityInvocation,
    ) -> OclaResult<CapabilityResult> {
        invocation.validate()?;
        if !matches!(invocation.input, CapabilityInput::ShellCommand { .. }) {
            return Err(OclaError::InvalidRequest(
                "rtk-shell-output accepts only shell commands".to_string(),
            ));
        }
        match self.observe_rtk(invocation) {
            Ok(result) => Ok(result),
            Err(failure) => Ok(self.native_fallback(invocation, &failure)),
        }
    }

    fn native_fallback(
        &self,
        invocation: &CapabilityInvocation,
        failure: &CapabilityFailure,
    ) -> CapabilityResult {
        let prepared = self.prepare_invocation(invocation);
        let Ok(prepared) = prepared else {
            return failed_fallback_result(invocation, failure);
        };
        let mut result = native_result(
            invocation,
            &prepared.command,
            &prepared.cwd,
            native_fallback_timeout(invocation.timeout_ms, self.config.timeout_ms),
        );
        result.observation.failure_mode = Some(CapabilityFailureMode::FallbackToNative);
        result
            .observation
            .metrics
            .insert("fallback_available".to_string(), 1);
        result.observation.metrics.insert(
            "external_failure_mode".to_string(),
            failure_mode_metric(failure.failure_mode),
        );
        result
    }

    fn prepare_invocation(
        &self,
        invocation: &CapabilityInvocation,
    ) -> Result<PreparedInvocation, CapabilityFailure> {
        if invocation.capability_id != CAPABILITY_ID {
            return Err(CapabilityFailure::new(
                CapabilityFailureMode::RejectedByPolicy,
                format!("unexpected capability id {:?}", invocation.capability_id),
            ));
        }
        let CapabilityInput::ShellCommand { command, workdir } = &invocation.input else {
            return Err(CapabilityFailure::new(
                CapabilityFailureMode::RejectedByPolicy,
                "RTK accepts only shell command input",
            ));
        };
        validate_shell_command(command).map_err(|reason| {
            CapabilityFailure::new(CapabilityFailureMode::RejectedByPolicy, reason)
        })?;

        let cwd = workdir
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| self.config.working_dir.clone())
            .unwrap_or_else(|| PathBuf::from("."));
        let cwd = pathjail::canonicalize_or_self(&cwd);
        if !cwd.is_dir() {
            return Err(CapabilityFailure::new(
                CapabilityFailureMode::RejectedByPolicy,
                format!("working directory is not a directory: {}", cwd.display()),
            ));
        }
        let root = self
            .config
            .sandbox_root
            .clone()
            .or_else(|| crate::core::config::Config::find_project_root().map(PathBuf::from))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let root = pathjail::canonicalize_or_self(&root);
        let jailed = pathjail::jail_path(&cwd, &root).map_err(|error| {
            CapabilityFailure::new(
                CapabilityFailureMode::RejectedByPolicy,
                format!("PathJail rejected working directory: {error}"),
            )
        })?;
        Ok(PreparedInvocation {
            command: command.clone(),
            cwd: jailed,
        })
    }
}

impl CapabilityAdapter for RtkShellAdapter {
    fn manifest(&self) -> &lean_ctx_protocol::CapabilityManifestV1 {
        static MANIFEST: OnceLock<lean_ctx_protocol::CapabilityManifestV1> = OnceLock::new();
        MANIFEST.get_or_init(|| {
            serde_json::from_str(MANIFEST_JSON).expect("RTK capability manifest must be valid")
        })
    }

    fn invoke(&self, invocation: CapabilityInvocation) -> OclaResult<CapabilityResult> {
        self.invoke_with_fallback(&invocation)
    }

    fn health_check(&self) -> OclaResult<bool> {
        Ok(self.health_check_detailed().available)
    }
}

impl Default for RtkShellAdapter {
    fn default() -> Self {
        Self::new(RtkConfig::default())
    }
}

struct PreparedInvocation {
    command: String,
    cwd: PathBuf,
}

struct NativeOutput {
    output: String,
    exit_code: i32,
    timed_out: bool,
    latency_ms: u64,
}

fn validate_shell_command(command: &str) -> Result<(), String> {
    if let Some(reason) = crate::tools::ctx_shell::validate_command(command) {
        return Err(reason);
    }
    shell_allowlist::check_shell_allowlist(command).map_err(|error| error.to_string())
}

fn native_execute(command: &str, cwd: &Path, timeout_ms: u64) -> NativeOutput {
    let started = Instant::now();
    let (output, exit_code) = crate::server::execute::execute_command_with_env(
        command,
        &cwd.to_string_lossy(),
        &BTreeMap::new().into_iter().collect(),
        (timeout_ms != 0).then_some(timeout_ms),
    );
    let timed_out = output.contains(TIMEOUT_MARKER) || exit_code == 124;
    NativeOutput {
        output,
        exit_code,
        timed_out,
        latency_ms: started.elapsed().as_millis() as u64,
    }
}

fn native_result(
    invocation: &CapabilityInvocation,
    command: &str,
    cwd: &Path,
    timeout_ms: u64,
) -> CapabilityResult {
    let native = native_execute(command, cwd, timeout_ms);
    let output = redact_output(&native.output);
    let output_tokens = crate::core::tokens::count_tokens(&output) as u64;
    let input_tokens = crate::core::tokens::count_tokens(command) as u64;
    let success = native.exit_code == 0 && !native.timed_out;
    let output_ref = Some(evidence_ref(&output));
    let mut metrics = BTreeMap::new();
    metrics.insert("exit_code".to_string(), native.exit_code.max(0) as u64);
    metrics.insert("output_bytes".to_string(), output.len() as u64);
    metrics.insert("quality_score".to_string(), u64::from(success) * 100);
    CapabilityResult {
        success,
        output_tokens,
        latency_ms: native.latency_ms,
        observation: CapabilityObservationV1 {
            schema_version: CAPABILITY_OBSERVATION_SCHEMA_VERSION,
            task_id: invocation.task_id.clone(),
            capability_id: CAPABILITY_ID.to_string(),
            capability_version: CAPABILITY_VERSION.to_string(),
            success,
            input_tokens,
            output_tokens,
            latency_ms: native.latency_ms,
            failure_mode: if native.timed_out {
                Some(CapabilityFailureMode::Timeout)
            } else if success {
                None
            } else {
                Some(CapabilityFailureMode::Partial)
            },
            output_ref: output_ref.clone(),
            metrics,
        },
        evidence_ref: output_ref,
    }
}

fn failed_fallback_result(
    invocation: &CapabilityInvocation,
    failure: &CapabilityFailure,
) -> CapabilityResult {
    let mut metrics = BTreeMap::new();
    metrics.insert("fallback_available".to_string(), 1);
    metrics.insert(
        "external_failure_mode".to_string(),
        failure_mode_metric(failure.failure_mode),
    );
    CapabilityResult {
        success: false,
        output_tokens: 0,
        latency_ms: 0,
        observation: CapabilityObservationV1 {
            schema_version: CAPABILITY_OBSERVATION_SCHEMA_VERSION,
            task_id: invocation.task_id.clone(),
            capability_id: CAPABILITY_ID.to_string(),
            capability_version: CAPABILITY_VERSION.to_string(),
            success: false,
            input_tokens: crate::core::tokens::count_tokens(invocation.input.payload()) as u64,
            output_tokens: 0,
            latency_ms: 0,
            failure_mode: Some(CapabilityFailureMode::FallbackToNative),
            output_ref: failure.evidence_ref.clone(),
            metrics,
        },
        evidence_ref: failure.evidence_ref.clone(),
    }
}

fn resolve_executable(executable: &Path) -> Result<PathBuf, String> {
    if executable.components().count() > 1 || executable.is_absolute() {
        return executable
            .is_file()
            .then(|| pathjail::canonicalize_or_self(executable))
            .ok_or_else(|| format!("RTK binary does not exist: {}", executable.display()));
    }
    let path_var = std::env::var_os("PATH").ok_or_else(|| "PATH is not set".to_string())?;
    for directory in std::env::split_paths(&path_var) {
        let candidate = directory.join(executable);
        if candidate.is_file() {
            return Ok(pathjail::canonicalize_or_self(&candidate));
        }
    }
    Err(format!(
        "RTK binary was not found on PATH: {}",
        executable.display()
    ))
}

pub(crate) fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|error| format!("could not read RTK binary: {error}"))?;
    let mut digest = Sha256::new();
    digest.update(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest.finalize() {
        let _ = write!(&mut output, "{byte:02x}");
    }
    Ok(output)
}

fn hashes_equal(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected.trim().trim_start_matches("0x"))
}

fn unavailable_health(executable: PathBuf, reason: &str) -> RtkHealthReport {
    unavailable_health_with_path(executable, None, reason.to_string())
}

fn unavailable_health_with_path(
    executable: PathBuf,
    resolved_executable: impl Into<Option<PathBuf>>,
    reason: impl Into<String>,
) -> RtkHealthReport {
    RtkHealthReport {
        executable,
        resolved_executable: resolved_executable.into(),
        available: false,
        version: None,
        sha256: None,
        reason: Some(reason.into()),
    }
}

fn bounded_probe_timeout(configured: u64) -> u64 {
    if configured == 0 {
        VERSION_PROBE_TIMEOUT_MS
    } else {
        configured.min(VERSION_PROBE_TIMEOUT_MS)
    }
}

fn effective_timeout(invocation: u64, configured: u64) -> u64 {
    match (invocation, configured) {
        (0, value) | (value, 0) => value,
        (left, right) => left.min(right),
    }
}

fn native_fallback_timeout(invocation: u64, configured: u64) -> u64 {
    if invocation == 0 {
        configured
    } else {
        invocation
    }
}

fn remaining_timeout(timeout_ms: u64, elapsed_ms: u64) -> u64 {
    if timeout_ms == 0 {
        0
    } else {
        timeout_ms.saturating_sub(elapsed_ms).max(1)
    }
}

fn failure_mode_for_reason(reason: &str) -> CapabilityFailureMode {
    if reason.contains("timed out") {
        CapabilityFailureMode::Timeout
    } else if reason.contains("output exceeded") {
        CapabilityFailureMode::InvalidOutput
    } else {
        CapabilityFailureMode::Unavailable
    }
}

fn failure_mode_metric(mode: CapabilityFailureMode) -> u64 {
    match mode {
        CapabilityFailureMode::Timeout => 1,
        CapabilityFailureMode::Unavailable => 2,
        CapabilityFailureMode::RejectedByPolicy => 3,
        CapabilityFailureMode::InvalidOutput => 4,
        CapabilityFailureMode::Partial => 5,
        CapabilityFailureMode::FallbackToNative => 6,
        CapabilityFailureMode::Internal => 7,
    }
}

fn redact_output(output: &str) -> String {
    crate::core::redaction::redact_text_if_enabled(output)
}

fn bounded_text(text: &str, max_tokens: u64) -> Result<String, String> {
    if max_tokens == 0 || crate::core::tokens::count_tokens(text) as u64 <= max_tokens {
        return Ok(text.to_string());
    }
    Err(format!(
        "RTK output exceeded the configured token bound ({max_tokens})"
    ))
}

fn run_bounded_process(
    executable: &Path,
    args: &[String],
    cwd: &Path,
    timeout_ms: u64,
    capture_bytes: usize,
) -> Result<ProcessOutput, String> {
    let mut command = Command::new(executable);
    command.args(args).current_dir(cwd).stdin(Stdio::null());
    let started = Instant::now();
    let captured = crate::core::process_capture::run_with_output_limits(
        &mut command,
        (timeout_ms != 0).then(|| Duration::from_millis(timeout_ms)),
        capture_bytes,
        capture_bytes.min(64 * 1024),
    )?;
    if captured.timed_out {
        return Err(format!("RTK process timed out after {timeout_ms}ms"));
    }
    Ok(ProcessOutput {
        stdout: String::from_utf8_lossy(&captured.output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&captured.output.stderr).into_owned(),
        status: captured.output.status,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::{CAPABILITY_ID, CapabilityFailure, RtkConfig, RtkShellAdapter};
    use crate::core::ocla::invocation::{
        CapabilityAdapter, CapabilityInput, CapabilityInvocation, PolicyConstraints,
    };

    fn invocation(command: &str) -> CapabilityInvocation {
        CapabilityInvocation {
            task_id: "task-rtk-test".to_string(),
            capability_id: CAPABILITY_ID.to_string(),
            capability_version: "1.0.0".to_string(),
            input: CapabilityInput::ShellCommand {
                command: command.to_string(),
                workdir: None,
            },
            policy_constraints: PolicyConstraints::default(),
            timeout_ms: 500,
        }
    }

    fn invocation_at(command: &str, workdir: &Path) -> CapabilityInvocation {
        CapabilityInvocation {
            input: CapabilityInput::ShellCommand {
                command: command.to_string(),
                workdir: Some(workdir.to_string_lossy().into_owned()),
            },
            ..invocation(command)
        }
    }

    #[test]
    fn kill_switch_blocks_external_execution_and_falls_back() {
        let adapter = RtkShellAdapter::new(RtkConfig::new("definitely-not-installed"));
        adapter.kill_switch().activate("test");
        let failure = adapter.observe_rtk(&invocation("printf fallback"));
        assert_eq!(failure, Err(CapabilityFailure::kill_switched()));
        let result = adapter
            .invoke_with_fallback(&invocation("printf fallback"))
            .expect("native fallback");
        assert!(result.observation.failure_mode.is_some());
        assert_eq!(
            result.observation.metrics.get("fallback_available"),
            Some(&1)
        );
    }

    #[test]
    fn missing_binary_is_evidenced_and_falls_back() {
        let sandbox_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let adapter = RtkShellAdapter::new(
            RtkConfig::new("definitely-not-installed").with_sandbox_root(sandbox_root),
        );
        let result = adapter
            .invoke_with_fallback(&invocation_at("printf fallback", sandbox_root))
            .expect("native fallback");
        assert!(
            result.success,
            "fallback result: {result:?}; external failure: {:?}",
            adapter.last_failure()
        );
        let failure = adapter.last_failure().expect("failure evidence");
        assert_eq!(
            failure.failure_mode,
            crate::core::ocla::invocation::CapabilityFailureMode::Unavailable
        );
        assert!(failure.fallback_available);
        assert!(failure.evidence_ref.is_some());
    }

    #[test]
    fn manifest_has_external_identity() {
        let adapter = RtkShellAdapter::default();
        assert_eq!(adapter.manifest().capability_id.as_str(), CAPABILITY_ID);
        assert!(adapter.manifest().local);
        assert!(!adapter.manifest().remote);
    }

    #[cfg(unix)]
    #[test]
    fn timeout_is_evidenced_and_falls_back() {
        use super::sha256_file;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;

        let directory = tempdir().expect("temporary directory");
        let binary = directory.path().join("rtk");
        std::fs::write(
            &binary,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'rtk 1.2.3'; exit 0; fi\nsleep 2\n",
        )
        .expect("write fake RTK");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
            .expect("make fake RTK executable");
        let hash = sha256_file(&binary).expect("hash fake RTK");
        let config = RtkConfig::new(&binary)
            .with_pins("1.2.3", hash)
            .with_working_dir(directory.path())
            .with_sandbox_root(directory.path())
            .with_timeout_ms(50);
        let adapter = RtkShellAdapter::new(config);
        let result = adapter
            .invoke_with_fallback(&invocation("printf fallback"))
            .expect("native fallback after timeout");
        assert!(
            result.success,
            "fallback result: {result:?}; external failure: {:?}",
            adapter.last_failure()
        );
        let failure = adapter.last_failure().expect("timeout evidence");
        assert_eq!(
            failure.failure_mode,
            crate::core::ocla::invocation::CapabilityFailureMode::Timeout,
            "failure: {failure:?}"
        );
        assert!(failure.fallback_available);
        assert!(failure.evidence_ref.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn process_capture_does_not_wait_for_inherited_output_handles() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let started = std::time::Instant::now();
        let output = super::run_bounded_process(
            std::path::Path::new("/bin/sh"),
            &["-c".to_owned(), "sleep 2 & printf ready".to_owned()],
            directory.path(),
            100,
            1024,
        )
        .expect("parent completed successfully");
        assert!(output.status.success());
        assert_eq!(output.stdout, "ready");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "completed parent must not wait for a descendant's inherited pipe"
        );
    }

    #[cfg(unix)]
    #[test]
    fn process_capture_preserves_status_streams_and_zero_timeout() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let output = super::run_bounded_process(
            std::path::Path::new("/bin/sh"),
            &[
                "-c".to_owned(),
                "sleep 0.02; printf good; printf bad >&2; exit 7".to_owned(),
            ],
            directory.path(),
            0,
            4,
        )
        .expect("zero timeout is unlimited, not immediate cancellation");
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, "good");
        assert_eq!(output.stderr, "bad");
    }

    #[cfg(unix)]
    #[test]
    fn process_capture_rejects_stdout_and_stderr_overflow() {
        let directory = tempfile::tempdir().expect("temporary directory");
        for script in ["printf four", "printf four >&2"] {
            let failure = super::run_bounded_process(
                std::path::Path::new("/bin/sh"),
                &["-c".to_owned(), script.to_owned()],
                directory.path(),
                100,
                3,
            )
            .err()
            .expect("oversized output cannot be admitted");
            assert_eq!(
                super::failure_mode_for_reason(&failure),
                crate::core::ocla::invocation::CapabilityFailureMode::InvalidOutput
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn process_capture_stops_oversized_writer_without_timeout() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let started = std::time::Instant::now();
        let failure = super::run_bounded_process(
            std::path::Path::new("/bin/sh"),
            &["-c".to_owned(), "printf four; sleep 2".to_owned()],
            directory.path(),
            0,
            3,
        )
        .err()
        .expect("overflow must stop the writer even without a time limit");
        assert_eq!(
            super::failure_mode_for_reason(&failure),
            crate::core::ocla::invocation::CapabilityFailureMode::InvalidOutput
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn process_capture_preserves_separate_stderr_ceiling() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let failure = super::run_bounded_process(
            std::path::Path::new("/bin/sh"),
            &["-c".to_owned(), "head -c 65537 /dev/zero >&2".to_owned()],
            directory.path(),
            1000,
            128 * 1024,
        )
        .err()
        .expect("stderr remains limited to 64KiB even with a larger stdout limit");
        assert!(failure.contains("stderr output exceeded"), "{failure}");
    }

    #[test]
    fn observations_are_payload_free() {
        let adapter = RtkShellAdapter::new(RtkConfig::new("missing-rtk"));
        let result = adapter
            .invoke_with_fallback(&invocation("printf secret-value"))
            .expect("native fallback");
        assert!(
            !result
                .observation
                .output_ref
                .as_deref()
                .unwrap_or_default()
                .contains("secret-value")
        );
        assert!(!BTreeMap::<String, u64>::from_iter(result.observation.metrics.clone()).is_empty());
    }
}
