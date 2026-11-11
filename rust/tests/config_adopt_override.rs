// SPDX-License-Identifier: Apache-2.0

//! Regression guard: an explicit `LEAN_CTX_*_DIR` override must never pull the
//! real install's config out of its default config dir.
//!
//! The canary case: a process with `LEAN_CTX_CONFIG_DIR` pointing at an empty
//! dir resolved `config_dir_member("config.toml")` (the context kernel does so
//! on every `ctx_search`), and legacy adoption *renamed*
//! `dirs::config_dir()/lean-ctx/config.toml` (`~/.config/lean-ctx` on Linux)
//! into the override. When the override was a temp dir, the user's config was
//! gone.
//!
//! Portable across Unix: the adoption source is `dirs::config_dir()/lean-ctx`
//! (`$XDG_CONFIG_HOME/lean-ctx` on Linux, `~/Library/Application Support/lean-ctx`
//! on macOS), while the canonical/mixed config dir is `$XDG_CONFIG_HOME/lean-ctx`
//! on every OS. The sandbox derives each from the private HOME it sets, so the
//! cases test the same paths the library resolves on either OS.
//!
//! This runs as an INTEGRATION test on purpose: the library is then compiled
//! WITHOUT `#[cfg(test)]`, so the real (non-sandboxed) adoption path executes
//! exactly as in production. Every test isolates HOME, the XDG bases and all
//! four `LEAN_CTX_*_DIR` variables itself.

#![cfg(unix)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

const KEYS: [&str; 9] = [
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "LEAN_CTX_DATA_DIR",
    "LEAN_CTX_CONFIG_DIR",
    "LEAN_CTX_STATE_DIR",
    "LEAN_CTX_CACHE_DIR",
];

const USER_CONFIG: &str = "# the user's only config\n[proxy]\nenabled = true\n";

/// A private HOME + XDG layout; restores the previous environment on drop.
struct Sandbox {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    xdg_config: PathBuf,
    root: PathBuf,
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl Sandbox {
    /// Caller holds `test_env_lock()`. `overrides` sets `LEAN_CTX_*_DIR`
    /// variables (relative to the sandbox root); all others are removed.
    fn new(overrides: &[(&str, &str)]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let home = root.join("home");
        let xdg_config = root.join("xc");
        std::fs::create_dir_all(&home).unwrap();
        let saved = KEYS.iter().map(|k| (*k, std::env::var_os(k))).collect();
        let set = |k: &str, v: &Path| {
            // SAFETY: serialized by `test_env_lock()`.
            unsafe { std::env::set_var(k, v) };
        };
        set("HOME", &home);
        set("XDG_CONFIG_HOME", &xdg_config);
        set("XDG_DATA_HOME", &root.join("xd"));
        set("XDG_STATE_HOME", &root.join("xs"));
        set("XDG_CACHE_HOME", &root.join("xk"));
        for k in &KEYS[5..] {
            // SAFETY: serialized by `test_env_lock()`.
            unsafe { std::env::remove_var(k) };
        }
        for (k, rel) in overrides {
            let dir = root.join(rel);
            std::fs::create_dir_all(&dir).unwrap();
            set(k, &dir);
        }
        Self {
            _tmp: tmp,
            home,
            xdg_config,
            root,
            saved,
        }
    }

    /// The OS-native legacy config dir that adoption reads from
    /// (`dirs::config_dir()/lean-ctx`, resolved under the sandbox HOME).
    fn default_config_dir(&self) -> PathBuf {
        let base = dirs::config_dir().expect("dirs::config_dir under the sandbox HOME");
        assert!(
            base.starts_with(&self.root),
            "dirs::config_dir() {} escapes the sandbox {}",
            base.display(),
            self.root.display()
        );
        base.join("lean-ctx")
    }

    /// The XDG config dir (`$XDG_CONFIG_HOME/lean-ctx`) — the canonical config
    /// dir on every OS, and the mixed-install dir when it holds data.
    fn xdg_config_dir(&self) -> PathBuf {
        self.xdg_config.join("lean-ctx")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                // SAFETY: serialized by `test_env_lock()`.
                Some(val) => unsafe { std::env::set_var(k, val) },
                // SAFETY: serialized by `test_env_lock()`.
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn is_empty_dir(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut it| it.next().is_none())
}

#[test]
fn config_dir_member_never_moves_the_user_config_into_a_config_override() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let sb = Sandbox::new(&[("LEAN_CTX_CONFIG_DIR", "override-config")]);
    let original = sb.default_config_dir().join("config.toml");
    write(&original, USER_CONFIG);

    let member = lean_ctx::core::paths::config_dir_member("config.toml");
    let personas = lean_ctx::core::paths::config_dir_member("personas");

    let override_dir = sb.root.join("override-config");
    assert_eq!(member.unwrap(), override_dir.join("config.toml"));
    assert_eq!(personas.unwrap(), override_dir.join("personas"));
    assert_eq!(
        std::fs::read_to_string(&original).unwrap(),
        USER_CONFIG,
        "the user's config must stay unchanged in place"
    );
    assert!(
        is_empty_dir(&override_dir),
        "nothing may be adopted into the override"
    );
}

#[test]
fn config_dir_member_never_adopts_under_any_dir_override() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    // The canary shape: all four overrides on one empty temp dir (as an
    // in-process harness sets them), with a real-looking HOME.
    let sb = Sandbox::new(&[
        ("LEAN_CTX_DATA_DIR", "iso"),
        ("LEAN_CTX_CONFIG_DIR", "iso"),
        ("LEAN_CTX_STATE_DIR", "iso"),
        ("LEAN_CTX_CACHE_DIR", "iso"),
    ]);
    let original = sb.default_config_dir().join("config.toml");
    write(&original, USER_CONFIG);

    let member = lean_ctx::core::paths::config_dir_member("config.toml").unwrap();

    assert_eq!(member, sb.root.join("iso/config.toml"));
    assert_eq!(std::fs::read_to_string(&original).unwrap(), USER_CONFIG);
    assert!(!member.exists(), "nothing may be adopted into the override");
}

#[test]
fn split_and_consolidation_never_feed_a_dir_override() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let sb = Sandbox::new(&[("LEAN_CTX_CONFIG_DIR", "override-config")]);
    // A legacy single-dir install that a split would otherwise migrate,
    // config included, into the (overridden) config target.
    let legacy = sb.home.join(".lean-ctx");
    write(&legacy.join("config.toml"), USER_CONFIG);
    write(&legacy.join("stats.json"), "{}");

    assert!(lean_ctx::core::xdg_migrate::migrate().is_none());
    assert!(lean_ctx::core::data_consolidate::consolidate().is_none());

    assert_eq!(
        std::fs::read_to_string(legacy.join("config.toml")).unwrap(),
        USER_CONFIG
    );
    assert!(legacy.join("stats.json").exists());
    assert!(is_empty_dir(&sb.root.join("override-config")));
}

fn dir_names(dir: &Path) -> Vec<OsString> {
    std::fs::read_dir(dir)
        .map(|rd| rd.map(|e| e.unwrap().file_name()).collect())
        .unwrap_or_default()
}

/// Startup heal (`layout_pin::heal` → `config_heal::heal_all` →
/// `config_heal::heal`) under a `LEAN_CTX_CONFIG_DIR` override on a legacy
/// `~/.lean-ctx` install: the data dir is still the real one.
#[test]
fn startup_heal_never_moves_legacy_config_into_config_override() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let sb = Sandbox::new(&[("LEAN_CTX_CONFIG_DIR", "override-config")]);
    let legacy = sb.home.join(".lean-ctx");
    write(&legacy.join("stats.json"), "{}");
    write(&legacy.join("config.toml"), USER_CONFIG);

    lean_ctx::core::layout_pin::heal();

    let left = std::fs::read_to_string(legacy.join("config.toml"));
    let ovr = dir_names(&sb.root.join("override-config"));
    assert!(
        left.as_deref().ok() == Some(USER_CONFIG),
        "legacy config gone: {left:?}; override now holds {ovr:?}"
    );
    assert!(ovr.is_empty(), "override holds {ovr:?}");
    assert!(lean_ctx::core::config_heal::pending().is_none());
}

/// Same, mixed install (`$XDG_CONFIG_HOME/lean-ctx` holds data).
#[test]
fn startup_heal_never_moves_mixed_config_into_config_override() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let sb = Sandbox::new(&[("LEAN_CTX_CONFIG_DIR", "override-config")]);
    let mixed = sb.xdg_config_dir();
    write(&mixed.join("stats.json"), "{}");
    write(&mixed.join("config.toml"), USER_CONFIG);

    lean_ctx::core::layout_pin::heal();

    let left = std::fs::read_to_string(mixed.join("config.toml"));
    let ovr = dir_names(&sb.root.join("override-config"));
    assert!(
        left.as_deref().ok() == Some(USER_CONFIG),
        "mixed config gone: {left:?}; override now holds {ovr:?}"
    );
    assert!(ovr.is_empty(), "override holds {ovr:?}");
}

/// An adopted and then edited member must stay the active one after the
/// `doctor --fix` split; the retired legacy original must not win again.
#[test]
fn adopted_then_edited_member_survives_the_split() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let sb = Sandbox::new(&[]);
    let legacy_install = sb.home.join(".lean-ctx");
    write(&legacy_install.join("stats.json"), "{}");
    write(
        &sb.default_config_dir().join("providers.toml"),
        "id = \"old\"\n",
    );
    let member = lean_ctx::core::paths::config_dir_member("providers.toml").unwrap();
    assert_eq!(member, legacy_install.join("providers.toml"));
    std::fs::write(&member, "id = \"edited\"\n").unwrap();

    let _ = lean_ctx::core::xdg_migrate::migrate();

    let after = lean_ctx::core::paths::config_dir_member("providers.toml").unwrap();
    let body = std::fs::read_to_string(&after).unwrap();
    let siblings = dir_names(after.parent().unwrap());
    assert_eq!(
        body, "id = \"edited\"\n",
        "active {after:?} reverted; dir holds {siblings:?}"
    );
}

#[test]
fn legacy_adoption_without_override_copies_and_retires_the_original() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let sb = Sandbox::new(&[]);
    // A legacy single-dir install: config resolves to `~/.lean-ctx`, while an
    // older build left `providers.toml` under `dirs::config_dir()/lean-ctx`.
    let legacy_install = sb.home.join(".lean-ctx");
    write(&legacy_install.join("stats.json"), "{}");
    let original = sb.default_config_dir().join("providers.toml");
    write(&original, "id = \"x\"\n");

    let member = lean_ctx::core::paths::config_dir_member("providers.toml").unwrap();

    assert_eq!(member, legacy_install.join("providers.toml"));
    assert_eq!(
        std::fs::read_to_string(&member).unwrap(),
        "id = \"x\"\n",
        "the legacy member is adopted into the canonical dir"
    );
    assert!(!original.exists(), "no live stale duplicate stays behind");
    assert_eq!(
        std::fs::read_to_string(sb.default_config_dir().join("providers.toml.adopted")).unwrap(),
        "id = \"x\"\n",
        "adoption keeps the original as <name>.adopted"
    );
}
