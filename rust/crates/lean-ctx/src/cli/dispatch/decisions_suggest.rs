// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx decisions suggest [--since <ref>] [--json] [--max-commits N]`
//! and `lean-ctx decisions confirm <draft-id> [--id <slug>] [--decided-by W]`
//! (contract §38).
//!
//! `suggest` derives draft decisions from the local git history and writes
//! them to the review file `.leanctx/decisions-review/suggested.json`; it also
//! reports the trust of the records confirmed earlier. Nothing is confirmed
//! automatically: a human runs `confirm` for one draft at a time.
//!
//! Kept separate from the D1 `decisions` command; dispatch routes only the
//! `suggest` and `confirm` subcommands here. Text from the history is printed
//! through [`tty`], never raw.

use lean_ctx::core::git::decision_history::{self as dh, Options, Trust};

const USAGE: &str = "Usage: lean-ctx decisions suggest [--since <ref>] [--json] [--max-commits N]
       lean-ctx decisions confirm <draft-id> [--id <slug>] [--decided-by owner|team|review]

Derives draft decisions from commit and merge-request messages and writes
them to .leanctx/decisions-review/suggested.json for review. Nothing is
confirmed automatically: `confirm` records one draft in .leanctx/decisions
with its verbatim evidence. Each run re-reads the evidence of the records
confirmed earlier and reports their trust.

Trust levels:
  confirmed      explicit statement (Decision: trailer, owner quote); quote kept verbatim
  reconstructed  derived from policy wording (never, no longer, from now on, ...)
  re-check       evidence changed since the last suggestion, or its commit left the history
                 (stays flagged until the review file is removed)

Superseded drafts and records are detected from reverts, Supersedes: trailers
and a later draft on the same topic with the opposite polarity.";

/// Entry point; `args` are the words after `suggest`. Returns the exit code.
pub(crate) fn run(args: &[String]) -> i32 {
    let mut opts = Options::default();
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--since" => match it.next() {
                Some(v) => opts.since = Some(v.clone()),
                None => return usage_error("--since needs a ref"),
            },
            "--max-commits" => match it.next().and_then(|v| v.parse::<usize>().ok()) {
                Some(n) if n > 0 => opts.max_commits = n,
                _ => return usage_error("--max-commits needs a positive number"),
            },
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            other => {
                if let Some(v) = other.strip_prefix("--since=") {
                    opts.since = Some(v.to_string());
                } else {
                    return usage_error(&format!("unknown argument: {other}"));
                }
            }
        }
    }

    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => return fail(&format!("current directory: {e}")),
    };
    let root = match dh::repo_root(&cwd) {
        Ok(r) => r,
        Err(e) => return fail(&format!("not a git repository: {e}")),
    };
    let report = match dh::suggest(&root, &opts) {
        Ok(r) => r,
        Err(e) => return fail(&e),
    };
    if let Err(e) = dh::write_review(&root, &report) {
        return fail(&e);
    }
    if json {
        match dh::to_json(&report) {
            Ok(s) => print!("{s}"),
            Err(e) => return fail(&e),
        }
        return 0;
    }
    print_text(&report);
    0
}

fn print_text(report: &dh::Report) {
    let open = report
        .drafts
        .iter()
        .filter(|d| d.superseded_by.is_none())
        .count();
    println!(
        "{} draft decision(s) from {} ({} open, {} superseded) -> {}/{}",
        report.drafts.len(),
        tty(&report.range),
        open,
        report.drafts.len() - open,
        dh::REVIEW_DIR,
        dh::REVIEW_FILE
    );
    println!("Drafts only: record one with `lean-ctx decisions confirm <draft-id>`.");
    for d in &report.drafts {
        println!();
        println!("[{}] {}  {}", d.trust.label(), tty(&d.id), tty(&d.title));
        let where_ = if d.evidence.kind == "merge-request" {
            format!(
                "{} {}",
                tty(&d.evidence.reference),
                tty(dh::short(&d.evidence.commit))
            )
        } else {
            tty(&d.evidence.reference)
        };
        let (date, quote) = (tty(&d.evidence.date), tty(&d.evidence.quote));
        if d.trust == Trust::Confirmed {
            println!("  evidence {where_} ({date}): \"{quote}\"");
        } else {
            println!("  from {where_} ({date}): {quote}");
        }
        if let Some(reason) = &d.recheck_reason {
            println!("  re-check: {}", tty(reason));
        }
        if let Some(by) = &d.superseded_by {
            println!("  superseded by {}", tty(by));
        }
        if !d.supersedes.is_empty() {
            println!("  supersedes {}", tty(&d.supersedes.join(", ")));
        }
    }
    if !report.already_recorded.is_empty() {
        println!();
        println!(
            "Already recorded (skipped): {}",
            tty(&report.already_recorded.join(", "))
        );
    }
    if !report.records.is_empty() {
        println!();
        println!("Recorded decisions:");
        for r in &report.records {
            let trust = r.trust.map_or("no evidence", Trust::label);
            println!("[{trust}] {}", tty(&r.id));
            if let Some(reason) = &r.recheck_reason {
                println!("  re-check: {}", tty(reason));
            }
            if let Some(by) = &r.superseded_by {
                println!("  superseded by {}", tty(by));
            }
        }
    }
}

/// `decisions confirm`; `args` are the words after `confirm`.
pub(crate) fn run_confirm(args: &[String]) -> i32 {
    let mut opts = dh::ConfirmOptions::default();
    let mut draft: Option<String> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--id" => match it.next() {
                Some(v) => opts.id = Some(v.clone()),
                None => return usage_error("--id needs a slug"),
            },
            "--decided-by" => match it.next() {
                Some(v) => opts.decided_by = Some(v.clone()),
                None => return usage_error("--decided-by needs owner, team or review"),
            },
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            other => {
                if let Some(v) = other.strip_prefix("--id=") {
                    opts.id = Some(v.to_string());
                } else if let Some(v) = other.strip_prefix("--decided-by=") {
                    opts.decided_by = Some(v.to_string());
                } else if other.starts_with('-') || draft.is_some() {
                    return usage_error(&format!("unknown argument: {other}"));
                } else {
                    draft = Some(other.to_string());
                }
            }
        }
    }
    let Some(draft) = draft else {
        return usage_error("confirm needs a draft id (see `lean-ctx decisions suggest`)");
    };
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => return fail(&format!("current directory: {e}")),
    };
    let root = match dh::repo_root(&cwd) {
        Ok(r) => r,
        Err(e) => return fail(&format!("not a git repository: {e}")),
    };
    match dh::confirm(&root, &draft, &opts) {
        Ok(rec) => {
            println!(
                "Recorded {} (trust {}). Review statement, rationale, scope and checks; \
                 `lean-ctx decisions check` validates every record.",
                tty(&rec.file),
                rec.trust.label()
            );
            if !rec.supersedes.is_empty() {
                println!("Supersedes {}.", tty(&rec.supersedes.join(", ")));
            }
            0
        }
        Err(e) => {
            eprintln!("lean-ctx decisions confirm: {}", tty(&e));
            if e.starts_with("no draft") || e.starts_with("no review file") {
                2
            } else {
                1
            }
        }
    }
}

/// Commit text for a terminal: control characters (C0, DEL, C1, so no
/// escape sequences) and bidi controls become `?`.
fn tty(s: &str) -> String {
    lean_ctx::core::decisions::terminal_safe(s)
}

/// Usage errors exit 2.
fn usage_error(msg: &str) -> i32 {
    eprintln!("lean-ctx decisions: {}\n\n{USAGE}", tty(msg));
    2
}

/// Runtime errors exit 1.
fn fail(msg: &str) -> i32 {
    eprintln!("lean-ctx decisions suggest: {}", tty(msg));
    1
}

#[cfg(test)]
mod tests {
    use super::tty;

    #[test]
    fn terminal_output_drops_escape_sequences() {
        assert_eq!(tty("ok \u{1b}[2J\u{9b}31m\u{202e}x\ty"), "ok ?[2J?31m?x?y");
        assert_eq!(tty("Merge with merge commits"), "Merge with merge commits");
    }
}
