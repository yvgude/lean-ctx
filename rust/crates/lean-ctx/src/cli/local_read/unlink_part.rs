// SPDX-License-Identifier: Apache-2.0
//! `cloud.unlink` (rc4-6): stop sharing one project folder with its vault,
//! on this machine only ([`lean_ctx_cloud::unlink`]).
//!
//! - `cloud.unlink`: `POST /v1/local/cloud/unlink`, body `{project_root}`,
//!   app-only (`app_key`). CLI `lean-ctx cloud unlink [--project-root <dir>]
//!   [--json]` (the person's own terminal) prints the same document.
//!
//! The document (besides `contract` and `kind`):
//! - `{ok: true, project_root, removed: "link"|"none", vault?: {id, name},
//!   drafts_dropped, link_file, link_file_reason?}`; `removed: "none"` and
//!   no `vault` when the folder was not shared. `drafts_dropped`: unsent
//!   knowledge drafts of the folder that were dropped (0 when none).
//!   `link_file`: `removed` | `kept` | `absent` (the folder's
//!   `.lean-ctx-cloud.json`); `link_file_reason` only with `kept`:
//!   `other_vault` | `unreadable` | `symlink`.
//! - `{ok: false, code, error, main_root?}`, `code` one of `invalid_root`
//!   (no project folder, or unreadable), `shared_from_main` (a git worktree
//!   shared through its main checkout `main_root`: stop sharing there) and
//!   `failed` (a file could not be written; `error` names what was already
//!   done).
//!
//! Only the binding is removed; local knowledge, cache and sessions stay.

use serde_json::{Value, json};

use super::routes::{Method, Param, Project, Route, Shape};
use super::{Answer, Code, Via};

/// The kind of the unlink document.
pub(crate) const KIND: &str = "cloud.unlink";

/// This part's routes. App-only: no CLI words, listed as a socket verb.
pub(crate) const ROUTES: &[Route] = &[Route {
    kind: KIND,
    method: Method::Post,
    path: "/v1/local/cloud/unlink",
    cli: &[],
    action: &["unlink"],
    params: &[Param {
        name: "project_root",
        flag: "--project-root",
        shape: Shape::Value,
    }],
    // Optional so a folder that is no project reaches the answer, which
    // names it `invalid_root`; without a folder the answer is the same.
    project: Project::Optional,
    names: Some(("cloud", "unlink")),
}];

/// This part's document kinds.
pub(crate) const KINDS: &[&str] = &[KIND];

/// Command path → CLI verbs (`unlink` is a socket verb only).
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[];

const INVALID_ROOT: &str = "invalid_root";
const SHARED_FROM_MAIN: &str = "shared_from_main";
const FAILED: &str = "failed";

/// The body of the unlink document, and the code of a refusal.
fn build(args: &[String], via: Via) -> (Value, Option<Code>) {
    let refused = |code: &str, error: String, status: Code| {
        (
            json!({"ok": false, "code": code, "error": error}),
            Some(status),
        )
    };
    // The socket has no working directory of the reader's: only a named
    // folder counts there.
    let named = match via {
        Via::Cli => super::project_root(args).map(Some),
        Via::Socket => super::named_root(args),
    };
    let raw = match named {
        Ok(Some(raw)) => raw,
        Ok(None) => {
            return refused(
                INVALID_ROOT,
                "project_root is required".into(),
                Code::NoProject,
            );
        }
        Err(failure) => return refused(INVALID_ROOT, failure.message, Code::NoProject),
    };
    let root = match super::routes::project_dir(&raw) {
        Ok(root) => root,
        Err(failure) => return refused(INVALID_ROOT, failure.message, Code::NoProject),
    };
    let store = match lean_ctx_cloud::CloudStore::open() {
        Ok(store) => store,
        Err(error) => return refused(FAILED, error.to_string(), Code::Internal),
    };
    // A link file naming another server than the connected Cloud's stays.
    let server = store
        .instance()
        .ok()
        .flatten()
        .and_then(|instance| super::vaults::server_of(&instance.url));
    match lean_ctx_cloud::unlink::unlink(
        store.data_dir(),
        std::path::Path::new(&root),
        server.as_deref(),
    ) {
        Ok(done) => {
            let mut body = json!({
                "ok": true,
                "project_root": root,
                "removed": if done.vault.is_some() { "link" } else { "none" },
                "drafts_dropped": done.drafts_dropped,
                "link_file": done.link_file.as_str(),
            });
            if let Some(vault) = &done.vault {
                body["vault"] =
                    json!({"id": vault.vault_id, "name": format!("{}/{}", vault.ns, vault.name)});
            }
            if let Some(reason) = done.link_file.reason() {
                body["link_file_reason"] = json!(reason);
            }
            (body, None)
        }
        Err(lean_ctx_cloud::unlink::UnlinkError::SharedFromMain { main }) => {
            let error = lean_ctx_cloud::unlink::UnlinkError::SharedFromMain { main: main.clone() };
            let (mut body, code) = refused(SHARED_FROM_MAIN, error.to_string(), Code::InvalidState);
            body["main_root"] = json!(main);
            (body, code)
        }
        Err(error) => refused(FAILED, error.to_string(), Code::Internal),
    }
}

/// The unlink document as an answer: the body under `contract` and `kind`,
/// with the HTTP status of a refusal.
fn answer_of(body: &Value, failed: Option<Code>) -> Answer {
    Answer {
        text: super::document(KIND, body),
        failed,
        notice: None,
        released: false,
        withheld: false,
    }
}

/// The document of `kind` for the socket. App-only: the socket checked the
/// app key before this; the generic CLI path is refused (the person's own
/// `cloud unlink` goes through [`run`]).
pub(crate) fn answer(kind: &str, args: &[String], via: Via) -> Answer {
    match kind {
        KIND if via == Via::Cli => Answer::failure(&super::app_key::refusal()),
        KIND => {
            let (body, failed) = build(args, via);
            answer_of(&body, failed)
        }
        other => Answer::failure(&super::Failure::invalid(format!("unknown kind {other}"))),
    }
}

/// `lean-ctx cloud unlink --json`: print the document, exit 1 on a refusal.
pub(crate) fn run(args: &[String]) {
    let (body, failed) = build(args, Via::Cli);
    super::print(&answer_of(&body, failed));
}

/// `lean-ctx cloud unlink`: one line for the person.
pub(crate) fn run_human(args: &[String]) {
    let (body, failed) = build(args, Via::Cli);
    let root = body["project_root"].as_str().unwrap_or_default();
    if failed.is_some() {
        eprintln!(
            "cloud unlink: {}",
            body["error"].as_str().unwrap_or_default()
        );
        std::process::exit(1);
    }
    let dropped = match body["drafts_dropped"].as_u64().unwrap_or_default() {
        0 => String::new(),
        1 => " Dropped 1 queued draft.".to_string(),
        n => format!(" Dropped {n} queued drafts."),
    };
    match body["vault"]["name"].as_str() {
        Some(vault) => println!(
            "Stopped sharing {root} with the vault {vault}. The vault and its data stay in Cloud.{dropped}"
        ),
        None => println!("{root} is not shared with a vault; nothing to stop."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn unlink_is_an_app_only_post_route() {
        assert!(super::super::part_of(KIND).is_some(), "{KIND} has no part");
        let route = super::super::routes::all()
            .find(|r| r.kind == KIND)
            .expect("routed");
        assert!(matches!(route.method, Method::Post));
        assert_eq!(route.path, "/v1/local/cloud/unlink");
        assert!(route.cli.is_empty(), "no CLI words");
        assert!(super::super::app_key::guards(KIND));
        // Refused on the generic CLI path before anything is read.
        let refused = super::super::kinds::answer(KIND, &words(&["unlink", "--json"]), Via::Cli);
        assert_eq!(refused.failed, Some(Code::AppOnly));
    }

    #[test]
    fn a_folder_that_is_not_shared_answers_ok_with_nothing_removed() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        let root = repo.path().canonicalize().unwrap();
        let root = root.to_string_lossy();
        let answer = answer(
            KIND,
            &words(&["unlink", "--json", "--project-root", &root]),
            Via::Socket,
        );
        assert_eq!(answer.failed, None, "{}", answer.text);
        let doc: Value = serde_json::from_str(&answer.text).unwrap();
        assert_eq!(doc["contract"], "local-read/1");
        assert_eq!(doc["kind"], KIND);
        assert_eq!(doc["ok"], true);
        assert_eq!(doc["project_root"], root.as_ref());
        assert_eq!(doc["removed"], "none");
        assert!(doc.get("vault").is_none());
        assert_eq!(doc["drafts_dropped"], 0);
        assert_eq!(doc["link_file"], "absent");
        assert!(doc.get("link_file_reason").is_none());
    }

    #[test]
    fn a_shared_folder_answers_the_vault_it_left() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        let root = repo.path().canonicalize().unwrap();
        lean_ctx_cloud::link::link(&root, "acme/payments").unwrap();
        let store = lean_ctx_cloud::CloudStore::open().unwrap();
        let mut map = lean_ctx_cloud::vault::VaultMap::default();
        map.vaults.insert(
            "k1".into(),
            lean_ctx_cloud::vault::VaultRecord {
                vault_id: "vlt_1".into(),
                ns: "acme".into(),
                name: "payments".into(),
                sealed: false,
                roots: vec![root.to_string_lossy().into_owned()],
            },
        );
        std::fs::create_dir_all(store.data_dir()).unwrap();
        map.save(store.data_dir()).unwrap();

        let root_text = root.to_string_lossy();
        let args = words(&["unlink", "--json", "--project-root", &root_text]);
        let doc: Value = serde_json::from_str(&answer(KIND, &args, Via::Socket).text).unwrap();
        assert_eq!(doc["ok"], true);
        assert_eq!(doc["removed"], "link");
        assert_eq!(
            doc["vault"],
            json!({"id": "vlt_1", "name": "acme/payments"})
        );
        assert_eq!(doc["drafts_dropped"], 0);
        assert_eq!(doc["link_file"], "removed");
        assert!(doc.get("link_file_reason").is_none());
        assert!(!root.join(lean_ctx_cloud::link::LINK_FILE).exists());
        // `cloud status` reads the same map: the folder is no longer shared.
        let map = lean_ctx_cloud::vault::VaultMap::load(store.data_dir());
        assert!(map.for_root(&root_text).is_none());
        // Idempotent.
        let again: Value = serde_json::from_str(&answer(KIND, &args, Via::Socket).text).unwrap();
        assert_eq!(again["removed"], "none");
        assert_eq!(again["link_file"], "absent");
        assert_eq!(again["drafts_dropped"], 0);
    }

    #[test]
    fn unlink_answers_a_kept_link_file_with_its_reason() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        let root = repo.path().canonicalize().unwrap();
        // The folder's link file names another vault than the one it shares.
        lean_ctx_cloud::link::link(&root, "acme/other").unwrap();
        let store = lean_ctx_cloud::CloudStore::open().unwrap();
        let mut map = lean_ctx_cloud::vault::VaultMap::default();
        map.vaults.insert(
            "k1".into(),
            lean_ctx_cloud::vault::VaultRecord {
                vault_id: "vlt_1".into(),
                ns: "acme".into(),
                name: "payments".into(),
                sealed: false,
                roots: vec![root.to_string_lossy().into_owned()],
            },
        );
        std::fs::create_dir_all(store.data_dir()).unwrap();
        map.save(store.data_dir()).unwrap();

        let root_text = root.to_string_lossy();
        let args = words(&["unlink", "--json", "--project-root", &root_text]);
        let doc: Value = serde_json::from_str(&answer(KIND, &args, Via::Socket).text).unwrap();
        assert_eq!(doc["ok"], true);
        assert_eq!(doc["removed"], "link");
        assert_eq!(doc["drafts_dropped"], 0);
        assert_eq!(doc["link_file"], "kept");
        assert_eq!(doc["link_file_reason"], "other_vault");
        assert!(root.join(lean_ctx_cloud::link::LINK_FILE).exists());
        // The kept link file does not share the folder again (S1).
        assert!(lean_ctx_cloud::unlinked::contains(store.data_dir(), &root));
    }

    #[test]
    fn a_folder_that_is_no_project_is_an_invalid_root() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let answer = answer(
            KIND,
            &words(&["unlink", "--json", "--project-root", "/no/such/folder"]),
            Via::Socket,
        );
        assert_eq!(answer.failed, Some(Code::NoProject));
        let doc: Value = serde_json::from_str(&answer.text).unwrap();
        assert_eq!(doc["ok"], false);
        assert_eq!(doc["code"], "invalid_root");
        assert!(doc["error"].is_string());
        // Without a folder the socket never falls back to its own one.
        let none = super::answer(KIND, &words(&["unlink", "--json"]), Via::Socket);
        let doc: Value = serde_json::from_str(&none.text).unwrap();
        assert_eq!(doc["code"], "invalid_root");
    }
}
