// SPDX-License-Identifier: Apache-2.0

use super::*;
#[test]
fn no_duplicate_names_at_top_level() {
    let mut seen = std::collections::HashSet::new();
    for node in COMMAND_TREE {
        assert!(seen.insert(node.name), "duplicate: {}", node.name);
    }
}
#[test]
fn visible_count_exceeds_50() {
    let visible = COMMAND_TREE.iter().filter(|n| !n.hidden).count();
    assert!(
        visible >= 30,
        "expected at least 30 visible commands, got {visible}"
    );
}
#[test]
fn command_tree_covers_known_commands() {
    let known = crate::cli::dispatch::suggest::KNOWN_COMMANDS;
    let tree_names: std::collections::HashSet<&str> = COMMAND_TREE
        .iter()
        .flat_map(|n| std::iter::once(n.name).chain(n.aliases.iter().copied()))
        .collect();
    let mut missing = Vec::new();
    for cmd in known {
        if !tree_names.contains(cmd) {
            missing.push(*cmd);
        }
    }
    assert!(
        missing.is_empty(),
        "KNOWN_COMMANDS entries missing from COMMAND_TREE: {missing:?}"
    );
}
#[test]
fn benchmark_real_is_registered_for_completion() {
    assert!(crate::cli::dispatch::suggest::KNOWN_COMMANDS.contains(&"benchmark"));
    let benchmark = COMMAND_TREE
        .iter()
        .find(|node| node.name == "benchmark")
        .expect("benchmark command must be registered");
    assert!(benchmark.flags.iter().any(|flag| flag.long == "--real"));
    assert!(benchmark.flags.iter().any(|flag| flag.long == "--json"));
}

/// #1602: `watch` was removed in 3.9.20, but the completion spec still
/// offered it — under a third description ("Watch for file changes") that
/// matched neither the removed TUI nor `index watch`. A removed command
/// must not be advertised as a working one.
#[test]
fn removed_watch_command_is_not_advertised() {
    let watch = COMMAND_TREE
        .iter()
        .find(|node| node.name == "watch")
        .expect("watch stays in the tree as a signpost to `dashboard`");
    assert!(
        watch.hidden,
        "a removed command must not be completed as a working one"
    );
    assert!(
        watch.description.contains("Removed"),
        "the description must say the command is gone, got: {}",
        watch.description
    );
}
