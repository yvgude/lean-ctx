// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx docs`: the project's doc pages, with or without Hive.
//!
//! A checkout linked to a vault reads the vault's pages (as `ctx_doc` and
//! `hive docs` do); any other checkout keeps its pages on this Instance
//! ([`lean_ctx_hive::local_docs`]): every write is one revision with its
//! author, and history is kept. `--json` prints Local Read v1 documents
//! (`docs.list`, `docs.page`, `docs.history`, `docs.saved`).

use std::io::Read;

use serde::Serialize;

use lean_ctx_doc::local::{self, Author, Revision};
use lean_ctx_hive::HiveError;
use lean_ctx_hive::local_docs::{self, SharedTo, shared_of};
use lean_ctx_server::tools::ctx_doc;

use super::local_read::docs::{PageDetail, PageRow, VaultRef, page_detail};
use super::local_read::{self, Code, Failure};

pub(crate) const USAGE: &str = "lean-ctx docs list [--rev N] [--json]
lean-ctx docs show <slug> [--rev N] [--json]
lean-ctx docs history <slug> [--json]
lean-ctx docs write <slug> (--content TEXT | --stdin) [--type T] [--status draft|canonical] [--author NAME] [--json]
lean-ctx docs move <slug> <new-slug> [--author NAME] [--json]
lean-ctx docs delete <slug> [--author NAME] [--json]
  (all take --project-root P; history, move, delete and --rev are for local docs)";

/// The largest page a write takes, in bytes of markdown.
const MAX_WRITE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Serialize)]
pub(crate) struct LocalRow {
    #[serde(flatten)]
    pub row: PageRow,
    pub updated: Option<String>,
    pub updated_by: Option<Author>,
    pub shared: Option<SharedTo>,
}

#[derive(Debug, Serialize)]
pub(crate) struct LocalList {
    pub project_root: String,
    pub source: &'static str,
    pub vault: Option<VaultRef>,
    /// The revision the pages are shown as of; `null` = now.
    pub rev: Option<u64>,
    pub pages: Vec<LocalRow>,
}

#[derive(Debug, Serialize)]
pub(crate) struct LocalDetail {
    #[serde(flatten)]
    pub detail: PageDetail,
    /// The slugs of the pages this page links to.
    pub links: Vec<String>,
    pub updated: Option<String>,
    pub updated_by: Option<Author>,
    pub shared: Option<SharedTo>,
}

#[derive(Debug, Serialize)]
pub(crate) struct LocalPageDoc {
    pub project_root: String,
    pub source: &'static str,
    pub vault: Option<VaultRef>,
    pub rev: Option<u64>,
    pub page: LocalDetail,
}

#[derive(Debug, Serialize)]
pub(crate) struct HistoryPage {
    /// The slug asked for.
    pub id: String,
    /// The store's page id (`n_…`).
    pub page_id: String,
    /// Whether the page is live now under this slug.
    pub live: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct History {
    pub project_root: String,
    pub source: &'static str,
    pub page: HistoryPage,
    pub revisions: Vec<Revision>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Saved {
    pub project_root: String,
    pub source: &'static str,
    pub action: String,
    pub slug: String,
    /// The new revision; `null` when nothing changed.
    pub rev: Option<u64>,
    pub events: usize,
}

fn failure(e: HiveError) -> Failure {
    match e {
        HiveError::Config(m) => Failure::invalid(m),
        other => Failure::from(other),
    }
}

fn rev_arg(args: &[String]) -> Result<Option<u64>, Failure> {
    local_read::flag(args, "--rev")?
        .map(|raw| {
            raw.parse::<u64>()
                .ok()
                .filter(|n| *n >= 1)
                .ok_or_else(|| Failure::invalid("--rev is a revision number (1 or more)"))
        })
        .transpose()
}

/// `docs.list` of the local store of `root`.
pub(crate) fn list_document(root: &str, rev: Option<u64>) -> Result<LocalList, Failure> {
    let store = local_docs::store(root).map_err(failure)?;
    let (entries, space) = local_docs::read(root, &store, rev).map_err(failure)?;
    let last = local::last_revisions(&local_upto(&entries, rev));
    let rows = super::local_read::docs::list_rows(&space);
    let pages = rows
        .into_iter()
        .map(|row| {
            let page = space.page_by_slug(&row.id);
            let id = page.map(|p| p.page.id.clone()).unwrap_or_default();
            let touched = last.get(&id);
            LocalRow {
                updated: touched.map(|r| r.at.clone()),
                updated_by: touched.map(|r| r.author.clone()),
                shared: page.and_then(shared_of),
                row,
            }
        })
        .collect();
    Ok(LocalList {
        project_root: root.to_owned(),
        source: "local",
        vault: None,
        rev,
        pages,
    })
}

/// `docs.page` of local page `slug`.
pub(crate) fn page_document(
    root: &str,
    slug: &str,
    rev: Option<u64>,
) -> Result<LocalPageDoc, Failure> {
    let store = local_docs::store(root).map_err(failure)?;
    let (entries, space) = local_docs::read(root, &store, rev).map_err(failure)?;
    let p = space
        .page_by_slug(slug)
        .ok_or_else(|| Failure::new(Code::NotFound, format!("no page {slug}")))?;
    let touched = local::last_revisions(&local_upto(&entries, rev)).remove(&p.page.id);
    let mut links: Vec<String> = space
        .links
        .get(&p.page.id)
        .into_iter()
        .flatten()
        .filter_map(|(_, to)| space.pages.get(to).map(|t| t.slug().to_owned()))
        .collect();
    links.sort();
    links.dedup();
    Ok(LocalPageDoc {
        project_root: root.to_owned(),
        source: "local",
        vault: None,
        rev,
        page: LocalDetail {
            detail: page_detail(&space, p),
            links,
            updated: touched.as_ref().map(|r| r.at.clone()),
            updated_by: touched.map(|r| r.author),
            shared: shared_of(p),
        },
    })
}

/// `docs.history` of local page `slug` (a moved or deleted one too).
pub(crate) fn history_document(root: &str, slug: &str) -> Result<History, Failure> {
    let store = local_docs::store(root).map_err(failure)?;
    let entries = store.entries().map_err(|e| failure(e.into()))?;
    let page_id = local::page_id_of(&entries, slug)
        .ok_or_else(|| Failure::new(Code::NotFound, format!("no page {slug}")))?;
    let live = local::space_of(&entries, None, &[])
        .by_slug
        .get(slug)
        .is_some_and(|id| *id == page_id);
    Ok(History {
        project_root: root.to_owned(),
        source: "local",
        revisions: local::history(&entries, &page_id),
        page: HistoryPage {
            id: slug.to_owned(),
            page_id,
            live,
        },
    })
}

fn local_upto(entries: &[local::Entry], rev: Option<u64>) -> Vec<local::Entry> {
    entries
        .iter()
        .filter(|e| rev.is_none_or(|r| e.rev <= r))
        .cloned()
        .collect()
}

/// The page markdown of a write: `--content TEXT` or `--stdin`, bounded.
fn content(args: &[String]) -> Result<String, Failure> {
    if let Some(text) = local_read::flag(args, "--content")? {
        if text.len() > MAX_WRITE_BYTES {
            return Err(Failure::invalid("the page is larger than 1 MiB"));
        }
        return Ok(text.to_owned());
    }
    if args.iter().any(|a| a == "--stdin") {
        let mut raw = String::new();
        std::io::stdin()
            .take(MAX_WRITE_BYTES as u64 + 1)
            .read_to_string(&mut raw)
            .map_err(|e| Failure::invalid(format!("stdin: {e}")))?;
        if raw.len() > MAX_WRITE_BYTES {
            return Err(Failure::invalid("the page is larger than 1 MiB"));
        }
        return Ok(raw);
    }
    Err(Failure::invalid("write needs --content TEXT or --stdin"))
}

/// A write action as the person (`--author NAME`, default `person`).
pub(crate) fn save(
    root: &str,
    action: &str,
    slug: &str,
    to: Option<&str>,
    body: Option<&str>,
    args: &[String],
) -> Result<Saved, Failure> {
    let author = Author::person(local_read::flag(args, "--author")?.unwrap_or("person"));
    let call = ctx_doc::Args {
        action,
        slug: Some(slug),
        content: body,
        kind: local_read::flag(args, "--type")?,
        status: local_read::flag(args, "--status")?,
        to,
        author: Some(author),
        ..ctx_doc::Args::default()
    };
    let done = ctx_doc::commit_local(root, &call).map_err(failure)?;
    Ok(Saved {
        project_root: root.to_owned(),
        source: "local",
        action: action.to_owned(),
        slug: to.unwrap_or(slug).to_owned(),
        rev: done.rev,
        events: done.events,
    })
}

/// The positional arguments (not flags or their values).
fn positionals(args: &[String]) -> Vec<&str> {
    const VALUED: [&str; 7] = [
        "--project-root",
        "--rev",
        "--content",
        "--type",
        "--status",
        "--author",
        "--limit",
    ];
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
            continue;
        }
        if VALUED.contains(&a.as_str()) {
            skip = true;
        } else if !a.starts_with("--") {
            out.push(a.as_str());
        }
    }
    out
}

/// `lean-ctx docs …`.
pub(crate) fn run(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") || args.first().is_some_and(|a| a == "help")
    {
        println!(
            "lean-ctx docs: the project's doc pages (local, or its linked Hive vault)\n\nusage:\n{USAGE}"
        );
        return;
    }
    let json = local_read::wants_json(args);
    let root = match local_read::project_root(args) {
        Ok(root) => root,
        Err(f) if json => local_read::fail(&f),
        Err(f) => exit_text(&f.message),
    };
    let pos = positionals(args);
    let action = pos.first().copied().unwrap_or("list");
    let slug = pos.get(1).copied();
    if json {
        run_json(&root, action, slug, pos.get(2).copied(), args);
    } else {
        run_text(&root, action, slug, pos.get(2).copied(), args);
    }
}

/// The `--json` documents of `docs <action>`, printed.
fn run_json(root: &str, action: &str, slug: Option<&str>, to: Option<&str>, args: &[String]) {
    local_read::print(&json_answer(
        root,
        action,
        slug,
        to,
        args,
        local_read::Via::Cli,
    ));
}

/// The `--json` document of `docs <action>`, for `via`; `hive docs
/// list|show --json` (CLI and socket) on a checkout no vault is linked to
/// comes here too. Never ends the process: the socket builds it inside the
/// daemon.
pub(crate) fn json_answer(
    root: &str,
    action: &str,
    slug: Option<&str>,
    to: Option<&str>,
    args: &[String],
    via: local_read::Via,
) -> local_read::Answer {
    let linked = match local_docs::is_linked(root) {
        Ok(linked) => linked,
        // Neither local nor the vault: a linked checkout never writes local.
        Err(reason) => {
            return local_read::Answer::failure(&Failure::new(Code::Unavailable, reason));
        }
    };
    if linked {
        return match action {
            // The vault's pages, as `hive docs list|show --json` prints them.
            "list" | "show" => local_read::docs::answer(&vault_args(action, slug, root), via),
            _ => local_read::Answer::failure(&Failure::new(
                Code::Forbidden,
                "this checkout's docs are in its Hive vault: write them through ctx_doc",
            )),
        };
    }
    let need_slug = || slug.ok_or_else(|| Failure::invalid(format!("docs {action} <slug>")));
    match action {
        "list" => local_read::answer_released("ctx_doc", root, "docs.list", via, || {
            list_document(root, rev_arg(args)?)
        }),
        "show" => local_read::answer_released("ctx_doc", root, "docs.page", via, || {
            page_document(root, need_slug()?, rev_arg(args)?)
        }),
        "history" => local_read::answer_released("ctx_doc", root, "docs.history", via, || {
            history_document(root, need_slug()?)
        }),
        // Writes: the release point sees the request (its body included,
        // read before) before anything is written (G08); a withheld write
        // writes nothing.
        "write" | "move" | "delete" => {
            let body = (action == "write").then(|| content(args));
            let preview = write_preview(
                action,
                slug,
                to,
                body.as_ref().and_then(|b| b.as_deref().ok()),
                args,
            );
            local_read::answer_released_write("ctx_doc", root, "docs.saved", via, &preview, || {
                match action {
                    "write" => {
                        let body = body.unwrap_or_else(|| content(args))?;
                        save(root, "write", need_slug()?, None, Some(&body), args)
                    }
                    "move" => {
                        let to =
                            to.ok_or_else(|| Failure::invalid("docs move <slug> <new-slug>"))?;
                        save(root, "move", need_slug()?, Some(to), None, args)
                    }
                    _ => save(root, "delete", need_slug()?, None, None, args),
                }
            })
        }
        other => local_read::Answer::failure(&Failure::invalid(format!(
            "docs {other}: see `lean-ctx docs --help`"
        ))),
    }
}

/// A doc write's request as the release point sees it before anything is
/// written (G08), for `--json`, the socket and plain text alike.
fn write_preview(
    action: &str,
    slug: Option<&str>,
    to: Option<&str>,
    body: Option<&str>,
    args: &[String],
) -> serde_json::Value {
    let mut preview = local_read::request_preview(args, &["--type", "--status", "--author"]);
    if let serde_json::Value::Object(fields) = &mut preview {
        fields.insert("action".into(), action.into());
        for (key, text) in [("slug", slug), ("to", to), ("content", body)] {
            if let Some(text) = text {
                fields.insert(key.into(), text.into());
            }
        }
    }
    preview
}

/// Every action `ctx_doc` writes (local or to the linked vault).
const TEXT_WRITES: [&str; 6] = ["write", "edit", "note", "confirm", "move", "delete"];

/// The plain-text command's release check before a write (G08), the same
/// decision on the same preview as [`json_answer`]'s: `Err` when withheld,
/// and then nothing is written, locally or to the vault.
fn release_text_write(
    root: &str,
    action: &str,
    slug: Option<&str>,
    to: Option<&str>,
    body: Option<&str>,
    args: &[String],
) -> Result<(), Failure> {
    if !TEXT_WRITES.contains(&action) {
        return Ok(());
    }
    local_read::release_text_write(
        "ctx_doc",
        root,
        "docs.saved",
        &write_preview(action, slug, to, body, args),
    )
}

/// `list|show <slug> --json --project-root <root>` for the vault reader:
/// always the checkout this answer was asked for, never the reader's cwd.
fn vault_args(action: &str, slug: Option<&str>, root: &str) -> Vec<String> {
    let mut out = vec![action.to_owned()];
    out.extend(slug.map(str::to_owned));
    out.extend([
        "--json".to_owned(),
        "--project-root".to_owned(),
        root.to_owned(),
    ]);
    out
}

fn run_text(root: &str, action: &str, slug: Option<&str>, to: Option<&str>, args: &[String]) {
    let rev = match rev_arg(args) {
        Ok(rev) => rev,
        Err(f) => exit_text(&f.message),
    };
    let body = if action == "write" {
        match content(args) {
            Ok(body) => Some(body),
            Err(f) => exit_text(&f.message),
        }
    } else {
        None
    };
    let author = match local_read::flag(args, "--author") {
        Ok(name) => Author::person(name.unwrap_or("person")),
        Err(f) => exit_text(&f.message),
    };
    let (kind, status) = (
        local_read::flag(args, "--type").ok().flatten(),
        local_read::flag(args, "--status").ok().flatten(),
    );
    let call = ctx_doc::Args {
        action,
        slug,
        content: body.as_deref(),
        kind,
        status,
        to,
        rev,
        author: Some(author),
        ..ctx_doc::Args::default()
    };
    match text_answer(root, &call, args) {
        Ok(out) => {
            print!("{out}");
            if !out.ends_with('\n') {
                println!();
            }
        }
        Err(message) => exit_text(&message),
    }
}

/// A plain-text call as `run_text` builds it, by the person.
#[cfg(test)]
pub(crate) fn text_call<'a>(
    action: &'a str,
    slug: Option<&'a str>,
    to: Option<&'a str>,
    content: Option<&'a str>,
) -> ctx_doc::Args<'a> {
    ctx_doc::Args {
        action,
        slug,
        content,
        to,
        author: Some(Author::person("person")),
        ..ctx_doc::Args::default()
    }
}

/// The plain-text answer of `call`: a write passes the release check first
/// (G08), a withheld one writes nothing; `Err` is the error line.
pub(crate) fn text_answer(
    root: &str,
    call: &ctx_doc::Args<'_>,
    args: &[String],
) -> Result<String, String> {
    release_text_write(root, call.action, call.slug, call.to, call.content, args)
        .map_err(|f| f.message)?;
    let out = ctx_doc::handle(root, call);
    match out.strip_prefix("ctx_doc: ") {
        Some(error) => Err(error.to_owned()),
        None => Ok(out),
    }
}

fn exit_text(message: &str) -> ! {
    eprintln!("lean-ctx docs: {message}");
    std::process::exit(1);
}

#[cfg(test)]
#[path = "docs_cmd_tests.rs"]
mod tests;
