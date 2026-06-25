use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::registry::Plugin;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "hook", rename_all = "snake_case")]
pub enum HookPoint {
    OnSessionStart,
    OnSessionEnd,
    PreRead {
        path: String,
    },
    PostCompress {
        path: String,
        original_tokens: usize,
        compressed_tokens: usize,
    },
    OnKnowledgeUpdate {
        fact_id: String,
    },
}

impl HookPoint {
    #[must_use]
    pub fn hook_name(&self) -> &'static str {
        match self {
            Self::OnSessionStart => "on_session_start",
            Self::OnSessionEnd => "on_session_end",
            Self::PreRead { .. } => "pre_read",
            Self::PostCompress { .. } => "post_compress",
            Self::OnKnowledgeUpdate { .. } => "on_knowledge_update",
        }
    }

    #[must_use]
    pub fn all_hook_names() -> &'static [&'static str] {
        &[
            "on_session_start",
            "on_session_end",
            "pre_read",
            "post_compress",
            "on_knowledge_update",
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookResult {
    pub plugin_name: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub duration_ms: u64,
}

#[must_use]
pub fn execute_hook_sync(plugin: &Plugin, hook: &HookPoint) -> HookResult {
    let hook_name = hook.hook_name();
    let plugin_name = plugin.manifest.plugin.name.clone();

    let Some(entry) = plugin.manifest.hooks.get(hook_name) else {
        return HookResult {
            plugin_name,
            success: true,
            output: None,
            error: None,
            duration_ms: 0,
        };
    };

    let timeout = Duration::from_millis(entry.timeout_ms);
    let start = std::time::Instant::now();

    let hook_json = match serde_json::to_string(hook) {
        Ok(j) => j,
        Err(e) => {
            return HookResult {
                plugin_name,
                success: false,
                output: None,
                error: Some(format!("failed to serialize hook data: {e}")),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };

    let result = run_subprocess(
        &entry.command,
        &plugin.path,
        &[("LEAN_CTX_HOOK", hook_name)],
        &hook_json,
        timeout,
        &plugin.manifest.trust.policy(),
    );
    let duration_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let success = output.status.success();
            HookResult {
                plugin_name,
                success,
                output: if stdout.is_empty() {
                    None
                } else {
                    Some(stdout)
                },
                error: if stderr.is_empty() && success {
                    None
                } else if !stderr.is_empty() {
                    Some(stderr)
                } else {
                    Some(format!("exit code: {}", output.status))
                },
                duration_ms,
            }
        }
        Err(e) => HookResult {
            plugin_name,
            success: false,
            output: None,
            error: Some(e),
            duration_ms,
        },
    }
}

/// Spawn `command` (whitespace-split into program + args) as a sandboxed child:
/// piped stdio, `LEAN_CTX_PLUGIN_DIR` exported, plus any `extra_env`. The
/// `stdin_data` is written to the child's stdin and the process is bounded by
/// `timeout`. The [`SandboxPolicy`] is applied before spawn (env scrub + cwd
/// jail; EPIC 12.3). Shared by hook execution and manifest-declared tool
/// invocation (EPIC 12.11) so both honor the same isolation contract.
pub(crate) fn run_subprocess(
    command: &str,
    plugin_dir: &std::path::Path,
    extra_env: &[(&str, &str)],
    stdin_data: &str,
    timeout: Duration,
    policy: &super::sandbox::SandboxPolicy,
) -> Result<std::process::Output, String> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    let Some((program, args)) = parts.split_first() else {
        return Err("empty command".to_string());
    };

    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Apply the sandbox (env scrub + cwd jail) first, then set the trusted
    // lean-ctx env + caller extras so they always win over the scrubbed base.
    policy.apply(&mut cmd, plugin_dir);
    cmd.env("LEAN_CTX_PLUGIN_DIR", plugin_dir);
    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    let mut child = cmd.spawn().map_err(|e| format!("failed to spawn: {e}"))?;

    if let Some(ref mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(stdin_data.as_bytes());
    }

    wait_with_timeout(&mut child, timeout)
}

fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = child
                    .stdout
                    .take()
                    .map(|mut s| {
                        use std::io::Read;
                        let mut buf = Vec::new();
                        let _ = s.read_to_end(&mut buf);
                        buf
                    })
                    .unwrap_or_default();
                let stderr = child
                    .stderr
                    .take()
                    .map(|mut s| {
                        use std::io::Read;
                        let mut buf = Vec::new();
                        let _ = s.read_to_end(&mut buf);
                        buf
                    })
                    .unwrap_or_default();
                return Ok(std::process::Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err(format!("timeout after {}ms", timeout.as_millis()));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(format!("wait error: {e}")),
        }
    }
}

#[must_use]
pub fn execute_hooks_for_point(plugins: &[&Plugin], hook: &HookPoint) -> Vec<HookResult> {
    let hook_name = hook.hook_name();
    plugins
        .iter()
        .filter(|p| p.enabled && p.manifest.hooks.contains_key(hook_name))
        .map(|p| execute_hook_sync(p, hook))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_point_names() {
        assert_eq!(HookPoint::OnSessionStart.hook_name(), "on_session_start");
        assert_eq!(HookPoint::OnSessionEnd.hook_name(), "on_session_end");
        assert_eq!(
            HookPoint::PreRead { path: "x".into() }.hook_name(),
            "pre_read"
        );
        assert_eq!(
            HookPoint::PostCompress {
                path: "x".into(),
                original_tokens: 100,
                compressed_tokens: 50,
            }
            .hook_name(),
            "post_compress"
        );
        assert_eq!(
            HookPoint::OnKnowledgeUpdate {
                fact_id: "f1".into()
            }
            .hook_name(),
            "on_knowledge_update"
        );
    }

    #[test]
    fn all_hook_names_complete() {
        let names = HookPoint::all_hook_names();
        assert_eq!(names.len(), 5);
        assert!(names.contains(&"on_session_start"));
        assert!(names.contains(&"pre_read"));
        assert!(names.contains(&"post_compress"));
    }

    #[test]
    fn hook_point_serializes_to_json() {
        let hook = HookPoint::PostCompress {
            path: "/tmp/file.rs".into(),
            original_tokens: 1000,
            compressed_tokens: 200,
        };
        let json = serde_json::to_string(&hook).unwrap();
        assert!(json.contains("post_compress"));
        assert!(json.contains("1000"));
        assert!(json.contains("200"));
    }

    #[test]
    fn execute_missing_hook_is_noop() {
        let manifest = crate::core::plugins::manifest::PluginManifest::from_str(
            r#"
[plugin]
name = "no-hooks"
version = "1.0.0"
"#,
            &std::path::PathBuf::from("test.toml"),
        )
        .unwrap();

        let plugin = Plugin {
            manifest,
            enabled: true,
            path: std::path::PathBuf::from("/tmp/no-hooks"),
        };

        let result = execute_hook_sync(&plugin, &HookPoint::OnSessionStart);
        assert!(result.success);
        assert_eq!(result.duration_ms, 0);
    }

    #[test]
    fn execute_nonexistent_binary_fails() {
        let manifest = crate::core::plugins::manifest::PluginManifest::from_str(
            r#"
[plugin]
name = "bad-binary"
version = "1.0.0"

[hooks.on_session_start]
command = "__nonexistent_lean_ctx_test_binary__ start"
timeout_ms = 1000
"#,
            &std::path::PathBuf::from("test.toml"),
        )
        .unwrap();

        let plugin = Plugin {
            manifest,
            enabled: true,
            path: std::path::PathBuf::from("/tmp/bad-binary"),
        };

        let result = execute_hook_sync(&plugin, &HookPoint::OnSessionStart);
        assert!(!result.success);
        assert!(result.error.unwrap().contains("failed to spawn"));
    }

    #[cfg(unix)]
    #[test]
    fn run_subprocess_echoes_stdin() {
        let out = run_subprocess(
            "cat",
            std::path::Path::new("/tmp"),
            &[("LEAN_CTX_TOOL", "demo")],
            "hello-stdin",
            Duration::from_secs(2),
            &super::super::sandbox::SandboxPolicy::strict(),
        )
        .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hello-stdin");
    }

    #[test]
    fn run_subprocess_empty_command_errors() {
        let err = run_subprocess(
            "   ",
            std::path::Path::new("/tmp"),
            &[],
            "",
            Duration::from_millis(500),
            &super::super::sandbox::SandboxPolicy::strict(),
        )
        .unwrap_err();
        assert!(err.contains("empty command"));
    }

    #[cfg(unix)]
    #[test]
    fn execute_echo_plugin_succeeds() {
        let manifest = crate::core::plugins::manifest::PluginManifest::from_str(
            r#"
[plugin]
name = "echo-plugin"
version = "1.0.0"

[hooks.on_session_start]
command = "echo hello"
timeout_ms = 2000
"#,
            &std::path::PathBuf::from("test.toml"),
        )
        .unwrap();

        let plugin = Plugin {
            manifest,
            enabled: true,
            path: std::path::PathBuf::from("/tmp/echo-plugin"),
        };

        let result = execute_hook_sync(&plugin, &HookPoint::OnSessionStart);
        assert!(result.success);
        assert!(result.output.unwrap().contains("hello"));
    }
}
