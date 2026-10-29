//! CLI tab-completions: `lean-ctx completions <shell>` generates a script,
//! `lean-ctx __complete <shell> -- <words…>` serves dynamic completions.

mod engine;
mod shells;
pub mod spec;
mod spec_account;
mod spec_setup;
mod spec_team;
mod spec_tools;
mod spec_value;
mod workspace;

/// `lean-ctx completions zsh|bash|fish` — print a static completion script.
pub fn run_completions(args: &[String]) {
    let shell = args.first().map_or("zsh", String::as_str);
    let script = match shell {
        "zsh" => shells::zsh_script(),
        "bash" => shells::bash_script(),
        "fish" => shells::fish_script(),
        other => {
            eprintln!("Unknown shell: {other}  (supported: zsh, bash, fish)");
            std::process::exit(1);
        }
    };
    print!("{script}");
}

/// `lean-ctx __complete zsh -- <words…>` — emit completions for the current input.
#[allow(non_snake_case)]
pub fn run___complete(args: &[String]) {
    let (shell, words) = match args.iter().position(|a| a == "--") {
        Some(pos) => {
            let shell = args.first().map_or("zsh", String::as_str);
            (shell, &args[pos + 1..])
        }
        None => ("zsh", args),
    };

    let completions = engine::complete(words);

    let output = match shell {
        "zsh" => shells::format_zsh(&completions),
        "fish" => shells::format_fish(&completions),
        _ => shells::format_bash(&completions),
    };
    print!("{output}");
}
