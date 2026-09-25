//! `lean-ctx prompt-segment`: one short, dim piece of the shell prompt —
//! `◆ −1.2M tok ⛨ 3` for the project the shell is in.
//!
//! Runs on every prompt, so it only reads the project's small snapshot file
//! and prints nothing (no newline either) when there is nothing measured, the
//! numbers are stale, or the value display is off. `init --prompt` wires it
//! into zsh, bash and fish; Starship calls it with `--shell plain`.
//! `--json` serves editor status bars (the VS Code extension): the segment
//! plus its labelled breakdown, from the same snapshot.

use std::io::Write;
use std::path::{Path, PathBuf};
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
    let Some(opts) = Options::parse(args) else {
        usage();
        std::process::exit(2);
    };
    let mode = Config::load_arc().value_display.effective_mode();
    let dir = opts.dir.or_else(|| std::env::current_dir().ok());
    if opts.json {
        let snap = (mode != ValueDisplayMode::Off)
            .then(|| dir.as_deref().and_then(snapshot::load_for_dir))
            .flatten();
        let speed = (mode != ValueDisplayMode::Off)
            .then(crate::core::eval_ab::speed::SpeedHeadline::latest)
            .flatten();
        let watch = snapshot::value_dir().map(|d| d.join("projects"));
        let payload = json_payload(mode, snap.as_ref(), speed.as_ref(), watch.as_deref());
        println!("{payload}");
        return;
    }
    if mode == ValueDisplayMode::Off {
        return;
    }
    let snap = dir
        .as_deref()
        .and_then(snapshot::load_for_dir)
        .filter(|s| s.is_fresh(MAX_AGE));
    let style = format::Style::from_env();
    if let Some(out) = snap.and_then(|s| render(&s, opts.shell, style)) {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(out.as_bytes());
    }
}

struct Options {
    shell: PromptShell,
    json: bool,
    dir: Option<PathBuf>,
}

impl Options {
    fn parse(args: &[String]) -> Option<Self> {
        let mut opts = Self {
            shell: PromptShell::Plain,
            json: false,
            dir: None,
        };
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--json" => opts.json = true,
                "--shell" => opts.shell = parse_or_exit(it.next()?),
                "--dir" => opts.dir = Some(PathBuf::from(it.next()?)),
                a if a.starts_with("--shell=") => {
                    opts.shell = parse_or_exit(&a["--shell=".len()..]);
                }
                a if a.starts_with("--dir=") => {
                    opts.dir = Some(PathBuf::from(&a["--dir=".len()..]));
                }
                _ => return None,
            }
        }
        Some(opts)
    }
}

/// `--json`: everything an editor status bar needs in one call, so the
/// extension formats nothing itself. Schema 1:
/// `{schema, display, segment, tooltip[], speed?, watch, verify}`.
/// `segment` is null when there is nothing fresh to show — never a 0.
pub(crate) fn json_payload(
    mode: ValueDisplayMode,
    snap: Option<&snapshot::ValueSnapshot>,
    speed: Option<&crate::core::eval_ab::speed::SpeedHeadline>,
    watch: Option<&Path>,
) -> serde_json::Value {
    let style = format::Style {
        color: false,
        unicode: true,
    };
    let fresh = snap.filter(|s| mode != ValueDisplayMode::Off && s.is_fresh(MAX_AGE));
    let segment = fresh.and_then(|s| format::compact(s, style));
    let tooltip = fresh.map(tooltip_lines).unwrap_or_default();
    serde_json::json!({
        "schema": 1,
        "display": mode,
        "segment": segment,
        "tooltip": tooltip,
        "speed": speed.map(|h| serde_json::json!({ "phrase": h.phrase(), "detail": h.detail() })),
        "watch": watch.map(|p| p.to_string_lossy().into_owned()),
        "verify": "lean-ctx value",
    })
}

/// The breakdown behind the segment, each line labelled by where its number
/// comes from: `✓` counted, `≈` arithmetic on counted values.
fn tooltip_lines(snap: &snapshot::ValueSnapshot) -> Vec<String> {
    use crate::core::wrapped::format_tokens;
    let mut lines = Vec::new();
    if snap.tokens_saved > 0 {
        lines.push(format!(
            "✓ {} tokens kept out of context",
            format_tokens(snap.tokens_saved)
        ));
        if let Some(pct) = snap.saved_pct() {
            lines.push(format!(
                "≈ {pct:.0}% of {} tokens of tool output",
                format_tokens(snap.tokens_input)
            ));
        }
    }
    if snap.cache_hits > 0 {
        lines.push(format!("✓ {} re-reads served from cache", snap.cache_hits));
    }
    lines.extend(
        format::security_phrases(&snap.security)
            .into_iter()
            .map(|p| format!("✓ {p}")),
    );
    lines
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
         Usage: lean-ctx prompt-segment [--shell zsh|bash|fish|plain] [--json] [--dir PATH]\n\n\
         Options:\n  \
           --shell <shell>  escape the dim colour for this shell's prompt;\n                   \
           plain (default) prints no colour, for Starship\n  \
           --json           segment, labelled breakdown and verify hint as JSON\n                   \
           (for editor status bars)\n  \
           --dir <path>     the project directory (default: the current one)\n"
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

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn options_parse_in_any_order_and_reject_strays() {
        let o = Options::parse(&args(&["--dir", "/w", "--json", "--shell=zsh"])).unwrap();
        assert!(o.json);
        assert_eq!(o.shell, PromptShell::Zsh);
        assert_eq!(o.dir, Some(PathBuf::from("/w")));
        assert!(Options::parse(&args(&["--dir"])).is_none());
        assert!(Options::parse(&args(&["extra"])).is_none());
        assert!(!Options::parse(&[]).unwrap().json);
    }

    fn fresh() -> snapshot::ValueSnapshot {
        let mut s = snap();
        s.updated_at = Some(chrono::Utc::now());
        s.cache_hits = 41;
        s
    }

    #[test]
    fn json_labels_every_line_by_its_source() {
        let v = json_payload(
            ValueDisplayMode::Minimal,
            Some(&fresh()),
            None,
            Some(Path::new("/d/value/projects")),
        );
        assert_eq!(v["schema"], 1);
        assert_eq!(v["display"], "minimal");
        assert_eq!(v["segment"], "◆ −1.2M tok ⛨ 3");
        assert_eq!(v["watch"], "/d/value/projects");
        assert_eq!(v["verify"], "lean-ctx value");
        assert!(v["speed"].is_null());
        let lines: Vec<&str> = v["tooltip"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap())
            .collect();
        assert_eq!(
            lines,
            [
                "✓ 1.2M tokens kept out of context",
                "≈ 60% of 2.0M tokens of tool output",
                "✓ 41 re-reads served from cache",
                "✓ 3 secrets kept out of context",
            ]
        );
    }

    #[test]
    fn json_shows_nothing_for_stale_or_disabled_numbers() {
        for (mode, s) in [
            (ValueDisplayMode::Minimal, snap()),
            (ValueDisplayMode::Off, fresh()),
        ] {
            let v = json_payload(mode, Some(&s), None, None);
            assert!(v["segment"].is_null(), "{v}");
            assert_eq!(v["tooltip"], serde_json::json!([]));
        }
        assert!(json_payload(ValueDisplayMode::Minimal, None, None, None)["segment"].is_null());
    }

    #[test]
    fn json_quotes_speed_only_from_a_verified_headline() {
        let h = crate::core::eval_ab::speed::SpeedHeadline {
            faster_pct: 24.0,
            measured_on: "2026-09-25".into(),
            tasks: 12,
            runs: 3,
            model: "m".into(),
        };
        let v = json_payload(ValueDisplayMode::Minimal, None, Some(&h), None);
        assert_eq!(v["speed"]["phrase"], h.phrase());
        assert_eq!(v["speed"]["detail"], h.detail());
    }
}
