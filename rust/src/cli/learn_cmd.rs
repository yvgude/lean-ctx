use crate::core::gotcha_tracker::{self, GotchaStore, learn};

pub(crate) fn cmd_learn(args: &[String]) {
    // Offline mining mode: `lean-ctx learn --mine <dir>` distills recurring
    // error signatures from a directory of .jsonl transcripts/logs.
    if let Some(pos) = args.iter().position(|a| a == "--mine") {
        let dir = args.get(pos + 1).map(String::as_str);
        cmd_learn_mine(dir);
        return;
    }

    let project_root = super::common::detect_project_root(args);
    let apply = args.iter().any(|a| a == "--apply");

    let gotchas = gotcha_tracker::load_universal_gotchas();
    let store = GotchaStore {
        project_hash: String::new(),
        gotchas,
        error_log: Vec::new(),
        stats: gotcha_tracker::GotchaStats::default(),
        updated_at: chrono::Utc::now(),
        pending_errors: Vec::new(),
    };

    let learnings = learn::extract_learnings(&store);

    if learnings.is_empty() {
        println!(
            "No learnings yet. lean-ctx needs to detect and resolve errors across sessions first."
        );
        println!("Tip: Use lean-ctx normally — errors are automatically tracked and correlated.");
        return;
    }

    println!("=== Learned Gotchas ({} total) ===\n", learnings.len());
    for l in &learnings {
        println!("  {l}");
    }

    if apply {
        println!();
        match learn::apply_to_agents_md(&project_root, &learnings) {
            Ok(msg) => println!("{msg}"),
            Err(e) => eprintln!("Error: {e}"),
        }
    } else {
        println!("\nUse `lean-ctx learn --apply` to write these to AGENTS.md.");
    }
}

/// `lean-ctx learn --mine <dir>`: distill recurring error signatures from a
/// directory of `.jsonl` transcripts/logs. Read-only — it surfaces the project's
/// recurring pain points for review, it never mutates stored state.
fn cmd_learn_mine(dir: Option<&str>) {
    let Some(dir) = dir else {
        eprintln!("Usage: lean-ctx learn --mine <dir>  (directory of .jsonl transcripts/logs)");
        return;
    };
    let path = std::path::Path::new(dir);
    if !path.is_dir() {
        eprintln!("Error: '{dir}' is not a directory");
        return;
    }
    let mined = gotcha_tracker::mining::mine_jsonl_dir(path);
    println!(
        "{}",
        gotcha_tracker::mining::format_mining_report(&mined, 2)
    );
}
