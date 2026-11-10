// SPDX-License-Identifier: Apache-2.0
//! A project's docs on this Instance alone ([`lean_ctx_doc::local`]): the
//! store of a checkout that no vault is linked to. Keyed like knowledge
//! (`<data dir>/docs/<hash_project_root>`), so the CLI, the MCP tool and
//! Hive Desktop find the same pages. Nothing here contacts Hive.
//!
//! A linked checkout keeps reading its vault; [`pages`] and [`mark_shared`]
//! let the link bring local pages into the vault once.

use std::path::Path;

use lean_ctx_doc::local::{self, Author, Entry, LocalDocs, Revision};
use lean_ctx_doc::replica::DocSpace;
// A doc page's field event, not a C0 fact's `maintain::field_set`: named
// apart so the C0 field guard (review::maintain tests) tells them apart.
use lean_ctx_doc::write::{EventSpec, field_set as doc_field_set};
use serde::Serialize;
use serde_json::{Value, json};

use crate::HiveError;

fn local_err(e: impl std::fmt::Display) -> HiveError {
    HiveError::Local(e.to_string())
}

impl From<local::LocalError> for HiveError {
    fn from(e: local::LocalError) -> Self {
        local_err(e)
    }
}

/// The store of the project at `root`; creates nothing until a write.
pub fn store(root: &str) -> Result<LocalDocs, HiveError> {
    if lean_ctx::core::pathutil::is_broad_or_unsafe_root(Path::new(root)) {
        return Err(HiveError::Config(format!("{root} is not a project")));
    }
    let data = lean_ctx::core::data_dir::lean_ctx_data_dir().map_err(HiveError::Local)?;
    Ok(LocalDocs::at(data.join("docs").join(
        lean_ctx::core::project_hash::hash_project_root(root),
    )))
}

/// Whether `root` is linked to a vault on this machine (its docs are the
/// vault's). Opens nothing beyond the Hive config; creates nothing.
///
/// The vault map alone decides, as for local tasks: a Hive state or a
/// vault map that cannot be read is an error (its reason; callers answer
/// `unavailable` and refuse), never "unlinked", so a linked checkout's
/// writes never go local silently. Only a vault map that does not exist
/// yet means no checkout is linked. Signed out, a linked checkout stays
/// linked: its doc reads and writes go to the vault and are refused
/// (`not_connected`) until the machine signs in again; nothing is written
/// locally for later.
pub fn is_linked(root: &str) -> Result<bool, String> {
    let unavailable =
        |e: &dyn std::fmt::Display| format!("the Hive link of this project cannot be read: {e}");
    let store = crate::HiveStore::open().map_err(|e| unavailable(&e))?;
    let path = store
        .data_dir()
        .join(lean_ctx::core::vault_map::VAULTS_FILE);
    let map: crate::vault::VaultMap = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| unavailable(&e))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(unavailable(&e)),
    };
    Ok(map.for_root(root).is_some())
}

/// RFC 3339 UTC now, whole seconds.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The journal and the pages as of `upto` (now when `None`), with the
/// anchors rechecked against the checkout. The recheck is applied to the
/// view only, never stored: a read writes nothing.
pub fn read(
    root: &str,
    store: &LocalDocs,
    upto: Option<u64>,
) -> Result<(Vec<Entry>, DocSpace), HiveError> {
    let entries = store.entries()?;
    let space = local::space_of(&entries, upto, &[]);
    let anchored = space.pages.values().any(|p| {
        p.page
            .fields
            .get("anchors")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
    });
    if !anchored || upto.is_some() {
        return Ok((entries, space));
    }
    let extra: Vec<EventSpec> = crate::docs::recheck(root, &space);
    if extra.is_empty() {
        return Ok((entries, space));
    }
    let checked = local::space_of(&entries, None, &extra);
    Ok((entries, checked))
}

/// Where a local page went when the checkout was linked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SharedTo {
    /// `ns/name`.
    pub vault: String,
    pub page_id: String,
    pub at: String,
}

/// A local page, for a link that brings pages into the vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalPage {
    /// The local page id (`n_…`).
    pub id: String,
    pub slug: String,
    pub title: String,
    /// The page markdown, `# Title` first, links as `/slug`.
    pub body: String,
    pub kind: String,
    pub status: String,
    pub updated: Option<String>,
    pub updated_by: Option<Author>,
    pub shared: Option<SharedTo>,
}

/// Where page `p` was brought on a link, if it was.
pub fn shared_of(p: &lean_ctx_doc::replica::PageView) -> Option<SharedTo> {
    let s = p.page.fields.get("shared")?;
    Some(SharedTo {
        vault: s.get("vault")?.as_str()?.to_owned(),
        page_id: s.get("page_id")?.as_str()?.to_owned(),
        at: s.get("at")?.as_str()?.to_owned(),
    })
}

/// The live local pages of `root`, by slug.
pub fn pages(root: &str) -> Result<Vec<LocalPage>, HiveError> {
    let store = store(root)?;
    let (entries, space) = read(root, &store, None)?;
    let last = local::last_revisions(&entries);
    let slug_of = |id: &str| space.pages.get(id).map(|p| p.slug().to_owned());
    let mut out: Vec<LocalPage> = space
        .pages
        .values()
        .map(|p| {
            let rev: Option<&Revision> = last.get(&p.page.id);
            LocalPage {
                id: p.page.id.clone(),
                slug: p.slug().to_owned(),
                title: p.title().to_owned(),
                body: lean_ctx_doc::links::to_slugs(&p.markdown(), slug_of),
                kind: p.page.str("type").unwrap_or("reference").to_owned(),
                status: p.page.str("status").unwrap_or("draft").to_owned(),
                updated: rev.map(|r| r.at.clone()),
                updated_by: rev.map(|r| r.author.clone()),
                shared: shared_of(p),
            }
        })
        .collect();
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(out)
}

/// Records that local page `local_id` now lives in `vault` as
/// `vault_page_id` (one journaled revision, action `share`), so a second
/// link run skips it.
pub fn mark_shared(
    root: &str,
    local_id: &str,
    vault: &str,
    vault_page_id: &str,
    author: &Author,
) -> Result<(), HiveError> {
    let store = store(root)?;
    let at = now();
    store.commit::<HiveError>(author, &at, "share", |space| {
        let page = space
            .pages
            .get(local_id)
            .ok_or_else(|| HiveError::Config(format!("no local page {local_id}")))?;
        Ok(vec![doc_field_set(
            local_id,
            "shared",
            json!({ "vault": vault, "page_id": vault_page_id, "at": at }),
            &page.page.head,
        )])
    })?;
    Ok(())
}
