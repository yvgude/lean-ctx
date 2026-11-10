// SPDX-License-Identifier: Apache-2.0

//! Reading an emspace export into pages, for the move of a space to Hive.
//!
//! The export is the JSON an emspace program writes (`Export`): pages with
//! their sources and open notes, tasks and boards. [`plan`] turns it into
//! pages of this model and a report that says what could not be carried over
//! as it was, so the migration is checked before anything is written.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::anchor::{self, Anchor, AnchorKind, AnchorMode, AnchorRole};
use crate::links::{self, Target};
use crate::model::{Note, Page, PageStatus};
use crate::sections;

/// One emspace space, as an export program writes it.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Export {
    pub space: String,
    pub pages: Vec<ExportPage>,
    #[serde(default)]
    pub tasks: Vec<ExportTask>,
    #[serde(default)]
    pub boards: Vec<ExportBoard>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPage {
    pub slug: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub status: String,
    pub body: String,
    /// The one-line page description ([`Description`]); a value that is
    /// neither text nor `null` is an error. [`Page`] has no field for it
    /// yet, so the plan counts it ([`Report::descriptions`]) and flags a page
    /// without the key ([`Report::without_description_key`]): the import
    /// fails closed rather than lose descriptions it did not see.
    #[serde(default, deserialize_with = "present")]
    pub description: Description,
    #[serde(default)]
    pub retired: bool,
    #[serde(default)]
    pub sources: Vec<ExportSource>,
    #[serde(default)]
    pub notes: Vec<ExportNote>,
    /// Keys of the page record this reader does not map. The plan flags
    /// the description-like ones ([`Report::description_like_keys`]).
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// How a page record carries its `description` key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub enum Description {
    /// The record has no `description` key.
    #[default]
    Missing,
    /// `"description": null`.
    Null,
    Text(String),
}

/// A present key: only text or `null` is accepted.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Description, D::Error> {
    Option::<String>::deserialize(d).map(|o| o.map_or(Description::Null, Description::Text))
}

/// A key that may carry a page description under another name.
#[must_use]
pub fn description_like(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    [
        "desc", "summary", "abstract", "blurb", "tagline", "subtitle", "excerpt", "synopsis",
        "teaser", "intro", "lede", "overview",
    ]
    .iter()
    .any(|w| k.contains(w))
}

/// Unmapped page record keys known to hold no description: any other
/// unmapped key makes an `--apply` refuse (it could hold one). Kept in step
/// with the list in the internal emspace-to-hive migration notes; check it
/// against the first real export. A key beside `page` is reported as
/// `record.<key>` and checked by its bare name ([`no_description_key`]).
///
/// `title`: the importer takes a page's title from its body's `# heading`
/// (byte-lossless split + join), so a `title` field adds nothing to read.
pub const NO_DESCRIPTION_KEYS: [&str; 12] = [
    "title",
    "spaceId",
    "createdAt",
    "updatedAt",
    "createdBy",
    "updatedBy",
    "revision",
    "revisedAt",
    "lsn",
    "bodyHash",
    "edges",
    "links",
];

/// The prefix of an unmapped key that sits beside `page` in a
/// `pages.jsonl` record, so it never shadows a `page` key of the same name.
pub const RECORD_KEY_PREFIX: &str = "record.";

/// Whether unmapped `key` (bare, or with [`RECORD_KEY_PREFIX`]) is on
/// [`NO_DESCRIPTION_KEYS`].
#[must_use]
pub fn no_description_key(key: &str) -> bool {
    let bare = key.strip_prefix(RECORD_KEY_PREFIX).unwrap_or(key);
    NO_DESCRIPTION_KEYS.contains(&bare)
}

/// Every description-like key ([`description_like`]) below `value`, as a
/// path from `prefix` (`meta.description`, `blocks[0].summary`).
fn nested_description_keys(prefix: &str, value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let path = format!("{prefix}.{k}");
                if description_like(k) {
                    out.push(path.clone());
                }
                nested_description_keys(&path, v, out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                nested_description_keys(&format!("{prefix}[{i}]"), v, out);
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSource {
    pub locator: String,
    /// `derived` or `cites`.
    pub role: String,
    /// `content` (default) or `shape`.
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub pinned_hash: String,
    #[serde(default)]
    pub pinned_ref: Option<String>,
    /// The target changed since it was pinned.
    #[serde(default)]
    pub moved: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ExportNote {
    pub body: String,
    /// Locators of the sources the note covers.
    #[serde(default)]
    pub covers: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ExportTask {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub status: String,
    #[serde(default)]
    pub priority: Option<String>,
    /// Slugs of the boards the task is filed on.
    #[serde(default)]
    pub boards: Vec<String>,
    /// The page the task is about.
    #[serde(default)]
    pub page: Option<String>,
    /// Further pages the task is about (`emspace export` names all of them).
    #[serde(default)]
    pub pages: Vec<String>,
}

/// The task statuses emspace counts as open work (`backlog → todo → doing`);
/// `done` and `dropped` are closed and stay behind.
pub const OPEN_TASK_STATUSES: [&str; 3] = ["backlog", "todo", "doing"];

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ExportBoard {
    pub slug: String,
    pub title: String,
}

/// A page of the plan: the page and how each anchor stood in emspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PagePlan {
    pub page: Page,
    /// Parallel to `page.anchors`: the source had moved in emspace.
    pub moved: Vec<bool>,
}

/// A task of the plan. Boards become labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskPlan {
    pub source_id: String,
    pub title: String,
    pub body: String,
    pub status: String,
    pub priority: String,
    pub labels: Vec<String>,
    /// The migrated pages the task is about (a retired page drops out).
    pub pages: Vec<String>,
}

/// What the plan could not carry over unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    pub pages: usize,
    pub sections: usize,
    pub anchors: usize,
    pub notes: usize,
    pub tasks: usize,
    pub retired_skipped: usize,
    /// Migrated pages that carry a description: the page model has no
    /// field for it, so an import would lose these descriptions.
    pub descriptions: usize,
    /// Migrated pages whose record has no `description` key: the export may
    /// carry their descriptions where this reader does not look.
    pub without_description_key: Vec<String>,
    /// `(page, key path)` for keys of a migrated page record that look like
    /// a description under another name ([`description_like`]), nested
    /// ones included (`meta.description`).
    pub description_like_keys: Vec<(String, String)>,
    /// `(page, key)` for every unmapped key of a migrated page record.
    pub unmapped_keys: Vec<(String, String)>,
    /// `(page, key)` for unmapped keys outside [`NO_DESCRIPTION_KEYS`]:
    /// they could hold a description, so an import fails closed on them.
    pub unknown_keys: Vec<(String, String)>,
    /// Done or dropped tasks, left behind (only open work moves).
    pub closed_tasks_skipped: usize,
    /// Slugs that are not C0 2.2 slugs ([`crate::model::valid_slug`]):
    /// Hive refuses these pages, so they need a new slug first.
    pub invalid_slugs: Vec<String>,
    /// `(page, target)` for links to a slug no migrated page has.
    pub dangling: Vec<(String, String)>,
    /// `(page, locator)` for sources outside the mapped directories.
    pub unmapped_sources: Vec<(String, String)>,
    /// Pages whose body does not survive split + join byte for byte.
    pub not_lossless: Vec<String>,
    /// `(page, status)` for statuses this model does not know.
    pub unknown_status: Vec<(String, String)>,
}

/// The pages, tasks and report of a migration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub pages: Vec<PagePlan>,
    pub tasks: Vec<TaskPlan>,
    pub report: Report,
}

/// Records in `report` how page `p` carries its description and every key
/// the reader did not map (fail closed: an import must not lose a
/// description it did not see).
fn check_description_keys(p: &ExportPage, report: &mut Report) {
    match &p.description {
        Description::Missing => report.without_description_key.push(p.slug.clone()),
        Description::Text(d) if !d.trim().is_empty() => report.descriptions += 1,
        Description::Null | Description::Text(_) => {}
    }
    for (k, v) in &p.extra {
        report.unmapped_keys.push((p.slug.clone(), k.clone()));
        if !no_description_key(k) {
            report.unknown_keys.push((p.slug.clone(), k.clone()));
        }
        let mut paths = Vec::new();
        if description_like(k) {
            paths.push(k.clone());
        }
        nested_description_keys(k, v, &mut paths);
        // Deterministic whatever order serde_json keeps object keys in.
        paths.sort();
        report
            .description_like_keys
            .extend(paths.into_iter().map(|path| (p.slug.clone(), path)));
    }
}

/// Plans the move of `export`. `dirs` maps an emspace directory name (the
/// `repo` of `dir:repo/rust/src`) to its path prefix in the target
/// repository (`""` for the root); `is_dir` says whether a mapped path is a
/// directory of the target checkout. Retired pages are left in the archive.
pub fn plan(
    export: &Export,
    dirs: &BTreeMap<String, String>,
    is_dir: &dyn Fn(&str) -> bool,
) -> Plan {
    let mut report = Report::default();
    let live: Vec<&ExportPage> = export
        .pages
        .iter()
        .filter(|p| !p.retired && p.status != "retired")
        .collect();
    report.retired_skipped = export.pages.len() - live.len();
    let slugs: BTreeSet<&str> = live.iter().map(|p| p.slug.as_str()).collect();

    let mut pages = Vec::with_capacity(live.len());
    for p in live {
        check_description_keys(p, &mut report);
        if !crate::model::valid_slug(&p.slug) {
            report.invalid_slugs.push(p.slug.clone());
        }
        let (title, secs) = sections::split(&p.body);
        if sections::join(&title, &secs) != p.body {
            report.not_lossless.push(p.slug.clone());
        }
        let status = PageStatus::parse(&p.status).unwrap_or_else(|| {
            report
                .unknown_status
                .push((p.slug.clone(), p.status.clone()));
            PageStatus::Draft
        });
        for t in links::targets(&p.body) {
            if let Target::Page { slug, .. } = links::classify(t) {
                if !slugs.contains(slug) {
                    report.dangling.push((p.slug.clone(), t.to_owned()));
                }
            }
        }
        let mut anchors = Vec::new();
        let mut moved = Vec::new();
        let mut by_locator = BTreeMap::new();
        for s in &p.sources {
            match to_anchor(s, dirs, is_dir) {
                Some(a) => {
                    by_locator.insert(s.locator.as_str(), a.path.clone());
                    anchors.push(a);
                    moved.push(s.moved);
                }
                None => report
                    .unmapped_sources
                    .push((p.slug.clone(), s.locator.clone())),
            }
        }
        let notes: Vec<Note> = p
            .notes
            .iter()
            .map(|n| Note {
                body: n.body.clone(),
                covers: n
                    .covers
                    .iter()
                    .filter_map(|l| by_locator.get(l.as_str()).cloned())
                    .collect(),
            })
            .collect();
        report.sections += secs.len();
        report.anchors += anchors.len();
        report.notes += notes.len();
        pages.push(PagePlan {
            page: Page {
                slug: p.slug.clone(),
                title,
                kind: p.kind.clone(),
                status,
                sections: secs,
                anchors,
                notes,
            },
            moved,
        });
    }
    report.pages = pages.len();

    let tasks = plan_tasks(&export.tasks, &slugs);
    report.tasks = tasks.len();
    report.closed_tasks_skipped = export.tasks.len() - tasks.len();
    Plan {
        pages,
        tasks,
        report,
    }
}

/// The open tasks ([`OPEN_TASK_STATUSES`]) as task plans; a page link
/// survives only to a page in `slugs` (retired pages drop out).
fn plan_tasks(tasks: &[ExportTask], slugs: &BTreeSet<&str>) -> Vec<TaskPlan> {
    tasks
        .iter()
        .filter(|t| OPEN_TASK_STATUSES.contains(&t.status.as_str()))
        .map(|t| {
            let mut about: Vec<String> = Vec::new();
            for s in t.page.iter().chain(&t.pages) {
                if slugs.contains(s.as_str()) && !about.contains(s) {
                    about.push(s.clone());
                }
            }
            TaskPlan {
                source_id: t.id.clone(),
                title: t.title.clone(),
                body: t.body.clone(),
                status: t.status.clone(),
                priority: t.priority.clone().unwrap_or_else(|| "normal".to_owned()),
                labels: t.boards.iter().map(|b| format!("board:{b}")).collect(),
                pages: about,
            }
        })
        .collect()
}

/// An emspace `dir:<name>/<path>` source as an anchor, if `<name>` is mapped.
fn to_anchor(
    s: &ExportSource,
    dirs: &BTreeMap<String, String>,
    is_dir: &dyn Fn(&str) -> bool,
) -> Option<Anchor> {
    let rest = s.locator.strip_prefix("dir:")?;
    let (name, path) = rest.split_once('/').unwrap_or((rest, ""));
    let prefix = dirs.get(name)?;
    let path = match (prefix.trim_end_matches('/'), path) {
        ("", p) => p.to_owned(),
        (pre, "") => pre.to_owned(),
        (pre, p) => format!("{pre}/{p}"),
    };
    let kind = if anchor::is_glob(&path) {
        AnchorKind::Glob
    } else if is_dir(&path) {
        AnchorKind::Dir
    } else {
        AnchorKind::File
    };
    let mode = if s.mode.as_deref() == Some("shape") {
        AnchorMode::Shape
    } else {
        AnchorMode::Content
    };
    let role = if s.role == "cites" {
        AnchorRole::Cites
    } else {
        AnchorRole::Derived
    };
    // emspace hashes with its own algorithm: the migrator re-pins the digest
    // at `commit` (emspace's pinnedRef) before anything is written.
    Some(Anchor {
        kind,
        mode,
        role,
        path,
        digest: String::new(),
        commit: s.pinned_ref.clone(),
    })
}

/// Reads what `emspace export <dir>` writes: the text of its `pages.jsonl`,
/// `tasks.jsonl` and `journal.jsonl`. The export carries no notes of its
/// own, so the open notes come from the journal (`jsonl_notes`).
///
/// # Errors
///
/// A line that is not JSON (the error names its file and line), or a
/// `pages.jsonl` record without `page`.
pub fn from_jsonl(space: &str, pages: &str, tasks: &str, journal: &str) -> Result<Export, String> {
    let mut notes_of = jsonl_notes(journal)?;
    let mut export = Export {
        space: space.to_owned(),
        ..Export::default()
    };
    for r in records("pages.jsonl", pages) {
        let (line, r) = r?;
        let p = r
            .get("page")
            .ok_or_else(|| format!("pages.jsonl:{line}: a record without `page`"))?;
        // In `page`; tolerated beside it as well. Both are read: text in
        // either one counts, two different texts are refused.
        let description = record_description(p, &r)
            .map_err(|why| format!("pages.jsonl:{line}: page {}: {why}", text(p, "slug")))?;
        let mut extra = BTreeMap::new();
        for (obj, known) in [
            (
                p,
                &[
                    "id",
                    "slug",
                    "type",
                    "status",
                    "body",
                    "description",
                    "retiredAt",
                ][..],
            ),
            (&r, &["page", "sources", "description"][..]),
        ] {
            // A record key never replaces a `page` key of the same name.
            let prefix = if std::ptr::eq(obj, p) {
                ""
            } else {
                RECORD_KEY_PREFIX
            };
            if let Some(map) = obj.as_object() {
                for (k, v) in map.iter().filter(|(k, _)| !known.contains(&k.as_str())) {
                    extra.insert(format!("{prefix}{k}"), v.clone());
                }
            }
        }
        let sources = r
            .get("sources")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|s| ExportSource {
                        locator: text(s, "locator"),
                        role: text(s, "role"),
                        mode: opt(s, "mode"),
                        pinned_hash: text(s, "pinnedHash"),
                        pinned_ref: opt(s, "pinnedRef"),
                        moved: s.get("moved").and_then(Value::as_bool).unwrap_or(false),
                    })
                    .collect()
            })
            .unwrap_or_default();
        export.pages.push(ExportPage {
            slug: text(p, "slug"),
            kind: text(p, "type"),
            status: text(p, "status"),
            body: text(p, "body"),
            description,
            retired: p.get("retiredAt").is_some_and(|v| !v.is_null()),
            sources,
            notes: notes_of.remove(&text(p, "id")).unwrap_or_default(),
            extra,
        });
    }
    for t in records("tasks.jsonl", tasks) {
        let (_, t) = t?;
        export.tasks.push(ExportTask {
            id: text(&t, "id"),
            title: text(&t, "title"),
            body: text(&t, "body"),
            status: text(&t, "status"),
            priority: opt(&t, "priority"),
            boards: opt(&t, "board").into_iter().collect(),
            page: None,
            pages: strs(&t, "pages"),
        });
    }
    Ok(export)
}

/// The description of a `pages.jsonl` record, read from `page` and from
/// beside it. Text in either one wins over `null` or a missing key (blank
/// text only over those); two different non-blank texts are an error, as
/// is a value that is neither text nor `null`.
fn record_description(page: &Value, record: &Value) -> Result<Description, String> {
    fn read(v: Option<&Value>, at: &str) -> Result<Description, String> {
        match v {
            None => Ok(Description::Missing),
            Some(Value::Null) => Ok(Description::Null),
            Some(Value::String(s)) => Ok(Description::Text(s.clone())),
            Some(_) => Err(format!("`{at}` is not text")),
        }
    }
    fn rank(d: &Description) -> u8 {
        match d {
            Description::Missing => 0,
            Description::Null => 1,
            Description::Text(t) if t.trim().is_empty() => 2,
            Description::Text(_) => 3,
        }
    }
    let inner = read(page.get("description"), "description")?;
    let outer = read(record.get("description"), "record.description")?;
    if let (Description::Text(a), Description::Text(b)) = (&inner, &outer) {
        if rank(&inner) == 3 && rank(&outer) == 3 && a != b {
            return Err(
                "`description` in `page` and `record.description` differ: refused rather than lose one"
                    .to_owned(),
            );
        }
    }
    Ok(if rank(&outer) > rank(&inner) {
        outer
    } else {
        inner
    })
}

/// The open notes of a journal, by page id: `note.add` and `note.update`
/// set a note's body, `note.resolve` closes it for good (a later update
/// does not reopen it). Notes keep note-id order (emspace ids sort by time).
fn jsonl_notes(journal: &str) -> Result<BTreeMap<String, Vec<ExportNote>>, String> {
    let mut open: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut resolved: BTreeSet<String> = BTreeSet::new();
    for e in records("journal.jsonl", journal) {
        let (_, e) = e?;
        let id = text(&e, "objectId");
        let payload = e.get("payload").unwrap_or(&Value::Null);
        match e.get("op").and_then(Value::as_str) {
            Some("note.add" | "note.update") if !resolved.contains(&id) => {
                open.insert(id, (text(payload, "pageId"), text(payload, "body")));
            }
            Some("note.resolve") => {
                open.remove(&id);
                resolved.insert(id);
            }
            _ => {}
        }
    }
    let mut notes_of: BTreeMap<String, Vec<ExportNote>> = BTreeMap::new();
    for (page, body) in open.into_values() {
        notes_of.entry(page).or_default().push(ExportNote {
            body,
            covers: Vec::new(),
        });
    }
    Ok(notes_of)
}

/// The JSON records of one JSONL file with their (1-based) line; an error
/// names `name` and the line.
fn records<'a>(
    name: &'a str,
    text: &'a str,
) -> impl Iterator<Item = Result<(usize, Value), String>> + 'a {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(move |(i, l)| {
            serde_json::from_str(l)
                .map(|v| (i + 1, v))
                .map_err(|e| format!("{name}:{}: {e}", i + 1))
        })
}

fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn opt(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn strs(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(locator: &str, role: &str, moved: bool) -> ExportSource {
        ExportSource {
            locator: locator.into(),
            role: role.into(),
            moved,
            pinned_hash: "h".into(),
            ..Default::default()
        }
    }

    fn export() -> Export {
        Export {
            space: "s".into(),
            pages: vec![
                ExportPage {
                    slug: "a".into(),
                    kind: "reference".into(),
                    status: "canonical".into(),
                    body: "# A\n## One\nsee [B](/b) and [X](/gone)\n".into(),
                    sources: vec![
                        source("dir:repo/rust/src/core", "derived", true),
                        source("dir:repo/rust/src/**/*.rs", "cites", false),
                        source("url:https://x", "cites", false),
                    ],
                    notes: vec![ExportNote {
                        body: "moved".into(),
                        covers: vec!["dir:repo/rust/src/core".into()],
                    }],
                    ..Default::default()
                },
                ExportPage {
                    slug: "b".into(),
                    kind: "design".into(),
                    status: "draft".into(),
                    body: "# B\n".into(),
                    description: Description::Text("The B page.".into()),
                    extra: BTreeMap::from([("summary".to_owned(), Value::from("B in short"))]),
                    ..Default::default()
                },
                ExportPage {
                    slug: "old".into(),
                    kind: "x".into(),
                    status: "canonical".into(),
                    retired: true,
                    body: "# O\n".into(),
                    description: Description::Text("Retired, stays behind.".into()),
                    ..Default::default()
                },
            ],
            tasks: vec![
                ExportTask {
                    id: "t1".into(),
                    title: "Do It".into(),
                    status: "todo".into(),
                    boards: vec!["w".into()],
                    page: Some("old".into()),
                    pages: vec!["b".into(), "old".into()],
                    ..Default::default()
                },
                ExportTask {
                    id: "t2".into(),
                    title: "Done Already".into(),
                    status: "done".into(),
                    ..Default::default()
                },
            ],
            boards: vec![ExportBoard {
                slug: "w".into(),
                title: "Work".into(),
            }],
        }
    }

    #[test]
    fn plans_pages_anchors_notes_and_tasks() {
        let dirs = BTreeMap::from([("repo".to_owned(), String::new())]);
        let plan = plan(&export(), &dirs, &|p| p == "rust/src/core");
        let r = &plan.report;
        assert_eq!((r.pages, r.retired_skipped, r.tasks, r.notes), (2, 1, 1, 1));
        assert_eq!(
            r.descriptions, 1,
            "a retired page's description stays behind"
        );
        assert_eq!(
            r.without_description_key,
            ["a"],
            "a page record without the key fails closed"
        );
        assert_eq!(
            r.description_like_keys,
            [("b".to_owned(), "summary".to_owned())]
        );
        assert_eq!(r.dangling, [("a".to_owned(), "/gone".to_owned())]);
        assert_eq!(
            r.unmapped_sources,
            [("a".to_owned(), "url:https://x".to_owned())]
        );
        assert!(r.not_lossless.is_empty());

        let a = &plan.pages[0];
        assert_eq!(a.page.title, "A");
        assert_eq!(a.page.status, PageStatus::Canonical);
        assert_eq!(a.page.anchors[0].kind, AnchorKind::Dir);
        assert_eq!(a.page.anchors[0].path, "rust/src/core");
        assert_eq!(a.page.anchors[1].kind, AnchorKind::Glob);
        assert_eq!(a.page.anchors[1].role, AnchorRole::Cites);
        assert_eq!(a.moved, [true, false]);
        assert_eq!(a.page.notes[0].covers, ["rust/src/core"]);
        assert_eq!(a.page.body(), export().pages[0].body);

        assert_eq!(plan.tasks.len(), 1, "only open work moves");
        assert_eq!(r.closed_tasks_skipped, 1);
        let t = &plan.tasks[0];
        assert_eq!(t.priority, "normal");
        assert_eq!(t.labels, ["board:w"]);
        assert_eq!(
            t.pages,
            ["b"],
            "a task about a retired page loses that link"
        );
    }

    /// The JSONL of `emspace export <dir>`: retired pages, sources, open
    /// notes from the journal (a resolved note stays behind, even when an
    /// update follows its resolve) and tasks with their board.
    #[test]
    fn reads_the_jsonl_of_an_export_dir() {
        let pages = concat!(
            r##"{"page":{"id":"p1","slug":"a","type":"guide","status":"canonical","body":"# A\n","description":"What A is."},"sources":[{"locator":"dir:repo/x.rs","role":"derived","pinnedHash":"h","pinnedRef":"c1","moved":true}]}"##,
            "\n",
            r##"{"page":{"id":"p2","slug":"gone","type":"x","status":"draft","body":"# G\n","retiredAt":"2026-10-30T00:00:00Z"},"sources":[]}"##,
            "\n"
        );
        let journal = concat!(
            r#"{"op":"note.add","objectId":"n1","payload":{"pageId":"p1","body":"first"}}"#,
            "\n",
            r#"{"op":"note.update","objectId":"n1","payload":{"pageId":"p1","body":"open"}}"#,
            "\n",
            r#"{"op":"note.add","objectId":"n2","payload":{"pageId":"p1","body":"closed"}}"#,
            "\n",
            r#"{"op":"note.resolve","objectId":"n2","payload":{"pageId":"p1"}}"#,
            "\n",
            r#"{"op":"note.update","objectId":"n2","payload":{"pageId":"p1","body":"late"}}"#,
            "\n",
            r#"{"op":"page.revise","objectId":"p1","payload":{}}"#,
            "\n"
        );
        let tasks = r#"{"id":"t1","title":"Fix It Now","body":"why","status":"backlog","priority":"high","board":"w","pages":["a"]}"#;
        let e = from_jsonl("s", pages, tasks, journal).expect("a valid export");
        assert_eq!(e.pages.len(), 2);
        let a = &e.pages[0];
        assert_eq!((a.kind.as_str(), a.retired), ("guide", false));
        assert!(e.pages[1].retired);
        assert_eq!(a.description, Description::Text("What A is.".to_owned()));
        assert_eq!(
            e.pages[1].description,
            Description::Missing,
            "no key, not an empty text"
        );
        assert!(a.extra.is_empty(), "{:?}", a.extra);
        assert_eq!(a.sources[0].pinned_ref.as_deref(), Some("c1"));
        assert!(a.sources[0].moved);
        let notes: Vec<&str> = a.notes.iter().map(|n| n.body.as_str()).collect();
        assert_eq!(notes, ["open"]);
        let t = &e.tasks[0];
        assert_eq!(
            (t.priority.as_deref(), t.boards.as_slice()),
            (Some("high"), &["w".to_owned()][..])
        );
        assert_eq!(t.pages, ["a"]);
        let err = from_jsonl("s", "{not json", "", "").expect_err("not JSON");
        assert!(err.starts_with("pages.jsonl:1:"), "{err}");
    }

    /// Review 05.11.: the description check must fail closed. `null` is a
    /// present key, a description under another name is kept as an extra
    /// key (and flagged by the plan), a non-text one is an error naming its
    /// line, in both the JSONL and the JSON form.
    #[test]
    fn descriptions_are_read_strictly() {
        let pages = concat!(
            r##"{"page":{"id":"p1","slug":"a","type":"g","status":"draft","body":"# A\n","description":null}}"##,
            "\n",
            r##"{"page":{"id":"p2","slug":"b","type":"g","status":"draft","body":"# B\n","pageDescription":"B"}}"##,
            "\n",
        );
        let e = from_jsonl("s", pages, "", "").expect("a valid export");
        assert_eq!(e.pages[0].description, Description::Null);
        assert_eq!(e.pages[1].description, Description::Missing);
        let dirs = BTreeMap::new();
        let r = plan(&e, &dirs, &|_| false).report;
        assert_eq!(
            (r.descriptions, r.without_description_key.as_slice()),
            (0, &["b".to_owned()][..])
        );
        assert_eq!(
            r.description_like_keys,
            [("b".to_owned(), "pageDescription".to_owned())]
        );

        let bad = concat!(
            r##"{"page":{"id":"p1","slug":"a","type":"g","status":"draft","body":"# A\n","description":"ok"}}"##,
            "\n",
            r##"{"page":{"id":"p2","slug":"b","type":"g","status":"draft","body":"# B\n","description":{"text":"B"}}}"##,
        );
        let err = from_jsonl("s", bad, "", "").expect_err("not text");
        assert_eq!(err, "pages.jsonl:2: page b: `description` is not text");

        let json = |d: &str| {
            serde_json::from_str::<ExportPage>(&format!(
                r##"{{"slug":"a","type":"g","status":"draft","body":"# A\n"{d}}}"##
            ))
        };
        assert_eq!(json("").expect("no key").description, Description::Missing);
        assert_eq!(
            json(r#","description":null"#).expect("null").description,
            Description::Null
        );
        assert_eq!(
            json(r#","summary":"S""#).expect("extra").extra["summary"],
            "S"
        );
        assert!(json(r#","description":3"#).is_err(), "non-text is refused");
    }

    /// Review 05.11. (third round): unknown keys fail closed. Every unmapped
    /// key is reported; one outside [`NO_DESCRIPTION_KEYS`] is unknown, and
    /// a description-like key nested in another field is flagged by path.
    #[test]
    fn unmapped_keys_are_reported_and_unknown_ones_flagged() {
        let pages = concat!(
            r##"{"page":{"id":"p1","slug":"a","type":"g","status":"draft","body":"# A\n","description":null,"title":"A","createdAt":"2026-11-01"}}"##,
            "\n",
            r##"{"page":{"id":"p2","slug":"b","type":"g","status":"draft","body":"# B\n","description":null,"meta":{"tags":[{"lede":"x"}],"description":"lost"}}}"##,
            "\n",
            r##"{"page":{"id":"p3","slug":"c","type":"g","status":"draft","body":"# C\n","description":null},"intro":"C"}"##,
            "\n",
        );
        let e = from_jsonl("s", pages, "", "").expect("a valid export");
        let r = plan(&e, &BTreeMap::new(), &|_| false).report;
        let pairs = |v: &[(&str, &str)]| -> Vec<(String, String)> {
            v.iter()
                .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
                .collect()
        };
        assert_eq!(
            r.unmapped_keys,
            pairs(&[
                ("a", "createdAt"),
                ("a", "title"),
                ("b", "meta"),
                ("c", "record.intro")
            ])
        );
        assert_eq!(
            r.unknown_keys,
            pairs(&[("b", "meta"), ("c", "record.intro")]),
            "title and createdAt are on the no-description list"
        );
        assert_eq!(
            r.description_like_keys,
            pairs(&[
                ("b", "meta.description"),
                ("b", "meta.tags[0].lede"),
                ("c", "record.intro")
            ])
        );
        assert!(r.without_description_key.is_empty() && r.descriptions == 0);
    }

    /// Review 05.11. (fourth round, F2c): both `description` fields are
    /// read. Text in either one counts, two different texts are refused,
    /// and a non-text record-level value is an error too.
    #[test]
    fn both_description_fields_are_read() {
        let line = |page: &str, record: &str| {
            format!(
                r##"{{"page":{{"id":"p1","slug":"a","type":"g","status":"draft","body":"# A\n"{page}}}{record}}}"##
            )
        };
        let read = |page: &str, record: &str| {
            from_jsonl("s", &line(page, record), "", "").map(|e| e.pages[0].description.clone())
        };
        let text = |s: &str| Ok(Description::Text(s.to_owned()));
        assert_eq!(
            read(r#","description":null"#, r#","description":"R""#),
            text("R")
        );
        assert_eq!(read("", r#","description":"R""#), text("R"));
        assert_eq!(
            read(r#","description":"P""#, r#","description":null"#),
            text("P")
        );
        assert_eq!(
            read(r#","description":"P""#, r#","description":"P""#),
            text("P")
        );
        assert_eq!(
            read(r#","description":"  ""#, r#","description":"R""#),
            text("R")
        );
        assert_eq!(read("", r#","description":null"#), Ok(Description::Null));
        assert_eq!(
            read(r#","description":"P""#, r#","description":"R""#),
            Err("pages.jsonl:1: page a: `description` in `page` and `record.description` differ: refused rather than lose one".to_owned())
        );
        assert_eq!(
            read(r#","description":null"#, r#","description":5"#),
            Err("pages.jsonl:1: page a: `record.description` is not text".to_owned())
        );

        let e = from_jsonl(
            "s",
            &line(r#","description":null"#, r#","description":"R""#),
            "",
            "",
        )
        .expect("a valid export");
        let r = plan(&e, &BTreeMap::new(), &|_| false).report;
        assert_eq!(r.descriptions, 1, "the record-level text is counted");
    }

    /// Review 05.11. (fourth round, F2d): a record key never replaces the
    /// `page` key of the same name, so a nested description in either is
    /// flagged.
    #[test]
    fn record_keys_do_not_shadow_page_keys() {
        let pages = r##"{"page":{"id":"p1","slug":"a","type":"g","status":"draft","body":"# A\n","description":null,"links":[{"summary":"x"}]},"links":[],"title":"A"}"##;
        let e = from_jsonl("s", pages, "", "").expect("a valid export");
        let r = plan(&e, &BTreeMap::new(), &|_| false).report;
        let pairs = |v: &[&str]| -> Vec<(String, String)> {
            v.iter()
                .map(|k| ("a".to_owned(), (*k).to_owned()))
                .collect()
        };
        assert_eq!(
            r.unmapped_keys,
            pairs(&["links", "record.links", "record.title"])
        );
        assert!(
            r.unknown_keys.is_empty(),
            "checked by the bare name: {:?}",
            r.unknown_keys
        );
        assert_eq!(r.description_like_keys, pairs(&["links[0].summary"]));

        let swapped = r##"{"page":{"id":"p1","slug":"a","type":"g","status":"draft","body":"# A\n","description":null,"links":[]},"links":[{"summary":"x"}]}"##;
        let e = from_jsonl("s", swapped, "", "").expect("a valid export");
        let r = plan(&e, &BTreeMap::new(), &|_| false).report;
        assert_eq!(r.description_like_keys, pairs(&["record.links[0].summary"]));
        assert!(no_description_key("record.links") && !no_description_key("record.intro"));
    }

    #[test]
    fn a_mapped_prefix_is_prepended() {
        let dirs = BTreeMap::from([("hive".to_owned(), "services/hive".to_owned())]);
        let s = source("dir:hive/crates/x.rs", "derived", false);
        assert_eq!(
            to_anchor(&s, &dirs, &|_| false).map(|a| a.reference()),
            Some("file:services/hive/crates/x.rs".to_owned())
        );
        assert!(to_anchor(&source("dir:other/x", "derived", false), &dirs, &|_| false).is_none());
    }
}
