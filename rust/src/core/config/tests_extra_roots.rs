use super::*;

#[test]
fn default_is_empty() {
    let cfg = Config::default();
    assert!(cfg.extra_roots.is_empty());
}

#[test]
fn deserialization_from_toml() {
    let cfg: Config = toml::from_str(r#"extra_roots = ["/data/store", "/test/env"]"#).unwrap();
    assert_eq!(cfg.extra_roots, vec!["/data/store", "/test/env"]);
}

#[test]
fn merge_extends() {
    let mut base = Config {
        extra_roots: vec!["/base".to_string()],
        ..Config::default()
    };
    base.merge_local(r#"extra_roots = ["/local"]"#, true);
    assert_eq!(base.extra_roots, vec!["/base", "/local"]);
}

#[test]
fn merge_local_omitting_shell_allowlist_keeps_global() {
    // Regression: the field defaults (via serde) to the full built-in list, so a
    // local override that never mentions `shell_allowlist` must NOT clobber a
    // deliberately shorter global allowlist.
    let mut base = Config {
        shell_allowlist: vec!["git".to_string(), "cargo".to_string()],
        ..Config::default()
    };
    base.merge_local(r"minimal_overhead = true", true);
    assert_eq!(base.shell_allowlist, vec!["git", "cargo"]);
}

#[test]
fn merge_local_defining_shell_allowlist_overrides() {
    let mut base = Config {
        shell_allowlist: vec!["git".to_string(), "cargo".to_string()],
        ..Config::default()
    };
    base.merge_local(r#"shell_allowlist = ["npm"]"#, true);
    assert_eq!(base.shell_allowlist, vec!["npm"]);
}

#[test]
fn merge_local_empty_shell_allowlist_disables_restriction() {
    // Explicit empty list = intentional blocklist-only mode; must be honored.
    let mut base = Config {
        shell_allowlist: vec!["git".to_string()],
        ..Config::default()
    };
    base.merge_local(r"shell_allowlist = []", true);
    assert!(base.shell_allowlist.is_empty());
}

#[test]
fn merge_local_untrusted_withholds_sensitive_keeps_comfort() {
    // Finding 4: an untrusted workspace's security-sensitive overrides
    // (allowlist, path-jail widening) are withheld, but a comfort override
    // (theme) still applies — selective gating, not a blanket block.
    let mut base = Config {
        shell_allowlist: vec!["git".to_string()],
        ..Config::default()
    };
    base.merge_local(
        "shell_allowlist = [\"rm\"]\nextra_roots = [\"/etc\"]\ntheme = \"midnight\"\n",
        false,
    );
    assert_eq!(
        base.shell_allowlist,
        vec!["git"],
        "sensitive allowlist override must be withheld for untrusted workspace"
    );
    assert!(
        base.extra_roots.is_empty(),
        "path-jail widening must be withheld for untrusted workspace"
    );
    assert_eq!(
        base.theme, "midnight",
        "comfort override must still apply for untrusted workspace"
    );
}

#[test]
fn merge_local_trusted_applies_sensitive() {
    let mut base = Config {
        shell_allowlist: vec!["git".to_string()],
        ..Config::default()
    };
    base.merge_local(
        "shell_allowlist = [\"rm\"]\nextra_roots = [\"/etc\"]\n",
        true,
    );
    assert_eq!(base.shell_allowlist, vec!["rm"]);
    assert_eq!(base.extra_roots, vec!["/etc"]);
}

/// GH #833: untrusted workspace must not disable gitignore-respecting indexing.
#[test]
fn merge_local_untrusted_withholds_respect_gitignore_833() {
    let mut base = Config::default();
    assert!(
        base.index.respect_gitignore,
        "default must respect gitignore"
    );
    base.merge_local("[index]\nrespect_gitignore = false\n", false);
    assert!(
        base.index.respect_gitignore,
        "untrusted workspace must not disable gitignore respect"
    );
}

/// `semantic_mode = "eager"` would let background enrichment start language
/// servers, which execute project code: an untrusted workspace may only lower
/// the mode, a trusted one may raise it.
#[test]
fn merge_local_semantic_mode_escalation_requires_trust() {
    use crate::core::config::SemanticMode;
    let mut untrusted = Config::default();
    untrusted.merge_local("semantic_mode = \"eager\"\n", false);
    assert_eq!(untrusted.semantic_mode, SemanticMode::Auto);

    let mut lowered = Config::default();
    lowered.merge_local("semantic_mode = \"off\"\n", false);
    assert_eq!(lowered.semantic_mode, SemanticMode::Off);

    let mut trusted = Config::default();
    trusted.merge_local("semantic_mode = \"eager\"\n", true);
    assert_eq!(trusted.semantic_mode, SemanticMode::Eager);
}

/// Regression (review): the semantic mode for a project must come from *that*
/// project's `.lean-ctx.toml` and trust — a daemon serving several roots must
/// not apply the ambient (cwd) project's `eager` to an untrusted repo.
#[test]
fn semantic_mode_is_evaluated_for_the_target_project() {
    use crate::core::config::SemanticMode;
    let _lock = crate::core::data_dir::test_env_lock();
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(
        repo.path().join(".lean-ctx.toml"),
        "semantic_mode = \"eager\"\n",
    )
    .unwrap();
    let empty = tempfile::tempdir().unwrap();
    let root = repo.path().to_string_lossy().to_string();
    let global_only = Config::load_for_project_root(&empty.path().to_string_lossy()).semantic_mode;

    crate::test_env::remove_var("LEAN_CTX_TRUST_WORKSPACE");
    assert_eq!(
        Config::load_for_project_root(&root).semantic_mode,
        global_only,
        "untrusted repo cannot raise the mode"
    );

    crate::test_env::set_var("LEAN_CTX_TRUST_WORKSPACE", "1");
    let trusted = Config::load_for_project_root(&root).semantic_mode;
    crate::test_env::remove_var("LEAN_CTX_TRUST_WORKSPACE");
    assert_eq!(
        trusted,
        SemanticMode::Eager,
        "its own trusted config applies"
    );

    // Review round 2: `eager` from the user's environment must not start
    // servers in an untrusted repo either — it runs as `auto` there.
    crate::test_env::set_var("LEAN_CTX_SEMANTIC_MODE", "eager");
    let untrusted_env = SemanticMode::for_project(&root);
    crate::test_env::set_var("LEAN_CTX_TRUST_WORKSPACE", "1");
    let trusted_env = SemanticMode::for_project(&root);
    crate::test_env::remove_var("LEAN_CTX_TRUST_WORKSPACE");
    crate::test_env::remove_var("LEAN_CTX_SEMANTIC_MODE");
    assert_eq!(untrusted_env, SemanticMode::Auto);
    assert_eq!(trusted_env, SemanticMode::Eager);
}

/// GH #833: trusted workspace CAN disable gitignore respect.
#[test]
fn merge_local_trusted_allows_respect_gitignore_833() {
    let mut base = Config::default();
    base.merge_local("[index]\nrespect_gitignore = false\n", true);
    assert!(
        !base.index.respect_gitignore,
        "trusted workspace must be able to disable gitignore respect"
    );
}

/// GH #1419 follow-up: untrusted workspace must not weaken shell-allowlist
/// subcommand scoping back to the vulnerable base-binary-only matching.
#[test]
fn merge_local_untrusted_withholds_shell_allowlist_subcommand_scoping() {
    let mut base = Config::default();
    assert!(
        base.shell_allowlist_subcommand_scoping,
        "default must scope multi-word allowlist entries to their subcommand"
    );
    base.merge_local("shell_allowlist_subcommand_scoping = false\n", false);
    assert!(
        base.shell_allowlist_subcommand_scoping,
        "untrusted workspace must not disable subcommand scoping"
    );
}

/// GH #1419 follow-up: trusted workspace CAN opt back into the pre-fix,
/// base-binary-only matching as a deliberate compat/rollback path.
#[test]
fn merge_local_trusted_allows_shell_allowlist_subcommand_scoping() {
    let mut base = Config::default();
    base.merge_local("shell_allowlist_subcommand_scoping = false\n", true);
    assert!(
        !base.shell_allowlist_subcommand_scoping,
        "trusted workspace must be able to disable subcommand scoping"
    );
}

#[test]
fn merge_local_untrusted_withholds_tool_surface_overrides() {
    // Regression: an untrusted repo's .lean-ctx.toml could silently widen
    // the agent's tool surface (tool_profile/tools_enabled/
    // default_tool_categories weren't in strip_sensitive_overrides), bypassing
    // an org-pinned minimal profile with no [SECURITY] warning.
    let mut base = Config {
        tool_profile: Some("minimal".to_string()),
        ..Config::default()
    };
    base.merge_local(
        "tool_profile = \"power\"\n\
         tools_enabled = [\"ctx_shell\"]\n\
         default_tool_categories = [\"arch\"]\n",
        false,
    );
    assert_eq!(
        base.tool_profile,
        Some("minimal".to_string()),
        "tool_profile override must be withheld for untrusted workspace"
    );
    assert!(
        base.tools_enabled.is_empty(),
        "tools_enabled override must be withheld for untrusted workspace"
    );
    assert!(
        base.default_tool_categories.is_empty(),
        "default_tool_categories override must be withheld for untrusted workspace"
    );
}

#[test]
fn merge_local_trusted_applies_tool_surface_overrides() {
    let mut base = Config {
        tool_profile: Some("minimal".to_string()),
        ..Config::default()
    };
    base.merge_local(
        "tool_profile = \"power\"\n\
         tools_enabled = [\"ctx_shell\"]\n\
         default_tool_categories = [\"arch\"]\n",
        true,
    );
    assert_eq!(base.tool_profile, Some("power".to_string()));
    assert_eq!(base.tools_enabled, vec!["ctx_shell"]);
    assert_eq!(base.default_tool_categories, vec!["arch"]);
}

#[test]
fn allow_symlink_roots_follows_extra_roots_trust_semantics() {
    // #596 premium: the symlink write-through allowlist is security-sensitive,
    // so an untrusted workspace's entry is withheld while a trusted one applies.
    let mut untrusted = Config::default();
    untrusted.merge_local(r#"allow_symlink_roots = ["/opt/dotfiles"]"#, false);
    assert!(
        untrusted.allow_symlink_roots.is_empty(),
        "symlink-escape roots must be withheld for an untrusted workspace"
    );

    let mut trusted = Config::default();
    trusted.merge_local(r#"allow_symlink_roots = ["/opt/dotfiles"]"#, true);
    assert_eq!(trusted.allow_symlink_roots, vec!["/opt/dotfiles"]);
}
