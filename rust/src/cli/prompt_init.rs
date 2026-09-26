//! `lean-ctx init --prompt [off]`: puts the value segment into the shell
//! prompt — zsh right prompt, bash `PS1` prefix, fish right prompt.
//!
//! The prompt script lives next to the shell hook (`prompt.<shell>` in the
//! config dir) and is sourced from its own marker block, independent of the
//! alias hook: `init --global` never touches it, `init --prompt off` and
//! `lean-ctx uninstall` remove it. Starship users get a `custom` module
//! instead, since Starship owns the whole prompt.

use std::path::{Path, PathBuf};

use super::shell_init::{
    backup_shell_config, config_artifact_dir, resolved_hook_dir_display, write_hook_file,
};

const BEGIN: &str = "# lean-ctx prompt — begin";
const END: &str = "# lean-ctx prompt — end";
const SCRIPTS: [&str; 3] = ["prompt.zsh", "prompt.bash", "prompt.fish"];

pub(crate) const STARSHIP_MODULE: &str = "[custom.lean_ctx]\n\
     command = \"lean-ctx prompt-segment --shell plain\"\n\
     when = true\n\
     style = \"dimmed\"\n\
     format = \"[$output]($style) \"";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shell {
    Zsh,
    Bash,
    Fish,
}

impl Shell {
    fn ext(self) -> &'static str {
        match self {
            Self::Zsh => "zsh",
            Self::Bash => "bash",
            Self::Fish => "fish",
        }
    }

    fn rc(self, home: &Path) -> PathBuf {
        match self {
            Self::Zsh => home.join(".zshrc"),
            Self::Bash => home.join(".bashrc"),
            Self::Fish => home.join(".config/fish/config.fish"),
        }
    }
}

/// `init --prompt` (`enable`) or `init --prompt off`.
pub(crate) fn cmd_init_prompt(enable: bool, binary: &str) {
    let Some(home) = dirs::home_dir() else {
        eprintln!("Cannot determine the home directory.");
        std::process::exit(1);
    };
    if !enable {
        if !uninstall(&home, false) {
            println!("No lean-ctx prompt segment installed.");
        }
        return;
    }
    if std::env::var_os("STARSHIP_SHELL").is_some() {
        println!("Starship draws your prompt — add this module to ~/.config/starship.toml:\n");
        println!("{STARSHIP_MODULE}\n");
        println!("Details: docs/guides/value-display.md");
        return;
    }
    let shell_name = std::env::var("SHELL").unwrap_or_default();
    let shell = if shell_name.contains("zsh") {
        Shell::Zsh
    } else if shell_name.contains("fish") {
        Shell::Fish
    } else if shell_name.contains("bash") {
        Shell::Bash
    } else {
        println!("No zsh, bash or fish detected (SHELL={shell_name:?}).");
        println!("PowerShell and other shells: use Starship with this module:\n");
        println!("{STARSHIP_MODULE}");
        return;
    };
    let binary = crate::hooks::to_bash_compatible_path(binary);
    if write_hook_file(&format!("prompt.{}", shell.ext()), &script(shell, &binary)).is_none() {
        std::process::exit(1);
    }
    let rc = shell.rc(&home);
    let existing = std::fs::read_to_string(&rc).unwrap_or_default();
    let updated = upsert_block(&existing, &rc_block(shell, &resolved_dir()));
    if updated != existing {
        backup_shell_config(&rc);
        if let Some(parent) = rc.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&rc, &updated) {
            eprintln!("Cannot write {}: {e}", rc.display());
            eprintln!(
                "Add this block yourself:\n\n{}",
                rc_block(shell, &resolved_dir())
            );
            std::process::exit(1);
        }
    }
    println!("◆ lean-ctx prompt segment added to {}", rc.display());
    println!("  Shows what lean-ctx did in the current project, e.g. `◆ −1.2M tok ⛨ 3`,");
    println!("  and nothing when there is nothing to show. Proof: lean-ctx value");
    println!("  Open a new shell to see it. Remove: lean-ctx init --prompt off");
}

fn resolved_dir() -> String {
    let dir = resolved_hook_dir_display();
    if cfg!(windows) {
        crate::hooks::to_bash_compatible_path(&dir)
    } else {
        dir
    }
}

/// POSIX single-quoting: safe for any binary path in zsh and bash.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// fish single-quoting: only `\` and `'` are special inside.
fn fish_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', r"\\").replace('\'', r"\'"))
}

fn script(shell: Shell, binary: &str) -> String {
    let header = "# lean-ctx prompt segment — written by `lean-ctx init --prompt`.\n\
                  # Remove with `lean-ctx init --prompt off`.\n";
    match shell {
        Shell::Zsh => format!(
            "{header}[[ -o interactive ]] || return 0\n\
             __lean_ctx_prompt_precmd() {{\n\
             \x20 LEAN_CTX_PROMPT=\"$({bin} prompt-segment --shell zsh 2>/dev/null)\"\n\
             }}\n\
             autoload -Uz add-zsh-hook\n\
             add-zsh-hook precmd __lean_ctx_prompt_precmd\n\
             setopt prompt_subst\n\
             [[ \"$RPROMPT\" == *'${{LEAN_CTX_PROMPT}}'* ]] || RPROMPT='${{LEAN_CTX_PROMPT}}'\"${{RPROMPT:+ $RPROMPT}}\"\n",
            bin = sh_quote(binary)
        ),
        Shell::Bash => format!(
            "{header}[[ $- == *i* ]] || return 0\n\
             __lean_ctx_prompt() {{\n\
             \x20 LEAN_CTX_PROMPT=\"$({bin} prompt-segment --shell bash 2>/dev/null)\"\n\
             \x20 [ -n \"$LEAN_CTX_PROMPT\" ] && LEAN_CTX_PROMPT=\"$LEAN_CTX_PROMPT \"\n\
             }}\n\
             case \";${{PROMPT_COMMAND[*]:-}};\" in\n\
             \x20 *__lean_ctx_prompt*) ;;\n\
             \x20 *) PROMPT_COMMAND=\"__lean_ctx_prompt${{PROMPT_COMMAND:+;$PROMPT_COMMAND}}\" ;;\n\
             esac\n\
             case \"$PS1\" in\n\
             \x20 *'${{LEAN_CTX_PROMPT}}'*) ;;\n\
             \x20 *) PS1='${{LEAN_CTX_PROMPT}}'\"$PS1\" ;;\n\
             esac\n",
            bin = sh_quote(binary)
        ),
        Shell::Fish => format!(
            "{header}status is-interactive; or exit\n\
             if functions -q fish_right_prompt; and not functions -q __lean_ctx_orig_right_prompt\n\
             \x20   functions -c fish_right_prompt __lean_ctx_orig_right_prompt\n\
             end\n\
             function fish_right_prompt\n\
             \x20   set -l segment ({bin} prompt-segment --shell fish 2>/dev/null)\n\
             \x20   test -n \"$segment\"; and echo -n $segment\n\
             \x20   if functions -q __lean_ctx_orig_right_prompt\n\
             \x20       test -n \"$segment\"; and echo -n ' '\n\
             \x20       __lean_ctx_orig_right_prompt\n\
             \x20   end\n\
             end\n",
            bin = fish_quote(binary)
        ),
    }
}

fn rc_block(shell: Shell, dir: &str) -> String {
    let ext = shell.ext();
    match shell {
        Shell::Fish => format!(
            "{BEGIN}\nif test -f \"{dir}/prompt.{ext}\"\n  source \"{dir}/prompt.{ext}\"\nend\n{END}\n"
        ),
        _ => format!(
            "{BEGIN}\nif [ -f \"{dir}/prompt.{ext}\" ]; then\n  . \"{dir}/prompt.{ext}\"\nfi\n{END}\n"
        ),
    }
}

/// `content` with our block replaced in place, or appended when absent.
fn upsert_block(content: &str, block: &str) -> String {
    if content.contains(block) {
        return content.to_string();
    }
    if content.contains(BEGIN) && content.contains(END) {
        let mut out = String::new();
        let mut in_block = false;
        for line in content.lines() {
            if !in_block && line.trim() == BEGIN {
                in_block = true;
                out.push_str(block);
                continue;
            }
            if in_block {
                in_block = line.trim() != END;
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        return out;
    }
    let sep = if content.is_empty() || content.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    format!("{content}{sep}{block}")
}

/// `content` without our block; everything else untouched.
pub(crate) fn remove_block(content: &str) -> String {
    if !content.contains(BEGIN) {
        return content.to_string();
    }
    let mut out = String::new();
    let mut in_block = false;
    for line in content.lines() {
        if !in_block && line.trim() == BEGIN {
            in_block = true;
            continue;
        }
        if in_block {
            in_block = line.trim() != END;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Removes the prompt block from every rc file and the prompt scripts.
/// Returns whether anything was (or, with `dry_run`, would be) removed.
pub(crate) fn uninstall(home: &Path, dry_run: bool) -> bool {
    let verb = if dry_run { "Would remove" } else { "✓" };
    let mut removed = false;
    for shell in [Shell::Zsh, Shell::Bash, Shell::Fish] {
        let rc = shell.rc(home);
        let Ok(content) = std::fs::read_to_string(&rc) else {
            continue;
        };
        let cleaned = remove_block(&content);
        if cleaned != content {
            if !dry_run && std::fs::write(&rc, &cleaned).is_err() {
                eprintln!("  Cannot update {}", rc.display());
                continue;
            }
            println!("  {verb} Prompt segment removed from {}", rc.display());
            removed = true;
        }
    }
    if let Some(dir) = config_artifact_dir() {
        for name in SCRIPTS {
            let path = dir.join(name);
            if path.exists() {
                if !dry_run {
                    let _ = std::fs::remove_file(&path);
                }
                println!("  {verb} Removed {}", path.display());
                removed = true;
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_is_appended_once_and_updated_in_place() {
        let block = rc_block(Shell::Zsh, "/cfg");
        let rc = "export A=1\n";
        let once = upsert_block(rc, &block);
        assert_eq!(once, format!("export A=1\n{block}"));
        assert_eq!(upsert_block(&once, &block), once, "idempotent");

        let moved = rc_block(Shell::Zsh, "/elsewhere");
        let updated = upsert_block(&format!("{once}alias x=y\n"), &moved);
        assert_eq!(updated, format!("export A=1\n{moved}alias x=y\n"));
        assert_eq!(updated.matches(BEGIN).count(), 1);
    }

    #[test]
    fn remove_leaves_the_shell_hook_and_user_lines_alone() {
        let hook = "# lean-ctx shell hook — begin\n. hook\n# lean-ctx shell hook — end\n";
        let rc = format!("a\n{hook}{}b\n", rc_block(Shell::Bash, "/cfg"));
        assert_eq!(remove_block(&rc), format!("a\n{hook}b\n"));
        assert_eq!(remove_block("plain\n"), "plain\n");
    }

    #[test]
    fn the_shell_hook_remover_leaves_the_prompt_block_alone() {
        let block = rc_block(Shell::Zsh, "/cfg");
        let rc = format!("# lean-ctx shell hook — begin\nx\n# lean-ctx shell hook — end\n{block}");
        assert_eq!(super::super::shell_init::remove_lean_ctx_block(&rc), block);
    }

    #[test]
    fn scripts_quote_the_binary_and_ask_for_their_shell() {
        let zsh = script(Shell::Zsh, "/opt/it's/lean-ctx");
        assert!(zsh.contains(r"'/opt/it'\''s/lean-ctx' prompt-segment --shell zsh"));
        assert!(zsh.contains("add-zsh-hook precmd"));
        let bash = script(Shell::Bash, "/usr/bin/lean-ctx");
        assert!(bash.contains("--shell bash"));
        assert!(bash.contains("PROMPT_COMMAND"));
        let fish = script(Shell::Fish, r"C:\it's\lean-ctx");
        assert!(fish.contains(r"'C:\\it\'s\\lean-ctx' prompt-segment --shell fish"));
        assert!(fish.contains("functions -c fish_right_prompt __lean_ctx_orig_right_prompt"));
    }

    #[cfg(unix)]
    fn shell_available(name: &str) -> bool {
        std::process::Command::new(name)
            .arg("-c")
            .arg("exit 0")
            .status()
            .is_ok_and(|s| s.success())
    }

    /// An interactive shell (`-i`) detached from the controlling terminal.
    /// With the terminal it grabs the foreground (tcsetpgrp) and stops
    /// whatever runs the tests — `cargo test` in a terminal, or an agent.
    #[cfg(unix)]
    fn interactive_shell(name: &str) -> std::process::Command {
        use std::os::unix::process::CommandExt;
        let mut cmd = std::process::Command::new(name);
        cmd.stdin(std::process::Stdio::null());
        // SAFETY: setsid is async-signal-safe and touches no parent state.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd
    }

    /// Sourcing the script twice must not stack the segment or the hook.
    #[cfg(unix)]
    #[test]
    fn bash_script_is_idempotent_when_sourced_twice() {
        if !shell_available("bash") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prompt.bash");
        std::fs::write(&path, script(Shell::Bash, "/bin/echo")).unwrap();
        let p = path.display();
        let out = interactive_shell("bash")
            .args(["--norc", "-i", "-c"])
            .arg(format!(
                "PS1='$ '; PROMPT_COMMAND='other'; . '{p}'; . '{p}'; \
                 printf '%s|%s' \"$PS1\" \"$PROMPT_COMMAND\""
            ))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(stdout, "${LEAN_CTX_PROMPT}$ |__lean_ctx_prompt;other");
    }

    #[cfg(unix)]
    #[test]
    fn zsh_script_is_idempotent_when_sourced_twice() {
        if !shell_available("zsh") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prompt.zsh");
        std::fs::write(&path, script(Shell::Zsh, "/bin/echo")).unwrap();
        let p = path.display();
        let out = interactive_shell("zsh")
            .args(["-f", "-i", "-c"])
            .arg(format!(
                "RPROMPT='%T'; . '{p}'; . '{p}'; print -rn -- \"$RPROMPT|${{#precmd_functions}}\""
            ))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(stdout, "${LEAN_CTX_PROMPT} %T|1");
    }
}
