//! Tests for `CLAUDE_CONFIG_DIR` support in instructions and compiler output.
//!
//! These tests modify process-global env vars — must run serialized.

use serial_test::serial;

#[test]
#[serial]
fn claude_code_instructions_default_path() {
    // Ensure CLAUDE_CONFIG_DIR is unset so we get the default.
    let prev = std::env::var("CLAUDE_CONFIG_DIR").ok();
    unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") };

    let instr = lean_ctx::instructions::claude_code_instructions();
    assert!(
        instr.contains("Full instructions at ~/.claude/CLAUDE.md"),
        "Default instructions should reference ~/.claude/CLAUDE.md, got:\n{instr}"
    );

    // Restore.
    if let Some(v) = prev {
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", v) };
    }
}

#[test]
#[serial]
fn claude_code_instructions_custom_config_dir() {
    let prev = std::env::var("CLAUDE_CONFIG_DIR").ok();
    unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", "~/.arc/claude") };

    let instr = lean_ctx::instructions::claude_code_instructions();
    assert!(
        instr.contains("Full instructions at ~/.arc/claude/CLAUDE.md"),
        "Custom CLAUDE_CONFIG_DIR should appear in instructions, got:\n{instr}"
    );
    assert!(
        !instr.contains("Full instructions at ~/.claude/CLAUDE.md"),
        "Default path should NOT appear when CLAUDE_CONFIG_DIR is set"
    );

    // Restore.
    match prev {
        Some(v) => unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", v) },
        None => unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") },
    }
}

#[test]
#[serial]
fn claude_config_dir_display_resolves_home() {
    let prev = std::env::var("CLAUDE_CONFIG_DIR").ok();

    let home = dirs::home_dir().expect("need home dir for test");
    let custom = format!("{}/.arc/claude", home.display());
    unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", &custom) };

    let display = lean_ctx::instructions::claude_config_dir_display();
    assert_eq!(
        display, "~/.arc/claude",
        "Absolute path under $HOME should be collapsed to tilde form"
    );

    // Restore.
    match prev {
        Some(v) => unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", v) },
        None => unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") },
    }
}

#[test]
#[serial]
fn claude_config_dir_display_tilde_passthrough() {
    let prev = std::env::var("CLAUDE_CONFIG_DIR").ok();
    unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", "~/.custom/claude") };

    let display = lean_ctx::instructions::claude_config_dir_display();
    assert_eq!(display, "~/.custom/claude");

    // Restore.
    match prev {
        Some(v) => unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", v) },
        None => unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") },
    }
}
