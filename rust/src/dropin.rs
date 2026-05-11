//! Drop-in style installation.
//!
//! Some users (or their dotfiles managers — chezmoi, yadm, stow, oh-my-zsh
//! `custom/`, etc.) keep their shell config split into a small main file plus
//! a directory of numbered fragments that the main file sources in lexical
//! order (e.g. `~/.zshenv.d/00-homebrew.zsh`, `10-fnm.zsh`, ...).
//!
//! When that convention is in use, appending an inline fenced block to the
//! main rc file creates drift between the main file and the dotfiles source
//! of truth. The drop-in install mode writes the same hook content into a
//! single fragment file in the `.d/` directory instead, leaving the main rc
//! file untouched.
//!
//! Detection is conservative: we require both that the `.d/` directory
//! exists AND that the main rc file references it from a non-comment line.
//! That avoids treating an unused empty directory as opt-in.

use std::path::{Path, PathBuf};

/// Detect whether the user's rc file in `home` references the named drop-in
/// directory and that directory exists. Returns the resolved directory path
/// on success.
///
/// Arguments are kept generic so this works for `.zshenv` / `.zshenv.d`,
/// `.zshrc` / `.zshrc.d`, `.bashrc` / `.bashrc.d`, etc.
pub fn detect(home: &Path, rc_file_name: &str, dropin_dir_name: &str) -> Option<PathBuf> {
    let dir = home.join(dropin_dir_name);
    if !dir.is_dir() {
        return None;
    }
    let rc_contents = std::fs::read_to_string(home.join(rc_file_name)).ok()?;
    if rc_references_dropin(&rc_contents, dropin_dir_name) {
        Some(dir)
    } else {
        None
    }
}

/// True if any non-comment line in `rc_contents` mentions `dropin_dir_name`.
///
/// We deliberately don't try to parse the shell — any user who has put the
/// directory name in their live config (a source loop, a glob `for` loop,
/// even an `autoload` call) has made the intent clear enough.
pub fn rc_references_dropin(rc_contents: &str, dropin_dir_name: &str) -> bool {
    rc_contents.lines().any(|line| {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            return false;
        }
        line.contains(dropin_dir_name)
    })
}

/// Write `content` to `<dir>/<filename>`, creating `dir` if needed.
///
/// Idempotent: overwrites any existing file. The trailing newline is
/// normalised so re-runs with identical content produce identical bytes.
pub fn write(dir: &Path, filename: &str, content: &str, quiet: bool, label: &str) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::error!("Cannot create {}: {e}", dir.display());
        return;
    }
    let file = dir.join(filename);
    let body = format!("{}\n", content.trim_end_matches('\n'));
    if let Err(e) = std::fs::write(&file, body) {
        tracing::error!("Cannot write {}: {e}", file.display());
        return;
    }
    if !quiet {
        eprintln!("  Installed {label} at {}", file.display());
    }
}

/// Remove `<dir>/<filename>` if it exists. No-op otherwise.
pub fn remove(dir: &Path, filename: &str, quiet: bool, label: &str) {
    let file = dir.join(filename);
    if !file.exists() {
        return;
    }
    if let Err(e) = std::fs::remove_file(&file) {
        tracing::error!("Cannot remove {}: {e}", file.display());
        return;
    }
    if !quiet {
        println!("  Removed {label} from {}", file.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn rc_references_dropin_matches_source_loop() {
        let rc = r#"# top
if [[ -d "$HOME/.zshenv.d" ]]; then
    for f in "$HOME/.zshenv.d"/*.zsh(N); do source "$f"; done
fi
"#;
        assert!(rc_references_dropin(rc, ".zshenv.d"));
    }

    #[test]
    fn rc_references_dropin_ignores_comment_only_mentions() {
        let rc = "# Once we have a ~/.zshenv.d we should adopt it.\nexport PATH=/usr/bin\n";
        assert!(!rc_references_dropin(rc, ".zshenv.d"));
    }

    #[test]
    fn rc_references_dropin_handles_empty_file() {
        assert!(!rc_references_dropin("", ".zshenv.d"));
    }

    #[test]
    fn detect_returns_none_without_directory() {
        let tmp = fixture();
        std::fs::write(
            tmp.path().join(".zshenv"),
            "for f in $HOME/.zshenv.d/*.zsh; do source $f; done\n",
        )
        .unwrap();
        assert!(detect(tmp.path(), ".zshenv", ".zshenv.d").is_none());
    }

    #[test]
    fn detect_returns_none_with_directory_but_no_reference() {
        let tmp = fixture();
        std::fs::create_dir_all(tmp.path().join(".zshenv.d")).unwrap();
        std::fs::write(tmp.path().join(".zshenv"), "export PATH=/usr/bin\n").unwrap();
        assert!(detect(tmp.path(), ".zshenv", ".zshenv.d").is_none());
    }

    #[test]
    fn detect_returns_dir_when_loop_and_directory_present() {
        let tmp = fixture();
        std::fs::create_dir_all(tmp.path().join(".zshenv.d")).unwrap();
        std::fs::write(
            tmp.path().join(".zshenv"),
            "if [[ -d \"$HOME/.zshenv.d\" ]]; then\n  for f in $HOME/.zshenv.d/*.zsh; do source $f; done\nfi\n",
        )
        .unwrap();
        let got = detect(tmp.path(), ".zshenv", ".zshenv.d");
        assert_eq!(got, Some(tmp.path().join(".zshenv.d")));
    }

    #[test]
    fn detect_returns_none_when_rc_file_missing() {
        let tmp = fixture();
        std::fs::create_dir_all(tmp.path().join(".zshenv.d")).unwrap();
        assert!(detect(tmp.path(), ".zshenv", ".zshenv.d").is_none());
    }

    #[test]
    fn write_then_remove_roundtrip() {
        let tmp = fixture();
        let dir = tmp.path().join(".zshenv.d");
        write(&dir, "00-lean-ctx.zsh", "echo hi", true, "test");
        let file = dir.join("00-lean-ctx.zsh");
        assert!(file.exists());
        let body = std::fs::read_to_string(&file).unwrap();
        assert_eq!(body, "echo hi\n");
        remove(&dir, "00-lean-ctx.zsh", true, "test");
        assert!(!file.exists());
    }

    #[test]
    fn write_creates_missing_directory() {
        let tmp = fixture();
        let dir = tmp.path().join("nested").join(".zshenv.d");
        write(&dir, "00-lean-ctx.zsh", "echo hi", true, "test");
        assert!(dir.join("00-lean-ctx.zsh").exists());
    }

    #[test]
    fn write_is_idempotent_for_identical_content() {
        let tmp = fixture();
        let dir = tmp.path().join(".zshenv.d");
        write(&dir, "00-lean-ctx.zsh", "echo hi", true, "test");
        let first = std::fs::read(dir.join("00-lean-ctx.zsh")).unwrap();
        write(&dir, "00-lean-ctx.zsh", "echo hi", true, "test");
        let second = std::fs::read(dir.join("00-lean-ctx.zsh")).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn write_overwrites_changed_content() {
        let tmp = fixture();
        let dir = tmp.path().join(".zshenv.d");
        write(&dir, "00-lean-ctx.zsh", "echo old", true, "test");
        write(&dir, "00-lean-ctx.zsh", "echo new", true, "test");
        let body = std::fs::read_to_string(dir.join("00-lean-ctx.zsh")).unwrap();
        assert_eq!(body, "echo new\n");
    }

    #[test]
    fn remove_is_noop_when_file_missing() {
        let tmp = fixture();
        let dir = tmp.path().join(".zshenv.d");
        std::fs::create_dir_all(&dir).unwrap();
        remove(&dir, "00-lean-ctx.zsh", true, "test");
    }

    #[test]
    fn remove_is_noop_when_directory_missing() {
        let tmp = fixture();
        let dir = tmp.path().join(".zshenv.d");
        // Directory deliberately not created.
        remove(&dir, "00-lean-ctx.zsh", true, "test");
    }
}
