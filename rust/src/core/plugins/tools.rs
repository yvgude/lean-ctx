//! Manifest-declared tools (EPIC 12.11).
//!
//! Flattens `[[tools]]` entries from enabled plugins into [`PluginToolSpec`]s
//! and invokes them as sandboxed subprocesses. The tool layer adapts each spec
//! into a native MCP tool (`tools::registered::plugin_tool`) and registers it
//! dynamically in `build_registry()` — so a developer adds a tool by shipping a
//! manifest, never by forking the registry.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use super::sandbox::SandboxPolicy;

/// A flattened, ready-to-register tool contributed by a plugin manifest.
#[derive(Debug, Clone)]
pub struct PluginToolSpec {
    /// Owning plugin name (for diagnostics + capabilities).
    pub plugin_name: String,
    /// Plugin directory, exported to the child as `LEAN_CTX_PLUGIN_DIR`.
    pub plugin_dir: PathBuf,
    /// Tool name as exposed to agents.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Command to run (whitespace-split into program + args).
    pub command: String,
    /// Per-call timeout.
    pub timeout_ms: u64,
    /// JSON Schema for the tool's arguments.
    pub input_schema: Value,
    /// Sandbox policy inherited from the owning plugin's `[trust]` (EPIC 12.3).
    pub policy: SandboxPolicy,
}

/// Invoke a plugin tool: the JSON `args_json` is written to the child's stdin
/// and stdout is returned as the tool result. Runs sandboxed with the shared
/// subprocess runner (piped stdio + bounded timeout).
pub fn invoke(spec: &PluginToolSpec, args_json: &str) -> Result<String, String> {
    let output = super::executor::run_subprocess(
        &spec.command,
        &spec.plugin_dir,
        &[("LEAN_CTX_TOOL", spec.name.as_str())],
        args_json,
        Duration::from_millis(spec.timeout_ms),
        &spec.policy,
    )?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(if stderr.trim().is_empty() {
            format!("tool '{}' exited with {}", spec.name, output.status)
        } else {
            format!("tool '{}': {}", spec.name, stderr.trim())
        })
    }
}

// Every test here shells out to unix-only commands (`cat`, `false`), so the
// whole module is unix-gated. Gating the module (rather than each item) keeps
// Windows free of dead-code / unused-import errors under `-D warnings`.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn spec(command: &str) -> PluginToolSpec {
        PluginToolSpec {
            plugin_name: "demo".into(),
            plugin_dir: PathBuf::from("/tmp"),
            name: "demo_tool".into(),
            description: "demo".into(),
            command: command.into(),
            timeout_ms: 2000,
            input_schema: Value::Null,
            policy: SandboxPolicy::strict(),
        }
    }

    #[test]
    fn invoke_returns_stdout() {
        let out = invoke(&spec("cat"), "{\"q\":1}").unwrap();
        assert_eq!(out, "{\"q\":1}");
    }

    #[test]
    fn invoke_reports_failure() {
        // `false` exits non-zero with no stdout/stderr.
        let err = invoke(&spec("false"), "{}").unwrap_err();
        assert!(err.contains("demo_tool"));
    }
}
