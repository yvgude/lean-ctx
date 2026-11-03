// SPDX-License-Identifier: Apache-2.0
//! `ctx_doc`: the project's documentation in its Hive vault (C0 2.2 docs).
//!
//! Reads come from the local replica, so they work offline and cost no
//! request. Writes are queued as signed events in the replica and sent in
//! the background ([`lean_ctx_hive::background`]); a write never waits
//! for Hive. Links are `/slug#heading` for agents and `hive:<id>` on the
//! wire ([`lean_ctx_doc::links`]).

use lean_ctx_doc::links;
use lean_ctx_doc::model::{Page, PageStatus};
use lean_ctx_doc::replica::{DocSpace, PageView};
use lean_ctx_doc::sections;
use lean_ctx_doc::state::DocState;
use lean_ctx_doc::write;

use lean_ctx_hive::HiveError;
use lean_ctx_hive::docs;
use lean_ctx_hive::knowledge::screening::Screen;

/// The arguments of one call.
#[derive(Debug, Default)]
pub struct Args<'a> {
    pub action: &'a str,
    pub slug: Option<&'a str>,
    pub section: Option<&'a str>,
    pub query: Option<&'a str>,
    pub content: Option<&'a str>,
    pub find: Option<&'a str>,
    pub replace: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub status: Option<&'a str>,
    pub outline: bool,
    /// write: the code the page derives from (`file:`, `dir:`, `glob:`, `shape:`).
    pub anchors: &'a [String],
}

const USAGE: &str = "ctx_doc actions: list [slug=prefix] | show slug [section=heading-slug] [outline=true] | \
search query | status | write slug content (whole page, `# Title` first) [type] [status=draft|canonical] | \
edit slug find replace | note slug content | confirm slug (re-pin after checking the page)";

/// Runs one call for the checkout at `root`.
pub fn handle(root: &str, args: &Args<'_>) -> String {
    match run(root, args) {
        Ok(out) => lean_ctx_hive::background::with_answers("ctx_doc", out),
        Err(e) => format!("ctx_doc: {e}"),
    }
}

fn run(root: &str, args: &Args<'_>) -> Result<String, HiveError> {
    match args.action {
        "list" | "show" | "search" | "status" => {
            docs::with_vault(root, |v| Ok(read(&docs::space(v.target.replica)?, args)))
        }
        "write" | "edit" | "note" | "confirm" => {
            docs::require_docs_protocol(&lean_ctx_hive::HiveStore::open()?)?;
            let queued = docs::with_vault(root, |v| {
                let space = docs::space(v.target.replica)?;
                let pinned = pins(root, &space, args)?;
                let specs = plan_write(&space, args, &pinned)?;
                let n = specs.len();
                if n == 0 {
                    return Ok(format!("{}: nothing changed", v.label));
                }
                let now = chrono::Utc::now().timestamp_millis().unsigned_abs();
                let mut screen = Screen::for_installation();
                docs::queue_specs(&v.target, specs, &mut screen, now)?;
                Ok(format!(
                    "{}: queued {n} event(s); they are sent in the background",
                    v.label
                ))
            })?;
            let root = root.to_owned();
            lean_ctx_hive::background::submit_once("ctx_doc", &format!("push:{root}"), move || {
                Some(match docs::push_queued(&root) {
                    Ok(line) => line,
                    Err(e) if e.is_transient() => format!(
                        "docs: not sent yet ({e}); the next `lean-ctx hive sync` sends them"
                    ),
                    Err(e) => format!("docs: push failed ({e})"),
                })
            });
            Ok(queued)
        }
        _ => Err(HiveError::Config(USAGE.into())),
    }
}

fn state_word(p: &PageView) -> &'static str {
    match p.state.state {
        DocState::Fresh => "fresh",
        DocState::Noted => "noted",
        DocState::Stale => "stale",
    }
}

fn slug_of(space: &DocSpace) -> impl Fn(&str) -> Option<String> + '_ {
    |id: &str| space.pages.get(id).map(|p| p.slug().to_owned())
}

/// The read actions, from the replica's view alone.
pub fn read(space: &DocSpace, args: &Args<'_>) -> String {
    match args.action {
        "list" => list(space, args.slug.unwrap_or("")),
        "status" => status(space),
        "search" => search(space, args.query.unwrap_or("")),
        _ => match args.slug.and_then(|s| space.page_by_slug(s)) {
            Some(p) => show(space, p, args.section, args.outline),
            None => format!(
                "no page {}; try action=list",
                args.slug.unwrap_or("(slug missing)")
            ),
        },
    }
}

fn list(space: &DocSpace, prefix: &str) -> String {
    let mut pages: Vec<&PageView> = space
        .pages
        .values()
        .filter(|p| p.slug().starts_with(prefix))
        .collect();
    pages.sort_by(|a, b| a.slug().cmp(b.slug()));
    if pages.is_empty() {
        return "no pages".into();
    }
    let mut out = format!("{} page(s)\n", pages.len());
    for p in pages {
        let notes = p.notes.iter().filter(|n| n.open).count();
        out.push_str(&format!("/{} — {} [{}", p.slug(), p.title(), state_word(p)));
        if notes > 0 {
            out.push_str(&format!(", {notes} note(s)"));
        }
        out.push_str("]\n");
    }
    out
}

fn status(space: &DocSpace) -> String {
    let mut behind: Vec<&PageView> = space
        .pages
        .values()
        .filter(|p| p.state.state != DocState::Fresh)
        .collect();
    behind.sort_by(|a, b| a.slug().cmp(b.slug()));
    if behind.is_empty() {
        return format!("{} page(s), all fresh", space.pages.len());
    }
    let mut out = format!(
        "{} of {} page(s) behind their code\n",
        behind.len(),
        space.pages.len()
    );
    for p in behind {
        out.push_str(&format!("/{} [{}]", p.slug(), state_word(p)));
        if !p.state.uncovered.is_empty() {
            out.push_str(&format!(" moved: {}", p.state.uncovered.join(", ")));
        }
        out.push('\n');
    }
    out
}

fn show(space: &DocSpace, p: &PageView, section: Option<&str>, outline: bool) -> String {
    let md = links::to_slugs(&p.markdown(), slug_of(space));
    let mut out = format!("/{} [{}]\n", p.slug(), state_word(p));
    if !p.state.uncovered.is_empty() {
        out.push_str(&format!(
            "stale: {} moved since the page was written\n",
            p.state.uncovered.join(", ")
        ));
    }
    for n in p.notes.iter().filter(|n| n.open) {
        out.push_str(&format!("note: {}\n", n.body));
    }
    if outline {
        for h in links::headings(&md) {
            out.push_str(&format!(
                "{} {}  (#{})\n",
                "#".repeat(h.level),
                h.text,
                h.slug
            ));
        }
        return out;
    }
    match section {
        Some(want) => match links::section_of(&md, want) {
            Some(text) => out.push_str(text),
            None => out.push_str(&format!("no section #{want}; try outline=true\n")),
        },
        None => out.push_str(&md),
    }
    out
}

fn search(space: &DocSpace, query: &str) -> String {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return "search needs a query".into();
    }
    let mut hits: Vec<(usize, &PageView)> = space
        .pages
        .values()
        .filter_map(|p| {
            let text = format!("{}\n{}", p.title(), p.markdown()).to_lowercase();
            let score: usize = words.iter().map(|w| text.matches(w.as_str()).count()).sum();
            (words.iter().all(|w| text.contains(w.as_str())) && score > 0).then_some((score, p))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.slug().cmp(b.1.slug())));
    if hits.is_empty() {
        return format!("no page mentions {query}");
    }
    let mut out = String::new();
    for (score, p) in hits.into_iter().take(20) {
        out.push_str(&format!("/{} — {} ({score})\n", p.slug(), p.title()));
    }
    out
}

/// C0 2.2 slug: `seg(/seg){0,7}`, seg `[a-z0-9]+(-[a-z0-9]+)*`, ≤ 128 bytes.
pub fn valid_slug(slug: &str) -> bool {
    let segs: Vec<&str> = slug.split('/').collect();
    slug.len() <= 128
        && segs.len() <= 8
        && segs.iter().all(|s| {
            !s.is_empty()
                && !s.starts_with('-')
                && !s.ends_with('-')
                && !s.contains("--")
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// The anchors a write pins: the `anchors` argument for `write`, the page's
/// own anchors re-pinned for `confirm`; none otherwise.
fn pins(
    root: &str,
    space: &DocSpace,
    args: &Args<'_>,
) -> Result<Vec<lean_ctx_doc::Anchor>, HiveError> {
    let wanted: Vec<lean_ctx_doc::Anchor> = match args.action {
        "write" if !args.anchors.is_empty() => Vec::new(),
        "confirm" => match args.slug.and_then(|s| space.page_by_slug(s)) {
            Some(p) => docs::page_anchors(p),
            None => return Ok(Vec::new()),
        },
        _ => return Ok(Vec::new()),
    };
    let files = docs::tracked_files(root)
        .ok_or_else(|| HiveError::Config("anchors need a git checkout".into()))?;
    if args.action == "write" {
        return args
            .anchors
            .iter()
            .map(|r| docs::pin(root, &files, r))
            .collect();
    }
    // confirm: the same refs and roles at today's digests; a gone target
    // cannot be confirmed (edit the page's anchors instead).
    wanted
        .into_iter()
        .map(|mut a| {
            a.digest = docs::digest_now(root, &files, &a).ok_or_else(|| {
                HiveError::Config(format!(
                    "{} is gone: write the page with its new anchors",
                    a.reference()
                ))
            })?;
            a.commit = docs::head_commit(root);
            Ok(a)
        })
        .collect()
}

/// The events of a write action against the current `space`; `pinned` are
/// the anchors `pins` resolved against the checkout.
pub fn plan_write(
    space: &DocSpace,
    args: &Args<'_>,
    pinned: &[lean_ctx_doc::Anchor],
) -> Result<Vec<write::EventSpec>, HiveError> {
    let refuse = |m: &str| HiveError::Config(m.to_owned());
    let slug = args.slug.ok_or_else(|| refuse("slug is required"))?;
    let id_of = |s: &str| space.by_slug.get(s).cloned();
    let mut new_id = docs::new_id;
    let verified_at = chrono::Utc::now().timestamp_millis().unsigned_abs();
    match args.action {
        "confirm" => {
            let page = space
                .page_by_slug(slug)
                .ok_or_else(|| refuse("no such page"))?;
            if pinned.is_empty() {
                return Err(refuse("the page has no anchors to confirm"));
            }
            Ok(write::confirm(page, pinned, verified_at))
        }
        "note" => {
            let page = space
                .page_by_slug(slug)
                .ok_or_else(|| refuse("no such page"))?;
            let body = args.content.ok_or_else(|| refuse("content is required"))?;
            Ok(write::add_note(&page.page.id, body, &[], &mut new_id))
        }
        "edit" => {
            let page = space
                .page_by_slug(slug)
                .ok_or_else(|| refuse("no such page"))?;
            let (find, replace) = match (args.find, args.replace) {
                (Some(f), Some(r)) if !f.is_empty() => (f, r),
                _ => return Err(refuse("edit needs find and replace")),
            };
            let current = links::to_slugs(&page.markdown(), slug_of(space));
            match current.matches(find).count() {
                1 => {}
                0 => {
                    return Err(refuse(
                        "find matches nothing: the page moved on, re-read it",
                    ));
                }
                n => {
                    return Err(refuse(&format!(
                        "find matches {n} times: extend it until it is unique"
                    )));
                }
            }
            revise(
                space,
                page,
                &current.replacen(find, replace, 1),
                &mut new_id,
            )
        }
        _ => {
            let content = args.content.ok_or_else(|| refuse("content is required"))?;
            if let Some(page) = space.page_by_slug(slug) {
                let mut specs = revise(space, page, content, &mut new_id)?;
                if !pinned.is_empty() {
                    specs.extend(write::confirm(page, pinned, verified_at));
                }
                return Ok(specs);
            }
            if !valid_slug(slug) {
                return Err(refuse(
                    "slug must be lowercase words joined by - and /, at most 8 segments",
                ));
            }
            let (stored, _dangling) = links::to_storage(content, id_of);
            let (title, sections) = sections::split(&stored);
            if title.is_empty() {
                return Err(refuse("a new page starts with its `# Title` line"));
            }
            let status = match args.status {
                Some("canonical") => PageStatus::Canonical,
                _ => PageStatus::Draft,
            };
            let page = Page {
                slug: slug.to_owned(),
                title,
                kind: args.kind.unwrap_or("reference").to_owned(),
                status,
                sections,
                anchors: pinned.to_vec(),
                notes: Vec::new(),
            };
            let (specs, _) = write::create_page(&page, &link_targets(&stored), &mut new_id);
            Ok(specs)
        }
    }
}

fn link_targets(stored: &str) -> Vec<String> {
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

fn revise(
    space: &DocSpace,
    page: &PageView,
    markdown: &str,
    new_id: write::NewId<'_>,
) -> Result<Vec<write::EventSpec>, HiveError> {
    let (stored, _) = links::to_storage(markdown, |s| space.by_slug.get(s).cloned());
    let now = space.links.get(&page.page.id).cloned().unwrap_or_default();
    Ok(write::revise_page(
        page,
        &stored,
        &now,
        &link_targets(&stored),
        new_id,
    ))
}

#[cfg(test)]
#[path = "ctx_doc_tests.rs"]
mod tests;
