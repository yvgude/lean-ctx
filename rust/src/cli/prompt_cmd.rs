//! `lean-ctx prompt-segment`: one short, dim piece of the shell prompt —
//! `◆ −1.2M tok ⛨ 3` for the project the shell is in.
//!
//! Runs on every prompt, so it only reads the project's small snapshot file
//! and prints nothing (no newline either) when there is nothing measured, the
//! numbers are stale, or the value display is off. `init --prompt` wires it
//! into zsh, bash and fish; Starship calls it with `--shell plain`.

use std::io::Write;
use std::time::Duration;

use crate::core::config::{Config, ValueDisplayMode};
use crate::core::value::{format, snapshot};

/// Older than this, the prompt shows nothing rather than an old number.
const MAX_AGE: Duration = Duration::from_hours(12);

/// How the dim colour must be escaped so the shell measures the prompt width
/// correctly (zero-width markers differ per shell).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptShell {
    Zsh,
    Bash,
    Fish,
    /// No colour at all — for Starship and other prompt engines that style
    /// the segment themselves.
    Plain,
}

impl PromptShell {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "zsh" => Some(Self::Zsh),
            "bash" => Some(Self::Bash),
            "fish" => Some(Self::Fish),
            "plain" | "starship" => Some(Self::Plain),
            _ => None,
        }
    }
}

pub(crate) fn cmd_prompt_segment(args: &[String]) {
    if args.iter().any(|a| matches!(a.as_str(), "-h" | "--help")) {
        usage();
        return;
    }
    let shell = match args {
        [] => PromptShell::Plain,
        [flag, value] if flag == "--shell" => parse_or_exit(value),
        [arg] if arg.starts_with("--shell=") => parse_or_exit(&arg["--shell=".len()..]),
        _ => {
            usage();
            std::process::exit(2);
        }
    };
    if Config::load_arc().value_display.effective_mode() == ValueDisplayMode::Off {
        return;
    }
    let snap = std::env::current_dir()
        .ok()
        .and_then(|cwd| snapshot::load_for_dir(&cwd))
        .filter(|s| s.is_fresh(MAX_AGE));
    let style = format::Style::from_env();
    if let Some(out) = snap.and_then(|s| render(&s, shell, style)) {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(out.as_bytes());
    }
}

fn parse_or_exit(value: &str) -> PromptShell {
    PromptShell::parse(value).unwrap_or_else(|| {
        eprintln!("Unknown shell '{value}'. Valid: zsh, bash, fish, plain");
        std::process::exit(2);
    })
}

/// The segment for `shell`, dimmed with that shell's zero-width markers
/// unless colour is off.
pub(crate) fn render(
    snap: &snapshot::ValueSnapshot,
    shell: PromptShell,
    style: format::Style,
) -> Option<String> {
    let text = format::compact(snap, style)?;
    if !style.color {
        return Some(text);
    }
    Some(match shell {
        PromptShell::Zsh => format!("%{{\x1b[2m%}}{text}%{{\x1b[0m%}}"),
        PromptShell::Bash => format!("\x01\x1b[2m\x02{text}\x01\x1b[0m\x02"),
        PromptShell::Fish => format!("\x1b[2m{text}\x1b[0m"),
        PromptShell::Plain => text,
    })
}

fn usage() {
    println!(
        "Shell prompt segment: what lean-ctx did in this project, e.g. `◆ −1.2M tok ⛨ 3`.\n\n\
         Prints nothing when nothing was measured yet or the numbers are stale.\n\
         `lean-ctx value` proves them. Set up with `lean-ctx init --prompt`.\n\n\
         Usage: lean-ctx prompt-segment [--shell zsh|bash|fish|plain]\n\n\
         Options:\n  \
           --shell <shell>  escape the dim colour for this shell's prompt;\n                   \
           plain (default) prints no colour, for Starship\n"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> snapshot::ValueSnapshot {
        let mut s = snapshot::ValueSnapshot {
            tokens_saved: 1_200_000,
            tokens_input: 2_000_000,
            ..snapshot::ValueSnapshot::default()
        };
        s.security.secrets_redacted = 3;
        s
    }

    const COLOR: format::Style = format::Style {
        color: true,
        unicode: true,
    };

    #[test]
    fn zsh_wraps_escapes_in_zero_width_markers() {
        assert_eq!(
            render(&snap(), PromptShell::Zsh, COLOR).unwrap(),
            "%{\x1b[2m%}◆ −1.2M tok ⛨ 3%{\x1b[0m%}"
        );
    }

    #[test]
    fn bash_uses_readline_ignore_markers() {
        let out = render(&snap(), PromptShell::Bash, COLOR).unwrap();
        assert_eq!(out, "\x01\x1b[2m\x02◆ −1.2M tok ⛨ 3\x01\x1b[0m\x02");
    }

    #[test]
    fn fish_gets_raw_ansi_and_plain_gets_none() {
        assert_eq!(
            render(&snap(), PromptShell::Fish, COLOR).unwrap(),
            "\x1b[2m◆ −1.2M tok ⛨ 3\x1b[0m"
        );
        assert_eq!(
            render(&snap(), PromptShell::Plain, COLOR).unwrap(),
            "◆ −1.2M tok ⛨ 3"
        );
    }

    #[test]
    fn no_color_means_no_escapes_in_any_shell() {
        for shell in [PromptShell::Zsh, PromptShell::Bash, PromptShell::Fish] {
            let out = render(&snap(), shell, format::Style::PLAIN).unwrap();
            assert_eq!(out, "◆ −1.2M tok ⛨ 3");
        }
    }

    #[test]
    fn prompt_text_is_inert_in_every_shell() {
        // The segment is substituted into PS1/PROMPT: nothing in it may be
        // re-interpreted as an expansion or a prompt escape.
        let out = render(&snap(), PromptShell::Plain, format::Style::PLAIN).unwrap();
        assert!(!out.contains(['$', '`', '%', '\\', '!']), "{out}");
    }

    #[test]
    fn nothing_measured_renders_nothing() {
        let empty = snapshot::ValueSnapshot::default();
        assert_eq!(render(&empty, PromptShell::Zsh, COLOR), None);
    }

    #[test]
    fn shell_names_parse() {
        assert_eq!(PromptShell::parse("zsh"), Some(PromptShell::Zsh));
        assert_eq!(PromptShell::parse("starship"), Some(PromptShell::Plain));
        assert_eq!(PromptShell::parse("tcsh"), None);
    }
}
