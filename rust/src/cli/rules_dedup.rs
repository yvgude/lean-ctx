// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx rules dedup [--apply]`: report or apply the dedup plan of
//! [`crate::core::rules_dedup`].

use crate::core::rules_dedup::{Action, apply, plan};

pub use crate::core::rules_dedup::auto_apply;

/// CLI entry: `lean-ctx rules dedup [--apply]`.
pub fn run(apply_changes: bool) -> i32 {
    let Some(home) = dirs::home_dir() else {
        eprintln!("Error: could not determine home directory");
        return 1;
    };
    let project = std::env::current_dir().unwrap_or_else(|_| home.clone());
    let actions = plan(&home, &project);

    if actions.is_empty() {
        println!("No duplicated lean-ctx rules found — every client pays once.");
        return 0;
    }

    println!(
        "{} (project: {})\n",
        if apply_changes {
            "Deduplicating lean-ctx rules"
        } else {
            "Dedup plan (dry-run — pass --apply to execute)"
        },
        project.display()
    );

    let mut fixable = 0usize;
    for action in &actions {
        match action {
            Action::DeleteFile { path, reason } => {
                fixable += 1;
                if apply_changes {
                    println!("  {}", apply(action));
                } else {
                    println!("  delete    {}\n            ({reason})", path.display());
                }
            }
            Action::StripBlocks { path, reason } => {
                fixable += 1;
                if apply_changes {
                    println!("  {}", apply(action));
                } else {
                    println!("  strip     {}\n            ({reason})", path.display());
                }
            }
            Action::StripCompression { path, reason } => {
                fixable += 1;
                if apply_changes {
                    println!("  {}", apply(action));
                } else {
                    println!("  thin      {}\n            ({reason})", path.display());
                }
            }
            Action::Report { .. } => println!("  {}", apply(action)),
        }
    }

    if !apply_changes && fixable > 0 {
        println!("\nRun `lean-ctx rules dedup --apply` to fix {fixable} duplicate(s).");
    }
    if apply_changes && fixable > 0 {
        println!("\nDone. Verify with `lean-ctx doctor overhead`.");
    }
    0
}
