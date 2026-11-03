// SPDX-License-Identifier: Apache-2.0

//! Reading an emspace export into pages, for the move of a space to Hive.
//!
//! The export is the JSON an emspace program writes (`Export`): pages with
//! their sources and open notes, tasks and boards. [`plan`] turns it into
//! pages of this model and a report that says what could not be carried over
//! as it was, so the migration is checked before anything is written.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

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
    #[serde(default)]
    pub retired: bool,
    #[serde(default)]
    pub sources: Vec<ExportSource>,
    #[serde(default)]
    pub notes: Vec<ExportNote>,
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
}

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
    pub page: Option<String>,
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

    let tasks: Vec<TaskPlan> = export
        .tasks
        .iter()
        .map(|t| TaskPlan {
            source_id: t.id.clone(),
            title: t.title.clone(),
            body: t.body.clone(),
            status: t.status.clone(),
            priority: t.priority.clone().unwrap_or_else(|| "normal".to_owned()),
            labels: t.boards.iter().map(|b| format!("board:{b}")).collect(),
            page: t.page.clone().filter(|s| slugs.contains(s.as_str())),
        })
        .collect();
    report.tasks = tasks.len();
    Plan {
        pages,
        tasks,
        report,
    }
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
                    ..Default::default()
                },
                ExportPage {
                    slug: "old".into(),
                    kind: "x".into(),
                    status: "canonical".into(),
                    retired: true,
                    body: "# O\n".into(),
                    ..Default::default()
                },
            ],
            tasks: vec![ExportTask {
                id: "t1".into(),
                title: "Do It".into(),
                status: "todo".into(),
                boards: vec!["w".into()],
                page: Some("old".into()),
                ..Default::default()
            }],
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

        let t = &plan.tasks[0];
        assert_eq!(t.priority, "normal");
        assert_eq!(t.labels, ["board:w"]);
        assert_eq!(t.page, None, "a task about a retired page loses the link");
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
