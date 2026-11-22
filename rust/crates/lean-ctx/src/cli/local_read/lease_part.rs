// SPDX-License-Identifier: Apache-2.0
//! `cloud.lease.refresh` (3.12.3): renew this computer's plan lease with
//! LeanCTX Cloud now, as a Local Read kind.
//!
//! The app asks after a checkout or a plan change so the new plan applies
//! without waiting for the scheduler's next renewal. The engine sends the
//! same request the scheduler sends (`POST /v1/instances/{id}/lease/renew`,
//! empty body, instance token), verifies the signed lease against the
//! stored key set and stores it, exactly like [`Session::renew`]. Nothing
//! else is sent; no payment provider is contacted from here.
//!
//! - `cloud.lease.refresh`: `POST /v1/local/cloud/lease/refresh`, CLI
//!   `lean-ctx cloud lease refresh --json`.
//!
//! Wired as one `local_read::Part`: its routes, kinds and commands are
//! chained into the route table and the capabilities builder.
//!
//! [`Session::renew`]: lean_ctx_cloud::auth::Session::renew

use serde::Serialize;

use lean_ctx_cloud::{CloudError, CloudStore};
use lean_ctx_protocol::UtcTimestamp;
use lean_ctx_protocol::cloud::lease::{CloudLeaseV1, CloudPlan, LeaseState};

use super::routes::{Method, Project, Route};
use super::{Answer, Failure, Via};

/// The kind of the renewed lease's document.
pub(crate) const KIND: &str = "cloud.lease.refresh";

/// This part's routes (Route as in `routes.rs`).
pub(crate) const ROUTES: &[Route] = &[Route {
    kind: KIND,
    method: Method::Post,
    path: "/v1/local/cloud/lease/refresh",
    cli: &["cloud", "lease", "refresh", "--json"],
    action: &["refresh"],
    params: &[],
    project: Project::None,
    names: None,
}];

/// This part's document kinds.
pub(crate) const KINDS: &[&str] = &[KIND];

/// Command path → CLI verbs, as `capabilities::COMMANDS`.
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[("cloud.lease", &["refresh"])];

/// The release root: the lease names no project.
const NO_PROJECT: &str = "";

/// The renewed lease as the app reads it: the plan it admits and how long.
#[derive(Debug, Serialize)]
pub(crate) struct LeaseDoc {
    pub plan: CloudPlan,
    pub state: LeaseState,
    pub issued_at: UtcTimestamp,
    /// The lease's `expires_at`: paid capabilities hold until then.
    pub lease_valid_until: UtcTimestamp,
    pub grace_until: UtcTimestamp,
}

/// `lease` (in `state` at service time now) as the document's body.
pub(crate) fn lease_doc(lease: &CloudLeaseV1, state: LeaseState) -> LeaseDoc {
    LeaseDoc {
        plan: lease.plan,
        state,
        issued_at: lease.issued_at.clone(),
        lease_valid_until: lease.expires_at.clone(),
        grace_until: lease.grace_until.clone(),
    }
}

/// Renew the connected instance's lease with its Cloud and store it.
fn refresh() -> Result<LeaseDoc, Failure> {
    let store = CloudStore::open()?;
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    let root = lean_ctx_cloud::root_cert::trust_root(&store, &config, &instance.url)?;
    let mut session = lean_ctx_cloud::auth::Session::load(store, root)?;
    let verified = session.renew()?;
    Ok(lease_doc(
        &verified.lease,
        verified.state_at(lean_ctx_time::UtcDateTime::now()),
    ))
}

/// The released document of `kind`, for the CLI or the socket. A renewal
/// writes the lease file: released as a write (G08) with no request fields.
pub(crate) fn answer(kind: &str, _args: &[String], via: Via) -> Answer {
    match kind {
        KIND => super::answer_released_write(
            "ctx_agent",
            NO_PROJECT,
            kind,
            via,
            &serde_json::json!({}),
            refresh,
        ),
        other => Answer::failure(&Failure::invalid(format!("unknown kind {other}"))),
    }
}

/// `lean-ctx cloud lease refresh --json`: the Local Read document; `false`
/// for every other command line.
pub(crate) fn try_run(command: &str, rest: &[String]) -> bool {
    let words: Vec<&str> = rest.iter().map(String::as_str).take(2).collect();
    if command != "cloud" || words != ["lease", "refresh"] || !super::wants_json(rest) {
        return false;
    }
    super::print(&answer(KIND, rest, Via::Cli));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn refresh_is_dispatched_routed_and_listed_as_a_post() {
        assert!(super::super::part_of(KIND).is_some(), "{KIND} has no part");
        let table = super::super::routes::table_json();
        let text = table.to_string();
        assert!(
            text.contains("/v1/local/cloud/lease/refresh"),
            "the route is listed: {text}"
        );
        let route = super::super::routes::all()
            .find(|r| r.kind == KIND)
            .expect("routed");
        assert!(matches!(route.method, Method::Post));
        assert!(matches!(route.project, Project::None));
    }

    #[test]
    fn the_document_names_the_plan_and_how_long_the_lease_holds() {
        let lease: CloudLeaseV1 = serde_json::from_value(json!({
            "schema": "leanctx.cloud.lease/v1",
            "lease_id": "lease_1",
            "org_id": "org_1",
            "instance_id": "inst_1",
            "plan": "team",
            "seats_limit": 6,
            "rights": ["knowledge.read"],
            "issued_at": "2026-11-16T10:00:00Z",
            "expires_at": "2026-11-17T10:00:00Z",
            "grace_until": "2026-11-23T10:00:00Z",
            "server_time": "2026-11-16T10:00:00Z"
        }))
        .expect("lease");
        let text = super::super::document(KIND, &lease_doc(&lease, LeaseState::Valid));
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["contract"], "local-read/1");
        assert_eq!(value["kind"], KIND);
        assert_eq!(value["plan"], "team");
        assert_eq!(value["state"], "valid");
        assert_eq!(value["lease_valid_until"], "2026-11-17T10:00:00Z");
        assert_eq!(value["grace_until"], "2026-11-23T10:00:00Z");
        assert!(value.get("lease_id").is_none(), "only what the app reads");
    }

    #[test]
    fn without_a_connection_nothing_is_sent_and_the_answer_is_not_connected() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let failure = refresh().expect_err("no instance here");
        assert_eq!(failure.code, super::super::Code::NotConnected);
    }

    #[test]
    fn only_the_lease_refresh_command_line_is_taken() {
        assert!(
            !try_run("cloud", &words(&["lease", "refresh"])),
            "needs --json"
        );
        assert!(!try_run("cloud", &words(&["sync", "status", "--json"])));
        assert!(!try_run("tasks", &words(&["lease", "refresh", "--json"])));
    }
}
