// SPDX-License-Identifier: Apache-2.0
//! V7.5: conductor actions only for the workers the server lists; E28/E29
//! bodies as the C0 fixtures; the server's refusals as typed answers.

use std::cell::RefCell;
use std::path::Path;

use serde_json::{Value, json};

use super::*;

const CONDUCTOR: &str = "wkr_cxdouocvevdhm22vpdrh";
const SWARM: &str = "9a4d7e21-6b3c-4f58-8e09-1d2c3b4a5f02";

fn fixture(rel: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2")
        .join(rel);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// Answers every call with `answer`, recording `METHOD path body`.
struct Fake {
    answer: (u16, Value),
    calls: RefCell<Vec<(String, String, Option<Value>)>>,
}

impl Fake {
    fn new(status: u16, body: Value) -> Self {
        Self {
            answer: (status, body),
            calls: RefCell::default(),
        }
    }
}

impl ConvertApi for Fake {
    fn answer(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), HiveError> {
        self.calls
            .borrow_mut()
            .push((method.into(), path.into(), body.cloned()));
        Ok(self.answer.clone())
    }
}

fn conductor<'a>(api: &'a Fake, worker: &'a str, eligible: bool) -> Conductor<'a> {
    Conductor {
        api,
        swarm_id: SWARM,
        worker_id: worker,
        eligible,
    }
}

#[test]
fn the_role_comes_from_the_goal_answer() {
    let goal = fixture("routes/swarm_goal.response.json");
    assert!(eligible(&goal, CONDUCTOR));
    assert!(!eligible(&goal, "wkr_76aa2uwmpczt3cqkhfed"));
    let mut delegated = goal.clone();
    delegated["swarm"]["delegates"] = json!(["wkr_76aa2uwmpczt3cqkhfed"]);
    assert!(eligible(&delegated, "wkr_76aa2uwmpczt3cqkhfed"));
}

#[test]
fn a_worker_without_the_role_gets_no_conductor_action() {
    let api = Fake::new(200, fixture("routes/conductor_lease.response.json"));
    let c = conductor(&api, "wkr_76aa2uwmpczt3cqkhfed", false);
    assert_eq!(c.acquire().unwrap_err().unwrap(), Refusal::NotConductor);
    assert_eq!(
        c.act(3, &Action::Unblock { goal: "g_x".into() })
            .unwrap_err()
            .unwrap(),
        Refusal::NotConductor
    );
    assert!(api.calls.borrow().is_empty(), "nothing reaches the server");
}

#[test]
fn lease_calls_match_the_fixtures() {
    let api = Fake::new(200, fixture("routes/conductor_lease.response.json"));
    let c = conductor(&api, CONDUCTOR, true);
    let lease = c.acquire().unwrap();
    assert_eq!(lease.epoch, 3);
    assert_eq!(lease.holder.worker_id, CONDUCTOR);
    c.renew(3).unwrap();
    let api_release = Fake::new(204, Value::Null);
    conductor(&api_release, CONDUCTOR, true).release(3).unwrap();
    let calls = api.calls.borrow();
    assert_eq!(calls[0].1, format!("/swarms/{SWARM}/conductor/lease"));
    assert_eq!(
        calls[0].2,
        Some(fixture("routes/conductor_lease.request.json"))
    );
    assert_eq!(calls[1].1, format!("/swarms/{SWARM}/conductor/lease/renew"));
    assert_eq!(
        calls[1].2,
        Some(fixture("routes/conductor_lease.renew.request.json"))
    );
    let release = &api_release.calls.borrow()[0];
    assert_eq!(release.0, "DELETE");
    assert_eq!(
        release.1,
        format!("/swarms/{SWARM}/conductor/lease?epoch=3&worker_id={CONDUCTOR}")
    );
}

#[test]
fn every_action_body_matches_its_fixture() {
    let api = Fake::new(200, fixture("routes/conductor_action.response.json"));
    let c = conductor(&api, CONDUCTOR, true);
    let s = |v: &str| v.to_string();
    let children = vec![
        Child {
            title: s("Profile the checkout handler"),
            text: s("Flame graph of POST /checkout under the staging profile."),
            acceptance: vec![s("Flame graph attached")],
            kind: s("task"),
        },
        Child {
            title: s("Enable TLS session resumption"),
            text: s("Turn on resumption in the checkout HTTP client."),
            acceptance: vec![s("p95 drop measured")],
            kind: s("task"),
        },
    ];
    let cases = [
        (
            "split",
            Action::Split {
                parent: s("g_sssssssssssssssssssssssssa"),
                children,
            },
        ),
        (
            "assign",
            Action::Assign {
                goal: s("g_ttttttttttttttttttttttttta"),
                assignee: s("wkr_76aa2uwmpczt3cqkhfed"),
            },
        ),
        (
            "synthesize",
            Action::Synthesize {
                parent: s("g_sssssssssssssssssssssssssa"),
                title: s("Combine the latency fixes"),
            },
        ),
        (
            "block",
            Action::Block {
                goal: s("g_yyyyyyyyyyyyyyyyyyyyyyyyya"),
                reason: s("Waits for the staging load profile."),
            },
        ),
        (
            "unblock",
            Action::Unblock {
                goal: s("g_yyyyyyyyyyyyyyyyyyyyyyyyya"),
            },
        ),
        (
            "drop",
            Action::Drop {
                goal: s("g_yyyyyyyyyyyyyyyyyyyyyyyyya"),
                reason: s("Superseded by the TLS change."),
            },
        ),
        (
            "reopen",
            Action::Reopen {
                goal: s("g_ttttttttttttttttttttttttta"),
                reason: s("The flame graph missed the tax lookup."),
            },
        ),
        (
            "edit",
            Action::Edit {
                goal: s("g_ttttttttttttttttttttttttta"),
                title: None,
                text: None,
                acceptance: Some(vec![
                    s("Flame graph attached"),
                    s("Top three hot spots named"),
                ]),
            },
        ),
    ];
    for (name, action) in &cases {
        let answer = c.act(3, action).unwrap();
        assert_eq!(answer["seqs"], json!([6]));
        let sent = api.calls.borrow().last().unwrap().clone();
        assert_eq!(sent.1, format!("/swarms/{SWARM}/conductor/actions"));
        assert_eq!(
            sent.2.unwrap(),
            fixture(&format!("routes/conductor_action.{name}.request.json")),
            "{name}"
        );
    }
    let empty = Action::Split {
        parent: s("g_sssssssssssssssssssssssssa"),
        children: vec![],
    };
    assert!(c.action_body(3, &empty).is_err());
}

#[test]
fn server_refusals_are_typed() {
    let refusal = |status: u16, body: Value| {
        let api = Fake::new(status, body);
        conductor(&api, CONDUCTOR, true)
            .acquire()
            .unwrap_err()
            .unwrap()
    };
    assert_eq!(
        refusal(
            403,
            fixture("routes/swarm_claim.error.worker_not_owned.json")
        ),
        Refusal::WorkerNotOwned
    );
    assert_eq!(
        refusal(
            403,
            fixture("routes/conductor_lease.error.not_conductor.json")
        ),
        Refusal::NotConductor
    );
    assert_eq!(
        refusal(
            409,
            fixture("routes/conductor_lease.error.conductor_busy.json")
        ),
        Refusal::Busy {
            holder: Some(CONDUCTOR.into())
        }
    );
    assert_eq!(
        refusal(
            409,
            fixture("routes/conductor_lease.error.stale_epoch.json")
        ),
        Refusal::StaleEpoch
    );
    assert_eq!(Refusal::WorkerNotOwned.code(), "worker_not_owned");
}
