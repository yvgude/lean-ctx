//! XDG layout commitment pin (GL #623 / #624).
//!
//! `paths::single_dir_override` re-derives which directory is canonical purely
//! from on-disk markers on *every* operation. Without a commitment signal a
//! single stray `~/.lean-ctx/<marker>` (a legacy residue, a restored backup, a
//! concurrent older binary, a half-finished migration) permanently re-collapses
//! config/data/state/cache back onto that one directory — exactly the "XDG
//! layout not stable" report (GL #623): config stops being found and the graph
//! disappears from the dashboard.
//!
//! The pin is a tiny `layout.toml` in the **config** dir
//! (`$XDG_CONFIG_HOME/lean-ctx/layout.toml`). It is resolved through the XDG
//! config base directly — never through the single-dir collapse it governs — so
//! it can never depend on the decision it is meant to make (no cycle). Its
//! presence with `mode = "xdg"` tells the resolver: this install is committed to
//! XDG, so ignore a legacy `~/.lean-ctx` / mixed `$XDG_CONFIG_HOME` data marker.
//!
//! Determinism (#498): the resolver only ever *reads* the pin (a pure function
//! of the filesystem). Writes happen at explicit, idempotent call sites (setup,
//! `doctor`, daemon/server start) and the body is byte-stable.

use std::path::Path;

/// Pin filename, living alongside `config.toml` in the config dir. Categorized
/// as config by `xdg_migrate` so a split never relocates it.
pub(crate) const LAYOUT_FILE: &str = "layout.toml";

/// Byte-stable body for an XDG-committed install (#498).
const XDG_PIN_BODY: &str = "# lean-ctx layout pin (GL #623) — managed by lean-ctx, do not edit.\n# Marks this install as committed to the XDG four-dir layout so a stray\n# ~/.lean-ctx never re-collapses config/data/state/cache. Remove via\n# `lean-ctx doctor` only if you intentionally revert to a single-dir layout.\nmode = \"xdg\"\n";

/// `true` when `<config_base>/lean-ctx/layout.toml` pins the XDG layout.
/// `config_base` is the XDG **config** base (e.g. `~/.config`) — the same base
/// [`crate::core::paths::single_dir_override`] resolves — so the read location
/// always matches the mixed-install probe it sits next to. Hermetic (no env
/// access) so the resolver path stays unit-testable.
pub(crate) fn is_xdg_pinned_in(config_base: &Path) -> bool {
    read_mode(&config_base.join("lean-ctx").join(LAYOUT_FILE)).as_deref() == Some("xdg")
}

/// Runtime read honoring the same env resolution as the resolver
/// (`$XDG_CONFIG_HOME/lean-ctx`). Used by `doctor`/diagnostics.
#[must_use]
pub(crate) fn is_xdg_pinned() -> bool {
    crate::core::paths::xdg_config_lean_ctx_dir()
        .is_some_and(|d| read_mode(&d.join(LAYOUT_FILE)).as_deref() == Some("xdg"))
}

/// Pin this install to the XDG layout — but only when it genuinely *is* XDG:
///
/// - skips when a directory override (`LEAN_CTX_CONFIG_DIR`, `_DATA_DIR`,
///   `_STATE_DIR`, `_CACHE_DIR`) is set: it applies to this process only;
/// - skips while a legacy `~/.lean-ctx` or mixed `$XDG_CONFIG_HOME/lean-ctx`
///   still holds data markers (a real single-dir/mixed install that must keep
///   resolving in place until `doctor --fix` splits it);
/// - otherwise writes `mode = "xdg"` atomically.
///
/// Idempotent: a no-op once the pin already says `xdg`. Safe to call from any
/// startup path.
pub(crate) fn ensure_pinned() {
    // Any directory override, not only the data dir (GH #2007).
    if crate::core::paths::dir_override_active() {
        return;
    }
    // `single_dir_override` returns `Some` only for an unpinned legacy/mixed
    // single-dir install; `None` means we are already on (or defaulting to) XDG.
    if crate::core::paths::single_dir_override().is_some() {
        return;
    }
    let Some(dir) = crate::core::paths::xdg_config_lean_ctx_dir() else {
        return;
    };
    let path = dir.join(LAYOUT_FILE);
    if read_mode(&path).as_deref() == Some("xdg") {
        return;
    }
    if std::fs::create_dir_all(&dir).is_ok() {
        crate::core::data_dir::ensure_dir_permissions(&dir);
        write_atomic(&path, XDG_PIN_BODY);
    }
}

/// Self-heal the layout at startup. Idempotent and best-effort:
///
/// 1. [`ensure_pinned`] — commit to XDG once the install genuinely is XDG.
/// 2. When committed, drain a residual `~/.lean-ctx` into the XDG dirs and
///    remove it. Safe precisely *because* of the pin: `single_dir_override`
///    ignores `~/.lean-ctx` for a committed install, so no live writer targets
///    it and the drain can never race a concurrent write (GL #623 / #626).
///
/// Cheap to call from any startup path: the reclaim returns immediately when
/// `~/.lean-ctx` does not exist.
pub(crate) fn heal() {
    ensure_pinned();
    if is_xdg_pinned() {
        let _ = crate::core::xdg_migrate::reclaim_legacy();
    }
    // GH #1421: heal stale config.toml copies in non-canonical directories
    // (legacy ~/.lean-ctx, stale XDG, or data dir). Runs after the layout pin
    // and reclaim so the canonical config dir is stable.
    for report in crate::core::config_heal::heal_all() {
        eprintln!(
            "[lean-ctx] config heal: {} {} → {}",
            match report.action {
                crate::core::config_heal::HealAction::Adopted => "adopted",
                crate::core::config_heal::HealAction::Superseded => "superseded",
            },
            report.from.display(),
            report.to.display(),
        );
    }
}

/// Atomic write via a sibling temp file + rename, so a crash never leaves a
/// half-written pin that could be misread as a different mode.
fn write_atomic(path: &Path, body: &str) {
    let tmp = path.with_extension("toml.tmp");
    if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Parse the `mode = "..."` value from a pin file, ignoring comments/blank
/// lines. Returns `None` when the file is absent, unreadable, or has no `mode`.
fn read_mode(path: &Path) -> Option<String> {
    let body = std::fs::read_to_string(path).ok()?;
    body.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("mode")?;
        let val = rest
            .trim_start()
            .strip_prefix('=')?
            .trim()
            .trim_matches('"')
            .trim();
        (!val.is_empty()).then(|| val.to_string())
    })
}

/// Write the XDG pin under `<config_base>/lean-ctx` regardless of the current
/// install state. Test-only helper for driving the hermetic resolver tests; the
/// production write path is [`ensure_pinned`].
#[cfg(test)]
pub(crate) fn write_xdg_pin_in(config_base: &Path) -> std::io::Result<()> {
    let dir = config_base.join("lean-ctx");
    std::fs::create_dir_all(&dir)?;
    crate::core::data_dir::ensure_dir_permissions(&dir);
    std::fs::write(dir.join(LAYOUT_FILE), XDG_PIN_BODY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_mode_parses_xdg_pin() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path();
        write_xdg_pin_in(cfg).unwrap();
        assert!(is_xdg_pinned_in(cfg));
    }

    #[test]
    fn unpinned_dir_is_not_pinned() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_xdg_pinned_in(tmp.path()));
    }

    #[test]
    fn read_mode_ignores_comments_and_other_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("lean-ctx");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(LAYOUT_FILE),
            "# mode = \"legacy\"\nother = 1\nmode = \"xdg\"\n",
        )
        .unwrap();
        assert!(is_xdg_pinned_in(tmp.path()));
    }

    #[test]
    fn non_xdg_mode_is_not_xdg_pinned() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("lean-ctx");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(LAYOUT_FILE), "mode = \"single\"\n").unwrap();
        assert!(!is_xdg_pinned_in(tmp.path()));
    }
}
