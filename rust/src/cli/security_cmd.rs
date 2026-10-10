//! `lean-ctx security` — one place to see and flip lean-ctx's security posture,
//! plus the `lean-ctx yolo` / `lean-ctx secure` master switches.
//!
//! Motivation (community feedback, #507): the individual knobs already exist
//! (`path_jail`, `shell_security`, `secret_detection`), but they are scattered
//! and hard to discover, so usability-first users "tried hard and failed" to
//! turn containment off. This command unifies them around the two independent
//! planes modelled in [`crate::core::security_posture`]:
//!
//! - **Containment** (path jail + shell gating) — protects the machine from the
//!   agent. `lean-ctx yolo` drops it; `lean-ctx secure` restores it.
//! - **Secret/`.env` redaction** — protects secrets from the LLM provider. A
//!   deliberately *separate* toggle (`lean-ctx security secrets on|off`) that
//!   `yolo` never touches, so "let the agent do anything" never implies "leak my
//!   API keys".
//!
//! Every change is written through the schema-validated config setter, takes
//! effect immediately (no daemon restart), and is fully reversible — granular
//! re-enabling stays available via plain `lean-ctx config set …` / `lean-ctx
//! allow …`.

use super::prompt::{confirm, wants_yes};
use crate::core::config::setter::set_by_key;
use crate::core::pathjail_scope::PathJailScope;
use crate::core::security_posture::{JailState, PostureLevel, SecurityPosture};
use crate::core::shell_allowlist::ShellSecurity;

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RST: &str = "\x1b[0m";

/// `lean-ctx security [status|open|strict|secrets …]`.
pub(crate) fn cmd_security(args: &[String]) {
    match args.first().map(String::as_str) {
        None | Some("status" | "show" | "--status") => print_status(),
        Some("open" | "yolo") => apply_open(&args[1..]),
        Some("strict" | "secure" | "lockdown") => apply_strict(),
        Some("secrets" | "secret") => set_secrets(&args[1..]),
        Some("-h" | "--help" | "help") => print_usage(),
        Some(other) => {
            eprintln!("Unknown subcommand: security {other}");
            print_usage();
            std::process::exit(2);
        }
    }
}

/// `lean-ctx yolo` — top-level alias for `security open`.
pub(crate) fn cmd_yolo(args: &[String]) {
    apply_open(args);
}

/// `lean-ctx secure` / `lean-ctx lockdown` — top-level alias for `security strict`.
pub(crate) fn cmd_secure(_args: &[String]) {
    apply_strict();
}

/// Drop containment (path jail + shell gating) in one step. Secret/`.env`
/// redaction is intentionally left untouched.
fn apply_open(args: &[String]) {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_usage();
        return;
    }

    println!(
        "{BOLD}lean-ctx yolo{RST} — disable containment so the agent can read/write {BOLD}any path{RST} and run {BOLD}any command{RST}."
    );
    println!(
        "  {DIM}This sets {RST}path_jail = false{DIM} and {RST}shell_security = off{DIM} in your global config.{RST}"
    );
    println!(
        "  {GREEN}Secret/.env redaction stays ON{RST} {DIM}(separate switch — secrets never leak to the provider).{RST}"
    );
    println!("  {DIM}Reverse any time with {RST}lean-ctx secure{DIM}.{RST}");

    if !confirm("\nDisable containment now?", wants_yes(args)) {
        println!("Aborted — nothing changed.");
        return;
    }

    apply_keys(&[("path_jail", "false"), ("shell_security", "off")], "yolo");
    println!("\n{YELLOW}⚠ Containment disabled.{RST} Intended for trusted local machines only.");
    println!();
    print_status();
}

/// Restore the secure defaults for both containment planes. Always safe, so no
/// confirmation is required. Leaves legitimate multi-root config
/// (`allow_paths`/`extra_roots`, `shell_allowlist_extra`) untouched.
fn apply_strict() {
    apply_keys(
        &[
            ("path_jail", "true"),
            ("shell_security", "enforce"),
            ("secret_detection.enabled", "true"),
        ],
        "secure",
    );
    println!(
        "{GREEN}✓ Secure defaults restored.{RST} Path jail enforced, shell gating on, secret redaction on."
    );
    println!(
        "  {DIM}Any extra allow_paths / allowed commands you added are kept — review them with {RST}lean-ctx security status{DIM}.{RST}"
    );
    println!();
    print_status();
}

/// `lean-ctx security secrets <on|off>` — the standalone secret/`.env` switch.
fn set_secrets(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("on" | "enable" | "true") => {
            apply_keys(&[("secret_detection.enabled", "true")], "secrets on");
            println!(
                "{GREEN}✓ Secret/.env redaction enabled.{RST} Detected credentials are masked before they reach the model."
            );
        }
        Some("off" | "disable" | "false") => {
            println!(
                "{RED}⚠ Disabling secret redaction lets API keys and .env values reach the LLM provider verbatim.{RST}"
            );
            if !confirm("Turn secret/.env redaction OFF?", wants_yes(args)) {
                println!("Aborted — redaction left on.");
                return;
            }
            apply_keys(&[("secret_detection.enabled", "false")], "secrets off");
            println!("{YELLOW}Secret/.env redaction disabled.{RST}");
        }
        _ => {
            let p = SecurityPosture::detect();
            println!(
                "Secret/.env redaction: {}",
                onoff(p.secrets_enabled && p.secrets_redact)
            );
            println!("Usage: lean-ctx security secrets <on|off>");
        }
    }
}

/// Writes each `(key, value)` through the schema-validated setter, exiting on the
/// first failure so we never half-apply a posture change.
fn apply_keys(pairs: &[(&str, &str)], label: &str) {
    for (key, value) in pairs {
        if let Err(e) = set_by_key(key, value) {
            eprintln!("{RED}Error applying `{label}` ({key} = {value}): {e}{RST}");
            std::process::exit(1);
        }
    }
}

fn print_status() {
    let p = SecurityPosture::detect();
    let (level_label, level_color) = match p.level() {
        PostureLevel::Strict => ("STRICT", GREEN),
        PostureLevel::Relaxed => ("RELAXED", YELLOW),
        PostureLevel::Open => ("OPEN", RED),
    };

    println!(
        "{BOLD}Security posture{RST}  {level_color}{level_label}{RST}  {DIM}(lean-ctx config: {}){RST}",
        config_path_display()
    );
    println!();
    println!("  {BOLD}Containment{RST} {DIM}— protects your machine from the agent{RST}");
    println!("    Path jail       {}", jail_line(&p.jail));
    if !p.jail.is_disabled() {
        println!(
            "    Jail scope      {}",
            scope_line(PathJailScope::resolve())
        );
    }
    println!("    Shell gating    {}", shell_line(p.shell));
    println!();
    println!("  {BOLD}Secret defense{RST} {DIM}— protects your secrets from the LLM provider{RST}");
    println!("    .env / secrets  {}", secrets_line(&p));
    println!();
    println!("  {BOLD}Switches{RST}");
    println!(
        "    {DIM}all off →{RST}  lean-ctx yolo                {DIM}any path + any command (keeps secret redaction){RST}"
    );
    println!(
        "    {DIM}all on  →{RST}  lean-ctx secure              {DIM}restore secure defaults{RST}"
    );
    println!("    {DIM}secrets →{RST}  lean-ctx security secrets <on|off>");
    println!(
        "    {DIM}granular →{RST} lean-ctx config set shell_security warn|off · path_jail false · path_jail_scope home|projects|project"
    );
    println!(
        "    {DIM}paths    →{RST}  lean-ctx allow-path <dir>     {DIM}open one extra/protected dir · lean-ctx allow <cmd> for commands{RST}"
    );
}

fn scope_line(scope: PathJailScope) -> String {
    match scope {
        PathJailScope::Home => format!(
            "{GREEN}home{RST}  {DIM}(read and write everything below ~; ~/.ssh, ~/.config & other dot dirs, ~/Library stay closed){RST}"
        ),
        PathJailScope::Projects => format!(
            "{YELLOW}projects{RST}  {DIM}(read every project below ~, write the active one — `path_jail_scope home` for all of ~){RST}"
        ),
        PathJailScope::Project => format!(
            "{YELLOW}project{RST}  {DIM}(only the active project + allow_paths — `path_jail_scope home` for all of ~){RST}"
        ),
    }
}

fn jail_line(jail: &JailState) -> String {
    match jail {
        JailState::Enforced => {
            format!("{GREEN}enforced{RST}  {DIM}(see scope below + configured allow_paths){RST}")
        }
        JailState::Relaxed(sources) => format!(
            "{GREEN}enforced{RST} {YELLOW}but widened via {}{RST}  {DIM}(reads beyond the project root){RST}",
            sources.join(", ")
        ),
        JailState::Disabled(source) => {
            format!("{RED}disabled{RST}  {DIM}({source} — every tool path allowed){RST}")
        }
    }
}

fn shell_line(shell: ShellSecurity) -> String {
    match shell {
        ShellSecurity::Enforce => {
            format!("{GREEN}enforce{RST}  {DIM}(allowlist + dangerous-pattern blocks active){RST}")
        }
        ShellSecurity::Warn => {
            format!("{YELLOW}warn{RST}  {DIM}(violations logged, never blocked){RST}")
        }
        ShellSecurity::Off => {
            format!("{RED}off{RST}  {DIM}(every command allowed; compression still active){RST}")
        }
    }
}

fn secrets_line(p: &SecurityPosture) -> String {
    if !p.secrets_enabled {
        return format!(
            "{RED}off{RST}  {DIM}(detection disabled — secrets can reach the provider){RST}"
        );
    }
    if p.secrets_redact {
        format!(
            "{GREEN}on{RST}  {DIM}(API keys / .env values masked before the model sees them){RST}"
        )
    } else {
        format!(
            "{YELLOW}detect-only{RST}  {DIM}(secrets flagged but NOT masked — set secret_detection.redact = true){RST}"
        )
    }
}

fn config_path_display() -> String {
    crate::core::config::Config::path().map_or_else(
        || "~/.lean-ctx/config.toml".to_string(),
        |p| p.display().to_string(),
    )
}

fn onoff(on: bool) -> String {
    if on {
        format!("{GREEN}on{RST}")
    } else {
        format!("{RED}off{RST}")
    }
}

fn print_usage() {
    println!(
        "Usage: lean-ctx security <status|open|strict|secrets>\n\
         \n\
         lean-ctx security status            Show the current security posture (default)\n\
         lean-ctx security open    | yolo    Disable containment: any path + any command\n\
         lean-ctx security strict  | secure  Restore secure defaults (jail + shell + secrets)\n\
         lean-ctx security secrets <on|off>  Toggle secret/.env redaction (separate concern)\n\
         \n\
         Top-level aliases: {BOLD}lean-ctx yolo{RST} = security open · {BOLD}lean-ctx secure{RST} = security strict\n\
         \n\
         Two independent planes:\n\
         \x20 Containment   = path jail + shell gating  (protects the machine from the agent)\n\
         \x20 Secret defense = .env/secret redaction     (protects secrets from the LLM provider)\n\
         `yolo` drops only containment and always keeps secret redaction on."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jail_line_reflects_state() {
        assert!(jail_line(&JailState::Enforced).contains("enforced"));
        assert!(jail_line(&JailState::Disabled("path_jail = false".into())).contains("disabled"));
        assert!(
            jail_line(&JailState::Relaxed(vec!["LEAN_CTX_ALLOW_PATH".into()]))
                .contains("LEAN_CTX_ALLOW_PATH")
        );
    }

    #[test]
    fn shell_line_covers_all_modes() {
        assert!(shell_line(ShellSecurity::Enforce).contains("enforce"));
        assert!(shell_line(ShellSecurity::Warn).contains("warn"));
        assert!(shell_line(ShellSecurity::Off).contains("off"));
    }
}
