use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::RwLock;

use crate::core::cache::SessionCache;
use crate::core::session::SessionState;
use crate::server::registry::{ToolRegistry, build_registry};
use crate::server::tool_trait::{ShellOutcome, ToolContext, ToolOutput};

const SCHEMA_VERSION: u32 = 1;
const TRANSPORT_VERSION: u32 = 1;
const INTERFACE_VERSION: &str = "1.0.0";
const MAX_POLICY_BYTES: u64 = 64 * 1024;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_ID_BYTES: usize = 128;

const READ_TOOLS: &[&str] = &[
    "ctx_compose",
    "ctx_glob",
    "ctx_read",
    "ctx_search",
    "ctx_symbol",
    "ctx_tree",
];
const WRITE_TOOLS: &[&str] = &["ctx_edit", "ctx_fill", "ctx_patch"];
const EXEC_TOOLS: &[&str] = &["ctx_shell"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyV1 {
    schema_version: u32,
    allow_write: bool,
    allow_exec: bool,
    allowed_executables: Vec<String>,
    allowed_env: Vec<String>,
    max_timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestV1 {
    id: String,
    op: String,
    #[serde(default)]
    schema_version: Option<u32>,
    #[serde(default)]
    transport_version: Option<u32>,
    #[serde(default)]
    agent_tools_interface_version: Option<String>,
    #[serde(default)]
    sdk_version: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    arguments: Option<Map<String, Value>>,
}

#[derive(Debug, Serialize)]
struct ErrorV1 {
    code: &'static str,
    message: String,
}

#[derive(Debug, Serialize)]
struct ResponseV1<T: Serialize> {
    id: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorV1>,
}

#[derive(Debug, Serialize)]
struct HelloResultV1 {
    schema_version: u32,
    transport_version: u32,
    agent_tools_interface_version: &'static str,
    engine_version: &'static str,
    capabilities: Vec<&'static str>,
    allow_write: bool,
    allow_exec: bool,
}

#[derive(Debug, Serialize)]
struct ToolResultV1 {
    text: String,
    content_blocks: Value,
    original_tokens: usize,
    output_tokens: usize,
    saved_tokens: usize,
    mode: Option<String>,
    changed: bool,
    shell: Option<Value>,
}

struct Session {
    root: String,
    policy: PolicyV1,
    registry: ToolRegistry,
    cache: Arc<RwLock<SessionCache>>,
    state: Arc<RwLock<SessionState>>,
    bm25_cache: crate::core::bm25_cache::SharedBm25Cache,
}

enum InputFrame {
    Data(Vec<u8>),
    TooLarge,
}

impl Session {
    fn capabilities(&self) -> Vec<&'static str> {
        let mut result = READ_TOOLS.to_vec();
        if self.policy.allow_write {
            result.extend_from_slice(WRITE_TOOLS);
        }
        if self.policy.allow_exec {
            result.extend_from_slice(EXEC_TOOLS);
        }
        result.sort_unstable();
        result
    }

    fn permitted(&self, tool: &str) -> bool {
        READ_TOOLS.contains(&tool)
            || (self.policy.allow_write && WRITE_TOOLS.contains(&tool))
            || (self.policy.allow_exec && EXEC_TOOLS.contains(&tool))
    }

    fn call(&self, tool: &str, arguments: &Map<String, Value>) -> Result<ToolOutput, ErrorV1> {
        if !self.permitted(tool) {
            let code = if WRITE_TOOLS.contains(&tool) || EXEC_TOOLS.contains(&tool) {
                "permission_denied"
            } else {
                "unsupported_capability"
            };
            return Err(ErrorV1 {
                code,
                message: format!("tool is not permitted: {tool}"),
            });
        }
        if tool == "ctx_shell" {
            return self.call_shell(arguments);
        }
        let handler = self.registry.get(tool).ok_or_else(|| ErrorV1 {
            code: "unsupported_capability",
            message: format!("tool is unavailable: {tool}"),
        })?;
        let mut resolved_paths = HashMap::new();
        if let Some(path) = arguments.get("path").and_then(Value::as_str) {
            let resolved =
                crate::core::path_resolve::resolve_tool_path(Some(&self.root), None, path)
                    .map_err(|message| ErrorV1 {
                        code: "path_rejected",
                        message,
                    })?;
            resolved_paths.insert(
                "path".to_string(),
                if resolved.is_empty() || resolved == "." {
                    self.root.clone()
                } else {
                    resolved
                },
            );
        }

        let context = ToolContext {
            project_root: self.root.clone(),
            resolved_paths,
            cache: Some(self.cache.clone()),
            bm25_cache: Some(self.bm25_cache.clone()),
            session: Some(self.state.clone()),
            ..Default::default()
        };
        handler
            .handle(arguments, &context)
            .map_err(|error| ErrorV1 {
                code: "tool_error",
                message: error.to_string(),
            })
    }

    fn call_shell(&self, arguments: &Map<String, Value>) -> Result<ToolOutput, ErrorV1> {
        let prepared = self.prepare_shell(arguments)?;
        let argv = arguments["argv"]
            .as_array()
            .expect("validated argv")
            .iter()
            .map(|value| value.as_str().expect("validated argv item"))
            .collect::<Vec<_>>();
        let cwd = arguments["cwd"].as_str().expect("validated cwd");
        let resolved_cwd =
            crate::core::path_resolve::resolve_tool_path(Some(&self.root), None, cwd).map_err(
                |message| ErrorV1 {
                    code: "path_rejected",
                    message,
                },
            )?;
        let resolved_cwd = if resolved_cwd.is_empty() || resolved_cwd == "." {
            self.root.as_str()
        } else {
            resolved_cwd.as_str()
        };
        let environment = arguments["env"].as_object().expect("validated env");
        let timeout_ms = prepared["timeout_ms"].as_u64().expect("validated timeout");
        let display = prepared["command"].as_str().expect("prepared command");
        let executable = resolve_allowed_executable(argv[0], &self.root)?;
        let mut command = std::process::Command::new(executable);
        command
            .args(&argv[1..])
            .current_dir(resolved_cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env_clear();
        for name in ["PATH", "SYSTEMROOT", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command.env("LANG", "C").env("LC_ALL", "C").env("TZ", "UTC");
        command.envs(
            environment
                .iter()
                .map(|(name, value)| (name, value.as_str().expect("validated env value"))),
        );
        let child = command.spawn().map_err(|_| ErrorV1 {
            code: "tool_error",
            message: "shell process could not be executed".to_string(),
        })?;
        let output = crate::shell::exec::wait_with_limits(
            child,
            MAX_RESPONSE_BYTES / 2,
            std::time::Duration::from_millis(timeout_ms),
            true,
        );
        let exit_code = output.status.code().unwrap_or(1);
        let mut raw_output = String::from_utf8_lossy(&output.stdout).into_owned();
        raw_output.push_str(&String::from_utf8_lossy(&output.stderr));
        let original_tokens = crate::core::tokens::count_tokens(&raw_output);
        let text = crate::tools::ctx_shell::handle(
            display,
            &raw_output,
            exit_code,
            crate::core::protocol::CrpMode::Compact,
        );
        let output_tokens = crate::core::tokens::count_tokens(&text);
        Ok(ToolOutput {
            text,
            original_tokens,
            saved_tokens: original_tokens.saturating_sub(output_tokens),
            mode: Some("shell".to_string()),
            path: None,
            changed: false,
            shell_outcome: Some(ShellOutcome::Exit(exit_code)),
            content_blocks: None,
        })
    }

    fn prepare_shell(&self, arguments: &Map<String, Value>) -> Result<Map<String, Value>, ErrorV1> {
        const KEYS: &[&str] = &["argv", "cwd", "env", "timeout_ms"];
        if arguments.keys().any(|key| !KEYS.contains(&key.as_str())) {
            return Err(invalid_request("shell arguments are invalid"));
        }
        let argv = arguments
            .get("argv")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_request("argv is required"))?;
        if argv.is_empty() || argv.len() > 256 {
            return Err(invalid_request("argv length is invalid"));
        }
        let argv: Result<Vec<&str>, ErrorV1> = argv
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|item| !item.is_empty() && !item.contains('\0'))
                    .ok_or_else(|| invalid_request("argv item is invalid"))
            })
            .collect();
        let argv = argv?;
        if argv[0].contains('/') || argv[0].contains('\\') {
            return Err(ErrorV1 {
                code: "permission_denied",
                message: "executable must be a bare allowlisted name".to_string(),
            });
        }
        let executable = argv[0];
        if !self
            .policy
            .allowed_executables
            .iter()
            .any(|allowed| allowed == executable)
        {
            return Err(ErrorV1 {
                code: "permission_denied",
                message: format!("executable is not allowed: {executable}"),
            });
        }

        let timeout_ms = arguments
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(self.policy.max_timeout_ms);
        if !(100..=self.policy.max_timeout_ms).contains(&timeout_ms) {
            return Err(ErrorV1 {
                code: "permission_denied",
                message: "shell timeout exceeds policy".to_string(),
            });
        }
        let environment = arguments
            .get("env")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid_request("env must be an object"))?;
        for (name, value) in environment {
            if !value.is_string() || !self.policy.allowed_env.contains(name) {
                return Err(ErrorV1 {
                    code: "permission_denied",
                    message: format!("environment variable is not allowed: {name}"),
                });
            }
        }
        if arguments.get("cwd").and_then(Value::as_str).is_none() {
            return Err(invalid_request("cwd must be a string"));
        }

        let mut prepared = arguments.clone();
        prepared.remove("argv");
        prepared.insert(
            "command".to_string(),
            Value::String(crate::shell::platform::join_command(
                &argv
                    .iter()
                    .map(|item| (*item).to_string())
                    .collect::<Vec<_>>(),
            )),
        );
        prepared.insert("timeout_ms".to_string(), Value::from(timeout_ms));
        Ok(prepared)
    }
}

fn invalid_request(message: &str) -> ErrorV1 {
    ErrorV1 {
        code: "invalid_request",
        message: message.to_string(),
    }
}

fn resolve_allowed_executable(name: &str, root: &str) -> Result<PathBuf, ErrorV1> {
    let path = std::env::var_os("PATH").ok_or_else(|| ErrorV1 {
        code: "permission_denied",
        message: "trusted executable search path is unavailable".to_string(),
    })?;
    for directory in std::env::split_paths(&path).filter(|path| path.is_absolute()) {
        if directory.starts_with(root) {
            continue;
        }
        #[cfg(windows)]
        let candidates = if Path::new(name).extension().is_some() {
            vec![directory.join(name)]
        } else {
            vec![
                directory.join(format!("{name}.exe")),
                directory.join(format!("{name}.com")),
            ]
        };
        #[cfg(not(windows))]
        let candidates = vec![directory.join(name)];
        for candidate in candidates {
            let Ok(canonical) = candidate.canonicalize() else {
                continue;
            };
            if candidate.starts_with(root) || !canonical.is_file() || canonical.starts_with(root) {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if canonical
                    .metadata()
                    .map_or(true, |metadata| metadata.permissions().mode() & 0o111 == 0)
                {
                    continue;
                }
            }
            return Ok(canonical);
        }
    }
    Err(ErrorV1 {
        code: "permission_denied",
        message: format!("allowed executable was not found on trusted PATH: {name}"),
    })
}

pub(super) fn cmd_agent_tools(args: &[String]) {
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        eprintln!("engine: runtime_unavailable");
        std::process::exit(2);
    };
    let result = runtime.block_on(async {
        tokio::task::block_in_place(|| run(args, io::stdin().lock(), io::stdout().lock()))
    });
    if let Err(code) = result {
        eprintln!("engine: {code}");
        std::process::exit(2);
    }
}

fn run<R: BufRead, W: Write>(args: &[String], reader: R, writer: W) -> Result<(), &'static str> {
    let (root, policy_path) = parse_args(args)?;
    let root = canonical_root(&root)?;
    let policy = read_policy(&policy_path)?;
    let mut state = SessionState::new();
    state.project_root = Some(root.clone());
    state.shell_cwd = Some(root.clone());
    let session = Session {
        root,
        policy,
        registry: build_registry(),
        cache: Arc::new(RwLock::new(SessionCache::new())),
        state: Arc::new(RwLock::new(state)),
        bm25_cache: Arc::new(Mutex::new(None)),
    };
    serve(&session, reader, writer)
}

fn serve<R: BufRead, W: Write>(
    session: &Session,
    mut reader: R,
    writer: W,
) -> Result<(), &'static str> {
    let mut writer = BufWriter::new(writer);
    let mut hello_complete = false;
    while let Some(frame) = read_frame(&mut reader).map_err(|_| "request_read_failed")? {
        let response = match frame {
            InputFrame::TooLarge => {
                error_response("", "request_too_large", "request exceeds size bound")
            }
            InputFrame::Data(line) => match serde_json::from_slice::<RequestV1>(&line) {
                Ok(request) => handle_request(session, request, &mut hello_complete),
                Err(_) => error_response("", "invalid_request", "request is not strict JSON v1"),
            },
        };
        let should_close = !response.id.is_empty()
            && response
                .result
                .as_ref()
                .is_some_and(|value| value.get("closed").and_then(Value::as_bool) == Some(true));
        let mut encoded = serde_json::to_vec(&response).map_err(|_| "response_write_failed")?;
        if encoded.len().saturating_add(1) > MAX_RESPONSE_BYTES {
            encoded = serde_json::to_vec(&error_response(
                &response.id,
                "response_too_large",
                "response exceeds size bound",
            ))
            .map_err(|_| "response_write_failed")?;
        }
        writer
            .write_all(&encoded)
            .map_err(|_| "response_write_failed")?;
        writer
            .write_all(b"\n")
            .map_err(|_| "response_write_failed")?;
        writer.flush().map_err(|_| "response_write_failed")?;
        if should_close {
            break;
        }
    }
    Ok(())
}

fn read_frame<R: BufRead>(reader: &mut R) -> io::Result<Option<InputFrame>> {
    let mut data = Vec::new();
    let mut too_large = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return if data.is_empty() && !too_large {
                Ok(None)
            } else if too_large {
                Ok(Some(InputFrame::TooLarge))
            } else {
                Ok(Some(InputFrame::Data(data)))
            };
        }

        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(buffer.len(), |index| index + 1);
        let payload = newline.map_or(buffer, |index| &buffer[..index]);
        if !too_large {
            if data.len().saturating_add(payload.len()) > MAX_REQUEST_BYTES {
                data.clear();
                too_large = true;
            } else {
                data.extend_from_slice(payload);
            }
        }
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some(if too_large {
                InputFrame::TooLarge
            } else {
                InputFrame::Data(data)
            }));
        }
    }
}

fn handle_request(session: &Session, request: RequestV1, hello: &mut bool) -> ResponseV1<Value> {
    if validate_id(&request.id).is_err() {
        return error_response("", "invalid_request", "id is invalid");
    }
    match request.op.as_str() {
        "hello" => {
            if *hello {
                return error_response(&request.id, "invalid_state", "hello already completed");
            }
            if request.schema_version != Some(SCHEMA_VERSION)
                || request.transport_version != Some(TRANSPORT_VERSION)
                || request.agent_tools_interface_version.as_deref() != Some(INTERFACE_VERSION)
                || request.sdk_version.as_deref().is_none_or(str::is_empty)
                || request.tool.is_some()
                || request.arguments.is_some()
            {
                return error_response(
                    &request.id,
                    "unsupported_interface",
                    "hello version or shape is unsupported",
                );
            }
            *hello = true;
            let result = HelloResultV1 {
                schema_version: SCHEMA_VERSION,
                transport_version: TRANSPORT_VERSION,
                agent_tools_interface_version: INTERFACE_VERSION,
                engine_version: env!("CARGO_PKG_VERSION"),
                capabilities: session.capabilities(),
                allow_write: session.policy.allow_write,
                allow_exec: session.policy.allow_exec,
            };
            success_response(
                &request.id,
                serde_json::to_value(result).expect("serializable"),
            )
        }
        "call" => {
            if !*hello {
                return error_response(&request.id, "invalid_state", "hello is required first");
            }
            if request.schema_version.is_some()
                || request.transport_version.is_some()
                || request.agent_tools_interface_version.is_some()
                || request.sdk_version.is_some()
            {
                return error_response(&request.id, "invalid_request", "call shape is invalid");
            }
            let Some(tool) = request.tool else {
                return error_response(&request.id, "invalid_request", "tool is required");
            };
            let arguments = request.arguments.unwrap_or_default();
            match session.call(&tool, &arguments) {
                Ok(output) => {
                    let result = ToolResultV1 {
                        output_tokens: output.original_tokens.saturating_sub(output.saved_tokens),
                        original_tokens: output.original_tokens,
                        saved_tokens: output.saved_tokens,
                        mode: output.mode,
                        changed: output.changed,
                        shell: agent_shell_outcome(output.shell_outcome.as_ref()),
                        content_blocks: serde_json::to_value(
                            output.content_blocks.unwrap_or_default(),
                        )
                        .expect("serializable content blocks"),
                        text: output.text,
                    };
                    success_response(
                        &request.id,
                        serde_json::to_value(result).expect("serializable"),
                    )
                }
                Err(error) => ResponseV1 {
                    id: request.id,
                    ok: false,
                    result: None,
                    error: Some(error),
                },
            }
        }
        "close" => {
            if !*hello
                || request.schema_version.is_some()
                || request.transport_version.is_some()
                || request.agent_tools_interface_version.is_some()
                || request.sdk_version.is_some()
                || request.tool.is_some()
                || request.arguments.is_some()
            {
                return error_response(&request.id, "invalid_request", "close shape is invalid");
            }
            success_response(&request.id, serde_json::json!({ "closed": true }))
        }
        _ => error_response(&request.id, "invalid_request", "unknown operation"),
    }
}

fn success_response(id: &str, result: Value) -> ResponseV1<Value> {
    ResponseV1 {
        id: id.to_string(),
        ok: true,
        result: Some(result),
        error: None,
    }
}

fn agent_shell_outcome(outcome: Option<&ShellOutcome>) -> Option<Value> {
    match outcome {
        Some(ShellOutcome::Exit(code)) => Some(serde_json::json!({ "exitCode": code })),
        Some(other) => other.structured(),
        None => None,
    }
}

fn error_response(id: &str, code: &'static str, message: &str) -> ResponseV1<Value> {
    ResponseV1 {
        id: id.to_string(),
        ok: false,
        result: None,
        error: Some(ErrorV1 {
            code,
            message: message.to_string(),
        }),
    }
}

fn validate_id(id: &str) -> Result<(), ()> {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_ID_BYTES || id.chars().any(char::is_control) {
        Err(())
    } else {
        Ok(())
    }
}

fn parse_args(args: &[String]) -> Result<(PathBuf, PathBuf), &'static str> {
    let mut root = None;
    let mut policy = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        index += 1;
        let value = args.get(index).ok_or("invalid_request")?;
        match flag {
            "--project-root" if root.is_none() => root = Some(PathBuf::from(value)),
            "--policy-file" if policy.is_none() => policy = Some(PathBuf::from(value)),
            _ => return Err("invalid_request"),
        }
        index += 1;
    }
    Ok((
        root.ok_or("invalid_request")?,
        policy.ok_or("invalid_request")?,
    ))
}

fn canonical_root(root: &Path) -> Result<String, &'static str> {
    if crate::core::pathutil::is_broad_or_unsafe_root(root) {
        return Err("unsafe_root");
    }
    let root = fs::canonicalize(root).map_err(|_| "unsafe_root")?;
    if !root.is_dir() || crate::core::pathutil::is_broad_or_unsafe_root(&root) {
        return Err("unsafe_root");
    }
    Ok(root.to_string_lossy().into_owned())
}

fn read_policy(path: &Path) -> Result<PolicyV1, &'static str> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "policy_unavailable")?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_POLICY_BYTES {
        return Err("policy_unavailable");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("policy_permissions");
        }
    }
    let bytes = fs::read(path).map_err(|_| "policy_unavailable")?;
    let policy: PolicyV1 = serde_json::from_slice(&bytes).map_err(|_| "invalid_policy")?;
    let executable_policy_valid = (!policy.allow_exec && policy.allowed_executables.is_empty())
        || (policy.allow_exec
            && !policy.allowed_executables.is_empty()
            && is_canonical_names(&policy.allowed_executables, is_executable_name));
    if policy.schema_version != SCHEMA_VERSION
        || !executable_policy_valid
        || !is_canonical_names(&policy.allowed_env, is_env_name)
        || !(100..=120_000).contains(&policy.max_timeout_ms)
    {
        return Err("invalid_policy");
    }
    Ok(policy)
}

fn is_canonical_names(values: &[String], valid: fn(&str) -> bool) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1]) && values.iter().all(|value| valid(value))
}

fn is_executable_name(value: &str) -> bool {
    !value.is_empty()
        && Path::new(value).file_name().and_then(|name| name.to_str()) == Some(value)
        && value
            .chars()
            .all(|character| character.is_alphanumeric() || "._+-".contains(character))
}

fn is_env_name(value: &str) -> bool {
    let mut characters = value.chars();
    !matches!(
        value.to_ascii_uppercase().as_str(),
        "COMSPEC"
            | "DYLD_INSERT_LIBRARIES"
            | "HOME"
            | "LD_PRELOAD"
            | "PATH"
            | "PATHEXT"
            | "PYTHONPATH"
            | "RUSTC_WRAPPER"
            | "SHELL"
    ) && characters
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
        && characters.all(|character| character.is_alphanumeric() || character == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(write: bool, exec: bool) -> PolicyV1 {
        PolicyV1 {
            schema_version: 1,
            allow_write: write,
            allow_exec: exec,
            allowed_executables: if exec {
                vec!["git".to_string()]
            } else {
                Vec::new()
            },
            allowed_env: Vec::new(),
            max_timeout_ms: 30_000,
        }
    }

    fn session(root: &Path, write: bool, exec: bool) -> Session {
        let root = root.to_string_lossy().into_owned();
        let mut state = SessionState::new();
        state.project_root = Some(root.clone());
        state.shell_cwd = Some(root.clone());
        Session {
            root,
            policy: policy(write, exec),
            registry: build_registry(),
            cache: Arc::new(RwLock::new(SessionCache::new())),
            state: Arc::new(RwLock::new(state)),
            bm25_cache: Arc::new(Mutex::new(None)),
        }
    }

    fn request(id: &str, op: &str) -> RequestV1 {
        RequestV1 {
            id: id.to_string(),
            op: op.to_string(),
            schema_version: None,
            transport_version: None,
            agent_tools_interface_version: None,
            sdk_version: None,
            tool: None,
            arguments: None,
        }
    }

    #[test]
    fn permissions_are_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let session = session(root.path(), false, false);
        assert_eq!(
            session.call("ctx_patch", &Map::new()).err().unwrap().code,
            "permission_denied"
        );
        assert_eq!(
            session.call("ctx_shell", &Map::new()).err().unwrap().code,
            "permission_denied"
        );
        assert_eq!(
            session
                .call("ctx_provider", &Map::new())
                .err()
                .unwrap()
                .code,
            "unsupported_capability"
        );
    }

    #[test]
    fn hello_is_required_and_close_is_explicit() {
        let root = tempfile::tempdir().unwrap();
        let session = session(root.path(), false, false);
        let mut hello = false;
        let mut call = request("1", "call");
        call.tool = Some("ctx_tree".into());
        assert_eq!(
            handle_request(&session, call, &mut hello)
                .error
                .unwrap()
                .code,
            "invalid_state"
        );

        let mut greeting = request("2", "hello");
        greeting.schema_version = Some(1);
        greeting.transport_version = Some(1);
        greeting.agent_tools_interface_version = Some("1.0.0".into());
        greeting.sdk_version = Some("1.1.0".into());
        assert!(handle_request(&session, greeting, &mut hello).ok);
        assert!(handle_request(&session, request("3", "close"), &mut hello).ok);
    }

    #[test]
    fn session_dispatches_real_tools_with_shared_cache() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("sample.txt"), "alpha\nbeta\n").unwrap();
        let session = session(root.path(), false, false);
        let arguments = Map::from_iter([
            ("path".into(), Value::String("sample.txt".into())),
            ("mode".into(), Value::String("full".into())),
        ]);
        let first = session.call("ctx_read", &arguments).unwrap();
        let second = session.call("ctx_read", &arguments).unwrap();
        assert!(!first.text.is_empty());
        assert!(!second.text.is_empty());
        assert!(second.saved_tokens >= first.saved_tokens);
    }

    #[test]
    fn malformed_input_gets_a_framed_error() {
        let root = tempfile::tempdir().unwrap();
        let session = session(root.path(), false, false);
        let mut output = Vec::new();
        serve(&session, "not-json\n".as_bytes(), &mut output).unwrap();
        let value: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "invalid_request");
    }

    #[test]
    fn oversized_frame_is_drained_before_the_next_request() {
        let mut input = vec![b'x'; MAX_REQUEST_BYTES + 1];
        input.extend_from_slice(b"\n{}\n");
        let mut reader = input.as_slice();
        assert!(matches!(
            read_frame(&mut reader).unwrap(),
            Some(InputFrame::TooLarge)
        ));
        let Some(InputFrame::Data(next)) = read_frame(&mut reader).unwrap() else {
            panic!("next frame missing");
        };
        assert_eq!(next, b"{}");
    }

    #[test]
    fn shell_policy_accepts_only_structured_allowed_argv() {
        let root = tempfile::tempdir().unwrap();
        let session = session(root.path(), false, true);
        let arguments = Map::from_iter([
            (
                "argv".into(),
                serde_json::json!(["git", "status", "--short"]),
            ),
            ("cwd".into(), Value::String(".".into())),
            ("env".into(), serde_json::json!({})),
            ("timeout_ms".into(), Value::from(1_000)),
        ]);
        let prepared = session.prepare_shell(&arguments).unwrap();
        assert_eq!(
            prepared["command"],
            crate::shell::platform::join_command(&[
                "git".to_string(),
                "status".to_string(),
                "--short".to_string(),
            ])
        );
        assert!(prepared.get("argv").is_none());

        let mut denied = arguments;
        denied.insert("argv".into(), serde_json::json!(["python", "-V"]));
        assert_eq!(
            session.prepare_shell(&denied).unwrap_err().code,
            "permission_denied"
        );
        for executable in ["/tmp/git", r"C:\tools\git"] {
            denied.insert("argv".into(), serde_json::json!([executable, "status"]));
            assert_eq!(
                session.prepare_shell(&denied).unwrap_err().code,
                "permission_denied"
            );
        }
    }

    #[test]
    fn allowed_executable_resolves_to_absolute_path_outside_project() {
        let root = tempfile::tempdir().unwrap();
        let executable = resolve_allowed_executable("git", root.path().to_str().unwrap()).unwrap();
        assert!(executable.is_absolute());
        assert!(executable.is_file());
        assert!(!executable.starts_with(root.path()));
    }

    #[cfg(unix)]
    #[test]
    fn shell_child_cannot_consume_protocol_stdin() {
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path(), false, true);
        session.policy.allowed_executables = vec!["sh".to_string()];
        let arguments = Map::from_iter([
            (
                "argv".into(),
                serde_json::json!([
                    "sh",
                    "-c",
                    "if read value; then exit 9; else printf closed; fi"
                ]),
            ),
            ("cwd".into(), Value::String(".".into())),
            ("env".into(), serde_json::json!({})),
            ("timeout_ms".into(), Value::from(1_000)),
        ]);
        let result = session.call_shell(&arguments).unwrap();
        assert_eq!(result.shell_outcome, Some(ShellOutcome::Exit(0)));
        assert!(result.text.contains("closed"));
    }

    #[test]
    fn foreground_shell_exit_is_explicit_for_sdk_clients() {
        assert_eq!(
            agent_shell_outcome(Some(&ShellOutcome::Exit(0))),
            Some(serde_json::json!({ "exitCode": 0 }))
        );
        assert_eq!(
            agent_shell_outcome(Some(&ShellOutcome::Exit(7))),
            Some(serde_json::json!({ "exitCode": 7 }))
        );
    }
}
