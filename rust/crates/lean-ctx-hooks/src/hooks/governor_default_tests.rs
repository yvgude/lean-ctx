// SPDX-License-Identifier: Apache-2.0

//! The governor default: new installs, existing setups kept, pins, and the
//! switch back of an automatic 3.12.1 migration — all in isolated homes and
//! lean-ctx dirs.

use super::*;
use crate::hooks::agents::claude_governor;

/// Isolated lean-ctx dirs (holds the env lock), a temp `HOME`, a stable hook
/// binary and no mode overrides.
struct TestEnv {
    home: tempfile::TempDir,
    prev_home: Option<std::ffi::OsString>,
    _iso: lean_ctx::core::data_dir::IsolatedDataDir,
}

impl TestEnv {
    fn new() -> Self {
        let iso = lean_ctx::core::data_dir::isolated_data_dir();
        let home = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("HOME");
        lean_ctx::test_env::set_var("HOME", home.path());
        lean_ctx::test_env::set_var("LEAN_CTX_HOOK_BINARY", "lean-ctx");
        for var in ["CLAUDE_CONFIG_DIR", "LEAN_CTX_HOOK_MODE", "LEAN_CTX_QUIET"] {
            lean_ctx::test_env::remove_var(var);
        }
        std::fs::create_dir_all(home.path().join(".claude")).unwrap();
        Self {
            home,
            prev_home,
            _iso: iso,
        }
    }

    fn path(&self) -> &Path {
        self.home.path()
    }

    fn settings(&self) -> String {
        std::fs::read_to_string(self.path().join(".claude/settings.json")).unwrap_or_default()
    }

    fn mcp(&self) -> serde_json::Value {
        let raw = std::fs::read_to_string(self.path().join(".claude.json")).unwrap_or_default();
        serde_json::from_str(&raw).unwrap_or_default()
    }

    fn claude_md(&self) -> String {
        std::fs::read_to_string(self.path().join(".claude/CLAUDE.md")).unwrap_or_default()
    }

    fn snapshot(&self) -> (String, serde_json::Value, String) {
        (self.settings(), self.mcp(), self.claude_md())
    }

    /// A pre-governor hybrid install: standard hooks, MCP entry, rules block
    /// below the user's own text.
    fn standard_setup(&self) {
        crate::hooks::install_claude_hook_config(self.path());
        self.mcp_entry();
        std::fs::write(
            self.path().join(".claude/CLAUDE.md"),
            "# My notes\nkeep me\n\n<!-- lean-ctx -->\nstandard block: use ctx_read\n<!-- /lean-ctx -->\n",
        )
        .unwrap();
    }

    fn mcp_entry(&self) {
        std::fs::write(
            self.path().join(".claude.json"),
            r#"{"projects":{"/x":{}},"mcpServers":{"lean-ctx":{"command":"lean-ctx","alwaysLoad":true},"other":{"command":"o"}}}"#,
        )
        .unwrap();
    }

    /// What 3.12.1's update did: back up every touched file, then install the
    /// no-MCP governor and mark the host migrated.
    fn auto_migrate(&self) {
        for file in claude_governor::touched_files(self.path()) {
            if file.is_file() {
                let mut name = file.file_name().unwrap().to_os_string();
                name.push(".pre-governor-3.12.1-dogfood.9.bak");
                std::fs::copy(&file, file.with_file_name(name)).unwrap();
            }
        }
        claude_governor::install(self.path(), claude_governor::GovernorArm::NoMcp);
        write_state(state_file(claude(), "done").as_ref(), "3.12.1-dogfood.9");
        assert!(claude_governor::is_installed(self.path()));
        assert!(self.mcp().pointer("/mcpServers/lean-ctx").is_none());
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        lean_ctx::test_env::remove_var("LEAN_CTX_HOOK_BINARY");
        match &self.prev_home {
            Some(h) => lean_ctx::test_env::set_var("HOME", h),
            None => lean_ctx::test_env::remove_var("HOME"),
        }
    }
}

fn claude() -> &'static GovernorHost {
    host("claude").expect("Claude Code has a governor")
}

#[test]
fn hosts_with_tool_hooks_have_a_governor() {
    assert!(host("claude").is_some() && host("Claude-Code").is_some());
    for with in ["codex", "cursor", "copilot", "gemini", "codebuddy"] {
        assert!(host(with).is_some(), "{with} has a governor");
    }
    for other in ["zed", "jetbrains"] {
        assert!(host(other).is_none(), "{other} keeps its setup");
    }
}

/// Owner 2026-11-04: a new Claude Code install gets governor-mcp, so Hive,
/// the agent bus and knowledge work from the first session.
#[test]
fn a_new_claude_install_gets_governor_mcp() {
    let env = TestEnv::new();
    assert!(!claude_governor::has_standard_setup(env.path()));
    let outcome = apply_default(claude(), env.path());
    assert_eq!(outcome, Outcome::Installed(GovernorArm::McpDeferred));
    assert_eq!(
        claude_governor::installed_arm(env.path()),
        Some(GovernorArm::McpDeferred)
    );
    let entry = &env.mcp()["mcpServers"]["lean-ctx"];
    assert_eq!(entry["alwaysLoad"], false, "deferred tools: {entry}");
    assert!(env.settings().contains("lean-ctx hook govern"));
    assert!(outcome.notice(claude()).unwrap().contains("governor-mcp"));
    // A later setup run is a no-op.
    let before = env.snapshot();
    assert_eq!(
        apply_default(claude(), env.path()),
        Outcome::AlreadyGovernor
    );
    assert_eq!(env.snapshot(), before);
}

/// Owner 2026-11-04: an update or a setup refresh never switches an existing
/// setup, and writes no backup.
#[test]
fn an_existing_standard_setup_is_never_migrated() {
    let env = TestEnv::new();
    env.standard_setup();
    let before = env.snapshot();
    for _ in 0..2 {
        let outcome = apply_default(claude(), env.path());
        assert_eq!(outcome, Outcome::Standard);
        assert!(outcome.standard_setup_runs() && !outcome.ran_steps());
    }
    assert_eq!(env.snapshot(), before);
    assert!(migration_backups(claude(), env.path()).is_empty());
    assert!(pinned_mode(claude()).is_none(), "nothing pinned");
}

/// A host 3.12.1 switched automatically is switched back: governor artifacts
/// gone, the previous mode pinned, the standard steps own it again; the next
/// run is a no-op.
#[test]
fn an_automatic_migration_is_switched_back_and_pinned() {
    let env = TestEnv::new();
    env.standard_setup();
    env.auto_migrate();

    let outcome = apply_default(claude(), env.path());
    let Outcome::Restored { mode, from } = &outcome else {
        panic!("restored: {outcome:?}");
    };
    assert_eq!(*mode, HookMode::Hybrid, "MCP entry + hooks in the backup");
    assert_eq!(from.len(), 3, "settings, .claude.json, CLAUDE.md: {from:?}");
    assert!(outcome.standard_setup_runs() && outcome.ran_steps());
    assert!(
        outcome
            .notice(claude())
            .unwrap()
            .contains("switched back to the hybrid setup")
    );
    assert!(
        !claude_governor::is_installed(env.path()),
        "governor hooks gone"
    );
    assert_eq!(pinned_mode(claude()).as_deref(), Some("hybrid"));
    assert!(!state_file(claude(), "done").unwrap().exists());
    assert!(
        env.mcp().pointer("/mcpServers/other").is_some(),
        "foreign kept"
    );

    // The standard steps render the pinned mode (`recommend_hook_mode`).
    assert_eq!(
        crate::hooks::recommend_hook_mode("claude"),
        HookMode::Hybrid
    );
    let before = env.snapshot();
    assert_eq!(
        apply_default(claude(), env.path()),
        Outcome::Pinned("hybrid".into())
    );
    assert_eq!(env.snapshot(), before);
}

/// The mode comes from the backup: the MCP entry alone was an `mcp` setup.
#[test]
fn a_switched_back_mcp_only_setup_stays_mcp() {
    let env = TestEnv::new();
    env.mcp_entry();
    env.auto_migrate();
    match apply_default(claude(), env.path()) {
        Outcome::Restored { mode, .. } => assert_eq!(mode, HookMode::Mcp),
        other => panic!("restored: {other:?}"),
    }
    assert_eq!(pinned_mode(claude()).as_deref(), Some("mcp"));
    assert_eq!(crate::hooks::recommend_hook_mode("claude"), HookMode::Mcp);
}

/// A crash between the 3.12.1 journal and the last step (MCP entry already
/// gone): a lifecycle run (MCP server start, hook refresh) leaves it, since it
/// cannot register the MCP entry again; the next setup run switches it back.
#[test]
fn an_interrupted_migration_is_switched_back_by_the_next_setup_run() {
    let env = TestEnv::new();
    env.standard_setup();
    claude_governor::install(env.path(), claude_governor::GovernorArm::NoMcp);
    write_state(state_file(claude(), "pending").as_ref(), "3.12.1");
    let before = env.snapshot();
    complete_pending(env.path());
    assert_eq!(env.snapshot(), before, "no half switch-back without MCP");
    assert!(state_file(claude(), "pending").unwrap().exists());

    assert!(matches!(
        apply_default(claude(), env.path()),
        Outcome::Restored {
            mode: HookMode::Hybrid,
            ..
        }
    ));
    assert!(!claude_governor::is_installed(env.path()));
    assert!(!state_file(claude(), "pending").unwrap().exists());
    assert_eq!(pinned_mode(claude()).as_deref(), Some("hybrid"));
}

/// 3.12.1 only installed the no-MCP arm. governor-mcp next to its backups
/// was chosen afterwards (3.12.1's `init --mode governor-mcp` wrote no pin)
/// and stays.
#[test]
fn a_governor_mcp_chosen_after_the_migration_stays() {
    let env = TestEnv::new();
    env.standard_setup();
    env.auto_migrate();
    claude_governor::install(env.path(), claude_governor::GovernorArm::McpDeferred);
    let before = env.snapshot();
    assert_eq!(
        apply_default(claude(), env.path()),
        Outcome::AlreadyGovernor
    );
    assert_eq!(env.snapshot(), before);
    assert!(pinned_mode(claude()).is_none());
}

/// Gemini CLI keeps hooks and MCP servers in one settings.json: its hooks in
/// that backup make the switch back hybrid, not mcp.
#[test]
fn a_switched_back_gemini_setup_keeps_its_hooks() {
    let env = TestEnv::new();
    let gemini = host("gemini").expect("Gemini has a governor");
    let settings = (gemini.mcp_config_path)(env.path());
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        r#"{"mcpServers":{"lean-ctx":{"command":"lean-ctx"}},"hooks":{"BeforeTool":[{"matcher":"run_shell_command","hooks":[{"type":"command","command":"lean-ctx hook rewrite"}]}]}}"#,
    )
    .unwrap();
    for file in (gemini.touched_files)(env.path()) {
        if file.is_file() {
            let mut name = file.file_name().unwrap().to_os_string();
            name.push(".pre-governor-3.12.1.bak");
            std::fs::copy(&file, file.with_file_name(name)).unwrap();
        }
    }
    install(gemini, env.path(), GovernorArm::NoMcp);
    assert!((gemini.is_installed)(env.path()));
    assert!(matches!(
        apply_default(gemini, env.path()),
        Outcome::Restored {
            mode: HookMode::Hybrid,
            ..
        }
    ));
}

#[test]
fn pinned_and_governor_pinned_hosts_are_untouched() {
    let env = TestEnv::new();
    env.standard_setup();
    pin(claude(), Some("hybrid"));
    let before = env.snapshot();
    assert_eq!(
        apply_default(claude(), env.path()),
        Outcome::Pinned("hybrid".into())
    );
    assert_eq!(env.snapshot(), before);

    // An explicit global hook_mode is a choice too.
    pin(claude(), None);
    lean_ctx::test_env::set_var("LEAN_CTX_HOOK_MODE", "replace");
    assert!(matches!(
        apply_default(claude(), env.path()),
        Outcome::Pinned(_)
    ));
    lean_ctx::test_env::remove_var("LEAN_CTX_HOOK_MODE");

    // A chosen governor survives the repair, backups or not.
    env.auto_migrate();
    pin(claude(), Some("governor"));
    let before = env.snapshot();
    assert_eq!(
        apply_default(claude(), env.path()),
        Outcome::AlreadyGovernor
    );
    assert_eq!(env.snapshot(), before);
}

#[test]
fn a_governor_pin_without_the_setup_installs_that_arm() {
    let env = TestEnv::new();
    env.standard_setup();
    pin(claude(), Some("governor-mcp"));
    assert_eq!(
        apply_default(claude(), env.path()),
        Outcome::Installed(GovernorArm::McpDeferred)
    );
    assert_eq!(
        claude_governor::installed_arm(env.path()),
        Some(GovernorArm::McpDeferred)
    );
}

#[test]
fn an_opt_in_governor_gets_the_rule_modes_once_and_keeps_its_arm() {
    let env = TestEnv::new();
    // A pre-default `--mode governor-mcp` install: no [governor] rule keys,
    // except one the user set.
    claude_governor::install(env.path(), claude_governor::GovernorArm::McpDeferred);
    // Written directly: `update_global` keeps keys already on disk (the
    // preserving writer), so it cannot remove the ones `install` just wrote.
    let config = lean_ctx::core::config::Config::path().expect("config path");
    std::fs::write(&config, "[governor]\nr2 = \"off\"\n").unwrap();
    let before = env.snapshot();

    let outcome = apply_default(claude(), env.path());
    let Outcome::RuleModesEnabled(backups) = &outcome else {
        panic!("rule modes: {outcome:?}");
    };
    let bak = backups.first().expect("config backup");
    assert!(
        std::fs::read_to_string(bak)
            .unwrap()
            .contains("r2 = \"off\"")
    );
    let cfg = lean_ctx::core::config::Config::load_global().governor;
    assert_eq!(
        [
            cfg.get("r1_bash"),
            cfg.get("r2"),
            cfg.get("r3"),
            cfg.get("r4")
        ],
        [Some("on"), Some("off"), Some("shadow"), Some("on")],
        "unset keys filled, the user's key kept"
    );
    assert_eq!(
        env.snapshot(),
        before,
        "hooks, MCP entry and rules block untouched"
    );
    assert!(outcome.notice(claude()).is_some());
    assert_eq!(
        apply_default(claude(), env.path()),
        Outcome::AlreadyGovernor
    );

    // Interrupted after the journal: the next lifecycle run completes it.
    lean_ctx::core::config::Config::update_global(|c| c.governor.r4 = None).unwrap();
    write_state(state_file(claude(), "rules-pending").as_ref(), VERSION);
    complete_pending(env.path());
    assert_eq!(
        lean_ctx::core::config::Config::load_global()
            .governor
            .get("r4"),
        Some("on")
    );
    assert!(!state_file(claude(), "rules-pending").unwrap().exists());
}

/// The IDE hosts: a new install gets the standard MCP setup (owner
/// 2026-11-04), an existing setup is kept, and an automatic 3.12.1 switch is
/// switched back.
#[test]
fn an_ide_host_keeps_or_gets_back_its_standard_setup() {
    let codebuddy = host("codebuddy").expect("CodeBuddy has a governor");

    let fresh = TestEnv::new();
    assert_eq!(apply_default(codebuddy, fresh.path()), Outcome::Standard);
    assert!(!(codebuddy.is_installed)(fresh.path()));
    drop(fresh);

    let existing = TestEnv::new();
    let mcp = existing.path().join(".codebuddy/mcp.json");
    std::fs::create_dir_all(mcp.parent().unwrap()).unwrap();
    std::fs::write(
        &mcp,
        r#"{"mcpServers":{"lean-ctx":{"command":"lean-ctx"}}}"#,
    )
    .unwrap();
    assert!((codebuddy.has_standard_setup)(existing.path()));
    assert_eq!(apply_default(codebuddy, existing.path()), Outcome::Standard);
    assert!(!(codebuddy.is_installed)(existing.path()));

    // 3.12.1's switch: backups, then the governor steps.
    for file in (codebuddy.touched_files)(existing.path()) {
        if file.is_file() {
            let mut name = file.file_name().unwrap().to_os_string();
            name.push(".pre-governor-3.12.1.bak");
            std::fs::copy(&file, file.with_file_name(name)).unwrap();
        }
    }
    install(codebuddy, existing.path(), GovernorArm::NoMcp);
    assert!((codebuddy.is_installed)(existing.path()));
    assert!(matches!(
        apply_default(codebuddy, existing.path()),
        Outcome::Restored {
            mode: HookMode::Mcp,
            ..
        }
    ));
    assert!(!(codebuddy.is_installed)(existing.path()));
    drop(existing);

    let pinned = TestEnv::new();
    lean_ctx::test_env::set_var("LEAN_CTX_HOOK_MODE", "hybrid");
    assert!(matches!(
        apply_default(codebuddy, pinned.path()),
        Outcome::Pinned(_)
    ));
    lean_ctx::test_env::remove_var("LEAN_CTX_HOOK_MODE");
    assert!(!(codebuddy.is_installed)(pinned.path()));
}
