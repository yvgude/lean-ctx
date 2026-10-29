// SPDX-License-Identifier: Apache-2.0
//! `GET /api/gateway/receipts` — the newest Context Gateway receipts with
//! what each one proves (G08).
//!
//! A receipt says PREPARED: it is stored, with its trace sidecar and a signed
//! audit anchor, before a byte leaves. What was observed afterwards is a
//! separate, local, unsigned record. The route shows both, in the words of
//! `lean-ctx inspect` (`receipt_store::view`, the one reading both share).
//!
//! Content: digests, closed state words, counts, and the references a trace
//! sidecar holds — which passed the gateway's release check when the sidecar
//! was built. No source text leaves here, and no path or id beyond those
//! released sidecar references. It is a subset of `lean-ctx inspect --json`.
//!
//! Which receipts: every writer — MCP session, CLI run, tool session, Engine
//! operation, host egress — indexes a receipt under the ONE canonical key of
//! the project root it worked on (`receipt_store::roots`); a session rooted
//! in a sub-directory keeps its own key. This route therefore lists by
//! repository, with the same resolver as `lean-ctx inspect`
//! (`roots::of_repository`): the dashboard's project root, the repository
//! root above it, and the other roots of that repository receipts were
//! indexed under (bounded). Then, as the fallback for indexes written before
//! the one key existed, the latest session's project root exactly as that
//! session stored it; then the index of Engine operations that work on no
//! project, and the proxy's. A receipt is listed once and says which index
//! listed it.

use crate::core::context_admission::egress::PROXY_RECEIPT_KEY;
use crate::core::context_admission::receipt_store::{self, AuditAnchors, roots, view};
use crate::core::session::SessionState;

use super::helpers::detect_project_root_for_dashboard;

/// Receipts shown per index, newest first.
const RECEIPTS_PER_INDEX: usize = 25;

/// The receipt indexes this dashboard reads: a label and the root (or the
/// reserved index name) each is read by.
pub(super) fn indexes() -> Vec<(&'static str, String)> {
    let session = SessionState::load_latest()
        .and_then(|session| session.project_root.clone())
        .or_else(|| {
            SessionState::load_global_latest_pointer()
                .and_then(|session| session.project_root.clone())
        });
    indexes_of(&detect_project_root_for_dashboard(), session)
}

/// [`indexes`] for the dashboard root `project` and the latest session's
/// stored root: `project`, `repository` (the other roots of the project's
/// repository), `session` (legacy spelling), `engine`, `proxy`.
fn indexes_of(project: &str, session: Option<String>) -> Vec<(&'static str, String)> {
    let mut indexes: Vec<(&'static str, String)> = roots::of_repository(project)
        .into_iter()
        .enumerate()
        .map(|(position, root)| {
            (
                if position == 0 {
                    "project"
                } else {
                    "repository"
                },
                root,
            )
        })
        .collect();
    if let Some(session) = session
        .filter(|root| !root.trim().is_empty() && !indexes.iter().any(|(_, listed)| listed == root))
    {
        indexes.push(("session", session));
    }
    indexes.push(("engine", roots::ENGINE_KEY.to_owned()));
    indexes.push(("proxy", PROXY_RECEIPT_KEY.to_owned()));
    indexes
}

fn receipts_document(indexes: &[(&'static str, String)]) -> serde_json::Value {
    // The audit trail is read and its chain verified once for all receipts.
    let anchors = AuditAnchors::load();
    let mut seen = std::collections::HashSet::new();
    let mut receipts = Vec::new();
    for (index, key) in indexes {
        for (hex, loaded) in receipt_store::latest(key, RECEIPTS_PER_INDEX) {
            // One receipt listed under two spellings of a root is one receipt.
            if !seen.insert(hex.clone()) {
                continue;
            }
            let mut entry = view::summary_json(&hex, &loaded, &anchors);
            entry["index"] = serde_json::Value::from(*index);
            receipts.push(entry);
        }
    }
    serde_json::json!({
        "receipts": receipts,
        "indexes": indexes.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
        "per_index": RECEIPTS_PER_INDEX,
        "audit_trail": {
            "present": anchors.trail_present,
        "unavailable": anchors.trail_unavailable,
            "chain_verified": anchors.chain_valid,
        },
    })
}

pub(super) fn handle(
    path: &str,
    _query: &str,
    method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/gateway/receipts" if method.eq_ignore_ascii_case("GET") => {
            let body = receipts_document(&indexes()).to_string();
            Some(("200 OK", "application/json", body))
        }
        "/api/gateway/receipts" => Some((
            "405 Method Not Allowed",
            "application/json",
            r#"{"error":"method not allowed"}"#.to_string(),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One governed MCP delivery of `text` by a session rooted at `root`;
    /// the digest of its receipt.
    fn deliver(root: &str, text: &str) -> String {
        use crate::core::context_admission::capture::{ADMISSIONS, AdmissionCapture, Delivered};
        use crate::core::context_admission::receipt_store::{
            Duty, ReceiptRecord, ReceiptSubject, trace,
        };
        let duty = Duty::at_entry();
        let capture = AdmissionCapture::for_call(duty.snapshot().clone());
        ADMISSIONS
            .sync_scope(Some(capture.clone()), || {
                crate::core::context_admission::admit_context_source(text)
            })
            .expect("a clean source is admitted");
        let delivery = trace::Delivery::mcp("ctx_read", duty.snapshot());
        let record = receipt_store::record_delivery(
            &capture,
            &ReceiptSubject {
                tool: "ctx_read",
                agent_id: None,
                presenter_ref: None,
                project_root: root,
                delivery: &delivery,
            },
            &Delivered {
                text,
                is_error: false,
                tokens: 6,
            },
            &duty,
        );
        let ReceiptRecord::Stored { digest, .. } = record else {
            panic!("a governed delivery is receipted");
        };
        digest.as_str().to_owned()
    }

    /// ONE key per project root, read by repository: the dashboard and
    /// `lean-ctx inspect` (`roots::latest_of_repository`) list a
    /// sub-directory session's receipts for the repository root and the
    /// root's for the sub-directory, not those of a nested repository; an
    /// index filed under a writer's own spelling is still read through that
    /// spelling.
    #[test]
    fn a_repository_lists_the_receipts_of_its_roots_and_a_legacy_index() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let config = crate::core::paths::config_dir_read_only().expect("config dir");
        std::fs::create_dir_all(&config).expect("config dir");
        std::fs::write(
            config.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\n",
        )
        .expect("config");
        let repository = tempfile::tempdir().expect("repository");
        let root = repository.path().canonicalize().expect("canonical root");
        let (sub, nested) = (root.join("crates/app"), root.join("vendor/other"));
        for directory in [root.join(".git"), sub.clone(), nested.join(".git")] {
            std::fs::create_dir_all(directory).expect("directory");
        }
        // The roots as the store keys them, whatever the platform's spelling.
        let text = |path: &std::path::Path| roots::canonical(&path.to_string_lossy());
        let (root, sub, nested) = (text(&root), text(&sub), text(&nested));

        // The session spells its root with a trailing separator: one key.
        let under_sub = deliver(&format!("{sub}/"), "pub fn sub_body() {}\n");
        let under_root = deliver(&root, "pub fn root_body() {}\n");
        let under_nested = deliver(&nested, "pub fn nested_body() {}\n");

        let dashboard = |project: &str, session: Option<String>| -> Vec<(String, String)> {
            let document = receipts_document(&indexes_of(project, session));
            let word = |value: &serde_json::Value| value.as_str().unwrap_or_default().to_owned();
            document["receipts"]
                .as_array()
                .expect("receipts")
                .iter()
                .map(|receipt| (word(&receipt["digest"]), word(&receipt["index"])))
                .collect()
        };
        let inspect = |project: &str| -> Vec<String> {
            roots::latest_of_repository(project, 8)
                .into_iter()
                .map(|(hex, _)| format!("sha256:{hex}"))
                .collect()
        };
        let listed = |own: &str, other: &str| {
            vec![
                (own.to_owned(), "project".to_owned()),
                (other.to_owned(), "repository".to_owned()),
            ]
        };
        assert_eq!(dashboard(&root, None), listed(&under_root, &under_sub));
        assert_eq!(dashboard(&sub, None), listed(&under_sub, &under_root));
        assert_eq!(inspect(&root), [under_root.clone(), under_sub.clone()]);
        assert_eq!(inspect(&sub), [under_sub.clone(), under_root.clone()]);
        // A nested repository is another repository: listed for itself only.
        assert_eq!(inspect(&nested), [under_nested]);

        // An index from before the one key is filed under its writer's own
        // spelling: the canonical lookup misses it, that spelling finds it.
        let latest = crate::core::data_dir::lean_ctx_data_dir()
            .expect("data dir")
            .join("gateway/latest");
        let index = |spelling: &str| {
            let key = blake3::hash(spelling.as_bytes()).to_hex();
            latest.join(format!("{}.json", &key[..16]))
        };
        let legacy = format!("{sub}/");
        std::fs::rename(index(&sub), index(&legacy)).expect("legacy index");
        let own = vec![(under_root.clone(), "project".to_owned())];
        assert_eq!(dashboard(&root, None), own);
        let with_session = dashboard(&root, Some(legacy));
        assert_eq!(
            with_session,
            [own[0].clone(), (under_sub.clone(), "session".to_owned())]
        );
        // That read remembered the spelling: the repository listing now finds
        // the older index itself, without the session fallback or a new write.
        assert_eq!(dashboard(&root, None), listed(&under_root, &under_sub));
    }

    /// Doc 19, one decision contract across the call routes: the same source
    /// delivered through MCP, CLI, a tool session and the Engine interface is
    /// decided alike, each lineage names its own route, and none of them —
    /// paths that hand context to a host — claims an observed model egress.
    #[test]
    fn every_call_route_yields_the_same_decision_in_one_lineage_vocabulary() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let config = crate::core::paths::config_dir_read_only().expect("config dir");
        std::fs::create_dir_all(&config).expect("config dir");
        // H01: a clean source and a withheld one (a classification banner
        // under `classification = "block"`) go through every call route and
        // the proxy alike.
        std::fs::write(
            config.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\nclassification = \"block\"\n",
        )
        .expect("config");
        for (text, withheld) in [
            ("pub fn parity_body() {}\n", false),
            ("CONFIDENTIAL\nroadmap for the board\n", true),
        ] {
            call_and_proxy_routes_decide_alike(text, withheld);
        }
    }

    fn call_and_proxy_routes_decide_alike(text: &str, withheld: bool) {
        use crate::core::context_admission::capture::{
            ADMISSIONS, AdmissionCapture, CallSnapshot, Delivered,
        };
        use crate::core::context_admission::egress;
        use crate::core::context_admission::receipt_store::{
            Duty, ReceiptRecord, ReceiptSubject, trace,
        };
        use lean_ctx_protocol::context_lineage::{
            ContextDecisionOutcomeV1, ContextReceiptStateV1, IntegrationRouteV1,
        };

        let routes: [(IntegrationRouteV1, fn(&CallSnapshot) -> trace::Delivery); 4] = [
            (IntegrationRouteV1::Mcp, |s| {
                trace::Delivery::mcp("ctx_read", s)
            }),
            (IntegrationRouteV1::Cli, |s| {
                trace::Delivery::cli("ctx_read", trace::Channel::Stdout, s)
            }),
            (IntegrationRouteV1::ToolSession, |s| {
                trace::Delivery::tool_session("ctx_read", s)
            }),
            (IntegrationRouteV1::EngineInterface, |s| {
                trace::Delivery::engine_interface("context.read", s)
            }),
        ];
        // Each route's decision: the distinct (outcome, reason codes) pairs —
        // the proxy decides per request part, a call route per source.
        let decided = |lineage: &lean_ctx_protocol::context_lineage::ContextDecisionLineageV1| {
            let mut pairs: Vec<_> = lineage
                .outcomes
                .iter()
                .map(|outcome| {
                    let codes: Vec<String> = outcome
                        .reason_codes
                        .iter()
                        .map(|code| code.as_str().to_owned())
                        .collect();
                    (outcome.outcome, codes)
                })
                .collect();
            pairs.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
            pairs.dedup();
            pairs
        };
        let mut outcomes = Vec::new();
        let mut states = Vec::new();
        for (route, delivery) in routes {
            let duty = Duty::at_entry();
            let capture = AdmissionCapture::for_call(duty.snapshot().clone());
            let admitted = ADMISSIONS.sync_scope(Some(capture.clone()), || {
                crate::core::context_admission::admit_context_source(text)
            });
            assert_eq!(admitted.is_err(), withheld, "{route:?}: {admitted:?}");
            // A withheld source hands over the refusal, never the text.
            let handed_over = if withheld {
                "the source was withheld"
            } else {
                text
            };
            let record = receipt_store::record_delivery(
                &capture,
                &ReceiptSubject {
                    tool: "ctx_read",
                    agent_id: None,
                    presenter_ref: None,
                    project_root: "/work/parity",
                    delivery: &delivery(duty.snapshot()),
                },
                &Delivered {
                    text: handed_over,
                    is_error: withheld,
                    tokens: 6,
                },
                &duty,
            );
            let ReceiptRecord::Stored { digest, .. } = record else {
                panic!("a governed delivery on {route:?} is receipted");
            };
            let stored = receipt_store::load(digest.hex()).expect("stored receipt");
            let lineage = view::lineage(&stored).expect("lineage of a call route");
            assert_eq!(lineage.request.route, route);
            assert!(
                !route.observes_model_egress(),
                "{route:?} hands context to a host"
            );
            assert_eq!(view::source_rights(Some(&lineage)), "not_recorded");
            states.push((route, lineage.receipt_state));
            outcomes.push((route, decided(&lineage)));
        }

        // The proxy decides the same text as one message of a request.
        let doc = serde_json::json!({"model": "claude", "messages": [
            {"role": "user", "content": text}
        ]});
        let wire = doc.to_string();
        let target = egress::EgressTarget {
            provider: "Anthropic",
            model: Some("claude-sonnet-5-5"),
            upstream_base: "https://api.anthropic.com",
        };
        let outcome = egress::admit_request(&doc, &target);
        let forwarded = match &outcome.body {
            egress::EgressBody::Unchanged => Some(wire.clone()),
            egress::EgressBody::Rewritten(admitted) => Some(admitted.to_string()),
            egress::EgressBody::Refused(_) => None,
        };
        if withheld {
            assert!(
                forwarded
                    .as_deref()
                    .is_none_or(|bytes| !bytes.contains("roadmap for the board")),
                "the proxy forwarded what the call routes withhold: {:?}",
                outcome.body
            );
        } else {
            assert!(matches!(outcome.body, egress::EgressBody::Unchanged));
        }
        let evidence = egress::finish_admission_for(
            &outcome,
            forwarded.as_deref().map(str::as_bytes),
            wire.len(),
            None,
            egress::EgressCaller::Proxy,
        )
        .unwrap_or_else(|error| panic!("{error}: {:?}", outcome.body))
        .expect("governed admission evidence");
        let decision = evidence
            .admission
            .as_ref()
            .and_then(|admission| admission.decision_receipt_digest.clone())
            .expect("decision receipt digest");
        let stored = receipt_store::load(decision.hex()).expect("stored decision receipt");
        let lineage = view::lineage(&stored).expect("lineage of the proxy route");
        assert_eq!(lineage.request.route, IntegrationRouteV1::Proxy);
        states.push((IntegrationRouteV1::Proxy, lineage.receipt_state));
        outcomes.push((IntegrationRouteV1::Proxy, decided(&lineage)));

        let expected = if withheld {
            ContextDecisionOutcomeV1::Deny
        } else {
            ContextDecisionOutcomeV1::Allow
        };
        for (route, decided) in &outcomes {
            assert!(
                !decided.is_empty() && decided.iter().all(|(outcome, _)| *outcome == expected),
                "{route:?}: expected {expected:?} (withheld={withheld}): {outcomes:?}"
            );
            assert_eq!(
                decided, &outcomes[0].1,
                "{route:?} decided the same source differently: {outcomes:?}"
            );
        }
        // The handover boundary: nothing is observed at admission on any
        // route; a withheld source is withheld everywhere, a clean one is
        // only prepared.
        for (route, state) in &states {
            assert_ne!(
                *state,
                ContextReceiptStateV1::ObservedModelEgress,
                "{route:?}"
            );
            assert_eq!(state, &states[0].1, "{route:?}: {states:?}");
        }
    }

    /// The route reads the index a writer keyed by ITS root string, lists a
    /// receipt once, and shows it as prepared and unconfirmed — never as a
    /// delivery — with nothing of the delivered text.
    #[test]
    fn a_receipt_is_listed_under_its_writers_key_as_prepared_not_delivered() {
        use crate::core::context_admission::capture::{ADMISSIONS, AdmissionCapture, Delivered};
        use crate::core::context_admission::receipt_store::{Duty, ReceiptSubject, trace};

        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let config = crate::core::paths::config_dir_read_only().expect("config dir");
        std::fs::create_dir_all(&config).expect("config dir");
        std::fs::write(
            config.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\n",
        )
        .expect("config");
        let duty = Duty::at_entry();
        let capture = AdmissionCapture::for_call(duty.snapshot().clone());
        let delivered = "pub fn dashboard_body() {}\n";
        ADMISSIONS
            .sync_scope(Some(capture.clone()), || {
                crate::core::context_admission::admit_context_source(delivered)
            })
            .expect("a clean source is admitted");
        let delivery = trace::Delivery::mcp("ctx_read", duty.snapshot());
        receipt_store::record_delivery(
            &capture,
            &ReceiptSubject {
                tool: "ctx_read",
                agent_id: None,
                presenter_ref: None,
                project_root: "/work/repo/sub",
                delivery: &delivery,
            },
            &Delivered {
                text: delivered,
                is_error: false,
                tokens: 6,
            },
            &duty,
        );

        // The dashboard's root is another string than the writer's key: the
        // project index alone finds nothing, the session's stored root does.
        let project_only = receipts_document(&[("project", "/work/repo".to_owned())]);
        assert_eq!(project_only["receipts"].as_array().map(Vec::len), Some(0));
        let document = receipts_document(&[
            ("project", "/work/repo".to_owned()),
            ("session", "/work/repo/sub".to_owned()),
            ("session", "/work/repo/sub".to_owned()),
        ]);
        let receipts = document["receipts"].as_array().expect("receipts");
        assert_eq!(receipts.len(), 1, "listed once: {document}");
        let receipt = &receipts[0];
        assert_eq!(receipt["index"], "session");
        assert_eq!(receipt["prepared"]["outcome_recorded_at"], "preparation");
        assert_eq!(receipt["observed"]["state"], "unconfirmed");
        assert_eq!(receipt["observed"]["observation_signed"], false);
        // No source candidate was recorded: the right to read is an open
        // question, never an implied permission (E07).
        assert_eq!(receipt["source_rights"], "not_recorded");
        assert!(
            !document.to_string().contains("dashboard_body"),
            "{document}"
        );
    }
}
