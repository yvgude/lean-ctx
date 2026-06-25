macro_rules! qprintln {
    ($($t:tt)*) => {
        if !super::quiet_enabled() {
            println!($($t)*);
        }
    };
}

pub fn print_hook_stdout(shell: &str) {
    let binary = crate::core::portable_binary::resolve_portable_binary();
    let binary = hook_binary_for_shell(shell, &binary);

    let code = match shell {
        "bash" | "zsh" => generate_hook_posix(&binary),
        "fish" => generate_hook_fish(&binary),
        "powershell" | "pwsh" => generate_hook_powershell(&binary),
        _ => {
            tracing::error!("lean-ctx: unsupported shell '{shell}'");
            eprintln!("Supported: bash, zsh, fish, powershell");
            std::process::exit(1);
        }
    };
    print!("{code}");
}

/// Pick the executable-path form to embed in a generated shell hook.
///
/// bash/zsh/fish (incl. Git Bash / MSYS on Windows) source the hook and invoke
/// the binary from a POSIX shell, so on Windows they need the MSYS `/c/...`
/// form. PowerShell and `pwsh` execute the path via the `&` call operator and
/// cannot run an MSYS `/c/...` path (#518); they get the native path unchanged.
/// On Unix `to_bash_compatible_path` is a no-op, so all shells are unaffected.
fn hook_binary_for_shell(shell: &str, binary: &str) -> String {
    match shell {
        "powershell" | "pwsh" => binary.to_string(),
        _ => crate::hooks::to_bash_compatible_path(binary),
    }
}

fn backup_shell_config(path: &std::path::Path) {
    if !path.exists() {
        return;
    }
    let bak = path.with_extension("lean-ctx.bak");
    if std::fs::copy(path, &bak).is_ok() {
        qprintln!(
            "  Backup: {}",
            bak.file_name().map_or_else(
                || bak.display().to_string(),
                |n| format!("~/{}", n.to_string_lossy())
            )
        );
    }
}

/// Directory for config artifacts written by `init`/`setup` — shell hooks and
/// `env.sh`. These are config files (RO-safe), so they live in [`config_dir`]
/// (GH #408). For legacy/mixed installs `config_dir()` collapses onto the same
/// single directory as before, so this is a no-op there.
fn config_artifact_dir() -> Option<std::path::PathBuf> {
    crate::core::paths::config_dir().ok()
}

fn write_hook_file(filename: &str, content: &str) -> Option<std::path::PathBuf> {
    let dir = config_artifact_dir()?;
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(filename);
    match std::fs::write(&path, content) {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
            }
            Some(path)
        }
        Err(e) => {
            tracing::error!("Error writing {}: {e}", path.display());
            None
        }
    }
}

fn resolved_hook_dir_display() -> String {
    config_artifact_dir().map_or_else(
        || "$HOME/.config/lean-ctx".to_string(),
        |p| p.to_string_lossy().to_string(),
    )
}

fn source_line_posix(shell_ext: &str) -> String {
    let mut dir = resolved_hook_dir_display();
    // Git Bash / MSYS expects /c/... style paths in bashrc/zshrc.
    if cfg!(windows) {
        dir = crate::hooks::to_bash_compatible_path(&dir);
    }
    format!(
        "# lean-ctx shell hook — begin\n\
         if [ -f \"{dir}/shell-hook.{shell_ext}\" ]; then\n\
           . \"{dir}/shell-hook.{shell_ext}\"\n\
         fi\n\
         # lean-ctx shell hook — end\n"
    )
}

fn source_line_fish() -> String {
    let mut dir = resolved_hook_dir_display();
    // Fish on Windows (MSYS) also expects /c/... style paths.
    if cfg!(windows) {
        dir = crate::hooks::to_bash_compatible_path(&dir);
    }
    format!(
        "# lean-ctx shell hook — begin\n\
         if test -f \"{dir}/shell-hook.fish\"\n\
           source \"{dir}/shell-hook.fish\"\n\
         end\n\
         # lean-ctx shell hook — end\n"
    )
}

fn source_line_powershell() -> String {
    let dir = resolved_hook_dir_display();
    let dir_ps = dir.replace('/', "\\");
    format!(
        "# lean-ctx shell hook — begin\n\
         $leanCtxHook = \"{dir_ps}\\shell-hook.ps1\"\n\
         if ((Test-Path $leanCtxHook) -and -not [Console]::IsOutputRedirected) {{ . $leanCtxHook }}\n"
    )
}

fn upsert_source_line(rc_path: &std::path::Path, source_line: &str) {
    backup_shell_config(rc_path);

    if let Ok(existing) = std::fs::read_to_string(rc_path) {
        if existing.contains(source_line.trim()) {
            return;
        }

        // Remove any legacy blocks and one-liner source lines, then append our canonical block.
        let cleaned = remove_lean_ctx_block(&existing);
        let cleaned = cleaned
            .lines()
            .filter(|line| {
                !line.contains("lean-ctx/shell-hook.")
                    && !line.contains("lean-ctx\\shell-hook.")
                    && line.trim() != "lean-ctx shell hook"
            })
            .collect::<Vec<_>>()
            .join("\n");
        let cleaned = if cleaned.ends_with('\n') {
            cleaned
        } else {
            format!("{cleaned}\n")
        };

        match std::fs::write(rc_path, format!("{cleaned}{source_line}")) {
            Ok(()) => {
                qprintln!("Updated lean-ctx hook in {}", rc_path.display());
            }
            Err(e) => {
                tracing::error!("Error updating {}: {e}", rc_path.display());
                print_shell_write_error(rc_path, source_line, &e);
            }
        }
        return;
    }

    match std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(rc_path)
    {
        Ok(mut f) => {
            use std::io::Write;
            let _ = f.write_all(source_line.as_bytes());
            qprintln!("Added lean-ctx hook to {}", rc_path.display());
        }
        Err(e) => {
            tracing::error!("Error writing {}: {e}", rc_path.display());
            print_shell_write_error(rc_path, source_line, &e);
        }
    }
}

fn print_shell_write_error(rc_path: &std::path::Path, source_line: &str, err: &std::io::Error) {
    eprintln!();
    eprintln!("  \x1B[33m⚠ Cannot write to {}\x1B[0m", rc_path.display());
    eprintln!("    Error: {err}");
    if err.kind() == std::io::ErrorKind::PermissionDenied {
        eprintln!();
        eprintln!("    Your shell config is read-only (nix-darwin, Home Manager, or similar).");
        eprintln!("    Add the following to a writable shell config file manually:");
    } else {
        eprintln!();
        eprintln!("    Add the following to your shell config manually:");
    }
    eprintln!();
    for line in source_line.lines() {
        eprintln!("      {line}");
    }
    eprintln!();
    eprintln!("    Or source it from a writable file (e.g. ~/.zshrc.local):");
    eprintln!("      echo 'source ~/.zshrc.local' # (add to nix config)");
    eprintln!("      Then add the hook lines to ~/.zshrc.local");
    eprintln!();
}

#[must_use]
pub fn generate_hook_powershell(binary: &str) -> String {
    let config = crate::core::config::Config::load();
    let activation = config.shell_activation_effective();
    let baked_default = match activation {
        crate::core::config::ShellActivation::Always => "always",
        crate::core::config::ShellActivation::AgentsOnly => "agents-only",
        crate::core::config::ShellActivation::Off => "off",
    };
    let binary_escaped = binary.replace('\\', "\\\\");
    format!(
        r#"# lean-ctx shell hook — transparent CLI compression (95+ patterns)
$_leanCtxActivation = if ($env:LEAN_CTX_SHELL_ACTIVATION) {{ $env:LEAN_CTX_SHELL_ACTIVATION }} else {{ "{baked_default}" }}
$_leanCtxShouldActivate = $false
if (-not $env:LEAN_CTX_ACTIVE -and -not $env:LEAN_CTX_DISABLED -and -not $env:LEAN_CTX_NO_HOOK) {{
  switch ($_leanCtxActivation) {{
    {{ $_ -in 'off','none','manual' }} {{ $_leanCtxShouldActivate = $false }}
    {{ $_ -in 'agents-only','agents_only','agentsonly' }} {{
      $_leanCtxShouldActivate = $env:LEAN_CTX_AGENT -or $env:CLAUDECODE -or $env:CODEBUDDY -or $env:CODEX_CLI_SESSION -or $env:GEMINI_SESSION
    }}
    default {{ $_leanCtxShouldActivate = $true }}
  }}
}}
if ($_leanCtxShouldActivate) {{
  $LeanCtxBin = "{binary_escaped}"
  function _lc {{
    $nativeCmd = Get-Command $args[0] -CommandType Application -ErrorAction SilentlyContinue
    if ($env:LEAN_CTX_DISABLED -or $env:LEAN_CTX_NO_HOOK -or [Console]::IsOutputRedirected) {{
      if ($nativeCmd) {{ & $nativeCmd.Source $args[1..$args.Length] }} else {{ Write-Error "Command not found: $($args[0])" }}
      return
    }}
    & $LeanCtxBin -c @args
    if ($LASTEXITCODE -eq 127 -or $LASTEXITCODE -eq 126) {{
      if ($nativeCmd) {{ & $nativeCmd.Source $args[1..$args.Length] }} else {{ Write-Error "Command not found: $($args[0])" }}
    }}
  }}
  function lean-ctx-raw {{ $env:LEAN_CTX_RAW = '1'; & @args; Remove-Item Env:LEAN_CTX_RAW -ErrorAction SilentlyContinue }}
  if (Get-Command lean-ctx -ErrorAction SilentlyContinue) {{
    function git {{ _lc git @args }}
    function cargo {{ _lc cargo @args }}
    function docker {{ _lc docker @args }}
    function kubectl {{ _lc kubectl @args }}
    function gh {{ _lc gh @args }}
    function pip {{ _lc pip @args }}
    function pip3 {{ _lc pip3 @args }}
    function ruff {{ _lc ruff @args }}
    function go {{ _lc go @args }}
    function curl {{ _lc curl @args }}
    function wget {{ _lc wget @args }}
    foreach ($c in @('npm','pnpm','yarn','eslint','prettier','tsc')) {{
      if (Get-Command $c -CommandType Application -ErrorAction SilentlyContinue) {{
        $body = "_lc $c `@args"
        New-Item -Path "function:$c" -Value ([scriptblock]::Create($body)) -Force | Out-Null
      }}
    }}
  }}
}}
"#
    )
}

pub fn init_powershell(binary: &str) {
    // OS-aware profile path: ~/.config/powershell on macOS/Linux (never ~/Documents,
    // which triggers a macOS TCC prompt, #356). On Windows resolve the live $PROFILE
    // so OneDrive-redirected Documents folders are honored (#558).
    let profile_path = if let Some(home) = dirs::home_dir() {
        let path = crate::shell::platform::resolve_powershell_profile_path(&home);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        path
    } else {
        tracing::error!("Could not resolve PowerShell profile directory");
        return;
    };

    let hook_content = generate_hook_powershell(binary);

    if write_hook_file("shell-hook.ps1", &hook_content).is_some() {
        upsert_source_line(&profile_path, &source_line_powershell());
        qprintln!("  Binary: {binary}");
    }
}

#[must_use]
pub fn remove_lean_ctx_block_ps(content: &str) -> String {
    let mut result = String::new();
    let mut in_block = false;
    let mut brace_depth = 0i32;

    for line in content.lines() {
        if line.contains("lean-ctx shell hook") {
            in_block = true;
            continue;
        }
        if in_block {
            brace_depth += line.matches('{').count() as i32;
            brace_depth -= line.matches('}').count() as i32;
            if brace_depth <= 0 && (line.trim() == "}" || line.trim().is_empty()) {
                if line.trim() == "}" {
                    in_block = false;
                    brace_depth = 0;
                }
                continue;
            }
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

#[must_use]
pub fn generate_hook_fish(binary: &str) -> String {
    let config = crate::core::config::Config::load();
    let activation = config.shell_activation_effective();
    let baked_default = match activation {
        crate::core::config::ShellActivation::Always => "always",
        crate::core::config::ShellActivation::AgentsOnly => "agents-only",
        crate::core::config::ShellActivation::Off => "off",
    };
    let alias_list = crate::rewrite_registry::shell_alias_list();
    format!(
        "# lean-ctx shell hook — smart shell mode (track-by-default)\n\
        set -g _lean_ctx_cmds {alias_list}\n\
        \n\
        function _lc_is_agent\n\
        \tset -q LEAN_CTX_AGENT; or set -q CODEX_CLI_SESSION; or set -q CLAUDECODE; or set -q CODEBUDDY; or set -q GEMINI_SESSION\n\
        end\n\
        \n\
        function _lc\n\
        \tif set -q LEAN_CTX_DISABLED; or set -q LEAN_CTX_NO_HOOK\n\
        \t\tcommand $argv\n\
        \t\treturn\n\
        \tend\n\
        \tif not isatty stdout; and not _lc_is_agent\n\
        \t\tcommand $argv\n\
        \t\treturn\n\
        \tend\n\
        \t'{binary}' -t $argv\n\
        \tset -l _lc_rc $status\n\
        \tif test $_lc_rc -eq 127 -o $_lc_rc -eq 126\n\
        \t\tcommand $argv\n\
        \telse\n\
        \t\treturn $_lc_rc\n\
        \tend\n\
        end\n\
        \n\
        function _lc_compress\n\
        \tif set -q LEAN_CTX_DISABLED; or set -q LEAN_CTX_NO_HOOK\n\
        \t\tcommand $argv\n\
        \t\treturn\n\
        \tend\n\
        \tif not isatty stdout; and not _lc_is_agent\n\
        \t\tcommand $argv\n\
        \t\treturn\n\
        \tend\n\
        \t'{binary}' -c $argv\n\
        \tset -l _lc_rc $status\n\
        \tif test $_lc_rc -eq 127 -o $_lc_rc -eq 126\n\
        \t\tcommand $argv\n\
        \telse\n\
        \t\treturn $_lc_rc\n\
        \tend\n\
        end\n\
        \n\
        function lean-ctx-on\n\
        \tfor _lc_cmd in $_lean_ctx_cmds\n\
        \t\talias $_lc_cmd '_lc '$_lc_cmd\n\
        \tend\n\
        \talias k '_lc kubectl'\n\
        \tset -gx LEAN_CTX_ENABLED 1\n\
        \tisatty stdout; and echo 'lean-ctx: ON (track mode — output unchanged, token savings recorded)'\n\
        end\n\
        \n\
        function lean-ctx-off\n\
        \tfor _lc_cmd in $_lean_ctx_cmds\n\
        \t\tfunctions --erase $_lc_cmd 2>/dev/null; true\n\
        \tend\n\
        \tfunctions --erase k 2>/dev/null; true\n\
        \tset -gx LEAN_CTX_ENABLED 0\n\
        \tisatty stdout; and echo 'lean-ctx: OFF'\n\
        end\n\
        \n\
        function lean-ctx-mode\n\
        \tswitch $argv[1]\n\
        \t\tcase compress\n\
        \t\t\tfor _lc_cmd in $_lean_ctx_cmds\n\
        \t\t\t\talias $_lc_cmd '_lc_compress '$_lc_cmd\n\
        \t\t\t\tend\n\
        \t\t\talias k '_lc_compress kubectl'\n\
        \t\t\tset -gx LEAN_CTX_ENABLED 1\n\
        \t\t\tisatty stdout; and echo 'lean-ctx: COMPRESS mode (all output compressed)'\n\
        \t\tcase track\n\
        \t\t\tlean-ctx-on\n\
        \t\tcase off\n\
        \t\t\tlean-ctx-off\n\
        \t\tcase '*'\n\
        \t\t\techo 'Usage: lean-ctx-mode <track|compress|off>'\n\
        \t\t\techo '  track    — Full output, stats recorded (default)'\n\
        \t\t\techo '  compress — Compressed output for all commands'\n\
        \t\t\techo '  off      — No aliases, raw shell'\n\
        \tend\n\
        end\n\
        \n\
        function lean-ctx-raw\n\
        \tset -lx LEAN_CTX_RAW 1\n\
        \tcommand $argv\n\
        end\n\
        \n\
        function lean-ctx-status\n\
        \tif set -q LEAN_CTX_DISABLED\n\
        \t\tisatty stdout; and echo 'lean-ctx: DISABLED (LEAN_CTX_DISABLED is set)'\n\
        \telse if set -q LEAN_CTX_ENABLED\n\
        \t\tisatty stdout; and echo 'lean-ctx: ON'\n\
        \telse\n\
        \t\tisatty stdout; and echo 'lean-ctx: OFF'\n\
        \tend\n\
        end\n\
        \n\
        function _lean_ctx_should_activate\n\
        \tif set -q LEAN_CTX_ACTIVE; or set -q LEAN_CTX_DISABLED; or test (set -q LEAN_CTX_ENABLED; and echo $LEAN_CTX_ENABLED; or echo 1) = '0'\n\
        \t\treturn 1\n\
        \tend\n\
        \tset -l _lc_mode (set -q LEAN_CTX_SHELL_ACTIVATION; and echo $LEAN_CTX_SHELL_ACTIVATION; or echo '{baked_default}')\n\
        \tswitch $_lc_mode\n\
        \t\tcase off none manual\n\
        \t\t\treturn 1\n\
        \t\tcase 'agents-only' agents_only agentsonly\n\
        \t\t\tif set -q LEAN_CTX_AGENT; or set -q CLAUDECODE; or set -q CODEBUDDY; or set -q CODEX_CLI_SESSION; or set -q GEMINI_SESSION\n\
        \t\t\t\treturn 0\n\
        \t\t\tend\n\
        \t\t\treturn 1\n\
        \t\tcase '*'\n\
        \t\t\treturn 0\n\
        \tend\n\
        end\n\
        \n\
        if _lean_ctx_should_activate\n\
        \tif command -q lean-ctx\n\
        \t\tlean-ctx-on\n\
        \tend\n\
        end\n"
    )
}

pub fn init_fish(binary: &str) {
    let config = dirs::home_dir()
        .map(|h| h.join(".config/fish/config.fish"))
        .unwrap_or_default();

    let hook_content = generate_hook_fish(binary);

    if write_hook_file("shell-hook.fish", &hook_content).is_some() {
        upsert_source_line(&config, &source_line_fish());
        qprintln!("  Binary: {binary}");
    }
}

#[must_use]
pub fn generate_hook_posix(binary: &str) -> String {
    let config = crate::core::config::Config::load();
    let activation = config.shell_activation_effective();
    let baked_default = match activation {
        crate::core::config::ShellActivation::Always => "always",
        crate::core::config::ShellActivation::AgentsOnly => "agents-only",
        crate::core::config::ShellActivation::Off => "off",
    };
    let alias_list = crate::rewrite_registry::shell_alias_list();
    format!(
        r#"# lean-ctx shell hook — smart shell mode (track-by-default)
_lean_ctx_cmds=({alias_list})

_lc_is_agent() {{
    [ -n "${{LEAN_CTX_AGENT:-}}" ] || [ -n "${{CODEX_CLI_SESSION:-}}" ] || [ -n "${{CLAUDECODE:-}}" ] || [ -n "${{CODEBUDDY:-}}" ] || [ -n "${{GEMINI_SESSION:-}}" ]
}}

_lc() {{
    if [ -n "${{LEAN_CTX_DISABLED:-}}" ] || [ -n "${{LEAN_CTX_NO_HOOK:-}}" ]; then
        command "$@"
        return
    fi
    if [ ! -t 1 ] && ! _lc_is_agent; then
        command "$@"
        return
    fi
    '{binary}' -t "$@"
    local _lc_rc=$?
    if [ "$_lc_rc" -eq 127 ] || [ "$_lc_rc" -eq 126 ]; then
        command "$@"
    else
        return "$_lc_rc"
    fi
}}

_lc_compress() {{
    if [ -n "${{LEAN_CTX_DISABLED:-}}" ] || [ -n "${{LEAN_CTX_NO_HOOK:-}}" ]; then
        command "$@"
        return
    fi
    if [ ! -t 1 ] && ! _lc_is_agent; then
        command "$@"
        return
    fi
    '{binary}' -c "$@"
    local _lc_rc=$?
    if [ "$_lc_rc" -eq 127 ] || [ "$_lc_rc" -eq 126 ]; then
        command "$@"
    else
        return "$_lc_rc"
    fi
}}

lean-ctx-on() {{
    for _lc_cmd in "${{_lean_ctx_cmds[@]}}"; do
        # shellcheck disable=SC2139
        alias "$_lc_cmd"='_lc '"$_lc_cmd"
    done
    alias k='_lc kubectl'
    export LEAN_CTX_ENABLED=1
    [ -t 1 ] && echo "lean-ctx: ON (track mode — output unchanged, token savings recorded)"
}}

lean-ctx-off() {{
    for _lc_cmd in "${{_lean_ctx_cmds[@]}}"; do
        unalias "$_lc_cmd" 2>/dev/null || true
    done
    unalias k 2>/dev/null || true
    export LEAN_CTX_ENABLED=0
    [ -t 1 ] && echo "lean-ctx: OFF"
}}

lean-ctx-mode() {{
    case "${{1:-}}" in
        compress)
            for _lc_cmd in "${{_lean_ctx_cmds[@]}}"; do
                # shellcheck disable=SC2139
                alias "$_lc_cmd"='_lc_compress '"$_lc_cmd"
            done
            alias k='_lc_compress kubectl'
            export LEAN_CTX_ENABLED=1
            [ -t 1 ] && echo "lean-ctx: COMPRESS mode (all output compressed)"
            ;;
        track)
            lean-ctx-on
            ;;
        off)
            lean-ctx-off
            ;;
        *)
            echo "Usage: lean-ctx-mode <track|compress|off>"
            echo "  track    — Full output, stats recorded (default)"
            echo "  compress — Compressed output for all commands"
            echo "  off      — No aliases, raw shell"
            ;;
    esac
}}

lean-ctx-raw() {{
    LEAN_CTX_RAW=1 command "$@"
}}

lean-ctx-status() {{
    if [ -n "${{LEAN_CTX_DISABLED:-}}" ]; then
        [ -t 1 ] && echo "lean-ctx: DISABLED (LEAN_CTX_DISABLED is set)"
    elif [ -n "${{LEAN_CTX_ENABLED:-}}" ]; then
        [ -t 1 ] && echo "lean-ctx: ON"
    else
        [ -t 1 ] && echo "lean-ctx: OFF"
    fi
}}

if [ -n "${{ZSH_VERSION:-}}" ]; then
    _lean_ctx_comp() {{
        shift words
        (( CURRENT-- ))
        _normal
    }}
    compdef _lean_ctx_comp _lc 2>/dev/null
    compdef _lean_ctx_comp _lc_compress 2>/dev/null
fi

_lean_ctx_should_activate() {{
    [ -z "${{LEAN_CTX_ACTIVE:-}}" ] && [ -z "${{LEAN_CTX_DISABLED:-}}" ] && [ "${{LEAN_CTX_ENABLED:-1}}" != "0" ] || return 1
    case "${{LEAN_CTX_SHELL_ACTIVATION:-{baked_default}}}" in
        off|none|manual) return 1 ;;
        agents-only|agents_only|agentsonly)
            [ -n "${{LEAN_CTX_AGENT:-}}" ] || [ -n "${{CLAUDECODE:-}}" ] || [ -n "${{CODEBUDDY:-}}" ] || [ -n "${{CODEX_CLI_SESSION:-}}" ] || [ -n "${{GEMINI_SESSION:-}}" ] ;;
        *) return 0 ;;
    esac
}}

if _lean_ctx_should_activate; then
    command -v lean-ctx >/dev/null 2>&1 && lean-ctx-on
fi
"#
    )
}

pub fn init_posix(is_zsh: bool, binary: &str) {
    let rc_file = if is_zsh {
        dirs::home_dir()
            .map(|h| h.join(".zshrc"))
            .unwrap_or_default()
    } else {
        dirs::home_dir()
            .map(|h| h.join(".bashrc"))
            .unwrap_or_default()
    };

    let shell_ext = if is_zsh { "zsh" } else { "bash" };
    let hook_content = generate_hook_posix(binary);

    if let Some(hook_path) = write_hook_file(&format!("shell-hook.{shell_ext}"), &hook_content) {
        upsert_source_line(&rc_file, &source_line_posix(shell_ext));

        // Bash login shells don't read ~/.bashrc — make sure they pick it up so the hook
        // (and the installer's PATH export) take effect in Terminal.app / IDE login shells.
        if !is_zsh {
            ensure_bash_login_sources_bashrc();
        }

        qprintln!("  Binary: {binary}");

        write_env_sh_for_containers(&hook_content);
        print_docker_env_hints(is_zsh);

        let _ = hook_path;
    }
}

/// Bash login shells (macOS Terminal.app, many IDE terminals, `bash -l`) read
/// `~/.bash_profile` (or `~/.bash_login` / `~/.profile`) and never `~/.bashrc`. Because we
/// install the hook — and the installer adds `~/.local/bin` to PATH — into `~/.bashrc`, a login
/// shell would otherwise see neither. Ensure the login profile sources `~/.bashrc`, exactly as
/// the Debian/Ubuntu default `.profile` does. Idempotent; zsh is unaffected (it always reads
/// `~/.zshrc`), so this is only wired in for bash.
fn ensure_bash_login_sources_bashrc() {
    let Some(home) = dirs::home_dir() else {
        return;
    };

    // Bash reads only the FIRST existing of these on login; target that one, else create
    // ~/.bash_profile. (~/.bashrc is never a login file, so it's not a candidate.)
    let target = [".bash_profile", ".bash_login", ".profile"]
        .iter()
        .map(|f| home.join(f))
        .find(|p| p.exists())
        .unwrap_or_else(|| home.join(".bash_profile"));

    // Already sourcing ~/.bashrc (our snippet or the user's own)? Nothing to do.
    if let Ok(existing) = std::fs::read_to_string(&target) {
        let sources_bashrc = existing
            .lines()
            .any(|l| !l.trim_start().starts_with('#') && l.contains(".bashrc"));
        if sources_bashrc {
            return;
        }
    }

    let snippet = "\n# lean-ctx: load ~/.bashrc in login shells (e.g. macOS Terminal) — begin\n\
         if [ -f \"$HOME/.bashrc\" ]; then . \"$HOME/.bashrc\"; fi\n\
         # lean-ctx: load ~/.bashrc in login shells (e.g. macOS Terminal) — end\n";

    backup_shell_config(&target);
    match std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&target)
    {
        Ok(mut f) => {
            use std::io::Write;
            if f.write_all(snippet.as_bytes()).is_ok() {
                qprintln!("  Login shell: {} now sources ~/.bashrc", target.display());
            }
        }
        Err(e) => {
            tracing::warn!("could not update {}: {e}", target.display());
        }
    }
}

pub fn write_env_sh_for_containers(aliases: &str) {
    // env.sh is a config artifact (sourced via BASH_ENV/CLAUDE_ENV_FILE) → config_dir (#408).
    let env_sh = match crate::core::paths::config_dir() {
        Ok(d) => d.join("env.sh"),
        Err(_) => return,
    };
    if let Some(parent) = env_sh.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let sanitized_aliases = crate::core::sanitize::neutralize_shell_content(aliases);
    let mut content = String::from(
        r#"# lean-ctx: passthrough stubs for non-interactive subshells (fixes #255).
# These ensure _lc/_lc_compress exist so inherited aliases don't break.
# The full hook definitions override these when the interactive shell loads.
_lc()          { command "$@"; }
_lc_compress() { command "$@"; }

"#,
    );
    content.push_str(&sanitized_aliases);
    content.push_str(
        r#"

# lean-ctx docker self-heal: re-inject Claude MCP config if Claude overwrote ~/.claude.json
# Guards: container-only + no recursion + no re-entry via BASH_ENV + 60s cooldown + PID-lock
if [ -f /.dockerenv ] || grep -qsE '/docker/|/lxc/' /proc/1/cgroup 2>/dev/null; then
  if [ -z "${LEAN_CTX_ACTIVE:-}" ] && [ -z "${_LEAN_CTX_HEAL:-}" ]; then
    # XDG-only paths (GL #623): never touch ~/.lean-ctx, which would re-collapse
    # a committed XDG layout. heal_ts is STATE, locks live in the DATA dir
    # (matches process_guard::lock_dir defaults).
    _LEAN_CTX_STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/lean-ctx"
    _LEAN_CTX_HEAL_TS="${_LEAN_CTX_STATE_DIR}/.heal_ts"
    _LEAN_CTX_HEAL_COOLDOWN=60
    _lean_ctx_heal_needed=1
    if [ -f "$_LEAN_CTX_HEAL_TS" ]; then
      _last_heal=$(cat "$_LEAN_CTX_HEAL_TS" 2>/dev/null || echo 0)
      _now=$(date +%s 2>/dev/null || echo 0)
      if [ $(( _now - _last_heal )) -lt $_LEAN_CTX_HEAL_COOLDOWN ]; then
        _lean_ctx_heal_needed=0
      fi
    fi
    _lean_ctx_lock_count=0
    for _lf in "${XDG_DATA_HOME:-$HOME/.local/share}/lean-ctx/locks"/slot-*.lock; do
      [ -f "$_lf" ] && _lean_ctx_lock_count=$(( _lean_ctx_lock_count + 1 ))
    done
    if [ "$_lean_ctx_heal_needed" = "1" ] && [ "$_lean_ctx_lock_count" -lt 4 ]; then
      export _LEAN_CTX_HEAL=1
      if command -v claude >/dev/null 2>&1 && command -v lean-ctx >/dev/null 2>&1; then
        if ! claude mcp list 2>/dev/null | grep -q "lean-ctx"; then
          LEAN_CTX_ACTIVE=1 LEAN_CTX_QUIET=1 lean-ctx init --agent claude >/dev/null 2>&1
          mkdir -p "$_LEAN_CTX_STATE_DIR" 2>/dev/null
          date +%s > "$_LEAN_CTX_HEAL_TS" 2>/dev/null
        fi
      fi
    fi
  fi
fi
"#,
    );
    match std::fs::write(&env_sh, content) {
        Ok(()) => {
            // Keep JSON-mode stdout clean; non-quiet hints go to stderr.
            if !super::quiet_enabled() {
                eprintln!("  env.sh: {}", env_sh.display());
            }
        }
        Err(e) => tracing::warn!("could not write {}: {e}", env_sh.display()),
    }
}

fn print_docker_env_hints(is_zsh: bool) {
    if is_zsh || !crate::shell::is_container() {
        return;
    }
    let env_sh = crate::core::paths::config_dir().map_or_else(
        |_| "/root/.config/lean-ctx/env.sh".to_string(),
        |d| d.join("env.sh").to_string_lossy().to_string(),
    );

    let has_bash_env = std::env::var("BASH_ENV").is_ok();
    let has_claude_env = std::env::var("CLAUDE_ENV_FILE").is_ok();

    if has_bash_env && has_claude_env {
        return;
    }

    eprintln!();
    eprintln!("  \x1b[33m⚠  Docker detected — environment hints:\x1b[0m");

    if !has_bash_env {
        eprintln!("  For generic bash -c usage (non-interactive shells):");
        eprintln!("    \x1b[1mENV BASH_ENV=\"{env_sh}\"\x1b[0m");
    }
    if !has_claude_env {
        eprintln!("  For Claude Code (sources before each command):");
        eprintln!("    \x1b[1mENV CLAUDE_ENV_FILE=\"{env_sh}\"\x1b[0m");
    }
    eprintln!();
}

#[must_use]
pub fn remove_lean_ctx_block(content: &str) -> String {
    if content.contains("# lean-ctx shell hook — end") {
        return remove_lean_ctx_block_by_marker(content);
    }
    remove_lean_ctx_block_legacy(content)
}

fn remove_lean_ctx_block_by_marker(content: &str) -> String {
    let mut result = String::new();
    let mut in_block = false;

    for line in content.lines() {
        if !in_block && line.contains("lean-ctx shell hook") && !line.contains("end") {
            in_block = true;
            continue;
        }
        if in_block {
            if line.trim() == "# lean-ctx shell hook — end" {
                in_block = false;
            }
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

fn remove_lean_ctx_block_legacy(content: &str) -> String {
    let mut result = String::new();
    let mut in_block = false;

    for line in content.lines() {
        if line.contains("lean-ctx shell hook") {
            in_block = true;
            continue;
        }
        if in_block {
            if line.trim() == "fi" || line.trim() == "end" || line.trim().is_empty() {
                if line.trim() == "fi" || line.trim() == "end" {
                    in_block = false;
                }
                continue;
            }
            if !line.starts_with("alias ") && !line.starts_with('\t') && !line.starts_with("if ") {
                in_block = false;
                result.push_str(line);
                result.push('\n');
            }
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remove_lean_ctx_block_posix() {
        let input = r#"# existing config
export PATH="$HOME/bin:$PATH"

# lean-ctx shell hook — transparent CLI compression (95+ patterns)
if [ -z "$LEAN_CTX_ACTIVE" ]; then
alias git='lean-ctx -c git'
alias npm='lean-ctx -c npm'
fi

# other stuff
export EDITOR=vim
"#;
        let result = remove_lean_ctx_block(input);
        assert!(!result.contains("lean-ctx"), "block should be removed");
        assert!(result.contains("export PATH"), "other content preserved");
        assert!(
            result.contains("export EDITOR"),
            "trailing content preserved"
        );
    }

    #[test]
    fn test_remove_lean_ctx_block_fish() {
        let input = "# other fish config\nset -x FOO bar\n\n# lean-ctx shell hook — transparent CLI compression (95+ patterns)\nif not set -q LEAN_CTX_ACTIVE\n\talias git 'lean-ctx -c git'\n\talias npm 'lean-ctx -c npm'\nend\n\n# more config\nset -x BAZ qux\n";
        let result = remove_lean_ctx_block(input);
        assert!(!result.contains("lean-ctx"), "block should be removed");
        assert!(result.contains("set -x FOO"), "other content preserved");
        assert!(result.contains("set -x BAZ"), "trailing content preserved");
    }

    #[test]
    fn test_remove_lean_ctx_block_ps() {
        let input = "# PowerShell profile\n$env:FOO = 'bar'\n\n# lean-ctx shell hook — transparent CLI compression (95+ patterns)\nif (-not $env:LEAN_CTX_ACTIVE) {\n  $LeanCtxBin = \"C:\\\\bin\\\\lean-ctx.exe\"\n  function git { & $LeanCtxBin -c \"git $($args -join ' ')\" }\n}\n\n# other stuff\n$env:EDITOR = 'vim'\n";
        let result = remove_lean_ctx_block_ps(input);
        assert!(
            !result.contains("lean-ctx shell hook"),
            "block should be removed"
        );
        assert!(result.contains("$env:FOO"), "other content preserved");
        assert!(result.contains("$env:EDITOR"), "trailing content preserved");
    }

    #[test]
    fn test_remove_lean_ctx_block_ps_nested() {
        let input = "# PowerShell profile\n$env:FOO = 'bar'\n\n# lean-ctx shell hook — transparent CLI compression (95+ patterns)\nif (-not $env:LEAN_CTX_ACTIVE) {\n  $LeanCtxBin = \"lean-ctx\"\n  function _lc {\n    & $LeanCtxBin -c \"$($args -join ' ')\"\n  }\n  if (Get-Command lean-ctx -ErrorAction SilentlyContinue) {\n    function git { _lc git @args }\n    foreach ($c in @('npm','pnpm')) {\n      if ($a) {\n        Set-Variable -Name \"_lc_$c\" -Value $a.Source -Scope Script\n      }\n    }\n  }\n}\n\n# other stuff\n$env:EDITOR = 'vim'\n";
        let result = remove_lean_ctx_block_ps(input);
        assert!(
            !result.contains("lean-ctx shell hook"),
            "block should be removed"
        );
        assert!(!result.contains("_lc"), "function should be removed");
        assert!(result.contains("$env:FOO"), "other content preserved");
        assert!(result.contains("$env:EDITOR"), "trailing content preserved");
    }

    #[test]
    fn test_remove_block_no_lean_ctx() {
        let input = "# normal bashrc\nexport PATH=\"$HOME/bin:$PATH\"\n";
        let result = remove_lean_ctx_block(input);
        assert!(result.contains("export PATH"), "content unchanged");
    }

    #[test]
    fn test_bash_hook_contains_pipe_guard_and_agent_bypass() {
        let output = generate_hook_posix("/usr/local/bin/lean-ctx");
        assert!(
            output.contains("! -t 1"),
            "bash/zsh hook must contain pipe guard [ ! -t 1 ]"
        );
        assert!(
            output.contains("_lc_is_agent"),
            "bash/zsh hook must have agent-aware bypass"
        );
        assert!(
            output.contains("CODEX_CLI_SESSION"),
            "agent check must include CODEX_CLI_SESSION"
        );
    }

    #[test]
    fn test_lc_uses_track_mode_by_default() {
        let binary = "/usr/local/bin/lean-ctx";
        let alias_list = crate::rewrite_registry::shell_alias_list();
        let aliases = format!(
            r#"_lc() {{
    '{binary}' -t "$@"
}}
_lc_compress() {{
    '{binary}' -c "$@"
}}"#
        );
        assert!(
            aliases.contains("-t \"$@\""),
            "_lc must use -t (track mode) by default"
        );
        assert!(
            aliases.contains("-c \"$@\""),
            "_lc_compress must use -c (compress mode)"
        );
        let _ = alias_list;
    }

    #[test]
    fn test_posix_shell_has_lean_ctx_mode() {
        let alias_list = crate::rewrite_registry::shell_alias_list();
        let aliases = r#"
lean-ctx-mode() {{
    case "${{1:-}}" in
        compress) echo compress ;;
        track) echo track ;;
        off) echo off ;;
    esac
}}
"#
        .to_string();
        assert!(
            aliases.contains("lean-ctx-mode()"),
            "lean-ctx-mode function must exist"
        );
        assert!(
            aliases.contains("compress"),
            "compress mode must be available"
        );
        assert!(aliases.contains("track"), "track mode must be available");
        let _ = alias_list;
    }

    #[test]
    fn test_fish_hook_contains_pipe_guard_and_agent_bypass() {
        let output = generate_hook_fish("/usr/local/bin/lean-ctx");
        assert!(
            output.contains("isatty stdout"),
            "fish hook must contain pipe guard (isatty stdout)"
        );
        assert!(
            output.contains("_lc_is_agent"),
            "fish hook must have agent-aware bypass"
        );
    }

    #[test]
    fn test_powershell_hook_contains_pipe_guard() {
        let hook = "function _lc { if ($env:LEAN_CTX_DISABLED -or [Console]::IsOutputRedirected) { & @args; return } }";
        assert!(
            hook.contains("IsOutputRedirected"),
            "PowerShell hook must contain pipe guard ([Console]::IsOutputRedirected)"
        );
    }

    #[test]
    fn powershell_hook_binary_is_native_not_msys() {
        // #518: PowerShell/pwsh execute the path via the `&` call operator and
        // cannot run an MSYS `/c/...` path — they must get the native binary.
        let win = "C:/Users/Dawid/.cargo/bin/lean-ctx.exe";
        assert_eq!(hook_binary_for_shell("powershell", win), win);
        assert_eq!(hook_binary_for_shell("pwsh", win), win);
        assert!(!hook_binary_for_shell("powershell", win).contains("/c/"));
    }

    #[test]
    fn posix_hook_binary_keeps_msys_form_on_windows_drive() {
        // bash/zsh/fish source the hook from a POSIX shell, so a Windows drive
        // path is converted to the MSYS `/c/...` form for them.
        let win = "C:/Users/Dawid/.cargo/bin/lean-ctx.exe";
        let msys = "/c/Users/Dawid/.cargo/bin/lean-ctx.exe";
        assert_eq!(hook_binary_for_shell("bash", win), msys);
        assert_eq!(hook_binary_for_shell("zsh", win), msys);
        assert_eq!(hook_binary_for_shell("fish", win), msys);
    }

    #[test]
    fn test_remove_lean_ctx_block_new_format_with_end_marker() {
        let input = r#"# existing config
export PATH="$HOME/bin:$PATH"

# lean-ctx shell hook — transparent CLI compression (95+ patterns)
_lean_ctx_cmds=(git npm pnpm)

lean-ctx-on() {
    for _lc_cmd in "${_lean_ctx_cmds[@]}"; do
        alias "$_lc_cmd"='lean-ctx -c '"$_lc_cmd"
    done
    export LEAN_CTX_ENABLED=1
    [ -t 1 ] && echo "lean-ctx: ON"
}

lean-ctx-off() {
    export LEAN_CTX_ENABLED=0
    [ -t 1 ] && echo "lean-ctx: OFF"
}

if [ -z "${LEAN_CTX_ACTIVE:-}" ] && [ "${LEAN_CTX_ENABLED:-1}" != "0" ]; then
    lean-ctx-on
fi
# lean-ctx shell hook — end

# other stuff
export EDITOR=vim
"#;
        let result = remove_lean_ctx_block(input);
        assert!(!result.contains("lean-ctx-on"), "block should be removed");
        assert!(!result.contains("lean-ctx shell hook"), "marker removed");
        assert!(result.contains("export PATH"), "other content preserved");
        assert!(
            result.contains("export EDITOR"),
            "trailing content preserved"
        );
    }

    #[test]
    fn env_sh_for_containers_includes_self_heal() {
        let _g = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        // env.sh is a config artifact (#408) → written under config_dir().
        let config_dir = tmp.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("mkdir config");
        crate::test_env::set_var("LEAN_CTX_CONFIG_DIR", &config_dir);

        write_env_sh_for_containers("alias git='lean-ctx -c git'\n");
        let env_sh = config_dir.join("env.sh");
        let content = std::fs::read_to_string(&env_sh).expect("env.sh exists");
        if !cfg!(windows)
            && let Ok(mut bash) = std::process::Command::new("bash")
                .arg("-n")
                .arg(&env_sh)
                .spawn()
        {
            let ok = bash.wait().is_ok_and(|s| s.success());
            assert!(ok, "generated env.sh must be valid bash");
        }
        assert!(
            content.contains(r#"_lc()          { command "$@"; }"#),
            "env.sh must contain _lc passthrough stub for non-interactive shells"
        );
        assert!(
            content.contains(r#"_lc_compress() { command "$@"; }"#),
            "env.sh must contain _lc_compress passthrough stub"
        );
        assert!(content.contains("lean-ctx docker self-heal"));
        assert!(content.contains("claude mcp list"));
        assert!(content.contains("lean-ctx init --agent claude"));
        assert!(
            content.contains("_LEAN_CTX_HEAL"),
            "env.sh must guard against recursive self-heal"
        );
        assert!(
            content.contains("LEAN_CTX_ACTIVE"),
            "env.sh must check LEAN_CTX_ACTIVE to prevent re-entry"
        );
        assert!(
            content.contains("/.dockerenv"),
            "env.sh self-heal must be gated to container environments"
        );
        // GL #623/#627: the self-heal must never create or read ~/.lean-ctx,
        // which would re-collapse a committed XDG layout. heal_ts → XDG state,
        // lock count → XDG data.
        assert!(
            !content.contains("$HOME/.lean-ctx") && !content.contains("${HOME}/.lean-ctx"),
            "self-heal must not touch ~/.lean-ctx (GL #623)"
        );
        assert!(
            content.contains("${XDG_STATE_HOME:-$HOME/.local/state}/lean-ctx"),
            "heal_ts must live under the XDG state dir"
        );
        assert!(
            content.contains("${XDG_DATA_HOME:-$HOME/.local/share}/lean-ctx/locks"),
            "lock count must read the XDG data lock dir"
        );

        crate::test_env::remove_var("LEAN_CTX_CONFIG_DIR");
    }

    #[cfg(unix)]
    #[test]
    fn bash_login_profile_sources_bashrc_idempotently() {
        let _g = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        let prev = std::env::var_os("HOME");
        crate::test_env::set_var("HOME", home);

        std::fs::write(home.join(".bashrc"), "# bashrc\n").expect("write .bashrc");
        // No login profile yet → the function must create ~/.bash_profile.

        ensure_bash_login_sources_bashrc();
        let profile = home.join(".bash_profile");
        let first = std::fs::read_to_string(&profile).expect(".bash_profile created");
        assert!(
            first.contains(". \"$HOME/.bashrc\""),
            "login profile must source ~/.bashrc: {first}"
        );
        let markers = first.matches("load ~/.bashrc in login shells").count();

        // Second run is a no-op: it already sources ~/.bashrc.
        ensure_bash_login_sources_bashrc();
        let second = std::fs::read_to_string(&profile).expect("read profile");
        assert_eq!(
            second.matches("load ~/.bashrc in login shells").count(),
            markers,
            "snippet must not be duplicated on re-run"
        );

        match prev {
            Some(v) => crate::test_env::set_var("HOME", v),
            None => crate::test_env::remove_var("HOME"),
        }
    }

    #[test]
    fn test_source_line_posix() {
        let line = source_line_posix("zsh");
        assert!(line.contains("shell-hook.zsh"));
        assert!(line.contains("[ -f"));
    }

    #[test]
    fn test_source_line_fish() {
        let line = source_line_fish();
        assert!(line.contains("shell-hook.fish"));
        assert!(line.contains("source"));
    }

    #[test]
    fn test_source_line_powershell() {
        let line = source_line_powershell();
        assert!(line.contains("shell-hook.ps1"));
        assert!(line.contains("Test-Path"));
    }
}
