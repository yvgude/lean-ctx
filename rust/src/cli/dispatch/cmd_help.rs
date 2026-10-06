// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx <command> --help` must describe the command, never run it (#1906).
//!
//! Most handlers never looked at `--help`: `secure --help` rewrote the config,
//! `proof --help` wrote proof artifacts, `skillify --help` generated rules,
//! `upgrade --help` installed a release. Instead of teaching ~100 handlers the
//! flag one by one, the dispatcher answers it centrally from the `help all`
//! reference — the single place every command is documented — and only hands
//! `--help` to handlers whose own help is verified to be side-effect free.
//!
//! Every dispatcher arm is listed in exactly one of [`SELF_HELP`], [`EXEMPT`]
//! or [`GUARDED`]; a unit test parses `mod.rs` so a new command cannot slip
//! through unclassified.

use super::help::full_help_text;

/// Arms whose handler prints real usage for a leading `--help`/`-h` and exits
/// 0 without touching disk (covered by the `cli_help` integration test).
/// `--help` anywhere later (`index build --help`) still goes to the guard,
/// because sub-actions are not all help-aware.
const SELF_HELP: &[&[&str]] = &[
    &["autopilot"],
    &["spend"],
    &["savings"],
    &["output-savings", "output_savings"],
    &["value-report", "value_report"],
    &["evidence-export"],
    &["evidence"],
    &["shadow"],
    &["finops"],
    &["roi"],
    &["measure"],
    &["token-report", "report-tokens"],
    &["policy"],
    &["addon", "addons"],
    &["enable-gpu", "gpu"],
    &["rules"],
    &["prove"],
    &["value"],
    &["prompt-segment"],
    &["statusline"],
    &["inspect"],
    &["claude-mod", "claude_mod"],
    &["eval"],
    &["compliance"],
    &["agent"],
    &["instructions"],
    &["index"],
    &["semantic-search", "search-code"],
    &["explore"],
    &["dashboard"],
    &["provider"],
    &["serve"],
    &["proxy"],
    &["daemon"],
    &["init"],
    &["setup"],
    &["onboard"],
    &["wrap"],
    &["unwrap"],
    &["status"],
    &["ocla"],
    &["import"],
    &["checkpoints"],
    &["knowledge"],
    &["kit", "kits"],
    &["skillify"],
    &["summary"],
    &["benchmark"],
    &["benchmark-run", "bench-run"],
    &["calibrate"],
    &["pair"],
    &["security"],
    &["trust"],
    &["untrust"],
    &["update", "--self-update"],
    &["doctor"],
    &["harden"],
    &["gotchas", "bugs"],
    &["uninstall"],
    &["cheat", "cheatsheet", "cheat-sheet"],
    &["telemetry"],
    &["cloud"],
];

/// Arms the guard never intercepts: passthroughs whose arguments belong to the
/// wrapped command, the help/version entry points themselves, the completion
/// engine, and removed commands that only print a pointer to the replacement.
const EXEMPT: &[&[&str]] = &[
    &["-c", "exec"],
    &["-t", "--track"],
    &["raw", "bypass"],
    &["help"],
    &["--help", "-h"],
    &["--version", "-V"],
    &["__complete"],
    &["wrapped"],
    &["plugin", "plugins"],
    &["buddy", "pet"],
    &["watch"],
];

/// Arms answered from the `help all` reference. Each needs at least one
/// reference line whose leading word is one of its names.
const GUARDED: &[&[&str]] = &[
    // Their bare form reads live state, which takes a lock / touches the
    // project index, so `--help` must never reach the handler.
    &["cache"],
    &["terse", "compression"],
    &["migrate"],
    // Launches an interactive Codex session; `--help` must never start it.
    &["codex-protected"],
    &["badge"],
    &["shell", "--shell"],
    &["gain"],
    &["learning"],
    &["conformance", "selftest"],
    &["health"],
    &["quality-lab", "quality_lab"],
    &["billing"],
    &["triage"],
    &["scenario"],
    &["pack"],
    &["embeddings"],
    &["model"],
    &["proof"],
    &["git-trailer"],
    &["snapshot"],
    &["verify"],
    &["verify-cache", "cache-selftest"],
    &["visualize"],
    &["audit"],
    &["repomap", "repo-map"],
    &["cep"],
    &["demo"],
    &["team"],
    &["install"],
    &["bootstrap"],
    &["cognitive"],
    &["read"],
    &["call"],
    &["diff"],
    &["grep"],
    &["glob"],
    &["find"],
    &["ls"],
    &["deps"],
    &["discover"],
    &["engine"],
    &["ghost"],
    &["filter"],
    &["heatmap"],
    &["graph"],
    &["smells"],
    &["session"],
    &["ledger"],
    &["control", "context-control"],
    &["plan", "context-plan"],
    &["compile", "context-compile"],
    &["overview"],
    &["compress"],
    &["sessions", "session-store"],
    &["compact"],
    &["profile"],
    &["tools"],
    &["config"],
    &["allow"],
    &["yolo"],
    &["secure", "lockdown"],
    &["stats"],
    &["introspect"],
    &["theme"],
    &["enterprise"],
    &["tee"],
    &["slow-log"],
    &["debug-log"],
    &["editor-signal"],
    &["editor-session"],
    &["editor-bridge"],
    &["restart"],
    &["stop"],
    &["dev-install"],
    &["codesign-setup"],
    &["export-rules"],
    &["completions"],
    &["learn"],
    &["hook"],
    &["report-issue", "report"],
    &["safety-levels", "safety"],
    &["login"],
    &["register"],
    &["forgot-password"],
    &["sync"],
    &["contribute"],
    &["upgrade"],
    &["mcp"],
];

/// `help all` sections that describe something other than commands.
const NON_COMMAND_SECTIONS: &[&str] = &[
    "SHELL HOOK PATTERNS",
    "READ MODES:",
    "ENVIRONMENT:",
    "OPTIONS:",
    "TROUBLESHOOTING:",
];

/// [`SELF_HELP`] arms whose handler honours `--help` at *any* position with its
/// own, richer usage (`init --agent claude --help`, #1849 — covered by the
/// `init_help_safety_1849` integration test).
const SELF_HELP_ANYWHERE: &[&str] = &["init"];

fn group_of<'a>(table: &'a [&'a [&'a str]], cmd: &str) -> Option<&'a [&'a str]> {
    table.iter().copied().find(|group| group.contains(&cmd))
}

fn wants_help(rest: &[String]) -> bool {
    rest.iter()
        .take_while(|a| *a != "--")
        .any(|a| a == "--help")
        || matches!(rest, [only] if only == "-h")
}

/// The usage text to print instead of running `cmd`, or `None` when the arm
/// should run as usual (no help requested, exempt, or self-helping handler).
pub(super) fn guarded_help(cmd: &str, rest: &[String]) -> Option<String> {
    if !wants_help(rest) || group_of(EXEMPT, cmd).is_some() {
        return None;
    }
    let leading_help = rest.first().is_some_and(|a| a == "--help" || a == "-h");
    if (leading_help || SELF_HELP_ANYWHERE.contains(&cmd)) && group_of(SELF_HELP, cmd).is_some() {
        return None;
    }
    let group = group_of(GUARDED, cmd)
        .or_else(|| group_of(SELF_HELP, cmd))
        .unwrap_or(&[]);
    let lines = reference_lines(&full_help_text(), group);
    if lines.is_empty() {
        // Unknown command: let the dispatcher's own "unknown command" path answer.
        return None;
    }
    Some(format!(
        "Usage (lean-ctx {cmd}):\n{}\n\nFull command reference: lean-ctx help all\n",
        lines.join("\n")
    ))
}

/// Reference lines (plus their indented continuation lines) whose leading word
/// — after an optional `lean-ctx ` prefix — names one of `names`.
fn reference_lines(help: &str, names: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_commands = true;
    let mut matched = false;
    for line in help.lines() {
        if !line.starts_with(' ') && !line.is_empty() {
            in_commands = !NON_COMMAND_SECTIONS.iter().any(|s| line.starts_with(s));
            matched = false;
            continue;
        }
        if !in_commands {
            continue;
        }
        let trimmed = line.trim_start();
        if line.len() - trimmed.len() > 8 {
            if matched {
                out.push(line.to_string());
            }
            continue;
        }
        let word = trimmed
            .strip_prefix("lean-ctx ")
            .unwrap_or(trimmed)
            .split_whitespace()
            .next()
            .unwrap_or("");
        let word = word.split('|').next().unwrap_or(word);
        matched = names.contains(&word);
        if matched {
            out.push(line.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Every `"name" | "alias" =>` arm of the top-level dispatcher match.
    fn dispatcher_arms() -> Vec<Vec<String>> {
        let src = include_str!("mod.rs");
        let start = src
            .find("        match args[1].as_str() {")
            .expect("dispatcher match");
        let mut arms = Vec::new();
        for line in src[start..].lines().skip(1) {
            if line.starts_with("        }") {
                break;
            }
            let Some(head) = line.strip_prefix("            \"") else {
                continue;
            };
            let Some((names, _)) = head.split_once("=>") else {
                continue;
            };
            let names: Vec<String> = format!("\"{names}")
                .split('|')
                .map(|n| n.trim().trim_matches('"').to_string())
                .collect();
            arms.push(names);
        }
        arms
    }

    #[test]
    fn every_dispatcher_arm_is_classified_exactly_once() {
        let arms = dispatcher_arms();
        assert!(arms.len() > 100, "parser found only {} arms", arms.len());
        let mut seen: BTreeMap<Vec<String>, usize> = BTreeMap::new();
        for group in SELF_HELP.iter().chain(EXEMPT).chain(GUARDED) {
            let key: Vec<String> = group.iter().map(|s| (*s).to_string()).collect();
            *seen.entry(key).or_default() += 1;
        }
        for arm in &arms {
            assert_eq!(
                seen.get(arm),
                Some(&1),
                "dispatcher arm {arm:?} must appear in exactly one of SELF_HELP / EXEMPT / GUARDED"
            );
        }
        assert_eq!(
            seen.len(),
            arms.len(),
            "a SELF_HELP / EXEMPT / GUARDED entry names no dispatcher arm"
        );
    }

    #[test]
    fn every_non_exempt_arm_has_reference_lines() {
        let help = full_help_text();
        for group in SELF_HELP.iter().chain(GUARDED) {
            assert!(
                !reference_lines(&help, group).is_empty(),
                "`help all` has no line for {group:?}; `lean-ctx {} --help` would print nothing",
                group[0]
            );
        }
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn guarded_commands_answer_help_without_running() {
        let out = guarded_help("secure", &args(&["--help"])).expect("guarded");
        assert!(out.starts_with("Usage (lean-ctx secure):"));
        assert!(out.contains("Restore secure defaults"));
        assert!(guarded_help("lockdown", &args(&["-h"])).is_some());
        assert!(guarded_help("proof", &args(&["--format", "json", "--help"])).is_some());
    }

    #[test]
    fn continuation_lines_follow_their_command() {
        let out = guarded_help("uninstall", &args(&["--keep-config", "--help"])).unwrap();
        assert!(out.contains("--dry-run previews without changing anything"));
        assert!(!out.contains("SHELL HOOK"));
    }

    #[test]
    fn self_help_handlers_keep_a_leading_help_flag() {
        assert_eq!(guarded_help("doctor", &args(&["--help"])), None);
        assert_eq!(guarded_help("doctor", &args(&["-h"])), None);
        // A later `--help` is not guaranteed to reach a help-aware code path.
        assert!(guarded_help("index", &args(&["build", "--help"])).is_some());
        // ...except for handlers verified to honour it anywhere (#1849).
        assert_eq!(
            guarded_help("init", &args(&["--agent", "claude", "--help"])),
            None
        );
    }

    #[test]
    // Named so it avoids the `gh[ops]_` token prefixes the history secret
    // rule (SEC001) matches on.
    fn passthrough_and_non_help_invocations_are_untouched() {
        assert_eq!(guarded_help("-c", &args(&["ls", "--help"])), None);
        assert_eq!(guarded_help("raw", &args(&["cargo", "--help"])), None);
        assert_eq!(guarded_help("grep", &args(&["fn", "src"])), None);
        assert_eq!(guarded_help("grep", &args(&["--", "--help"])), None);
        // `-h` is only help on its own; elsewhere it may be a real flag.
        assert_eq!(guarded_help("ls", &args(&["-h", "src"])), None);
    }

    #[test]
    fn reference_lines_skip_non_command_sections() {
        // `grep` appears under SHELL HOOK PATTERNS ("utils curl, grep/rg");
        // only the command reference and examples may be returned.
        let lines = reference_lines(&full_help_text(), &["grep"]);
        assert!(
            lines.iter().any(|l| l.contains("grep <pattern>")),
            "{lines:?}"
        );
        assert!(lines.iter().all(|l| !l.contains("grep/rg")), "{lines:?}");
    }
}
