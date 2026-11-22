// SPDX-License-Identifier: Apache-2.0
//! Landing impact analysis over committed trees: changed units, depth-bounded
//! dependents, affected tests and the semantic overlap of two heads.
//!
//! Test selection fails closed: whenever a change cannot be attributed to
//! named units (unmapped non-source files, opaque module-level code, git or
//! read errors, the search cap or the depth bound), the side reports
//! `full_suite=true` with a note naming the reason. The security tests are
//! always selected; if they cannot be resolved the side reports `full_suite`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::LazyLock;

use lean_ctx::core::call_graph::{callee_matches, callee_segment};
use regex::Regex;

use super::git;
use super::units::{
    self, ModuleLine, Parsed, SECURITY, SECURITY_DECL_PATTERNS, Span, WHOLE_FILE, grep_name, unit,
};

/// Dependent units one side may collect before the search stops (`complete=false`).
const MAX_CALLERS: usize = 5000;

/// Any `include!`, `include_str!` or `include_bytes!` invocation.
static INCLUDE_MACRO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\binclude(?:_str|_bytes)?!\s*\(").expect("static regex"));
/// `include_*!("relative/path")`, resolved against the including file.
static INCLUDE_REL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\binclude(?:_str|_bytes)?!\s*\(\s*"([^"\\]*)"\s*,?\s*\)"#).expect("static regex")
});
/// `include_*!(concat!(env!("CARGO_MANIFEST_DIR"), "/path"))`, resolved
/// against the crate of the including file.
static INCLUDE_CRATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\binclude(?:_str|_bytes)?!\s*\(\s*concat!\s*\(\s*env!\s*\(\s*"CARGO_MANIFEST_DIR"\s*\)\s*,\s*"([^"\\]*)"\s*,?\s*\)\s*,?\s*\)"#,
    )
    .expect("static regex")
});

/// Why a test is affected; a test listed for several reasons keeps the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Reason {
    Changed,
    Reaches,
    SameFile,
    Security,
}

impl Reason {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::Reaches => "reaches",
            Self::SameFile => "same_file",
            Self::Security => "security",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Changed {
    pub path: String,
    pub key: String,
    pub kind: String,
    pub name: String,
    /// `added`, `removed` or `modified`.
    pub status: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Caller {
    pub path: String,
    pub key: String,
    pub depth: usize,
    /// The unit this dependent uses (a changed unit or a dependent one hop closer).
    pub via: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestHit {
    pub path: String,
    pub key: String,
    pub reason: Reason,
}

/// One side of a landing: `merge_base(base, commit)..commit`.
#[derive(Debug, Clone)]
pub(crate) struct Side {
    pub ref_name: String,
    pub commit: String,
    pub merge_base: String,
    pub changed: BTreeMap<String, Changed>,
    pub callers: BTreeMap<String, Caller>,
    pub tests: BTreeMap<String, TestHit>,
    pub notes: BTreeSet<String>,
    /// `false` when the dependent search hit its cap or failed.
    pub complete: bool,
    /// Only the whole suite is a safe selection; `notes` say why.
    pub full_suite: bool,
    /// Units whose dependents are searched although they did not change
    /// themselves (every unit of a file whose imports changed). Not rendered.
    pub(crate) extra_seeds: BTreeMap<String, Changed>,
}

impl Side {
    /// Fail closed: the whole suite must run, for the reason in `note`.
    fn fail(&mut self, note: String) {
        self.full_suite = true;
        self.notes.insert(note);
    }

    fn seeds(&self) -> BTreeMap<String, Changed> {
        let mut all = self.extra_seeds.clone();
        all.extend(self.changed.iter().map(|(k, v)| (k.clone(), v.clone())));
        all
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Pair {
    /// `same_unit`, `head_calls_other`, `other_calls_head` or `shared_caller`.
    pub kind: &'static str,
    pub head_unit: String,
    pub other_unit: String,
    pub hops: usize,
    /// The unit where both changes meet (`shared_caller` only).
    pub via: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Overlap {
    pub other: Side,
    pub pairs: Vec<Pair>,
    /// `false` when a dependent search behind the pairs failed or hit its cap:
    /// an empty `pairs` then proves nothing.
    pub complete: bool,
    pub notes: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Report {
    pub base_ref: String,
    pub base_commit: String,
    pub depth: usize,
    pub head: Side,
    pub overlap: Option<Overlap>,
}

/// Source files of one commit, read and parsed on demand.
pub(super) struct Tree<'a> {
    root: &'a Path,
    commit: String,
    /// `None` = read, but missing, not a blob or not UTF-8.
    files: BTreeMap<String, Option<Parsed>>,
    listing: Option<BTreeSet<String>>,
}

impl<'a> Tree<'a> {
    pub(super) fn new(root: &'a Path, commit: &str) -> Self {
        Self {
            root,
            commit: commit.to_string(),
            files: BTreeMap::new(),
            listing: None,
        }
    }

    fn load(&mut self, paths: &[String]) -> Result<(), String> {
        let missing: Vec<String> = paths
            .iter()
            .filter(|p| units::is_source(p) && !self.files.contains_key(*p))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let mut blobs = git::read_blobs(self.root, &self.commit, &missing)?;
        for p in missing {
            let parsed = blobs.remove(&p).map(|c| Parsed::parse(&p, &c));
            self.files.insert(p, parsed);
        }
        Ok(())
    }

    fn get(&self, path: &str) -> Option<&Parsed> {
        self.files.get(path).and_then(Option::as_ref)
    }

    /// Read, but not readable as UTF-8 source.
    fn unreadable(&self, path: &str) -> bool {
        matches!(self.files.get(path), Some(None))
    }

    fn listing(&mut self) -> Result<&BTreeSet<String>, String> {
        if self.listing.is_none() {
            let files = git::list_files(self.root, &self.commit)?;
            self.listing = Some(files.into_iter().collect());
        }
        Ok(self.listing.get_or_insert_with(BTreeSet::new))
    }
}

fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

pub(crate) fn analyze(
    root: &Path,
    base: &str,
    head: &str,
    other: Option<&str>,
    depth: usize,
) -> Result<Report, String> {
    analyze_capped(root, base, head, other, depth, MAX_CALLERS)
}

/// [`analyze`] with every dependent search stopping at `cap` units.
pub(super) fn analyze_capped(
    root: &Path,
    base: &str,
    head: &str,
    other: Option<&str>,
    depth: usize,
    cap: usize,
) -> Result<Report, String> {
    if !lean_ctx::core::git::git_available() {
        return Err("git is not available".to_string());
    }
    let base_commit = git::resolve_commit(root, base)?;
    let head_commit = git::resolve_commit(root, head)?;
    let mut head_tree = Tree::new(root, &head_commit);
    let mut head_side = side(root, &base_commit, head, &head_commit, &mut head_tree)?;
    let search = find_callers(&mut head_tree, &head_side.seeds(), depth, cap);
    finish_side(&mut head_side, &mut head_tree, search, depth);

    let overlap = match other {
        None => None,
        Some(other) => {
            let other_commit = git::resolve_commit(root, other)?;
            let mut other_tree = Tree::new(root, &other_commit);
            let mut other_side = side(root, &base_commit, other, &other_commit, &mut other_tree)?;
            let search = find_callers(&mut other_tree, &other_side.seeds(), depth, cap);
            finish_side(&mut other_side, &mut other_tree, search, depth);
            // A call added on one side is only visible in that side's tree:
            // search the dependents of each side's changes in the other tree too.
            let head_in_other = find_callers(&mut other_tree, &head_side.changed, depth, cap);
            let other_in_head = find_callers(&mut head_tree, &other_side.changed, depth, cap);
            let mut notes = BTreeSet::new();
            for (label, s) in [("head", &head_side), ("other", &other_side)] {
                if !s.complete {
                    notes.insert(format!("{label} dependent search incomplete"));
                }
            }
            for s in [&head_in_other, &other_in_head] {
                if !s.complete {
                    notes.insert(format!(
                        "cross-tree dependent search stopped at {cap} units"
                    ));
                }
                notes.extend(s.errors.iter().cloned());
            }
            let pairs = overlap_pairs(
                &head_side,
                &other_side,
                &head_in_other.found,
                &other_in_head.found,
            );
            Some(Overlap {
                other: other_side,
                pairs,
                complete: notes.is_empty(),
                notes,
            })
        }
    };

    Ok(Report {
        base_ref: base.to_string(),
        base_commit,
        depth,
        head: head_side,
        overlap,
    })
}

fn push_touched(touched: &mut Vec<(String, Option<Span>)>, key: String, span: Option<Span>) {
    if !touched.iter().any(|(k, _)| *k == key) {
        touched.push((key, span));
    }
}

/// Changed units of `merge_base(base, commit)..commit`.
fn side<'a>(
    root: &'a Path,
    base_commit: &str,
    ref_name: &str,
    commit: &str,
    tree: &mut Tree<'a>,
) -> Result<Side, String> {
    let mut s = Side {
        ref_name: ref_name.to_string(),
        commit: commit.to_string(),
        merge_base: String::new(),
        changed: BTreeMap::new(),
        callers: BTreeMap::new(),
        tests: BTreeMap::new(),
        notes: BTreeSet::new(),
        complete: true,
        full_suite: false,
        extra_seeds: BTreeMap::new(),
    };
    s.merge_base = if let Some(mb) = git::merge_base(root, base_commit, commit) {
        mb
    } else {
        s.notes
            .insert("no merge base: compared against base directly".to_string());
        base_commit.to_string()
    };
    let diffs = git::diff_files(root, &s.merge_base, commit)?;
    let mut base_tree = Tree::new(root, &s.merge_base);
    let new_paths: Vec<String> = diffs.iter().filter_map(|d| d.new_path.clone()).collect();
    let old_paths: Vec<String> = diffs.iter().filter_map(|d| d.old_path.clone()).collect();
    if let Err(e) = tree.load(&new_paths) {
        s.fail(format!(
            "changed files unreadable at {}: {e}",
            short(commit)
        ));
    }
    if let Err(e) = base_tree.load(&old_paths) {
        let mb = short(&s.merge_base).to_string();
        s.fail(format!("changed files unreadable at {mb}: {e}"));
    }

    // Files whose imports changed, with the tree their units come from.
    let mut import_files: BTreeSet<(String, bool)> = BTreeSet::new();
    for d in &diffs {
        let path = d.path().to_string();
        if units::is_build_manifest(&path) {
            s.fail(format!("build manifest changed: {path}"));
        }
        let file_status = match (&d.old_path, &d.new_path) {
            (None, Some(_)) => "added",
            (Some(_), None) => "removed",
            _ => "modified",
        };
        let new = d.new_path.as_deref().and_then(|p| tree.get(p));
        let old = d.old_path.as_deref().and_then(|p| base_tree.get(p));
        let mut touched: Vec<(String, Option<Span>)> = Vec::new();
        if units::is_source(&path) {
            let sides = [
                (d.new_path.as_deref(), &*tree, &d.new_lines, true),
                (d.old_path.as_deref(), &base_tree, &d.old_lines, false),
            ];
            for (p, t, ranges, is_new) in sides {
                let Some(p) = p else { continue };
                let Some(parsed) = t.get(p) else {
                    if t.unreadable(p) {
                        s.fail(format!("changed source is not UTF-8 text: {p}"));
                    }
                    continue;
                };
                for &(from, to) in ranges {
                    for line in from..=to {
                        if parsed.doc_fence_at(line) {
                            s.fail(format!(
                                "doc comment with a code block changed (doctest): {p}:{line}"
                            ));
                        }
                        let (key, span) = parsed.unit_at(line);
                        if key != WHOLE_FILE {
                            push_touched(&mut touched, key, span.cloned());
                            continue;
                        }
                        push_touched(&mut touched, WHOLE_FILE.to_string(), None);
                        match parsed.module_line(line) {
                            ModuleLine::Inert => {}
                            ModuleLine::Import => {
                                import_files.insert((p.to_string(), is_new));
                            }
                            ModuleLine::Decorates(key, span) => {
                                push_touched(&mut touched, key, Some(span));
                            }
                            ModuleLine::Opaque => s.fail(format!(
                                "module-level change in {p}:{line}: its dependents cannot be named"
                            )),
                        }
                    }
                }
            }
        }
        if touched.is_empty() {
            touched.push((WHOLE_FILE.to_string(), None));
        }
        let keys = |p: Option<&Parsed>| -> BTreeSet<String> {
            p.map(Parsed::all_keys)
                .unwrap_or_default()
                .into_iter()
                .collect()
        };
        let (head_keys, base_keys) = (keys(new), keys(old));
        for (key, span) in touched {
            let status = if key == WHOLE_FILE {
                file_status
            } else if !base_keys.contains(&key) {
                "added"
            } else if !head_keys.contains(&key) {
                "removed"
            } else {
                "modified"
            };
            let (kind, name) =
                span.map_or_else(|| ("file".to_string(), String::new()), |s| (s.kind, s.name));
            s.changed.entry(unit(&path, &key)).or_insert(Changed {
                path: path.clone(),
                key,
                kind,
                name,
                status,
            });
        }
    }

    for (path, is_new) in import_files {
        let t = if is_new { &*tree } else { &base_tree };
        let Some(parsed) = t.get(&path) else { continue };
        for span in &parsed.spans {
            let key = parsed.key_of_span(span);
            s.extra_seeds
                .entry(unit(&path, &key))
                .or_insert_with(|| Changed {
                    path: path.clone(),
                    key,
                    kind: span.kind.clone(),
                    name: span.name.clone(),
                    status: "modified",
                });
        }
        s.notes.insert(format!(
            "imports changed in {path}: dependents of all its units followed"
        ));
    }

    attribute_includes(&mut s, &diffs, tree, &mut base_tree);
    Ok(s)
}

/// How a changed path is used by source files of one tree.
#[derive(Debug, Default)]
struct IncludeScan {
    /// `(file, line)` of an include macro resolving to the path.
    includers: Vec<(String, usize)>,
    /// `(file, line)` of an include macro naming the file name that could
    /// not be resolved to a path.
    unresolved: Vec<(String, usize)>,
    /// Other source lines naming the file name (a runtime read, a comment…).
    mentions: usize,
}

/// `dir/rel` normalized (`.`/`..`), `None` for absolute or escaping paths.
fn join_rel(dir: &str, rel: &str) -> Option<String> {
    if rel.starts_with('/') || rel.contains('\\') {
        return None;
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for c in rel.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    Some(parts.join("/"))
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Directory of the nearest `Cargo.toml` above `file` in `listing`.
fn crate_dir(file: &str, listing: &BTreeSet<String>) -> Option<String> {
    let mut dir = parent(file);
    loop {
        let manifest = if dir.is_empty() {
            "Cargo.toml".to_string()
        } else {
            format!("{dir}/Cargo.toml")
        };
        if listing.contains(&manifest) {
            return Some(dir.to_string());
        }
        if dir.is_empty() {
            return None;
        }
        dir = parent(dir);
    }
}

/// Text of the include invocation around `line`: the line itself when it
/// holds the macro, else an include opened on one of the two lines above and
/// still unclosed at `line`.
fn include_context(parsed: &Parsed, line: usize) -> Option<String> {
    let here = parsed.line(line)?;
    if INCLUDE_MACRO.is_match(here) {
        return Some(here.to_string());
    }
    for k in [line.saturating_sub(1), line.saturating_sub(2)] {
        if k == 0 || k == line {
            continue;
        }
        let joined: String = (k..=line)
            .filter_map(|i| parsed.line(i))
            .collect::<Vec<_>>()
            .join(" ");
        let Some(m) = INCLUDE_MACRO.find(&joined) else {
            continue;
        };
        let before_here: usize = (k..line)
            .filter_map(|i| parsed.line(i))
            .map(|l| l.len() + 1)
            .sum();
        let mut depth = 0i32;
        let mut open_at_line = true;
        for (i, c) in joined.char_indices().skip_while(|(i, _)| *i < m.end() - 1) {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            if depth == 0 && i < before_here {
                open_at_line = false;
                break;
            }
        }
        if open_at_line {
            return Some(joined);
        }
    }
    None
}

/// Paths an include context resolves to, and whether any include in it could
/// not be resolved.
fn resolve_includes(
    ctx: &str,
    file: &str,
    tree: &mut Tree<'_>,
) -> Result<(Vec<String>, bool), String> {
    let total = INCLUDE_MACRO.find_iter(ctx).count();
    let mut seen = 0usize;
    let mut resolved = Vec::new();
    for c in INCLUDE_REL.captures_iter(ctx) {
        seen += 1;
        resolved.extend(join_rel(parent(file), &c[1]));
    }
    let crate_rel: Vec<String> = INCLUDE_CRATE
        .captures_iter(ctx)
        .map(|c| c[1].to_string())
        .collect();
    if !crate_rel.is_empty() {
        let dir = crate_dir(file, tree.listing()?);
        for rel in crate_rel {
            seen += 1;
            if let Some(d) = &dir {
                resolved.extend(join_rel(d, rel.trim_start_matches('/')));
            }
        }
    }
    let unresolved = seen < total || resolved.len() < seen;
    Ok((resolved, unresolved))
}

/// Include macros and other source lines naming each of `targets` in `tree`.
fn scan_includes(
    tree: &mut Tree<'_>,
    targets: &[String],
) -> Result<BTreeMap<String, IncludeScan>, String> {
    let names: Vec<String> = targets
        .iter()
        .map(|t| units::base_name(t).to_string())
        .filter(|n| !n.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let hits: Vec<(String, usize)> = git::grep_fixed_lines(tree.root, &tree.commit, &names)?
        .into_iter()
        .filter(|(p, _)| units::is_source(p))
        .collect();
    let files: Vec<String> = hits.iter().map(|(p, _)| p.clone()).collect();
    tree.load(&files)?;
    let mut out: BTreeMap<String, IncludeScan> = BTreeMap::new();
    for target in targets {
        let name = units::base_name(target);
        let mut scan = IncludeScan::default();
        for (file, line) in &hits {
            if file == target {
                continue;
            }
            let Some(parsed) = tree.get(file) else {
                scan.unresolved.push((file.clone(), *line));
                continue;
            };
            if !parsed.line(*line).is_some_and(|l| l.contains(name)) {
                continue;
            }
            let Some(ctx) = include_context(parsed, *line) else {
                scan.mentions += 1;
                continue;
            };
            let (resolved, unresolved) = resolve_includes(&ctx, file, tree)?;
            if resolved.iter().any(|p| p == target) {
                scan.includers.push((file.clone(), *line));
            } else if unresolved {
                scan.unresolved.push((file.clone(), *line));
            }
        }
        out.insert(target.clone(), scan);
    }
    Ok(out)
}

/// Attribute every changed path to the source units that include it
/// (`include_str!`/`include_bytes!`/`include!`), and fail closed for a
/// non-source path that is neither docs nor fully attributed.
fn attribute_includes<'a>(
    s: &mut Side,
    diffs: &[git::FileDiff],
    head: &mut Tree<'a>,
    base: &mut Tree<'a>,
) {
    #[derive(Default)]
    struct Use {
        includers: usize,
        mentions: usize,
    }
    let mut uses: BTreeMap<String, Use> = BTreeMap::new();
    for (tree, is_head) in [(head, true), (base, false)] {
        let targets: Vec<String> = diffs
            .iter()
            .filter(|d| !units::is_build_manifest(d.path()))
            .filter_map(|d| {
                if is_head {
                    d.new_path.clone()
                } else {
                    d.old_path.clone()
                }
            })
            .collect();
        if targets.is_empty() {
            continue;
        }
        let scans = match scan_includes(tree, &targets) {
            Ok(scans) => scans,
            Err(e) => {
                s.fail(format!("include search failed: {e}"));
                continue;
            }
        };
        for (target, scan) in scans {
            for (file, line) in &scan.unresolved {
                s.fail(format!(
                    "include naming {target} not resolvable: {file}:{line}"
                ));
            }
            let entry = uses.entry(target.clone()).or_default();
            entry.mentions += scan.mentions;
            for (file, line) in scan.includers {
                entry.includers += 1;
                let Some(parsed) = tree.get(&file) else {
                    continue;
                };
                let (key, span) = parsed.unit_at(line);
                if key == WHOLE_FILE && !units::is_test_path(&file) {
                    s.fail(format!(
                        "{target} is included at module level of {file}:{line}"
                    ));
                    continue;
                }
                let (kind, name) = span.map_or_else(
                    || ("file".to_string(), String::new()),
                    |sp| (sp.kind.clone(), sp.name.clone()),
                );
                s.notes
                    .insert(format!("{target} is included by {}", unit(&file, &key)));
                s.changed.entry(unit(&file, &key)).or_insert(Changed {
                    path: file.clone(),
                    key,
                    kind,
                    name,
                    status: "modified",
                });
            }
        }
    }
    for (path, u) in uses {
        if units::is_source(&path) {
            continue;
        }
        let docs = units::is_docs(&path);
        if u.includers == 0 {
            if docs {
                s.notes.insert(format!("docs-only change: {path}"));
            } else {
                s.fail(format!("non-source change without a test mapping: {path}"));
            }
        } else if u.mentions > 0 && !docs {
            s.fail(format!(
                "{path} is also named outside include macros: dependents cannot be named"
            ));
        }
    }
}

/// Result of one dependent search.
pub(super) struct Search {
    found: BTreeMap<String, Caller>,
    /// `false` when the search hit its cap ([`MAX_CALLERS`] in production).
    complete: bool,
    /// The cap the search ran with.
    cap: usize,
    /// Dependents beyond the depth bound were left unexplored.
    unexplored: bool,
    /// Reasons the search could not be trusted (git/read errors, names that
    /// cannot be searched, module-level uses).
    errors: BTreeSet<String>,
}

/// Transitive dependents of `seeds` in `tree`, breadth first, at most `depth`
/// hops, stopping at `cap` units. A dependent is a unit with a call matching a seed (call graph) or a
/// whole-word reference to a seed's name outside imports and comments, so
/// constants, statics, types, fields and traits reach their users too.
pub(super) fn find_callers(
    tree: &mut Tree<'_>,
    seeds: &BTreeMap<String, Changed>,
    depth: usize,
    cap: usize,
) -> Search {
    let mut s = Search {
        found: BTreeMap::new(),
        complete: true,
        cap,
        unexplored: false,
        errors: BTreeSet::new(),
    };
    let mut frontier: Vec<(String, String)> = seeds
        .iter()
        .filter(|(_, c)| c.key != WHOLE_FILE)
        .map(|(u, c)| (c.name.clone(), u.clone()))
        .collect();
    'levels: for level in 1..=depth {
        if frontier.is_empty() {
            break;
        }
        let mut by_segment: HashMap<String, Vec<(String, String)>> = HashMap::new();
        let mut by_word: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (name, target) in &frontier {
            let Some(g) = grep_name(name) else {
                s.errors.insert(format!(
                    "dependents of {target} cannot be searched (name '{name}')"
                ));
                continue;
            };
            by_segment
                .entry(g.to_lowercase())
                .or_default()
                .push((name.clone(), target.clone()));
            by_word.entry(g).or_default().insert(target.clone());
        }
        let names: Vec<String> = by_word.keys().cloned().collect();
        let files: Vec<String> = match git::grep_words(tree.root, &tree.commit, &names) {
            Ok(files) => files.into_iter().filter(|f| units::is_source(f)).collect(),
            Err(e) => {
                s.errors.insert(format!("dependent search failed: {e}"));
                frontier.clear();
                break;
            }
        };
        if let Err(e) = tree.load(&files) {
            s.errors
                .insert(format!("dependent candidates unreadable: {e}"));
            frontier.clear();
            break;
        }
        let mut next: Vec<(String, String)> = Vec::new();
        for file in &files {
            let Some(parsed) = tree.get(file) else {
                s.errors
                    .insert(format!("dependent candidate is not UTF-8 text: {file}"));
                continue;
            };
            let mut hits: BTreeSet<(usize, String)> = BTreeSet::new();
            for (line, callee) in &parsed.calls {
                let Some(targets) = by_segment.get(&callee_segment(callee).to_lowercase()) else {
                    continue;
                };
                for (name, target) in targets {
                    if callee_matches(callee, name) {
                        hits.insert((*line, target.clone()));
                    }
                }
            }
            for (word, targets) in &by_word {
                for line in parsed.reference_lines(word) {
                    for t in targets {
                        hits.insert((line, t.clone()));
                    }
                }
            }
            for (line, target) in hits {
                let (key, span) = parsed.unit_at(line);
                if key == WHOLE_FILE && !units::is_test_path(file) {
                    s.errors.insert(format!(
                        "module-level use of {target} in {file}:{line}: its dependents cannot be named"
                    ));
                    continue;
                }
                let caller = unit(file, &key);
                if caller == target || seeds.contains_key(&caller) || s.found.contains_key(&caller)
                {
                    continue;
                }
                if s.found.len() >= cap {
                    s.complete = false;
                    frontier.clear();
                    break 'levels;
                }
                s.found.insert(
                    caller.clone(),
                    Caller {
                        path: file.clone(),
                        key: key.clone(),
                        depth: level,
                        via: target,
                    },
                );
                // A test is a leaf: nothing depends on it.
                if let Some(span) = span.filter(|sp| !parsed.is_test(sp)) {
                    next.push((span.name.clone(), caller));
                }
            }
        }
        frontier = next;
    }
    s.unexplored = !frontier.is_empty();
    s
}

fn finish_side(side: &mut Side, tree: &mut Tree<'_>, search: Search, depth: usize) {
    side.callers = search.found;
    side.complete = search.complete && search.errors.is_empty();
    if !search.complete {
        side.fail(format!(
            "dependent search stopped at {} units: run the full suite",
            search.cap
        ));
    }
    if search.unexplored {
        side.fail(format!(
            "dependents beyond depth {depth} not followed: run the full suite or raise depth"
        ));
    }
    for e in search.errors {
        side.fail(e);
    }
    select_tests(side, tree);
}

fn add_test(hits: &mut BTreeMap<String, TestHit>, path: &str, key: &str, reason: Reason) {
    let u = unit(path, key);
    if hits.get(&u).is_some_and(|h| h.reason <= reason) {
        return;
    }
    hits.insert(
        u,
        TestHit {
            path: path.to_string(),
            key: key.to_string(),
            reason,
        },
    );
}

/// Classify one unit of `path` that a change reaches: a test function is
/// listed itself; anything else in a test file lists the whole file.
fn add_reached(
    hits: &mut BTreeMap<String, TestHit>,
    tree: &Tree<'_>,
    path: &str,
    key: &str,
    reason: Reason,
) {
    let test_fn = tree.get(path).and_then(|p| {
        p.spans
            .iter()
            .find(|s| p.key_of_span(s) == key)
            .filter(|s| p.is_test(s))
    });
    if test_fn.is_some() {
        add_test(hits, path, key, reason);
    } else if units::is_test_path(path) && units::is_source(path) {
        add_test(hits, path, WHOLE_FILE, reason);
    }
}

/// Affected tests of `side`, always including the security tests. A side
/// whose security set cannot be resolved (or is not fully present in the
/// selection) fails closed.
pub(super) fn select_tests(side: &mut Side, tree: &mut Tree<'_>) {
    let mut hits = BTreeMap::new();
    for c in side.changed.values() {
        if c.status == "removed" {
            continue;
        }
        if c.key != WHOLE_FILE {
            add_reached(&mut hits, tree, &c.path, &c.key, Reason::Changed);
            continue;
        }
        // Module-level change: every test of the file may observe it.
        match tree.get(&c.path) {
            Some(p) if !p.tests().is_empty() => {
                for t in p.tests() {
                    add_test(&mut hits, &c.path, &p.key_of_span(t), Reason::SameFile);
                }
            }
            _ if units::is_test_path(&c.path) && units::is_source(&c.path) => {
                add_test(&mut hits, &c.path, WHOLE_FILE, Reason::Changed);
            }
            _ => {}
        }
    }
    for c in side.callers.values() {
        add_reached(&mut hits, tree, &c.path, &c.key, Reason::Reaches);
    }
    match security_tests(tree) {
        Ok(security) => {
            for (path, key) in &security {
                add_test(&mut hits, path, key, Reason::Security);
            }
            if let Some((path, key)) = security
                .iter()
                .find(|(p, k)| !hits.contains_key(&unit(p, k)))
            {
                side.fail(format!(
                    "security test missing from the selection: {}",
                    unit(path, key)
                ));
            }
        }
        Err(e) => side.fail(format!("security tests could not be resolved: {e}")),
    }
    side.tests = hits;
}

/// Tests that are never dropped: every test in a file whose path names a
/// security area, and every test named after one. A security file that
/// cannot be read or parsed is selected whole (`*`); a git failure is an error.
pub(super) fn security_tests(tree: &mut Tree<'_>) -> Result<Vec<(String, String)>, String> {
    let by_path: BTreeSet<String> = tree
        .listing()?
        .iter()
        .filter(|p| units::is_source(p) && SECURITY.is_match(p))
        .cloned()
        .collect();
    let by_decl: BTreeSet<String> =
        git::grep_regex(tree.root, &tree.commit, SECURITY_DECL_PATTERNS)?
            .into_iter()
            .filter(|p| units::is_source(p))
            .collect();
    let all: Vec<String> = by_path.union(&by_decl).cloned().collect();
    tree.load(&all)?;
    let mut out = Vec::new();
    for path in &all {
        let Some(parsed) = tree.get(path) else {
            out.push((path.clone(), WHOLE_FILE.to_string()));
            continue;
        };
        let path_hit = by_path.contains(path);
        let mut any = false;
        for t in parsed.tests() {
            if path_hit || SECURITY.is_match(&t.name) {
                out.push((path.clone(), parsed.key_of_span(t)));
                any = true;
            }
        }
        if !any && units::is_test_path(path) {
            out.push((path.clone(), WHOLE_FILE.to_string()));
        }
    }
    Ok(out)
}

fn same_unit(a: &Changed, b: &Changed) -> bool {
    if a.path != b.path {
        return false;
    }
    if a.key == b.key {
        return true;
    }
    a.key != WHOLE_FILE
        && b.key != WHOLE_FILE
        && (b.key.starts_with(&format!("{}/", a.key)) || a.key.starts_with(&format!("{}/", b.key)))
}

/// The changed unit a caller chain of `side` ends in.
fn chain_root(
    changed: &BTreeMap<String, Changed>,
    callers: &BTreeMap<String, Caller>,
    start: &str,
) -> Option<String> {
    let mut cur = start.to_string();
    for _ in 0..=super::MAX_DEPTH {
        let c = callers.get(&cur)?;
        if changed.contains_key(&c.via) {
            return Some(c.via.clone());
        }
        cur.clone_from(&c.via);
    }
    None
}

fn overlap_pairs(
    head: &Side,
    other: &Side,
    head_in_other: &BTreeMap<String, Caller>,
    other_in_head: &BTreeMap<String, Caller>,
) -> Vec<Pair> {
    let mut pairs: BTreeSet<Pair> = BTreeSet::new();
    for (ua, a) in &head.changed {
        for (ub, b) in &other.changed {
            if same_unit(a, b) {
                pairs.insert(Pair {
                    kind: "same_unit",
                    head_unit: ua.clone(),
                    other_unit: ub.clone(),
                    hops: 0,
                    via: None,
                });
            }
        }
    }
    // A unit changed on `other` calls a unit changed on head, in either tree.
    for ub in other.changed.keys() {
        for callers in [&head.callers, head_in_other] {
            if let Some(c) = callers.get(ub)
                && let Some(root) = chain_root(&head.changed, callers, ub)
            {
                pairs.insert(Pair {
                    kind: "other_calls_head",
                    head_unit: root,
                    other_unit: ub.clone(),
                    hops: c.depth,
                    via: None,
                });
            }
        }
    }
    for ua in head.changed.keys() {
        for callers in [&other.callers, other_in_head] {
            if let Some(c) = callers.get(ua)
                && let Some(root) = chain_root(&other.changed, callers, ua)
            {
                pairs.insert(Pair {
                    kind: "head_calls_other",
                    head_unit: ua.clone(),
                    other_unit: root,
                    hops: c.depth,
                    via: None,
                });
            }
        }
    }
    // One unit directly calls a unit changed on each side.
    for (u, c) in head.callers.iter().filter(|(_, c)| c.depth == 1) {
        if let Some(o) = other.callers.get(u).filter(|o| o.depth == 1) {
            pairs.insert(Pair {
                kind: "shared_caller",
                head_unit: c.via.clone(),
                other_unit: o.via.clone(),
                hops: 1,
                via: Some(u.clone()),
            });
        }
    }
    // Keep the shortest chain per (kind, head, other).
    let mut best: BTreeMap<(&'static str, String, String, Option<String>), Pair> = BTreeMap::new();
    for p in pairs {
        let k = (
            p.kind,
            p.head_unit.clone(),
            p.other_unit.clone(),
            p.via.clone(),
        );
        if best.get(&k).is_none_or(|b| p.hops < b.hops) {
            best.insert(k, p);
        }
    }
    best.into_values().collect()
}
