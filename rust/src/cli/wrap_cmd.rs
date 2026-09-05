//! Agent-specific proxy setup for `lean-ctx wrap` and `lean-ctx unwrap`.
//!
//! This command deliberately snapshots only files it owns before changing
//! them.  `unwrap` can therefore restore an agent's configuration exactly as
//! it was, instead of trying to reconstruct a user's previous endpoint.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

const DEFAULT_PROXY_PORT: u16 = 4444;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WrapAgent {
    Claude,
    Codex,
    Cursor,
    Windsurf,
    Cline,
    Grok,
    Aider,
    Copilot,
}

impl WrapAgent {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Windsurf => "windsurf",
            Self::Cline => "cline",
            Self::Grok => "grok",
            Self::Aider => "aider",
            Self::Copilot => "copilot",
        }
    }

    const fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Cursor => "Cursor",
            Self::Windsurf => "Windsurf",
            Self::Cline => "Cline",
            Self::Grok => "Grok",
            Self::Aider => "Aider",
            Self::Copilot => "Copilot CLI",
        }
    }

    fn environment(
        self,
        port: u16,
        proxy_enabled: bool,
        anthropic_redirect_allowed: bool,
    ) -> Vec<(&'static str, String)> {
        let base = format!("http://127.0.0.1:{port}");
        let mut variables = match self {
            Self::Claude if proxy_enabled => vec![("ANTHROPIC_BASE_URL", base)],
            Self::Claude | Self::Cursor => Vec::new(),
            Self::Codex => vec![("OPENAI_BASE_URL", format!("{base}/v1"))],
            Self::Windsurf | Self::Cline => vec![
                ("OPENAI_BASE_URL", format!("{base}/v1")),
                ("ANTHROPIC_BASE_URL", base),
            ],
            Self::Grok => vec![("GROK_MODELS_BASE_URL", format!("{base}/v1"))],
            Self::Aider => vec![
                ("OPENAI_API_BASE", format!("{base}/v1")),
                ("ANTHROPIC_BASE_URL", base),
            ],
            Self::Copilot => vec![("COPILOT_PROVIDER_BASE_URL", format!("{base}/v1"))],
        };
        if !anthropic_redirect_allowed {
            variables.retain(|(name, _)| *name != "ANTHROPIC_BASE_URL");
        }
        variables
    }
}

impl FromStr for WrapAgent {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "claude" | "claude-code" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "cursor" => Ok(Self::Cursor),
            "windsurf" => Ok(Self::Windsurf),
            "cline" => Ok(Self::Cline),
            "grok" => Ok(Self::Grok),
            "aider" => Ok(Self::Aider),
            "copilot" | "copilot-cli" => Ok(Self::Copilot),
            // GH #1520: agents that ARE fully supported via `init`/`setup` but
            // have no proxy-wrap profile must point users at the working path
            // instead of a bare "unsupported agent".
            other if crate::hooks::is_supported_agent(other) => Err(format!(
                "'{value}' has no proxy wrap profile, but it IS supported — run:\n  \
                 lean-ctx init --agent {value}\n\
                 (installs MCP server, hooks and rules for {value})"
            )),
            _ => Err(format!("unsupported agent '{value}'")),
        }
    }
}

/// Parsed arguments for `lean-ctx wrap`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WrapArgs {
    pub(crate) agent: WrapAgent,
    pub(crate) port: u16,
    pub(crate) unwrap: bool,
}

impl WrapArgs {
    fn parse(args: &[String], force_unwrap: bool) -> Result<Self, String> {
        let mut agent = None;
        let mut port = DEFAULT_PROXY_PORT;
        let mut unwrap = force_unwrap;
        let mut index = 0;

        while index < args.len() {
            let arg = &args[index];
            match arg.as_str() {
                "--unwrap" => unwrap = true,
                "--port" | "-p" => {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or_else(|| format!("{arg} requires a port number"))?;
                    port = parse_port(value)?;
                }
                _ if arg.starts_with("--port=") => port = parse_port(&arg[7..])?,
                _ if arg.starts_with('-') => return Err(format!("unknown flag '{arg}'")),
                _ if agent.is_none() => agent = Some(arg.parse()?),
                _ => return Err(format!("unexpected argument '{arg}'")),
            }
            index += 1;
        }

        let agent = agent.ok_or_else(|| "missing agent".to_string())?;
        Ok(Self {
            agent,
            port,
            unwrap,
        })
    }
}

fn parse_port(value: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| format!("invalid port '{value}'"))
}

#[derive(Debug, Serialize, Deserialize)]
struct BackupManifest {
    agent: String,
    files: Vec<BackupFile>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BackupFile {
    path: PathBuf,
    existed: bool,
    backup: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WrapTransport {
    Proxy,
    ClaudeSubscription { repair_owned_redirect: bool },
    ClaudeExistingEndpoint,
}

impl WrapTransport {
    const fn uses_proxy(self) -> bool {
        matches!(self, Self::Proxy)
    }
}

/// Dispatch `lean-ctx wrap <agent> [--port N] [--unwrap]`.
pub(crate) fn cmd_wrap(args: &[String]) -> i32 {
    if wants_help(args) {
        print_help();
        return 0;
    }

    let parsed = match WrapArgs::parse(args, false) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("wrap: {error}");
            print_usage();
            return 2;
        }
    };

    if parsed.unwrap {
        finish_command("wrap", unwrap_agent(parsed.agent))
    } else {
        finish_command("wrap", wrap_agent(&parsed))
    }
}

/// Dispatch the backwards-compatible `lean-ctx unwrap <agent>` spelling.
pub(crate) fn cmd_unwrap(args: &[String]) -> i32 {
    if wants_help(args) {
        print_unwrap_help();
        return 0;
    }

    let parsed = match WrapArgs::parse(args, true) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("unwrap: {error}");
            print_unwrap_usage();
            return 2;
        }
    };
    finish_command("unwrap", unwrap_agent(parsed.agent))
}

fn finish_command(name: &str, result: Result<(), String>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{name}: {error}");
            1
        }
    }
}

fn wrap_agent(args: &WrapArgs) -> Result<(), String> {
    let home = dirs::home_dir().ok_or_else(|| "cannot determine the home directory".to_string())?;
    wrap_agent_at_home(args, &home, ensure_proxy_running)
}

fn wrap_agent_at_home(
    args: &WrapArgs,
    home: &Path,
    ensure_proxy: impl FnOnce(u16) -> Result<(), String>,
) -> Result<(), String> {
    let transport = wrap_transport(args.agent, args.port, home)?;
    let proxy_enabled = transport.uses_proxy();

    let mut files = agent_config_paths(args.agent, home);
    files.extend(existing_shell_profiles(home));
    let files: BTreeSet<_> = files.into_iter().collect();

    if proxy_enabled {
        ensure_proxy(args.port)?;
        crate::core::config::Config::update_global(|config| config.proxy_enabled = Some(true))
            .map_err(|error| format!("could not persist proxy state: {error}"))?;
    }

    save_backup_manifest(args.agent, &files)
        .map_err(|error| format!("could not create backups: {error}"))?;

    configure_agent_endpoint(args.agent, args.port, home, transport)
        .map_err(|error| format!("could not configure {}: {error}", args.agent.display_name()))?;

    register_mcp(args.agent, home)
        .map_err(|error| format!("could not register MCP server: {error}"))?;

    install_shell_exports(args.agent, args.port, home, proxy_enabled)
        .map_err(|error| format!("could not persist environment variables: {error}"))?;

    print_wrap_success(args.agent, args.port, transport);
    Ok(())
}

fn wrap_transport(agent: WrapAgent, port: u16, home: &Path) -> Result<WrapTransport, String> {
    if agent != WrapAgent::Claude {
        return Ok(WrapTransport::Proxy);
    }

    let current_url = claude_base_url(home)?;
    let expected_url = format!("http://127.0.0.1:{port}");
    let previously_wrapped = manifest_path(agent)?.exists();
    Ok(classify_claude_transport(
        crate::proxy_setup::anthropic_api_key_available(home),
        current_url.as_deref(),
        &expected_url,
        previously_wrapped,
    ))
}

fn classify_claude_transport(
    api_key_available: bool,
    current_url: Option<&str>,
    expected_url: &str,
    previously_wrapped: bool,
) -> WrapTransport {
    let owns_current = previously_wrapped && current_url == Some(expected_url);

    if !api_key_available {
        if current_url.is_some() && !owns_current {
            return WrapTransport::ClaudeExistingEndpoint;
        }
        return WrapTransport::ClaudeSubscription {
            repair_owned_redirect: owns_current,
        };
    }

    if current_url.is_none() || owns_current {
        WrapTransport::Proxy
    } else {
        WrapTransport::ClaudeExistingEndpoint
    }
}

fn claude_base_url(home: &Path) -> Result<Option<String>, String> {
    let path = crate::core::editor_registry::claude_state_dir(home).join("settings.json");
    let content = match std::fs::read_to_string(&path) {
        Ok(content) if !content.trim().is_empty() => content,
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let document = crate::core::jsonc::parse_jsonc(&content)
        .map_err(|error| format!("{} contains invalid JSON: {error}", path.display()))?;
    let Some(env) = document.get("env") else {
        return Ok(None);
    };
    let env = env
        .as_object()
        .ok_or_else(|| format!("{}.env must be a JSON object", path.display()))?;
    match env.get("ANTHROPIC_BASE_URL") {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| Some(value.to_string()))
            .ok_or_else(|| format!("{}.env.ANTHROPIC_BASE_URL must be a string", path.display())),
    }
}

fn unwrap_agent(agent: WrapAgent) -> Result<(), String> {
    match load_backup_manifest(agent) {
        Ok(Some(manifest)) => match restore_backup_manifest(&manifest) {
            Ok(()) => {
                remove_backup_manifest(agent);
                println!("✓ lean-ctx unwrapped {}.", agent.display_name());
                println!(
                    "  Restart {} to use its restored configuration.",
                    agent.display_name()
                );
                Ok(())
            }
            Err(error) => Err(format!("could not restore backups: {error}")),
        },
        Ok(None) => {
            // A manually removed manifest should not strand an integration.  This
            // fallback only removes entries that identify themselves as lean-ctx.
            remove_owned_integration(agent)?;
            println!(
                "✓ Removed lean-ctx integration for {}.",
                agent.display_name()
            );
            Ok(())
        }
        Err(error) => Err(format!("could not read backups: {error}")),
    }
}

fn ensure_proxy_running(port: u16) -> Result<(), String> {
    if crate::proxy_setup::is_proxy_reachable(port) {
        return Ok(());
    }

    let loaded = crate::proxy_autostart::is_loaded();
    if loaded {
        println!("  Proxy LaunchAgent is loaded but port {port} is unavailable; refreshing it...");
    } else {
        println!("  Proxy LaunchAgent is not loaded; starting it on port {port}...");
    }

    // `install` writes the requested port into the LaunchAgent/systemd unit and
    // loads it.  This is necessary when a previously installed service used a
    // different port.
    if !crate::proxy_autostart::install(port, true) {
        return Err(format!("could not load the proxy service on port {port}"));
    }

    for _ in 0..20 {
        if crate::proxy_setup::is_proxy_reachable(port) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    Err(format!(
        "proxy did not become reachable at http://127.0.0.1:{port}/health"
    ))
}

fn agent_config_paths(agent: WrapAgent, home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let targets = crate::core::editor_registry::build_targets(home);
    paths.extend(
        targets
            .iter()
            .filter(|target| target.agent_key == agent.as_str())
            .map(|target| target.config_path.clone()),
    );

    if agent == WrapAgent::Claude {
        paths.push(crate::core::editor_registry::claude_state_dir(home).join("settings.json"));
    }
    paths
}

fn configure_agent_endpoint(
    agent: WrapAgent,
    port: u16,
    home: &Path,
    transport: WrapTransport,
) -> Result<(), String> {
    match (agent, transport) {
        (WrapAgent::Claude, WrapTransport::Proxy) => {
            let path = crate::core::editor_registry::claude_state_dir(home).join("settings.json");
            set_json_env(
                &path,
                "ANTHROPIC_BASE_URL",
                &format!("http://127.0.0.1:{port}"),
            )
        }
        (
            WrapAgent::Claude,
            WrapTransport::ClaudeSubscription {
                repair_owned_redirect: true,
            },
        ) => crate::proxy_setup::repair_claude_subscription_env(
            home,
            &format!("http://127.0.0.1:{port}"),
        ),
        (WrapAgent::Codex, _) => {
            let path = crate::core::home::resolve_codex_config_path()
                .unwrap_or_else(|| home.join(".codex/config.toml"));
            set_codex_base_url(&path, &format!("http://127.0.0.1:{port}/v1"))
        }
        (
            WrapAgent::Claude
            | WrapAgent::Cursor
            | WrapAgent::Windsurf
            | WrapAgent::Cline
            | WrapAgent::Grok
            | WrapAgent::Aider
            | WrapAgent::Copilot,
            _,
        ) => Ok(()),
    }
}

fn register_mcp(agent: WrapAgent, home: &Path) -> Result<(), String> {
    let targets = crate::core::editor_registry::build_targets(home);
    let target = targets
        .iter()
        .find(|target| target.agent_key == agent.as_str())
        .ok_or_else(|| format!("no MCP configuration is known for {}", agent.display_name()))?;
    let binary = crate::core::portable_binary::resolve_portable_binary();

    // Claude's official `mcp add-json` may update state outside ~/.claude.json.
    // This command promises a reversible file edit, so force the registry writer
    // down its deterministic config-file path.
    let _quiet = QuietEnvGuard::set();
    crate::core::editor_registry::write_config_with_options(
        target,
        &binary,
        crate::core::editor_registry::WriteOptions {
            overwrite_invalid: false,
        },
    )
    .map(|_| ())
}

struct QuietEnvGuard {
    previous: Option<OsString>,
}

impl QuietEnvGuard {
    fn set() -> Self {
        let previous = std::env::var_os("LEAN_CTX_QUIET");
        // SAFETY: this short-lived CLI command mutates its own environment before
        // invoking synchronous configuration code; no spawned process inherits it.
        unsafe { std::env::set_var("LEAN_CTX_QUIET", "1") };
        Self { previous }
    }
}

impl Drop for QuietEnvGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => {
                // SAFETY: restores the exact value captured by `set` above.
                unsafe { std::env::set_var("LEAN_CTX_QUIET", value) };
            }
            None => {
                // SAFETY: restores the absence of the variable captured by `set`.
                unsafe { std::env::remove_var("LEAN_CTX_QUIET") };
            }
        }
    }
}

fn set_json_env(path: &Path, key: &str, value: &str) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut document = if existing.trim().is_empty() {
        serde_json::json!({})
    } else {
        crate::core::jsonc::parse_jsonc(&existing)
            .map_err(|error| format!("{} contains invalid JSON: {error}", path.display()))?
    };

    let object = document
        .as_object_mut()
        .ok_or_else(|| format!("{} must contain a JSON object", path.display()))?;
    let env = object
        .entry("env")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("{}.env must be a JSON object", path.display()))?;
    env.insert(
        key.to_string(),
        serde_json::Value::String(value.to_string()),
    );

    let rendered = serde_json::to_string_pretty(&document).map_err(|error| error.to_string())?;
    write_config(path, &(rendered + "\n"))
}

fn set_codex_base_url(path: &Path, value: &str) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut document = existing
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("{} contains invalid TOML: {error}", path.display()))?;
    document["openai_base_url"] = toml_edit::value(value);
    write_config(path, &document.to_string())
}

fn existing_shell_profiles(home: &Path) -> Vec<PathBuf> {
    [
        home.join(".zshrc"),
        home.join(".bashrc"),
        home.join(".config/fish/config.fish"),
    ]
    .into_iter()
    .filter(|path| path.exists())
    .collect()
}

fn install_shell_exports(
    agent: WrapAgent,
    port: u16,
    home: &Path,
    proxy_enabled: bool,
) -> Result<(), String> {
    let variables = agent.environment(
        port,
        proxy_enabled,
        crate::proxy_setup::anthropic_api_key_available(home),
    );
    if variables.is_empty() {
        for profile in existing_shell_profiles(home) {
            remove_marked_block(&profile, agent)?;
        }
        return Ok(());
    }

    // Make these available to future CLI sessions.  We intentionally modify only
    // existing profiles; creating a shell startup file is a surprising side effect.
    for profile in existing_shell_profiles(home) {
        let fish = profile
            .file_name()
            .is_some_and(|name| name == "config.fish");
        let block = shell_block(agent, &variables, fish);
        let existing = std::fs::read_to_string(&profile).map_err(|error| error.to_string())?;
        let rendered = replace_marked_block(&existing, agent, &block);
        if rendered != existing {
            write_config(&profile, &rendered)?;
        }
    }
    Ok(())
}

fn shell_block(agent: WrapAgent, variables: &[(&str, String)], fish: bool) -> String {
    let (start, end) = shell_markers(agent);
    let exports = variables
        .iter()
        .map(|(key, value)| {
            if fish {
                format!("set -gx {key} \"{value}\"")
            } else {
                format!("export {key}=\"{value}\"")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{start}\n{exports}\n{end}\n")
}

fn shell_markers(agent: WrapAgent) -> (String, String) {
    (
        format!("# >>> lean-ctx wrap {} >>>", agent.as_str()),
        format!("# <<< lean-ctx wrap {} <<<", agent.as_str()),
    )
}

fn replace_marked_block(existing: &str, agent: WrapAgent, replacement: &str) -> String {
    let (start, end) = shell_markers(agent);
    let Some(start_at) = existing.find(&start) else {
        let separator = if existing.is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        return format!("{existing}{separator}{replacement}");
    };
    let after_start = start_at + start.len();
    let Some(end_offset) = existing[after_start..].find(&end) else {
        return existing.to_string();
    };
    let mut end_at = after_start + end_offset + end.len();
    if existing[end_at..].starts_with('\n') {
        end_at += 1;
    }
    format!(
        "{}{}{}",
        &existing[..start_at],
        replacement,
        &existing[end_at..]
    )
}

fn write_config(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    crate::config_io::write_atomic_with_backup(path, content)
}

fn backup_root() -> Result<PathBuf, String> {
    Ok(crate::core::paths::state_dir()?.join("wrap"))
}

fn manifest_path(agent: WrapAgent) -> Result<PathBuf, String> {
    Ok(backup_root()?.join(agent.as_str()).join("manifest.json"))
}

fn save_backup_manifest(agent: WrapAgent, paths: &BTreeSet<PathBuf>) -> Result<(), String> {
    let manifest_path = manifest_path(agent)?;
    if manifest_path.exists() {
        return Ok(());
    }

    let Some(snapshot_dir) = manifest_path.parent() else {
        return Err("invalid backup manifest path".to_string());
    };
    std::fs::create_dir_all(snapshot_dir).map_err(|error| error.to_string())?;

    let mut files = Vec::with_capacity(paths.len());
    for (index, path) in paths.iter().enumerate() {
        let existed = path.exists();
        let backup = if existed {
            let backup = snapshot_dir.join(format!("{index}.bak"));
            std::fs::copy(path, &backup)
                .map_err(|error| format!("backup {}: {error}", path.display()))?;
            Some(backup)
        } else {
            None
        };
        files.push(BackupFile {
            path: path.clone(),
            existed,
            backup,
        });
    }

    let manifest = BackupManifest {
        agent: agent.as_str().to_string(),
        files,
    };
    let rendered = serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?;
    crate::config_io::write_atomic(&manifest_path, &rendered)
}

fn load_backup_manifest(agent: WrapAgent) -> Result<Option<BackupManifest>, String> {
    let path = manifest_path(agent)?;
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str(&content)
        .map(Some)
        .map_err(|error| format!("invalid backup manifest: {error}"))
}

fn restore_backup_manifest(manifest: &BackupManifest) -> Result<(), String> {
    for file in &manifest.files {
        if file.existed {
            let backup = file
                .backup
                .as_ref()
                .ok_or_else(|| format!("missing backup record for {}", file.path.display()))?;
            if let Some(parent) = file.path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            std::fs::copy(backup, &file.path)
                .map_err(|error| format!("restore {}: {error}", file.path.display()))?;
        } else if file.path.exists() {
            std::fs::remove_file(&file.path)
                .map_err(|error| format!("remove {}: {error}", file.path.display()))?;
        }
    }
    Ok(())
}

fn remove_backup_manifest(agent: WrapAgent) {
    if let Ok(root) = backup_root() {
        let _ = std::fs::remove_dir_all(root.join(agent.as_str()));
    }
}

fn remove_owned_integration(agent: WrapAgent) -> Result<(), String> {
    let Some(home) = dirs::home_dir() else {
        return Err("cannot determine the home directory".to_string());
    };
    let targets = crate::core::editor_registry::build_targets(&home);
    if let Some(target) = targets
        .iter()
        .find(|target| target.agent_key == agent.as_str())
    {
        crate::core::editor_registry::remove_lean_ctx_server(
            target,
            crate::core::editor_registry::WriteOptions {
                overwrite_invalid: false,
            },
        )?;
    }

    for profile in existing_shell_profiles(&home) {
        remove_marked_block(&profile, agent)?;
    }
    Ok(())
}

fn remove_marked_block(path: &Path, agent: WrapAgent) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let (start, end) = shell_markers(agent);
    let Some(start_at) = existing.find(&start) else {
        return Ok(());
    };
    let after_start = start_at + start.len();
    let Some(end_offset) = existing[after_start..].find(&end) else {
        return Ok(());
    };
    let mut end_at = after_start + end_offset + end.len();
    if existing[end_at..].starts_with('\n') {
        end_at += 1;
    }
    write_config(
        path,
        &format!("{}{}", &existing[..start_at], &existing[end_at..]),
    )
}

fn print_wrap_success(agent: WrapAgent, port: u16, transport: WrapTransport) {
    println!("✓ lean-ctx wrapped {}.", agent.display_name());
    println!("  MCP:   lean-ctx registered for {}", agent.display_name());

    match transport {
        WrapTransport::Proxy => println!("  Proxy: http://127.0.0.1:{port}"),
        WrapTransport::ClaudeSubscription { .. } => {
            println!("  Proxy: skipped — Claude Pro/Max OAuth must connect directly to Anthropic");
            println!("         MCP and shell-output compression remain available");
            println!("         Wire-level compression requires ANTHROPIC_API_KEY");
        }
        WrapTransport::ClaudeExistingEndpoint => {
            println!("  Proxy: skipped — existing Claude endpoint preserved");
            println!("         MCP and shell-output compression remain available");
        }
    }

    if agent == WrapAgent::Cursor {
        println!();
        println!("  Cursor Settings → Models:");
        println!("    OpenAI Base URL:    http://127.0.0.1:{port}/v1");
        println!("    Anthropic Base URL: http://127.0.0.1:{port}");
    }

    if transport.uses_proxy() {
        println!();
        println!("  Verify: curl -fsS http://127.0.0.1:{port}/health");
    }
    println!(
        "  Then restart {} and confirm the lean-ctx MCP tools appear.",
        agent.display_name()
    );
    println!("  Undo: lean-ctx unwrap {}", agent.as_str());
}

fn wants_help(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--help" || arg == "-h")
}

fn print_usage() {
    eprintln!(
        "Usage: lean-ctx wrap <claude|codex|cursor|windsurf|cline|grok|aider|copilot> [--port 9340] [--unwrap]"
    );
}

fn print_unwrap_usage() {
    eprintln!("Usage: lean-ctx unwrap <claude|codex|cursor|windsurf|cline|grok|aider|copilot>");
}

fn print_help() {
    print_usage();
    println!("\nRegisters lean-ctx MCP and configures a compatible local proxy route.");
    println!("Claude Pro/Max OAuth stays direct; its MCP and shell paths remain available.");
    println!("`--unwrap` restores the pre-wrap configuration.");
}

fn print_unwrap_help() {
    print_unwrap_usage();
    println!("\nRestores the configuration backed up by `lean-ctx wrap`.");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn parses_default_port_and_agent() {
        let parsed = WrapArgs::parse(&args(&["codex"]), false).unwrap();
        assert_eq!(parsed.agent, WrapAgent::Codex);
        assert_eq!(parsed.port, DEFAULT_PROXY_PORT);
        assert!(!parsed.unwrap);
    }

    /// GH #1520: a supported-but-not-wrappable agent (opencode et al.) must be
    /// pointed at `init --agent`, not dismissed as "unsupported agent".
    #[test]
    fn supported_non_wrap_agent_points_at_init() {
        let err = WrapAgent::from_str("opencode").expect_err("opencode has no wrap profile");
        assert!(
            err.contains("lean-ctx init --agent opencode"),
            "error must name the working command: {err}"
        );
        assert!(!err.starts_with("unsupported agent"));
        // Genuinely unknown agents keep the plain error.
        let unknown = WrapAgent::from_str("nonexistent-agent-xyz").expect_err("unknown agent");
        assert!(unknown.contains("unsupported agent"));
    }

    #[test]
    fn supports_each_requested_agent() {
        for (name, agent) in [
            ("claude", WrapAgent::Claude),
            ("codex", WrapAgent::Codex),
            ("cursor", WrapAgent::Cursor),
            ("windsurf", WrapAgent::Windsurf),
            ("cline", WrapAgent::Cline),
            ("aider", WrapAgent::Aider),
        ] {
            assert_eq!(WrapAgent::from_str(name), Ok(agent));
        }
        assert!(WrapAgent::Cursor.environment(9340, true, true).is_empty());
        assert_eq!(
            WrapAgent::Windsurf.environment(9340, true, true),
            vec![
                ("OPENAI_BASE_URL", "http://127.0.0.1:9340/v1".to_string()),
                ("ANTHROPIC_BASE_URL", "http://127.0.0.1:9340".to_string()),
            ]
        );
    }

    #[test]
    fn parses_unwrap_and_explicit_port() {
        let parsed = WrapArgs::parse(&args(&["aider", "--port=9555", "--unwrap"]), false).unwrap();
        assert_eq!(parsed.agent, WrapAgent::Aider);
        assert_eq!(parsed.port, 9555);
        assert!(parsed.unwrap);
    }

    #[test]
    fn rejects_missing_or_invalid_arguments() {
        assert!(WrapArgs::parse(&[], false).is_err());
        assert!(WrapArgs::parse(&args(&["unknown"]), false).is_err());
        assert!(WrapArgs::parse(&args(&["codex", "--port=0"]), false).is_err());
    }

    #[test]
    fn renders_requested_environment_variables() {
        assert_eq!(
            WrapAgent::Aider.environment(9340, true, true),
            vec![
                ("OPENAI_API_BASE", "http://127.0.0.1:9340/v1".to_string()),
                ("ANTHROPIC_BASE_URL", "http://127.0.0.1:9340".to_string()),
            ]
        );
        assert_eq!(
            WrapAgent::Copilot.environment(9340, true, true),
            vec![(
                "COPILOT_PROVIDER_BASE_URL",
                "http://127.0.0.1:9340/v1".to_string(),
            )]
        );
        assert_eq!(
            WrapAgent::Cline.environment(9340, true, true),
            vec![
                ("OPENAI_BASE_URL", "http://127.0.0.1:9340/v1".to_string()),
                ("ANTHROPIC_BASE_URL", "http://127.0.0.1:9340".to_string()),
            ]
        );
    }

    #[test]
    fn replaces_only_its_own_shell_block() {
        let existing = "export KEEP=1\n# >>> lean-ctx wrap codex >>>\nold\n# <<< lean-ctx wrap codex <<<\nexport LAST=1\n";
        let replacement = shell_block(
            WrapAgent::Codex,
            &WrapAgent::Codex.environment(9340, true, true),
            false,
        );
        let rendered = replace_marked_block(existing, WrapAgent::Codex, &replacement);
        assert!(rendered.contains("export KEEP=1"));
        assert!(rendered.contains("OPENAI_BASE_URL=\"http://127.0.0.1:9340/v1\""));
        assert!(rendered.contains("export LAST=1"));
        assert!(!rendered.contains("old"));
    }

    #[test]
    fn claude_subscription_mode_never_exports_proxy_url() {
        assert!(WrapAgent::Claude.environment(9340, false, false).is_empty());
        assert_eq!(
            WrapAgent::Claude.environment(9340, true, true),
            vec![("ANTHROPIC_BASE_URL", "http://127.0.0.1:9340".to_string(),)]
        );
    }

    #[test]
    fn no_agent_exports_anthropic_url_without_an_api_key() {
        for agent in [
            WrapAgent::Claude,
            WrapAgent::Windsurf,
            WrapAgent::Cline,
            WrapAgent::Aider,
        ] {
            assert!(
                agent
                    .environment(9340, agent != WrapAgent::Claude, false)
                    .iter()
                    .all(|(name, _)| *name != "ANTHROPIC_BASE_URL"),
                "{} must not leak an Anthropic redirect",
                agent.as_str()
            );
        }
        assert_eq!(
            WrapAgent::Windsurf.environment(9340, true, false),
            vec![("OPENAI_BASE_URL", "http://127.0.0.1:9340/v1".to_string())]
        );
    }

    #[test]
    fn claude_transport_requires_auth_and_proven_endpoint_ownership() {
        let expected = "http://127.0.0.1:9340";
        assert_eq!(
            classify_claude_transport(false, None, expected, false),
            WrapTransport::ClaudeSubscription {
                repair_owned_redirect: false
            }
        );
        assert_eq!(
            classify_claude_transport(false, Some(expected), expected, false),
            WrapTransport::ClaudeExistingEndpoint
        );
        assert_eq!(
            classify_claude_transport(false, Some(expected), expected, true),
            WrapTransport::ClaudeSubscription {
                repair_owned_redirect: true
            }
        );
        assert_eq!(
            classify_claude_transport(true, Some("https://gateway.example"), expected, false),
            WrapTransport::ClaudeExistingEndpoint
        );
        assert_eq!(
            classify_claude_transport(true, None, expected, false),
            WrapTransport::Proxy
        );
        assert_eq!(
            classify_claude_transport(true, Some(expected), expected, true),
            WrapTransport::Proxy
        );
    }

    #[test]
    fn claude_subscription_mode_preserves_custom_endpoint() {
        let home = tempfile::tempdir().unwrap();
        let settings_dir = home.path().join(".claude");
        std::fs::create_dir_all(&settings_dir).unwrap();
        let settings = settings_dir.join("settings.json");
        let original = r#"{"env":{"ANTHROPIC_BASE_URL":"https://gateway.example"}}"#;
        std::fs::write(&settings, original).unwrap();

        configure_agent_endpoint(
            WrapAgent::Claude,
            9340,
            home.path(),
            WrapTransport::ClaudeExistingEndpoint,
        )
        .unwrap();

        assert_eq!(std::fs::read_to_string(settings).unwrap(), original);
    }

    #[test]
    fn unowned_local_claude_endpoint_is_preserved() {
        let home = tempfile::tempdir().unwrap();
        let settings_dir = home.path().join(".claude");
        std::fs::create_dir_all(&settings_dir).unwrap();
        let settings = settings_dir.join("settings.json");
        let original = r#"{"env":{"ANTHROPIC_BASE_URL":"http://localhost:4444"}}"#;
        std::fs::write(&settings, original).unwrap();

        crate::proxy_setup::repair_claude_subscription_env(home.path(), "http://127.0.0.1:4444")
            .unwrap();

        assert_eq!(std::fs::read_to_string(settings).unwrap(), original);
    }

    #[test]
    fn failed_proxy_prerequisite_stops_before_agent_mutations() {
        let home = tempfile::tempdir().unwrap();
        let profile = home.path().join(".zshrc");
        std::fs::write(&profile, "export KEEP=1\n").unwrap();
        let parsed = WrapArgs {
            agent: WrapAgent::Codex,
            port: 9340,
            unwrap: false,
        };

        let error = wrap_agent_at_home(&parsed, home.path(), |_| {
            Err("proxy startup failed".to_string())
        })
        .unwrap_err();

        assert_eq!(error, "proxy startup failed");
        assert_eq!(std::fs::read_to_string(profile).unwrap(), "export KEEP=1\n");
        assert!(!home.path().join(".codex/config.toml").exists());
    }

    #[test]
    fn command_status_distinguishes_usage_and_setup_failures() {
        assert_eq!(cmd_wrap(&args(&["unknown"])), 2);
        assert_eq!(finish_command("wrap", Err("failed".to_string())), 1);
        assert_eq!(finish_command("wrap", Ok(())), 0);
    }
}
