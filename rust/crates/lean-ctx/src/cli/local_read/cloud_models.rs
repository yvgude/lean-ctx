// SPDX-License-Identifier: Apache-2.0
//! `cloud.models.catalog` (3.12.7): the model catalog of the connected
//! LeanCTX Cloud for a local reader such as LeanCTX Code, as a Local Read
//! kind.
//!
//! The engine reads `GET /v1/models/catalog` with its instance token (as
//! `lean-ctx models` does) and hands the document on unchanged in
//! `catalog`. Beside it:
//!
//! - `verified`: `true` when the catalog's Ed25519 signature verifies
//!   against a key set v3 `policy` key (as the effective policy is
//!   verified), `false` when it is signed but does not verify, `null` when
//!   the Cloud sent no signature;
//! - `models_subscription`: the lease is valid or in grace and grants the
//!   right `models.subscription`;
//! - `gateway_base_url`: the LeanCTX gateway of the enrolled Cloud.
//!
//! No token or key reaches the document, the logs or an error.
//!
//! - `cloud.models.catalog`: `GET /v1/local/cloud/models/catalog`, CLI
//!   `lean-ctx cloud models catalog --json` (also `lean-ctx models --json`).
//!
//! Wired as one `local_read::Part`.

use serde::Serialize;
use serde_json::Value;

use lean_ctx_cloud::models_access::CatalogRead;

use super::routes::{Method, Project, Route};
use super::{Answer, Failure, Via};

/// The kind of the catalog document.
pub(crate) const KIND: &str = "cloud.models.catalog";

/// This part's routes (Route as in `routes.rs`).
pub(crate) const ROUTES: &[Route] = &[Route {
    kind: KIND,
    method: Method::Get,
    path: "/v1/local/cloud/models/catalog",
    cli: &["cloud", "models", "catalog", "--json"],
    action: &["catalog"],
    params: &[],
    project: Project::None,
    names: None,
}];

/// This part's document kinds.
pub(crate) const KINDS: &[&str] = &[KIND];

/// Command path → CLI verbs, as `capabilities::COMMANDS`.
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[("cloud.models", &["catalog"])];

/// The release tool of the catalog (a read, like the model credit).
const READ_TOOL: &str = "ctx_knowledge";

/// The catalog document's body.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct CatalogDoc {
    /// `GET /v1/models/catalog` as the Cloud answered it.
    pub catalog: Value,
    /// Signature check: `true`, `false`, or `null` for an unsigned catalog.
    pub verified: Option<bool>,
    pub models_subscription: bool,
    pub gateway_base_url: String,
}

pub(crate) fn build(read: CatalogRead) -> CatalogDoc {
    CatalogDoc {
        catalog: read.document,
        verified: read.verified,
        models_subscription: read.models_subscription,
        gateway_base_url: read.gateway_base_url,
    }
}

fn read() -> Result<CatalogDoc, Failure> {
    Ok(build(lean_ctx_cloud::models_access::read_catalog()?))
}

/// The released document of `kind`, for the CLI or the socket.
pub(crate) fn answer(kind: &str, _args: &[String], via: Via) -> Answer {
    match kind {
        KIND => super::answer_released(READ_TOOL, "", kind, via, read),
        other => Answer::failure(&Failure::invalid(format!("unknown kind {other}"))),
    }
}

/// `lean-ctx cloud models catalog --json` and `lean-ctx models --json`: the
/// Local Read document; `false` for every other command line.
pub(crate) fn try_run(command: &str, rest: &[String]) -> bool {
    if !super::wants_json(rest) {
        return false;
    }
    let words: Vec<&str> = rest
        .iter()
        .map(String::as_str)
        .filter(|w| !w.starts_with("--"))
        .collect();
    let taken = match command {
        "cloud" => words == ["models", "catalog"],
        "models" => words.is_empty(),
        _ => false,
    };
    if taken {
        super::print(&answer(KIND, rest, Via::Cli));
    }
    taken
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_catalog_is_dispatched_routed_and_listed_without_a_project() {
        assert!(super::super::part_of(KIND).is_some(), "{KIND} has no part");
        let route = super::super::routes::all()
            .find(|r| r.kind == KIND)
            .expect("routed");
        assert!(matches!(route.method, Method::Get));
        assert!(matches!(route.project, Project::None));
        assert!(
            super::super::routes::table_json()
                .to_string()
                .contains("/v1/local/cloud/models/catalog")
        );
    }

    #[test]
    fn the_cloud_document_reaches_the_reader_unchanged() {
        let catalog = json!({
            "schema": "leanctx.cloud.model-catalog/v1",
            "catalog_version": "1",
            "price_version": "1",
            "models": [],
            "direct": [],
            "key_id": "0123456789abcdef",
            "signature": "00"
        });
        let doc = build(CatalogRead {
            document: catalog.clone(),
            verified: Some(false),
            models_subscription: true,
            gateway_base_url: "https://cloud.example.com/gateway/v1".into(),
        });
        let text = super::super::document(KIND, &doc);
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["contract"], "local-read/1");
        assert_eq!(value["kind"], KIND);
        assert_eq!(value["catalog"], catalog);
        assert_eq!(value["verified"], false);
        assert_eq!(value["models_subscription"], true);
        assert_eq!(
            value["gateway_base_url"],
            "https://cloud.example.com/gateway/v1"
        );
        let unsigned = build(CatalogRead {
            document: json!({}),
            verified: None,
            models_subscription: false,
            gateway_base_url: String::new(),
        });
        assert!(serde_json::to_value(unsigned).unwrap()["verified"].is_null());
    }

    #[test]
    fn without_a_connection_the_answer_is_not_connected() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let failure = read().expect_err("no instance here");
        assert_eq!(failure.code, super::super::Code::NotConnected);
    }

    #[test]
    fn only_the_catalog_command_lines_are_taken() {
        assert!(
            !try_run("cloud", &words(&["models", "catalog"])),
            "needs --json"
        );
        assert!(!try_run("models", &words(&["help"])));
        assert!(!try_run("cloud", &words(&["billing", "credit", "--json"])));
        assert!(!try_run("cloud", &words(&["models", "--json"])));
        assert!(!try_run("models", &words(&["list", "--json"])));
    }
}
