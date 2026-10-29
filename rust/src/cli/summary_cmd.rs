//! `lean-ctx summary` — record + recall AI session summaries (#292).

use crate::core::session::SessionState;
use crate::tools::ctx_summary;

pub(crate) fn cmd_summary(args: &[String]) {
    let positionals: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let first = positionals.first().map_or("recall", |s| s.as_str());

    // `--help` never reaches `positionals`, so it is checked on its own —
    // before project-root detection and the default `recall` (#1906).
    if matches!(first, "help" | "-h") || args.iter().any(|a| a == "--help") {
        print_help();
        return;
    }

    let project_root = super::common::detect_project_root(args);

    // Known sub-actions; anything else is treated as a recall query so that
    // `lean-ctx summary what did I change?` just works.
    let known = matches!(first, "recall" | "record" | "list");
    let (action, query_parts): (&str, &[&String]) = if known {
        (first, &positionals[1..])
    } else {
        ("recall", &positionals[..])
    };

    let query = query_parts
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let query_opt = (!query.trim().is_empty()).then_some(query.as_str());

    let top_k = parse_top_k(args).unwrap_or(5);

    // `record` snapshots the persisted session for this project.
    let session = (action == "record")
        .then(|| SessionState::load_latest_for_project_root(&project_root))
        .flatten();

    let out =
        super::common::CliToolRun::new("ctx_summary", &project_root, None).deliver_text(|| {
            ctx_summary::handle(&project_root, session.as_ref(), action, query_opt, top_k)
        });
    super::common::print_released(&out);
}

fn parse_top_k(args: &[String]) -> Option<usize> {
    let pos = args.iter().position(|a| a == "--top-k" || a == "-k")?;
    args.get(pos + 1)?.parse().ok()
}

fn print_help() {
    println!(
        "lean-ctx summary — record + recall AI session summaries\n\
         \n\
         USAGE:\n    \
             lean-ctx summary [action] [query] [--top-k N]\n\
         \n\
         ACTIONS:\n    \
             recall <query>     Find past summaries (semantic when warm, else lexical)\n    \
             record             Snapshot the current session now\n    \
             list               Show recent summaries\n    \
             help               Show this help\n\
         \n\
         Bare text is treated as a recall query:\n    \
             lean-ctx summary what did I change in the graph index?"
    );
}
