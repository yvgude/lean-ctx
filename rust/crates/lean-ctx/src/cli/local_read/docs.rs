// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hive docs list|show --json`: the doc pages of the vault linked
//! to this checkout, from the local replica (as `ctx_doc` reads them). No
//! request leaves the machine; only a vault linked here is read.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use lean_ctx_doc::model::Section;
use lean_ctx_doc::replica::{DocSpace, PageView};
use lean_ctx_doc::state::{DocState, StateReason};
use lean_ctx_hive::HiveStore;
use lean_ctx_hive::replica::Replica;

use super::{Code, Failure};

/// The replica of the vault linked to `root`, opened read only.
pub(crate) struct LinkedVault {
    /// `ns/name`.
    pub address: String,
    pub replica: Replica,
}

/// Opens the replica of the vault linked to `root`: `not_connected` without
/// an enrollment, `no_vault` when nothing is linked here or no replica was
/// synced yet. Creates nothing.
pub(crate) fn open_linked(root: &str) -> Result<LinkedVault, Failure> {
    let store = HiveStore::open()?;
    if store.instance()?.is_none() {
        return Err(Failure::new(
            Code::NotConnected,
            "this machine is not connected to Hive",
        ));
    }
    let record = lean_ctx_hive::vault::VaultMap::load(store.data_dir())
        .for_root(root)
        .cloned()
        .ok_or_else(|| {
            Failure::new(Code::NoVault, "this checkout is not linked to a Hive vault")
        })?;
    let dir = lean_ctx_hive::replica::store::place_dir(store.data_dir(), &record.vault_id);
    if !dir.join("replica.db").exists() {
        return Err(Failure::new(
            Code::NoVault,
            format!("{}/{} has no replica here yet", record.ns, record.name),
        ));
    }
    let (mk, _) = lean_ctx_hive::replica::crypt::master_key(store.config_dir(), false)?;
    let (replica, _) = Replica::open(store.data_dir(), &record.vault_id, &mk)?;
    Ok(LinkedVault {
        address: format!("{}/{}", record.ns, record.name),
        replica,
    })
}

#[derive(Debug, Serialize)]
pub(crate) struct VaultRef {
    pub address: String,
    pub linked: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct DocsList {
    pub project_root: String,
    /// `vault`; local pages print `local` ([`crate::cli::docs_cmd`]).
    pub source: &'static str,
    pub vault: VaultRef,
    pub pages: Vec<PageRow>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct PageRow {
    pub id: String,
    pub title: String,
    pub page_kind: String,
    pub status: String,
    pub anchor_state: &'static str,
    pub sections: usize,
    pub notes: usize,
    pub anchors: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct DocsPage {
    pub project_root: String,
    pub source: &'static str,
    pub vault: VaultRef,
    pub page: PageDetail,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct PageDetail {
    pub id: String,
    pub title: String,
    pub page_kind: String,
    pub status: String,
    pub anchor_state: &'static str,
    pub sections: Vec<SectionRow>,
    pub anchors: Vec<AnchorRow>,
    pub notes: Vec<NoteRow>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct SectionRow {
    pub order: u32,
    pub heading: Option<String>,
    pub text: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct AnchorRow {
    /// `file`, `dir` or `glob`.
    pub anchor_kind: String,
    pub path: String,
    pub symbol: Option<String>,
    /// `derived` or `cites`.
    pub role: String,
    pub state: &'static str,
    pub noted: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct NoteRow {
    pub body: String,
    pub covers: Vec<String>,
    pub anchor_state: &'static str,
    /// The `order` of the section the note is about; `null` for the page.
    pub section: Option<u32>,
}

/// `fresh`, `to_verify` or `gone` of a page (a noted page reads fresh).
fn page_anchor_state(p: &PageView) -> &'static str {
    match (p.state.state, p.state.reason) {
        (DocState::Fresh | DocState::Noted, _) => "fresh",
        (DocState::Stale, Some(StateReason::AnchorGone)) => "gone",
        (DocState::Stale, _) => "to_verify",
    }
}

fn str_field(p: &PageView, field: &str, default: &str) -> String {
    p.page.str(field).unwrap_or(default).to_owned()
}

fn page_anchors(p: &PageView) -> &[Value] {
    p.page
        .fields
        .get("anchors")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Ref → digest observed now (`None`: gone) of every anchor that moved.
fn drift(p: &PageView) -> BTreeMap<String, Option<String>> {
    p.page
        .fields
        .get("drift")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|d| {
            Some((
                d.get("ref")?.as_str()?.to_owned(),
                d.get("observed").and_then(Value::as_str).map(str::to_owned),
            ))
        })
        .collect()
}

fn moved_state(observed: Option<&Option<String>>) -> &'static str {
    match observed {
        None => "fresh",
        Some(None) => "gone",
        Some(Some(_)) => "to_verify",
    }
}

pub(crate) fn page_row(p: &PageView) -> PageRow {
    PageRow {
        id: p.slug().to_owned(),
        title: p.title().to_owned(),
        page_kind: str_field(p, "type", ""),
        status: str_field(p, "status", "draft"),
        anchor_state: page_anchor_state(p),
        sections: p.sections.len(),
        notes: p.notes.iter().filter(|n| n.open).count(),
        anchors: page_anchors(p).len(),
    }
}

pub(crate) fn page_detail(space: &DocSpace, p: &PageView) -> PageDetail {
    let slug_of = |id: &str| space.pages.get(id).map(|p| p.slug().to_owned());
    let moved = drift(p);
    let uncovered: Vec<&str> = p.state.uncovered.iter().map(String::as_str).collect();
    let anchors = page_anchors(p)
        .iter()
        .filter_map(|a| {
            let reference = a.get("ref")?.as_str()?;
            let (kind, path) = reference.split_once(':').unwrap_or(("file", reference));
            let observed = moved.get(reference);
            Some(AnchorRow {
                anchor_kind: kind.to_owned(),
                path: path.to_owned(),
                symbol: None,
                role: a
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("derived")
                    .to_owned(),
                state: moved_state(observed),
                noted: observed.is_some() && !uncovered.contains(&reference),
            })
        })
        .collect();
    let section_order: BTreeMap<&str, u32> = p
        .sections
        .iter()
        .map(|s| (s.id.as_str(), s.order))
        .collect();
    let notes = p
        .notes
        .iter()
        .filter(|n| n.open)
        .map(|n| {
            let covers: Vec<String> = n.covers.iter().map(|c| c.reference.clone()).collect();
            let worst = covers
                .iter()
                .filter(|r| uncovered.contains(&r.as_str()))
                .map(|r| moved_state(moved.get(r)))
                .max_by_key(|s| match *s {
                    "gone" => 2,
                    "to_verify" => 1,
                    _ => 0,
                })
                .unwrap_or("fresh");
            NoteRow {
                body: n.body.clone(),
                covers,
                anchor_state: worst,
                section: section_order.get(n.about.as_str()).copied(),
            }
        })
        .collect();
    PageDetail {
        id: p.slug().to_owned(),
        title: p.title().to_owned(),
        page_kind: str_field(p, "type", ""),
        status: str_field(p, "status", "draft"),
        anchor_state: page_anchor_state(p),
        sections: p
            .sections
            .iter()
            .map(|s| {
                let text = lean_ctx_doc::links::to_slugs(&s.body, slug_of);
                let heading = Section {
                    order: s.order,
                    text: text.clone(),
                }
                .heading()
                .map(str::to_owned);
                SectionRow {
                    order: s.order,
                    heading,
                    text,
                }
            })
            .collect(),
        anchors,
        notes,
    }
}

/// The pages of `space`, by slug.
pub(crate) fn list_rows(space: &DocSpace) -> Vec<PageRow> {
    let mut pages: Vec<&PageView> = space.pages.values().collect();
    pages.sort_by(|a, b| a.slug().cmp(b.slug()));
    pages.into_iter().map(page_row).collect()
}

/// `hive docs list|show <id> --json [--project-root P]`.
pub(crate) fn run(args: &[String]) {
    super::print(&answer(args, super::Via::Cli));
}

/// The document of `hive docs list|show <id> --json`, for `via`.
pub(crate) fn answer(args: &[String], via: super::Via) -> super::Answer {
    let positional = super::positionals(args);
    let action = positional.first().copied().unwrap_or("list");
    let root = match super::project_root(args) {
        Ok(root) => root,
        Err(failure) => return super::Answer::failure(&failure),
    };
    // No vault linked here: the project's local pages (`"source": "local"`).
    // A link that cannot be read is `unavailable` inside docs_cmd::json_answer.
    let linked = lean_ctx_hive::local_docs::is_linked(&root);
    if matches!(action, "list" | "show") && !matches!(linked, Ok(true)) {
        let slug = positional.get(1).copied();
        return crate::cli::docs_cmd::json_answer(&root, action, slug, None, args, via);
    }
    match action {
        "list" => super::answer_released("ctx_doc", &root, "docs.list", via, || {
            let vault = open_linked(&root)?;
            let space = lean_ctx_hive::docs::space(&vault.replica)?;
            Ok(DocsList {
                project_root: root.clone(),
                source: "vault",
                vault: VaultRef {
                    address: vault.address,
                    linked: true,
                },
                pages: list_rows(&space),
            })
        }),
        "show" => {
            let Some(id) = positional.get(1).copied() else {
                return super::Answer::failure(&Failure::invalid("hive docs show <id> --json"));
            };
            super::answer_released("ctx_doc", &root, "docs.page", via, || {
                let vault = open_linked(&root)?;
                let space = lean_ctx_hive::docs::space(&vault.replica)?;
                let page = space
                    .page_by_slug(id)
                    .ok_or_else(|| Failure::new(Code::NotFound, format!("no page {id}")))?;
                Ok(DocsPage {
                    project_root: root.clone(),
                    source: "vault",
                    vault: VaultRef {
                        address: vault.address,
                        linked: true,
                    },
                    page: page_detail(&space, page),
                })
            })
        }
        other => super::Answer::failure(&Failure::invalid(format!(
            "hive docs {other}: --json supports list and show"
        ))),
    }
}
