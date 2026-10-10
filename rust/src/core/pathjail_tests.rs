// SPDX-License-Identifier: Apache-2.0

//! Unit tests for the path jail (#660 LOC gate: split out of pathjail.rs).

use super::*;

#[cfg(not(feature = "no-jail"))]
#[test]
fn rejects_path_outside_root() {
    // Hermetic config (empty data dir => jail on) so a parallel test that
    // flips `path_jail` cannot leak into this enforcement check. The guard
    // holds the global test_env_lock, which also serializes against every
    // `LEAN_CTX_ALLOW_PATH` mutation (all of them go through that lock).
    let _iso = crate::core::data_dir::isolated_data_dir();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(root.join("a.txt"), "ok").unwrap();
    std::fs::write(other.join("b.txt"), "no").unwrap();

    let ok = jail_path(&root.join("a.txt"), &root);
    assert!(ok.is_ok());

    let bad = jail_path(&other.join("b.txt"), &root);
    assert!(bad.is_err());
}

/// #475: a configured read-only root is readable but never writable. Reads
/// resolve (the root joins the allow-list like an extra_root), while the
/// single write choke point `enforce_writable` default-denies every write
/// inside it — including a not-yet-existing file, which inherits the
/// directory's read-only status. `isolated_data_dir` holds `test_env_lock`,
/// serialising the `LEAN_CTX_READ_ONLY_ROOTS` mutation against other tests.
#[cfg(not(feature = "no-jail"))]
#[test]
fn read_only_roots_deny_writes_but_allow_reads() {
    // `isolated_data_dir` already holds `test_env_lock` for its lifetime —
    // taking it again here would self-deadlock on a non-reentrant Mutex.
    let _iso = crate::core::data_dir::isolated_data_dir();

    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    let refrepo = tmp.path().join("refrepo");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(refrepo.join("sub")).unwrap();
    std::fs::write(refrepo.join("lib.rs"), "pub fn x() {}\n").unwrap();

    // Canonicalize the configured root the same (symlink-resolving) way the
    // guard does, so macOS /var → /private/var can't defeat the prefix match.
    let ro_canon = canonicalize_secure(&refrepo);
    crate::test_env::set_var(
        "LEAN_CTX_READ_ONLY_ROOTS",
        ro_canon.to_string_lossy().as_ref(),
    );

    let existing = refrepo.join("lib.rs");
    let new_file = refrepo.join("sub").join("new.rs");
    let proj_file = project.join("main.rs");

    // Capture every decision while the env is live (it is cleared below).
    let read_existing = jail_path(&existing, &project);
    let deny_existing = enforce_writable(&existing);
    let deny_new = enforce_writable(&new_file);
    let allow_project = enforce_writable(&proj_file);
    let ro_existing = is_read_only_path(&existing);
    let ro_project = is_read_only_path(&proj_file);

    crate::test_env::remove_var("LEAN_CTX_READ_ONLY_ROOTS");

    assert!(
        deny_existing.is_err(),
        "write to an existing file in a read-only root must be denied"
    );
    assert!(
        deny_new.is_err(),
        "creating a new file in a read-only root must be denied"
    );
    assert!(
        allow_project.is_ok(),
        "writes into the project root must stay allowed: {allow_project:?}"
    );
    assert!(
        read_existing.is_ok(),
        "reads inside a read-only root must resolve (read allow-list): {read_existing:?}"
    );
    assert!(ro_existing, "the file is inside the read-only root");
    assert!(!ro_project, "the project file is not read-only");
}

/// #406 regression: a long-lived process (the MCP server) must honor
/// `path_jail = false` written to config after startup. The config cache is
/// now keyed on content, so even an edit that preserves the file mtime takes
/// effect — a path outside the jail root is accepted once the flag flips.
/// (With the former mtime-only cache the stale `None` kept the jail on.)
#[cfg(not(feature = "no-jail"))]
#[test]
fn honors_path_jail_false_after_mtime_preserving_edit() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let cfg_path = crate::core::config::Config::path().unwrap();
    if let Some(parent) = cfg_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("project");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let secret = outside.join("secret.txt");
    std::fs::write(&secret, "x").unwrap();

    // Warm the config cache with the jail on (no path_jail key).
    std::fs::write(&cfg_path, "# jail on\n").unwrap();
    let mtime0 = std::fs::metadata(&cfg_path).unwrap().modified().unwrap();
    assert_eq!(crate::core::config::Config::load().path_jail, None);

    // Flip path_jail=false but restore the original mtime, so any mtime-only
    // cache would keep serving the stale jail-on value.
    std::fs::write(&cfg_path, "path_jail = false\n").unwrap();
    filetime::set_file_mtime(&cfg_path, filetime::FileTime::from_system_time(mtime0)).unwrap();

    assert!(
        jail_path(&secret, &root).is_ok(),
        "path_jail=false must take effect without a fresh process (#406)"
    );
}

#[test]
fn allows_nonexistent_child_under_root() {
    // jail_path reads the protected-session env, which the macOS
    // write_guard tests set process-wide under this lock.
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), "ok").unwrap();

    let p = root.join("new").join("file.txt");
    let ok = jail_path(&p, &root).unwrap();
    assert!(ok.to_string_lossy().contains("file.txt"));
}

#[cfg(not(feature = "no-jail"))]
#[test]
fn relative_candidate_resolves_against_root_not_cwd() {
    // Regression: in the daemon (CWD != project) a relative graph path like
    // `sub/file.rs` must resolve under the jail root, not the process CWD.
    let _iso = crate::core::data_dir::isolated_data_dir();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("project");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub").join("file.rs"), "ok").unwrap();

    let jailed = jail_path(Path::new("sub/file.rs"), &root)
        .expect("relative candidate should resolve under the jail root");
    assert!(jailed.ends_with("sub/file.rs"));
    assert!(
        is_under_prefix(&canonicalize_or_self(&jailed), &canonicalize_or_self(&root)),
        "resolved path must live under the jail root: {jailed:?}"
    );
}

#[test]
fn ide_allow_dirs_are_registry_derived_and_skip_home() {
    use crate::core::editor_registry::{ConfigType, EditorTarget};

    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    // VS Code keeps its config outside a dotfile dir — the old hard-coded
    // list missed this entirely.
    std::fs::create_dir_all(h.join("Library/Application Support/Code/User")).unwrap();
    std::fs::create_dir_all(h.join(".cursor")).unwrap();

    let targets = vec![
        EditorTarget {
            name: "VS Code",
            agent_key: "vscode".into(),
            config_path: h.join("Library/Application Support/Code/User/mcp.json"),
            detect_path: h.join("Library/Application Support/Code"),
            config_type: ConfigType::VsCodeMcp,
        },
        EditorTarget {
            name: "Cursor",
            agent_key: "cursor".into(),
            config_path: h.join(".cursor/mcp.json"),
            detect_path: h.join(".cursor"),
            config_type: ConfigType::McpJson,
        },
        // A $HOME-level config file: its parent is $HOME and must be skipped.
        EditorTarget {
            name: "Claude Code",
            agent_key: "claude".into(),
            config_path: h.join(".claude.json"),
            detect_path: h.join(".no-such-dir"),
            config_type: ConfigType::McpJson,
        },
    ];

    let mut out = Vec::new();
    collect_ide_allow_dirs(h, &targets, &mut out);

    assert!(
        out.iter().any(|p| p.ends_with("Code/User")),
        "non-dotfile VS Code dir must be covered: {out:?}"
    );
    assert!(out.iter().any(|p| p.ends_with(".cursor")), "{out:?}");
    let home_canon = canonicalize_secure(h);
    assert!(
        !out.contains(&home_canon),
        "must never widen the jail to $HOME: {out:?}"
    );
}

// P0-10 (#422): foreign editor config dirs are opt-in. lean-ctx's own state
// dir is added by the caller via the data_dir root, NOT by `home_allow_dirs`,
// so the default home allow-list is empty and `~/.lean-ctx` (not an editor)
// never appears here.
#[test]
fn ide_config_dirs_are_excluded_by_default() {
    let home = tempfile::tempdir().unwrap();
    for d in [".lean-ctx", ".cursor", ".codex"] {
        std::fs::create_dir_all(home.path().join(d)).unwrap();
    }

    let denied = home_allow_dirs(home.path(), false);
    assert!(
        denied.is_empty(),
        "foreign editor dirs must stay jailed by default: {denied:?}"
    );

    // Opt-in exposes the editor dirs that actually exist under this home.
    // Entries are registry-derived (foreign real-$HOME paths are filtered out
    // by the in-home guard), so the result stays hermetic — and `~/.lean-ctx`
    // is never added here because it is not an editor.
    let allowed = home_allow_dirs(home.path(), true);
    assert!(
        allowed.iter().any(|p| p.ends_with(".cursor")),
        "opt-in must expose editor dirs: {allowed:?}"
    );
    assert!(
        !allowed.iter().any(|p| p.ends_with(".lean-ctx")),
        "lean-ctx's own dir is covered by the data_dir root, not home_allow_dirs: {allowed:?}"
    );
}

#[test]
fn canonicalize_or_self_strips_verbatim() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("project");
    std::fs::create_dir_all(&dir).unwrap();

    let result = canonicalize_or_self(&dir);
    let s = result.to_string_lossy();
    assert!(
        !s.starts_with(r"\\?\"),
        "canonicalize_or_self should strip verbatim prefix, got: {s}"
    );
}

#[test]
fn jail_path_accepts_same_dir_different_format() {
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("file.rs"), "ok").unwrap();

    let result = jail_path(&root.join("file.rs"), &root);
    assert!(result.is_ok(), "same dir should be accepted: {result:?}");
}

#[cfg(not(feature = "no-jail"))]
#[test]
fn error_message_contains_escape_info() {
    // isolated_data_dir holds the global test_env_lock, serializing this
    // against any parallel `LEAN_CTX_ALLOW_PATH="/"` mutation.
    let _iso = crate::core::data_dir::isolated_data_dir();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("b.txt"), "no").unwrap();

    let err = jail_path(&other.join("b.txt"), &root).unwrap_err();
    assert!(
        err.to_string().contains("path escapes project root"),
        "error should mention escape: {err}"
    );
}

// GH #887: over MCP (meta hints gated off) the rejection was bare, so
// agents shelled out to work around the jail instead of widening it via
// config. The agent-visible error must name the sanctioned config keys.
#[cfg(not(feature = "no-jail"))]
#[test]
fn escape_error_names_config_keys_without_meta() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::test_env::remove_var("LEAN_CTX_META");
    crate::test_env::remove_var("LEAN_CTX_DIAGNOSTICS");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("b.txt"), "no").unwrap();

    let err = jail_path(&other.join("b.txt"), &root).unwrap_err();
    let msg = err.to_string();
    // One command that resolves it without a restart — never "open a new
    // IDE window" or an env var the running server cannot see.
    assert!(
        msg.contains("lean-ctx allow-path") && msg.contains("no restart"),
        "agent-visible escape error should hint at resolution: {msg}"
    );
    assert!(!msg.contains("LEAN_CTX_EXTRA_ROOTS="), "{msg}");
}

// GH #392: config entries like "$HOME/code" or "~/code" were taken
// literally and never matched.
#[test]
fn expand_user_path_expands_tilde_and_vars() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    let home = dirs::home_dir().expect("home dir");
    let home_s = home.to_string_lossy().to_string();

    assert_eq!(expand_user_path("~"), home);
    assert_eq!(expand_user_path("~/code"), home.join("code"));
    assert_eq!(expand_user_path("$HOME/code"), home.join("code"));
    assert_eq!(expand_user_path("${HOME}/code"), home.join("code"));
    // Multiple variables in one entry.
    crate::test_env::set_var("LEAN_CTX_TEST_SUB", "sub");
    assert_eq!(
        expand_user_path("$HOME/$LEAN_CTX_TEST_SUB/x"),
        PathBuf::from(format!("{home_s}/sub/x"))
    );
    crate::test_env::remove_var("LEAN_CTX_TEST_SUB");
    // Absolute paths pass through untouched.
    assert_eq!(expand_user_path("/etc"), PathBuf::from("/etc"));
}

#[cfg(windows)]
#[test]
fn expand_user_path_home_falls_back_when_env_is_unset() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    let expected_home = dirs::home_dir().expect("home dir");
    let previous_home = std::env::var_os("HOME");

    struct RestoreHome(Option<std::ffi::OsString>);

    impl Drop for RestoreHome {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => crate::test_env::set_var("HOME", value),
                None => crate::test_env::remove_var("HOME"),
            }
        }
    }

    let _restore_home = RestoreHome(previous_home);
    crate::test_env::remove_var("HOME");

    assert_eq!(std::env::var_os("HOME"), None);
    assert_eq!(expand_user_path("$HOME/code"), expected_home.join("code"));
    assert_eq!(expand_user_path("${HOME}/code"), expected_home.join("code"));
}

#[test]
fn expand_user_path_leaves_unset_vars_verbatim() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_TEST_UNSET_VAR");
    let p = expand_user_path("$LEAN_CTX_TEST_UNSET_VAR/code");
    assert_eq!(p, PathBuf::from("$LEAN_CTX_TEST_UNSET_VAR/code"));
}

// GH #392: `allow_paths = ["/"]` (via the same env-var channel) must grant
// access to any absolute path — "/" is a prefix of everything.
//
// Env-mutating tests here hold the process-global
// `data_dir::test_env_lock()` (directly, or via `isolated_data_dir()`
// which wraps it) — NOT a module-local mutex. test_env's SAFETY contract
// says *all* test env mutation serializes through that one lock; a local
// lock only serializes this module against itself, so e.g.
// `artifacts::external_corpus_requires_allow_list` (which holds the
// global lock) could observe this test's `LEAN_CTX_ALLOW_PATH="/"` and
// fail its jail-rejection assert (the pre-existing parallel-run flake
// reported in #695).
#[cfg(unix)]
#[test]
fn allow_path_root_slash_permits_everything() {
    let _guard = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("b.txt"), "allowed").unwrap();

    crate::test_env::set_var("LEAN_CTX_ALLOW_PATH", "/");
    let result = jail_path(&other.join("b.txt"), &root);
    crate::test_env::remove_var("LEAN_CTX_ALLOW_PATH");

    assert!(result.is_ok(), "allow path '/' must permit all: {result:?}");
}

// Finding 3 (GH security audit): env-channel jail relaxations must be
// detectable so startup + doctor can surface them loudly.
#[test]
fn active_relaxations_detects_allow_path_env() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::test_env::remove_var("LEAN_CTX_EXTRA_ROOTS");
    crate::test_env::remove_var("LEAN_CTX_ALLOW_IDE_DIRS");
    crate::test_env::set_var("LEAN_CTX_ALLOW_PATH", "/tmp");

    let relaxed = active_relaxations();

    crate::test_env::remove_var("LEAN_CTX_ALLOW_PATH");

    assert!(
        relaxed.iter().any(|r| r.source == "LEAN_CTX_ALLOW_PATH"),
        "LEAN_CTX_ALLOW_PATH must be reported as a jail relaxation: {relaxed:?}"
    );
}

#[cfg(not(feature = "no-jail"))]
#[test]
fn active_relaxations_empty_when_jail_intact() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    for var in [
        "LEAN_CTX_ALLOW_PATH",
        "LCTX_ALLOW_PATH",
        "LEAN_CTX_EXTRA_ROOTS",
        "LEAN_CTX_ALLOW_IDE_DIRS",
    ] {
        crate::test_env::remove_var(var);
    }

    assert!(
        active_relaxations().is_empty(),
        "an intact jail (clean config, no relaxation env) must report no relaxations: {:?}",
        active_relaxations()
    );
}

#[test]
fn allow_path_env_permits_outside_root() {
    let _guard = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("b.txt"), "allowed").unwrap();

    let canon = canonicalize_or_self(&other);
    crate::test_env::set_var("LEAN_CTX_ALLOW_PATH", canon.to_string_lossy().as_ref());
    let result = jail_path(&other.join("b.txt"), &root);
    crate::test_env::remove_var("LEAN_CTX_ALLOW_PATH");

    assert!(
        result.is_ok(),
        "LEAN_CTX_ALLOW_PATH should permit access: {result:?}"
    );
}

#[cfg(all(unix, not(feature = "no-jail")))]
#[test]
fn rejects_symlink_escape_on_unix() {
    use std::os::unix::fs::symlink;

    // isolated_data_dir holds the global test_env_lock — no parallel test
    // can set `LEAN_CTX_ALLOW_PATH="/"` and let this escape resolve.
    let _iso = crate::core::data_dir::isolated_data_dir();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("secret.txt"), "no").unwrap();

    let link = root.join("link.txt");
    symlink(other.join("secret.txt"), &link).unwrap();

    let bad = jail_path(&link, &root);
    assert!(bad.is_err(), "symlink escape must be rejected: {bad:?}");
}

#[test]
fn rejects_null_byte_in_path() {
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();

    let bad_path = PathBuf::from("file\0.txt");
    let result = jail_path(&bad_path, &root);
    assert!(result.is_err(), "null byte in path must be rejected");
    assert!(
        result.unwrap_err().to_string().contains("null byte"),
        "error must mention null byte"
    );
}

/// #403 Bug 1: an explicit path under a session-scoped `extra_root` (e.g. a
/// sibling git worktree from MCP `roots/list`) must resolve, while the same
/// path is rejected without it — and a path under *no* root is rejected even
/// when extra roots are present. Holds both env locks so neither a parallel
/// `path_jail` flip nor a `LEAN_CTX_ALLOW_PATH` mutation can leak in.
#[cfg(not(feature = "no-jail"))]
#[test]
fn extra_roots_permit_paths_outside_jail() {
    let _iso = crate::core::data_dir::isolated_data_dir();

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("project");
    let worktree = tmp.path().join("worktree");
    let elsewhere = tmp.path().join("elsewhere");
    for d in [&root, &worktree, &elsewhere] {
        std::fs::create_dir_all(d).unwrap();
    }
    let in_worktree = worktree.join("a.txt");
    std::fs::write(&in_worktree, "x").unwrap();
    let outside = elsewhere.join("b.txt");
    std::fs::write(&outside, "y").unwrap();

    // Parity: with no extra roots, the worktree path escapes the jail.
    assert!(jail_path(&in_worktree, &root).is_err());
    assert!(jail_path_with_roots(&in_worktree, &root, &[]).is_err());

    // The session-scoped extra root permits it — via the slice alone, with
    // nothing in env/config.
    let extra = vec![worktree.to_string_lossy().to_string()];
    assert!(
        jail_path_with_roots(&in_worktree, &root, &extra).is_ok(),
        "path under a session extra_root must resolve (#403)"
    );

    // A path under neither the jail nor any extra root is still rejected.
    assert!(
        jail_path_with_roots(&outside, &root, &extra).is_err(),
        "paths outside ALL roots must still be rejected"
    );

    // Empty entries are ignored (no accidental allow-all).
    assert!(jail_path_with_roots(&outside, &root, &[String::new()]).is_err());
}

/// GH #1228: Claude Code auto-memory under `…/.claude/projects/<slug>/memory/`
/// must be readable/writable via ctx_* without a manual extra_roots edit.
#[cfg(not(feature = "no-jail"))]
#[test]
fn harness_auto_memory_path_is_allowed_without_extra_roots() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("project");
    let memory = tmp
        .path()
        .join(".claude")
        .join("projects")
        .join("-tmp-project")
        .join("memory");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&memory).unwrap();
    let mem_file = memory.join("MEMORY.md");
    std::fs::write(&mem_file, "# index\n").unwrap();

    assert!(is_harness_auto_memory_path(&mem_file));
    assert!(is_harness_auto_memory_path(&memory));
    assert!(!is_harness_auto_memory_path(
        &tmp.path()
            .join(".claude")
            .join("projects")
            .join("-tmp-project")
            .join("session.jsonl")
    ));

    assert!(
        jail_path_with_roots(&mem_file, &root, &[]).is_ok(),
        "auto-memory file must pass PathJail without extra_roots"
    );
}

/// #820: lean-ctx state dir (tee files) is implicitly allowed by the jail.
#[test]
fn state_dir_tee_files_pass_jail() {
    let _lock = crate::core::data_dir::test_env_lock();
    let state = crate::core::paths::state_dir().expect("state_dir must be available");
    let tee_path = state.join("tee").join("some_command_deadbeef.log");
    // Use a root that is clearly NOT the state dir's parent
    let fake_root = std::env::temp_dir().join("pathjail_test_820_root");
    std::fs::create_dir_all(&fake_root).ok();
    // The tee path is outside the fake root, but the state dir allowance
    // should make it pass (the state dir itself exists on disk).
    let result = jail_path_with_roots(&tee_path, &fake_root, &[]);
    // If state_dir exists on disk (it does in dev), the path should be allowed.
    // If the tee file itself doesn't exist, canonicalize_existing_ancestor
    // resolves to the state_dir (which does exist) + remainder.
    if state.exists() {
        assert!(
            result.is_ok(),
            "tee-file path under lean-ctx state dir must be auto-allowed: {result:?}"
        );
    }
    std::fs::remove_dir_all(&fake_root).ok();
}

#[test]
fn detected_cache_hint_recognizes_go_cargo_python() {
    use std::path::Path;
    let go = detected_cache_hint(Path::new("/Users/x/go/pkg/mod/github.com/foo/bar/main.go"));
    assert!(go.is_some(), "Go module cache should be detected");
    assert!(go.unwrap().contains("Go module cache"));

    let cargo = detected_cache_hint(Path::new(
        "/home/x/.cargo/registry/src/crates.io/serde-1.0/lib.rs",
    ));
    assert!(cargo.is_some(), "Rust cargo registry should be detected");
    assert!(cargo.unwrap().contains("Rust crate registry"));

    let py = detected_cache_hint(Path::new(
        "/usr/lib/python3.12/site-packages/requests/api.py",
    ));
    assert!(py.is_some(), "Python site-packages should be detected");

    let normal = detected_cache_hint(Path::new("/home/x/projects/myapp/src/main.rs"));
    assert!(normal.is_none(), "Normal project path should not match");
}

#[test]
fn detect_cache_root_extracts_marker_dir() {
    let cases = [
        (
            "/Users/x/go/pkg/mod/github.com/foo/bar@v1.2.3/baz.go",
            "Go module cache",
            "/Users/x/go/pkg/mod",
        ),
        (
            "/home/u/.cargo/registry/src/index-abc/serde-1.0/src/lib.rs",
            "Rust crate registry",
            "/home/u/.cargo/registry",
        ),
        (
            "/opt/venv/lib/python3.12/site-packages/requests/api.py",
            "Python site-packages",
            "/opt/venv/lib/python3.12/site-packages",
        ),
        (
            "/w/app/node_modules/react/index.js",
            "Node modules",
            "/w/app/node_modules",
        ),
        (
            "/Users/x/.rustup/toolchains/stable-aarch64-apple-darwin/lib/rustlib/src/rust/library/core/src/lib.rs",
            "Rust toolchain source",
            "/Users/x/.rustup/toolchains",
        ),
        (
            "/home/u/.cargo/git/checkouts/tokio-abc/1234567/src/lib.rs",
            "Cargo git dependencies",
            "/home/u/.cargo/git/checkouts",
        ),
        (
            "/home/u/.bun/install/cache/zod@3.23.8/index.d.ts",
            "Bun package cache",
            "/home/u/.bun/install/cache",
        ),
        (
            "/home/u/.pub-cache/hosted/pub.dev/http-1.2.0/lib/http.dart",
            "Dart pub cache",
            "/home/u/.pub-cache/hosted",
        ),
        (
            "/home/u/.gem/ruby/3.3.0/gems/rack-3.0.0/lib/rack.rb",
            "Ruby gems",
            "/home/u/.gem/ruby",
        ),
    ];
    for (path, want_label, want_root) in cases {
        let (label, root) = detect_language_cache_root(Path::new(path))
            .unwrap_or_else(|| panic!("expected cache match for {path}"));
        assert_eq!(label, want_label, "label for {path}");
        assert_eq!(root, PathBuf::from(want_root), "root for {path}");
    }
    assert!(
        detect_language_cache_root(Path::new("/home/u/proj/src/main.rs")).is_none(),
        "a normal project path is not a cache"
    );
    for private in [
        "/home/u/.cargo/credentials.toml",
        "/home/u/.claude/settings.json",
        "/home/u/.codex/auth.json",
        "/home/u/.config/gh/hosts.yml",
    ] {
        assert!(
            detect_language_cache_root(Path::new(private)).is_none(),
            "credential and config locations are never auto-admitted: {private}"
        );
    }
}

/// The core #899 guarantee: once a detected cache root is registered, a path
/// under it *reads* (jail resolves) but never *writes* (enforce_writable
/// denies), and registration is idempotent.
#[cfg(not(feature = "no-jail"))]
#[test]
fn registered_cache_root_reads_allow_writes_deny() {
    let _iso = crate::core::data_dir::isolated_data_dir();

    let tmp = tempfile::tempdir().unwrap();
    // A fake Go module cache so detect_language_cache_root matches the path.
    let dep = tmp.path().join("go/pkg/mod/example.com/lib@v1");
    std::fs::create_dir_all(&dep).unwrap();
    let file = dep.join("lib.go");
    std::fs::write(&file, "package lib").unwrap();

    // A project jail that does NOT contain the cache.
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    // Before registration: the read escapes the jail.
    assert!(jail_path_with_roots(&file, &project, &[]).is_err());

    // Register the detected root; the second call is a no-op.
    let (_, root) = detect_language_cache_root(&file).expect("cache match");
    assert!(
        register_session_read_only_root(&root),
        "first register is new"
    );
    assert!(
        !register_session_read_only_root(&root),
        "re-register is a no-op"
    );

    // After: the read resolves, but writes are denied (read-only tier).
    assert!(
        jail_path_with_roots(&file, &project, &[]).is_ok(),
        "registered cache root must be readable"
    );
    assert!(is_read_only_path(&file), "cache file is read-only");
    assert!(
        enforce_writable(&file).is_err(),
        "writes into the cache root must be denied"
    );
}
