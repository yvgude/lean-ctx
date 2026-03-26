use std::path::PathBuf;

fn resolve_binary_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "lean-ctx".to_string())
}

fn resolve_binary_path_for_bash() -> String {
    let path = resolve_binary_path();
    to_bash_compatible_path(&path)
}

pub fn to_bash_compatible_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        let drive = (path.as_bytes()[0] as char).to_ascii_lowercase();
        format!("/{drive}{}", &path[2..])
    } else {
        path
    }
}

pub fn install_agent_hook(agent: &str, global: bool) {
    match agent {
        "claude" | "claude-code" => install_claude_hook(global),
        "cursor" => install_cursor_hook(global),
        "gemini" => install_gemini_hook(),
        "codex" => install_codex_hook(),
        "windsurf" => install_windsurf_rules(global),
        "cline" | "roo" => install_cline_rules(global),
        "copilot" => install_claude_hook(global),
        "pi" => install_pi_hook(global),
        _ => {
            eprintln!("Unknown agent: {agent}");
            eprintln!("Supported: claude, cursor, gemini, codex, windsurf, cline, copilot, pi");
            std::process::exit(1);
        }
    }
}

fn install_claude_hook(global: bool) {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("Cannot resolve home directory");
            return;
        }
    };

    let hooks_dir = home.join(".claude").join("hooks");
    let _ = std::fs::create_dir_all(&hooks_dir);

    let script_path = hooks_dir.join("lean-ctx-rewrite.sh");
    let binary = resolve_binary_path_for_bash();
    let script = format!(
        r#"#!/usr/bin/env bash
# lean-ctx PreToolUse hook — rewrites bash commands to lean-ctx equivalents
set -euo pipefail

LEAN_CTX_BIN="{binary}"

INPUT=$(cat)
TOOL=$(echo "$INPUT" | grep -o '"tool_name":"[^"]*"' | head -1 | cut -d'"' -f4)

if [ "$TOOL" != "Bash" ] && [ "$TOOL" != "bash" ]; then
  exit 0
fi

CMD=$(echo "$INPUT" | grep -o '"command":"[^"]*"' | head -1 | cut -d'"' -f4)

if echo "$CMD" | grep -qE "^(lean-ctx |$LEAN_CTX_BIN )"; then
  exit 0
fi

REWRITE=""
case "$CMD" in
  git\ *)       REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  gh\ *)        REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  cargo\ *)     REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  npm\ *)       REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  pnpm\ *)      REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  yarn\ *)      REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  docker\ *)    REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  kubectl\ *)   REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  pip\ *|pip3\ *)  REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  ruff\ *)      REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  go\ *)        REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  curl\ *)      REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  grep\ *|rg\ *)  REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  find\ *)      REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  cat\ *|head\ *|tail\ *)  REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  ls\ *|ls)     REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  eslint*|prettier*|tsc*)  REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  pytest*|ruff\ *|mypy*)   REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  aws\ *)       REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  helm\ *)      REWRITE="$LEAN_CTX_BIN -c $CMD" ;;
  *)            exit 0 ;;
esac

if [ -n "$REWRITE" ]; then
  echo "{{\"command\":\"$REWRITE\"}}"
fi
"#
    );

    write_file(&script_path, &script);
    make_executable(&script_path);

    let settings_path = home.join(".claude").join("settings.json");
    let settings_content = if settings_path.exists() {
        std::fs::read_to_string(&settings_path).unwrap_or_default()
    } else {
        String::new()
    };

    if settings_content.contains("lean-ctx-rewrite") {
        println!("Claude Code hook already configured.");
    } else {
        let hook_entry = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash|bash",
                    "hooks": [{
                        "type": "command",
                        "command": script_path.to_string_lossy()
                    }]
                }]
            }
        });

        if settings_content.is_empty() {
            write_file(
                &settings_path,
                &serde_json::to_string_pretty(&hook_entry).unwrap(),
            );
        } else if let Ok(mut existing) =
            serde_json::from_str::<serde_json::Value>(&settings_content)
        {
            if let Some(obj) = existing.as_object_mut() {
                obj.insert("hooks".to_string(), hook_entry["hooks"].clone());
                write_file(
                    &settings_path,
                    &serde_json::to_string_pretty(&existing).unwrap(),
                );
            }
        }
        println!(
            "Installed Claude Code PreToolUse hook at {}",
            script_path.display()
        );
    }

    if !global {
        let claude_md = PathBuf::from("CLAUDE.md");
        if !claude_md.exists()
            || !std::fs::read_to_string(&claude_md)
                .unwrap_or_default()
                .contains("lean-ctx")
        {
            let content = include_str!("templates/CLAUDE.md");
            write_file(&claude_md, content);
            println!("Created CLAUDE.md in current project directory.");
        } else {
            println!("CLAUDE.md already configured.");
        }
    } else {
        println!(
            "Global mode: skipping project-local CLAUDE.md (use without --global in a project)."
        );
    }
}

fn install_cursor_hook(global: bool) {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("Cannot resolve home directory");
            return;
        }
    };

    let hooks_dir = home.join(".cursor").join("hooks");
    let _ = std::fs::create_dir_all(&hooks_dir);

    let script_path = hooks_dir.join("lean-ctx-rewrite.sh");
    let binary = resolve_binary_path_for_bash();
    let script = format!(
        r#"#!/usr/bin/env bash
# lean-ctx Cursor hook — rewrites shell commands
set -euo pipefail
LEAN_CTX_BIN="{binary}"
INPUT=$(cat)
CMD=$(echo "$INPUT" | grep -o '"command":"[^"]*"' | head -1 | cut -d'"' -f4 2>/dev/null || echo "")
if [ -z "$CMD" ] || echo "$CMD" | grep -qE "^(lean-ctx |$LEAN_CTX_BIN )"; then exit 0; fi
case "$CMD" in
  git\ *|gh\ *|cargo\ *|npm\ *|pnpm\ *|docker\ *|kubectl\ *|pip\ *|ruff\ *|go\ *|curl\ *|grep\ *|rg\ *|find\ *|ls\ *|ls|cat\ *|aws\ *|helm\ *)
    echo "{{\"command\":\"$LEAN_CTX_BIN -c $CMD\"}}" ;;
  *) exit 0 ;;
esac
"#
    );

    write_file(&script_path, &script);
    make_executable(&script_path);

    let hooks_json = home.join(".cursor").join("hooks.json");
    let hook_config = serde_json::json!({
        "hooks": [{
            "event": "preToolUse",
            "matcher": {
                "tool": "terminal_command"
            },
            "command": script_path.to_string_lossy()
        }]
    });

    let content = if hooks_json.exists() {
        std::fs::read_to_string(&hooks_json).unwrap_or_default()
    } else {
        String::new()
    };

    if content.contains("lean-ctx-rewrite") {
        println!("Cursor hook already configured.");
    } else {
        write_file(
            &hooks_json,
            &serde_json::to_string_pretty(&hook_config).unwrap(),
        );
        println!("Installed Cursor hook at {}", hooks_json.display());
    }

    if !global {
        let rules_dir = PathBuf::from(".cursor").join("rules");
        let _ = std::fs::create_dir_all(&rules_dir);
        let rule_path = rules_dir.join("lean-ctx.mdc");
        if !rule_path.exists() {
            let rule_content = include_str!("templates/lean-ctx.mdc");
            write_file(&rule_path, rule_content);
            println!("Created .cursor/rules/lean-ctx.mdc in current project.");
        } else {
            println!("Cursor rule already exists.");
        }
    } else {
        println!("Global mode: skipping project-local .cursor/rules/ (use without --global in a project).");
    }

    println!("Restart Cursor to activate.");
}

fn install_gemini_hook() {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("Cannot resolve home directory");
            return;
        }
    };

    let hooks_dir = home.join(".gemini").join("hooks");
    let _ = std::fs::create_dir_all(&hooks_dir);

    let script_path = hooks_dir.join("lean-ctx-hook-gemini.sh");
    let binary = resolve_binary_path_for_bash();
    let script = format!(
        r#"#!/usr/bin/env bash
# lean-ctx Gemini CLI BeforeTool hook
set -euo pipefail
LEAN_CTX_BIN="{binary}"
INPUT=$(cat)
CMD=$(echo "$INPUT" | grep -o '"command":"[^"]*"' | head -1 | cut -d'"' -f4 2>/dev/null || echo "")
if [ -z "$CMD" ] || echo "$CMD" | grep -qE "^(lean-ctx |$LEAN_CTX_BIN )"; then exit 0; fi
case "$CMD" in
  git\ *|gh\ *|cargo\ *|npm\ *|pnpm\ *|docker\ *|kubectl\ *|pip\ *|ruff\ *|go\ *|curl\ *|grep\ *|rg\ *|find\ *|ls\ *|ls|cat\ *|aws\ *|helm\ *)
    echo "{{\"command\":\"$LEAN_CTX_BIN -c $CMD\"}}" ;;
  *) exit 0 ;;
esac
"#
    );

    write_file(&script_path, &script);
    make_executable(&script_path);

    let settings_path = home.join(".gemini").join("settings.json");
    let settings_content = if settings_path.exists() {
        std::fs::read_to_string(&settings_path).unwrap_or_default()
    } else {
        String::new()
    };

    if settings_content.contains("lean-ctx") {
        println!("Gemini CLI hook already configured.");
    } else {
        let hook_config = serde_json::json!({
            "hooks": {
                "BeforeTool": [{
                    "command": script_path.to_string_lossy()
                }]
            }
        });

        if settings_content.is_empty() {
            write_file(
                &settings_path,
                &serde_json::to_string_pretty(&hook_config).unwrap(),
            );
        } else if let Ok(mut existing) =
            serde_json::from_str::<serde_json::Value>(&settings_content)
        {
            if let Some(obj) = existing.as_object_mut() {
                obj.insert("hooks".to_string(), hook_config["hooks"].clone());
                write_file(
                    &settings_path,
                    &serde_json::to_string_pretty(&existing).unwrap(),
                );
            }
        }
        println!("Installed Gemini CLI hook at {}", script_path.display());
    }
}

fn install_codex_hook() {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("Cannot resolve home directory");
            return;
        }
    };

    let codex_dir = home.join(".codex");
    let _ = std::fs::create_dir_all(&codex_dir);

    let agents_path = codex_dir.join("AGENTS.md");
    let agents_content = "# Global Agent Instructions\n\n@LEAN-CTX.md\n";

    let lean_ctx_md = codex_dir.join("LEAN-CTX.md");
    let binary = resolve_binary_path();
    let lean_ctx_content = format!(
        r#"# lean-ctx — Token Optimization

Prefix all shell commands with `{binary} -c` for compressed output:

```bash
{binary} -c git status    # instead of: git status
{binary} -c cargo test    # instead of: cargo test
{binary} -c ls src/       # instead of: ls src/
```

This saves 60-90% tokens per command. Works with: git, cargo, npm, pnpm, docker, kubectl, pip, ruff, go, curl, grep, find, ls, aws, helm, and 90+ more commands.
"#
    );

    if agents_path.exists() {
        let content = std::fs::read_to_string(&agents_path).unwrap_or_default();
        if content.contains("lean-ctx") || content.contains("LEAN-CTX") {
            println!("Codex AGENTS.md already configured.");
            return;
        }
    }

    write_file(&agents_path, agents_content);
    write_file(&lean_ctx_md, &lean_ctx_content);
    println!("Installed Codex instructions at {}", codex_dir.display());
}

fn install_windsurf_rules(global: bool) {
    if global {
        println!("Global mode: skipping project-local .windsurfrules (use without --global in a project).");
        return;
    }

    let rules_path = PathBuf::from(".windsurfrules");
    if rules_path.exists() {
        let content = std::fs::read_to_string(&rules_path).unwrap_or_default();
        if content.contains("lean-ctx") {
            println!(".windsurfrules already configured.");
            return;
        }
    }

    let rules = include_str!("templates/windsurfrules.txt");
    write_file(&rules_path, rules);
    println!("Installed .windsurfrules in current project.");
}

fn install_cline_rules(global: bool) {
    if global {
        println!(
            "Global mode: skipping project-local .clinerules (use without --global in a project)."
        );
        return;
    }

    let rules_path = PathBuf::from(".clinerules");
    if rules_path.exists() {
        let content = std::fs::read_to_string(&rules_path).unwrap_or_default();
        if content.contains("lean-ctx") {
            println!(".clinerules already configured.");
            return;
        }
    }

    let binary = resolve_binary_path();
    let rules = format!(
        r#"# lean-ctx Shell Optimization
# Prefix all shell commands with `{binary} -c` for 60-90% token savings.

When running shell commands, always prefix them with `{binary} -c` to compress output. For example:
- `{binary} -c git status` instead of `git status`
- `{binary} -c cargo test` instead of `cargo test`
- `{binary} -c ls src/` instead of `ls src/`

Supported commands: git, cargo, npm, pnpm, docker, kubectl, pip, ruff, go, curl, grep, find, ls, aws, helm, and 90+ more.
"#
    );

    write_file(&rules_path, &rules);
    println!("Installed .clinerules in current project.");
}

fn install_pi_hook(global: bool) {
    let has_pi = std::process::Command::new("pi")
        .arg("--version")
        .output()
        .is_ok();

    if !has_pi {
        println!("Pi Coding Agent not found in PATH.");
        println!("Install Pi first: npm install -g @mariozechner/pi-coding-agent");
        println!();
    }

    println!("Installing pi-lean-ctx Pi Package...");
    println!();

    let install_result = std::process::Command::new("pi")
        .args(["install", "pi-lean-ctx"])
        .status();

    match install_result {
        Ok(status) if status.success() => {
            println!("Installed pi-lean-ctx Pi Package.");
        }
        _ => {
            println!("Could not auto-install pi-lean-ctx. Install manually:");
            println!("  pi install pi-lean-ctx");
            println!();
        }
    }

    if !global {
        let agents_md = PathBuf::from("AGENTS.md");
        if !agents_md.exists()
            || !std::fs::read_to_string(&agents_md)
                .unwrap_or_default()
                .contains("lean-ctx")
        {
            let content = include_str!("templates/PI_AGENTS.md");
            write_file(&agents_md, content);
            println!("Created AGENTS.md in current project directory.");
        } else {
            println!("AGENTS.md already contains lean-ctx configuration.");
        }
    } else {
        println!(
            "Global mode: skipping project-local AGENTS.md (use without --global in a project)."
        );
    }

    println!();
    println!(
        "Setup complete. All Pi tools (bash, read, grep, find, ls) now route through lean-ctx."
    );
    println!("Use /lean-ctx in Pi to verify the binary path.");
}

fn write_file(path: &PathBuf, content: &str) {
    if let Err(e) = std::fs::write(path, content) {
        eprintln!("Error writing {}: {e}", path.display());
    }
}

#[cfg(unix)]
fn make_executable(path: &PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn make_executable(_path: &PathBuf) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_path_unix_unchanged() {
        assert_eq!(
            to_bash_compatible_path("/usr/local/bin/lean-ctx"),
            "/usr/local/bin/lean-ctx"
        );
    }

    #[test]
    fn bash_path_home_unchanged() {
        assert_eq!(
            to_bash_compatible_path("/home/user/.cargo/bin/lean-ctx"),
            "/home/user/.cargo/bin/lean-ctx"
        );
    }

    #[test]
    fn bash_path_windows_drive_converted() {
        assert_eq!(
            to_bash_compatible_path("C:\\Users\\Fraser\\bin\\lean-ctx.exe"),
            "/c/Users/Fraser/bin/lean-ctx.exe"
        );
    }

    #[test]
    fn bash_path_windows_lowercase_drive() {
        assert_eq!(
            to_bash_compatible_path("D:\\tools\\lean-ctx.exe"),
            "/d/tools/lean-ctx.exe"
        );
    }

    #[test]
    fn bash_path_windows_forward_slashes() {
        assert_eq!(
            to_bash_compatible_path("C:/Users/Fraser/bin/lean-ctx.exe"),
            "/c/Users/Fraser/bin/lean-ctx.exe"
        );
    }

    #[test]
    fn bash_path_bare_name_unchanged() {
        assert_eq!(to_bash_compatible_path("lean-ctx"), "lean-ctx");
    }
}
