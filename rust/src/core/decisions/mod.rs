// SPDX-License-Identifier: Apache-2.0
//! Decision layer (D1): versioned project decisions in `.leanctx/decisions/*.toml`.
//!
//! A decision record names what was decided, by whom, where it applies (repos,
//! path globs, symbols) and, optionally, machine-checkable `[[check]]`s. This
//! module loads and validates the records (strictly: an unknown key, a bad
//! glob or a bad regex is an error naming the file), answers scope queries and
//! renders the deterministic context suffix for reads (#498: no timestamps,
//! no counters, stable header, sorted by id). [`check`] evaluates the checks
//! against a change; [`crate::core::pre_edit`] runs them before an edit.
//!
//! LeanCTX only detects, reports and hints. Refusing an edit is opt-in
//! (`[decisions] enforce = "refuse"`); enforcing a land belongs to Weave.

pub mod check;
mod parse;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub use check::{OtherFiles, Violation, evaluate_change, evaluate_change_with};
pub use parse::{NewRecord, id_from_slug, parse_decision, render_record, scaffold};

/// Directory, relative to the project root, that holds one record per file.
pub const DECISIONS_DIR: &str = ".leanctx/decisions";
/// Header of the read/compose suffix. Stable: prompt caches key on it.
pub const SUFFIX_HEADER: &str = "--- Decisions in scope ---";
/// Records beyond this many are named by count only in a suffix.
const MAX_SUFFIX_DECISIONS: usize = 8;
/// A suffix line longer than this is cut (on a char boundary).
const MAX_SUFFIX_LINE_CHARS: usize = 200;
/// Files beyond this many are reported as an error, not parsed.
const MAX_DECISION_FILES: usize = 512;
/// A record larger than this is an error (records are short by design).
pub const MAX_DECISION_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Active,
    Superseded,
    Revoked,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Superseded => "superseded",
            Self::Revoked => "revoked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    ForbidPath,
    ForbidRegex,
    RequireRegex,
}

impl CheckKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ForbidPath => "forbid_path",
            Self::ForbidRegex => "forbid_regex",
            Self::RequireRegex => "require_regex",
        }
    }
}

/// A validated `[[check]]`. `paths` empty means "the decision's scope".
#[derive(Debug, Clone)]
pub struct Check {
    pub kind: CheckKind,
    pub paths: Vec<String>,
    globs: Vec<glob::Pattern>,
    pub pattern: Option<String>,
    regex: Option<regex::Regex>,
    pub message: Option<String>,
}

/// A validated decision record.
#[derive(Debug, Clone)]
pub struct Decision {
    pub id: String,
    pub title: String,
    pub statement: String,
    pub rationale: String,
    pub status: Status,
    pub supersedes: Vec<String>,
    pub decided_by: String,
    pub decided_at: String,
    pub source_kind: Option<String>,
    pub source_ref: Option<String>,
    pub repos: Vec<String>,
    pub paths: Vec<String>,
    path_globs: Vec<glob::Pattern>,
    pub symbols: Vec<String>,
    pub checks: Vec<Check>,
    /// `[evidence]`: the history line the record was confirmed from (§38).
    pub evidence: Option<RecordEvidence>,
    /// Record file, relative to the project root (forward slashes).
    pub file: String,
}

/// The verbatim history line behind a record (`[evidence]`, contract §38).
/// `lean-ctx decisions suggest` re-reads it: a commit that left the history or
/// a message that no longer holds `quote` turns the record's trust to
/// `re-check`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordEvidence {
    /// Commit id (lower-case hex, 7 to 64 digits).
    pub commit: String,
    /// The message line, verbatim.
    pub quote: String,
    /// `confirmed` (explicit statement) or `reconstructed` (policy wording).
    pub trust: String,
}

/// Every record of one project, sorted by id, plus the files that failed.
#[derive(Debug, Clone, Default)]
pub struct DecisionSet {
    pub decisions: Vec<Decision>,
    /// `<file>: <reason>`, sorted. A failing file contributes no decision.
    pub errors: Vec<String>,
    /// Names this checkout answers to for `scope.repos` (lower case).
    repo_names: Vec<String>,
    /// Working tree the records were loaded from (`require_regex` scans it).
    root: Option<PathBuf>,
}

fn match_options() -> glob::MatchOptions {
    glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    }
}

impl Check {
    /// Whether this check looks at `rel_path` (`paths`, else the decision's).
    pub fn covers(&self, decision: &Decision, rel_path: &str) -> bool {
        if self.globs.is_empty() {
            decision.covers_path(rel_path)
        } else {
            let opts = match_options();
            self.globs.iter().any(|g| g.matches_with(rel_path, opts))
        }
    }

    pub fn regex(&self) -> Option<&regex::Regex> {
        self.regex.as_ref()
    }
}

impl Decision {
    /// Repo-wide: no path and no symbol scope.
    pub fn is_repo_wide(&self) -> bool {
        self.paths.is_empty() && self.symbols.is_empty()
    }

    /// Whether `rel_path` (root-relative, forward slashes) is in path scope.
    /// A symbol-only decision covers no path; a repo-wide one covers all.
    pub fn covers_path(&self, rel_path: &str) -> bool {
        if self.is_repo_wide() {
            return true;
        }
        let opts = match_options();
        self.path_globs
            .iter()
            .any(|g| g.matches_with(rel_path, opts))
    }

    /// Whether `symbol` (`name` or `path::name`) is in symbol scope.
    pub fn covers_symbol(&self, symbol: &str) -> bool {
        self.symbols.iter().any(|s| symbol_matches(s, symbol))
    }

    /// The first non-empty line of the statement, cut to a suffix line.
    pub fn one_line(&self) -> String {
        let line = self
            .statement
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or_default();
        truncate_chars(line, MAX_SUFFIX_LINE_CHARS)
    }
}

/// `a` and `b` name the same symbol: equal, or equal last `::` segment when
/// at least one side is a bare name.
fn symbol_matches(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let last = |s: &str| s.rsplit("::").next().unwrap_or(s).to_string();
    (!a.contains("::") || !b.contains("::")) && last(a) == last(b)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

impl DecisionSet {
    /// Decisions in force: status `active` and not superseded by another
    /// active decision.
    pub fn active(&self) -> impl Iterator<Item = &Decision> {
        let superseded: Vec<&str> = self
            .decisions
            .iter()
            .filter(|d| d.status == Status::Active)
            .flat_map(|d| d.supersedes.iter().map(String::as_str))
            .collect();
        self.decisions
            .iter()
            .filter(move |d| d.status == Status::Active && !superseded.contains(&d.id.as_str()))
    }

    /// The effective status of `d` (an active record named in another active
    /// record's `supersedes` reads as superseded).
    pub fn effective_status(&self, d: &Decision) -> Status {
        if d.status == Status::Active && self.active().all(|a| a.id != d.id) {
            Status::Superseded
        } else {
            d.status
        }
    }

    pub fn get(&self, id: &str) -> Option<&Decision> {
        self.decisions.iter().find(|d| d.id == id)
    }

    /// Whether `d.scope.repos` admits this checkout.
    pub fn repo_matches(&self, d: &Decision) -> bool {
        d.repos.is_empty()
            || d.repos
                .iter()
                .any(|r| self.repo_names.iter().any(|n| n == &r.to_lowercase()))
    }

    /// Active decisions whose scope covers `rel_path`, sorted by id.
    pub fn for_path(&self, rel_path: &str) -> Vec<&Decision> {
        self.active()
            .filter(|d| self.repo_matches(d) && d.covers_path(rel_path))
            .collect()
    }

    /// Active decisions whose symbol scope names `symbol`, sorted by id.
    pub fn for_symbol(&self, symbol: &str) -> Vec<&Decision> {
        self.active()
            .filter(|d| self.repo_matches(d) && d.covers_symbol(symbol))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.decisions.is_empty() && self.errors.is_empty()
    }
}

// ── Loading ─────────────────────────────────────────────────────────────────

type Signature = Vec<(String, u64, u128)>;

static CACHE: Mutex<Option<HashMap<PathBuf, (Signature, Arc<DecisionSet>)>>> = Mutex::new(None);

/// Most project roots the record cache holds; a full cache is cleared before
/// a new root is inserted (bounded state in a long-lived server).
pub(crate) const MAX_CACHED_ROOTS: usize = 16;

/// One `*.toml` entry directly in a project's [`DECISIONS_DIR`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordEntry {
    /// `<root>/.leanctx/decisions/<name>`, under the root as given.
    pub path: PathBuf,
    /// Root-relative path with `/` separators (`.leanctx/decisions/<name>`).
    pub rel: String,
    /// A regular file. A symlink (to anything) or other entry is `false` and
    /// must not be read: [`read_record`] refuses it too.
    pub regular: bool,
}

/// The shared record collector: the `*.toml` entries directly in
/// `<root>/.leanctx/decisions`, sorted by name, never following a symlink.
///
/// `Ok(None)`: the project has no such directory. `Err`: it is not a
/// directory, or it (or a parent) resolves outside the project, e.g. a
/// symlinked directory. Used by the local loader ([`load`]) and by
/// `lean-ctx cloud decisions push`, so both refuse the same entries.
pub fn list_records(root: &Path) -> Result<Option<Vec<RecordEntry>>, String> {
    let dir = root.join(DECISIONS_DIR);
    let meta = match std::fs::symlink_metadata(&dir) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{DECISIONS_DIR}: {e}")),
    };
    if !meta.is_dir() {
        return Err(format!(
            "{DECISIONS_DIR} is not a directory inside the project"
        ));
    }
    // The directory (and each parent) must resolve inside the project.
    let real_root =
        std::fs::canonicalize(root).map_err(|e| format!("project root {}: {e}", root.display()))?;
    let real = std::fs::canonicalize(&dir).map_err(|e| format!("{DECISIONS_DIR}: {e}"))?;
    // Joined per component: a verbatim Windows path (`\\?\`) takes no `/`.
    let expected = DECISIONS_DIR
        .split('/')
        .fold(real_root, |p, part| p.join(part));
    if real != expected {
        return Err(format!("{DECISIONS_DIR} leads outside the project"));
    }
    let mut entries: Vec<RecordEntry> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{DECISIONS_DIR}: {e}"))?
        .filter_map(Result::ok)
        .filter(|e| {
            Path::new(&e.file_name())
                .extension()
                .is_some_and(|x| x == "toml")
        })
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // `DirEntry::file_type` does not follow symlinks.
            let regular = e.file_type().is_ok_and(|t| t.is_file());
            RecordEntry {
                path: dir.join(e.file_name()),
                rel: format!("{DECISIONS_DIR}/{name}"),
                regular,
            }
        })
        .collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Some(entries))
}

/// Read one record: opened once without following a symlink, checked on
/// that handle and read with a bound ([`MAX_DECISION_BYTES`]), so a file
/// swapped or grown after the directory scan cannot get through.
pub fn read_record(file: &Path) -> Result<String, &'static str> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(not(unix))]
    {
        if std::fs::symlink_metadata(file).is_ok_and(|m| !m.file_type().is_file()) {
            return Err("not a regular file");
        }
    }
    let handle = options.open(file).map_err(|e| {
        #[cfg(unix)]
        {
            if e.raw_os_error() == Some(libc::ELOOP) {
                return "not a regular file";
            }
        }
        let _ = e;
        "unreadable"
    })?;
    let meta = handle.metadata().map_err(|_| "unreadable")?;
    if !meta.is_file() {
        return Err("not a regular file");
    }
    if meta.len() > MAX_DECISION_BYTES {
        return Err("larger than 64 KiB");
    }
    let mut bytes = Vec::new();
    handle
        .take(MAX_DECISION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "unreadable")?;
    if bytes.len() as u64 > MAX_DECISION_BYTES {
        return Err("larger than 64 KiB");
    }
    String::from_utf8(bytes).map_err(|_| "not UTF-8 text")
}

fn signature(entries: &[RecordEntry]) -> Signature {
    entries
        .iter()
        .map(|e| {
            let p = &e.path;
            let meta = std::fs::symlink_metadata(p).ok();
            let len = meta.as_ref().map_or(0, std::fs::Metadata::len);
            let mtime = meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            (p.to_string_lossy().into_owned(), len, mtime)
        })
        .collect()
}

/// Load (cached by file list, sizes and mtimes) the records under `root`.
/// A project without `.leanctx/decisions` yields an empty set.
/// A record directory that resolves outside the project, or entries that
/// are not regular files, are reported as errors and never read.
pub fn load(root: &Path) -> Arc<DecisionSet> {
    let files = match list_records(root) {
        Ok(Some(entries)) if !entries.is_empty() => entries,
        Ok(_) => return Arc::new(DecisionSet::default()),
        Err(e) => {
            return Arc::new(from_records(
                root,
                vec![(DECISIONS_DIR.to_string(), Err(e))],
            ));
        }
    };
    let sig = signature(&files);
    {
        let guard = CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((cached_sig, set)) = guard.as_ref().and_then(|c| c.get(root))
            && *cached_sig == sig
        {
            return Arc::clone(set);
        }
    }
    let set = Arc::new(load_uncached(root, &files));
    let mut guard = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cache = guard.get_or_insert_with(HashMap::new);
    if !cache.contains_key(root) && cache.len() >= MAX_CACHED_ROOTS {
        cache.clear();
    }
    cache.insert(root.to_path_buf(), (sig, Arc::clone(&set)));
    set
}

#[cfg(test)]
pub(crate) fn cached_roots() -> usize {
    CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .map_or(0, HashMap::len)
}

fn load_uncached(root: &Path, files: &[RecordEntry]) -> DecisionSet {
    let records: Vec<(String, Result<String, String>)> = files
        .iter()
        .take(MAX_DECISION_FILES + 1)
        .map(|entry| {
            let text = if entry.regular {
                read_record(&entry.path).map_err(str::to_string)
            } else {
                Err("not a regular file (symlinks are not followed)".to_string())
            };
            (entry.rel.clone(), text)
        })
        .collect();
    let mut set = from_records(root, records);
    set.root = Some(root.to_path_buf());
    set
}

/// A set from record texts (`(root-relative file, text or read error)`),
/// e.g. read from a base commit. `repo_root` answers `scope.repos`. The set
/// knows no working tree, so `require_regex` needs an explicit
/// [`check::OtherFiles`] ([`evaluate_change_with`]).
pub fn from_records(
    repo_root: &Path,
    records: Vec<(String, Result<String, String>)>,
) -> DecisionSet {
    let mut set = DecisionSet {
        repo_names: repo_names(repo_root),
        ..DecisionSet::default()
    };
    if records.len() > MAX_DECISION_FILES {
        set.errors.push(format!(
            "{DECISIONS_DIR}: more than the limit of {MAX_DECISION_FILES} record files; none loaded"
        ));
        return set;
    }
    for (rel, text) in records {
        let stem = Path::new(&rel)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = match text {
            Ok(t) if t.len() as u64 > MAX_DECISION_BYTES => {
                set.errors.push(format!(
                    "{rel}: {} bytes, more than the limit of {MAX_DECISION_BYTES}",
                    t.len()
                ));
                continue;
            }
            Ok(t) => t,
            Err(e) => {
                set.errors.push(format!("{rel}: {e}"));
                continue;
            }
        };
        match parse_decision(&text, &stem, &rel) {
            Ok(d) if set.decisions.iter().any(|o| o.id == d.id) => {
                set.errors.push(format!("{rel}: duplicate id `{}`", d.id));
            }
            Ok(d) => set.decisions.push(d),
            Err(e) => set.errors.push(format!("{rel}: {e}")),
        }
    }
    set.decisions.sort_by(|a, b| a.id.cmp(&b.id));
    set.errors.sort();
    set
}

/// Names this checkout answers to: the root directory name and the `origin`
/// remote's repository name (read from git config; no subprocess).
fn repo_names(root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(name) = root.file_name() {
        names.push(name.to_string_lossy().to_lowercase());
    }
    if let Some(url) = origin_url(root) {
        let base = url
            .trim_end_matches('/')
            .rsplit(['/', ':'])
            .next()
            .unwrap_or_default()
            .trim_end_matches(".git")
            .to_lowercase();
        if !base.is_empty() && !names.contains(&base) {
            names.push(base);
        }
    }
    names
}

fn git_common_dir(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
    let gitdir = if gitdir.is_absolute() {
        gitdir
    } else {
        root.join(gitdir)
    };
    match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(common) => {
            let common = PathBuf::from(common.trim());
            Some(if common.is_absolute() {
                common
            } else {
                gitdir.join(common)
            })
        }
        Err(_) => Some(gitdir),
    }
}

fn origin_url(root: &Path) -> Option<String> {
    let config = std::fs::read_to_string(git_common_dir(root)?.join("config")).ok()?;
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
        } else if in_origin
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "url"
        {
            return Some(value.trim().to_string());
        }
    }
    None
}

/// The nearest ancestor of `path` (itself included when a directory) that
/// holds a `.leanctx/decisions` directory, stopping at the first git root.
pub fn find_root(path: &Path) -> Option<PathBuf> {
    let start = if path.is_dir() { path } else { path.parent()? };
    for dir in start.ancestors() {
        if dir.join(DECISIONS_DIR).is_dir() {
            return Some(dir.to_path_buf());
        }
        if dir.join(".git").exists() {
            return None;
        }
    }
    None
}

/// `path` relative to `root` with forward slashes; `None` outside `root`.
pub fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let rel: PathBuf = if let Ok(rel) = abs.strip_prefix(root) {
        rel.to_path_buf()
    } else {
        // Tolerate a symlinked root (e.g. /tmp vs /private/tmp on macOS).
        // A file that does not exist yet canonicalizes through its parent.
        let canon_root = root.canonicalize().ok()?;
        let canon = abs.canonicalize().ok().or_else(|| {
            let parent = abs.parent()?.canonicalize().ok()?;
            Some(parent.join(abs.file_name()?))
        })?;
        canon.strip_prefix(&canon_root).ok()?.to_path_buf()
    };
    let s = rel.to_string_lossy().replace('\\', "/");
    (!s.is_empty()).then_some(s)
}

// ── Rendering ───────────────────────────────────────────────────────────────

/// One line per decision: `id: title — statement`.
pub fn render_lines(decisions: &[&Decision]) -> String {
    let mut out = String::new();
    for d in decisions.iter().take(MAX_SUFFIX_DECISIONS) {
        let line = format!("{}: {} — {}", d.id, d.title.trim(), d.one_line());
        out.push_str(&truncate_chars(&line, MAX_SUFFIX_LINE_CHARS * 2));
        out.push('\n');
    }
    if decisions.len() > MAX_SUFFIX_DECISIONS {
        out.push_str(&format!(
            "(+{} more: lean-ctx decisions list)\n",
            decisions.len() - MAX_SUFFIX_DECISIONS
        ));
    }
    out
}

/// Deterministic suffix for a read of `rel_paths` (deduplicated, sorted):
/// empty when no active decision covers any of them.
pub fn suffix_for_paths(set: &DecisionSet, rel_paths: &[&str]) -> String {
    let mut hits: Vec<&Decision> = Vec::new();
    for p in rel_paths {
        for d in set.for_path(p) {
            if !hits.iter().any(|h| h.id == d.id) {
                hits.push(d);
            }
        }
    }
    if hits.is_empty() {
        return String::new();
    }
    hits.sort_by(|a, b| a.id.cmp(&b.id));
    format!("\n\n{SUFFIX_HEADER}\n{}", render_lines(&hits))
        .trim_end()
        .to_string()
}

/// The suffix for a read of `file` (absolute or root-relative). Empty when the
/// file is outside every project with decisions.
pub fn suffix_for_file(file: &Path) -> String {
    let Some(root) = find_root(file) else {
        return String::new();
    };
    let Some(rel) = relative_path(&root, file) else {
        return String::new();
    };
    suffix_for_paths(&load(&root), &[rel.as_str()])
}

/// The read suffix for a tool reply (`ctx_read`, `ctx_compose`): empty while
/// a gateway policy is active, since decision records are repository text
/// that the policy did not admit as a source of this reply.
pub fn suffix_for_read(file: &str) -> String {
    if crate::core::policy::runtime::is_active() {
        return String::new();
    }
    suffix_for_file(Path::new(file))
}

/// The suffix for paths a compose answer named, relative to `root`.
pub fn suffix_for_root_paths(root: &Path, rel_paths: &[&str]) -> String {
    if !root.join(DECISIONS_DIR).is_dir() || rel_paths.is_empty() {
        return String::new();
    }
    suffix_for_paths(&load(root), rel_paths)
}

/// Most distinct files a compose answer contributes to its suffix.
const MAX_COMPOSE_PATHS: usize = 64;

/// The suffix for a `ctx_compose` answer: decisions covering the files it
/// names (`path:line …` and `File: path` lines that exist under `root`).
pub fn suffix_for_compose(root: &Path, text: &str) -> String {
    if crate::core::policy::runtime::is_active() || !root.join(DECISIONS_DIR).is_dir() {
        return String::new();
    }
    let mut paths: Vec<&str> = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        let candidate = match line.strip_prefix("File: ") {
            Some(rest) => rest.split([' ', '(']).next().unwrap_or_default(),
            None => match line.split_once(':') {
                Some((p, rest)) if rest.starts_with(|c: char| c.is_ascii_digit()) => p,
                _ => continue,
            },
        };
        if candidate.is_empty()
            || candidate.contains(char::is_whitespace)
            || paths.contains(&candidate)
            || !root.join(candidate).is_file()
        {
            continue;
        }
        paths.push(candidate);
        if paths.len() >= MAX_COMPOSE_PATHS {
            break;
        }
    }
    suffix_for_root_paths(root, &paths)
}

/// Full, multi-line description of one decision (CLI `show`, `ctx_knowledge`).
pub fn describe(set: &DecisionSet, d: &Decision) -> String {
    let mut out = format!(
        "{} [{}] {}\n  statement: {}\n",
        d.id,
        set.effective_status(d).as_str(),
        d.title.trim(),
        d.statement.trim()
    );
    if !d.rationale.trim().is_empty() {
        out.push_str(&format!("  rationale: {}\n", d.rationale.trim()));
    }
    out.push_str(&format!(
        "  decided: {} by {}\n",
        d.decided_at, d.decided_by
    ));
    if let (Some(kind), Some(r)) = (&d.source_kind, &d.source_ref) {
        out.push_str(&format!("  source: {kind} {r}\n"));
    }
    if let Some(ev) = &d.evidence {
        out.push_str(&format!(
            "  evidence ({}): {} \"{}\"\n",
            ev.trust,
            &ev.commit[..ev.commit.len().min(12)],
            ev.quote
        ));
    }
    if !d.supersedes.is_empty() {
        out.push_str(&format!("  supersedes: {}\n", d.supersedes.join(", ")));
    }
    let scope = |v: &[String]| {
        if v.is_empty() {
            "*".to_string()
        } else {
            v.join(", ")
        }
    };
    out.push_str(&format!(
        "  scope: repos={} paths={} symbols={}\n",
        scope(&d.repos),
        scope(&d.paths),
        if d.symbols.is_empty() {
            "-".to_string()
        } else {
            d.symbols.join(", ")
        }
    ));
    for (i, c) in d.checks.iter().enumerate() {
        out.push_str(&format!(
            "  check[{i}]: {} paths={}{}\n",
            c.kind.as_str(),
            scope(&c.paths),
            c.pattern
                .as_deref()
                .map(|p| format!(" pattern={p}"))
                .unwrap_or_default()
        ));
    }
    out.push_str(&format!("  file: {}\n", d.file));
    // Records may quote git history: no escape sequence reaches a terminal.
    out.split('\n')
        .map(terminal_safe)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Text for a terminal: control characters (C0, DEL, C1, so no escape
/// sequences, also newlines) and bidi controls become `?`.
pub fn terminal_safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control()
                || matches!(
                    c,
                    '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                )
            {
                '?'
            } else {
                c
            }
        })
        .collect()
}

/// The decision query of `ctx_knowledge action=decisions` and `lean-ctx
/// decisions list`: by `path` (decisions whose scope covers it), else by
/// `symbol`, else all records; invalid records are listed after them.
pub fn query_text(project_root: &Path, path: Option<&str>, symbol: Option<&str>) -> String {
    let root = find_root(project_root).unwrap_or_else(|| project_root.to_path_buf());
    let set = load(&root);
    if set.is_empty() {
        return format!(
            "No decisions in this project ({DECISIONS_DIR}/*.toml). Create one: lean-ctx decisions new <slug>"
        );
    }
    let (label, hits): (String, Vec<&Decision>) = match (path, symbol) {
        (Some(p), _) => {
            let rel = relative_path(&root, Path::new(p))
                .unwrap_or_else(|| p.trim_start_matches("./").to_string());
            let hits = set.for_path(&rel);
            (format!("in scope of {rel}"), hits)
        }
        (None, Some(s)) => (format!("naming symbol {s}"), set.for_symbol(s)),
        (None, None) => ("all".to_string(), set.decisions.iter().collect()),
    };
    let mut out = format!("Decisions {label}: {}\n", hits.len());
    for d in hits {
        out.push_str(&describe(&set, d));
    }
    if !set.errors.is_empty() {
        out.push_str("Invalid records (not applied):\n");
        for e in &set.errors {
            // Parse errors may quote record files: no terminal escapes.
            out.push_str(&format!("  {}\n", terminal_safe(e)));
        }
    }
    out.trim_end().to_string()
}
