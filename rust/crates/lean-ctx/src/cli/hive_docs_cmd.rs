// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hive docs import <export.json> [--dir NAME=PREFIX]… [--dry-run]`:
//! moves an emspace space into the checkout's linked Hive vault as C0 2.2 doc
//! pages ([`lean_ctx_doc::emspace`]).
//!
//! Every page keeps its body byte for byte, split into sections; `/slug`
//! links become `hive:<id>` links and `links_to` edges; each source becomes
//! an anchor pinned at the commit emspace pinned it at (`pinnedRef`), so the
//! first re-check against `HEAD` shows the same stale pages emspace did.
//! Open notes come along. A page whose slug the vault already holds is left
//! alone, so a second run adds only what is missing. `--dry-run` writes
//! nothing and prints the report.
//!
//! `lean-ctx hive docs import --markdown <file|dir>… [--dry-run]` makes the
//! checkout's own markdown docs Hive pages: one page per tracked `.md` file
//! (its path is the slug, a `README`/`index` names its directory), sections
//! per heading, anchored to the file at `HEAD`. A file whose page exists
//! revises it section by section and confirms the anchor, so a re-run after
//! an edit leaves the page fresh.

use std::collections::BTreeMap;

use lean_ctx_doc::emspace::{Export, Plan, plan};
use lean_ctx_doc::links;
use lean_ctx_doc::write::{self, EventSpec};

use lean_ctx_hive::HiveError;
use lean_ctx_hive::docs::{self, GitAt};

pub(super) const USAGE: &str = "\
  lean-ctx hive docs import --markdown <file|dir>... [--dry-run]
    makes this checkout's markdown files pages of its Hive vault
    (one page per tracked .md file, sections per heading, anchored to the file)
  lean-ctx hive docs import <export.json> [--dir NAME=PREFIX]... [--dry-run]
    moves an emspace space export into this checkout's Hive vault
    (NAME is the emspace directory name, PREFIX its path in this checkout; repo= for the root)";

/// `--markdown` arguments: the files or directories, and `--dry-run`.
struct MarkdownOpts {
    paths: Vec<String>,
    dry_run: bool,
}

/// `Some` for `import --markdown …`, `None` for an emspace import.
fn parse_markdown(args: &[String]) -> Result<Option<MarkdownOpts>, HiveError> {
    if args.first().map(String::as_str) != Some("import") || !args.iter().any(|a| a == "--markdown")
    {
        return Ok(None);
    }
    let usage = || HiveError::Config(format!("usage:\n{USAGE}"));
    let mut paths = Vec::new();
    let mut dry_run = false;
    for a in &args[1..] {
        match a.as_str() {
            "--markdown" => {}
            "--dry-run" => dry_run = true,
            s if !s.starts_with("--") => paths.push(s.to_owned()),
            _ => return Err(usage()),
        }
    }
    if paths.is_empty() {
        return Err(usage());
    }
    Ok(Some(MarkdownOpts { paths, dry_run }))
}

/// The tracked markdown files `paths` name (a file, or every `.md` below a
/// directory; `.` is the checkout), repo-relative, sorted.
fn markdown_files(tracked: &[String], paths: &[String]) -> Result<Vec<String>, HiveError> {
    let mut out = Vec::new();
    for p in paths {
        let p = p.trim_start_matches("./").trim_end_matches('/');
        let p = if p == "." { "" } else { p };
        if p.contains("..") || p.starts_with('/') {
            return Err(HiveError::Config(format!("{p}: repo-relative paths only")));
        }
        let before = out.len();
        for f in tracked.iter().filter(|f| is_markdown(f)) {
            if f == p || p.is_empty() || f.starts_with(&format!("{p}/")) {
                out.push(f.clone());
            }
        }
        if out.len() == before {
            return Err(HiveError::Config(format!(
                "{}: no tracked markdown file there",
                if p.is_empty() { "." } else { p }
            )));
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn is_markdown(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

/// The page slug of a markdown file: its path without the extension, each
/// segment lowercase words joined by `-`; a `README` or `index` file names
/// its directory. `None` when no valid C0 slug comes out.
fn slug_of(path: &str) -> Option<String> {
    let stem = path.rsplit_once('.').map_or(path, |(stem, _)| stem);
    let mut segs: Vec<String> = stem.split('/').map(slug_segment).collect();
    if segs.len() > 1 && segs.last().is_some_and(|s| s == "readme" || s == "index") {
        segs.pop();
    }
    let slug = segs.join("/");
    lean_ctx_server::tools::ctx_doc::valid_slug(&slug).then_some(slug)
}

fn slug_segment(seg: &str) -> String {
    let mut out = String::new();
    for c in seg.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

/// The page markdown of `file`: its text, with a `# Title` from the file
/// name when it has none.
fn page_markdown(file: &str, text: &str) -> String {
    if text.trim_start().starts_with("# ") {
        return text.to_owned();
    }
    let name = file.rsplit('/').next().unwrap_or(file);
    let name = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let title: String = name
        .split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("# {title}\n\n{text}")
}

/// The events that make `files` pages of `space`: a new page per file
/// without one, a revision plus an anchor confirm for a file whose page
/// exists. A section `screen` refuses is held back alone (the page keeps
/// the rest); a refused title holds back the file. Returns the events,
/// `(created, revised, unchanged)` and what was skipped or held back, with
/// file, heading and reason.
#[allow(clippy::type_complexity)]
fn markdown_events(
    root: &str,
    tracked: &[String],
    files: &[String],
    space: &lean_ctx_doc::replica::DocSpace,
    verified_at: u64,
    screen: &mut lean_ctx_hive::knowledge::screening::Screen,
) -> Result<(Vec<EventSpec>, (usize, usize, usize), Vec<String>), HiveError> {
    use lean_ctx_doc::{Page, PageStatus, sections};
    let mut out = Vec::new();
    let (mut created, mut revised, mut unchanged) = (0, 0, 0);
    let mut skipped = Vec::new();
    let mut new_id = docs::new_id;
    for file in files {
        let Some(slug) = slug_of(file) else {
            skipped.push(format!("{file}: no valid page slug"));
            continue;
        };
        let text = std::fs::read_to_string(std::path::Path::new(root).join(file))
            .map_err(|e| HiveError::Config(format!("{file}: {e}")))?;
        let markdown = page_markdown(file, &text);
        let anchor = docs::pin(root, tracked, &format!("file:{file}"))?;
        let (stored, _) = links::to_storage(&markdown, |s| space.by_slug.get(s).cloned());
        let (title, all_sections) = sections::split(&stored);
        if let Err(why) = screen.check(&title) {
            skipped.push(format!("{file}: held back, its title ({why})"));
            continue;
        }
        let mut sections = Vec::with_capacity(all_sections.len());
        for s in all_sections {
            match screen.check(&s.text) {
                Ok(()) => sections.push(s),
                Err(why) => skipped.push(format!(
                    "{file} › {}: section held back ({why})",
                    section_heading(&s.text)
                )),
            }
        }
        // What goes out: the title and the sections screening let through.
        let stored = std::iter::once(format!("# {title}\n\n"))
            .chain(sections.iter().map(|s| s.text.clone()))
            .collect::<String>();
        let targets = node_targets(&stored);
        if let Some(page) = space.page_by_slug(&slug) {
            let now = space.links.get(&page.page.id).cloned().unwrap_or_default();
            let mut specs = write::revise_page(page, &stored, &now, &targets, &mut new_id);
            let fresh = docs::page_anchors(page) == [anchor.clone()];
            if specs.is_empty() && fresh {
                unchanged += 1;
                continue;
            }
            specs.extend(write::confirm(page, &[anchor], verified_at));
            revised += 1;
            out.extend(specs);
            continue;
        }
        let page = Page {
            slug,
            title,
            kind: "reference".to_owned(),
            status: PageStatus::Canonical,
            sections,
            anchors: vec![anchor],
            notes: Vec::new(),
        };
        let (specs, _) = write::create_page(&page, &targets, &mut new_id);
        created += 1;
        out.extend(specs);
    }
    Ok((out, (created, revised, unchanged), skipped))
}

/// The heading a section starts with, or `(lead)` for the text before the
/// first heading.
fn section_heading(text: &str) -> String {
    text.lines()
        .next()
        .and_then(|l| l.strip_prefix('#'))
        .map(|l| l.trim_start_matches('#').trim().to_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "(lead)".to_owned())
}

fn node_targets(stored: &str) -> Vec<String> {
    let mut out: Vec<String> = links::targets(stored)
        .into_iter()
        .filter_map(|t| match links::classify(t) {
            links::Target::Node { id, .. } => Some(id.to_owned()),
            _ => None,
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Runs `hive docs import --markdown …` for the checkout at `root`.
fn run_markdown(opts: &MarkdownOpts, root: &str) -> Result<Vec<String>, HiveError> {
    let tracked = docs::tracked_files(root)
        .ok_or_else(|| HiveError::Config("markdown pages need a git checkout".into()))?;
    let files = markdown_files(&tracked, &opts.paths)?;
    let verified_at = chrono::Utc::now().timestamp_millis().unsigned_abs();
    let report = |label: &str, n: usize, (c, r, u): (usize, usize, usize), skipped: &[String]| {
        let mut lines = vec![format!(
            "{label}: {} markdown file(s): {c} new page(s), {r} revised, {u} unchanged; {n} event(s)",
            files.len()
        )];
        if !skipped.is_empty() {
            lines.push(format!("skipped: {}", skipped.len()));
            lines.extend(skipped.iter().take(40).map(|l| format!("  {l}")));
        }
        lines
    };
    if opts.dry_run {
        let empty = lean_ctx_doc::replica::DocSpace::default();
        let mut screen = lean_ctx_hive::knowledge::screening::Screen::for_installation();
        let (specs, counts, skipped) =
            markdown_events(root, &tracked, &files, &empty, verified_at, &mut screen)?;
        let mut lines = report(
            "dry run (as for an empty vault)",
            specs.len(),
            counts,
            &skipped,
        );
        lines.push("nothing written".into());
        return Ok(lines);
    }
    docs::require_docs_protocol(&lean_ctx_hive::HiveStore::open()?)?;
    let lines = docs::with_vault(root, |v| {
        let space = docs::space(v.target.replica)?;
        let mut screen = lean_ctx_hive::knowledge::screening::Screen::for_installation();
        let (specs, counts, skipped) =
            markdown_events(root, &tracked, &files, &space, verified_at, &mut screen)?;
        let n = specs.len();
        let now = chrono::Utc::now().timestamp_millis().unsigned_abs();
        docs::queue_specs(&v.target, specs, &mut screen, now)?;
        Ok(report(&format!("{}: queued", v.label), n, counts, &skipped))
    })?;
    let mut lines = lines;
    lines.push(docs::push_queued(root)?);
    Ok(lines)
}

struct Opts {
    export: String,
    dirs: BTreeMap<String, String>,
    dry_run: bool,
}

fn parse(args: &[String]) -> Result<Opts, HiveError> {
    let usage = || HiveError::Config(format!("usage:\n{USAGE}"));
    let mut it = args.iter();
    match it.next().map(String::as_str) {
        Some("import") => {}
        _ => return Err(usage()),
    }
    let mut export = None;
    let mut dirs = BTreeMap::new();
    let mut dry_run = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dry-run" => dry_run = true,
            "--dir" => {
                let v = it.next().ok_or_else(usage)?;
                let (k, p) = v.split_once('=').ok_or_else(usage)?;
                dirs.insert(k.to_owned(), p.trim_matches('/').to_owned());
            }
            s if !s.starts_with("--") && export.is_none() => export = Some(s.to_owned()),
            _ => return Err(usage()),
        }
    }
    if dirs.is_empty() {
        dirs.insert("repo".into(), String::new());
    }
    Ok(Opts {
        export: export.ok_or_else(usage)?,
        dirs,
        dry_run,
    })
}

/// Re-pins every anchor of `plan` at its emspace commit (or at `HEAD` when
/// the commit is not here, marking moved sources `to_verify`). Returns the
/// number pinned at their commit, at `HEAD`, and gone at their commit.
fn repin(root: &str, plan: &mut Plan) -> (usize, usize, usize) {
    let head_files = docs::tracked_files(root).unwrap_or_default();
    let head = docs::head_commit(root);
    let mut at: BTreeMap<String, Option<GitAt>> = BTreeMap::new();
    let (mut exact, mut fallback, mut gone) = (0, 0, 0);
    for p in &mut plan.pages {
        for (a, moved) in p.page.anchors.iter_mut().zip(p.moved.iter_mut()) {
            let digest = a.commit.clone().and_then(|c| {
                at.entry(c.clone())
                    .or_insert_with(|| GitAt::open(root, &c))
                    .as_mut()
                    .map(|g| g.digest(a))
            });
            match digest {
                Some(Some(d)) => {
                    a.digest = d;
                    exact += 1;
                    *moved = false;
                }
                Some(None) => {
                    // Gone already at its own commit: pin what HEAD has, stale.
                    a.digest = docs::digest_now(root, &head_files, a).unwrap_or_default();
                    a.commit.clone_from(&head);
                    gone += 1;
                    *moved = true;
                }
                None => {
                    a.digest = docs::digest_now(root, &head_files, a).unwrap_or_default();
                    a.commit.clone_from(&head);
                    fallback += 1;
                }
            }
        }
    }
    (exact, fallback, gone)
}

/// The events that create every planned page the vault does not hold yet;
/// the number of pages skipped because their slug is taken.
fn events(plan: &Plan, taken: &BTreeMap<String, String>) -> (Vec<EventSpec>, usize, Vec<String>) {
    let new_id = docs::new_id;
    let mut ids: BTreeMap<String, String> = taken.clone();
    let fresh: Vec<&lean_ctx_doc::emspace::PagePlan> = plan
        .pages
        .iter()
        .filter(|p| !taken.contains_key(&p.page.slug))
        .collect();
    for p in &fresh {
        ids.insert(p.page.slug.clone(), new_id("n"));
    }
    let mut out = Vec::new();
    let mut dangling = Vec::new();
    for p in &fresh {
        let mut page = p.page.clone();
        let mut targets = Vec::new();
        for s in &mut page.sections {
            let (stored, d) = links::to_storage(&s.text, |slug| ids.get(slug).cloned());
            s.text = stored;
            dangling.extend(d.into_iter().map(|t| format!("{}: {t}", page.slug)));
            for t in links::targets(&s.text) {
                if let links::Target::Node { id, .. } = links::classify(t) {
                    targets.push(id.to_owned());
                }
            }
        }
        targets.sort();
        targets.dedup();
        let id = ids[&page.slug].clone();
        // The page id was chosen above (links need it before the page
        // exists): `create_page` asks for it first.
        let mut page_id = Some(id.clone());
        let mut choose = |prefix: &str| match page_id.take() {
            Some(chosen) if prefix == "n" => chosen,
            other => {
                page_id = other;
                new_id(prefix)
            }
        };
        let (mut specs, _) = write::create_page(&page, &targets, &mut choose);
        // A source that had moved in emspace starts as to_verify (no drift
        // detail yet: the first re-check adds it).
        if p.moved.iter().any(|m| *m) {
            let mut body = serde_json::Map::new();
            body.insert("field".into(), "anchor_state".into());
            body.insert("value".into(), "to_verify".into());
            specs.push(EventSpec {
                op: "field.set",
                target: id.clone(),
                node_type: None,
                body,
                base_event: None,
            });
        }
        out.extend(specs);
    }
    (out, plan.pages.len() - fresh.len(), dangling)
}

/// Runs `hive docs …` for the checkout at `root`.
pub(super) fn run(args: &[String], root: &str) -> Result<Vec<String>, HiveError> {
    if let Some(opts) = parse_markdown(args)? {
        return run_markdown(&opts, root);
    }
    let opts = parse(args)?;
    let text = std::fs::read_to_string(&opts.export)
        .map_err(|e| HiveError::Config(format!("{}: {e}", opts.export)))?;
    let export: Export = serde_json::from_str(&text)
        .map_err(|e| HiveError::Config(format!("{}: not an emspace export ({e})", opts.export)))?;
    let checkout = std::path::Path::new(root);
    let mut plan = plan(&export, &opts.dirs, &|p| checkout.join(p).is_dir());
    let (exact, fallback, gone) = repin(root, &mut plan);
    let r = &plan.report;
    let mut lines = vec![
        format!(
            "{}: {} page(s) ({} retired left behind), {} section(s), {} anchor(s), {} note(s); {} task(s) wait for task fields (C0 2.2)",
            export.space, r.pages, r.retired_skipped, r.sections, r.anchors, r.notes, r.tasks
        ),
        format!(
            "anchors: {exact} pinned at their emspace commit, {fallback} at HEAD, {gone} gone at their commit"
        ),
    ];
    for (label, list) in [
        ("not lossless", r.not_lossless.clone()),
        (
            "unknown status",
            r.unknown_status
                .iter()
                .map(|(p, s)| format!("{p}: {s}"))
                .collect(),
        ),
        (
            "unmapped sources",
            r.unmapped_sources
                .iter()
                .map(|(p, s)| format!("{p}: {s}"))
                .collect(),
        ),
    ] {
        if !list.is_empty() {
            lines.push(format!("{label}: {}", list.len()));
            lines.extend(list.iter().take(10).map(|l| format!("  {l}")));
        }
    }
    if opts.dry_run {
        let (specs, _, dangling) = events(&plan, &BTreeMap::new());
        lines.push(format!(
            "dry run: {} event(s) would be queued; nothing written",
            specs.len()
        ));
        if !dangling.is_empty() {
            lines.push(format!("dangling links: {}", dangling.len()));
            lines.extend(dangling.iter().take(10).map(|l| format!("  {l}")));
        }
        return Ok(lines);
    }
    docs::require_docs_protocol(&lean_ctx_hive::HiveStore::open()?)?;
    let queued = docs::with_vault(root, |v| {
        let space = docs::space(v.target.replica)?;
        let (specs, skipped, dangling) = events(&plan, &space.by_slug);
        let now = chrono::Utc::now().timestamp_millis().unsigned_abs();
        let mut screen = lean_ctx_hive::knowledge::screening::Screen::for_installation();
        let n = specs.len();
        docs::queue_specs(&v.target, specs, &mut screen, now)?;
        Ok((v.label.clone(), n, skipped, dangling.len()))
    })?;
    let (label, n, skipped, dangling) = queued;
    lines.push(format!(
        "{label}: queued {n} event(s); {skipped} page(s) already there; {dangling} dangling link(s) kept as written"
    ));
    lines.push(docs::push_queued(root)?);
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> lean_ctx_hive::knowledge::screening::Screen {
        lean_ctx_hive::knowledge::screening::Screen::for_installation()
    }

    /// A checkout with `files` (path, text) committed.
    fn checkout(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let p = dir.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        for args in [
            &["init", "-q"][..],
            &["add", "."],
            &["commit", "-qm", "docs"],
        ] {
            let ok = std::process::Command::new("git")
                .args(["-c", "user.name=LC", "-c", "user.email=lc@hive.local"])
                .args(args)
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        }
        dir
    }

    /// Dogfood 03.11.: one screening hit dropped a whole file. A section
    /// with a secret is held back alone and named; the rest of the page goes.
    #[test]
    fn a_screened_section_is_held_back_alone() {
        let token = format!("github_pat_{}", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4");
        let text = format!(
            "# Deploy\n\nDeploys run from main.\n\n## Secrets\n\nUse token {token} for the API.\n\n## Rollback\n\nRevert the tag and redeploy.\n"
        );
        let dir = checkout(&[("docs/deploy.md", &text)]);
        let root = dir.path().to_str().unwrap();
        let tracked = docs::tracked_files(root).unwrap();
        let files = markdown_files(&tracked, &["docs".to_owned()]).unwrap();
        let space = lean_ctx_doc::replica::DocSpace::default();
        let (specs, counts, skipped) =
            markdown_events(root, &tracked, &files, &space, 1, &mut screen()).unwrap();
        assert_eq!(counts, (1, 0, 0), "the page still goes out");
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        assert!(
            skipped[0].starts_with("docs/deploy.md › Secrets: section held back"),
            "{}",
            skipped[0]
        );
        let bodies: Vec<String> = specs
            .iter()
            .filter(|s| s.node_type == Some("section"))
            .map(|s| s.body["body"].to_string())
            .collect();
        assert!(
            bodies.iter().any(|b| b.contains("Revert the tag")),
            "{bodies:?}"
        );
        assert!(
            bodies.iter().all(|b| !b.contains("github_pat_")),
            "{bodies:?}"
        );
        docs::screen_all(&specs, &mut screen()).expect("what goes out passes screening");
    }

    /// The dry run over a real export: `LC_EMSPACE_EXPORT=<export.json>`
    /// `LC_EMSPACE_ROOT=<checkout>` [`LC_EMSPACE_DIR=name=prefix`]; skipped
    /// without them (CI).
    #[test]
    fn dry_run_over_a_real_export() {
        let (Ok(export), Ok(root)) = (
            std::env::var("LC_EMSPACE_EXPORT"),
            std::env::var("LC_EMSPACE_ROOT"),
        ) else {
            return;
        };
        let mut args = vec!["import".to_owned(), export, "--dry-run".to_owned()];
        if let Ok(dir) = std::env::var("LC_EMSPACE_DIR") {
            args.extend(["--dir".to_owned(), dir]);
        }
        let started = std::time::Instant::now();
        let lines = run(&args, &root).unwrap();
        for l in &lines {
            println!("DRYRUN {l}");
        }
        println!("DRYRUN took {:?}", started.elapsed());
    }

    #[test]
    fn arguments() {
        let a = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let o = parse(&a(&[
            "import",
            "x.json",
            "--dir",
            "hive=services/hive",
            "--dry-run",
        ]))
        .unwrap();
        assert_eq!((o.export.as_str(), o.dry_run), ("x.json", true));
        assert_eq!(o.dirs["hive"], "services/hive");
        assert_eq!(parse(&a(&["import", "x.json"])).unwrap().dirs["repo"], "");
        assert!(parse(&a(&["import"])).is_err() && parse(&a(&["export", "x"])).is_err());
    }

    #[test]
    fn markdown_files_become_slugs_from_their_paths() {
        assert_eq!(
            slug_of("docs/guides/Release Notes.md").as_deref(),
            Some("docs/guides/release-notes")
        );
        assert_eq!(slug_of("docs/hive/README.md").as_deref(), Some("docs/hive"));
        assert_eq!(slug_of("docs/index.markdown").as_deref(), Some("docs"));
        assert_eq!(slug_of("README.md").as_deref(), Some("readme"));
        assert_eq!(slug_of("a/b/c/d/e/f/g/h/i.md"), None, "at most 8 segments");
        assert_eq!(slug_of("docs/---.md"), None);
    }

    #[test]
    fn markdown_arguments_and_file_selection() {
        let a = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let o = parse_markdown(&a(&[
            "import",
            "--markdown",
            "docs",
            "README.md",
            "--dry-run",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!((o.paths.len(), o.dry_run), (2, true));
        assert!(parse_markdown(&a(&["import", "x.json"])).unwrap().is_none());
        assert!(parse_markdown(&a(&["import", "--markdown"])).is_err());

        let tracked = a(&[
            "README.md",
            "docs/a.md",
            "docs/sub/b.MD",
            "docs/c.txt",
            "src/lib.rs",
        ]);
        assert_eq!(
            markdown_files(&tracked, &a(&["docs"])).unwrap(),
            a(&["docs/a.md", "docs/sub/b.MD"])
        );
        assert_eq!(markdown_files(&tracked, &a(&["."])).unwrap().len(), 3);
        assert_eq!(
            markdown_files(&tracked, &a(&["README.md", "./docs/"]))
                .unwrap()
                .len(),
            3
        );
        assert!(
            markdown_files(&tracked, &a(&["src"])).is_err(),
            "no markdown there"
        );
        assert!(markdown_files(&tracked, &a(&["../x"])).is_err());
    }

    #[test]
    fn a_file_without_a_title_gets_one_from_its_name() {
        assert_eq!(
            page_markdown("docs/release-steps.md", "Run it.\n"),
            "# Release Steps\n\nRun it.\n"
        );
        assert_eq!(
            page_markdown("docs/x.md", "# Own\n\nBody.\n"),
            "# Own\n\nBody.\n"
        );
    }

    /// One page per file in a real checkout: sections per heading, the file
    /// as its pinned anchor, canonical.
    #[test]
    fn markdown_files_become_new_pages_anchored_to_their_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(
            root.join("docs/retry.md"),
            "# The Retry Budget\n\nPayments retry a failed charge.\n\n## Budget\n\nThree retries.\n",
        )
        .unwrap();
        std::fs::write(root.join("docs/notes.md"), "Plain notes.\n").unwrap();
        for args in [
            &["init", "-q"][..],
            &["add", "."],
            &["commit", "-qm", "docs"],
        ] {
            let ok = std::process::Command::new("git")
                .args(["-c", "user.name=LC", "-c", "user.email=lc@hive.local"])
                .args(args)
                .current_dir(root)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        }
        let root = root.to_str().unwrap();
        let tracked = docs::tracked_files(root).unwrap();
        let files = markdown_files(&tracked, &["docs".to_owned()]).unwrap();
        let space = lean_ctx_doc::replica::DocSpace::default();
        let (specs, counts, skipped) =
            markdown_events(root, &tracked, &files, &space, 1, &mut screen()).unwrap();
        assert_eq!((counts, skipped.len()), ((2, 0, 0), 0));
        let pages: Vec<_> = specs
            .iter()
            .filter(|s| s.node_type == Some("page"))
            .collect();
        assert_eq!(pages.len(), 2);
        let retry = pages
            .iter()
            .find(|p| p.body.get("slug") == Some(&"docs/retry".into()))
            .unwrap();
        assert_eq!(retry.body["title"], "The Retry Budget");
        assert_eq!(retry.body["status"], "canonical");
        let anchors = retry.body["anchors"].to_string();
        assert!(anchors.contains("docs/retry.md"), "{anchors}");
        let sections = specs
            .iter()
            .filter(|s| s.node_type == Some("section"))
            .count();
        assert!(sections >= 3, "two headings and one plain file: {sections}");
        assert!(
            pages
                .iter()
                .any(|p| p.body.get("title") == Some(&"Notes".into())),
            "a file without a title is named after itself"
        );
    }

    #[test]
    fn links_resolve_to_the_ids_chosen_for_the_import() {
        let export: Export = serde_json::from_value(serde_json::json!({
            "space": "s",
            "pages": [
                {"slug": "a", "type": "guide", "status": "canonical", "body": "# A\nsee [B](/b#x)\n"},
                {"slug": "b", "type": "guide", "status": "draft", "body": "# B\n## X\nx\n"}
            ]
        }))
        .unwrap();
        let p = plan(&export, &BTreeMap::new(), &|_| false);
        let (specs, skipped, dangling) = events(&p, &BTreeMap::new());
        assert_eq!((skipped, dangling.len()), (0, 0));
        let page_b = specs
            .iter()
            .find(|s| s.body.get("slug") == Some(&serde_json::Value::from("b")))
            .unwrap()
            .target
            .clone();
        let section_a = specs
            .iter()
            .find(|s| s.node_type == Some("section"))
            .unwrap();
        assert_eq!(
            section_a.body["body"],
            format!("see [B](hive:{page_b}#x)\n")
        );
        assert!(
            specs
                .iter()
                .any(|s| s.body.get("kind") == Some(&"links_to".into()) && s.body["to"] == page_b)
        );
        let taken = BTreeMap::from([("a".to_owned(), "n_existing".to_owned())]);
        let (_, skipped, _) = events(&p, &taken);
        assert_eq!(skipped, 1, "a second run adds only what is missing");
    }
}
