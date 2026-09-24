//! Which directory a command's segments actually run in (GH #1661, #1662).
//!
//! A tool call carries one `cwd`, but the command it runs may move: `cd repo &&
//! grep -rn … .` walks a tree the call never named, and `cd "$SCRATCH" && curl
//! -o shot.png …` writes to a directory that is sanctioned for exactly that.
//! Judging either against the call's `cwd` alone gets the answer wrong — too
//! lenient in the first case, too strict in the second.
//!
//! Only a plain `cd <literal>` segment is followed, and only where it is
//! certain to have run before the next command (#1850). Everything else that
//! can move the shell — a variable, a substitution, `pushd`/`popd`, a `cd`
//! inside a subshell, a brace group, a loop or a function, a `cd` that may be
//! skipped (`a || cd x`) or may fail (`cd missing; …`) — leaves the directory
//! unknown, and an unknown directory is reported as such rather than guessed
//! at. The write guard relies on that: judging a later command against a
//! directory it does not run in is how a redirect lands in the project.

use std::path::{Path, PathBuf};

use super::shell_allowlist::Separator;

/// One command segment together with the directory it runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentCwd {
    pub segment: String,
    /// `None` once a `cd` has moved somewhere this cannot resolve statically,
    /// or when more than one directory is possible.
    pub cwd: Option<PathBuf>,
}

/// Split `command` into segments, tracking `cd` as it goes.
///
/// Segmentation is delegated to the shell tokenizer so this never disagrees
/// with the allowlist about where one command ends and the next begins.
pub fn segments_with_cwd(command: &str, base: Option<&Path>) -> Vec<SegmentCwd> {
    track(command, base).0
}

/// The directory in effect once every segment has run.
pub fn final_cwd(command: &str, base: Option<&Path>) -> Option<PathBuf> {
    track(command, base).1
}

/// A `cd` that may not be in effect: it can fail (its target does not exist
/// yet) or be skipped (`a && cd x`, `a || cd x`). Commands reached from it only
/// through `&&` — and the pipelines among them — run in `moved`; anything that
/// can also be reached when the `cd` did not happen runs in either directory.
struct Pending {
    moved: Option<PathBuf>,
    /// Where the shell is if this `cd` did not take effect.
    old: Option<PathBuf>,
    /// Behind `||`: a successful left side skips the `cd` entirely.
    skippable: bool,
    /// `cd x || …`: the next segment is the fallback, which runs only if the
    /// `cd` did not take effect.
    fallback: bool,
}

impl Pending {
    fn either(self) -> Option<PathBuf> {
        merge(self.moved, self.old.as_deref())
    }
}

fn track(command: &str, base: Option<&Path>) -> (Vec<SegmentCwd>, Option<PathBuf>) {
    let segments = super::shell_allowlist::segments_with_separators(command);
    // `CDPATH` reroutes a relative `cd sub` to wherever it finds `sub` first.
    let cdpath = std::env::var_os("CDPATH").is_some_and(|v| !v.is_empty())
        || segments.iter().any(|(s, _)| s.contains("CDPATH"));

    // The directory the next segment starts in.
    let mut cwd: Option<PathBuf> = base.map(Path::to_path_buf);
    // Where the current and-or list started: `a && cd x &` runs the whole list
    // in a background subshell, so nothing in it moves what comes after.
    let mut list_base = cwd.clone();
    let mut pending: Option<Pending> = None;
    let mut prev: Option<Separator> = None;
    let mut out = Vec::with_capacity(segments.len());

    for (segment, sep) in segments {
        // Every segment runs where it starts — a `cd` included: its own
        // redirect is opened before it moves, and a failed `cd` stays put.
        let here = cwd.clone();
        let mut own = here.clone();

        if let Some(p) = pending.take_if(|p| p.fallback) {
            // `cd x || exit 1; …` — past the fallback, only a working `cd` is
            // left. A skippable `cd` is not: `a || cd x || exit` exits on neither
            // path when `a` succeeds.
            cwd = if exits(&segment) && !p.skippable {
                p.moved
            } else if cd_target(&segment).is_some() || moves_opaquely(&segment) {
                None
            } else {
                p.either()
            };
        } else if let Some(target) = cd_target(&segment) {
            let moved = if cdpath && is_cdpath_candidate(target) {
                None
            } else {
                resolve_cd(here.as_deref(), target)
            };
            // With an uncertain `cd` still open (`cd a && cd b`), not taking
            // this one leaves the shell in `a` *or* where `a` started.
            let old = match pending.take() {
                Some(p) => merge(here.clone(), p.old.as_deref()),
                None => here.clone(),
            };
            let runs_unconditionally = matches!(
                prev,
                None | Some(Separator::Sequence | Separator::Background)
            );
            if prev == Some(Separator::Pipe) || sep == Some(Separator::Pipe) {
                // A pipeline element runs in a subshell — in bash. zsh runs the
                // last one in the current shell, so neither reading is safe.
                cwd = merge(moved, old.as_deref());
            } else if runs_unconditionally && moved.as_deref().is_some_and(Path::is_dir) {
                // Runs, and into a directory that exists: it takes effect.
                cwd = moved;
            } else {
                let skippable = prev == Some(Separator::Or);
                let p = Pending {
                    moved,
                    old,
                    skippable,
                    fallback: sep == Some(Separator::Or),
                };
                cwd = match sep {
                    Some(Separator::Or) => p.old.clone(),
                    Some(Separator::Sequence) => merge(p.moved.clone(), p.old.as_deref()),
                    _ if skippable => merge(p.moved.clone(), p.old.as_deref()),
                    _ => p.moved.clone(),
                };
                if matches!(sep, Some(Separator::Or | Separator::And) | None) {
                    pending = Some(p);
                }
            }
        } else {
            if moves_opaquely(&segment) {
                own = None;
                // A subshell `( … )` cannot move what follows it; anything else
                // (a brace group, a loop body, a function, `pushd`) can.
                if !segment.starts_with('(') {
                    cwd = None;
                    pending = None;
                }
            }
            // Leaving the `&&` chain of an uncertain `cd`: what comes next can
            // also be reached when that `cd` did not happen.
            if let Some(p) = pending.take() {
                match sep {
                    Some(Separator::And | Separator::Pipe) | None => pending = Some(p),
                    Some(Separator::Or | Separator::Sequence | Separator::Background) => {
                        cwd = p.either();
                    }
                }
            }
        }

        out.push(SegmentCwd { segment, cwd: own });
        match sep {
            Some(Separator::Background) => {
                cwd.clone_from(&list_base);
                pending = None;
            }
            Some(Separator::Sequence) => list_base.clone_from(&cwd),
            _ => {}
        }
        prev = sep;
    }
    (out, cwd)
}

/// One directory when both possibilities agree, otherwise unknown.
fn merge(a: Option<PathBuf>, b: Option<&Path>) -> Option<PathBuf> {
    if a.as_deref() == b { a } else { None }
}

/// A `cd x || exit` fallback: nothing after it runs unless the `cd` worked.
fn exits(segment: &str) -> bool {
    matches!(segment.split_whitespace().next(), Some("exit" | "return"))
}

/// A relative name `CDPATH` would look up (`./x` and `../x` bypass it).
fn is_cdpath_candidate(target: &str) -> bool {
    let t = target.trim_matches(['"', '\'']);
    !(t.starts_with('/')
        || t.starts_with('~')
        || t.starts_with("./")
        || t.starts_with("../")
        || t == "."
        || t == ".."
        || Path::new(t).is_absolute())
}

/// Whether a segment that is not a plain `cd <literal>` may still change the
/// directory: `pushd`, `popd`, `cd` after `do`/`then`/`builtin`/`command`, or
/// any of them inside a subshell, brace group, substitution or function body.
/// Deliberately broad — a false positive only makes the directory unknown.
fn moves_opaquely(segment: &str) -> bool {
    segment
        .split(|c: char| c.is_whitespace() || "(){};&|`".contains(c))
        .any(|word| matches!(word, "cd" | "pushd" | "popd" | "chdir"))
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

    fn cwds(command: &str) -> Vec<Option<PathBuf>> {
        segments_with_cwd(command, Some(Path::new("/proj")))
            .into_iter()
            .map(|s| s.cwd)
            .collect()
    }

    const MISSING: &str = "/lean-ctx-1850-does-not-exist";

    /// A `cd` runs where it starts: its own redirect is opened before it moves.
    #[test]
    fn a_cd_segment_runs_in_the_directory_before_it() {
        assert_eq!(cwds("cd /tmp && ls")[0], Some(PathBuf::from("/proj")));
    }

    /// #1850: `cd x && a; b` — if the `cd` fails, `a` is skipped but `b` still
    /// runs, in the old directory. Past the `;` both are possible.
    #[test]
    fn a_cd_that_may_fail_does_not_carry_past_a_sequence() {
        let got = cwds(&format!("cd {MISSING} && true; echo x"));
        assert_eq!(got[1], Some(PathBuf::from(MISSING)), "reached only via &&");
        assert_eq!(got[2], None);
    }

    #[test]
    fn a_cd_that_may_be_skipped_leaves_the_directory_unknown() {
        assert_eq!(cwds("false && cd /tmp; echo x")[2], None);
        assert_eq!(cwds("true || cd /tmp && echo x")[2], None);
        assert_eq!(cwds("cd /tmp | cat; echo x")[2], None, "a pipeline element");
    }

    /// `cd a && cd .` must not merge the two possibilities into one directory
    /// just because both `cd`s name the same place.
    #[test]
    fn a_second_cd_does_not_make_a_failed_first_one_certain() {
        assert_eq!(cwds(&format!("cd {MISSING} && cd . ; echo x"))[2], None);
    }

    #[test]
    fn cd_or_exit_leaves_only_the_moved_directory() {
        let got = cwds(&format!("cd {MISSING} || exit 1; echo x"));
        assert_eq!(got[1], Some(PathBuf::from("/proj")), "the fallback");
        assert_eq!(got[2], Some(PathBuf::from(MISSING)));
    }

    /// `&` runs the whole and-or list in a background subshell.
    #[test]
    fn a_backgrounded_list_does_not_move_what_follows() {
        assert_eq!(
            cwds("cd /tmp && echo a & echo b")[2],
            Some(PathBuf::from("/proj"))
        );
    }

    #[test]
    fn opaque_movers_make_the_directory_unknown() {
        for command in [
            "pushd /tmp && echo x",
            "builtin cd /tmp && echo x",
            "{ cd /tmp; } && echo x",
        ] {
            assert_eq!(cwds(command).last(), Some(&None), "{command}");
        }
        let subshell = cwds("(cd /tmp) && echo x");
        assert_eq!(subshell[0], None, "its own directory is not followed");
        assert_eq!(
            subshell[1],
            Some(PathBuf::from("/proj")),
            "but it cannot move the parent shell"
        );
    }

    #[test]
    fn cdpath_makes_a_relative_cd_unknown() {
        assert_eq!(cwds("CDPATH=/elsewhere; cd sub && ls")[2], None);
        assert_eq!(
            cwds("CDPATH=/elsewhere; cd ./sub && ls")[2],
            Some(PathBuf::from("/proj/sub")),
            "`./` bypasses CDPATH"
        );
    }
}
