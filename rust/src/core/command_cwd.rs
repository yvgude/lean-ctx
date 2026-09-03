//! Which directory a command's segments actually run in (GH #1661, #1662).
//!
//! A tool call carries one `cwd`, but the command it runs may move: `cd repo &&
//! grep -rn … .` walks a tree the call never named, and `cd "$SCRATCH" && curl
//! -o shot.png …` writes to a directory that is sanctioned for exactly that.
//! Judging either against the call's `cwd` alone gets the answer wrong — too
//! lenient in the first case, too strict in the second.
//!
//! Only a plain leading `cd <literal>` is followed. Anything the shell would
//! have to evaluate — a variable, a substitution, `pushd`, a subshell — leaves
//! the directory unknown, and an unknown directory is reported as such rather
//! than guessed at.

use std::path::{Path, PathBuf};

/// One command segment together with the directory it runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentCwd {
    pub segment: String,
    /// `None` once a `cd` has moved somewhere this cannot resolve statically.
    pub cwd: Option<PathBuf>,
}

/// Split `command` into segments, tracking `cd` as it goes.
///
/// Segmentation is delegated to the shell tokenizer so this never disagrees
/// with the allowlist about where one command ends and the next begins.
pub fn segments_with_cwd(command: &str, base: Option<&Path>) -> Vec<SegmentCwd> {
    let mut cwd: Option<PathBuf> = base.map(Path::to_path_buf);
    let mut out = Vec::new();

    for segment in super::shell_allowlist::extract_all_commands_pub(command) {
        if let Some(target) = cd_target(&segment) {
            cwd = resolve_cd(cwd.as_deref(), target);
            // The `cd` itself runs in the directory it is leaving; recording it
            // with the new one would misattribute a `cd` that fails.
            out.push(SegmentCwd {
                segment,
                cwd: cwd.clone(),
            });
            continue;
        }
        out.push(SegmentCwd {
            segment,
            cwd: cwd.clone(),
        });
    }
    out
}

/// The directory in effect once every segment has run.
pub fn final_cwd(command: &str, base: Option<&Path>) -> Option<PathBuf> {
    segments_with_cwd(command, base)
        .last()
        .and_then(|s| s.cwd.clone())
}

/// The literal argument of a segment that is exactly `cd <path>`.
fn cd_target(segment: &str) -> Option<&str> {
    let mut words = segment.split_whitespace();
    if words.next()? != "cd" {
        return None;
    }
    let target = words.next()?;
    if words.next().is_some() {
        // `cd a b` is not something to reason about.
        return None;
    }
    Some(target)
}

/// Apply one `cd`, or give up on knowing the directory.
///
/// `cd` with no resolvable target is not "stay here": it moves somewhere this
/// cannot see. Returning `None` keeps callers from judging a later segment
/// against a directory it does not run in.
fn resolve_cd(from: Option<&Path>, target: &str) -> Option<PathBuf> {
    let unquoted = target.trim_matches(['"', '\'']);
    if unquoted.is_empty() || unquoted.contains('$') || unquoted.contains('`') || unquoted == "-" {
        return None;
    }
    // `cd ~` and `cd ~/x` are the shell's expansion, not a relative path.
    if let Some(rest) = unquoted.strip_prefix('~') {
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        return Some(join_in(&home, rest.trim_start_matches('/')));
    }
    // This is shell text, so `/tmp` is an absolute POSIX path even on Windows,
    // where `Path::is_absolute` says otherwise. Agents write Unix paths there
    // routinely (Git Bash, WSL, generated commands) — the same reason
    // `is_unix_scratch_prefix` exists (#1467). Treating them as relative made a
    // `cd /tmp` resolve to nothing on Windows, and the guard then judged the
    // download target against no directory at all.
    if unquoted.starts_with('/') {
        return Some(PathBuf::from(normalize_posix(unquoted)));
    }
    let path = Path::new(unquoted);
    if path.is_absolute() {
        return Some(normalize(path));
    }
    Some(join_in(from?, unquoted))
}

/// Join `rel` under `base`, keeping a POSIX-rooted base POSIX.
///
/// `Path::join` would splice a backslash into `/tmp` on Windows, and the
/// scratch-prefix checks match on `/tmp/`.
pub fn join_in(base: &Path, rel: &str) -> PathBuf {
    let base_str = base.to_string_lossy();
    if base_str.starts_with('/') {
        return PathBuf::from(normalize_posix(&format!(
            "{}/{rel}",
            base_str.trim_end_matches('/')
        )));
    }
    normalize(&base.join(rel))
}

/// Resolve `.` and `..` in a POSIX path textually, on any platform.
fn normalize_posix(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    format!("/{}", out.join("/"))
}

/// Resolve `.` and `..` textually. The directory may not exist yet, so this
/// deliberately does not touch the filesystem.
fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leading_cd_moves_the_following_segments() {
        let segs = segments_with_cwd(
            "cd /tmp/work && curl -o x.png http://e/x",
            Some(Path::new("/proj")),
        );
        assert_eq!(
            segs.last().unwrap().cwd.as_deref(),
            Some(Path::new("/tmp/work"))
        );
    }

    #[test]
    fn without_a_cd_every_segment_keeps_the_call_directory() {
        let segs = segments_with_cwd("echo a && echo b", Some(Path::new("/proj")));
        assert!(
            segs.iter()
                .all(|s| s.cwd.as_deref() == Some(Path::new("/proj")))
        );
    }

    #[test]
    fn a_relative_cd_is_joined_onto_the_call_directory() {
        assert_eq!(
            final_cwd("cd sub/dir && ls", Some(Path::new("/proj"))),
            Some(PathBuf::from("/proj/sub/dir"))
        );
        assert_eq!(
            final_cwd("cd ../side && ls", Some(Path::new("/proj/here"))),
            Some(PathBuf::from("/proj/side"))
        );
    }

    /// A directory the shell would have to compute is unknown, not the old one
    /// — judging a later segment against a stale directory is how a guard ends
    /// up permitting or blocking the wrong thing.
    #[test]
    fn an_unresolvable_cd_makes_the_directory_unknown() {
        assert_eq!(
            final_cwd("cd \"$TMPDIR\" && ls", Some(Path::new("/proj"))),
            None
        );
        assert_eq!(final_cwd("cd - && ls", Some(Path::new("/proj"))), None);
    }

    /// A Unix path in shell text stays a Unix path on Windows too (#1467).
    /// This is the case that broke CI: `cd /tmp` resolved to nothing there, so
    /// the download guard judged `shot.png` against no directory at all.
    #[test]
    fn a_unix_path_in_shell_text_is_absolute_on_every_platform() {
        assert_eq!(
            final_cwd("cd /tmp && curl -o shot.png https://e/x", None),
            Some(PathBuf::from("/tmp"))
        );
        assert_eq!(
            final_cwd("cd /private/tmp/session/../session && ls", None),
            Some(PathBuf::from("/private/tmp/session"))
        );
        assert_eq!(
            join_in(Path::new("/tmp/work"), "sub/shot.png"),
            PathBuf::from("/tmp/work/sub/shot.png"),
            "and joins with a forward slash, which the scratch prefixes match on"
        );
    }

    #[test]
    fn no_base_and_no_cd_is_unknown() {
        assert_eq!(final_cwd("ls -la", None), None);
    }
}
