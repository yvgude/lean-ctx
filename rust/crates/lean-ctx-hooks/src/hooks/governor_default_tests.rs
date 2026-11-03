// SPDX-License-Identifier: Apache-2.0

//! The governor default: new installs, once-per-version migration with
//! backups, pins, crash completion and the revert path — all in isolated
//! homes and lean-ctx dirs.

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

    /// A pre-governor hybrid install: standard hooks, MCP entry, rules block
    /// below the user's own text.
    fn standard_setup(&self) {
        crate::hooks::install_claude_hook_config(self.path());
        std::fs::write(
            self.path().join(".claude.json"),
            r#"{"projects":{"/x":{}},"mcpServers":{"lean-ctx":{"command":"lean-ctx","alwaysLoad":true},"other":{"command":"o"}}}"#,
        )
        .unwrap();
        std::fs::write(
            self.path().join(".claude/CLAUDE.md"),
            "# My notes\nkeep me\n\n<!-- lean-ctx -->\nstandard block: use ctx_read\n<!-- /lean-ctx -->\n",
        )
        .unwrap();
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

fn assert_governor_shape(env: &TestEnv) {
    let s: serde_json::Value = serde_json::from_str(&env.settings()).unwrap();
    let timeout = crate::hook_handlers::GOVERN_HOST_TIMEOUT_SECS;
    let post = s["hooks"]["PostToolUse"].to_string();
    assert!(post.contains("lean-ctx hook govern"), "{post}");
    assert!(post.contains(&format!("\"timeout\":{timeout}")), "{post}");
    let pre = s["hooks"]["PreToolUse"].to_string();
    assert!(pre.contains("lean-ctx hook rewrite-enforce"), "{pre}");
    assert!(
        !pre.contains("hook rewrite\""),
        "no compression rewrite: {pre}"
    );
    assert!(!pre.contains("hook redirect"), "no Read redirect: {pre}");
    assert!(
        env.mcp().pointer("/mcpServers/lean-ctx").is_none(),
        "no MCP entry"
    );
    let md = env.claude_md();
    assert!(
        !md.contains("<!-- lean-ctx"),
        "zero prefix: no rules block: {md}"
    );
    let cfg = lean_ctx::core::config::Config::load_global().governor;
    for (rule, mode) in [
        ("r1_read", "on"),
        ("r1_bash", "on"),
        ("r2", "on"),
        ("r3", "shadow"),
        ("r4", "on"),
    ] {
        assert_eq!(cfg.get(rule), Some(mode), "[governor] {rule}");
    }
}

#[test]
fn hosts_with_tool_hooks_have_a_governor() {
    assert!(host("claude").is_some() && host("Claude-Code").is_some());
    for with in ["codex", "cursor", "copilot", "gemini", "codebuddy"] {
        assert!(host(with).is_some(), "{with} rides the default");
    }
    for other in ["zed", "jetbrains"] {
        assert!(host(other).is_none(), "{other} keeps its setup");
    }
}

#[test]
fn a_new_install_gets_the_governor() {
    let env = TestEnv::new();
    assert!(!claude_governor::has_standard_setup(env.path()));
    let outcome = apply_default(claude(), env.path());
    assert_eq!(outcome, Outcome::Installed);
    assert_governor_shape(&env);
    assert!(outcome.notice(claude()).unwrap().contains("--mode hybrid"));
    // A later setup run is a no-op.
    let before = env.settings();
    assert_eq!(
        apply_default(claude(), env.path()),
        Outcome::AlreadyGovernor
    );
    assert_eq!(env.settings(), before);
}

#[test]
fn a_standard_install_is_migrated_once_with_backups() {
    let env = TestEnv::new();
    env.standard_setup();
    let settings_before = env.settings();
    let outcome = apply_default(claude(), env.path());
    let Outcome::Migrated(backups) = &outcome else {
        panic!("migrated: {outcome:?}");
    };
    assert_governor_shape(&env);
    // User text outside the markers and foreign MCP servers stay.
    let md = env.claude_md();
    assert!(md.starts_with("# My notes\nkeep me\n"), "{md}");
    assert!(!md.contains("standard block"));
    assert!(env.mcp().pointer("/mcpServers/other").is_some());
    assert!(env.mcp().pointer("/projects/~1x").is_some());
    // Every changed host file has a byte-exact backup.
    let settings_bak = backups
        .iter()
        .find(|p| p.to_string_lossy().contains("settings.json.pre-governor-"))
        .expect("settings backup");
    assert_eq!(
        std::fs::read_to_string(settings_bak).unwrap(),
        settings_before
    );
    assert!(
        backups
            .iter()
            .any(|p| p.to_string_lossy().contains(".claude.json.pre-governor-"))
    );
    let md_bak = backups
        .iter()
        .find(|p| p.to_string_lossy().contains("CLAUDE.md.pre-governor-"))
        .expect("CLAUDE.md backup");
    assert!(
        std::fs::read_to_string(md_bak)
            .unwrap()
            .contains("standard block")
    );
    let notice = outcome.notice(claude()).unwrap();
    assert!(notice.contains("Backups:") && notice.contains("init --agent claude --mode hybrid"));

    // Idempotent: the second run changes nothing.
    let after = (env.settings(), env.claude_md());
    assert_eq!(
        apply_default(claude(), env.path()),
        Outcome::AlreadyGovernor
    );
    assert_eq!((env.settings(), env.claude_md()), after);
}

#[test]
fn pinned_and_governor_installs_are_untouched() {
    let env = TestEnv::new();
    env.standard_setup();
    pin(claude(), Some("hybrid"));
    let before = (env.settings(), env.claude_md());
    assert_eq!(
        apply_default(claude(), env.path()),
        Outcome::Pinned("hybrid".into())
    );
    assert_eq!((env.settings(), env.claude_md()), before);
    assert!(env.mcp().pointer("/mcpServers/lean-ctx").is_some());

    // An explicit global hook_mode is a choice too.
    pin(claude(), None);
    lean_ctx::test_env::set_var("LEAN_CTX_HOOK_MODE", "replace");
    assert!(matches!(
        apply_default(claude(), env.path()),
        Outcome::Pinned(_)
    ));
    lean_ctx::test_env::remove_var("LEAN_CTX_HOOK_MODE");
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
    let before = (env.settings(), env.mcp(), env.claude_md());

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
        (env.settings(), env.mcp(), env.claude_md()),
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

#[test]
fn an_interrupted_migration_completes_on_the_next_run() {
    let env = TestEnv::new();
    env.standard_setup();
    // Crash after the settings step, before the MCP entry is removed.
    FAIL_BEFORE_STEP.with(|c| c.set(Some(1)));
    assert_eq!(apply_default(claude(), env.path()), Outcome::Interrupted);
    FAIL_BEFORE_STEP.with(|c| c.set(None));
    assert!(claude_governor::is_installed(env.path()));
    assert!(
        env.mcp().pointer("/mcpServers/lean-ctx").is_some(),
        "half-migrated"
    );
    // Any later lifecycle path finishes it, although the hooks already look
    // like a governor install.
    complete_pending(env.path());
    assert_governor_shape(&env);
    assert!(!env.claude_md().contains("standard block"));
    assert_eq!(
        apply_default(claude(), env.path()),
        Outcome::AlreadyGovernor
    );
}

/// The IDE hosts ride the same default as Claude Code (owner 2026-11-01:
/// all installs and all IDEs, also on update): a new install gets the
/// governor, an existing standard setup is switched once with backups, and a
/// global pin leaves the host alone.
#[test]
fn an_ide_host_gets_the_same_default_migration_and_pin() {
    let codebuddy = host("codebuddy").expect("CodeBuddy has a governor");

    let fresh = TestEnv::new();
    assert_eq!(apply_default(codebuddy, fresh.path()), Outcome::Installed);
    assert!((codebuddy.is_installed)(fresh.path()));
    drop(fresh);

    let existing = TestEnv::new();
    std::fs::create_dir_all(existing.path().join(".codebuddy")).unwrap();
    std::fs::write(
        existing.path().join(".codebuddy/mcp.json"),
        r#"{"mcpServers":{"lean-ctx":{"command":"lean-ctx"}}}"#,
    )
    .unwrap();
    assert!((codebuddy.has_standard_setup)(existing.path()));
    match apply_default(codebuddy, existing.path()) {
        Outcome::Migrated(backups) => assert!(!backups.is_empty(), "the switch backs up first"),
        other => panic!("expected a migration, got {other:?}"),
    }
    assert!((codebuddy.is_installed)(existing.path()));
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
