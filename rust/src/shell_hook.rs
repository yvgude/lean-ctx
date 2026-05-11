use std::path::{Path, PathBuf};

use crate::{dropin, marked_block};

const MARKER_START: &str = "# >>> lean-ctx shell hook >>>";
const MARKER_END: &str = "# <<< lean-ctx shell hook <<<";
const ALIAS_START: &str = "# >>> lean-ctx agent aliases >>>";
const ALIAS_END: &str = "# <<< lean-ctx agent aliases <<<";

/// File name we use inside `.d/` directories. Stable so install / migration /
/// uninstall can find it again without parsing. `00-` prefix sorts it ahead
/// of other drop-ins so the agent intercept fires before any tool init.
const DROPIN_ZSH: &str = "00-lean-ctx.zsh";
const DROPIN_SH: &str = "00-lean-ctx.sh";

const KNOWN_AGENT_ENV_VARS: &[&str] = &[
    "LEAN_CTX_AGENT",
    "CLAUDECODE",
    "CODEX_CLI_SESSION",
    "GEMINI_SESSION",
];

const AGENT_ALIASES: &[(&str, &str)] = &[
    ("claude", "claude"),
    ("codex", "codex"),
    ("gemini", "gemini"),
];

/// Installation style for the shell hook + agent aliases.
///
/// `Auto` (default) inspects each rc file to decide: if the file references
/// an adjacent `.d/` directory from a non-comment line and that directory
/// exists, install as a drop-in; otherwise fall back to an inline fenced
/// block in the rc file itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Style {
    /// Force inline marked-block install in the parent rc file.
    Inline,
    /// Force drop-in file install in the adjacent `.d/` directory.
    /// Falls back to `Inline` if no `.d/` source loop is configured.
    DropIn,
    /// Auto-detect per file.
    #[default]
    Auto,
}

/// Static description of a single install slot: which rc file, which
/// adjacent drop-in directory + filename, and the marker pair for the
/// inline form.
#[derive(Debug, Clone, Copy)]
struct Slot {
    rc_file: &'static str,
    dropin_dir: &'static str,
    dropin_file: &'static str,
    marker_start: &'static str,
    marker_end: &'static str,
}

const SLOT_ZSHENV: Slot = Slot {
    rc_file: ".zshenv",
    dropin_dir: ".zshenv.d",
    dropin_file: DROPIN_ZSH,
    marker_start: MARKER_START,
    marker_end: MARKER_END,
};

const SLOT_BASHENV: Slot = Slot {
    rc_file: ".bashenv",
    dropin_dir: ".bashenv.d",
    dropin_file: DROPIN_SH,
    marker_start: MARKER_START,
    marker_end: MARKER_END,
};

const SLOT_ZSHRC: Slot = Slot {
    rc_file: ".zshrc",
    dropin_dir: ".zshrc.d",
    dropin_file: DROPIN_ZSH,
    marker_start: ALIAS_START,
    marker_end: ALIAS_END,
};

const SLOT_BASHRC: Slot = Slot {
    rc_file: ".bashrc",
    dropin_dir: ".bashrc.d",
    dropin_file: DROPIN_SH,
    marker_start: ALIAS_START,
    marker_end: ALIAS_END,
};

/// Resolved destination for a single install slot.
enum InstallTarget {
    Marked {
        path: PathBuf,
        start: &'static str,
        end: &'static str,
    },
    DropIn {
        dir: PathBuf,
        filename: &'static str,
    },
}

impl InstallTarget {
    fn upsert(&self, content: &str, quiet: bool, label: &str) {
        match self {
            Self::Marked { path, start, end } => {
                marked_block::upsert(path, start, end, content, quiet, label);
            }
            Self::DropIn { dir, filename } => dropin::write(dir, filename, content, quiet, label),
        }
    }
}

/// Decide where a particular hook should live.
fn pick_target(home: &Path, slot: &Slot, style: Style) -> InstallTarget {
    let inline = InstallTarget::Marked {
        path: home.join(slot.rc_file),
        start: slot.marker_start,
        end: slot.marker_end,
    };
    match style {
        Style::Inline => inline,
        // DropIn and Auto both prefer dropin when available; only difference
        // is whether we fall back silently (Auto) or could be made to warn
        // (DropIn). Today they behave identically; the distinction lets
        // callers express intent in the CLI surface later.
        Style::DropIn | Style::Auto => match dropin::detect(home, slot.rc_file, slot.dropin_dir) {
            Some(dir) => InstallTarget::DropIn {
                dir,
                filename: slot.dropin_file,
            },
            None => inline,
        },
    }
}

/// Pre-formatted timestamp suffix for migration backups.
///
/// Created **once per install run** and threaded through every per-slot
/// install function, so all backups produced by a single
/// `install_all_with_style` invocation share the same suffix. This
/// rules out the "two near-simultaneous `Utc::now()` calls drifted by
/// 1 ms across a second boundary" bug class, and makes the backups
/// produced by one logical migration trivially groupable for the user
/// (e.g. `ls ~ | grep lean-ctx-20260511T203845Z`).
///
/// Tests construct one via `BackupStamp::at(...)` to get deterministic
/// filenames without touching the system clock.
struct BackupStamp(String);

impl BackupStamp {
    /// Capture the current UTC time. Call this **once** at the top of
    /// an install run.
    fn now() -> Self {
        Self::at(chrono::Utc::now())
    }

    /// Inject a specific moment in time. Used by tests; can also be
    /// used in future to align migration backups with a user-supplied
    /// release marker.
    fn at(stamp: chrono::DateTime<chrono::Utc>) -> Self {
        Self(stamp.format("%Y%m%dT%H%M%SZ").to_string())
    }

    /// Compose the full backup path for a given original file.
    fn backup_path_for(&self, path: &Path) -> Option<PathBuf> {
        let file_name = path.file_name().and_then(|n| n.to_str())?;
        Some(path.with_file_name(format!("{file_name}.lean-ctx-{}.bak", self.0)))
    }
}

/// Save a *timestamped* sibling backup of `path` before a destructive
/// migration step. Filename pattern: `<basename>.lean-ctx-<UTC>.bak`,
/// e.g. `.zshenv.lean-ctx-20260511T203845Z.bak`.
///
/// The block content owned by lean-ctx is normally treated as ours to
/// rewrite — `marked_block::upsert` already strips and replaces it on
/// every reinstall. That convention is acceptable for *idempotent
/// reinstalls* (the canonical content is always the same) but loses
/// information during a *style migration* if the user has hand-edited
/// anywhere in the file, including inside our fenced region.
///
/// Deliberate divergence from the elsewhere-in-the-codebase convention
/// (`cli::shell_init::backup_shell_config`, `config_io.rs`), which
/// writes a single `<file>.lean-ctx.bak` and clobbers it on every
/// invocation. That single-generation scheme is fine for "I backed
/// this up moments ago before this exact reinstall" use cases, but
/// risky for migration backups: a second migration event would
/// silently overwrite the first, destroying potentially-unrecoverable
/// user state. Timestamped names are append-only and let us migrate
/// repeatedly (e.g. across multiple `lean-ctx update` runs over
/// months) without ever losing a snapshot.
fn save_migration_backup(path: &Path, quiet: bool, stamp: &BackupStamp) {
    if !path.exists() {
        return;
    }
    let Some(bak) = stamp.backup_path_for(path) else {
        return;
    };
    match std::fs::copy(path, &bak) {
        Ok(_) => {
            if !quiet {
                eprintln!("  Backup: {} -> {}", path.display(), bak.display());
            }
        }
        Err(e) => {
            tracing::warn!("Failed to back up {}: {e}", path.display());
        }
    }
}

/// When we install one style, sweep away any prior install of the *other*
/// style so users transparently migrate (and so re-running setup never
/// leaves the hook in two places).
///
/// Whenever a migration would clobber pre-existing user content (a
/// fenced block in the rc file, or a hand-tweaked drop-in file), the
/// affected file is copied to `<filename>.lean-ctx-<stamp>.bak` first
/// (see `save_migration_backup`). The backup is only created when there
/// is something to migrate AWAY from, so clean installs and idempotent
/// reinstalls don't generate noise. `stamp` is taken by reference so
/// all migrations within one `install_all` invocation share the same
/// suffix.
fn strip_other_style(
    home: &Path,
    slot: &Slot,
    target: &InstallTarget,
    quiet: bool,
    label: &str,
    stamp: &BackupStamp,
) {
    match target {
        InstallTarget::Marked { .. } => {
            // Installing inline: remove any drop-in file we previously wrote.
            let dropin_dir = home.join(slot.dropin_dir);
            let dropin_path = dropin_dir.join(slot.dropin_file);
            if dropin_path.exists() {
                // Hand-edits to the drop-in file would otherwise be lost.
                // The backup lands next to the original; the `.bak`
                // suffix keeps it out of any `*.zsh` source glob.
                save_migration_backup(&dropin_path, quiet, stamp);
                dropin::remove(&dropin_dir, slot.dropin_file, quiet, label);
            }
        }
        InstallTarget::DropIn { .. } => {
            // Installing drop-in: remove any prior inline fenced block.
            // Back up the whole rc file first so anything between the
            // markers (and any unrelated user edits to the same file)
            // is recoverable from `<rc>.lean-ctx-<stamp>.bak`.
            let rc_path = home.join(slot.rc_file);
            if let Ok(existing) = std::fs::read_to_string(&rc_path) {
                if existing.contains(slot.marker_start) {
                    save_migration_backup(&rc_path, quiet, stamp);
                }
            }
            marked_block::remove_from_file(
                &rc_path,
                slot.marker_start,
                slot.marker_end,
                quiet,
                label,
            );
        }
    }
}

/// Public entrypoint: install with auto-detected style. Preserves the
/// previous signature so existing callers (setup.rs, cli/shell_init.rs)
/// don't need to change.
pub fn install_all(quiet: bool) {
    install_all_with_style(quiet, Style::Auto);
}

/// Explicit style entrypoint for callers that want to honour a `--style=`
/// CLI flag.
///
/// Captures a single `BackupStamp` here so every migration backup
/// produced by this invocation shares one suffix, even if the wall
/// clock ticks over while we're walking the slots.
pub fn install_all_with_style(quiet: bool, style: Style) {
    let Some(home) = dirs::home_dir() else {
        tracing::error!("Cannot resolve home directory");
        return;
    };

    let stamp = BackupStamp::now();
    install_zshenv(&home, quiet, style, &stamp);
    install_bashenv(&home, quiet, style, &stamp);
    install_aliases(&home, quiet, style, &stamp);
}

pub fn uninstall_all(quiet: bool) {
    let Some(home) = dirs::home_dir() else { return };

    // Try both styles unconditionally for each slot. marked_block::remove
    // and dropin::remove are both no-ops when their target is absent.
    let slots: &[(Slot, &str)] = &[
        (SLOT_ZSHENV, "shell hook for ~/.zshenv"),
        (SLOT_BASHENV, "shell hook for ~/.bashenv"),
        (SLOT_ZSHRC, "agent aliases for ~/.zshrc"),
        (SLOT_BASHRC, "agent aliases for ~/.bashrc"),
    ];

    for (slot, label) in slots {
        marked_block::remove_from_file(
            &home.join(slot.rc_file),
            slot.marker_start,
            slot.marker_end,
            quiet,
            label,
        );
        let dir_path = home.join(slot.dropin_dir);
        if dir_path.exists() {
            dropin::remove(&dir_path, slot.dropin_file, quiet, label);
        }
    }
}

fn install_zshenv(home: &Path, quiet: bool, style: Style, stamp: &BackupStamp) {
    let env_check = build_env_check();
    let hook = format!(
        r#"{MARKER_START}
if [[ -z "$LEAN_CTX_ACTIVE" && -n "$ZSH_EXECUTION_STRING" ]] && command -v lean-ctx &>/dev/null; then
  if {env_check}; then
    export LEAN_CTX_ACTIVE=1
    exec lean-ctx -c "$ZSH_EXECUTION_STRING"
  fi
fi
{MARKER_END}"#
    );

    let label = "shell hook in ~/.zshenv";
    let target = pick_target(home, &SLOT_ZSHENV, style);
    strip_other_style(home, &SLOT_ZSHENV, &target, quiet, label, stamp);
    target.upsert(&hook, quiet, label);
}

fn install_bashenv(home: &Path, quiet: bool, style: Style, stamp: &BackupStamp) {
    let env_check = build_env_check();
    let hook = format!(
        r#"{MARKER_START}
if [[ -z "$LEAN_CTX_ACTIVE" && -n "$BASH_EXECUTION_STRING" ]] && command -v lean-ctx &>/dev/null; then
  if {env_check}; then
    export LEAN_CTX_ACTIVE=1
    exec lean-ctx -c "$BASH_EXECUTION_STRING"
  fi
fi
{MARKER_END}"#
    );

    let label = "shell hook in ~/.bashenv";
    let target = pick_target(home, &SLOT_BASHENV, style);
    strip_other_style(home, &SLOT_BASHENV, &target, quiet, label, stamp);
    target.upsert(&hook, quiet, label);
}

fn install_aliases(home: &Path, quiet: bool, style: Style, stamp: &BackupStamp) {
    let mut lines = Vec::new();
    lines.push(ALIAS_START.to_string());
    for (alias_name, bin_name) in AGENT_ALIASES {
        lines.push(format!(
            "alias {alias_name}='LEAN_CTX_AGENT=1 BASH_ENV=\"$HOME/.bashenv\" {bin_name}'"
        ));
    }
    lines.push(ALIAS_END.to_string());
    let block = lines.join("\n");

    for slot in &[SLOT_ZSHRC, SLOT_BASHRC] {
        // Only act on rc files the user actually has. (Drop-in mode keys off
        // the parent rc anyway — see `dropin::detect`.)
        if !home.join(slot.rc_file).exists() {
            continue;
        }
        let label = format!("agent aliases in ~/{}", slot.rc_file);
        let target = pick_target(home, slot, style);
        strip_other_style(home, slot, &target, quiet, &label, stamp);
        target.upsert(&block, quiet, &label);
    }
}

fn build_env_check() -> String {
    let checks: Vec<String> = KNOWN_AGENT_ENV_VARS
        .iter()
        .map(|v| format!("-n \"${v}\""))
        .collect();
    format!("[[ {} ]]", checks.join(" || "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed deterministic stamp for tests that don't care about
    /// distinguishing migration generations. Tests that *do* care
    /// (e.g. the no-clobber regression) construct their own.
    fn test_stamp() -> BackupStamp {
        BackupStamp::at(
            chrono::DateTime::parse_from_rfc3339("2026-05-11T20:38:45Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        )
    }

    #[test]
    fn env_check_format() {
        let check = build_env_check();
        assert!(check.contains("LEAN_CTX_AGENT"));
        assert!(check.contains("CLAUDECODE"));
        assert!(check.contains("||"));
    }

    #[test]
    fn pick_target_inline_when_forced() {
        let tmp = tempfile::tempdir().unwrap();
        // Even with a .d/ loop, Style::Inline must force the marked target.
        std::fs::create_dir_all(tmp.path().join(".zshenv.d")).unwrap();
        std::fs::write(
            tmp.path().join(".zshenv"),
            "for f in $HOME/.zshenv.d/*.zsh; do source $f; done\n",
        )
        .unwrap();
        let t = pick_target(tmp.path(), &SLOT_ZSHENV, Style::Inline);
        assert!(matches!(t, InstallTarget::Marked { .. }));
    }

    #[test]
    fn pick_target_dropin_when_detected_under_auto() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".zshenv.d")).unwrap();
        std::fs::write(
            tmp.path().join(".zshenv"),
            "for f in $HOME/.zshenv.d/*.zsh; do source $f; done\n",
        )
        .unwrap();
        let t = pick_target(tmp.path(), &SLOT_ZSHENV, Style::Auto);
        assert!(matches!(t, InstallTarget::DropIn { .. }));
    }

    #[test]
    fn pick_target_inline_under_auto_when_no_dropin() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".zshenv"), "export PATH=/usr/bin\n").unwrap();
        let t = pick_target(tmp.path(), &SLOT_ZSHENV, Style::Auto);
        assert!(matches!(t, InstallTarget::Marked { .. }));
    }

    #[test]
    fn pick_target_dropin_falls_back_to_inline_when_no_directory() {
        // User asked for DropIn but the layout isn't set up. Don't error —
        // fall back to inline so the install still works.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".zshenv"), "export PATH=/usr/bin\n").unwrap();
        let t = pick_target(tmp.path(), &SLOT_ZSHENV, Style::DropIn);
        assert!(matches!(t, InstallTarget::Marked { .. }));
    }

    #[test]
    fn install_zshenv_writes_inline_block() {
        let tmp = tempfile::tempdir().unwrap();
        install_zshenv(tmp.path(), true, Style::Inline, &test_stamp());
        let body = std::fs::read_to_string(tmp.path().join(".zshenv")).unwrap();
        assert!(body.contains(MARKER_START));
        assert!(body.contains(MARKER_END));
        assert!(body.contains("ZSH_EXECUTION_STRING"));
    }

    #[test]
    fn install_zshenv_writes_dropin_when_loop_present() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".zshenv.d")).unwrap();
        std::fs::write(
            tmp.path().join(".zshenv"),
            "for f in $HOME/.zshenv.d/*.zsh; do source $f; done\n",
        )
        .unwrap();
        install_zshenv(tmp.path(), true, Style::Auto, &test_stamp());

        let dropin_file = tmp.path().join(".zshenv.d").join(DROPIN_ZSH);
        assert!(dropin_file.exists(), "expected drop-in file");
        let dropin_body = std::fs::read_to_string(&dropin_file).unwrap();
        assert!(dropin_body.contains("ZSH_EXECUTION_STRING"));

        let zshenv_body = std::fs::read_to_string(tmp.path().join(".zshenv")).unwrap();
        assert!(
            !zshenv_body.contains(MARKER_START),
            "drop-in install must not also leave the inline block"
        );
    }

    /// List sibling files of `path` whose name matches
    /// `<basename>.lean-ctx-<timestamp>.bak`.
    fn find_migration_backups(path: &Path) -> Vec<PathBuf> {
        let Some(parent) = path.parent() else {
            return Vec::new();
        };
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return Vec::new();
        };
        let prefix = format!("{name}.lean-ctx-");
        let mut out: Vec<PathBuf> = std::fs::read_dir(parent)
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
    fn migration_inline_to_dropin_preserves_hand_edits_via_backup() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".zshenv.d")).unwrap();
        // Existing install with a hand-edit *inside* our fenced region —
        // the bit a maintainer might worry about losing silently.
        let edited_zshenv = format!(
            "export PATH=/usr/bin\n\
             \n\
             {MARKER_START}\n\
             # USER CUSTOM: bump zsh history size for this workstation\n\
             export HISTSIZE=99999\n\
             # original lean-ctx hook content lived here\n\
             {MARKER_END}\n\
             \n\
             for f in $HOME/.zshenv.d/*.zsh; do source $f; done\n",
        );
        std::fs::write(tmp.path().join(".zshenv"), &edited_zshenv).unwrap();

        install_zshenv(tmp.path(), true, Style::Auto, &test_stamp());

        // Backup must exist and contain the user's exact pre-migration file.
        let baks = find_migration_backups(&tmp.path().join(".zshenv"));
        assert_eq!(baks.len(), 1, "expected one timestamped backup");
        let bak_body = std::fs::read_to_string(&baks[0]).unwrap();
        assert_eq!(bak_body, edited_zshenv);
        assert!(bak_body.contains("USER CUSTOM"));
        assert!(bak_body.contains("HISTSIZE=99999"));
    }

    #[test]
    fn migration_dropin_to_inline_preserves_hand_edits_via_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let dropin_dir = tmp.path().join(".zshenv.d");
        std::fs::create_dir_all(&dropin_dir).unwrap();
        // Pre-stage a drop-in file with user customisation.
        let edited_dropin = "# USER CUSTOM addition to lean-ctx drop-in\nexport FAVOURITE_EDITOR=helix\n# canonical lean-ctx content would follow\n";
        std::fs::write(dropin_dir.join(DROPIN_ZSH), edited_dropin).unwrap();
        // No source loop -> Style::Auto resolves to inline (so we migrate
        // *away* from the drop-in).
        std::fs::write(tmp.path().join(".zshenv"), "# plain zshenv\n").unwrap();

        install_zshenv(tmp.path(), true, Style::Inline, &test_stamp());

        let baks = find_migration_backups(&dropin_dir.join(DROPIN_ZSH));
        assert_eq!(baks.len(), 1, "expected one timestamped backup");
        let bak_body = std::fs::read_to_string(&baks[0]).unwrap();
        assert_eq!(bak_body, edited_dropin);
        assert!(bak_body.contains("USER CUSTOM"));
        // The original drop-in is gone, replaced by an inline block in .zshenv.
        assert!(!dropin_dir.join(DROPIN_ZSH).exists());
        let zshenv = std::fs::read_to_string(tmp.path().join(".zshenv")).unwrap();
        assert!(zshenv.contains(MARKER_START));
    }

    #[test]
    fn migration_skips_backup_when_no_prior_block_exists() {
        // Clean install (no prior lean-ctx artifacts) should not litter
        // the home dir with empty `.lean-ctx-<ts>.bak` files.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".zshenv.d")).unwrap();
        std::fs::write(
            tmp.path().join(".zshenv"),
            "for f in $HOME/.zshenv.d/*.zsh; do source $f; done\n",
        )
        .unwrap();

        install_zshenv(tmp.path(), true, Style::Auto, &test_stamp());

        assert!(
            find_migration_backups(&tmp.path().join(".zshenv")).is_empty(),
            "clean install should not create a .bak file"
        );
    }

    #[test]
    fn idempotent_dropin_reinstall_does_not_create_backup() {
        // Once installed in drop-in mode, a second `install` (e.g. via
        // `lean-ctx update` re-wiring) should not start producing backups
        // every run. The strip-other-style path only fires when there IS
        // an inline block to remove.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".zshenv.d")).unwrap();
        std::fs::write(
            tmp.path().join(".zshenv"),
            "for f in $HOME/.zshenv.d/*.zsh; do source $f; done\n",
        )
        .unwrap();

        install_zshenv(tmp.path(), true, Style::Auto, &test_stamp());
        install_zshenv(tmp.path(), true, Style::Auto, &test_stamp());

        assert!(find_migration_backups(&tmp.path().join(".zshenv")).is_empty());
    }

    #[test]
    fn backup_filename_handles_dotfile_correctly() {
        // `.zshenv` has no extension; Path::with_extension would replace
        // ".zshenv" wholesale. Using with_file_name produces the right
        // sibling path. Timestamp is appended between basename and `.bak`.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".zshenv"), "content\n").unwrap();
        save_migration_backup(&tmp.path().join(".zshenv"), true, &test_stamp());
        let baks = find_migration_backups(&tmp.path().join(".zshenv"));
        assert_eq!(baks.len(), 1);
        // The full filename must start with the original basename so it
        // sits as a sibling, not at the parent root.
        let name = baks[0].file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with(".zshenv.lean-ctx-"), "got: {name}");
        assert!(std::path::Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("bak")));
        // Sanity-check the timestamp is in the YYYYMMDDTHHMMSSZ slot.
        let stamp = name
            .trim_start_matches(".zshenv.lean-ctx-")
            .trim_end_matches(".bak");
        assert_eq!(stamp.len(), 16, "stamp should be YYYYMMDDTHHMMSSZ: {stamp}");
        assert!(stamp.contains('T'));
        assert!(stamp.ends_with('Z'));
    }

    #[test]
    fn repeated_migrations_never_clobber_prior_backups() {
        // Regression test for the convention upgrade: two migration
        // events on the same slot must produce two distinct backups,
        // not silently overwrite each other. We pin two different
        // stamps directly instead of sleeping past a second boundary.
        let stamp_first = BackupStamp::at(
            chrono::DateTime::parse_from_rfc3339("2026-05-11T20:38:45Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let stamp_later = BackupStamp::at(
            chrono::DateTime::parse_from_rfc3339("2026-05-12T09:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".zshenv.d")).unwrap();

        let with_block_v1 = format!(
            "{MARKER_START}\n# first-era custom content\n{MARKER_END}\n\nfor f in $HOME/.zshenv.d/*.zsh; do source $f; done\n",
        );
        std::fs::write(tmp.path().join(".zshenv"), &with_block_v1).unwrap();
        install_zshenv(tmp.path(), true, Style::Auto, &stamp_first);
        let baks_after_first = find_migration_backups(&tmp.path().join(".zshenv"));
        assert_eq!(baks_after_first.len(), 1);

        // User hand-puts a NEW inline block back (perhaps via a manual
        // edit or a partial reinstall in a tool we don't know about).
        let with_block_v2 = format!(
            "{}{MARKER_START}\n# second-era custom content\n{MARKER_END}\n",
            std::fs::read_to_string(tmp.path().join(".zshenv")).unwrap(),
        );
        std::fs::write(tmp.path().join(".zshenv"), &with_block_v2).unwrap();
        install_zshenv(tmp.path(), true, Style::Auto, &stamp_later);
        let baks_after_second = find_migration_backups(&tmp.path().join(".zshenv"));

        assert_eq!(
            baks_after_second.len(),
            2,
            "second migration should leave a second backup, not overwrite"
        );
        // First backup unchanged from after the first migration.
        assert_eq!(baks_after_second[0], baks_after_first[0]);
        let first_body = std::fs::read_to_string(&baks_after_second[0]).unwrap();
        let second_body = std::fs::read_to_string(&baks_after_second[1]).unwrap();
        assert!(first_body.contains("first-era custom"));
        assert!(second_body.contains("second-era custom"));
    }

    #[test]
    fn install_migrates_inline_to_dropin() {
        let tmp = tempfile::tempdir().unwrap();
        // Simulate an existing install: .zshenv with the old fenced block.
        std::fs::write(
            tmp.path().join(".zshenv"),
            format!(
                "export PATH=/usr/bin\n\n{MARKER_START}\n# old hook\n{MARKER_END}\n\nfor f in $HOME/.zshenv.d/*.zsh; do source $f; done\n",
            ),
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join(".zshenv.d")).unwrap();

        install_zshenv(tmp.path(), true, Style::Auto, &test_stamp());

        let zshenv_body = std::fs::read_to_string(tmp.path().join(".zshenv")).unwrap();
        assert!(
            !zshenv_body.contains(MARKER_START),
            "old inline block should be stripped after migration"
        );
        assert!(
            zshenv_body.contains(".zshenv.d"),
            "source loop must be preserved"
        );
        let dropin_file = tmp.path().join(".zshenv.d").join(DROPIN_ZSH);
        assert!(dropin_file.exists(), "new drop-in file should be present");
    }

    #[test]
    fn install_migrates_dropin_to_inline() {
        let tmp = tempfile::tempdir().unwrap();
        // No source loop → Style::Inline forces inline. Pre-stage a
        // leftover drop-in file as if the user previously had the layout.
        std::fs::create_dir_all(tmp.path().join(".zshenv.d")).unwrap();
        std::fs::write(
            tmp.path().join(".zshenv.d").join(DROPIN_ZSH),
            "# stale lean-ctx drop-in\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join(".zshenv"), "export PATH=/usr/bin\n").unwrap();

        install_zshenv(tmp.path(), true, Style::Inline, &test_stamp());

        assert!(
            !tmp.path().join(".zshenv.d").join(DROPIN_ZSH).exists(),
            "drop-in file should be removed when installing inline"
        );
        let body = std::fs::read_to_string(tmp.path().join(".zshenv")).unwrap();
        assert!(body.contains(MARKER_START));
    }

    #[test]
    fn install_is_idempotent_in_dropin_mode() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".zshenv.d")).unwrap();
        std::fs::write(
            tmp.path().join(".zshenv"),
            "for f in $HOME/.zshenv.d/*.zsh; do source $f; done\n",
        )
        .unwrap();

        install_zshenv(tmp.path(), true, Style::Auto, &test_stamp());
        let after_first = std::fs::read(tmp.path().join(".zshenv.d").join(DROPIN_ZSH)).unwrap();

        install_zshenv(tmp.path(), true, Style::Auto, &test_stamp());
        let after_second = std::fs::read(tmp.path().join(".zshenv.d").join(DROPIN_ZSH)).unwrap();

        assert_eq!(after_first, after_second);
    }

    #[test]
    fn install_is_idempotent_in_inline_mode() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".zshenv"), "# top\n").unwrap();

        install_zshenv(tmp.path(), true, Style::Inline, &test_stamp());
        let after_first = std::fs::read(tmp.path().join(".zshenv")).unwrap();

        install_zshenv(tmp.path(), true, Style::Inline, &test_stamp());
        let after_second = std::fs::read(tmp.path().join(".zshenv")).unwrap();

        assert_eq!(after_first, after_second);
    }

    #[test]
    fn install_aliases_skips_when_rc_missing() {
        let tmp = tempfile::tempdir().unwrap();
        // No .zshrc, no .bashrc — nothing should be created.
        install_aliases(tmp.path(), true, Style::Auto, &test_stamp());
        assert!(!tmp.path().join(".zshrc").exists());
        assert!(!tmp.path().join(".bashrc").exists());
    }

    #[test]
    fn install_aliases_writes_dropin_when_zshrc_d_configured() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".zshrc.d")).unwrap();
        std::fs::write(
            tmp.path().join(".zshrc"),
            "for f in $HOME/.zshrc.d/*.zsh; do source $f; done\n",
        )
        .unwrap();

        install_aliases(tmp.path(), true, Style::Auto, &test_stamp());

        let dropin_file = tmp.path().join(".zshrc.d").join(DROPIN_ZSH);
        assert!(dropin_file.exists());
        let body = std::fs::read_to_string(&dropin_file).unwrap();
        assert!(body.contains("LEAN_CTX_AGENT=1"));
    }
}
