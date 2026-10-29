// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::common::V1_SCHEMA_VERSION;
use crate::context_gateway::{
    ContextDecisionV1, DestinationLocalityV1, GatewayModeV1, PrincipalKindV1, SecurityCountsV1,
    SourceCountsV1, SourceKindV1, TokenAccountV1,
};
use crate::identity::SourceId;

fn digest(fill: char) -> Sha256Digest {
    Sha256Digest::new(format!("sha256:{}", fill.to_string().repeat(64))).expect("digest")
}

fn reference(value: &str) -> ProtocolReference {
    ProtocolReference::new(value).expect("reference")
}

fn reason(value: &str) -> ReasonCodeV1 {
    ReasonCodeV1::new(value).expect("reason")
}

fn decision(disposition: ContextDispositionV1, fill: char) -> ContextDecisionV1 {
    ContextDecisionV1 {
        object: digest(fill),
        disposition,
        reason_codes: if disposition == ContextDispositionV1::Allow {
            Vec::new()
        } else {
            vec![reason("secret.detected")]
        },
        signals: Vec::new(),
        required_transformations: Vec::new(),
    }
}

fn receipt(
    decisions: Vec<ContextDecisionV1>,
    outcome: DeliveryOutcomeV1,
) -> ContextDecisionReceiptV1 {
    let count = |d| {
        decisions
            .iter()
            .filter(|x: &&ContextDecisionV1| x.disposition == d)
            .count() as u32
    };
    ContextDecisionReceiptV1 {
        schema_version: V1_SCHEMA_VERSION,
        object_digest: None,
        receipt_id: reference("rcpt-1"),
        mode: GatewayModeV1::Governed,
        principal: PrincipalV1::new(PrincipalKindV1::Person, reference("user-1"))
            .expect("principal"),
        task: None,
        destination: DestinationV1 {
            provider: reference("anthropic"),
            model: None,
            locality: DestinationLocalityV1::Remote,
            organization_managed: true,
            account_ref: None,
            region: None,
        },
        policy: None,
        sources: SourceCountsV1 {
            inspected: 3,
            permitted: 2,
            selected: 2,
            blocked: 1,
        },
        security: SecurityCountsV1 {
            redactions: 0,
            blocked_objects: count(ContextDispositionV1::Deny),
            quarantined_objects: count(ContextDispositionV1::Quarantine),
            injection_signals: 0,
            incomplete_coverage: 0,
        },
        decisions,
        tokens: TokenAccountV1 {
            original: 900,
            delivered: 300,
        },
        final_context: (outcome == DeliveryOutcomeV1::Delivered).then(|| digest('f')),
        outcome,
        duration_us: 10,
        quality: None,
    }
}

fn request(route: IntegrationRouteV1) -> ContextRequestV1 {
    ContextRequestV1 {
        request_id: reference("req-1"),
        principal: PrincipalV1::new(PrincipalKindV1::Person, reference("user-1"))
            .expect("principal"),
        purpose: None,
        source_scope: vec![SourceRefV1 {
            kind: SourceKindV1::Provider,
            id: SourceId::new("gitlab").unwrap(),
        }],
        route,
        destination: receipt(Vec::new(), DeliveryOutcomeV1::Withheld).destination,
    }
}

fn candidate(validity: SourceValidityV1, acl_check: AclCheckV1) -> SourceCandidateV1 {
    SourceCandidateV1 {
        source: SourceRefV1 {
            kind: SourceKindV1::Provider,
            id: SourceId::new("gitlab:issue-1").unwrap(),
        },
        revision: Some("2026-10-14T10:00:00Z".into()),
        acl_check,
        validity,
    }
}

#[test]
fn every_disposition_has_one_doc19_outcome() {
    use ContextDecisionOutcomeV1 as O;
    use ContextDispositionV1 as D;
    for (disposition, outcome) in [
        (D::Allow, O::Allow),
        (D::AllowLocalModelOnly, O::Allow),
        (D::AllowRedacted, O::Redact),
        (D::AllowMinimized, O::Transform),
        (D::AllowSummaryOnly, O::Transform),
        (D::AllowWithApproval, O::Deny),
        (D::Quarantine, O::Deny),
        (D::Deny, O::Deny),
    ] {
        assert_eq!(O::from(disposition), outcome, "{disposition:?}");
        // Delivery agrees with the disposition it came from.
        assert_eq!(
            O::from(disposition).delivers_content(),
            disposition.delivers_content()
        );
    }
}

#[test]
fn only_a_valid_source_is_materialized_and_existing_codes_map_to_one_state() {
    use SourceValidityV1 as V;
    let all = [
        V::Valid,
        V::RevalidateRequired,
        V::Stale,
        V::Revoked,
        V::Deleted,
        V::Unavailable,
        V::Unsupported,
    ];
    for state in all {
        assert_eq!(state.permits_materialization(), state == V::Valid);
        assert_eq!(V::from_reason_code(state.reason_code()), Some(state));
    }
    assert_eq!(V::from_reason_code("store.source_changed"), Some(V::Stale));
    assert_eq!(
        V::from_reason_code("store.source_unavailable"),
        Some(V::Deleted)
    );
    assert_eq!(
        V::from_reason_code("lineage.source_no_longer_admitted"),
        Some(V::Revoked)
    );
    assert_eq!(
        V::from_reason_code("store.no_source_authority"),
        Some(V::Unsupported)
    );
    assert_eq!(
        V::from_reason_code("policy.changed"),
        Some(V::RevalidateRequired)
    );
    assert_eq!(V::from_reason_code("secret.redacted"), None);
}

#[test]
fn a_receipt_state_never_claims_more_than_was_observed() {
    use ContextReceiptStateV1 as S;
    use DeliveryOutcomeV1::{Delivered, Failed, Withheld};
    use IntegrationRouteV1 as R;
    // Context-only routes: an observation count cannot turn them into model egress.
    for route in [
        R::Mcp,
        R::Cli,
        R::Hook,
        R::ToolSession,
        R::EngineInterface,
        R::Sdk,
    ] {
        assert_eq!(
            S::derive(Delivered, route, true, 3, false),
            S::HandedOff,
            "{route:?}"
        );
        assert_eq!(
            S::derive(Delivered, route, false, 3, false),
            S::Prepared,
            "{route:?}"
        );
    }
    assert_eq!(
        S::derive(Delivered, R::Proxy, true, 1, false),
        S::ObservedModelEgress
    );
    assert_eq!(S::derive(Delivered, R::Proxy, true, 0, false), S::HandedOff);
    assert_eq!(
        S::derive(Delivered, R::HostEgress, false, 0, false),
        S::Prepared
    );
    assert_eq!(S::derive(Delivered, R::Proxy, true, 1, true), S::Partial);
    assert_eq!(S::derive(Withheld, R::Proxy, true, 1, false), S::Rejected);
    assert_eq!(S::derive(Failed, R::Mcp, false, 0, false), S::Rejected);
}

#[test]
fn a_lineage_projects_its_receipt_and_refuses_impossible_claims() {
    let delivered = receipt(
        vec![
            decision(ContextDispositionV1::Allow, 'a'),
            decision(ContextDispositionV1::Deny, 'c'),
        ],
        DeliveryOutcomeV1::Delivered,
    );
    let lineage = ContextDecisionLineageV1::for_receipt(
        request(IntegrationRouteV1::Mcp),
        vec![candidate(SourceValidityV1::Valid, AclCheckV1::Rechecked)],
        &delivered,
        true,
        None,
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        lineage.decision_receipt,
        delivered.canonical_digest().unwrap()
    );
    assert_eq!(lineage.receipt_state, ContextReceiptStateV1::Partial);
    assert_eq!(
        lineage
            .outcomes
            .iter()
            .map(|o| o.outcome)
            .collect::<Vec<_>>(),
        [
            ContextDecisionOutcomeV1::Allow,
            ContextDecisionOutcomeV1::Deny
        ]
    );

    let reject = |mutate: fn(&mut ContextDecisionLineageV1)| {
        let mut bad = lineage.clone();
        mutate(&mut bad);
        bad.validate().unwrap_err()
    };
    // Model egress on a context-only route, or without an observation.
    reject(|l| l.receipt_state = ContextReceiptStateV1::ObservedModelEgress);
    reject(|l| {
        l.request.route = IntegrationRouteV1::Proxy;
        l.receipt_state = ContextReceiptStateV1::ObservedModelEgress;
    });
    // A source nobody can re-check is never valid for governed use.
    reject(|l| l.candidates = vec![candidate(SourceValidityV1::Valid, AclCheckV1::Unsupported)]);
    // A non-allow outcome without a reason; an observation without its admission.
    reject(|l| l.outcomes[1].reason_codes.clear());
    reject(|l| l.egress_observations = vec![digest('e')]);
    reject(|l| l.schema_version = 2);

    // A stale candidate makes the delivery partial, never a clean hand-off.
    let stale = ContextDecisionLineageV1::for_receipt(
        request(IntegrationRouteV1::Cli),
        vec![candidate(SourceValidityV1::Stale, AclCheckV1::Rechecked)],
        &receipt(
            vec![decision(ContextDispositionV1::Allow, 'a')],
            DeliveryOutcomeV1::Delivered,
        ),
        true,
        None,
        Vec::new(),
    )
    .unwrap();
    assert_eq!(stale.receipt_state, ContextReceiptStateV1::Partial);
}

#[test]
fn the_wire_form_is_snake_case_and_closed() {
    let lineage = ContextDecisionLineageV1::for_receipt(
        request(IntegrationRouteV1::HostEgress),
        Vec::new(),
        &receipt(
            vec![decision(ContextDispositionV1::AllowRedacted, 'b')],
            DeliveryOutcomeV1::Delivered,
        ),
        false,
        Some(digest('d')),
        vec![digest('e')],
    )
    .unwrap();
    assert_eq!(
        lineage.receipt_state,
        ContextReceiptStateV1::ObservedModelEgress
    );
    let mut value = serde_json::to_value(&lineage).unwrap();
    assert_eq!(value["request"]["route"], "host_egress");
    assert_eq!(value["receipt_state"], "observed_model_egress");
    assert_eq!(value["outcomes"][0]["outcome"], "redact");
    assert_eq!(
        serde_json::from_value::<ContextDecisionLineageV1>(value.clone()).unwrap(),
        lineage
    );
    value["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ContextDecisionLineageV1>(value).is_err());
}
