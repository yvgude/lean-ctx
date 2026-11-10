// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx agents --json`: the coding agents on this machine that LeanCTX
//! knows — installed binary and version, signed-in state, LeanCTX attachment,
//! how to start each as an ACP server and how to resume a session in its own
//! terminal. A local reader (Hive Desktop) builds its agent picker from it.
//!
//! Never reads a credential: sign-in is judged from whether the agent's own
//! credential file EXISTS and from the NAMES of its API-key variables that are
//! set. No network. Each found binary runs once with `--version`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use super::Failure;

/// How long `<binary> --version` may take before the version is `null`.
const VERSION_TIMEOUT: Duration = Duration::from_secs(3);

/// What LeanCTX can do with an agent's ACP side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Acp {
    /// The agent's own binary speaks ACP with these arguments.
    Native(&'static [&'static str]),
    /// An adapter package, started with `npx -y <package>`.
    Adapter(&'static str),
}

/// One agent LeanCTX knows, independent of this machine.
#[derive(Debug)]
pub(crate) struct AgentSpec {
    pub id: &'static str,
    pub name: &'static str,
    /// Binary names, preferred first.
    pub binaries: &'static [&'static str],
    /// The agent's own config directory, relative to home.
    pub config_dir: &'static str,
    /// Editor-registry keys whose MCP config holds the LeanCTX entry.
    pub registry_keys: &'static [&'static str],
    /// MCP config files outside the registry, relative to home.
    pub extra_mcp_files: &'static [&'static str],
    /// The key `lean-ctx setup --agents` / `init --agent` takes, if any.
    pub setup_key: Option<&'static str>,
    /// Credential files, relative to home: only their existence is checked.
    pub credential_files: &'static [&'static str],
    /// The files above are the agent's only credential store (no keychain),
    /// so their absence means signed out.
    pub files_authoritative: bool,
    /// API-key variables the agent honours (names only).
    pub api_key_env: &'static [&'static str],
    pub acp: Acp,
    /// Resume by id in the agent's own terminal UI; `{session_id}` is the
    /// placeholder. Starts with the binary name.
    pub resume: Option<&'static [&'static str]>,
}

/// Every agent, in output order.
pub(crate) const CATALOG: &[AgentSpec] = &[
    AgentSpec {
        id: "claude",
        name: "Claude Code",
        binaries: &["claude"],
        config_dir: ".claude",
        registry_keys: &["claude"],
        extra_mcp_files: &[],
        setup_key: Some("claude"),
        credential_files: &[".claude/.credentials.json"],
        files_authoritative: false,
        api_key_env: &[
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ],
        acp: Acp::Adapter("@agentclientprotocol/claude-agent-acp"),
        resume: Some(&["claude", "--resume", "{session_id}"]),
    },
    AgentSpec {
        id: "codex",
        name: "Codex CLI",
        binaries: &["codex"],
        config_dir: ".codex",
        registry_keys: &["codex"],
        extra_mcp_files: &[],
        setup_key: Some("codex"),
        credential_files: &[".codex/auth.json"],
        files_authoritative: false,
        api_key_env: &["OPENAI_API_KEY"],
        acp: Acp::Adapter("@agentclientprotocol/codex-acp"),
        resume: Some(&["codex", "resume", "{session_id}"]),
    },
    AgentSpec {
        id: "copilot",
        name: "Copilot CLI",
        binaries: &["copilot"],
        config_dir: ".copilot",
        registry_keys: &["copilot"],
        extra_mcp_files: &[],
        setup_key: Some("copilot"),
        credential_files: &[],
        files_authoritative: false,
        api_key_env: &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"],
        acp: Acp::Native(&["--acp"]),
        resume: Some(&["copilot", "--resume={session_id}"]),
    },
    AgentSpec {
        id: "gemini",
        name: "Gemini CLI",
        binaries: &["gemini"],
        config_dir: ".gemini",
        registry_keys: &["gemini"],
        extra_mcp_files: &[],
        setup_key: Some("gemini"),
        credential_files: &[".gemini/oauth_creds.json"],
        files_authoritative: false,
        api_key_env: &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        acp: Acp::Native(&["--experimental-acp"]),
        resume: Some(&["gemini", "--resume", "{session_id}"]),
    },
    AgentSpec {
        id: "cursor",
        name: "Cursor CLI",
        binaries: &["cursor-agent"],
        config_dir: ".cursor",
        registry_keys: &["cursor"],
        extra_mcp_files: &[],
        setup_key: Some("cursor"),
        credential_files: &[],
        files_authoritative: false,
        api_key_env: &["CURSOR_API_KEY", "CURSOR_AUTH_TOKEN"],
        acp: Acp::Native(&["acp"]),
        resume: Some(&["cursor-agent", "--resume", "{session_id}"]),
    },
    AgentSpec {
        id: "opencode",
        name: "OpenCode",
        binaries: &["opencode"],
        config_dir: ".config/opencode",
        registry_keys: &["opencode"],
        extra_mcp_files: &[],
        setup_key: Some("opencode"),
        credential_files: &[".local/share/opencode/auth.json"],
        files_authoritative: false,
        api_key_env: &["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "OPENROUTER_API_KEY"],
        acp: Acp::Native(&["acp"]),
        resume: Some(&["opencode", "--session", "{session_id}"]),
    },
    AgentSpec {
        id: "goose",
        name: "Goose",
        binaries: &["goose"],
        config_dir: ".config/goose",
        registry_keys: &["goose"],
        extra_mcp_files: &[],
        setup_key: Some("goose"),
        credential_files: &[],
        files_authoritative: false,
        api_key_env: &["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "GOOGLE_API_KEY"],
        acp: Acp::Native(&["acp"]),
        resume: Some(&[
            "goose",
            "session",
            "--resume",
            "--session-id",
            "{session_id}",
        ]),
    },
    AgentSpec {
        id: "qwen",
        name: "Qwen Code",
        binaries: &["qwen"],
        config_dir: ".qwen",
        registry_keys: &["qwen"],
        extra_mcp_files: &[],
        setup_key: Some("qwen"),
        credential_files: &[".qwen/oauth_creds.json"],
        files_authoritative: false,
        api_key_env: &["OPENAI_API_KEY", "DASHSCOPE_API_KEY"],
        acp: Acp::Native(&["--acp"]),
        resume: Some(&["qwen", "--resume", "{session_id}"]),
    },
    AgentSpec {
        id: "cline",
        name: "Cline",
        binaries: &["cline"],
        config_dir: ".cline",
        registry_keys: &["cline"],
        extra_mcp_files: &[],
        setup_key: Some("cline"),
        credential_files: &[],
        files_authoritative: false,
        api_key_env: &[],
        acp: Acp::Native(&["--acp"]),
        resume: Some(&["cline", "--id", "{session_id}"]),
    },
    AgentSpec {
        id: "droid",
        name: "Factory Droid",
        binaries: &["droid"],
        config_dir: ".factory",
        registry_keys: &[],
        extra_mcp_files: &[".factory/mcp.json"],
        setup_key: None,
        credential_files: &[],
        files_authoritative: false,
        api_key_env: &["FACTORY_API_KEY"],
        acp: Acp::Native(&["exec", "--output-format", "acp-daemon"]),
        resume: Some(&["droid", "--resume", "{session_id}"]),
    },
    AgentSpec {
        id: "kiro",
        name: "Kiro CLI",
        binaries: &["kiro-cli"],
        config_dir: ".kiro",
        registry_keys: &["kiro"],
        extra_mcp_files: &[],
        setup_key: Some("kiro"),
        credential_files: &[],
        files_authoritative: false,
        api_key_env: &["KIRO_API_KEY"],
        acp: Acp::Native(&["acp"]),
        resume: Some(&["kiro-cli", "chat", "--resume-id", "{session_id}"]),
    },
];

#[derive(Debug, Serialize)]
pub(crate) struct AgentList {
    pub agents: Vec<AgentRow>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct AgentRow {
    pub id: &'static str,
    pub name: &'static str,
    pub installed: bool,
    pub binary: Option<String>,
    pub version: Option<String>,
    pub config_dir: bool,
    pub sign_in: SignIn,
    pub leanctx: Attachment,
    pub acp: AcpLaunch,
    pub resume: Resume,
    pub setup_key: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct SignIn {
    /// `signed_in` | `signed_out` | `unknown`.
    pub state: &'static str,
    /// `credentials_file` | `api_key_env` | `null`.
    pub evidence: Option<&'static str>,
    pub api_key_env: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct Attachment {
    /// `configured` | `missing` | `unsupported`.
    pub mcp: &'static str,
    /// `installed` | `missing` | `unsupported`.
    pub hooks: &'static str,
    /// `wired` | `not_wired` | `unsupported`.
    pub proxy: &'static str,
    /// `governor` | `replace` | `hybrid` | `mcp` | `null` (not attached).
    pub mode: Option<&'static str>,
    /// What the gateway sees of this agent (`core::host_coverage`), or
    /// `null` when not attached.
    pub coverage: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct AcpLaunch {
    /// `native` | `adapter`.
    pub support: &'static str,
    pub command: Vec<String>,
    pub package: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct Resume {
    pub supported: bool,
    pub command: Option<Vec<String>>,
}

/// What this machine says about one agent; everything [`row`] needs, so the
/// shape is testable without touching the real home.
#[derive(Debug, Default, Clone)]
pub(crate) struct Probe {
    pub binary: Option<PathBuf>,
    pub version: Option<String>,
    pub config_dir: bool,
    pub credential_file: bool,
    /// The agent's own config names a signed-in account ([`account_marker`]).
    pub account_config: bool,
    pub api_key_env: Vec<&'static str>,
    pub mcp_configured: bool,
    /// LeanCTX has a hook integration for this agent.
    pub hooks_supported: bool,
    /// The installed hook mode, `governor` included.
    pub hook_mode: Option<&'static str>,
    pub proxy_supported: bool,
    pub proxy_wired: bool,
    pub coverage: Option<&'static str>,
}

/// The config file (relative to home) and top-level key whose presence means
/// the agent has a signed-in account, for agents that keep the credential
/// itself in the system keychain. Only the key's presence is read: Claude
/// Code's `~/.claude.json` `oauthAccount` holds account metadata (no token;
/// the token is in the macOS keychain, which is never touched).
pub(crate) fn account_marker(id: &str) -> Option<(&'static str, &'static str)> {
    match id {
        "claude" => Some((".claude.json", "oauthAccount")),
        _ => None,
    }
}

/// Whether `text` (a JSON config) has a non-empty object at `key`.
pub(crate) fn names_account(text: &str, key: &str) -> bool {
    lean_ctx::core::jsonc::parse_jsonc(text)
        .ok()
        .and_then(|doc| doc.get(key).cloned())
        .and_then(|v| v.as_object().map(|o| !o.is_empty()))
        .unwrap_or(false)
}

/// The row of `spec` from what `probe` found.
pub(crate) fn row(spec: &'static AgentSpec, probe: Probe) -> AgentRow {
    let sign_in = if probe.credential_file {
        SignIn {
            state: "signed_in",
            evidence: Some("credentials_file"),
            api_key_env: probe.api_key_env,
        }
    } else if probe.account_config {
        SignIn {
            state: "signed_in",
            evidence: Some("account_config"),
            api_key_env: probe.api_key_env,
        }
    } else if !probe.api_key_env.is_empty() {
        SignIn {
            state: "signed_in",
            evidence: Some("api_key_env"),
            api_key_env: probe.api_key_env,
        }
    } else {
        SignIn {
            state: if spec.files_authoritative && probe.config_dir {
                "signed_out"
            } else {
                "unknown"
            },
            evidence: None,
            api_key_env: probe.api_key_env,
        }
    };
    let mcp = if spec.registry_keys.is_empty() && spec.extra_mcp_files.is_empty() {
        "unsupported"
    } else if probe.mcp_configured {
        "configured"
    } else {
        "missing"
    };
    let hooks = match (probe.hooks_supported, probe.hook_mode) {
        (_, Some(_)) => "installed",
        (true, None) => "missing",
        (false, None) => "unsupported",
    };
    let proxy = match (probe.proxy_supported, probe.proxy_wired) {
        (false, _) => "unsupported",
        (true, true) => "wired",
        (true, false) => "not_wired",
    };
    let mode = probe
        .hook_mode
        .or_else(|| probe.mcp_configured.then_some("mcp"));
    let binary_name = probe
        .binary
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| spec.binaries[0].to_owned());
    let acp = match spec.acp {
        Acp::Native(args) => AcpLaunch {
            support: "native",
            command: std::iter::once(binary_name)
                .chain(args.iter().map(|a| (*a).to_owned()))
                .collect(),
            package: None,
        },
        Acp::Adapter(package) => AcpLaunch {
            support: "adapter",
            command: vec!["npx".into(), "-y".into(), package.into()],
            package: Some(package),
        },
    };
    AgentRow {
        id: spec.id,
        name: spec.name,
        installed: probe.binary.is_some(),
        binary: probe.binary.map(|p| p.to_string_lossy().into_owned()),
        version: probe.version,
        config_dir: probe.config_dir,
        sign_in,
        leanctx: Attachment {
            mcp,
            hooks,
            proxy,
            mode,
            coverage: mode.and(probe.coverage),
        },
        acp,
        resume: Resume {
            supported: spec.resume.is_some(),
            command: spec
                .resume
                .map(|argv| argv.iter().map(|a| (*a).to_owned()).collect()),
        },
        setup_key: spec.setup_key,
    }
}

/// The version in `--version` output: the first token that starts with a
/// digit and contains a dot (`codex-cli 0.98.0` → `0.98.0`).
pub(crate) fn parse_version(output: &str) -> Option<String> {
    output
        .split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')')
        .map(|t| t.trim_start_matches('v'))
        .find(|t| t.starts_with(|c: char| c.is_ascii_digit()) && t.contains('.'))
        .map(|t| {
            t.trim_end_matches(|c: char| !c.is_ascii_alphanumeric())
                .to_owned()
        })
}

/// Directories searched for an agent binary: `PATH` first, then the usual
/// install locations a GUI app's short `PATH` misses.
pub(crate) fn search_dirs(home: &Path, path_var: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    // Only absolute entries: an empty or relative entry (`""`, `.`) would
    // resolve against the reader's cwd and run a project's `./codex`.
    let mut dirs: Vec<PathBuf> = path_var
        .map(|p| {
            std::env::split_paths(p)
                .filter(|d| d.is_absolute())
                .collect()
        })
        .unwrap_or_default();
    for extra in [
        home.join(".local/bin"),
        home.join(".claude/local"),
        home.join(".npm-global/bin"),
        home.join(".bun/bin"),
        home.join(".cargo/bin"),
        home.join(".opencode/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ] {
        if !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }
    dirs
}

/// The first executable named one of `names` in `dirs`.
pub(crate) fn find_binary(names: &[&str], dirs: &[PathBuf]) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(str::to_ascii_lowercase)
            .collect()
    } else {
        vec![String::new()]
    };
    for name in names {
        for dir in dirs {
            for ext in &exts {
                let candidate = dir.join(format!("{name}{ext}"));
                if is_executable(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The most `--version` output that is read.
const VERSION_OUTPUT_CAP: u64 = 8 * 1024;

/// `<binary> --version`, `None` on failure or after [`VERSION_TIMEOUT`].
///
/// stdout is read in its own thread under the same deadline (a wrapper's
/// child can keep the pipe open after the wrapper exits), capped at
/// [`VERSION_OUTPUT_CAP`]; on timeout the whole process group is killed.
fn binary_version(binary: &Path) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(binary);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.take(VERSION_OUTPUT_CAP).read_to_end(&mut out);
        let _ = tx.send(out);
    });
    let output = rx.recv_timeout(VERSION_TIMEOUT).ok();
    kill_tree(&mut child);
    parse_version(&String::from_utf8_lossy(&output?))
}

/// Kill (Unix: the process group of) `child`, then reap it. The group is
/// signalled before the child is reaped, so its id cannot have been reused;
/// this also ends a grandchild still holding the pipe.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    signal_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

/// SIGKILL to the process group led by `pid` (spawned with `process_group(0)`).
#[cfg(unix)]
fn signal_group(pid: u32) {
    // `group_target` refuses 0 (kill(-0) = our own group), 1, ids above
    // i32::MAX and the group this process belongs to.
    if let Ok(pgid) = lean_ctx::ipc::process::group_target(pid) {
        // SAFETY: kill(2) with a negative pid signals that process group;
        // `pgid` is positive and the group was created for this child alone.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

/// Whether the MCP config `text` (named `file_name`) registers a server
/// called `lean-ctx`. Structural, not a substring: `~/.claude.json` names
/// every project path, and a project called `lean-ctx` is not a server.
pub(crate) fn registers_lean_ctx(file_name: &str, text: &str) -> bool {
    let ext = Path::new(file_name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if ext == "toml" {
        return text
            .lines()
            .any(|l| l.trim().starts_with("[mcp_servers.lean-ctx]"));
    }
    if ext == "yaml" || ext == "yml" {
        return text
            .lines()
            .any(|l| l.starts_with([' ', '\t']) && l.trim() == "lean-ctx:");
    }
    let Ok(doc) = lean_ctx::core::jsonc::parse_jsonc(text) else {
        return false;
    };
    let roots = ["mcpServers", "servers", "mcp", "context_servers"];
    let named = |v: &serde_json::Value| v.get("lean-ctx").is_some();
    roots.iter().filter_map(|r| doc.get(*r)).any(|v| {
        named(v)
            || v.get("servers").is_some_and(named)
            || v.as_array().is_some_and(|a| {
                a.iter()
                    .any(|s| s.get("name").and_then(|n| n.as_str()) == Some("lean-ctx"))
            })
    }) || doc.get("amp.mcpServers").is_some_and(named)
}

fn mcp_configured(
    spec: &AgentSpec,
    home: &Path,
    targets: &[lean_ctx::core::editor_registry::EditorTarget],
) -> bool {
    let has = |p: &Path| {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        std::fs::read_to_string(p).is_ok_and(|c| registers_lean_ctx(&name, &c))
    };
    targets
        .iter()
        .filter(|t| spec.registry_keys.contains(&t.agent_key.as_str()))
        .any(|t| has(&t.config_path))
        || spec.extra_mcp_files.iter().any(|f| has(&home.join(f)))
}

/// The hook mode LeanCTX installed for `spec`, `governor` included.
fn hook_mode(spec: &AgentSpec, home: &Path) -> Option<&'static str> {
    let key = spec.setup_key?;
    if lean_ctx::core::governor_hosts::is_installed(key, home) {
        return Some("governor");
    }
    lean_ctx_hooks::hooks::installed_hook_mode(key, home).map(|m| match m {
        lean_ctx_hooks::hooks::HookMode::Replace => "replace",
        lean_ctx_hooks::hooks::HookMode::Hybrid => "hybrid",
        lean_ctx_hooks::hooks::HookMode::Mcp => "mcp",
    })
}

fn hooks_supported(spec: &AgentSpec) -> bool {
    spec.setup_key
        .is_some_and(lean_ctx_hooks::hooks::has_tool_hooks)
}

fn coverage(spec: &AgentSpec, mode: Option<&'static str>, wired: bool) -> Option<&'static str> {
    use lean_ctx_hooks::hooks::HookMode;
    let key = spec.setup_key.unwrap_or(spec.id);
    let installed = match mode? {
        "replace" => HookMode::Replace,
        "mcp" => HookMode::Mcp,
        _ => HookMode::Hybrid,
    };
    Some(
        lean_ctx::core::host_coverage::classify(key, Some(installed), wired)
            .level
            .as_str(),
    )
}

/// Probe every catalog agent on this machine (versions in parallel).
pub(crate) fn list(home: &Path) -> AgentList {
    let targets = lean_ctx::core::editor_registry::build_targets(home);
    let dirs = search_dirs(home, std::env::var_os("PATH").as_deref());
    let routed = crate::proxy_setup::egress_routed_hosts(home);
    let set_env = |name: &&'static str| std::env::var_os(name).is_some_and(|v| !v.is_empty());

    let probes: Vec<Probe> = CATALOG
        .iter()
        .map(|spec| {
            let mode = hook_mode(spec, home);
            let proxy_supported = lean_ctx::core::host_coverage::egress_routable(spec.id);
            let proxy_wired = routed.contains(&spec.id);
            Probe {
                binary: find_binary(spec.binaries, &dirs),
                version: None,
                config_dir: home.join(spec.config_dir).is_dir(),
                credential_file: spec.credential_files.iter().any(|f| home.join(f).is_file()),
                account_config: account_marker(spec.id).is_some_and(|(file, key)| {
                    std::fs::read_to_string(home.join(file)).is_ok_and(|t| names_account(&t, key))
                }),
                api_key_env: spec.api_key_env.iter().copied().filter(set_env).collect(),
                mcp_configured: mcp_configured(spec, home, &targets),
                hooks_supported: hooks_supported(spec),
                hook_mode: mode,
                proxy_supported,
                proxy_wired,
                coverage: None,
            }
        })
        .collect();

    let versions: Vec<Option<String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = probes
            .iter()
            .map(|p| {
                let binary = p.binary.clone();
                scope.spawn(move || binary.as_deref().and_then(binary_version))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().ok().flatten())
            .collect()
    });

    let agents = CATALOG
        .iter()
        .zip(probes)
        .zip(versions)
        .map(|((spec, mut probe), version)| {
            probe.version = version;
            let mode = probe
                .hook_mode
                .or_else(|| probe.mcp_configured.then_some("mcp"));
            probe.coverage = coverage(spec, mode, probe.proxy_wired);
            row(spec, probe)
        })
        .collect();
    AgentList { agents }
}

const HELP: &str = "\
Usage: lean-ctx agents [--json]

The coding agents on this machine that LeanCTX knows: installed version,
sign-in (from the presence of the agent's own credential file and API-key
variables; no credential is read), LeanCTX attachment (MCP, hooks, proxy,
mode), the ACP launch command and session resume.

  --json   one Local Read document (contract local-read/1, kind agents.list)
";

fn home() -> Result<PathBuf, Failure> {
    lean_ctx::core::home::resolve_home_dir()
        .ok_or_else(|| Failure::new(super::Code::Unavailable, "home directory not found"))
}

/// The `agents.list` document (released by `kinds::answer`, for the CLI
/// and `GET /v1/local/agents`).
pub(crate) fn build(_args: &[String]) -> Result<AgentList, Failure> {
    Ok(list(&home()?))
}

/// `lean-ctx agents [list] [--json]`.
pub(crate) fn cmd(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return;
    }
    // `list` is the verb capabilities names (`agents: [list]`).
    let args: Vec<String> = match args.first().map(String::as_str) {
        Some("list") => args[1..].to_vec(),
        _ => args.to_vec(),
    };
    let args = args.as_slice();
    if let Some(bad) = super::positionals(args).first() {
        let failure = Failure::invalid(format!("unknown argument: {bad}"));
        if super::wants_json(args) {
            super::fail(&failure);
        }
        eprintln!("{}\n\n{HELP}", failure.message);
        std::process::exit(2);
    }
    if super::wants_json(args) {
        super::kinds::run("agents.list", args);
        return;
    }
    let list = match home() {
        Ok(home) => list(&home),
        Err(failure) => {
            eprintln!("{}", failure.message);
            std::process::exit(1);
        }
    };
    for a in &list.agents {
        let installed = match (&a.binary, &a.version) {
            (Some(_), Some(v)) => v.clone(),
            (Some(_), None) => "installed".into(),
            (None, _) => "not installed".into(),
        };
        let attached = a.leanctx.mode.unwrap_or("not attached");
        println!(
            "{:<16} {:<16} sign-in: {:<11} LeanCTX: {:<13} ACP: {}",
            a.name, installed, a.sign_in.state, attached, a.acp.support
        );
    }
}
