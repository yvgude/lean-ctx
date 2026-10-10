// SPDX-License-Identifier: Apache-2.0
//! The `.old.exe` sidecar used by every Windows binary swap (#2048).
//!
//! Windows lets a running executable be renamed but not deleted, so a swap
//! moves `lean-ctx.exe` aside to `lean-ctx.old.exe` and puts the new binary
//! in its place. The process that performed the swap is still executing from
//! the sidecar, so its own cleanup cannot delete it; the sidecar is therefore
//! reclaimed by the *next* swap. If an old image is still running from it
//! (an editor's MCP server, a daemon), it is renamed to a unique name rather
//! than blocking the update, and swept once its holder has exited.

use std::path::{Path, PathBuf};

/// `<dir>/lean-ctx.old.exe` for `<dir>/lean-ctx.exe`.
pub(crate) fn sidecar_path(exe: &Path) -> PathBuf {
    exe.with_extension("old.exe")
}

/// Free the sidecar path for the next swap: sweep leftovers from earlier
/// swaps, then make sure `sidecar_path(exe)` no longer exists.
pub(crate) fn clear_sidecar(exe: &Path) -> Result<PathBuf, String> {
    clear_sidecar_with(exe, |path| std::fs::remove_file(path))
}

/// [`clear_sidecar`] with an injectable delete, so the "image still running"
/// branch is testable on platforms whose delete never fails on open files.
fn clear_sidecar_with(
    exe: &Path,
    remove: impl Fn(&Path) -> std::io::Result<()>,
) -> Result<PathBuf, String> {
    let sidecar = sidecar_path(exe);
    for leftover in leftover_sidecars(exe) {
        // Still held by a running old image: kept until a later swap.
        let _ = remove(&leftover);
    }
    if std::fs::symlink_metadata(&sidecar).is_err() {
        return Ok(sidecar);
    }
    if is_symlink(&sidecar) {
        return Err(format!(
            "refusing to replace symlinked rollback sidecar {}",
            sidecar.display()
        ));
    }
    // A running image can be renamed, so park it under a unique name.
    let parked = parked_path(exe);
    std::fs::rename(&sidecar, &parked).map_err(|e| {
        format!(
            "previous lean-ctx binary {} is still locked ({e}). \
             Run `lean-ctx stop`, close editors using lean-ctx, then retry.",
            sidecar.display()
        )
    })?;
    Ok(sidecar)
}

/// Sidecars from earlier swaps next to `exe`: `lean-ctx.old.exe` and the
/// parked `lean-ctx.old-<pid>-<nanos>.exe` names. Never `exe` itself.
fn leftover_sidecars(exe: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(stem)) = (exe.parent(), exe.file_stem().and_then(|s| s.to_str())) else {
        return Vec::new();
    };
    let prefix = format!("{}.old", stem.to_ascii_lowercase());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
                && path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|name| {
                        name.to_ascii_lowercase()
                            .strip_prefix(&prefix)
                            .is_some_and(is_sidecar_suffix)
                    })
        })
        .collect();
    found.sort();
    found
}

/// `""` (`lean-ctx.old.exe`) or `-<digits>-<digits>` (parked).
fn is_sidecar_suffix(middle: &str) -> bool {
    middle.is_empty()
        || middle.strip_prefix('-').is_some_and(|rest| {
            !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '-')
        })
}

fn parked_path(exe: &Path) -> PathBuf {
    let stem = exe
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("lean-ctx");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    exe.with_file_name(format!("{stem}.old-{}-{nanos}.exe", std::process::id()))
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn names(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn missing_sidecar_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("lean-ctx.exe");
        fs::write(&exe, b"v2").unwrap();
        assert_eq!(
            clear_sidecar(&exe).unwrap(),
            dir.path().join("lean-ctx.old.exe")
        );
        assert_eq!(names(dir.path()), ["lean-ctx.exe"]);
    }

    /// #2048: the sidecar left behind by the previous swap must not block the
    /// next update; once nothing runs from it, it is simply deleted.
    #[test]
    fn released_sidecar_from_previous_swap_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("lean-ctx.exe");
        fs::write(&exe, b"v2").unwrap();
        fs::write(dir.path().join("lean-ctx.old.exe"), b"v1").unwrap();
        clear_sidecar(&exe).unwrap();
        assert_eq!(names(dir.path()), ["lean-ctx.exe"]);
    }

    /// A sidecar an old process still executes from cannot be deleted on
    /// Windows; it is parked under a unique name so the swap can proceed.
    #[test]
    fn held_sidecar_is_parked_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("lean-ctx.exe");
        let sidecar = dir.path().join("lean-ctx.old.exe");
        fs::write(&exe, b"v2").unwrap();
        fs::write(&sidecar, b"v1").unwrap();
        let held = |_: &Path| Err(std::io::Error::other("in use"));

        assert_eq!(clear_sidecar_with(&exe, held).unwrap(), sidecar);
        assert!(!sidecar.exists());
        let parked: Vec<_> = names(dir.path())
            .into_iter()
            .filter(|n| n.starts_with("lean-ctx.old-"))
            .collect();
        assert_eq!(parked.len(), 1, "{parked:?}");
        assert_eq!(fs::read(dir.path().join(&parked[0])).unwrap(), b"v1");
        assert_eq!(fs::read(&exe).unwrap(), b"v2");
    }

    #[test]
    fn parked_sidecars_are_swept_once_released() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("lean-ctx.exe");
        fs::write(&exe, b"v3").unwrap();
        fs::write(dir.path().join("lean-ctx.old-42-1700000000.exe"), b"v1").unwrap();
        fs::write(dir.path().join("LEAN-CTX.OLD.EXE"), b"v2").unwrap();
        clear_sidecar(&exe).unwrap();
        assert_eq!(names(dir.path()), ["lean-ctx.exe"]);
    }

    #[test]
    fn sweep_never_touches_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("lean-ctx.exe");
        for name in [
            "lean-ctx.exe",
            "lean-ctx.older.exe",
            "lean-ctx.old-notes.exe",
            "lean-ctx.old.exe.bak",
            "lean-ctx-update.bat",
            "other.old.exe",
        ] {
            fs::write(dir.path().join(name), b"x").unwrap();
        }
        clear_sidecar(&exe).unwrap();
        assert_eq!(
            names(dir.path()),
            [
                "lean-ctx-update.bat",
                "lean-ctx.exe",
                "lean-ctx.old-notes.exe",
                "lean-ctx.old.exe.bak",
                "lean-ctx.older.exe",
                "other.old.exe",
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_sidecar_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("lean-ctx.exe");
        let target = dir.path().join("elsewhere");
        fs::write(&exe, b"v2").unwrap();
        fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("lean-ctx.old.exe")).unwrap();
        let err = clear_sidecar(&exe).unwrap_err();
        assert!(err.contains("symlinked"), "{err}");
        assert_eq!(fs::read(&target).unwrap(), b"keep");
    }
}
