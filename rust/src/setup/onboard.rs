use super::options::SetupOptions;
use super::with_options::run_setup_with_options;

/// Friendly, non-interactive "golden path" onboarding.
///
/// Unlike `run_setup` (the full 12-step interactive wizard), `onboard` makes
/// every decision for the user with sensible defaults — connect detected AI
/// tools, install the shell hook, set the `standard` tool profile — then prints
/// one clear "you're all set" message with a single obvious next step. This is
/// the recommended first-run path: time-to-value in seconds. Its one question,
/// on a terminal and only until a choice is recorded, is which of the detected
/// tools to connect.
pub fn run_onboard() {
    run_onboard_with(&OnboardAgents::Unspecified, false, false);
}

/// Which tools the caller asked `onboard` to connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnboardAgents {
    /// No `--agents`, no `--yes`, no `LEAN_CTX_AGENTS`.
    Unspecified,
    /// `--agents all` / `LEAN_CTX_AGENTS=all`: every detected tool.
    AllDetected,
    /// `--yes`: the recorded choice, else every detected tool (pre-3.12).
    Yes,
    /// `--agents a,b` / `LEAN_CTX_AGENTS=a,b` (validated, may be empty).
    Only(std::collections::BTreeSet<String>),
}

/// What one onboard run connects.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Decision {
    Explicit(std::collections::BTreeSet<String>),
    AllDetected,
    /// The recorded `setup.agents`.
    Saved,
    /// Ask on the terminal.
    Ask,
    /// No terminal, no choice: never connect a tool nobody picked (npm
    /// postinstall, CI). Tools that already carry a lean-ctx entry are
    /// refreshed; everything else waits for `lean-ctx setup`.
    ConnectedOnly,
}

fn decide(request: &OnboardAgents, has_saved: bool, interactive: bool) -> Decision {
    match request {
        OnboardAgents::Only(keys) => Decision::Explicit(keys.clone()),
        OnboardAgents::Yes | OnboardAgents::Unspecified if has_saved => Decision::Saved,
        OnboardAgents::AllDetected | OnboardAgents::Yes => Decision::AllDetected,
        OnboardAgents::Unspecified if interactive => Decision::Ask,
        OnboardAgents::Unspecified => Decision::ConnectedOnly,
    }
}

/// `dry_run` prints the plan and writes nothing. `no_prompt`
/// (`--non-interactive`) behaves like a run without a terminal.
pub fn run_onboard_with(request: &OnboardAgents, dry_run: bool, no_prompt: bool) {
    use crate::terminal_ui;

    let dim = "\x1b[2m";
    let bold = "\x1b[1m";
    let cyan = "\x1b[36m";
    let green = "\x1b[1;32m";
    let yellow = "\x1b[33m";
    let rst = "\x1b[0m";

    let has_saved = crate::core::config::Config::load_global()
        .setup
        .agents
        .is_some();
    let interactive = !no_prompt && !crate::shell::is_non_interactive();
    let decision = decide(request, has_saved, interactive);
    let (agents, refresh_only) = match decision {
        Decision::Explicit(keys) => (Some(keys), false),
        // Saved: the setup scope reads setup.agents itself.
        Decision::Saved => (None, false),
        // AllDetected must not fall back to a recorded choice.
        Decision::AllDetected => (Some(all_detected_keys()), false),
        // A plan does not ask: it shows what the offered default would touch.
        Decision::Ask if dry_run => (None, false),
        Decision::Ask => (ask_selection(), false),
        Decision::ConnectedOnly => (None, true),
    };

    if !dry_run {
        println!();
        println!("  {bold}Connecting lean-ctx to your AI tools…{rst}");
        println!("  {dim}Recommended defaults. Run `lean-ctx setup` for full control.{rst}");
        println!();
    }

    let opts = SetupOptions {
        non_interactive: true,
        yes: true,
        fix: true,
        agents,
        refresh_only,
        dry_run,
        ..Default::default()
    };

    let report = match run_setup_with_options(&opts) {
        Ok(r) if dry_run => {
            super::print_plan(&r);
            if refresh_only {
                print_choose_hint();
            }
            return;
        }
        Ok(r) => r,
        Err(e) => {
            eprintln!("  {yellow}Onboarding could not complete: {e}{rst}");
            eprintln!("  {dim}Try the guided setup instead: lean-ctx setup{rst}");
            std::process::exit(1);
        }
    };

    let connected: Vec<String> = report
        .steps
        .iter()
        .find(|s| s.name == "editors")
        .map(|s| {
            s.items
                .iter()
                .filter(|i| matches!(i.status.as_str(), "created" | "updated" | "already"))
                .map(|i| i.name.clone())
                .collect()
        })
        .unwrap_or_default();

    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map_or_else(|_| "~/.lean-ctx".to_string(), |p| p.display().to_string());

    println!();
    if refresh_only {
        print_choose_hint();
    }
    if connected.is_empty() && refresh_only {
        println!("  {yellow}No AI tool connected yet.{rst}");
    } else if connected.is_empty() {
        println!("  {yellow}No AI tools detected yet.{rst}");
        println!(
            "  {dim}Install Cursor, Claude Code, VS Code, etc., then re-run: lean-ctx onboard{rst}"
        );
    } else {
        println!("  {green}✓ lean-ctx is connected.{rst}");
        println!();
        println!("  {bold}Connected:{rst} {}", connected.join(", "));
    }
    println!("  {dim}Data dir:{rst}  {data_dir}");

    let source_cmd = crate::shell_hook::shell_source_command().unwrap_or("Restart your shell");
    println!();
    println!("  {bold}One last step:{rst}");
    println!("  {cyan}1.{rst} Reload your shell:  {bold}{source_cmd}{rst}");
    if !connected.is_empty() {
        println!(
            "  {cyan}2.{rst} {yellow}Fully restart your AI tool{rst} {dim}(so it reconnects to lean-ctx){rst}"
        );
        println!(
            "  {cyan}3.{rst} Ask your AI to read a file — lean-ctx optimizes it automatically."
        );
    }
    println!();
    println!(
        "  {dim}Check anytime:{rst}  {bold}lean-ctx doctor{rst}  {dim}·{rst}  {bold}lean-ctx cockpit{rst}"
    );
    println!();
    terminal_ui::print_command_box();

    crate::cli::show_first_run_wow();
}

/// Every installed tool, recorded as the choice so a later update refreshes
/// exactly these and never one installed afterwards.
fn all_detected_keys() -> std::collections::BTreeSet<String> {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        return std::collections::BTreeSet::new();
    };
    let targets = crate::core::editor_registry::build_targets(&home);
    super::agent_selection::detected_agents(&home, &targets)
        .into_iter()
        .map(|a| a.key)
        .collect()
}

/// Shown when onboard ran without a terminal and without a choice.
fn print_choose_hint() {
    let dim = "\x1b[2m";
    let bold = "\x1b[1m";
    let rst = "\x1b[0m";
    eprintln!("  No agent choice recorded and no terminal: only AI tools that already use");
    eprintln!("  lean-ctx were refreshed; no other tool was configured.");
    eprintln!("  Choose tools: {bold}lean-ctx setup{rst}");
    eprintln!(
        "  {dim}or: lean-ctx onboard --agents claude,cursor  ·  LEAN_CTX_AGENTS=all for every detected tool{rst}"
    );
}

/// First onboarding on a terminal: ask which detected tools to connect.
fn ask_selection() -> Option<std::collections::BTreeSet<String>> {
    use super::agent_selection as sel;

    let home = crate::core::home::resolve_home_dir()?;
    let targets = crate::core::editor_registry::build_targets(&home);
    let detected = sel::detected_agents(&home, &targets);
    if detected.is_empty() {
        return None;
    }
    println!();
    let default = sel::prompt_default(&detected, None);
    Some(sel::prompt_agent_selection(&detected, &default))
}

#[cfg(test)]
mod tests {
    use super::{Decision, OnboardAgents, decide};

    #[test]
    fn without_terminal_or_choice_onboard_connects_no_new_tool() {
        // npm postinstall / CI: nobody picked anything.
        assert_eq!(
            decide(&OnboardAgents::Unspecified, false, false),
            Decision::ConnectedOnly
        );
    }

    #[test]
    fn explicit_requests_and_records_win() {
        let keys: std::collections::BTreeSet<String> = ["cursor".to_string()].into();
        assert_eq!(
            decide(&OnboardAgents::Only(keys.clone()), true, false),
            Decision::Explicit(keys)
        );
        assert_eq!(
            decide(&OnboardAgents::AllDetected, false, false),
            Decision::AllDetected
        );
        assert_eq!(
            decide(&OnboardAgents::Unspecified, true, true),
            Decision::Saved
        );
        assert_eq!(decide(&OnboardAgents::Yes, true, false), Decision::Saved);
        assert_eq!(
            decide(&OnboardAgents::Yes, false, false),
            Decision::AllDetected
        );
        assert_eq!(
            decide(&OnboardAgents::AllDetected, true, false),
            Decision::AllDetected
        );
        assert_eq!(
            decide(&OnboardAgents::Unspecified, false, true),
            Decision::Ask
        );
    }
}
