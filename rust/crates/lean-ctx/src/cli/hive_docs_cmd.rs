// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hive docs import --from-emspace <export dir> [--vault @NS/NAME]
//! [--dir NAME=PREFIX]… [--apply]`: moves an emspace space (what `emspace
//! export <dir>` writes) into the checkout's linked Hive vault as C0 2.2 doc
//! pages ([`lean_ctx_doc::emspace`]) and its open tasks as task proposals.
//! The older `<export.json>` form takes the JSON an emspace program writes.
//!
//! Every page keeps its body byte for byte, split into sections; `/slug`
//! links become `hive:<id>` links and `links_to` edges; each source becomes
//! an anchor pinned at the commit emspace pinned it at (`pinnedRef`), so the
//! first re-check against `HEAD` shows the same stale pages emspace did.
//! Open notes come along. A page whose slug the vault already holds is left
//! alone, so a second run adds only what is missing; a page screening
//! refuses is held back alone and named. Open tasks (`backlog`, `todo`,
//! `doing`) become agent task proposals; done and dropped ones stay behind.
//!
//! An emspace import is a dry run unless `--apply` is given: it lists every
//! page and task it would create and writes nothing. `--apply` needs
//! `--vault`, and refuses when that is not the vault this checkout is linked
//! to (case aside), or when the export is not the space this checkout's
//! `.emspace` names, so a migration never lands in the wrong vault. It also
//! refuses while any page carries a description: the doc model cannot write
//! one yet and a second run skips pages that exist, so an early import would
//! lose them for good; `--drop-descriptions` imports anyway.
//!
//! `lean-ctx hive docs import --markdown <file|dir>… [--dry-run]` makes the
//! checkout's own markdown docs Hive pages: one page per tracked `.md` file
//! (its path is the slug, a `README`/`index` names its directory), sections
//! per heading, anchored to the file at `HEAD`. A file whose page exists
//! revises it section by section and confirms the anchor, so a re-run after
//! an edit leaves the page fresh.

use std::collections::{BTreeMap, BTreeSet};

use lean_ctx_doc::emspace::{Export, Plan, TaskPlan, plan};
use lean_ctx_doc::links;
use lean_ctx_doc::write::{self, EventSpec};

use lean_ctx_hive::HiveError;
use lean_ctx_hive::docs::{self, GitAt};
use lean_ctx_hive::knowledge::screening::Screen;
use lean_ctx_protocol::hive::tasks as wire;

pub(super) const USAGE: &str = "\
  lean-ctx hive docs import --markdown <file|dir>... [--dry-run]
    makes this checkout's markdown files pages of its Hive vault
    (one page per tracked .md file, sections per heading, anchored to the file)
  lean-ctx hive docs import --from-emspace <export dir> [--vault @NS/NAME] [--dir NAME=PREFIX]... [--apply [--drop-descriptions]]
    moves an emspace space (`emspace export <dir>`) into this checkout's Hive vault:
    pages, sections, open notes, anchors, and open tasks as task proposals.
    A dry run unless --apply; --apply needs --vault, the vault this checkout is linked to,
    and an export of the space this checkout's .emspace names. Page descriptions do not
    come over yet: --apply refuses while a page has one, unless --drop-descriptions.
    (NAME is the emspace directory name, PREFIX its path in this checkout; repo= for the root;
    `<export.json>` instead of --from-emspace reads an emspace program's JSON)";

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

/// Where an emspace import reads from.
#[derive(Debug, PartialEq, Eq)]
enum Source {
    /// The directory `emspace export <dir>` wrote.
    Dir(String),
    /// The JSON an emspace program writes ([`Export`]).
    Json(String),
}

struct Opts {
    source: Source,
    dirs: BTreeMap<String, String>,
    /// `@ns/name` (the `@` optional) the import must land in.
    vault: Option<String>,
    apply: bool,
    /// `--drop-descriptions`: apply although page descriptions are lost.
    drop_descriptions: bool,
}

fn parse(args: &[String]) -> Result<Opts, HiveError> {
    let usage = || HiveError::Config(format!("usage:\n{USAGE}"));
    let mut it = args.iter();
    match it.next().map(String::as_str) {
        Some("import") => {}
        _ => return Err(usage()),
    }
    let mut source = None;
    let mut dirs = BTreeMap::new();
    let mut vault = None;
    let mut apply = false;
    let mut drop_descriptions = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            // A dry run is the default; the flag stays accepted.
            "--dry-run" => {}
            "--apply" => apply = true,
            "--drop-descriptions" => drop_descriptions = true,
            "--dir" => {
                let v = it.next().ok_or_else(usage)?;
                let (k, p) = v.split_once('=').ok_or_else(usage)?;
                dirs.insert(k.to_owned(), p.trim_matches('/').to_owned());
            }
            "--vault" => {
                let v = it.next().ok_or_else(usage)?;
                vault = Some(format!("@{}", v.trim_start_matches('@')));
            }
            "--from-emspace" if source.is_none() => {
                source = Some(Source::Dir(it.next().ok_or_else(usage)?.clone()));
            }
            s if !s.starts_with("--") && source.is_none() => {
                source = Some(Source::Json(s.to_owned()));
            }
            _ => return Err(usage()),
        }
    }
    if apply && vault.is_none() {
        return Err(HiveError::Config(
            "--apply needs --vault @NS/NAME, the vault this import is meant for".into(),
        ));
    }
    if dirs.is_empty() {
        dirs.insert("repo".into(), String::new());
    }
    Ok(Opts {
        source: source.ok_or_else(usage)?,
        dirs,
        vault,
        apply,
        drop_descriptions,
    })
}

/// `--vault` names the linked vault: `@ns/name` labels match case aside.
fn same_vault(linked: &str, want: &str) -> bool {
    linked.eq_ignore_ascii_case(want)
}

/// The space the `.emspace` link of `root` (or of its nearest ancestor
/// that has one) names: `{"space": "<slug>"}`.
fn emspace_link(root: &std::path::Path) -> Option<String> {
    root.ancestors().find_map(|d| {
        let text = std::fs::read_to_string(d.join(".emspace")).ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        v.get("space")?.as_str().map(str::to_owned)
    })
}

/// Why `--apply` must not write this export from the checkout at `root`,
/// checked before anything is written: the export is another checkout's
/// space, or, without `--drop-descriptions`, the import could lose page
/// descriptions for good (a second run skips pages that exist): pages carry
/// one, a page record has no `description` key, a key (nested ones too)
/// looks like a description under another name, or a key is not on the
/// no-description list ([`lean_ctx_doc::emspace::NO_DESCRIPTION_KEYS`]).
/// It fails closed on what it cannot read.
fn apply_refusal(
    export_space: &str,
    report: &lean_ctx_doc::emspace::Report,
    drop_descriptions: bool,
    root: &std::path::Path,
) -> Option<String> {
    match emspace_link(root) {
        None => {
            return Some(format!(
                "this checkout has no .emspace link, so nothing shows that space {export_space} belongs here: nothing written"
            ));
        }
        Some(linked) if linked != export_space => {
            return Some(format!(
                "this checkout's .emspace names space {linked}, not {export_space}: nothing written"
            ));
        }
        Some(_) => {}
    }
    if drop_descriptions {
        return None;
    }
    let tail = "nothing written. Wait for descriptions in the engine, or pass --drop-descriptions to import without them";
    if report.descriptions > 0 {
        return Some(format!(
            "{} page description(s) would be lost (the doc model has no description field yet, and a later run skips pages that exist): {tail}",
            report.descriptions
        ));
    }
    if let Some(first) = report.without_description_key.first() {
        return Some(format!(
            "{} page record(s) have no `description` key (first: {first}), so the export may carry descriptions where the importer does not look: {tail}",
            report.without_description_key.len()
        ));
    }
    if let Some((page, key)) = report.description_like_keys.first() {
        return Some(format!(
            "{} page record key(s) look like a description the importer does not read (first: {page}: {key}): {tail}",
            report.description_like_keys.len()
        ));
    }
    report.unknown_keys.first().map(|(page, key)| {
        format!(
            "{} page record key(s) are not on the list of emspace keys known to hold no description (first: {page}: {key}), so they could hold one: {tail}",
            report.unknown_keys.len()
        )
    })
}

/// Reads the export `source` names.
fn read_export(source: &Source) -> Result<Export, HiveError> {
    let read = |p: &std::path::Path| {
        std::fs::read_to_string(p).map_err(|e| HiveError::Config(format!("{}: {e}", p.display())))
    };
    match source {
        Source::Json(file) => serde_json::from_str(&read(std::path::Path::new(file))?)
            .map_err(|e| HiveError::Config(format!("{file}: not an emspace export ({e})"))),
        Source::Dir(dir) => {
            let dir = std::path::Path::new(dir);
            let space: serde_json::Value = serde_json::from_str(&read(&dir.join("space.json"))?)
                .map_err(|e| {
                    HiveError::Config(format!(
                        "{}: not an emspace export dir ({e})",
                        dir.display()
                    ))
                })?;
            let slug = space["space"]["slug"].as_str().unwrap_or("space");
            // A space without tasks or journal writes no such file.
            let optional = |name: &str| {
                let p = dir.join(name);
                if p.exists() {
                    read(&p)
                } else {
                    Ok(String::new())
                }
            };
            lean_ctx_doc::emspace::from_jsonl(
                slug,
                &read(&dir.join("pages.jsonl"))?,
                &optional("tasks.jsonl")?,
                &optional("journal.jsonl")?,
            )
            .map_err(HiveError::Config)
        }
    }
}

/// The reason screening refuses `page` (title, a section or a note), if it
/// does. An emspace page is held back whole: its sections link to each
/// other and to the page, and a half page would read as the whole one.
fn screened_out(page: &lean_ctx_doc::Page, screen: &mut Screen) -> Option<String> {
    let texts = std::iter::once(("title", page.title.as_str()))
        .chain(page.sections.iter().map(|s| ("a section", s.text.as_str())))
        .chain(page.notes.iter().map(|n| ("a note", n.body.as_str())));
    for (what, text) in texts {
        if let Err(why) = screen.check(text) {
            return Some(format!("{what} ({why})"));
        }
    }
    None
}

/// An emspace task as a Hive task proposal, in the limits of
/// `leanctx.hive.task-propose/v1`. Hive tasks have no priority, board or
/// page fields, so those go into the goal; emspace tasks have no
/// acceptance, so it says the goal is the measure.
#[derive(Debug, PartialEq, Eq)]
struct Proposal {
    source_id: String,
    title: String,
    goal: String,
    acceptance: String,
    /// What was shortened, or why Hive would refuse it.
    problems: Vec<String>,
}

const ACCEPTANCE: &str = "Done when what the goal describes holds. The emspace task had no acceptance criteria of its own: sharpen this when you claim the task.";

fn proposal(space: &str, t: &TaskPlan) -> Proposal {
    let mut problems = Vec::new();
    let mut title: String = t.title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.chars().count() > wire::MAX_TITLE_CHARS {
        title = title.chars().take(wire::MAX_TITLE_CHARS - 1).collect();
        title.push('…');
        problems.push(format!(
            "title shortened to {} characters",
            wire::MAX_TITLE_CHARS
        ));
    }
    let mut about = vec![
        format!("status {}", t.status),
        format!("priority {}", t.priority),
    ];
    about.extend(t.labels.iter().cloned());
    if !t.pages.is_empty() {
        let pages: Vec<String> = t.pages.iter().map(|p| format!("/{p}")).collect();
        about.push(format!("pages {}", pages.join(" ")));
    }
    let footer = format!(
        "\n\nFrom emspace {space} task {} ({}).",
        t.source_id,
        about.join(", ")
    );
    let room = wire::MAX_GOAL_CHARS.saturating_sub(footer.chars().count());
    let body = t.body.trim();
    let body = if body.chars().count() > room {
        problems.push(format!(
            "goal shortened: the body has {} characters, Hive takes {}",
            body.chars().count(),
            wire::MAX_GOAL_CHARS
        ));
        let mut cut: String = body.chars().take(room.saturating_sub(1)).collect();
        cut.push('…');
        cut
    } else {
        body.to_owned()
    };
    let goal = format!("{body}{footer}").trim_start().to_owned();
    let request = wire::TaskProposeRequestV1 {
        schema: wire::TASK_PROPOSE_SCHEMA.into(),
        title: Some(title.clone()),
        goal: Some(goal.clone()),
        acceptance: Some(ACCEPTANCE.into()),
        role: None,
        origin: wire::TaskOriginV1 {
            kind: wire::TaskOriginKind::Agent,
            facts: Vec::new(),
            message: None,
        },
        session_ref: "emspace-import".into(),
    };
    if let Err(e) = request.validate() {
        problems.push(format!("Hive would refuse it: {e}"));
    }
    Proposal {
        source_id: t.source_id.clone(),
        title,
        goal,
        acceptance: ACCEPTANCE.into(),
        problems,
    }
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

/// The events that create every planned page the vault does not hold yet
/// and `held` does not hold back; the number of pages skipped because their
/// slug is taken. A link to a held-back page stays as written (dangling).
fn events(
    plan: &Plan,
    taken: &BTreeMap<String, String>,
    held: &BTreeSet<String>,
) -> (Vec<EventSpec>, usize, Vec<String>) {
    let new_id = docs::new_id;
    let mut ids: BTreeMap<String, String> = taken.clone();
    let fresh: Vec<&lean_ctx_doc::emspace::PagePlan> = plan
        .pages
        .iter()
        .filter(|p| !taken.contains_key(&p.page.slug) && !held.contains(&p.page.slug))
        .collect();
    let skipped = plan
        .pages
        .iter()
        .filter(|p| taken.contains_key(&p.page.slug))
        .count();
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
    (out, skipped, dangling)
}

/// Runs `hive docs …` for the checkout at `root`.
pub(super) fn run(args: &[String], root: &str) -> Result<Vec<String>, HiveError> {
    if let Some(opts) = parse_markdown(args)? {
        return run_markdown(&opts, root);
    }
    let opts = parse(args)?;
    let export = read_export(&opts.source)?;
    let checkout = std::path::Path::new(root);
    let mut plan = plan(&export, &opts.dirs, &|p| checkout.join(p).is_dir());
    let (exact, fallback, gone) = repin(root, &mut plan);
    let mut screen = Screen::for_installation();
    let mut held: BTreeMap<String, String> = plan
        .pages
        .iter()
        .filter_map(|p| screened_out(&p.page, &mut screen).map(|why| (p.page.slug.clone(), why)))
        .collect();
    for slug in &plan.report.invalid_slugs {
        held.insert(slug.clone(), "not a C0 2.2 slug".into());
    }
    let held_slugs: BTreeSet<String> = held.keys().cloned().collect();
    let proposals: Vec<Proposal> = plan
        .tasks
        .iter()
        .map(|t| proposal(&export.space, t))
        .collect();

    let mut lines = report_lines(
        &export.space,
        &plan,
        (exact, fallback, gone),
        &held,
        &proposals,
    );
    if !opts.apply {
        let (specs, _, dangling) = events(&plan, &BTreeMap::new(), &held_slugs);
        lines.push(format!(
            "dangling links (kept as written): {}",
            dangling.len()
        ));
        lines.extend(dangling.iter().map(|l| format!("  {l}")));
        lines.push(format!(
            "dry run (as for an empty vault{}): {} event(s) and {} task proposal(s) would be queued; nothing written. Run again with --apply --vault @NS/NAME to write.",
            opts.vault.as_deref().map(|v| format!(" {v}")).unwrap_or_default(),
            specs.len(),
            proposals.iter().filter(|p| sendable(p)).count()
        ));
        return Ok(lines);
    }

    if let Some(why) = apply_refusal(
        &export.space,
        &plan.report,
        opts.drop_descriptions,
        checkout,
    ) {
        return Err(HiveError::Refused(why));
    }
    let want = opts.vault.clone().unwrap_or_default();
    docs::require_docs_protocol(&lean_ctx_hive::HiveStore::open()?)?;
    let queued = docs::with_vault(root, |v| {
        if !same_vault(&v.label, &want) {
            return Err(HiveError::Refused(format!(
                "this checkout is linked to {}, not {want}: nothing written",
                v.label
            )));
        }
        let space = docs::space(v.target.replica)?;
        let (specs, skipped, dangling) = events(&plan, &space.by_slug, &held_slugs);
        let now = chrono::Utc::now().timestamp_millis().unsigned_abs();
        let n = specs.len();
        docs::queue_specs(&v.target, specs, &mut screen, now)?;
        Ok((v.label.clone(), n, skipped, dangling.len()))
    })?;
    let (label, n, skipped, dangling) = queued;
    lines.push(format!(
        "{label}: queued {n} event(s); {skipped} page(s) already there; {dangling} dangling link(s) kept as written"
    ));
    lines.push(docs::push_queued(root)?);
    let data_dir = lean_ctx_hive::HiveStore::open()?.data_dir().to_path_buf();
    let mut proposed = 0;
    for p in proposals.iter().filter(|p| sendable(p)) {
        match lean_ctx_hive::tasks::propose::propose_agent_task(
            &data_dir,
            checkout,
            &p.title,
            Some(&p.goal),
            &p.acceptance,
            None,
            "emspace-import",
        ) {
            Ok(_) => proposed += 1,
            Err(e) => lines.push(format!("task {} not proposed: {e}", p.source_id)),
        }
    }
    lines.push(format!(
        "{label}: queued {proposed} task proposal(s); `lean-ctx hive sync` sends them"
    ));
    Ok(lines)
}

/// A proposal Hive would take as it is.
fn sendable(p: &Proposal) -> bool {
    !p.problems
        .iter()
        .any(|x| x.starts_with("Hive would refuse"))
}

/// What an emspace import creates, line by line: every page and task, and
/// everything that does not come over as it was.
fn report_lines(
    space: &str,
    plan: &Plan,
    (exact, fallback, gone): (usize, usize, usize),
    held: &BTreeMap<String, String>,
    proposals: &[Proposal],
) -> Vec<String> {
    let r = &plan.report;
    let mut lines = vec![
        format!(
            "{space}: {} page(s) ({} retired left behind), {} section(s), {} anchor(s), {} open note(s); {} open task(s) ({} done or dropped left behind)",
            r.pages,
            r.retired_skipped,
            r.sections,
            r.anchors,
            r.notes,
            r.tasks,
            r.closed_tasks_skipped
        ),
        format!(
            "anchors: {exact} pinned at their emspace commit, {fallback} at HEAD, {gone} gone at their commit"
        ),
    ];
    if r.descriptions > 0 {
        lines.push(format!(
            "page descriptions: {} not carried over (no description field in the doc model yet); --apply refuses without --drop-descriptions",
            r.descriptions
        ));
    }
    let first = |list: &[String]| {
        let mut s = list.iter().take(10).cloned().collect::<Vec<_>>().join(", ");
        if list.len() > 10 {
            s.push_str(", …");
        }
        s
    };
    if !r.without_description_key.is_empty() {
        lines.push(format!(
            "pages without a `description` key: {} ({}); descriptions may sit elsewhere, so --apply refuses without --drop-descriptions",
            r.without_description_key.len(),
            first(&r.without_description_key)
        ));
    }
    if !r.description_like_keys.is_empty() {
        let keys: Vec<String> = r
            .description_like_keys
            .iter()
            .map(|(p, k)| format!("{p}: {k}"))
            .collect();
        lines.push(format!(
            "description-like keys not read: {} ({}); --apply refuses without --drop-descriptions",
            keys.len(),
            first(&keys)
        ));
    }
    // Every unmapped key, once each, with the pages that carry it.
    let mut by_key: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (page, key) in &r.unmapped_keys {
        by_key.entry(key).or_default().push(page.clone());
    }
    if !by_key.is_empty() {
        lines.push(format!("unmapped page record keys: {}", by_key.len()));
        for (key, pages) in &by_key {
            let known = lean_ctx_doc::emspace::no_description_key(key);
            lines.push(format!(
                "  {key}: {} page(s) ({}){}",
                pages.len(),
                first(pages),
                if known {
                    ""
                } else {
                    "; not on the no-description list, --apply refuses without --drop-descriptions"
                }
            ));
        }
    }
    lines.push(format!("pages: {}", plan.pages.len() - held.len()));
    for p in plan
        .pages
        .iter()
        .filter(|p| !held.contains_key(&p.page.slug))
    {
        let pg = &p.page;
        let moved = p.moved.iter().filter(|m| **m).count();
        lines.push(format!(
            "  + /{} [{} {}] \"{}\": {} section(s), {} anchor(s){}, {} note(s)",
            pg.slug,
            match pg.status {
                lean_ctx_doc::PageStatus::Draft => "draft",
                lean_ctx_doc::PageStatus::Canonical => "canonical",
                lean_ctx_doc::PageStatus::Retired => "retired",
            },
            pg.kind,
            pg.title,
            pg.sections.len(),
            pg.anchors.len(),
            if moved > 0 {
                format!(" ({moved} to verify)")
            } else {
                String::new()
            },
            pg.notes.len()
        ));
    }
    lines.push(format!("task proposals: {}", proposals.len()));
    for p in proposals {
        lines.push(format!("  + \"{}\" (emspace {})", p.title, p.source_id));
        lines.extend(p.problems.iter().map(|x| format!("    ! {x}")));
    }
    let held_list: Vec<String> = held.iter().map(|(s, why)| format!("{s}: {why}")).collect();
    for (label, list) in [
        ("held back (screening or slug)", held_list),
        ("not lossless", r.not_lossless.clone()),
        (
            "unknown status (taken as draft)",
            r.unknown_status
                .iter()
                .map(|(p, s)| format!("{p}: {s}"))
                .collect(),
        ),
        (
            "unmapped sources (no anchor)",
            r.unmapped_sources
                .iter()
                .map(|(p, s)| format!("{p}: {s}"))
                .collect(),
        ),
    ] {
        if !list.is_empty() {
            lines.push(format!("{label}: {}", list.len()));
            lines.extend(list.iter().map(|l| format!("  {l}")));
        }
    }
    lines
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

    /// The dry run over a real export: `LC_EMSPACE_EXPORT=<emspace export dir>`
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
        let mut args = vec!["import".to_owned(), "--from-emspace".to_owned(), export];
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
        assert_eq!(
            (&o.source, o.apply),
            (&Source::Json("x.json".into()), false)
        );
        assert_eq!(o.dirs["hive"], "services/hive");
        let plain = parse(&a(&["import", "x.json"])).unwrap();
        assert_eq!(plain.dirs["repo"], "");
        assert!(!plain.apply, "an emspace import is a dry run by default");
        let o = parse(&a(&[
            "import",
            "--from-emspace",
            "/tmp/oss",
            "--vault",
            "acme/oss",
            "--apply",
        ]))
        .unwrap();
        assert_eq!(o.source, Source::Dir("/tmp/oss".into()));
        assert_eq!((o.vault.as_deref(), o.apply), (Some("@acme/oss"), true));
        assert!(
            parse(&a(&["import", "--from-emspace", "d", "--apply"])).is_err(),
            "--apply names the vault it writes to"
        );
        assert!(!o.drop_descriptions, "descriptions are kept by default");
        let o = parse(&a(&[
            "import",
            "--from-emspace",
            "d",
            "--vault",
            "@a/b",
            "--apply",
            "--drop-descriptions",
        ]))
        .unwrap();
        assert!(o.apply && o.drop_descriptions);
        assert!(parse(&a(&["import"])).is_err() && parse(&a(&["export", "x"])).is_err());
    }

    #[test]
    fn the_vault_label_matches_case_aside() {
        assert!(same_vault("@LeanCTX/OSS", "@leanctx/oss"));
        assert!(!same_vault("@leanctx/oss", "@leanctx/hive"));
    }

    const DESCRIBED: &str = r##"{"page":{"id":"p1","slug":"a","type":"guide","status":"canonical","body":"# A\n","description":"What A is."},"sources":[]}"##;

    /// An export dir of space `space` whose `pages.jsonl` is `pages`.
    fn export_dir(space: &str, pages: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("space.json"),
            format!(r#"{{"space":{{"slug":"{space}"}}}}"#),
        )
        .unwrap();
        std::fs::write(dir.path().join("pages.jsonl"), pages).unwrap();
        dir
    }

    /// The arguments of an import of `export` into `@acme/oss`, plus `extra`.
    fn import_args(export: &tempfile::TempDir, extra: &[&str]) -> Vec<String> {
        [
            "import",
            "--from-emspace",
            export.path().to_str().unwrap(),
            "--vault",
            "@acme/oss",
        ]
        .iter()
        .chain(extra)
        .map(|s| (*s).to_owned())
        .collect()
    }

    /// What `run` refuses with (it must refuse, before any Hive write).
    fn refusal(args: &[String], root: &std::path::Path) -> String {
        match run(args, root.to_str().unwrap()) {
            Err(HiveError::Refused(why)) => why,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// Review 05.11. (third round): unknown keys fail closed. The dry run
    /// lists every unmapped key; a key outside the no-description list is
    /// refused, and a description nested in another field is flagged by path.
    #[test]
    fn apply_refuses_unknown_keys_and_flags_nested_descriptions() {
        let repo = checkout(&[("README.md", "# R\n")]);
        std::fs::write(repo.path().join(".emspace"), r#"{"space":"leanctx-oss"}"#).unwrap();

        let unknown = export_dir(
            "leanctx-oss",
            r##"{"page":{"id":"p1","slug":"a","type":"guide","status":"canonical","body":"# A\n","description":null,"title":"A","owner":"x"},"sources":[]}"##,
        );
        let dry = run(&import_args(&unknown, &[]), repo.path().to_str().unwrap()).unwrap();
        for want in [
            "unmapped page record keys: 2",
            "  owner: 1 page(s) (a); not on the no-description list",
            "  title: 1 page(s) (a)",
        ] {
            assert!(dry.iter().any(|l| l.starts_with(want)), "{want}: {dry:?}");
        }
        assert!(
            dry.iter().any(|l| l == "  title: 1 page(s) (a)"),
            "title is on the list: {dry:?}"
        );
        let why = refusal(&import_args(&unknown, &["--apply"]), repo.path());
        assert!(
            why.starts_with("1 page record key(s) are not on the list")
                && why.contains("(first: a: owner)"),
            "{why}"
        );

        let nested = export_dir(
            "leanctx-oss",
            r##"{"page":{"id":"p1","slug":"a","type":"guide","status":"canonical","body":"# A\n","description":null,"meta":{"description":"lost"}},"sources":[]}"##,
        );
        let dry = run(&import_args(&nested, &[]), repo.path().to_str().unwrap()).unwrap();
        assert!(
            dry.iter()
                .any(|l| l.starts_with("description-like keys not read: 1 (a: meta.description)")),
            "{dry:?}"
        );
        let why = refusal(&import_args(&nested, &["--apply"]), repo.path());
        assert!(why.contains("(first: a: meta.description)"), "{why}");
    }

    /// Review 05.11. (fourth round, F2c): `page.description: null` must not
    /// hide a record-level description. It counts, so `--apply` refuses;
    /// two different texts are an error before any plan.
    #[test]
    fn apply_refuses_a_record_description_beside_a_null_page_one() {
        let repo = checkout(&[("README.md", "# R\n")]);
        std::fs::write(repo.path().join(".emspace"), r#"{"space":"leanctx-oss"}"#).unwrap();

        let beside = export_dir(
            "leanctx-oss",
            r##"{"page":{"id":"p1","slug":"a","type":"guide","status":"canonical","body":"# A\n","description":null},"description":"What A is.","sources":[]}"##,
        );
        let dry = run(&import_args(&beside, &[]), repo.path().to_str().unwrap()).unwrap();
        assert!(
            dry.iter().any(|l| l.starts_with("page descriptions: 1 ")),
            "the record-level text is counted: {dry:?}"
        );
        let why = refusal(&import_args(&beside, &["--apply"]), repo.path());
        assert!(
            why.starts_with("1 page description(s) would be lost"),
            "{why}"
        );

        let differ = export_dir(
            "leanctx-oss",
            r##"{"page":{"id":"p1","slug":"a","type":"guide","status":"canonical","body":"# A\n","description":"One."},"description":"Other.","sources":[]}"##,
        );
        match run(
            &import_args(&differ, &["--apply", "--drop-descriptions"]),
            repo.path().to_str().unwrap(),
        ) {
            Err(HiveError::Config(e)) => {
                assert!(
                    e.starts_with("pages.jsonl:1: page a:") && e.contains("differ"),
                    "{e}"
                );
            }
            other => panic!("two different descriptions are an error, got {other:?}"),
        }
    }

    /// Review 05.11. (second round): the description check fails closed. A
    /// page record without the `description` key, or with the description
    /// under another name, is refused like a description that would be lost.
    #[test]
    fn apply_refuses_a_page_record_without_the_description_key() {
        let repo = checkout(&[("README.md", "# R\n")]);
        std::fs::write(repo.path().join(".emspace"), r#"{"space":"leanctx-oss"}"#).unwrap();

        let bare = export_dir(
            "leanctx-oss",
            r##"{"page":{"id":"p1","slug":"a","type":"guide","status":"canonical","body":"# A\n"},"sources":[]}"##,
        );
        let dry = run(&import_args(&bare, &[]), repo.path().to_str().unwrap()).unwrap();
        assert!(
            dry.iter()
                .any(|l| l.starts_with("pages without a `description` key: 1 (a)")),
            "the dry run flags it: {dry:?}"
        );
        let why = refusal(&import_args(&bare, &["--apply"]), repo.path());
        assert!(
            why.starts_with("1 page record(s) have no `description` key (first: a)"),
            "{why}"
        );

        let renamed = export_dir(
            "leanctx-oss",
            r##"{"page":{"id":"p1","slug":"a","type":"guide","status":"canonical","body":"# A\n","description":null,"summary":"A"},"sources":[]}"##,
        );
        let dry = run(&import_args(&renamed, &[]), repo.path().to_str().unwrap()).unwrap();
        assert!(
            dry.iter()
                .any(|l| l.starts_with("description-like keys not read: 1 (a: summary)")),
            "{dry:?}"
        );
        let why = refusal(&import_args(&renamed, &["--apply"]), repo.path());
        assert!(why.contains("(first: a: summary)"), "{why}");

        let wrong = export_dir(
            "leanctx-oss",
            r##"{"page":{"id":"p1","slug":"a","type":"guide","status":"canonical","body":"# A\n","description":7},"sources":[]}"##,
        );
        match run(&import_args(&wrong, &[]), repo.path().to_str().unwrap()) {
            Err(HiveError::Config(e)) => {
                assert!(e.starts_with("pages.jsonl:1: page a:"), "{e}");
            }
            other => panic!("a non-text description is an error, got {other:?}"),
        }
    }

    /// Review 05.11.: one early --apply lost every page description for good,
    /// and a private space could land in another checkout's vault. Both are
    /// refused before anything is written (no Hive store is opened here).
    #[test]
    fn apply_refuses_lost_descriptions_and_a_foreign_space() {
        let export = export_dir("leanctx-hive", DESCRIBED);
        let args = |extra: &[&str]| import_args(&export, extra);
        let refusal = |root: &std::path::Path, extra: &[&str]| refusal(&args(extra), root);

        let repo = checkout(&[("README.md", "# R\n")]);
        let dry = run(&args(&[]), repo.path().to_str().unwrap()).unwrap();
        assert!(
            dry.iter().any(|l| l.starts_with("page descriptions: 1 ")),
            "the dry run counts them: {dry:?}"
        );

        let why = refusal(repo.path(), &["--apply", "--drop-descriptions"]);
        assert!(why.contains("no .emspace link"), "{why}");

        std::fs::write(repo.path().join(".emspace"), r#"{"space":"leanctx-oss"}"#).unwrap();
        let why = refusal(repo.path(), &["--apply", "--drop-descriptions"]);
        assert!(
            why.contains("names space leanctx-oss, not leanctx-hive"),
            "{why}"
        );

        std::fs::write(repo.path().join(".emspace"), r#"{"space":"leanctx-hive"}"#).unwrap();
        let why = refusal(repo.path(), &["--apply"]);
        assert!(
            why.starts_with("1 page description(s) would be lost"),
            "{why}"
        );
        let described = lean_ctx_doc::emspace::Report {
            descriptions: 1,
            without_description_key: vec!["b".into()],
            ..Default::default()
        };
        assert_eq!(
            apply_refusal("leanctx-hive", &described, true, repo.path()),
            None,
            "--drop-descriptions is the explicit way past"
        );
        let clean = lean_ctx_doc::emspace::Report::default();
        assert_eq!(
            apply_refusal("leanctx-hive", &clean, false, repo.path()),
            None
        );
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
        let (specs, skipped, dangling) = events(&p, &BTreeMap::new(), &BTreeSet::new());
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
        let (_, skipped, _) = events(&p, &taken, &BTreeSet::new());
        assert_eq!(skipped, 1, "a second run adds only what is missing");

        // A page held back by screening is not created, and a link to it
        // stays as written instead of pointing at a node that never comes.
        let held = BTreeSet::from(["b".to_owned()]);
        let (specs, skipped, dangling) = events(&p, &BTreeMap::new(), &held);
        assert_eq!(skipped, 0);
        assert!(
            !specs
                .iter()
                .any(|s| s.body.get("slug") == Some(&serde_json::Value::from("b")))
        );
        assert_eq!(dangling.len(), 1, "{dangling:?}");
    }

    /// emspace tasks have no acceptance, priority or page fields in Hive:
    /// they go into the goal, the proposal passes the wire validation, and
    /// what Hive could not take is shortened and named.
    #[test]
    fn open_tasks_become_valid_proposals() {
        let task = |title: &str, body: String| TaskPlan {
            source_id: "01abc".into(),
            title: title.into(),
            body,
            status: "backlog".into(),
            priority: "high".into(),
            labels: vec!["board:cleanup".into()],
            pages: vec!["core/audit".into()],
        };
        let p = proposal(
            "leanctx-oss",
            &task("Hash  The\nTimestamp", "Why it matters.".into()),
        );
        assert_eq!(p.title, "Hash The Timestamp");
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        assert_eq!(
            p.goal,
            "Why it matters.\n\nFrom emspace leanctx-oss task 01abc (status backlog, priority high, board:cleanup, pages /core/audit)."
        );

        let long = proposal("s", &task(&"t".repeat(300), "x".repeat(5000)));
        assert_eq!(long.title.chars().count(), wire::MAX_TITLE_CHARS);
        assert_eq!(long.goal.chars().count(), wire::MAX_GOAL_CHARS);
        assert!(
            long.goal.ends_with(", pages /core/audit)."),
            "the origin survives the cut"
        );
        assert_eq!(long.problems.len(), 2, "{:?}", long.problems);
        assert!(sendable(&long));

        let short = proposal("s", &task("Fix", String::new()));
        assert!(!sendable(&short), "Hive refuses titles under 5 characters");
    }

    #[test]
    fn a_page_screening_refuses_is_held_back_whole() {
        let page = |body: String| lean_ctx_doc::Page {
            slug: "a".into(),
            title: "A".into(),
            kind: "guide".into(),
            status: lean_ctx_doc::PageStatus::Draft,
            sections: lean_ctx_doc::sections::split(&body).1,
            anchors: Vec::new(),
            notes: Vec::new(),
        };
        let token = format!("github_pat_{}", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4");
        assert!(screened_out(&page("# A\n\n## S\n\nplain\n".into()), &mut screen()).is_none());
        let why = screened_out(
            &page(format!("# A\n\n## S\n\nuse {token}\n")),
            &mut screen(),
        );
        assert!(why.is_some_and(|w| w.starts_with("a section")));
    }
}
