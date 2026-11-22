// SPDX-License-Identifier: Apache-2.0
//! The compiler's own list of the locked tests at `spec_commit` (G6,
//! review r8 alternative C): what libtest itself lists, so a test the
//! allowlist parser in `gate_report.rs` cannot name is still required.
//!
//! Before the judged run, the gate writes the whole tree of `spec_commit`
//! into a fresh run directory of its own and runs the declared command
//! there, with the gate's environment, its own `CARGO_TARGET_DIR` (never
//! the judged run's: dep-info left there would fake a build) and its own
//! deadline. Cargo's target runner for the host
//! (`CARGO_TARGET_<TRIPLE>_RUNNER`) is a lister the gate writes: for every
//! test binary cargo would run it records the binary's path and
//! `<binary> <args> --list --format terse` (the command's own filters
//! kept, `--format`, `-Z`, `--quiet` and `--list` dropped) and runs no
//! test. `RUSTDOC` is a wrapper the gate writes too: a doc-test run
//! (`rustdoc --test`, which runs past the target runner) does nothing.
//! A test target with `harness = false` is not libtest: it may ignore
//! `--list` and run (its code runs in the judged run anyway). The
//! listing's output goes to its own log; the judged run's log gets one
//! line about it.
//!
//! A listed test is kept when the binary that lists it was built (its own
//! dep-info) from a locked `*.rs` file `F` and from a target root of `F`'s
//! package that mounts `F` by a plain `mod` chain, and its path lies under
//! that mount but not under a child file module of `F` (`mod x;`, each
//! with its own file and so its own attribution). The gate keeps the path
//! relative to that mount: the judged commit may mount `F` elsewhere (a
//! `#[path]` mount under another module name), so in the judged tree the
//! path gets `F`'s mount there, the one the parser binds to (no plain
//! mount: `locked_test_not_run`). The gate adds those paths to the tests
//! the parser names for `F` (union), and checks them first: every one must
//! be reported `passed` by a binary of the judged run built from `F`, else
//! `locked_test_not_run`. The parser's checks
//! stay: the compiler list cannot tell a hollow test at the same path from
//! the locked one (r9 §4), the plain-mount binding and the allowlist can.
//!
//! **When there is no list.** A `spec_commit` that does not build (common
//! in test-first work: the locked tests call what the task adds) lists
//! nothing; then the parser alone decides, as before. Whatever binaries
//! did list are used: each listed path is the compiler's, so a partial
//! list only adds requirements. `spec_commit` decides this, not the judged
//! commit. Only the host triple gets the lister: a command with
//! `--target` for another triple runs its binaries unlisted.
//!
//! **Cost.** The listing builds every test binary of the command once
//! more, from scratch, in its own target directory: about one cold build
//! of the command per G6 run on top of the judged run (measured numbers:
//! `CHANGELOG.md`, 3.12.3). Dep-info is read from any profile
//! directory of that target (`--profile`, `--target`); a list whose
//! binaries have none is logged as unavailable, never as zero tests.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use super::GateError;
use super::report::{
    bin_sources, locked_rs_files, normalize, package_of, plain_mounts, target_roots, was_built,
};
use super::run::{cargo_run_dir, make_private_dir, run_command, run_env};
use super::tree::export_tree;

/// The compiler's list at `spec_commit`: per locked file, the paths of the
/// tests attributed to it relative to its mount there (`refunds`,
/// `inner::refunds`), and a one-line note for the run log.
pub(super) struct SpecList {
    pub tests: BTreeMap<String, BTreeSet<String>>,
    pub note: String,
}

/// List the tests of `spec` (in the private copy `git_dir`) under the
/// declared command of `inputs`, in a fresh run directory under `runs`,
/// with the target, temp files and log under `scratch`.
pub(super) fn spec_list(
    git_dir: &Path,
    spec: &str,
    runs: &Path,
    scratch: &Path,
    work: &Path,
    command: &str,
    locked: &[String],
    timeout: Duration,
) -> Result<SpecList, GateError> {
    let started = Instant::now();
    let run_dir = cargo_run_dir(runs)?;
    let tree = run_dir.path().join("tree");
    make_private_dir(&tree)?;
    export_tree(git_dir, spec, &tree, &|_| true)?;
    let dir = scratch.join("list");
    let lists = dir.join("lists");
    make_private_dir(&lists)?;
    let mut env = run_env(&dir, work)?;
    let target = dir.join("target");
    let deadline = started + timeout;

    let mut log = std::fs::File::create(dir.join("list.log"))
        .map_err(|e| GateError::Check(format!("the list log: {e}")))?;
    let Some(triple) = host_triple(&tree, &env, &dir, deadline)? else {
        return Ok(unavailable("rustc -vV named no host"));
    };
    let lister = dir.join("lister.sh");
    write_lister(&lister, &lists)?;
    let Some(lister) = launcher(&lister)? else {
        return Ok(unavailable("the gate's list directory holds whitespace"));
    };
    env.push((runner_var(&triple), lister.into_os_string()));
    // Doc-tests: rustdoc builds and runs them itself, past the runner.
    let rustdoc = dir.join("rustdoc.sh");
    write_rustdoc(&rustdoc)?;
    let Some(rustdoc) = launcher(&rustdoc)? else {
        return Ok(unavailable("the gate's list directory holds whitespace"));
    };
    env.push(("RUSTDOC".into(), rustdoc.clone().into_os_string()));
    env.push(("CARGO_BUILD_RUSTDOC".into(), rustdoc.into_os_string()));
    let exit = run_command(&tree, command, &env, &mut log, deadline)?;
    let _ = writeln!(log, "\n[G6: listing exited {exit}]");

    let mut tests: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut binaries = 0usize;
    let mut resolved = 0usize;
    for (bin, names) in read_lists(&lists) {
        binaries += 1;
        let sources = bin_sources(&target, &bin);
        if sources.is_empty() {
            let _ = writeln!(log, "[G6: no dep-info for the listed binary {bin}]");
            continue;
        }
        resolved += 1;
        for (file, paths) in attribute(&tree, locked, &sources, &names) {
            tests.entry(file).or_default().extend(paths);
        }
    }
    if binaries == 0 {
        return Ok(unavailable(&format!(
            "the command at spec_commit listed no test binary (exit {exit})"
        )));
    }
    if resolved == 0 {
        return Ok(unavailable(&format!(
            "none of the {binaries} listed test binaries has dep-info in the gate's target directory"
        )));
    }
    let count: usize = tests.values().map(BTreeSet::len).sum();
    let unresolved = binaries - resolved;
    let note = format!(
        "[G6: compiler list at spec_commit: {count} locked tests from {resolved} test binaries ({unresolved} without dep-info), exit {exit}, in {} s]",
        started.elapsed().as_secs()
    );
    Ok(SpecList { tests, note })
}

fn unavailable(why: &str) -> SpecList {
    SpecList {
        tests: BTreeMap::new(),
        note: format!(
            "[G6: compiler list at spec_commit unavailable: {why}; the parser alone names the locked tests]"
        ),
    }
}

/// The host triple of the toolchain the command runs in `tree` (its
/// `rust-toolchain` file applies), from `rustc -vV`.
fn host_triple(
    tree: &Path,
    env: &[(String, std::ffi::OsString)],
    dir: &Path,
    deadline: Instant,
) -> Result<Option<String>, GateError> {
    let path = dir.join("rustc-vv.txt");
    let mut out =
        std::fs::File::create(&path).map_err(|e| GateError::Check(format!("the list log: {e}")))?;
    if run_command(tree, "rustc -vV", env, &mut out, deadline)? != 0 {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| GateError::Check(format!("the list log: {e}")))?;
    Ok(text
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(str::trim)
        .filter(|t| {
            !t.is_empty()
                && t.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
        .map(str::to_owned))
}

/// Cargo's environment key for the target runner of `triple`.
fn runner_var(triple: &str) -> String {
    format!(
        "CARGO_TARGET_{}_RUNNER",
        triple.to_ascii_uppercase().replace(['-', '.'], "_")
    )
}

/// The lister cargo runs instead of each test binary: one file per binary
/// in `lists` (its path, then its terse list), nothing on stdout, exit 0
/// unless the binary could not list.
fn write_lister(file: &Path, lists: &Path) -> Result<(), GateError> {
    // Git for Windows' sh takes `C:/x/y`; a backslash stays literal in quotes.
    let quoted = lists.to_str().filter(|s| !s.contains('\'')).map(|s| {
        if cfg!(windows) {
            s.replace('\\', "/")
        } else {
            s.to_owned()
        }
    });
    let Some(lists) = quoted else {
        return Err(GateError::Check(format!(
            "the gate's list directory {} cannot be quoted",
            lists.display()
        )));
    };
    let script = format!(
        r#"#!/bin/sh
bin=$1
shift
n=$#
while [ "$n" -gt 0 ]; do
  a=$1
  shift
  n=$((n - 1))
  case $a in
    --format|-Z) if [ "$n" -gt 0 ]; then shift; n=$((n - 1)); fi ;;
    --format=*|-Z*|-q|--quiet|--list) ;;
    *) set -- "$@" "$a" ;;
  esac
done
i=0
while :; do
  out='{lists}'/list.$$.$i
  ( set -C; : > "$out" ) 2>/dev/null && break
  i=$((i + 1))
done
printf '%s\n' "$bin" > "$out" || exit 1
"$bin" "$@" --list --format terse >> "$out" || exit 1
"#
    );
    write_script(file, &script)
}

/// The `rustdoc` cargo runs while listing: a doc-test run (`--test`, which
/// would build and run the doc-tests of `spec_commit` itself, past the
/// target runner) does nothing and succeeds; anything else (`-vV`) goes to
/// the toolchain's `rustdoc`. Doc-tests are never locked tests here.
fn write_rustdoc(file: &Path) -> Result<(), GateError> {
    write_script(
        file,
        r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = --test ]; then exit 0; fi
done
exec rustdoc "$@"
"#,
    )
}

/// What cargo runs for the gate's shell script `script`: the script itself
/// on Unix; on Windows a `.cmd` beside it that hands it to the gate's `sh`
/// (cargo cannot start a `.sh`). Cargo splits a runner on whitespace, so a
/// path with whitespace gives `None`.
fn launcher(script: &Path) -> Result<Option<std::path::PathBuf>, GateError> {
    if script.to_string_lossy().contains(char::is_whitespace) {
        return Ok(None);
    }
    if cfg!(not(windows)) {
        return Ok(Some(script.to_path_buf()));
    }
    let sh = super::run::shell()?;
    let name = script
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cmd = script.with_extension("cmd");
    write_script(
        &cmd,
        &format!(
            "@\"{}\" \"%~dp0{name}\" %*\r\n@exit /b %ERRORLEVEL%\r\n",
            sh.display()
        ),
    )?;
    Ok(Some(cmd))
}

/// Write the gate's own shell script `file`, private and executable.
fn write_script(file: &Path, script: &str) -> Result<(), GateError> {
    let what = |e: std::io::Error| GateError::Check(format!("{}: {e}", file.display()));
    std::fs::write(file, script).map_err(what)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700)).map_err(what)?;
    }
    Ok(())
}

/// Every list the lister wrote: the binary and the test paths it listed
/// (`<path>: test`).
fn read_lists(lists: &Path) -> Vec<(String, Vec<String>)> {
    let Ok(entries) = std::fs::read_dir(lists) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Vec<String>)> = entries
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|text| {
            let mut lines = text.lines();
            let bin = lines.next()?.to_owned();
            let names = lines
                .filter_map(|l| l.strip_suffix(": test"))
                .map(str::to_owned)
                .collect();
            Some((bin, names))
        })
        .collect();
    out.sort();
    out
}

/// The listed `names` of one binary built from `sources` (its own
/// dep-info), per locked file of `tree` they belong to: the file was built
/// into the binary, a target root of its package that was built into it
/// mounts the file by a plain chain at a prefix of the name, and the name
/// is not under a child file module of the file. Each is kept relative to
/// that mount.
pub(super) fn attribute(
    tree: &Path,
    locked: &[String],
    sources: &BTreeSet<String>,
    names: &[String],
) -> BTreeMap<String, BTreeSet<String>> {
    let mut out = BTreeMap::new();
    for file in locked_rs_files(tree, locked) {
        if !was_built(tree, &file, sources) {
            continue;
        }
        let Some(package) = package_of(tree, &file) else {
            continue;
        };
        let mounts: BTreeSet<String> = target_roots(tree, &package)
            .iter()
            .filter(|root| was_built(tree, &root.to_string_lossy(), sources))
            .flat_map(|root| plain_mounts(tree, root, Path::new(&file)))
            .collect();
        if mounts.is_empty() {
            continue;
        }
        let children = child_files(tree, &file);
        let mine: BTreeSet<String> = names
            .iter()
            .filter_map(|name| {
                mounts.iter().find_map(|m| {
                    name.strip_prefix(m.as_str())
                        .filter(|rest| !children.iter().any(|c| rest.starts_with(c.as_str())))
                })
            })
            .map(str::to_owned)
            .collect();
        if !mine.is_empty() {
            out.insert(file, mine);
        }
    }
    out
}

/// The module paths, relative to the file `rel`, of its child modules
/// that live in a file of their own (`mod x;`, `#[path]` or not, at item
/// level of the file or of its inline modules): `x::`, `a::x::`.
fn child_files(tree: &Path, rel: &str) -> Vec<String> {
    fn walk(items: &[syn::Item], prefix: &str, out: &mut Vec<String>) {
        use syn::ext::IdentExt as _;
        for item in items {
            let syn::Item::Mod(m) = item else {
                continue;
            };
            let path = format!("{prefix}{}::", m.ident.unraw());
            match &m.content {
                // Inline content is this file's, `#[path]` or not: the
                // attribute only moves where its `mod x;` children live,
                // and those are excluded on their own.
                Some((_, inner)) => walk(inner, &path, out),
                None => out.push(path),
            }
        }
    }
    let mut out = Vec::new();
    let file = normalize(Path::new(rel));
    if let Some(parsed) = std::fs::read_to_string(tree.join(file))
        .ok()
        .and_then(|t| syn::parse_file(&t).ok())
    {
        walk(&parsed.items, "", &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which listed paths belong to which locked file: under its plain
    /// mount (a root's is ""), not under a child file module, and only when
    /// the binary was built from the file and from a root that mounts it.
    #[test]
    fn listed_paths_are_attributed_to_the_locked_file_that_holds_them() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path();
        for (rel, text) in [
            ("Cargo.toml", "[package]\nname = \"p\"\n"),
            ("src/lib.rs", "mod a;\nmod other;\n"),
            (
                "src/a.rs",
                "mod sub;\nmod inl {\n    mod deep;\n}\n#[path = \"moved\"]\nmod pinl {\n    mod far;\n}\n",
            ),
            ("src/a/sub.rs", ""),
            ("src/a/inl/deep.rs", ""),
            ("src/other.rs", ""),
            ("tests/it.rs", "mod helper;\n"),
            ("tests/helper.rs", ""),
        ] {
            let p = tree.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        let abs = |rels: &[&str]| -> BTreeSet<String> {
            rels.iter()
                .map(|r| tree.join(r).to_string_lossy().into_owned())
                .collect()
        };
        let strings = |v: &[&str]| -> Vec<String> { v.iter().map(|s| (*s).to_owned()).collect() };
        let locked = strings(&["src/a.rs", "tests/it.rs"]);
        let lib = [
            "a::t",
            "a::made_by_a_macro",
            "a::inl::x",
            "a::sub::s",
            "a::inl::deep::d",
            "a::pinl::made",
            "a::pinl::far::f",
            "other::o",
        ];
        // (why, the binary's dep-info, its listed paths, file → kept paths)
        type Case<'a> = (
            &'a str,
            BTreeSet<String>,
            &'a [&'a str],
            &'a [(&'a str, &'a [&'a str])],
        );
        let cases: [Case; 4] = [
            (
                "a.rs keeps its own and its inline modules' paths (relative to its mount, `#[path]` inline too), not its file modules'",
                abs(&["src/lib.rs", "src/a.rs", "src/a/sub.rs"]),
                &lib,
                &[(
                    "src/a.rs",
                    &["inl::x", "made_by_a_macro", "pinl::made", "t"],
                )],
            ),
            (
                "a test root keeps every path but its file modules'",
                abs(&["tests/it.rs", "tests/helper.rs"]),
                &["it", "m::inline", "helper::h"],
                &[("tests/it.rs", &["it", "m::inline"])],
            ),
            (
                "a binary not built from the locked file",
                abs(&["src/lib.rs", "src/other.rs"]),
                &lib,
                &[],
            ),
            (
                "built from the file, but from no root that mounts it",
                abs(&["src/a.rs"]),
                &lib,
                &[],
            ),
        ];
        for (why, sources, names, want) in cases {
            let want: BTreeMap<String, BTreeSet<String>> = want
                .iter()
                .map(|(f, n)| ((*f).to_owned(), strings(n).into_iter().collect()))
                .collect();
            assert_eq!(
                attribute(tree, &locked, &sources, &strings(names)),
                want,
                "{why}"
            );
        }
    }

    /// The lister drops the output and unstable options (`--format`, `-Z`,
    /// `--quiet`, `--list`, both spellings) and keeps the command's filters
    /// and the rest in order, then lists in terse format; it prints nothing.
    /// The rustdoc wrapper turns a doc-test run into a no-op.
    #[cfg(unix)]
    #[test]
    fn the_lister_keeps_filters_and_drops_output_options() {
        let dir = tempfile::tempdir().unwrap();
        let lists = dir.path().join("lists");
        std::fs::create_dir(&lists).unwrap();
        let lister = dir.path().join("lister.sh");
        write_lister(&lister, &lists).unwrap();
        // A stand-in test binary that lists its own arguments.
        let bin = dir.path().join("bin.sh");
        write_script(&bin, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        let out = std::process::Command::new(&lister)
            .arg(&bin)
            .args([
                "--format",
                "json",
                "-Zunstable-options",
                "--quiet",
                "-q",
                "--format=pretty",
                "-Z",
                "unstable-options",
                "--list",
                "--skip",
                "slow",
                "--exact",
                "refund_tests::refunds",
                "--ignored",
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert!(out.stdout.is_empty(), "{out:?}");
        let files: Vec<_> = std::fs::read_dir(&lists).unwrap().flatten().collect();
        assert_eq!(files.len(), 1);
        let text = std::fs::read_to_string(files[0].path()).unwrap();
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some(bin.to_str().unwrap()));
        assert_eq!(
            lines.collect::<Vec<_>>(),
            [
                "--skip",
                "slow",
                "--exact",
                "refund_tests::refunds",
                "--ignored",
                "--list",
                "--format",
                "terse"
            ]
        );

        let rustdoc = dir.path().join("rustdoc.sh");
        write_rustdoc(&rustdoc).unwrap();
        let out = std::process::Command::new(&rustdoc)
            .args(["--edition", "2021", "--test", "src/lib.rs"])
            .output()
            .unwrap();
        assert!(out.status.success() && out.stdout.is_empty(), "{out:?}");
    }
}
