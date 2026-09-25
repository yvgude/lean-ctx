//! `lean-ctx value`: every number the value surface shows (status line,
//! recaps, prompt segment), recomputed from the hash-chained savings ledger
//! and audit trail, with both chains verified. Exit 1 when a chain is broken.

use crate::core::value::{format::Style, proof};

pub(crate) fn cmd_value(args: &[String]) {
    if args.iter().any(|a| matches!(a.as_str(), "-h" | "--help")) {
        usage();
        return;
    }
    let Some(opts) = parse(args) else {
        usage();
        std::process::exit(2);
    };
    let proof = proof::build(opts.session.as_deref(), opts.all);
    if opts.json {
        match serde_json::to_string_pretty(&proof) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("value: cannot serialize proof: {e}");
                std::process::exit(2);
            }
        }
    } else {
        let mut style = Style::from_env();
        style.color &= std::io::IsTerminal::is_terminal(&std::io::stdout());
        print!("{}", proof::render(&proof, style));
    }
    if proof.tampered() {
        std::process::exit(1);
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Opts {
    session: Option<String>,
    all: bool,
    json: bool,
}

fn parse(args: &[String]) -> Option<Opts> {
    let mut opts = Opts::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--session" => opts.session = Some(it.next()?.clone()),
            "--all" => opts.all = true,
            "--json" => opts.json = true,
            other => opts.session = Some(other.strip_prefix("--session=")?.to_string()),
        }
    }
    (!(opts.all && opts.session.is_some())).then_some(opts)
}

fn usage() {
    println!(
        "Show what lean-ctx did — and prove it.\n\n\
         Every number is recomputed from the hash-chained savings ledger and the\n\
         signed audit trail; both chains are verified (exit 1 if either is broken).\n\n\
         Usage: lean-ctx value [--session <id> | --all] [--json]\n\n\
         Options:\n  \
           --session <id>  a specific session (default: the current/last one)\n  \
           --all           lifetime, across all sessions\n  \
           --json          machine-readable output\n"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn parses_flags() {
        assert_eq!(parse(&args(&[])), Some(Opts::default()));
        assert_eq!(
            parse(&args(&["--session", "abc", "--json"])),
            Some(Opts {
                session: Some("abc".into()),
                all: false,
                json: true
            })
        );
        assert_eq!(
            parse(&args(&["--session=xyz"])).and_then(|o| o.session),
            Some("xyz".into())
        );
        assert!(parse(&args(&["--all"])).is_some_and(|o| o.all));
    }

    #[test]
    fn rejects_unknown_and_conflicting_flags() {
        assert_eq!(parse(&args(&["--bogus"])), None);
        assert_eq!(parse(&args(&["--session"])), None);
        assert_eq!(parse(&args(&["--all", "--session", "a"])), None);
    }
}
