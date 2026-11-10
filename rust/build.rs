// SPDX-License-Identifier: Apache-2.0

fn main() {
    guard_source_contamination();
    stamp_build_info();
    if validate_runtime_channels().is_err() {
        eprintln!(
            "BUILD BLOCKED: invalid or incomplete staging or production runtime channel policy"
        );
        std::process::exit(1);
    }
    // Watch the whole source tree: the contamination guard must re-run on any
    // source edit. Never point this at a file that may not exist — a missing
    // path marks the build script stale on every invocation and recompiles the
    // whole crate each time (it pointed at a deleted dashboard.html for months).
    println!("cargo::rerun-if-changed=src");
}

/// Fail-closed gate for the channel policy that
/// `core::intelligence_runtime::bootstrap` reads via literal `option_env!`:
/// each channel is either fully absent or complete and well-formed.
fn validate_runtime_channels() -> Result<(), ()> {
    const STAGING_NAMES: [&str; 3] = [
        "LEANCTX_STAGING_RUNTIME_CHANNEL_URL",
        "LEANCTX_STAGING_RUNTIME_CHANNEL_SIGNATURE_URL",
        "LEANCTX_STAGING_RUNTIME_CHANNEL_ROOT_KEY_HEX",
    ];
    const PRODUCTION_NAMES: [&str; 3] = [
        "LEANCTX_PRODUCTION_RUNTIME_CHANNEL_URL",
        "LEANCTX_PRODUCTION_RUNTIME_CHANNEL_SIGNATURE_URL",
        "LEANCTX_PRODUCTION_RUNTIME_CHANNEL_ROOT_KEY_HEX",
    ];
    validate_channel(STAGING_NAMES)?;
    validate_channel(PRODUCTION_NAMES)?;
    // The Cockpit catalog of a track is signed by that track's runtime root:
    // its URL pair is admitted only beside the complete runtime triple.
    validate_cockpit_channel(
        [
            "LEANCTX_STAGING_COCKPIT_CHANNEL_URL",
            "LEANCTX_STAGING_COCKPIT_CHANNEL_SIGNATURE_URL",
        ],
        STAGING_NAMES[2],
    )?;
    validate_cockpit_channel(
        [
            "LEANCTX_PRODUCTION_COCKPIT_CHANNEL_URL",
            "LEANCTX_PRODUCTION_COCKPIT_CHANNEL_SIGNATURE_URL",
        ],
        PRODUCTION_NAMES[2],
    )
}

fn validate_cockpit_channel(names: [&str; 2], root_key: &str) -> Result<(), ()> {
    for name in names {
        println!("cargo::rerun-if-env-changed={name}");
    }
    match names.map(std::env::var) {
        [
            Err(std::env::VarError::NotPresent),
            Err(std::env::VarError::NotPresent),
        ] => Ok(()),
        [Ok(url), Ok(signature)]
            if build_channel_url(&url)
                && build_channel_url(&signature)
                && std::env::var(root_key).is_ok() =>
        {
            Ok(())
        }
        _ => Err(()),
    }
}

fn validate_channel(names: [&str; 3]) -> Result<(), ()> {
    for name in names {
        println!("cargo::rerun-if-env-changed={name}");
    }
    let values = names.map(std::env::var);
    let absent = std::env::VarError::NotPresent;
    match &values {
        [Err(a), Err(b), Err(c)] if *a == absent && *b == absent && *c == absent => Ok(()),
        [Ok(url), Ok(signature), Ok(key)]
            if build_channel_url(url)
                && build_channel_url(signature)
                && key.len() == 64
                && key
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            Ok(())
        }
        _ => Err(()),
    }
}

fn build_channel_url(value: &str) -> bool {
    // No credentials or unbounded/unstructured build metadata is embedded.
    // The host additionally applies its canonical HTTP URI validator before use.
    value.len() <= 2048
        && value.bytes().all(|b| (33..127).contains(&b))
        && !value.contains(['@', '?', '#', '\\', '"'])
        && (value.starts_with("https://")
            || value.starts_with("http://127.0.0.1:")
            || value.starts_with("http://[::1]:"))
}

fn guard_source_contamination() {
    let src = std::path::Path::new("src");
    if !src.is_dir() {
        return;
    }
    let mut contaminated = Vec::new();
    visit_rs_files(src, &mut contaminated);
    if !contaminated.is_empty() {
        let list = contaminated.join(
            "
  ",
        );
        panic!(
            "

[1;31mBUILD BLOCKED: lean-ctx marker contamination detected[0m

             The following source files contain `--- lean-ctx:` lines injected
             by shell hooks during in-place editing. Remove them before building:

               {list}

             Prevention: use StrReplace (not perl/sed) for source edits, or set
             LEAN_CTX_SHELL_PASSTHROUGH=1 before running in-place edit commands.
"
        );
    }
}

fn visit_rs_files(dir: &std::path::Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs")
            && let Ok(text) = std::fs::read_to_string(&path)
            && text.lines().any(|line| line.starts_with("--- lean-ctx:"))
        {
            out.push(path.display().to_string());
        }
    }
}

/// Stamps which pipeline built this binary (`LEANCTX_BUILD_KIND`) and from
/// which commit (`LEANCTX_BUILD_COMMIT`, `LEANCTX_BUILD_DIRTY`) as compile-time
/// env for `core::integrity`. The binary never asks git at runtime.
///
/// - `official`: only the upstream release workflow on `main` or a release
///   tag ([`build_origin::release_commit`]).
/// - `dogfood` / `dev`: a git checkout; `dogfood` when `LEANCTX_BUILD_KIND`
///   says so or `HEAD` is the promoted `dogfood/instance` state.
/// - `source`: a packaged crate (crates.io) or no git checkout; the commit
///   comes from Cargo's `.cargo_vcs_info.json` when present.
///
/// `dirty` here covers the Engine's re-run triggers (its `src`, `HEAD`); the
/// `lean-ctx` binary stamps commit and dirty again over every crate's
/// sources (`crates/lean-ctx/build.rs`) and hands them to the Engine at
/// start, so the binary's label covers the whole `rust/` tree.
fn stamp_build_info() {
    for name in [
        "GITHUB_ACTIONS",
        "GITHUB_REPOSITORY",
        "GITHUB_EVENT_NAME",
        "GITHUB_WORKFLOW_REF",
        "GITHUB_REF",
        "GITHUB_SHA",
        "LEANCTX_BUILD_KIND",
    ] {
        println!("cargo::rerun-if-env-changed={name}");
    }
    let (kind, commit, dirty) = build_info();
    println!("cargo::rustc-env=LEANCTX_BUILD_KIND={kind}");
    println!(
        "cargo::rustc-env=LEANCTX_BUILD_COMMIT={}",
        commit.unwrap_or_default()
    );
    println!("cargo::rustc-env=LEANCTX_BUILD_DIRTY={}", u8::from(dirty));
}

#[path = "src/core/build_origin.rs"]
mod build_origin;
use build_origin::is_commit;

fn build_info() -> (&'static str, Option<String>, bool) {
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    if let Some(sha) = build_origin::release_commit(&env) {
        return ("official", Some(sha), false);
    }
    let requested = match env("LEANCTX_BUILD_KIND").as_deref() {
        None => None,
        Some("dev") => Some("dev"),
        Some("dogfood") => Some("dogfood"),
        Some(other) => panic!(
            "LEANCTX_BUILD_KIND={other:?}: only \"dev\" or \"dogfood\"; \
             \"official\" is set by the release workflow alone"
        ),
    };
    // A packaged crate (crates.io, `cargo package` verify) may sit inside an
    // unrelated git tree: its own VCS record wins over any surrounding repo.
    if let Ok(vcs) = std::fs::read_to_string(".cargo_vcs_info.json") {
        return (requested.unwrap_or("source"), vcs_info_sha1(&vcs), false);
    }
    if let Some(head) = git(&["rev-parse", "HEAD"]).filter(|s| is_commit(s)) {
        watch_git_head();
        let dirty = git(&[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=no",
            "--",
            ".",
        ])
        .is_some_and(|s| !s.is_empty());
        let promoted = git(&[
            "rev-parse",
            "-q",
            "--verify",
            "refs/remotes/origin/dogfood/instance",
        ])
        .is_some_and(|s| s == head)
            || git(&["symbolic-ref", "-q", "--short", "HEAD"]).as_deref() == Some("dogfood");
        let kind = requested.unwrap_or(if promoted { "dogfood" } else { "dev" });
        return (kind, Some(head), dirty);
    }
    (requested.unwrap_or("source"), None, false)
}

fn git(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Re-run when `HEAD` moves: the `HEAD` file and the branch ref it names
/// (loose or packed). Only existing paths are watched, so an unchanged tree
/// stays fresh. The index is not watched (every `git add` would rebuild the
/// Engine); the dirty flag is refreshed whenever the build script re-runs.
fn watch_git_head() {
    let mut paths = vec!["HEAD".to_owned(), "packed-refs".to_owned()];
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        paths.push(branch);
    }
    for path in paths {
        if let Some(abs) = git(&["rev-parse", "--path-format=absolute", "--git-path", &path])
            && std::path::Path::new(&abs).exists()
        {
            println!("cargo::rerun-if-changed={abs}");
        }
    }
}

/// `git.sha1` of Cargo's `.cargo_vcs_info.json` (a packaged source crate).
fn vcs_info_sha1(text: &str) -> Option<String> {
    let rest = &text[text.find("\"sha1\"")? + 6..];
    let start = rest.find('"')? + 1;
    let sha = rest[start..].get(..40)?;
    is_commit(sha).then(|| sha.to_owned())
}
