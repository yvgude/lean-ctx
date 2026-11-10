// SPDX-License-Identifier: Apache-2.0
//! Stamps the binary's commit and dirty flag over every crate it links
//! (`LEANCTX_BINARY_COMMIT`, `LEANCTX_BINARY_DIRTY`); `main` hands them to
//! `core::integrity::set_binary_stamp`. The Engine stamps the build kind
//! (`official` / `dogfood` / `dev` / `source`) itself, but its build script
//! re-runs on Engine sources only: an edit in `crates/` after a clean build
//! would otherwise still read clean.
//!
//! This package rebuilds whenever any crate it links changes, so watching
//! their source trees here costs no extra Engine build. The git index is not
//! watched: `git add` or an index refresh changes no source.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn main() {
    let (commit, dirty) = stamp();
    println!("cargo::rustc-env=LEANCTX_BINARY_COMMIT={commit}");
    println!("cargo::rustc-env=LEANCTX_BINARY_DIRTY={}", u8::from(dirty));
}

fn stamp() -> (String, bool) {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    // A packaged crate keeps the Engine's stamp (its `.cargo_vcs_info.json`),
    // even when unpacked inside an unrelated git tree.
    println!("cargo::rerun-if-changed=build.rs");
    if manifest.join(".cargo_vcs_info.json").exists() {
        return (String::new(), false);
    }
    let workspace = manifest.join("../..");
    let git = |args: &[&str]| git(&workspace, args);
    let Some(head) = git(&["rev-parse", "HEAD"]).filter(|s| is_commit(s)) else {
        return (String::new(), false);
    };
    watch_sources(&workspace);
    let mut refs = vec!["HEAD".to_owned(), "packed-refs".to_owned()];
    refs.extend(git(&["symbolic-ref", "-q", "HEAD"]));
    for path in refs {
        if let Some(abs) = git(&["rev-parse", "--path-format=absolute", "--git-path", &path])
            && Path::new(&abs).exists()
        {
            println!("cargo::rerun-if-changed={abs}");
        }
    }
    // Tracked files of the Rust workspace only: what the binary is built from.
    let dirty = git(&[
        "--no-optional-locks",
        "status",
        "--porcelain",
        "--untracked-files=no",
        "--",
        ".",
    ])
    .is_some_and(|s| !s.is_empty());
    (head, dirty)
}

/// The workspace manifests, the Engine's `src`, and `src` of every crate
/// under `crates/` (one or two levels deep, e.g. `crates/vendor/rmcp`).
fn watch_sources(workspace: &Path) {
    let mut paths = vec![
        workspace.join("Cargo.toml"),
        workspace.join("Cargo.lock"),
        workspace.join("build.rs"),
        workspace.join("src"),
    ];
    let crates = workspace.join("crates");
    for dir in subdirs(&crates) {
        if dir.join("Cargo.toml").exists() {
            paths.push(dir.join("Cargo.toml"));
            paths.push(dir.join("src"));
        } else {
            for nested in subdirs(&dir) {
                paths.push(nested.join("Cargo.toml"));
                paths.push(nested.join("src"));
            }
        }
    }
    // Never watch a missing path: Cargo would re-run the script every build.
    for path in paths.into_iter().filter(|p| p.exists()) {
        println!("cargo::rerun-if-changed={}", path.display());
    }
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn is_commit(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
