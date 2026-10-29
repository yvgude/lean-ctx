//! `lean-ctx skillify` — distill recurring session patterns into .cursor/rules (#290).

use crate::tools::ctx_skillify;

pub(crate) fn cmd_skillify(args: &[String]) {
    let action = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map_or("mine", String::as_str);

    // `--help` is filtered out of `action` above, so it is checked on its own —
    // before the default `mine` action, which writes rules (#1906).
    if action == "help" || args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    let project_root = super::common::detect_project_root(args);

    // The promote action takes the next positional after the action as the slug.
    let slug = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .nth(1)
        .map(String::as_str);

    let out = super::common::CliToolRun::new("ctx_skillify", &project_root, None)
        .deliver_text(|| ctx_skillify::handle(&project_root, action, slug));
    super::common::print_released(&out);
}

fn print_help() {
    println!(
        "lean-ctx skillify — codify recurring session patterns into .cursor/rules\n\
         \n\
         USAGE:\n    \
             lean-ctx skillify <action> [args]\n\
         \n\
         ACTIONS:\n    \
             mine               Distill diary + knowledge into rules (default)\n    \
             list               Show generated skillify rules\n    \
             status             Show config + candidate/rule counts\n    \
             promote <slug>     Copy a project rule to ~/.cursor/rules\n    \
             help               Show this help"
    );
}
