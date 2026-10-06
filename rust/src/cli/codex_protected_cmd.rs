// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx codex-protected` — launch ONE bounded, protected Codex session.
//!
//! The enforcement boundary is *capability removal*, not call denial: Codex
//! hooks are structurally fail-open (an exit-0-no-stdout hook is byte-identical
//! to a deliberate allow), so this launcher never relies on them. Instead it
//! starts the qualified Codex build against a throwaway `CODEX_HOME`, an empty
//! working directory, and a permission profile that *denies* the project source
//! root to Codex' own filesystem tools. The only route to the project is the
//! `lean-ctx` MCP server, which is bound to that root and applies the project's
//! content policy.
//!
//! Deliberate non-goals — this command does not:
//! * modify the user's `~/.codex`, any lean-ctx config, or the project;
//! * enable hooks, auto-approvals, trust bypasses or extra feature flags;
//! * configure a model, provider or paid endpoint;
//! * accept arbitrary Codex flags (there is no pass-through argument).
//!
//! `--check` starts no inference. Interactive use starts the user's Codex session.
//!
//! Qualification is deliberately narrow. Only `codex-cli 0.156.1` on macOS is
//! accepted; anything else fails closed with an actionable message rather than
//! claiming protection it has not been proven to provide.

use crate::core::policy::runtime::{
    REQUIRED_POLICY_DIGEST_ENV, REQUIRED_POLICY_ROOT_ENV, protected_policy_digest,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

mod gitlab_bootstrap;
mod onboarding;
mod write_guard;

pub(crate) use gitlab_bootstrap::dispatch as protected_gitlab_dispatch;
pub(crate) use gitlab_bootstrap::initialize_agent_gitlab;
pub(crate) use onboarding::cmd_setup_codex_protected;
#[cfg(all(test, target_os = "macos"))]
pub(crate) use write_guard::CHILD_PROFILE_ENV;
#[cfg(test)]
pub(crate) use write_guard::pin_synthetic_session;
#[cfg(all(test, unix))]
pub(crate) use write_guard::unpin_synthetic_session;
pub(crate) use write_guard::{enforce_protected_store_path, protected_child_prefix};

/// The single Codex build whose behaviour this launcher has been qualified
/// against. Compared byte-exactly to `codex --version` output.
const QUALIFIED_VERSION: &str = "codex-cli 0.156.1";

/// Name of the generated permission profile. Local to the temporary
/// `CODEX_HOME`; it never appears in the user's own configuration.
const PERMISSION_PROFILE: &str = "leanctx-protected";

/// A bounded project must carry a lean-ctx content policy — that policy is what
/// makes the MCP route safer than raw reads, so a project without one is not a
/// candidate for a "protected" session.
const POLICY_RELATIVE: &str = ".lean-ctx/policy.toml";

/// Upper bound for the credential file we are willing to copy. Codex'
/// `auth.json` is a few kB; anything larger is not what we think it is.
const MAX_AUTH_BYTES: u64 = 512 * 1024;

/// Client environment we are willing to forward. Everything else is dropped by
/// `env_clear`, including profile selectors, API keys and plugin/project config.
const CLIENT_ENV_PASSTHROUGH: &[&str] = &[
    "DO_NOT_TRACK",
    "PATH",
    "TERM",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
];

/// Environment forwarded to the MCP server only (never to the Codex client), so
/// the server resolves the user's existing lean-ctx config, data and providers.
const MCP_ENV_PASSTHROUGH: &[&str] = &[
    "DO_NOT_TRACK",
    "LEAN_CTX_CACHE_DIR",
    "LEAN_CTX_CONFIG_DIR",
    "LEAN_CTX_DATA_DIR",
    "LEAN_CTX_STATE_DIR",
    "LEANCTX_ORG_POLICY",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
];

// ---------------------------------------------------------------------------
// Argument parsing (pure)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct Invocation {
    project: String,
    codex: Option<String>,
    gitlab: Option<crate::core::providers::selected_gitlab::Selection>,
    glab: Option<String>,
    check: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Parsed {
    Help,
    Run(Invocation),
    Error(String),
}

/// Strictly parses the fixed grammar. Anything unexpected is an error rather
/// than a best-effort guess: this command provisions a security boundary, so a
/// mistyped flag must not silently produce a differently-shaped session.
fn parse_args(args: &[String]) -> Parsed {
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        return Parsed::Help;
    }

    let mut project: Option<String> = None;
    let mut codex: Option<String> = None;
    let mut gitlab_host = None;
    let mut gitlab_project = None;
    let mut gitlab_namespace = None;
    let mut glab = None;
    let mut check = false;
    let mut index = 0;

    while index < args.len() {
        let arg = args[index].as_str();
        let (flag, inline) = arg
            .split_once('=')
            .map_or((arg, None), |(f, v)| (f, Some(v)));

        match flag {
            "--project" | "--codex" | "--gitlab-host" | "--gitlab-project"
            | "--gitlab-namespace" | "--glab" => {
                let value = if let Some(value) = inline {
                    index += 1;
                    value.to_string()
                } else {
                    let Some(next) = args.get(index + 1) else {
                        return Parsed::Error(format!("{flag} requires a path"));
                    };
                    index += 2;
                    next.clone()
                };
                if let Err(err) = validate_value(flag, &value) {
                    return Parsed::Error(err);
                }
                let slot = match flag {
                    "--project" => &mut project,
                    "--codex" => &mut codex,
                    "--gitlab-host" => &mut gitlab_host,
                    "--gitlab-project" => &mut gitlab_project,
                    "--gitlab-namespace" => &mut gitlab_namespace,
                    _ => &mut glab,
                };
                if slot.is_some() {
                    return Parsed::Error(format!("{flag} was given more than once"));
                }
                *slot = Some(value);
            }
            "--check" => {
                if inline.is_some() {
                    return Parsed::Error("--check takes no value".to_string());
                }
                if check {
                    return Parsed::Error("--check was given more than once".to_string());
                }
                check = true;
                index += 1;
            }
            other => {
                return Parsed::Error(format!(
                    "unexpected argument `{other}` — see codex-protected --help for supported options"
                ));
            }
        }
    }

    let gitlab =
        match (gitlab_host, gitlab_project, gitlab_namespace) {
            (None, None, None) if glab.is_none() => None,
            (Some(host), Some(id), Some(namespace)) => {
                let Ok(project) = id.parse::<u64>() else {
                    return Parsed::Error("--gitlab-project requires a positive numeric ID".into());
                };
                let source = crate::core::providers::selected_gitlab::Selection {
                    host,
                    project,
                    namespace,
                };
                if let Err(error) = source.validate() {
                    return Parsed::Error(error);
                }
                Some(source)
            }
            _ => return Parsed::Error(
                "--gitlab-host, --gitlab-project and --gitlab-namespace must be selected together"
                    .into(),
            ),
        };
    match project {
        Some(project) => Parsed::Run(Invocation {
            project,
            codex,
            gitlab,
            glab,
            check,
        }),
        None => Parsed::Error("--project <DIR> is required".to_string()),
    }
}

/// Rejects values that cannot be a path we are willing to act on. A value that
/// starts with `--` is almost always a forgotten argument, not a path.
fn validate_value(flag: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{flag} requires a non-empty path"));
    }
    if value.starts_with("--") {
        return Err(format!(
            "{flag} looks like it is missing its value (got `{value}`)"
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{flag} contains control characters"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Preflight
// ---------------------------------------------------------------------------

/// Everything validated before the temporary Codex session is created.
struct Preflight {
    gitlab: Option<gitlab_bootstrap::Launch>,
    /// Canonical project source root — denied to Codex' native tools.
    project: PathBuf,
    /// Canonical qualified Codex executable.
    codex: PathBuf,
    /// Canonical `lean-ctx` executable that will serve MCP.
    lean_ctx: PathBuf,
    /// The user's real Codex home — read for `auth.json` only, never written.
    source_codex_home: PathBuf,
    /// The user's real home — handed to the MCP server so it keeps using the
    /// existing lean-ctx configuration and providers.
    real_home: PathBuf,
    /// Environment injected into the MCP server process (sorted, deterministic).
    mcp_env: BTreeMap<String, String>,
    /// Selected immutable file authorities; runtime data stays writable.
    control_paths: Vec<PathBuf>,
    /// Local operator credentials are unavailable to the MCP and its children.
    operator_credentials: Vec<PathBuf>,
}

/// Paths that must not be swallowed by the project deny rule.
struct Guards {
    real_home: PathBuf,
    temp_root: PathBuf,
    codex: PathBuf,
    lean_ctx: PathBuf,
}

fn preflight(invocation: &Invocation) -> Result<Preflight, String> {
    preflight_with_policy(invocation, |_| Ok(()))
}

/// Setup may initialize an explicitly selected pack only after checking the
/// executable, platform and project boundary. Launches use a no-op initializer.
fn preflight_with_policy(
    invocation: &Invocation,
    initialize_policy: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<Preflight, String> {
    if !cfg!(target_os = "macos") {
        return Err(
            "lean-ctx codex-protected is qualified on macOS only. Support for other platforms is \
             open work — the sandbox/permission semantics have not been verified there, and this \
             command will not claim a protection it cannot demonstrate."
                .to_string(),
        );
    }

    let codex = resolve_codex_executable(invocation.codex.as_deref())?;
    let version = codex_version(&codex)?;
    verify_version(&version)?;

    let lean_ctx = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|err| format!("could not resolve the running lean-ctx executable: {err}"))?;
    utf8_path(&lean_ctx, "the lean-ctx executable path")?;

    let real_home = crate::core::home::resolve_home_dir()
        .ok_or_else(|| "could not resolve your home directory".to_string())?
        .canonicalize()
        .map_err(|err| format!("could not resolve your home directory: {err}"))?;

    let source_codex_home = crate::core::home::resolve_codex_dir()
        .ok_or_else(|| "could not resolve your Codex home directory".to_string())?;

    let project = canonical_directory(&invocation.project)?;
    let project_str = utf8_path(&project, "the project path")?;

    let temp_root = std::env::temp_dir()
        .canonicalize()
        .map_err(|err| format!("could not resolve the temporary directory: {err}"))?;

    validate_project_root(
        &project,
        &Guards {
            real_home: real_home.clone(),
            temp_root,
            codex: codex.clone(),
            lean_ctx: lean_ctx.clone(),
        },
    )?;

    // Match the HOME spelling used by Config/paths in this process. Canonical
    // HOME would name the same files but change the shared snapshot's path bytes
    // when the user's home is reached through a symlink.
    let mcp_home = dirs::home_dir().ok_or("could not resolve the configuration home")?;
    let mcp_home_str = utf8_path(&mcp_home, "the configuration home")?;
    if !mcp_home.is_absolute() {
        return Err("HOME must be absolute for a protected session".into());
    }
    let mut mcp_env = mcp_env_from(&project_str, &mcp_home_str, &|key| std::env::var(key).ok());
    validate_mcp_authority_paths(&mcp_env)?;
    // Resolve the legacy/XDG layout once. Runtime data markers must not select
    // a different authority directory after the kernel profile is compiled.
    let mut control_paths = write_guard::bind_authority(&project, &mcp_home, &mut mcp_env)?;
    let mut operator_credentials = write_guard::operator_credentials(&mcp_home)?;
    let gitlab = invocation
        .gitlab
        .clone()
        .map(|source| gitlab_bootstrap::prepare(source, invocation.glab.as_deref(), &mcp_home))
        .transpose()?;
    if let Some(launch) = &gitlab {
        control_paths.push(launch.glab.clone());
        operator_credentials.extend([
            launch.config_dir.clone(),
            mcp_home.join("Library/Application Support/glab-cli"),
            mcp_home.join(".config/glab-cli"),
        ]);
    }
    validate_mcp_authority_paths(&mcp_env)?;
    initialize_policy(&project)?;
    if !project.join(POLICY_RELATIVE).is_file() {
        return Err(format!(
            "{project_str} has no {POLICY_RELATIVE}. Review a pack with `lean-ctx policy show baseline --toml`, \
             then select it with `lean-ctx setup codex-protected --project <DIR> --policy-pack baseline`. \
             Existing policies are never replaced by setup."
        ));
    }
    mcp_env.insert(
        REQUIRED_POLICY_DIGEST_ENV.to_string(),
        protected_policy_digest(&project)?,
    );
    // Prepared here, from the pinned launch authority and before any tool call
    // exists: every model-directed subprocess of the MCP server runs under these
    // rules, and the server refuses to start one without them.
    mcp_env.insert(
        write_guard::CHILD_PROFILE_ENV.to_string(),
        write_guard::child_profile(&mcp_home)?,
    );

    Ok(Preflight {
        gitlab,
        project,
        codex,
        lean_ctx,
        source_codex_home,
        real_home,
        mcp_env,
        control_paths,
        operator_credentials,
    })
}

/// A project root is unusable when denying it would also deny something the
/// session itself needs — the user's home (the MCP server reads its config
/// there), the temporary session tree, or either executable.
fn validate_project_root(project: &Path, guards: &Guards) -> Result<(), String> {
    if project.parent().is_none() {
        return Err(
            "the filesystem root cannot be a bounded project root — denying it would leave the \
             session with no readable filesystem at all"
                .to_string(),
        );
    }

    for (label, guard) in [
        ("your home directory", &guards.real_home),
        ("the temporary session directory", &guards.temp_root),
        ("the Codex executable", &guards.codex),
        ("the lean-ctx executable", &guards.lean_ctx),
    ] {
        if guard.starts_with(project) {
            return Err(format!(
                "{} contains {label} ({}). Denying that path would break the protected session \
                 itself, so this root is refused rather than launched half-enforced.",
                project.display(),
                guard.display()
            ));
        }
    }

    Ok(())
}

fn canonical_directory(raw: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(raw);
    let canonical = path
        .canonicalize()
        .map_err(|err| format!("could not resolve {raw}: {err}"))?;
    if !canonical.is_dir() {
        return Err(format!("{raw} is not a directory"));
    }
    Ok(canonical)
}

fn utf8_path(path: &Path, label: &str) -> Result<String, String> {
    path.to_str().map(str::to_string).ok_or_else(|| {
        format!("{label} is not valid UTF-8; this launcher cannot express it in Codex config")
    })
}

/// Resolves the executable to launch. An explicit `--codex` path is trusted as
/// the user's choice (it is still required to be a real executable file); the
/// implicit case walks `PATH` entries directly — never through a shell.
fn resolve_codex_executable(explicit: Option<&str>) -> Result<PathBuf, String> {
    let candidate = match explicit {
        Some(raw) => PathBuf::from(raw)
            .canonicalize()
            .map_err(|err| format!("could not resolve {raw}: {err}"))?,
        None => find_codex_on_path()?,
    };
    ensure_executable_file(&candidate)?;
    utf8_path(&candidate, "the Codex executable path")?;
    Ok(candidate)
}

fn find_codex_on_path() -> Result<PathBuf, String> {
    let path = std::env::var_os("PATH").ok_or_else(|| {
        "PATH is not set, so `codex` cannot be located — pass --codex <PATH>".to_string()
    })?;
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join("codex");
        if candidate.is_file() {
            return candidate
                .canonicalize()
                .map_err(|err| format!("could not resolve {}: {err}", candidate.display()));
        }
    }
    Err("could not find `codex` on PATH — pass --codex <PATH>".to_string())
}

fn ensure_executable_file(path: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(path)
        .map_err(|err| format!("could not stat {}: {err}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return Err(format!("{} is not executable", path.display()));
        }
    }
    Ok(())
}

/// Runs `codex --version` in a near-empty environment. This starts no session,
/// reads no credentials and performs no inference.
fn codex_version(codex: &Path) -> Result<String, String> {
    let mut command = Command::new(codex);
    command.arg("--version").env_clear().stdin(Stdio::null());
    for key in ["PATH", "HOME", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let output = command
        .output()
        .map_err(|err| format!("could not run `{} --version`: {err}", codex.display()))?;
    if !output.status.success() {
        return Err(format!(
            "`{} --version` failed with {}",
            codex.display(),
            output.status
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn verify_version(reported: &str) -> Result<(), String> {
    if reported == QUALIFIED_VERSION {
        return Ok(());
    }
    Err(format!(
        "this launcher is qualified against `{QUALIFIED_VERSION}` only, but the selected Codex \
         reports `{reported}`. The protection depends on version-specific config and tool \
         behaviour, so the session is refused rather than started with unverified enforcement. \
         Point --codex at a qualified build, or re-qualify this launcher for {reported}."
    ))
}

// ---------------------------------------------------------------------------
// Session provisioning
// ---------------------------------------------------------------------------

/// The throwaway directories of one session. Everything lives under a
/// `tempfile::TempDir` whose lifetime bounds the session.
struct Session {
    home: PathBuf,
    codex_home: PathBuf,
    workspace: PathBuf,
}

/// Creates the owner-only session tree and writes the managed Codex config.
/// Credentials are deliberately NOT handled here — `--check` uses this same
/// function and must never touch `auth.json`.
fn provision(pre: &Preflight, root: &Path) -> Result<Session, String> {
    let session = Session {
        home: root.join("home"),
        codex_home: root.join("codex"),
        workspace: root.join("workspace"),
    };

    set_owner_only(root)?;
    let private_tmp = session.home.with_file_name("tmp");
    for dir in [
        &session.home,
        &session.codex_home,
        &session.workspace,
        &private_tmp,
    ] {
        std::fs::create_dir(dir)
            .map_err(|err| format!("could not create {}: {err}", dir.display()))?;
        set_owner_only(dir)?;
    }

    let profile = write_guard::profile(pre, &session)?;
    write_guard::validate(&profile)?;
    let config = render_config(pre, &session, &profile)?;
    let config_path = session.codex_home.join("config.toml");
    write_owner_only(&config_path, config.as_bytes())?;

    Ok(session)
}

fn set_owner_only(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|err| format!("could not restrict {}: {err}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn write_owner_only(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|err| format!("could not create {}: {err}", path.display()))?;
    file.write_all(bytes)
        .map_err(|err| format!("could not write {}: {err}", path.display()))
}

// ---------------------------------------------------------------------------
// Managed Codex configuration
// ---------------------------------------------------------------------------

/// Base instructions handed to the model. They describe the boundary and the
/// available route; they deliberately do NOT load or quote the project's own
/// instruction files — reading those is exactly what the deny rule prevents.
fn instructions_text(project: &str) -> String {
    format!(
        "This is a lean-ctx protected Codex session.\n\
         \n\
         - The project source root is `{project}`. It is DENIED to your native filesystem tools \
         on purpose; do not try to work around that.\n\
         - Use the `lean-ctx` MCP server for every project operation. It is bound to that root \
         and applies the project's content policy: `ctx_compose` to orient, `ctx_read` to read, \
         `ctx_search` to search, `ctx_glob`/`ctx_tree` to list, `ctx_shell` to run commands, \
         and `ctx_patch` to edit.\n\
         - Your working directory is an empty, read-only workspace. Edit project files through \
         `ctx_patch`; do not use native tools to create a second working copy.\n\
          - The native shell tool is not available in this session.\n\
          - Policy and configuration authority is bound at launch. After any change to those \
          files, including general settings, the user must review it and restart \
          `lean-ctx codex-protected`.\n\
          - Control files are read-only to LeanCTX and its child processes. Hardlinked control \
          files are refused at launch; use a regular file or a symlink to a regular file.\n\
         - If a project operation is refused, report the refusal; do not look for another path \
         to the same data."
    )
}

/// Builds the complete managed `config.toml`. Deterministic: the same inputs
/// always render byte-identical output.
fn render_config(pre: &Preflight, session: &Session, mcp_profile: &str) -> Result<String, String> {
    let project = utf8_path(&pre.project, "the project path")?;
    let workspace = utf8_path(&session.workspace, "the session workspace path")?;
    let binary = utf8_path(&pre.lean_ctx, "the lean-ctx executable path")?;

    let mut doc = toml_edit::DocumentMut::new();

    // No project-doc auto-read: AGENTS.md and friends must not be pulled in
    // behind the deny rule.
    doc["project_doc_max_bytes"] = toml_edit::value(0_i64);
    doc["default_permissions"] = toml_edit::value(PERMISSION_PROFILE);
    // Normal interactive approval. Never `never`, never a bypass.
    doc["approval_policy"] = toml_edit::value("on-request");
    doc["developer_instructions"] = toml_edit::value(instructions_text(&project));

    // Capability removal, not call denial: the native shell tool is gone.
    let features = doc["features"].or_insert(toml_edit::table());
    if let Some(table) = features.as_table_mut() {
        table.set_implicit(false);
        table["shell_tool"] = toml_edit::value(false);
    }

    // `[permissions.<profile>.filesystem]` — the actual boundary. No legacy
    // `sandbox_mode` alongside it; the two mechanisms must not be mixed.
    let permissions = doc["permissions"].or_insert(toml_edit::table());
    if let Some(table) = permissions.as_table_mut() {
        table.set_implicit(true);
    }
    let profile = permissions[PERMISSION_PROFILE].or_insert(toml_edit::table());
    if let Some(table) = profile.as_table_mut() {
        table.set_implicit(true);
    }
    let filesystem = profile["filesystem"].or_insert(toml_edit::table());
    if let Some(table) = filesystem.as_table_mut() {
        table.set_implicit(false);
        table[":minimal"] = toml_edit::value("read");
        table[workspace.as_str()] = toml_edit::value("read");
        table[project.as_str()] = toml_edit::value("deny");
        table[utf8_path(&session.home, "the session home")?.as_str()] = toml_edit::value("deny");
        table[utf8_path(&session.codex_home, "the session Codex home")?.as_str()] =
            toml_edit::value("deny");
        for path in write_guard::credential_read_paths(&pre.operator_credentials)? {
            table[&path] = toml_edit::value("deny");
        }
    }

    // Exactly one MCP server, ours, by absolute path.
    let servers = doc["mcp_servers"].or_insert(toml_edit::table());
    if let Some(table) = servers.as_table_mut() {
        table.set_implicit(true);
    }
    let lean = servers["lean-ctx"].or_insert(toml_edit::table());
    let Some(lean_table) = lean.as_table_mut() else {
        return Err("could not build the MCP server section".to_string());
    };
    lean_table.set_implicit(false);
    let mut args = toml_edit::Array::new();
    // Both protected starts — with and without a selected GitLab source — go
    // through the same outside wrapper. It applies the MCP profile, and it
    // stays alive as the server's supervisor: macOS refuses a nested
    // `sandbox_apply`, so the sandboxed server itself has no way to run a
    // command child under the launcher's rules. `null` is the ordinary start.
    lean_table["command"] = toml_edit::value(binary.as_str());
    args.push(gitlab_bootstrap::WRAPPER);
    args.push(
        serde_json::to_string(&pre.gitlab)
            .map_err(|_| "cannot serialize the protected launch configuration")?,
    );
    args.push(mcp_profile);
    lean_table["args"] = toml_edit::value(args);
    lean_table["startup_timeout_sec"] = toml_edit::value(30_i64);
    lean_table["tool_timeout_sec"] = toml_edit::value(120_i64);

    let env = lean_table["env"].or_insert(toml_edit::table());
    if let Some(table) = env.as_table_mut() {
        table.set_implicit(false);
        for (key, value) in &pre.mcp_env {
            table[key.as_str()] = toml_edit::value(value.as_str());
        }
    }

    Ok(doc.to_string())
}

/// Environment for the MCP server process. `HOME` is the user's real home so
/// the server keeps using the existing lean-ctx config and providers; the
/// documented directory overrides are preserved when the caller set them.
fn mcp_env_from(
    project: &str,
    real_home: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("HOME".to_string(), real_home.to_string());
    env.insert("LEAN_CTX_PROJECT_ROOT".to_string(), project.to_string());
    env.insert(REQUIRED_POLICY_ROOT_ENV.to_string(), project.to_string());
    for &key in MCP_ENV_PASSTHROUGH {
        if let Some(value) = lookup(key).filter(|value| !value.trim().is_empty()) {
            env.insert(key.to_string(), value);
        }
    }
    env
}

fn validate_mcp_authority_paths(env: &BTreeMap<String, String>) -> Result<(), String> {
    for &key in MCP_ENV_PASSTHROUGH {
        if key != "DO_NOT_TRACK"
            && let Some(value) = env.get(key)
            && !Path::new(value.trim()).is_absolute()
        {
            return Err(format!(
                "{key} must be an absolute path for a protected session; the MCP working directory is isolated"
            ));
        }
    }
    Ok(())
}

/// Environment for the Codex client process. Built from scratch on top of
/// `env_clear`, so profile selectors, API keys and any inherited lean-ctx or
/// plugin configuration simply do not exist in the child.
fn client_env(
    session: &Session,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for &key in CLIENT_ENV_PASSTHROUGH {
        if let Some(value) = lookup(key).filter(|value| !value.is_empty()) {
            env.insert(key.to_string(), value);
        }
    }
    env.insert("HOME".to_string(), session.home.display().to_string());
    env.insert(
        "CODEX_HOME".to_string(),
        session.codex_home.display().to_string(),
    );
    env.insert(
        "TMPDIR".to_string(),
        session.home.with_file_name("tmp").display().to_string(),
    );
    env
}

/// The complete, fixed Codex argument list. `--ephemeral` and
/// `--skip-git-repo-check` are `codex exec` flags and are NOT valid for the
/// interactive CLI, so they are not sent. `--no-daemon` keeps the session off
/// the shared background app-server, which may have been started under the
/// user's own Codex home.
fn codex_argv(workspace: &str) -> Vec<String> {
    vec![
        "--strict-config".to_string(),
        "--no-daemon".to_string(),
        "-C".to_string(),
        workspace.to_string(),
    ]
}

fn spawn_codex(
    codex: &Path,
    workspace: &Path,
    env: &BTreeMap<String, String>,
) -> std::io::Result<ExitStatus> {
    let workspace_arg = workspace.display().to_string();
    Command::new(codex)
        .args(codex_argv(&workspace_arg))
        .current_dir(workspace)
        .env_clear()
        .envs(env)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

/// Copies exactly one file — `auth.json` — into the temporary Codex home. No
/// other part of the user's Codex home (config, skills, plugins, history) is
/// read or copied. Errors never contain credential contents.
///
/// The copy is one-way on purpose: Codex may refresh the token inside the
/// temporary home, and copying that back would race with the user's real
/// session. The refreshed copy is discarded with the session instead.
fn copy_auth(source_codex_home: &Path, target_codex_home: &Path) -> Result<(), String> {
    use std::io::Read;
    let home_meta =
        std::fs::symlink_metadata(source_codex_home).map_err(|_| login_hint(source_codex_home))?;
    if home_meta.file_type().is_symlink() || !home_meta.is_dir() {
        return Err("Codex credential home must be a regular directory, not a symlink".into());
    }
    let source = source_codex_home.join("auth.json");
    let meta = std::fs::symlink_metadata(&source).map_err(|_| login_hint(source_codex_home))?;

    if meta.file_type().is_symlink() {
        return Err(format!(
            "{} is a symlink. Refusing to follow it for a credential copy.",
            source.display()
        ));
    }
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", source.display()));
    }
    if meta.len() == 0 || meta.len() > MAX_AUTH_BYTES {
        return Err(format!(
            "{} has an unexpected size ({} bytes). Refusing to copy it.",
            source.display(),
            meta.len()
        ));
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options
        .open(&source)
        .map_err(|err| format!("could not read {}: {}", source.display(), err.kind()))?;
    let opened = file
        .metadata()
        .map_err(|_| "could not inspect opened credentials")?;
    if !opened.is_file() || opened.len() == 0 || opened.len() > MAX_AUTH_BYTES {
        return Err("opened credentials are not a bounded regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_AUTH_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "could not read opened credentials")?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_AUTH_BYTES {
        return Err("credentials changed size while being read".into());
    }
    write_owner_only(&target_codex_home.join("auth.json"), &bytes)
}

fn login_hint(source_codex_home: &Path) -> String {
    format!(
        "no Codex credentials found at {}/auth.json. Run `codex login` once in your normal Codex \
         setup, then re-run this command. This launcher will not configure an API-key provider or \
         any other fallback on your behalf.",
        source_codex_home.display()
    )
}

// ---------------------------------------------------------------------------
// Session cleanup
// ---------------------------------------------------------------------------

/// Normal child exit can race with the last writes into its temporary home.
/// Retry only transient tree-removal errors; persistent residue remains an
/// explicit failure. No delay is added when the first removal succeeds.
const CLEANUP_ATTEMPTS: u32 = 12;
const CLEANUP_FIRST_BACKOFF: Duration = Duration::from_millis(25);
const CLEANUP_MAX_BACKOFF: Duration = Duration::from_millis(400);

/// Removes only the owned root. `remove_dir_all` does not follow symlinks;
/// never unlink nested credential paths separately through mutable parents.
fn close_session_root(root: &Path) -> std::io::Result<()> {
    close_session_root_with(root, CLEANUP_ATTEMPTS, CLEANUP_FIRST_BACKOFF, remove_tree)
}

/// `std::fs::remove_dir_all` pinned to `&Path`, so it can be handed to
/// `close_session_root_with` in place of a test's own removal.
fn remove_tree(path: &Path) -> std::io::Result<()> {
    std::fs::remove_dir_all(path)
}

/// `close_session_root` with the retry schedule and the removal itself
/// injected, so the transient, permanent and ownership cases are testable
/// without depending on when a real descendant happens to exit.
fn close_session_root_with(
    root: &Path,
    attempts: u32,
    first_backoff: Duration,
    mut remove: impl FnMut(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut backoff = first_backoff;
    let mut last = None;
    for attempt in 0..attempts {
        match remove(root) {
            Ok(()) => return Ok(()),
            // Either the root is already gone, or an entry disappeared from
            // under the walk while a writer tidied up after itself.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                if matches!(std::fs::symlink_metadata(root), Err(ref e) if e.kind() == std::io::ErrorKind::NotFound)
                {
                    return Ok(());
                }
                last = Some(err);
            }
            Err(err) if tree_refilled(&err) => last = Some(err),
            // A permission error, a read-only filesystem or anything else that
            // retrying cannot change fails on the first attempt.
            Err(err) => return Err(err),
        }
        if attempt + 1 < attempts {
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(CLEANUP_MAX_BACKOFF);
        }
    }

    Err(last.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "cleanup requires at least one attempt",
        )
    }))
}

/// `true` when the removal failed the way a tree does while something is still
/// creating entries in it: the directory refilled between the walk and the
/// final `rmdir`.
fn tree_refilled(err: &std::io::Error) -> bool {
    #[cfg(unix)]
    let raw = matches!(err.raw_os_error(), Some(libc::ENOTEMPTY | libc::EEXIST));
    #[cfg(not(unix))]
    let raw = false;

    raw || err.kind() == std::io::ErrorKind::DirectoryNotEmpty
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub(crate) fn cmd_codex_protected(args: &[String]) -> i32 {
    match parse_args(args) {
        Parsed::Help => {
            print!("{}", help_text());
            0
        }
        Parsed::Error(message) => {
            eprintln!("Error: {message}\n");
            eprint!("{}", help_text());
            2
        }
        Parsed::Run(invocation) => run(&invocation),
    }
}

fn run(invocation: &Invocation) -> i32 {
    let pre = match preflight(invocation) {
        Ok(pre) => pre,
        Err(message) => {
            eprintln!("Error: {message}");
            return 1;
        }
    };

    run_preflight(invocation, &pre)
}

fn run_preflight(invocation: &Invocation, pre: &Preflight) -> i32 {
    let temp = match tempfile::Builder::new()
        .prefix("leanctx-codex-protected-")
        .tempdir()
    {
        Ok(temp) => temp,
        Err(err) => {
            eprintln!("Error: could not create a temporary session directory: {err}");
            return 1;
        }
    };
    // macOS hands out `/var/folders/...`, which is a symlink to `/private/...`.
    // The permission entries must carry the canonical form, or a canonical-path
    // access would not match them.
    let root = match temp.path().canonicalize() {
        Ok(root) => root,
        Err(err) => {
            eprintln!("Error: could not resolve the temporary session directory: {err}");
            return 1;
        }
    };

    let session = match provision(pre, &root) {
        Ok(session) => session,
        Err(message) => {
            eprintln!("Error: {message}");
            return 1;
        }
    };

    let code = if invocation.check {
        print_check(pre, &session);
        0
    } else {
        run_interactive(pre, &session)
    };
    // Cleanup is explicit from here on. `keep()` disarms the `TempDir`'s
    // silent drop so that exactly one place removes this tree and exactly one
    // place reports a failure to remove it; the canonical `root` and the path
    // `keep()` hands back are the same directory.
    let _ = temp.keep();
    if let Err(err) = close_session_root(&root) {
        eprintln!(
            "Error: could not remove {}: {err}. This owner-only directory may contain copied Codex credentials; remove it before sharing or reusing the directory.",
            root.display()
        );
        return if code == 0 { 1 } else { code };
    }
    code
}

fn run_interactive(pre: &Preflight, session: &Session) -> i32 {
    if let Err(message) = copy_auth(&pre.source_codex_home, &session.codex_home) {
        eprintln!("Error: {message}");
        return 1;
    }

    print_banner(&pre, &session);

    let env = client_env(&session, &|key| std::env::var(key).ok());
    let status = match spawn_codex(&pre.codex, &session.workspace, &env) {
        Ok(status) => status,
        Err(err) => {
            eprintln!("Error: could not start {}: {err}", pre.codex.display());
            return 1;
        }
    };

    status.code().unwrap_or(130)
}

fn print_check(pre: &Preflight, session: &Session) {
    println!("lean-ctx codex-protected --check\n");
    println!("  Codex               : {}", pre.codex.display());
    println!("  qualified version   : {QUALIFIED_VERSION}");
    println!("  project (denied)    : {}", pre.project.display());
    println!("  policy              : {POLICY_RELATIVE} validated and bound at launch");
    println!("  MCP server          : {} mcp", pre.lean_ctx.display());
    println!("  MCP control writes  : kernel-denied, inherited by child processes");
    println!("  MCP HOME            : {}", pre.real_home.display());
    println!("  session workspace   : {}", session.workspace.display());
    println!("  session CODEX_HOME  : {}", session.codex_home.display());

    let auth = pre.source_codex_home.join("auth.json");
    let auth_state = match std::fs::symlink_metadata(&auth) {
        Ok(meta) if meta.file_type().is_symlink() => "present but a symlink — would be refused",
        Ok(meta) if !meta.is_file() => "present but not a regular file — would be refused",
        Ok(meta) if meta.len() == 0 || meta.len() > MAX_AUTH_BYTES => {
            "present but an unexpected size — would be refused"
        }
        Ok(_) => "present (not read by --check)",
        Err(_) => "missing — run `codex login` first",
    };
    println!("  credentials         : {auth_state}");

    println!(
        "\nGenerated {}:\n",
        session.codex_home.join("config.toml").display()
    );
    match std::fs::read_to_string(session.codex_home.join("config.toml")) {
        Ok(config) => println!("{config}"),
        Err(err) => println!("  <could not re-read the generated config: {err}>"),
    }
    println!(
        "--check verified the version, policy authority and kernel profile and generated the session configuration. No Codex auth.json was read, no Codex \
         session was started, and this temporary directory is removed as this command exits."
    );
}

fn print_banner(pre: &Preflight, session: &Session) {
    eprintln!("lean-ctx codex-protected");
    eprintln!(
        "  project (denied to Codex' own tools) : {}",
        pre.project.display()
    );
    eprintln!(
        "  reachable only via lean-ctx MCP      : {}",
        pre.project.display()
    );
    eprintln!(
        "  working directory (empty, scratch)   : {}",
        session.workspace.display()
    );
    eprintln!(
        "  temporary CODEX_HOME                 : {}",
        session.codex_home.display()
    );
    eprintln!(
        "  Codex                                : {} ({QUALIFIED_VERSION})",
        pre.codex.display()
    );
    eprintln!();
    eprintln!(
        "Setup leaves your Codex configuration and project unchanged. During the session, approved \
         LeanCTX operations can modify project files. A copy of your \
         existing Codex credentials is placed in the temporary home for this session only and is \
         removed with temporary session data on normal exit. Forced process termination can leave \
         this owner-only directory behind. Refreshed credentials are not copied back."
    );
    eprintln!();
}

fn help_text() -> String {
    format!(
        "Usage: lean-ctx codex-protected --project <DIR> [--codex <PATH>]\n\
         \x20      lean-ctx codex-protected --project <DIR> --check\n\
         \n\
         Starts ONE interactive Codex session that can reach <DIR> only through lean-ctx.\n\
         Codex runs with a throwaway CODEX_HOME, an empty working directory, its native shell\n\
         tool removed, and <DIR> denied to its own filesystem tools. The lean-ctx MCP server is\n\
         bound to <DIR> and applies that project's content policy.\n\
         A macOS kernel profile prevents MCP and descendant writes to selected control files.\n\
         Hardlinked control files are refused at launch; ordinary files and symlinked configs are supported.\n\
         \n\
         Options:\n\
         \x20 --project <DIR>   Project source root. Must be a directory containing {POLICY_RELATIVE}.\n\
         \x20 --codex <PATH>    Codex executable to use. Default: the first `codex` on PATH.\n\
         \x20 --check           Print generated configuration. Reads no credentials, starts no session.\n\
         \x20 --gitlab-host <HOST> --gitlab-project <ID> --gitlab-namespace <GROUP/PROJECT>\n\
         \x20                   Bind GitLab to this HTTPS host, numeric ID and expected namespace.\n\
         \x20 --glab <PATH>     Credential reader (default: glab on PATH); existing global login only.\n\
         \x20                   Credentials are acquired at MCP startup, not during --check.\n\
         \n\
         Requirements: macOS, and exactly `{QUALIFIED_VERSION}`. Other platforms and versions are\n\
         refused rather than launched with unverified enforcement.\n\
         \n\
         Setup does not modify your existing configuration or project. Session edits use LeanCTX.\n\
         It sets no model, provider or API key, enables no hooks or auto-approvals, and\n\
         accepts no pass-through Codex flags.\n"
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "codex_protected_cmd_tests.rs"]
mod tests;
