//! End-to-end coverage for `shell_hook::install_all_with_style`.
//!
//! The unit tests inside `shell_hook.rs` exercise each per-shell install
//! function with an explicit `home: &Path` argument, which keeps them race-
//! free. The tests in this file cover the top-level `install_all_with_style`
//! entry point, which resolves `$HOME` via `dirs::home_dir()`. Because that
//! reads the live env, these tests must run single-threaded — CI already
//! invokes `cargo test --all-features -- --test-threads=1`, and a
//! repo-local mutex below covers the case where someone runs tests
//! without that flag.
//!
//! The intent is to validate the cross-file behaviour:
//!   - All four touchpoints (.zshenv, .bashenv, .zshrc, .bashrc) end up in
//!     the right style for a given home layout.
//!   - Mixed layouts (e.g. dropin for .zshenv but inline for .bashrc) work.
//!   - Uninstall removes everything regardless of which style was used.

use std::path::Path;
use std::sync::Mutex;

use lean_ctx::shell_hook::{install_all, install_all_with_style, uninstall_all, Style};

/// Serialises tests in this file so concurrent `$HOME` mutation doesn't
/// race. Cargo runs each integration test binary's tests in parallel by
/// default; this guard plus CI's `--test-threads=1` keeps us correct in
/// both modes.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn with_home<F: FnOnce(&Path)>(f: F) {
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev = std::env::var_os("HOME");
    // SAFETY: serialised via HOME_LOCK.
    unsafe { std::env::set_var("HOME", tmp.path()) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(tmp.path())));
    match prev {
        Some(v) => unsafe { std::env::set_var("HOME", v) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    if let Err(p) = result {
        std::panic::resume_unwind(p);
    }
}

const MARKER_START: &str = "# >>> lean-ctx shell hook >>>";
const MARKER_END: &str = "# <<< lean-ctx shell hook <<<";
const ALIAS_START: &str = "# >>> lean-ctx agent aliases >>>";
const ALIAS_END: &str = "# <<< lean-ctx agent aliases <<<";

fn touch_rc(home: &Path, name: &str) {
    std::fs::write(home.join(name), "# placeholder rc\n").unwrap();
}

fn enable_dropin(home: &Path, rc: &str, dir: &str) {
    std::fs::create_dir_all(home.join(dir)).unwrap();
    std::fs::write(
        home.join(rc),
        format!("for f in $HOME/{dir}/*.zsh; do source $f; done\n"),
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// install_all entry point (default = Auto)
// ---------------------------------------------------------------------------

#[test]
fn install_all_default_is_auto_inline_on_plain_layout() {
    with_home(|home| {
        touch_rc(home, ".zshrc");
        touch_rc(home, ".bashrc");

        install_all(true);

        let zshenv = std::fs::read_to_string(home.join(".zshenv")).unwrap();
        assert!(zshenv.contains(MARKER_START));
        let zshrc = std::fs::read_to_string(home.join(".zshrc")).unwrap();
        assert!(zshrc.contains(ALIAS_START));
    });
}

#[test]
fn install_all_auto_writes_dropin_for_zshenv_when_loop_present() {
    with_home(|home| {
        enable_dropin(home, ".zshenv", ".zshenv.d");
        touch_rc(home, ".zshrc");

        install_all_with_style(true, Style::Auto);

        let dropin = home.join(".zshenv.d").join("00-lean-ctx.zsh");
        assert!(dropin.exists(), "expected drop-in for .zshenv");

        let zshenv = std::fs::read_to_string(home.join(".zshenv")).unwrap();
        assert!(
            !zshenv.contains(MARKER_START),
            "drop-in install must not also leave the inline fenced block"
        );
    });
}

// ---------------------------------------------------------------------------
// Mixed layout: dropin for env, inline for rc
// ---------------------------------------------------------------------------

#[test]
fn auto_resolves_each_slot_independently() {
    with_home(|home| {
        // zsh env uses .d/ drop-ins (chezmoi style)…
        enable_dropin(home, ".zshenv", ".zshenv.d");
        // …but zshrc does NOT — user hand-edits it.
        std::fs::write(home.join(".zshrc"), "# my plain zshrc\n").unwrap();

        install_all_with_style(true, Style::Auto);

        assert!(
            home.join(".zshenv.d").join("00-lean-ctx.zsh").exists(),
            ".zshenv hook should be a drop-in"
        );
        let zshrc = std::fs::read_to_string(home.join(".zshrc")).unwrap();
        assert!(
            zshrc.contains(ALIAS_START),
            ".zshrc aliases should be inline"
        );
        assert!(
            !home.join(".zshrc.d").exists(),
            "no .zshrc.d should be created when not pre-configured"
        );
    });
}

// ---------------------------------------------------------------------------
// Migration coverage
// ---------------------------------------------------------------------------

#[test]
fn re_running_after_chezmoi_adoption_migrates_to_dropin() {
    with_home(|home| {
        // Phase 1: user installs lean-ctx the old way — inline blocks
        // everywhere.
        touch_rc(home, ".zshrc");
        touch_rc(home, ".bashrc");
        install_all_with_style(true, Style::Inline);
        let inline_zshenv = std::fs::read_to_string(home.join(".zshenv")).unwrap();
        assert!(inline_zshenv.contains(MARKER_START));

        // Phase 2: user adopts a dotfiles tool that introduces .zshenv.d/
        // and changes .zshenv to source it.
        std::fs::create_dir_all(home.join(".zshenv.d")).unwrap();
        let migrated_zshenv = format!(
            "{}\n\nfor f in $HOME/.zshenv.d/*.zsh; do source $f; done\n",
            inline_zshenv.trim_end()
        );
        std::fs::write(home.join(".zshenv"), migrated_zshenv).unwrap();

        // Phase 3: lean-ctx update / re-run picks up the new layout.
        install_all_with_style(true, Style::Auto);

        // The fenced block must be gone from .zshenv …
        let final_zshenv = std::fs::read_to_string(home.join(".zshenv")).unwrap();
        assert!(
            !final_zshenv.contains(MARKER_START),
            "migration should strip the legacy fenced block from .zshenv"
        );
        // …and the drop-in file should now hold the hook.
        let dropin_body =
            std::fs::read_to_string(home.join(".zshenv.d").join("00-lean-ctx.zsh")).unwrap();
        assert!(dropin_body.contains("ZSH_EXECUTION_STRING"));
    });
}

#[test]
fn rolling_back_to_inline_removes_dropin_file() {
    with_home(|home| {
        // Start in drop-in mode.
        enable_dropin(home, ".zshenv", ".zshenv.d");
        install_all_with_style(true, Style::Auto);
        assert!(home.join(".zshenv.d").join("00-lean-ctx.zsh").exists());

        // User decides to force inline.
        install_all_with_style(true, Style::Inline);

        assert!(
            !home.join(".zshenv.d").join("00-lean-ctx.zsh").exists(),
            "switching back to Inline must remove the drop-in"
        );
        let zshenv = std::fs::read_to_string(home.join(".zshenv")).unwrap();
        assert!(zshenv.contains(MARKER_START));
    });
}

// ---------------------------------------------------------------------------
// Uninstall removes everything regardless of style
// ---------------------------------------------------------------------------

#[test]
fn uninstall_removes_inline_artifacts() {
    with_home(|home| {
        touch_rc(home, ".zshrc");
        touch_rc(home, ".bashrc");
        install_all_with_style(true, Style::Inline);

        uninstall_all(true);

        let zshenv = std::fs::read_to_string(home.join(".zshenv")).unwrap_or_default();
        assert!(!zshenv.contains(MARKER_START));
        let zshrc = std::fs::read_to_string(home.join(".zshrc")).unwrap();
        assert!(!zshrc.contains(ALIAS_START));
    });
}

#[test]
fn uninstall_removes_dropin_artifacts() {
    with_home(|home| {
        enable_dropin(home, ".zshenv", ".zshenv.d");
        enable_dropin(home, ".zshrc", ".zshrc.d");
        install_all_with_style(true, Style::Auto);

        assert!(home.join(".zshenv.d").join("00-lean-ctx.zsh").exists());
        assert!(home.join(".zshrc.d").join("00-lean-ctx.zsh").exists());

        uninstall_all(true);

        assert!(!home.join(".zshenv.d").join("00-lean-ctx.zsh").exists());
        assert!(!home.join(".zshrc.d").join("00-lean-ctx.zsh").exists());
    });
}

#[test]
fn uninstall_removes_both_styles_if_both_present() {
    with_home(|home| {
        // Pathological state: somehow both inline and drop-in are present.
        // Uninstall should clean both.
        enable_dropin(home, ".zshenv", ".zshenv.d");
        // First install: drop-in.
        install_all_with_style(true, Style::Auto);
        // Then forcibly add an inline block too (simulating a corrupt
        // mid-migration state).
        let mut zshenv = std::fs::read_to_string(home.join(".zshenv")).unwrap();
        zshenv
            .push_str("\n# >>> lean-ctx shell hook >>>\n# stray\n# <<< lean-ctx shell hook <<<\n");
        std::fs::write(home.join(".zshenv"), zshenv).unwrap();

        uninstall_all(true);

        assert!(!home.join(".zshenv.d").join("00-lean-ctx.zsh").exists());
        let zshenv = std::fs::read_to_string(home.join(".zshenv")).unwrap();
        assert!(!zshenv.contains(MARKER_START));
    });
}

#[test]
fn uninstall_is_noop_when_nothing_installed() {
    with_home(|home| {
        // Empty home — no files. Should not panic, should not create anything.
        uninstall_all(true);
        assert!(std::fs::read_dir(home).unwrap().next().is_none());
    });
}

// ---------------------------------------------------------------------------
// Hand-edit preservation across migration
// ---------------------------------------------------------------------------

/// Returns sibling files of `path` matching
/// `<basename>.lean-ctx-<ts>.bak`.
fn migration_backups_for(path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    let prefix = format!("{name}.lean-ctx-");
    let mut out: Vec<_> = std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with(&prefix)
                    && std::path::Path::new(n)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("bak"))
            })
        })
        .collect();
    out.sort();
    out
}

#[test]
fn migration_through_install_all_writes_backup_for_each_migrated_slot() {
    with_home(|home| {
        // Existing inline install: .zshenv has the fenced hook, .zshrc has
        // the fenced aliases. The user has slipped a custom line into the
        // .zshenv hook block.
        let zshenv_pre = format!(
            "# top of .zshenv\n\n\
             {MARKER_START}\n\
             # CUSTOM: capture pid before lean-ctx execs\n\
             echo \"$$\" > /tmp/last-shell-pid\n\
             {MARKER_END}\n",
        );
        std::fs::write(home.join(".zshenv"), &zshenv_pre).unwrap();
        std::fs::write(
            home.join(".zshrc"),
            format!("# top\n{ALIAS_START}\nalias k=kubectl\n{ALIAS_END}\n"),
        )
        .unwrap();

        // User then adopts the .d/ convention for .zshenv.
        std::fs::create_dir_all(home.join(".zshenv.d")).unwrap();
        let zshenv_with_loop = format!(
            "{}\nfor f in $HOME/.zshenv.d/*.zsh; do source $f; done\n",
            zshenv_pre.trim_end()
        );
        std::fs::write(home.join(".zshenv"), &zshenv_with_loop).unwrap();

        install_all_with_style(true, Style::Auto);

        // .zshenv migrated -> exactly one timestamped backup.
        let zshenv_baks = migration_backups_for(&home.join(".zshenv"));
        assert_eq!(zshenv_baks.len(), 1, "expected one .zshenv backup");
        let bak_body = std::fs::read_to_string(&zshenv_baks[0]).unwrap();
        assert!(bak_body.contains("CUSTOM: capture pid"));
        assert!(bak_body.contains("last-shell-pid"));

        // .zshrc didn't migrate (no .zshrc.d source loop) -> no backup noise.
        assert!(
            migration_backups_for(&home.join(".zshrc")).is_empty(),
            "no backup expected for slot that didn't migrate"
        );
    });
}

#[test]
fn no_backups_on_clean_install_through_install_all() {
    with_home(|home| {
        // Fresh user, nothing installed. install_all should not produce
        // any .bak files anywhere.
        std::fs::create_dir_all(home.join(".zshenv.d")).unwrap();
        std::fs::write(
            home.join(".zshenv"),
            "for f in $HOME/.zshenv.d/*.zsh; do source $f; done\n",
        )
        .unwrap();
        touch_rc(home, ".zshrc");

        install_all_with_style(true, Style::Auto);

        let mut baks: Vec<_> = walkdir(home)
            .filter(|p| {
                let s = p.to_string_lossy();
                s.contains(".lean-ctx-")
                    && std::path::Path::new(&*s)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("bak"))
            })
            .collect();
        baks.sort();
        assert!(
            baks.is_empty(),
            "clean install must not create any .bak files; found: {baks:?}"
        );
    });
}

fn walkdir(root: &Path) -> impl Iterator<Item = std::path::PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    std::iter::from_fn(move || {
        while let Some(dir) = stack.pop() {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else {
                        return Some(p);
                    }
                }
            }
        }
        None
    })
}

#[test]
fn double_install_then_uninstall_leaves_no_trace() {
    with_home(|home| {
        enable_dropin(home, ".zshenv", ".zshenv.d");
        touch_rc(home, ".zshrc");
        touch_rc(home, ".bashrc");

        install_all_with_style(true, Style::Auto);
        install_all_with_style(true, Style::Auto);
        uninstall_all(true);

        // .zshenv still exists (we only manage our own block); but no
        // lean-ctx artifacts should remain.
        let zshenv = std::fs::read_to_string(home.join(".zshenv")).unwrap();
        assert!(!zshenv.contains(MARKER_START));
        assert!(!home.join(".zshenv.d").join("00-lean-ctx.zsh").exists());

        let zshrc = std::fs::read_to_string(home.join(".zshrc")).unwrap();
        assert!(!zshrc.contains(ALIAS_START));
    });
}
