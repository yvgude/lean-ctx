//! Integration tests for the shell completion engine.

use lean_ctx::cli::completions::spec::COMMAND_TREE;

#[test]
fn top_level_has_shell_and_config() {
    let names: Vec<&str> = COMMAND_TREE.iter().map(|n| n.name).collect();
    assert!(names.contains(&"shell"), "missing 'shell'");
    assert!(names.contains(&"config"), "missing 'config'");
    assert!(names.contains(&"proxy"), "missing 'proxy'");
}

#[test]
fn alias_command_resolves() {
    let gotchas_node = COMMAND_TREE
        .iter()
        .find(|n| n.name == "gotchas")
        .expect("gotchas command must exist");
    assert!(
        gotchas_node.aliases.contains(&"bugs"),
        "gotchas must have 'bugs' alias"
    );
}
