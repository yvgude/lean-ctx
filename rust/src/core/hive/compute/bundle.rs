// SPDX-License-Identifier: Apache-2.0
//! The input bundle of a compute job: exactly the files the job names from
//! this checkout, as one deterministic, content-addressed tar.
//!
//! Rules, in order (a file must pass all of them):
//!
//! 1. The checkout root is a project, never the home directory or a parent
//!    of it; every requested path is jailed to the root (PathJail, enforced
//!    even when the tool jail is relaxed) and must exist.
//! 2. The walk starts at the root, so `.gitignore` (and `.git/info/exclude`)
//!    applies to every requested path; `.git/` and `.lean-ctx/` are never
//!    walked; symlinks are never followed or packed.
//! 3. A file the effective Hive policy excludes (`security.excluded_paths`
//!    of this vault, gitignore globs) is left out.
//! 4. A secret-like path (`.env`, keys, `.ssh/`, credential files:
//!    [`crate::core::io_boundary::is_secret_like`]) is left out.
//! 5. Every remaining file is screened with the secret detectors (the
//!    built-in rules and `[secret_detection] custom_patterns`) and for this
//!    machine's home path. One finding refuses the whole bundle; nothing is
//!    uploaded and the findings name only paths and rule names.
//! 6. At most [`MAX_BUNDLE_BYTES`] in all (the tar itself).
//!
//! The tar lists the files under `app/` sorted bytewise by path, with
//! mtime 0, uid/gid 0, no user names and mode 0644 (0755 when executable),
//! so the same files give the same bytes and the same digest on every
//! machine.

use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::core::hive::HiveError;
use lean_ctx_protocol::hive::compute::{BundleRefV1, MAX_BUNDLE_BYTES, content_digest};

/// The bundle's name in the job (`/work/in/src.tar`).
pub const BUNDLE_NAME: &str = "src.tar";
/// Directory the files sit under inside the tar.
pub const TAR_PREFIX: &str = "app";
/// Findings listed in a refusal; the rest are counted.
const MAX_LISTED: usize = 10;
/// Directories never walked (engine state and git internals).
const NEVER_WALKED: [&str; 2] = [".git", ".lean-ctx"];

/// What decides which files may leave.
#[derive(Debug, Clone, Default)]
pub struct BundleRules {
    /// `security.excluded_paths` of the effective policy (gitignore globs).
    pub excluded: Vec<String>,
    /// `[secret_detection] custom_patterns`.
    pub custom_secret_patterns: Vec<String>,
    /// This machine's home directory; content naming it is refused.
    pub home: Option<PathBuf>,
}

impl BundleRules {
    /// The rules of this installation for `excluded` policy globs.
    pub fn for_installation(excluded: Vec<String>) -> Self {
        Self {
            excluded,
            custom_secret_patterns: crate::core::config::Config::load()
                .secret_detection
                .custom_patterns
                .clone(),
            home: dirs::home_dir(),
        }
    }
}

/// What was left out, for the person.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BundleReport {
    pub files: usize,
    /// Files the effective policy excludes.
    pub excluded_by_policy: Vec<String>,
    /// Secret-like paths, with the rule that matched.
    pub secret_paths: Vec<(String, &'static str)>,
    /// Symlinks and paths that are not UTF-8.
    pub skipped: usize,
}

/// A built bundle.
#[derive(Debug, Clone)]
pub struct Bundle {
    pub bytes: Vec<u8>,
    pub reference: BundleRefV1,
    pub report: BundleReport,
}

fn refused(text: impl Into<String>) -> HiveError {
    HiveError::Refused(text.into())
}

fn canonical(path: &Path) -> Result<PathBuf, HiveError> {
    crate::core::pathutil::canonicalize_secure(path)
        .map_err(|e| refused(format!("{}: {e}", path.display())))
}

/// The jailed, canonical start points of `paths` below `root` (default:
/// the whole checkout).
fn starts(root: &Path, paths: &[String]) -> Result<Vec<PathBuf>, HiveError> {
    if paths.is_empty() {
        return Ok(vec![root.to_path_buf()]);
    }
    let mut out = Vec::new();
    for path in paths {
        let candidate = Path::new(path);
        let candidate = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            root.join(candidate)
        };
        let jailed = crate::core::pathjail::jail_path_enforced(&candidate, root)
            .map_err(|e| refused(format!("path_refused: {path}: {e}")))?;
        let jailed = crate::core::pathutil::canonicalize_secure(&jailed)
            .map_err(|_| refused(format!("path_refused: {path} does not exist")))?;
        if !jailed.starts_with(root) {
            return Err(refused(format!(
                "path_refused: {path} is outside this checkout"
            )));
        }
        out.push(jailed);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn excluded_matcher(root: &Path, globs: &[String]) -> Result<Gitignore, HiveError> {
    let mut builder = GitignoreBuilder::new(root);
    for glob in globs {
        builder.add_line(None, glob).map_err(|e| {
            refused(format!(
                "the policy's excluded path {glob:?} is invalid: {e}"
            ))
        })?;
    }
    builder
        .build()
        .map_err(|e| refused(format!("the policy's excluded paths are invalid: {e}")))
}

/// `a/b/c` of `path` below `root`, `None` when a part is not UTF-8.
fn relative(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for part in rel.components() {
        match part {
            std::path::Component::Normal(name) => parts.push(name.to_str()?.to_owned()),
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

#[cfg(unix)]
fn executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// The checkout must be a project, not the home directory or above it.
fn check_root(root: &Path, home: Option<&Path>) -> Result<(), HiveError> {
    if root.parent().is_none() {
        return Err(refused(
            "path_refused: the filesystem root is not a checkout",
        ));
    }
    if let Some(home) = home.and_then(|h| canonical(h).ok())
        && home.starts_with(root)
    {
        return Err(refused(
            "path_refused: a compute bundle is never built from your home directory or above it; run it in the project's checkout",
        ));
    }
    Ok(())
}

/// Build the bundle of `paths` (relative to `root`, or absolute inside it;
/// empty = the whole checkout) under `rules`.
pub fn build(root: &Path, paths: &[String], rules: &BundleRules) -> Result<Bundle, HiveError> {
    let root = canonical(root)?;
    check_root(&root, rules.home.as_deref())?;
    let starts = starts(&root, paths)?;
    let excluded = excluded_matcher(&root, &rules.excluded)?;
    let wanted = |path: &Path| starts.iter().any(|s| path.starts_with(s));
    let on_the_way = |path: &Path| starts.iter().any(|s| s.starts_with(path));

    let mut report = BundleReport::default();
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let prune = starts.clone();
    let walker = ignore::WalkBuilder::new(&root)
        .standard_filters(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .ignore(false)
        .parents(true)
        .require_git(false)
        .hidden(false)
        .follow_links(false)
        .filter_entry(move |entry| {
            if entry.depth() == 0 || !entry.file_type().is_some_and(|t| t.is_dir()) {
                return true;
            }
            let name = entry.file_name().to_string_lossy();
            let path = entry.path();
            !NEVER_WALKED.contains(&name.as_ref())
                && prune
                    .iter()
                    .any(|s| path.starts_with(s) || s.starts_with(path))
        })
        .build();
    for entry in walker {
        let entry = entry.map_err(|e| refused(format!("walk {}: {e}", root.display())))?;
        let path = entry.path();
        let Some(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() || !(wanted(path) || on_the_way(path)) {
            continue;
        }
        if kind.is_symlink() || !kind.is_file() {
            if wanted(path) {
                report.skipped += 1;
            }
            continue;
        }
        if !wanted(path) {
            continue;
        }
        let Some(rel) = relative(&root, path) else {
            report.skipped += 1;
            continue;
        };
        if excluded
            .matched_path_or_any_parents(Path::new(&rel), false)
            .is_ignore()
        {
            report.excluded_by_policy.push(rel);
            continue;
        }
        if let Some(why) = crate::core::io_boundary::is_secret_like(Path::new(&rel)) {
            report.secret_paths.push((rel, why));
            continue;
        }
        files.push((rel, path.to_path_buf()));
    }
    files.sort();
    files.dedup();
    report.excluded_by_policy.sort();
    report.secret_paths.sort();
    if files.is_empty() {
        return Err(refused(
            "bundle_empty: no file of the requested paths may leave this machine",
        ));
    }

    let home = rules
        .home
        .as_ref()
        .map(|h| h.to_string_lossy().into_owned())
        .filter(|h| h.len() > 1);
    let mut findings: Vec<String> = Vec::new();
    let mut builder = tar::Builder::new(Vec::new());
    let mut total: u64 = 0;
    for (rel, path) in &files {
        let meta =
            std::fs::symlink_metadata(path).map_err(|e| HiveError::Local(format!("{rel}: {e}")))?;
        if !meta.is_file() {
            report.skipped += 1;
            continue;
        }
        // 512 bytes of header plus the padded content, before reading it.
        total = total.saturating_add(512 + meta.len().div_ceil(512) * 512);
        if total > MAX_BUNDLE_BYTES {
            return Err(too_large());
        }
        let bytes = std::fs::read(path).map_err(|e| HiveError::Local(format!("{rel}: {e}")))?;
        let text = String::from_utf8_lossy(&bytes);
        let secrets = crate::core::secret_detection::detect_secrets_with_custom(
            &text,
            &rules.custom_secret_patterns,
        );
        if let Some(first) = secrets.first() {
            findings.push(format!("{rel} ({})", first.pattern_name));
            continue;
        }
        if let Some(home) = &home
            && text.contains(home.as_str())
        {
            findings.push(format!("{rel} (local home path)"));
            continue;
        }
        if !findings.is_empty() {
            // Refused anyway; keep screening, stop packing.
            continue;
        }
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(bytes.len() as u64);
        header.set_mode(if executable(&meta) { 0o755 } else { 0o644 });
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        builder
            .append_data(&mut header, format!("{TAR_PREFIX}/{rel}"), bytes.as_slice())
            .map_err(|e| HiveError::Local(format!("bundle {rel}: {e}")))?;
        if builder.get_ref().len() as u64 > MAX_BUNDLE_BYTES {
            return Err(too_large());
        }
        report.files += 1;
    }
    if !findings.is_empty() {
        let more = findings.len().saturating_sub(MAX_LISTED);
        let listed = findings
            .iter()
            .take(MAX_LISTED)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        return Err(refused(format!(
            "secret_found: {} file(s) look like they carry a credential or a local home path, so nothing was uploaded: {listed}{}. Remove them, or narrow the bundle with --path.",
            findings.len(),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        )));
    }
    let bytes = builder
        .into_inner()
        .map_err(|e| HiveError::Local(format!("bundle: {e}")))?;
    if bytes.len() as u64 > MAX_BUNDLE_BYTES {
        return Err(too_large());
    }
    let reference = BundleRefV1 {
        name: BUNDLE_NAME.into(),
        digest: content_digest(&bytes),
        size_bytes: bytes.len() as u64,
    };
    Ok(Bundle {
        bytes,
        reference,
        report,
    })
}

fn too_large() -> HiveError {
    refused("bundle_too_large: the input bundle would pass 64 MiB; narrow it with --path")
}
