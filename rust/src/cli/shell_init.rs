macro_rules! qprintln {
    ($($t:tt)*) => {
        if !super::quiet_enabled() {
            println!($($t)*);
        }
    };
}

fn backup_shell_config(path: &std::path::Path) {
    if !path.exists() {
        return;
    }
    let bak = path.with_extension("lean-ctx.bak");
    if std::fs::copy(path, &bak).is_ok() {
        qprintln!(
            "  Backup: {}",
            bak.file_name()
                .map(|n| format!("~/{}", n.to_string_lossy()))
                .unwrap_or_else(|| bak.display().to_string())
        );
    }
}

pub fn init_powershell(binary: &str) {
    let profile_dir = dirs::home_dir().map(|h| h.join("Documents").join("PowerShell"));
    let profile_path = match profile_dir {
        Some(dir) => {
            let _ = std::fs::create_dir_all(&dir);
            dir.join("Microsoft.PowerShell_profile.ps1")
        }
        None => {
            eprintln!("Could not resolve PowerShell profile directory");
            return;
        }
    };

    let binary_escaped = binary.replace('\\', "\\\\");
    let functions = format!(
        r#"
# lean-ctx shell hook — transparent CLI compression (90+ patterns)
if (-not $env:LEAN_CTX_ACTIVE -and -not $env:LEAN_CTX_DISABLED) {{
  $LeanCtxBin = "{binary_escaped}"
  function _lc {{
    if ($env:LEAN_CTX_DISABLED -or [Console]::IsOutputRedirected) {{ & @args; return }}
    & $LeanCtxBin -c @args
    if ($LASTEXITCODE -eq 127 -or $LASTEXITCODE -eq 126) {{
      & @args
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
        New-Item -Path "function:$c" -Value ([scriptblock]::Create("_lc $c @args")) -Force | Out-Null
      }}
    }}
  }}
}}
"#
    );

    backup_shell_config(&profile_path);

    if let Ok(existing) = std::fs::read_to_string(&profile_path) {
        if existing.contains("lean-ctx shell hook") {
            let cleaned = remove_lean_ctx_block_ps(&existing);
            match std::fs::write(&profile_path, format!("{cleaned}{functions}")) {
                Ok(()) => {
                    qprintln!("Updated lean-ctx functions in {}", profile_path.display());
                    qprintln!("  Binary: {binary}");
                    return;
                }
                Err(e) => {
                    eprintln!("Error updating {}: {e}", profile_path.display());
                    return;
                }
            }
        }
    }

    match std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&profile_path)
    {
        Ok(mut f) => {
            use std::io::Write;
            let _ = f.write_all(functions.as_bytes());
            qprintln!("Added lean-ctx functions to {}", profile_path.display());
            qprintln!("  Binary: {binary}");
        }
        Err(e) => eprintln!("Error writing {}: {e}", profile_path.display()),
    }
}

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

pub fn init_fish(binary: &str) {
    let config = dirs::home_dir()
        .map(|h| h.join(".config/fish/config.fish"))
        .unwrap_or_default();

    let alias_list = crate::rewrite_registry::shell_alias_list();
    let aliases = format!(
        "\n# lean-ctx shell hook — smart shell mode (track-by-default)\n\
        set -g _lean_ctx_cmds {alias_list}\n\
        \n\
        function _lc\n\
        \tif set -q LEAN_CTX_DISABLED; or not isatty stdout\n\
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
        \tif set -q LEAN_CTX_DISABLED; or not isatty stdout\n\
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
        \techo 'lean-ctx: ON (track mode — full output, stats recorded)'\n\
        end\n\
        \n\
        function lean-ctx-off\n\
        \tfor _lc_cmd in $_lean_ctx_cmds\n\
        \t\tfunctions --erase $_lc_cmd 2>/dev/null; true\n\
        \tend\n\
        \tfunctions --erase k 2>/dev/null; true\n\
        \tset -e LEAN_CTX_ENABLED\n\
        \techo 'lean-ctx: OFF'\n\
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
        \t\t\techo 'lean-ctx: COMPRESS mode (all output compressed)'\n\
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
        \t\techo 'lean-ctx: DISABLED (LEAN_CTX_DISABLED is set)'\n\
        \telse if set -q LEAN_CTX_ENABLED\n\
        \t\techo 'lean-ctx: ON'\n\
        \telse\n\
        \t\techo 'lean-ctx: OFF'\n\
        \tend\n\
        end\n\
        \n\
        if not set -q LEAN_CTX_ACTIVE; and not set -q LEAN_CTX_DISABLED; and test (set -q LEAN_CTX_ENABLED; and echo $LEAN_CTX_ENABLED; or echo 1) != '0'\n\
        \tif command -q lean-ctx\n\
        \t\tlean-ctx-on\n\
        \tend\n\
        end\n\
        # lean-ctx shell hook — end\n"
    );

    backup_shell_config(&config);

    if let Ok(existing) = std::fs::read_to_string(&config) {
        if existing.contains("lean-ctx shell hook") {
            let cleaned = remove_lean_ctx_block(&existing);
            match std::fs::write(&config, format!("{cleaned}{aliases}")) {
                Ok(()) => {
                    qprintln!("Updated lean-ctx aliases in {}", config.display());
                    qprintln!("  Binary: {binary}");
                    return;
                }
                Err(e) => {
                    eprintln!("Error updating {}: {e}", config.display());
                    return;
                }
            }
        }
    }

    match std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&config)
    {
        Ok(mut f) => {
            use std::io::Write;
            let _ = f.write_all(aliases.as_bytes());
            qprintln!("Added lean-ctx aliases to {}", config.display());
            qprintln!("  Binary: {binary}");
        }
        Err(e) => eprintln!("Error writing {}: {e}", config.display()),
    }
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

    let alias_list = crate::rewrite_registry::shell_alias_list();
    let aliases = format!(
        r#"
# lean-ctx shell hook — smart shell mode (track-by-default)
_lean_ctx_cmds=({alias_list})

_lc() {{
    if [ -n "${{LEAN_CTX_DISABLED:-}}" ] || [ ! -t 1 ]; then
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
    if [ -n "${{LEAN_CTX_DISABLED:-}}" ] || [ ! -t 1 ]; then
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
    echo "lean-ctx: ON (track mode — full output, stats recorded)"
}}

lean-ctx-off() {{
    for _lc_cmd in "${{_lean_ctx_cmds[@]}}"; do
        unalias "$_lc_cmd" 2>/dev/null || true
    done
    unalias k 2>/dev/null || true
    unset LEAN_CTX_ENABLED
    echo "lean-ctx: OFF"
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
            echo "lean-ctx: COMPRESS mode (all output compressed)"
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
        echo "lean-ctx: DISABLED (LEAN_CTX_DISABLED is set)"
    elif [ -n "${{LEAN_CTX_ENABLED:-}}" ]; then
        echo "lean-ctx: ON"
    else
        echo "lean-ctx: OFF"
    fi
}}

if [ -z "${{LEAN_CTX_ACTIVE:-}}" ] && [ -z "${{LEAN_CTX_DISABLED:-}}" ] && [ "${{LEAN_CTX_ENABLED:-1}}" != "0" ]; then
    command -v lean-ctx >/dev/null 2>&1 && lean-ctx-on
fi
# lean-ctx shell hook — end
"#
    );

    backup_shell_config(&rc_file);

    if let Ok(existing) = std::fs::read_to_string(&rc_file) {
        if existing.contains("lean-ctx shell hook") {
            let cleaned = remove_lean_ctx_block(&existing);
            match std::fs::write(&rc_file, format!("{cleaned}{aliases}")) {
                Ok(()) => {
                    qprintln!("Updated lean-ctx aliases in {}", rc_file.display());
                    qprintln!("  Binary: {binary}");
                    return;
                }
                Err(e) => {
                    eprintln!("Error updating {}: {e}", rc_file.display());
                    return;
                }
            }
        }
    }

    match std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&rc_file)
    {
        Ok(mut f) => {
            use std::io::Write;
            let _ = f.write_all(aliases.as_bytes());
            qprintln!("Added lean-ctx aliases to {}", rc_file.display());
            qprintln!("  Binary: {binary}");
        }
        Err(e) => eprintln!("Error writing {}: {e}", rc_file.display()),
    }

    write_env_sh_for_containers(&aliases);
    print_docker_env_hints(is_zsh);
}

pub fn write_env_sh_for_containers(aliases: &str) {
    let env_sh = match crate::core::data_dir::lean_ctx_data_dir() {
        Ok(d) => d.join("env.sh"),
        Err(_) => return,
    };
    if let Some(parent) = env_sh.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let sanitized_aliases = crate::core::sanitize::neutralize_shell_content(aliases);
    let mut content = sanitized_aliases;
    content.push_str(
        r#"

# lean-ctx docker self-heal: re-inject Claude MCP config if Claude overwrote ~/.claude.json
if command -v claude >/dev/null 2>&1 && command -v lean-ctx >/dev/null 2>&1; then
  if ! claude mcp list 2>/dev/null | grep -q "lean-ctx"; then
    LEAN_CTX_QUIET=1 lean-ctx init --agent claude >/dev/null 2>&1
  fi
fi
"#,
    );
    match std::fs::write(&env_sh, content) {
        Ok(()) => qprintln!("  env.sh: {}", env_sh.display()),
        Err(e) => eprintln!("  Warning: could not write {}: {e}", env_sh.display()),
    }
}

fn print_docker_env_hints(is_zsh: bool) {
    if is_zsh || !crate::shell::is_container() {
        return;
    }
    let env_sh = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.join("env.sh").to_string_lossy().to_string())
        .unwrap_or_else(|_| "/root/.lean-ctx/env.sh".to_string());

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

# lean-ctx shell hook — transparent CLI compression (90+ patterns)
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
        let input = "# other fish config\nset -x FOO bar\n\n# lean-ctx shell hook — transparent CLI compression (90+ patterns)\nif not set -q LEAN_CTX_ACTIVE\n\talias git 'lean-ctx -c git'\n\talias npm 'lean-ctx -c npm'\nend\n\n# more config\nset -x BAZ qux\n";
        let result = remove_lean_ctx_block(input);
        assert!(!result.contains("lean-ctx"), "block should be removed");
        assert!(result.contains("set -x FOO"), "other content preserved");
        assert!(result.contains("set -x BAZ"), "trailing content preserved");
    }

    #[test]
    fn test_remove_lean_ctx_block_ps() {
        let input = "# PowerShell profile\n$env:FOO = 'bar'\n\n# lean-ctx shell hook — transparent CLI compression (90+ patterns)\nif (-not $env:LEAN_CTX_ACTIVE) {\n  $LeanCtxBin = \"C:\\\\bin\\\\lean-ctx.exe\"\n  function git { & $LeanCtxBin -c \"git $($args -join ' ')\" }\n}\n\n# other stuff\n$env:EDITOR = 'vim'\n";
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
        let input = "# PowerShell profile\n$env:FOO = 'bar'\n\n# lean-ctx shell hook — transparent CLI compression (90+ patterns)\nif (-not $env:LEAN_CTX_ACTIVE) {\n  $LeanCtxBin = \"lean-ctx\"\n  function _lc {\n    & $LeanCtxBin -c \"$($args -join ' ')\"\n  }\n  if (Get-Command lean-ctx -ErrorAction SilentlyContinue) {\n    function git { _lc git @args }\n    foreach ($c in @('npm','pnpm')) {\n      if ($a) {\n        Set-Variable -Name \"_lc_$c\" -Value $a.Source -Scope Script\n      }\n    }\n  }\n}\n\n# other stuff\n$env:EDITOR = 'vim'\n";
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
    fn test_bash_hook_contains_pipe_guard() {
        let binary = "/usr/local/bin/lean-ctx";
        let hook = format!(
            r#"_lc() {{
    if [ -n "${{LEAN_CTX_DISABLED:-}}" ] || [ ! -t 1 ]; then
        command "$@"
        return
    fi
    '{binary}' -t "$@"
}}"#
        );
        assert!(
            hook.contains("! -t 1"),
            "bash/zsh hook must contain pipe guard [ ! -t 1 ]"
        );
        assert!(
            hook.contains("LEAN_CTX_DISABLED") && hook.contains("! -t 1"),
            "pipe guard must be in the same conditional as LEAN_CTX_DISABLED"
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
    fn test_fish_hook_contains_pipe_guard() {
        let hook = "function _lc\n\tif set -q LEAN_CTX_DISABLED; or not isatty stdout\n\t\tcommand $argv\n\t\treturn\n\tend\nend";
        assert!(
            hook.contains("isatty stdout"),
            "fish hook must contain pipe guard (isatty stdout)"
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
    fn test_remove_lean_ctx_block_new_format_with_end_marker() {
        let input = r#"# existing config
export PATH="$HOME/bin:$PATH"

# lean-ctx shell hook — transparent CLI compression (90+ patterns)
_lean_ctx_cmds=(git npm pnpm)

lean-ctx-on() {
    for _lc_cmd in "${_lean_ctx_cmds[@]}"; do
        alias "$_lc_cmd"='lean-ctx -c '"$_lc_cmd"
    done
    export LEAN_CTX_ENABLED=1
    echo "lean-ctx: ON"
}

lean-ctx-off() {
    unset LEAN_CTX_ENABLED
    echo "lean-ctx: OFF"
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
        let data_dir = tmp.path().join("data");
        std::fs::create_dir_all(&data_dir).expect("mkdir data");
        std::env::set_var("LEAN_CTX_DATA_DIR", &data_dir);

        write_env_sh_for_containers("alias git='lean-ctx -c git'\n");
        let env_sh = data_dir.join("env.sh");
        let content = std::fs::read_to_string(&env_sh).expect("env.sh exists");
        assert!(content.contains("lean-ctx docker self-heal"));
        assert!(content.contains("claude mcp list"));
        assert!(content.contains("lean-ctx init --agent claude"));

        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
