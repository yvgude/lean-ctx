// SPDX-License-Identifier: Apache-2.0
//! What the judged commit may not change against `spec_commit` because it
//! decides what the rerun runs, not what is tested, and what the gate
//! refuses outright because it would make the rerun no longer a run of the
//! commit.
//!
//! Refused when changed against `spec_commit`:
//! - any build script: the file a manifest's `package.build` names, or the
//!   default `build.rs` beside a `Cargo.toml` whose `build` is not `false`
//!   (a module file named `build.rs` that is not beside a manifest is not a
//!   build script, so it is left alone);
//! - the build-script and test-target keys of any manifest: `package.build`,
//!   `package.links`, `package.autotests`, `[lib]` `test`/`harness`/`path`,
//!   the `[[test]]` tables, `[build-dependencies]` and each
//!   `[target.<cfg>.build-dependencies]`, and the workspace selection
//!   (`workspace.members`/`default-members`/`exclude`, `package.workspace`);
//!   the refusal names the keys that changed (N5);
//! - any file of an in-tree path build-dependency, and of the in-tree path
//!   dependencies those crates use in turn (N3). Registry and git
//!   build-dependencies, and dependencies a build-dependency inherits with
//!   `workspace = true`, are not followed: their sources are not files of
//!   the judged commit, or come in through a manifest key compared above.
//!
//! Refused outright:
//! - a path dependency that resolves outside the run tree: its sources and
//!   build script are not in the judged commit, so the rerun would not be a
//!   run of it. Paths resolve through the symlinks of the commit's tree
//!   (mode `120000`), as the checkout will (R5-5);
//! - a symlink of the commit whose target is absolute or resolves outside
//!   the tree (R5-5);
//! - a git dependency on a local repository (`file:` or a bare path): it is
//!   not a file of the judged commit either (N4).
//!
//! The source closure of each build script (its `#[path]` modules, its
//! `include!`s) is handled after the run from cargo's dep-info
//! ([`super::report`]); [`refuse_changed_sources`] compares those blobs.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use super::{GateError, git, nul_fields, run_git};

/// Symlink hops one resolution may take (as the kernel's `MAXSYMLINKS`).
const MAX_HOPS: usize = 40;

/// The files of a commit: path → blob id, and the symlinks among them.
struct Tree {
    blobs: BTreeMap<String, String>,
    links: BTreeSet<String>,
}

/// Every file of `commit` in `git_dir`.
fn tree(git_dir: &Path, commit: &str) -> Result<Tree, GateError> {
    let mut cmd = git(git_dir);
    cmd.args(["ls-tree", "-r", "-z", "--full-tree", commit]);
    let mut out = Tree {
        blobs: BTreeMap::new(),
        links: BTreeSet::new(),
    };
    for field in nul_fields(&run_git(cmd, "ls-tree")?) {
        // `<mode> SP <type> SP <object> TAB <path>`
        let Some((meta, path)) = field.split_once('\t') else {
            continue;
        };
        let mut parts = meta.split(' ');
        if let (Some(mode), Some("blob"), Some(oid)) = (parts.next(), parts.next(), parts.next()) {
            if mode == "120000" {
                out.links.insert(path.to_owned());
            }
            out.blobs.insert(path.to_owned(), oid.to_owned());
        }
    }
    Ok(out)
}

/// Path → blob id of every file of `commit` in `git_dir`.
fn blob_ids(git_dir: &Path, commit: &str) -> Result<BTreeMap<String, String>, GateError> {
    Ok(tree(git_dir, commit)?.blobs)
}

fn blob(git_dir: &Path, oid: &str) -> Result<String, GateError> {
    let mut cmd = git(git_dir);
    cmd.args(["cat-file", "blob", oid]);
    Ok(String::from_utf8_lossy(&run_git(cmd, "cat-file")?).into_owned())
}

/// The symlinks of a commit: path → target.
fn link_targets(git_dir: &Path, tree: &Tree) -> Result<BTreeMap<String, String>, GateError> {
    let mut out = BTreeMap::new();
    for link in &tree.links {
        if let Some(oid) = tree.blobs.get(link) {
            out.insert(link.clone(), blob(git_dir, oid)?);
        }
    }
    Ok(out)
}

/// `path` (relative to the tree root) resolved through the tree's symlinks
/// `links`, as the checkout resolves it: `None` when it, or a symlink on
/// the way, is absolute, climbs above the root or takes more than
/// [`MAX_HOPS`] hops.
fn resolve(links: &BTreeMap<String, String>, path: &str) -> Option<String> {
    if path.starts_with('/') {
        return None;
    }
    let mut stack: Vec<String> = Vec::new();
    let mut todo: VecDeque<String> = path.split('/').map(str::to_owned).collect();
    let mut hops = 0;
    while let Some(part) = todo.pop_front() {
        match part.as_str() {
            "" | "." => {}
            ".." => {
                stack.pop()?;
            }
            name => {
                stack.push(name.to_owned());
                if let Some(target) = links.get(&stack.join("/")) {
                    hops += 1;
                    if hops > MAX_HOPS || target.starts_with('/') {
                        return None;
                    }
                    stack.pop();
                    for next in target.split('/').rev() {
                        todo.push_front(next.to_owned());
                    }
                }
            }
        }
    }
    Some(stack.join("/"))
}

fn is_manifest(path: &str) -> bool {
    path == "Cargo.toml" || path.ends_with("/Cargo.toml")
}

/// The directory of a manifest path (`""` for the root).
fn dir_of(manifest: &str) -> &str {
    manifest.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn join(dir: &str, file: &str) -> String {
    if dir.is_empty() {
        file.to_owned()
    } else {
        format!("{dir}/{file}")
    }
}

/// The dependencies of a manifest the gate looks at.
#[derive(Default)]
struct Deps {
    /// Every `path = "..."`, joined to the manifest's directory (absolute
    /// paths kept as given).
    paths: Vec<String>,
    /// The `path` build-dependencies (top level and per target).
    build: Vec<String>,
    /// The `path` normal dependencies (top level and per target): what a
    /// build-dependency crate compiles in turn.
    normal: Vec<String>,
    /// Git dependencies on a local repository.
    local_git: Vec<String>,
}

/// What a manifest says about build scripts and test targets, as
/// key → text to compare; the build script it uses, if any; and its
/// dependencies.
struct Selection {
    keys: BTreeMap<String, String>,
    build_script: Option<String>,
    deps: Deps,
}

fn selection(
    manifest: &str,
    text: Option<&str>,
    files: &BTreeMap<String, String>,
) -> Result<Selection, GateError> {
    let present = text.is_some();
    let value: toml::Table = toml::from_str(text.unwrap_or("")).map_err(|e| {
        GateError::Check(format!(
            "{manifest} does not parse, so the gate cannot tell what it builds and tests: {e}"
        ))
    })?;
    let table = |key: &str| value.get(key).and_then(toml::Value::as_table);
    let package = table("package");
    let lib = table("lib");
    let workspace = table("workspace");
    let pick = |t: Option<&toml::Table>, key: &str| {
        t.and_then(|t| t.get(key))
            .map_or_else(String::new, ToString::to_string)
    };
    let show = |key: &str| value.get(key).map_or_else(String::new, ToString::to_string);
    let mut keys: BTreeMap<String, String> = [
        ("package.build", pick(package, "build")),
        ("package.links", pick(package, "links")),
        ("package.autotests", pick(package, "autotests")),
        ("package.workspace", pick(package, "workspace")),
        ("lib.test", pick(lib, "test")),
        ("lib.harness", pick(lib, "harness")),
        ("lib.path", pick(lib, "path")),
        ("workspace.members", pick(workspace, "members")),
        (
            "workspace.default-members",
            pick(workspace, "default-members"),
        ),
        ("workspace.exclude", pick(workspace, "exclude")),
        ("test", show("test")),
        ("build-dependencies", show("build-dependencies")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    // Only the build-dependencies of a target decide what a build script
    // compiles; its other dependency tables are code like any other (N5).
    if let Some(targets) = table("target") {
        for (cfg, body) in targets {
            if let Some(build) = body.get("build-dependencies") {
                keys.insert(
                    format!("target.{cfg}.build-dependencies"),
                    build.to_string(),
                );
            }
        }
    }
    let dir = dir_of(manifest);
    let build_script = if present {
        match package.and_then(|p| p.get("build")) {
            Some(toml::Value::String(path)) => Some(join(dir, path.trim_start_matches("./"))),
            Some(toml::Value::Boolean(false)) => None,
            _ => {
                let default = join(dir, "build.rs");
                files.contains_key(&default).then_some(default)
            }
        }
    } else {
        None
    };
    let deps = dependencies(&value, dir);
    Ok(Selection {
        keys,
        build_script,
        deps,
    })
}

/// The dependencies of every dependency table of a manifest in `dir`.
fn dependencies(value: &toml::Table, dir: &str) -> Deps {
    let mut out = Deps::default();
    let mut tables: Vec<(&str, &toml::Table)> = Vec::new();
    for name in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(deps) = value.get(name).and_then(toml::Value::as_table) {
            tables.push((name, deps));
        }
    }
    // `[target.<cfg>.dependencies]` and friends.
    if let Some(targets) = value.get("target").and_then(toml::Value::as_table) {
        for cfg in targets.values().filter_map(toml::Value::as_table) {
            for name in ["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(deps) = cfg.get(name).and_then(toml::Value::as_table) {
                    tables.push((name, deps));
                }
            }
        }
    }
    // `[workspace.dependencies]`, `[patch.<source>]` and `[replace]`.
    if let Some(deps) = value
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(toml::Value::as_table)
    {
        tables.push(("workspace", deps));
    }
    if let Some(patches) = value.get("patch").and_then(toml::Value::as_table) {
        for deps in patches.values().filter_map(toml::Value::as_table) {
            tables.push(("patch", deps));
        }
    }
    if let Some(deps) = value.get("replace").and_then(toml::Value::as_table) {
        tables.push(("replace", deps));
    }
    for (name, deps) in tables {
        for spec in deps.values().filter_map(toml::Value::as_table) {
            if let Some(path) = spec.get("path").and_then(toml::Value::as_str) {
                let path = if path.starts_with('/') {
                    path.to_owned()
                } else {
                    join(dir, path)
                };
                match name {
                    "build-dependencies" => out.build.push(path.clone()),
                    "dependencies" => out.normal.push(path.clone()),
                    _ => {}
                }
                out.paths.push(path);
            }
            if let Some(url) = spec.get("git").and_then(toml::Value::as_str)
                && is_local_git(url)
            {
                out.local_git.push(url.to_owned());
            }
        }
    }
    out
}

/// A git URL that names a repository on this machine: `file:` or a bare
/// path (cargo takes no scp-like form, so anything without `://` is one).
fn is_local_git(url: &str) -> bool {
    let url = url.trim();
    url.get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
        || !url.contains("://")
}

/// The keys of `was` and `is` whose values differ.
fn changed_keys(was: &BTreeMap<String, String>, is: &BTreeMap<String, String>) -> Vec<String> {
    let names: BTreeSet<&String> = was.keys().chain(is.keys()).collect();
    names
        .into_iter()
        .filter(|k| was.get(*k) != is.get(*k))
        .cloned()
        .collect()
}

/// Refused: the judged commit `artifact` changes, against `spec`, a build
/// script, the build-script, test-target or workspace keys of a manifest,
/// or a file of an in-tree path build-dependency; or it carries a path
/// dependency, a symlink or a git dependency outside the run tree.
pub(super) fn refuse_build_changes(
    git_dir: &Path,
    spec: &str,
    artifact: &str,
) -> Result<(), GateError> {
    let then_tree = tree(git_dir, spec)?;
    let now_tree = tree(git_dir, artifact)?;
    let links = link_targets(git_dir, &now_tree)?;
    let (then, now) = (&then_tree.blobs, &now_tree.blobs);
    let manifests: BTreeSet<&String> = then
        .keys()
        .chain(now.keys())
        .filter(|p| is_manifest(p))
        .collect();
    let mut scripts: BTreeSet<String> = BTreeSet::new();
    let mut changed = Vec::new();
    let mut escaping: BTreeSet<String> = links
        .iter()
        .filter(|(link, _)| resolve(&links, link).is_none())
        .map(|(link, target)| format!("{link} (symlink to {target})"))
        .collect();
    let mut build_dirs: VecDeque<String> = VecDeque::new();
    let mut normal: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for manifest in manifests {
        let read = |ids: &BTreeMap<String, String>| {
            ids.get(manifest).map(|oid| blob(git_dir, oid)).transpose()
        };
        let (before, after) = (read(then)?, read(now)?);
        let was = selection(manifest, before.as_deref(), then)?;
        let is = selection(manifest, after.as_deref(), now)?;
        let keys = changed_keys(&was.keys, &is.keys);
        if before != after && !keys.is_empty() {
            changed.push(format!(
                "{manifest} (build or test targets: {})",
                keys.join(", ")
            ));
        }
        scripts.extend(was.build_script);
        scripts.extend(is.build_script);
        // A path dependency outside the tree is refused whether or not it
        // changed: the rerun would not be a run of the commit (N2, R5-5).
        for dep in &is.deps.paths {
            if resolve(&links, dep).is_none() {
                escaping.insert(dep.clone());
            }
        }
        escaping.extend(is.deps.local_git.iter().map(|url| format!("git {url}")));
        for dep in was.deps.build.iter().chain(&is.deps.build) {
            build_dirs.extend(resolve(&links, dep));
        }
        let dir = dir_of(manifest).to_owned();
        normal
            .entry(dir)
            .or_default()
            .extend(is.deps.normal.iter().filter_map(|dep| resolve(&links, dep)));
    }
    if !escaping.is_empty() {
        return Err(GateError::Check(format!(
            "the judged commit depends on paths outside the run tree ({}); the rerun would not be a run of the commit, so the gate refuses it",
            escaping.into_iter().collect::<Vec<_>>().join(", ")
        )));
    }
    for script in scripts {
        if then.get(&script) != now.get(&script) {
            changed.push(format!("{script} (build script)"));
        }
    }
    // N3: the in-tree crates a build script compiles, and the in-tree path
    // dependencies of those in turn.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    while let Some(dir) = build_dirs.pop_front() {
        if !seen.insert(dir.clone()) {
            continue;
        }
        if let Some(next) = normal.get(&dir) {
            build_dirs.extend(next.iter().cloned());
        }
    }
    for dir in &seen {
        let inside = |path: &&String| dir.is_empty() || path.starts_with(&format!("{dir}/"));
        let files: BTreeSet<&String> = then.keys().chain(now.keys()).filter(inside).collect();
        for file in files {
            if then.get(file) != now.get(file) {
                changed.push(format!("{file} (build-dependency source)"));
            }
        }
    }
    if changed.is_empty() {
        return Ok(());
    }
    Err(GateError::Check(format!(
        "the judged commit changes {} against spec_commit; build scripts and test targets decide what the rerun runs, so the gate does not run it (a person judges that change)",
        changed.join(", ")
    )))
}

/// Refused: any of `repo_paths` (relative to the tree root) differs in
/// content between `spec` and `artifact`. Used after the run for the
/// in-tree sources a build script compiled (its `#[path]` modules and
/// `include!`s), which the manifest cannot name.
pub(super) fn refuse_changed_sources(
    git_dir: &Path,
    spec: &str,
    artifact: &str,
    repo_paths: &[String],
) -> Result<(), GateError> {
    if repo_paths.is_empty() {
        return Ok(());
    }
    let then = blob_ids(git_dir, spec)?;
    let now = blob_ids(git_dir, artifact)?;
    let changed: Vec<&String> = repo_paths
        .iter()
        .filter(|p| then.get(*p) != now.get(*p))
        .collect();
    if changed.is_empty() {
        return Ok(());
    }
    Err(GateError::Check(format!(
        "the gate's run compiled build-script sources the judged commit changed against spec_commit ({}); they decide what the rerun runs, so it does not count",
        changed
            .into_iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect()
    }

    #[test]
    fn symlinks_resolve_as_the_checkout_does() {
        let l = links(&[("d", "."), ("x", "sub/deep"), ("loop", "loop")]);
        assert_eq!(resolve(&l, "x/.."), Some("sub".into()));
        assert_eq!(resolve(&l, "d/x"), Some("sub/deep".into()));
        // Lexically `d/..` is the root; through the link it is its parent.
        assert_eq!(resolve(&l, "d/.."), None);
        assert_eq!(resolve(&l, "../a"), None);
        assert_eq!(resolve(&l, "/abs"), None);
        assert_eq!(resolve(&l, "loop"), None);
        assert_eq!(resolve(&links(&[("y", "/etc")]), "y/passwd"), None);
    }

    #[test]
    fn local_git_urls_are_recognised() {
        assert!(is_local_git("file:///tmp/repo"));
        assert!(is_local_git("FILE:/tmp/repo"));
        assert!(is_local_git("/tmp/repo"));
        assert!(is_local_git("../repo"));
        assert!(!is_local_git("https://example.com/repo.git"));
        assert!(!is_local_git("ssh://git@example.com/repo.git"));
    }
}
