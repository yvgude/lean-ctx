// SPDX-License-Identifier: Apache-2.0
//! The ONE reading of a stored receipt that every view shows (G08):
//! `lean-ctx inspect` and the dashboard take their words from here, so a
//! receipt never reads "delivered" in one place and "prepared" in another.
//!
//! Everything returned is content-free: digests, closed state words, counts,
//! validated reason codes, and the words a trace sidecar holds — which
//! passed the gateway's release check when the sidecar was built. No source
//! text, and no path or id beyond those released sidecar references.

use lean_ctx_protocol::context_gateway::{
    ContextDecisionReceiptV1, DeliveryOutcomeV1, EgressTransportObservationV1, GatewayModeV1,
};
use lean_ctx_protocol::context_lineage::{
    AclCheckV1, ContextDecisionLineageV1, ContextRequestV1, IntegrationRouteV1,
};

use super::observed::{self, Observed, StoredObservation};
use super::trace::{self, SourceRef, StoredTrace, TraceSidecarV1};
use super::{AuditAnchors, LoadError, StoredReceipt};

fn serde_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// A receipt of an egress path — the proxy's, or a host's that dispatches
/// the request itself (sidecar transport `host_egress`): what was observed
/// of it is the egress lifecycle (the upstream request, observed by the
/// proxy or reported by that host), not a hand-over record.
pub(crate) fn is_proxy_egress(receipt: &ContextDecisionReceiptV1) -> bool {
    receipt.destination.provider.as_str() != "mcp-host"
}

/// What was OBSERVED of a delivery after its receipt was prepared: a state
/// word and a sentence. A V1 outcome is recorded at preparation and is never
/// shown as a proven delivery; this is the separate, observed state. A
/// receipt whose preparation failed is named as that, never as a delivery.
pub(crate) fn observed_finding(stored: &StoredReceipt) -> (&'static str, String) {
    let found = observed::load(&stored.digest);
    let proxy = is_proxy_egress(&stored.receipt);
    let state = match &found {
        StoredObservation::Absent if proxy => "egress_lifecycle",
        other => other.state(),
    };
    let observation = match found {
        StoredObservation::Bound { observed, .. } => Ok(observed),
        other => Err(other),
    };
    let sentence = match observation {
        Ok(Observed::PreparationFailed { step }) => format!(
            "prepared, NOT delivered: preparation failed (step: {}); the delivery was withheld",
            serde_name(&step)
        ),
        Ok(Observed::HandedOver {
            channel,
            bytes,
            exit_status,
        }) => {
            let status = exit_status
                .map(|status| format!(" · process ended with exit status {status}"))
                .unwrap_or_default();
            // A stream write is observed as write + flush; a file or the
            // clipboard as the success of the command's own hand-off.
            let how = match channel {
                trace::Channel::File | trace::Channel::Clipboard => "handed to",
                _ => "written and flushed to",
            };
            format!(
                "handed over (observed): {} {how} {}{status}; display or reading by a \
                 consumer is not observed",
                bytes.as_str(),
                serde_name(&channel),
            )
        }
        Ok(Observed::ReturnedToTransport { bytes }) => format!(
            "returned to transport: {} handed to the host's transport (the MCP transport \
             layer, or the tool session's stream); receipt by the host is not observable",
            bytes.as_str()
        ),
        Err(StoredObservation::Unbound) => {
            "MISMATCH — the observation filed under this receipt names another receipt".to_owned()
        }
        Err(StoredObservation::Unreadable) => {
            "UNREADABLE — the record is not an observation of a known schema".to_owned()
        }
        Err(_) if proxy => "what was observed of the upstream request — by the proxy, or reported \
                            by the host that dispatched it — is listed per attempt below"
            .to_owned(),
        Err(_) => "unconfirmed — no hand-over was observed (the writer does not report it, the \
                   write failed, or the observation could not be stored)"
            .to_owned(),
    };
    (state, sentence)
}

/// What kind of evidence the observation is — a state word and a sentence.
/// The SIGNED statement about a receipt is its audit anchor, and it says
/// PREPARED. The observation is a LOCAL, UNSIGNED record: it names the
/// signed audit entry of its receipt, and that link is checked here, but no
/// signature covers the observation itself.
pub(crate) fn observation_evidence(
    stored: &StoredReceipt,
    anchors: Option<&AuditAnchors>,
) -> (&'static str, &'static str) {
    let StoredObservation::Bound { anchor_entry, .. } = observed::load(&stored.digest) else {
        return ("none", "no observation record");
    };
    let Some(named) = anchor_entry else {
        return (
            "local_unsigned_unlinked",
            "local unsigned record; it names no audit entry",
        );
    };
    // An unreadable or contended trail checked nothing: that is "not
    // checked", never a missing or mismatching link.
    let Some(anchors) = anchors.filter(|anchors| !anchors.trail_unavailable) else {
        return (
            "local_unsigned_link_not_checked",
            "local unsigned record; its link to the receipt's audit entry was not checked",
        );
    };
    match anchors.of(stored.digest.hex()) {
        Some(anchor) if anchor.entry_hash == named => (
            "local_unsigned_link_verified",
            "local unsigned record; it names the signed audit entry that anchors this \
             receipt (link verified) — that signature covers receipt and sidecar, not the \
             observation",
        ),
        Some(_) => (
            "LINK_MISMATCH",
            "LINK MISMATCH — local unsigned record that names another audit entry than the \
             one anchoring this receipt",
        ),
        None => (
            "LINK_UNANCHORED",
            "local unsigned record; the audit entry it names is not found for this receipt",
        ),
    }
}

/// The observed state as a document: the state word, its sentence, that the
/// V1 outcome next to it was recorded at preparation, and — per receipt —
/// what is signed (receipt and sidecar, prepared) and what is not (the
/// observation).
pub(crate) fn observed_json(stored: &StoredReceipt, anchors: &AuditAnchors) -> serde_json::Value {
    let (state, detail) = observed_finding(stored);
    let (evidence, evidence_detail) = observation_evidence(stored, Some(anchors));
    serde_json::json!({
        "state": state,
        "detail": detail,
        "v1_outcome_recorded_at": "preparation",
        "signed_statement": "receipt_and_sidecar_prepared",
        "observation_signed": false,
        "observation_evidence": evidence,
        "observation_evidence_detail": evidence_detail,
    })
}

/// The trace sidecar of a receipt as a view finds it: a state word, the
/// digest of its stored bytes and — when it names this receipt — the
/// record. Never silent: a sidecar that is owed and missing, names another
/// receipt, or no longer matches the digest its audit anchor carries is a
/// finding of its own.
pub(crate) fn trace_finding(
    stored: &StoredReceipt,
    anchors: Option<&AuditAnchors>,
) -> (&'static str, Option<String>, Option<Box<TraceSidecarV1>>) {
    let anchored = anchors
        .and_then(|anchors| anchors.of(stored.digest.hex()))
        .and_then(|anchor| anchor.trace.as_deref());
    match trace::load(&stored.digest) {
        StoredTrace::Bound { digest, sidecar } => {
            let state = match anchored {
                Some(expected) if expected == digest => "verified",
                Some(_) => "ALTERED",
                None => "present_not_anchored",
            };
            (state, Some(digest), Some(sidecar))
        }
        StoredTrace::Unbound => ("MISMATCH", None, None),
        StoredTrace::Unreadable => ("UNREADABLE", None, None),
        StoredTrace::Absent
            if anchored.is_some() || stored.receipt.mode != GatewayModeV1::Developer =>
        {
            ("MISSING", None, None)
        }
        StoredTrace::Absent => ("not_owed", None, None),
    }
}

/// The doc-19 lineage of a stored receipt: the request it answered, its
/// decisions in the five-outcome vocabulary, and the strongest delivery state
/// the stored evidence supports (`prepared`, `handed_off`, `partial`,
/// `rejected`; `observed_model_egress` only for an egress route with a
/// recorded transport observation). Derived from the records the other views
/// read, never stored on its own. `None` when the receipt's route is unknown
/// (no sidecar and not an egress receipt) or its evidence is inconsistent.
pub(crate) fn lineage(stored: &StoredReceipt) -> Option<ContextDecisionLineageV1> {
    let receipt = &stored.receipt;
    let route = match trace::load(&stored.digest) {
        StoredTrace::Bound { sidecar, .. } => match sidecar.transport {
            trace::Transport::Mcp => IntegrationRouteV1::Mcp,
            trace::Transport::Cli => IntegrationRouteV1::Cli,
            trace::Transport::Proxy => IntegrationRouteV1::Proxy,
            trace::Transport::ToolSession => IntegrationRouteV1::ToolSession,
            trace::Transport::EngineInterface => IntegrationRouteV1::EngineInterface,
            trace::Transport::HostEgress => IntegrationRouteV1::HostEgress,
        },
        _ if is_proxy_egress(receipt) => IntegrationRouteV1::Proxy,
        _ => return None,
    };
    let handed_off = matches!(
        observed::load(&stored.digest),
        StoredObservation::Bound {
            observed: Observed::HandedOver { .. } | Observed::ReturnedToTransport { .. },
            ..
        }
    );
    let (admission, observations) = match super::load_egress_for_decision(&stored.digest) {
        Ok(Some(view)) => (
            view.admission.map(|admission| admission.digest),
            view.observations
                .into_iter()
                .map(|attempt| attempt.digest)
                .collect(),
        ),
        _ => (None, Vec::new()),
    };
    let request = ContextRequestV1 {
        request_id: receipt.receipt_id.clone(),
        principal: receipt.principal.clone(),
        purpose: receipt.task.clone(),
        source_scope: Vec::new(),
        route,
        destination: receipt.destination.clone(),
    };
    ContextDecisionLineageV1::for_receipt(
        request,
        Vec::new(),
        receipt,
        handed_off,
        admission,
        observations,
    )
    .ok()
}

/// What a receipt can say about the right to read its sources (E07):
/// `checked` only when every candidate names a re-check; a candidate without
/// one is `unsupported`; no recorded candidate is `not_recorded` — an open
/// question for the verifier, never an implied permission.
pub(crate) fn source_rights(lineage: Option<&ContextDecisionLineageV1>) -> &'static str {
    let candidates = lineage.map_or(&[][..], |lineage| lineage.candidates.as_slice());
    if candidates.is_empty() {
        "not_recorded"
    } else if candidates
        .iter()
        .any(|candidate| candidate.acl_check == AclCheckV1::Unsupported)
    {
        "unsupported"
    } else {
        "checked"
    }
}

/// One attempt of the proxy's upstream request, as observed. What the proxy
/// observes: the request was sent upstream and a response status came back.
/// Never a delivery to a model.
pub(crate) fn transport_observation_line(observation: &EgressTransportObservationV1) -> String {
    let detail = observation.http_status.map_or_else(
        || {
            observation
                .failure
                .as_ref()
                .map(serde_name)
                .unwrap_or_default()
        },
        |status| {
            format!(
                "sent_upstream · HTTP {status} (status class {}xx)",
                status / 100
            )
        },
    );
    format!(
        "attempt {}  {} · {} · {detail} (model consumption unknown)",
        observation.attempt,
        serde_name(&observation.observer),
        serde_name(&observation.result),
    )
}

/// The egress lifecycle of a proxy receipt, one line per finding or attempt,
/// in the words every view shows. Empty for a receipt that is not a proxy
/// egress receipt.
pub(crate) fn egress_lines(stored: &StoredReceipt) -> Vec<String> {
    if !is_proxy_egress(&stored.receipt) {
        return Vec::new();
    }
    // The lifecycle is keyed by the receipt's full digest.
    match super::load_egress_for_decision(&stored.digest) {
        Ok(Some(view)) => {
            let mut lines = Vec::new();
            if view.admission.is_none() {
                lines.push("egress     admission evidence unavailable".to_owned());
            }
            if view.observations.is_empty() {
                lines.push("transport  no observation recorded; dispatch unknown".to_owned());
            }
            lines.extend(
                view.observations
                    .iter()
                    .map(|attempt| transport_observation_line(&attempt.observation)),
            );
            lines
        }
        Ok(None) => vec!["transport  no egress lifecycle recorded".to_owned()],
        Err(error) => vec![format!("transport  evidence unavailable ({error})")],
    }
}

/// One receipt as a compact, content-free summary for a list view (the
/// dashboard): the prepared facts of the V1 receipt, the observed state and
/// what kind of evidence it is, the sidecar's state and released references,
/// and the egress lifecycle of a proxy receipt. A subset of what `lean-ctx
/// inspect --json` shows for the same receipt, in the same words.
pub(crate) fn summary_json(
    hex: &str,
    loaded: &Result<StoredReceipt, LoadError>,
    anchors: &AuditAnchors,
) -> serde_json::Value {
    let Ok(stored) = loaded else {
        return serde_json::json!({
            "digest": format!("sha256:{hex}"),
            "verified": false,
        });
    };
    let receipt = &stored.receipt;
    let proxy = is_proxy_egress(receipt);
    // A proxy receipt's V1 `delivered` predates dispatch: shown as prepared.
    let outcome = if proxy && receipt.outcome == DeliveryOutcomeV1::Delivered {
        "prepared".to_owned()
    } else {
        serde_name(&receipt.outcome)
    };
    let (trace_state, _, sidecar) = trace_finding(stored, Some(anchors));
    let lineage = lineage(stored);
    let sources: Vec<serde_json::Value> = sidecar
        .iter()
        .flat_map(|sidecar| sidecar.objects.iter())
        .filter_map(|object| match &object.source {
            Some(SourceRef::Path(name) | SourceRef::Id(name)) => Some(name.as_str()),
            Some(SourceRef::Withheld) => Some("withheld"),
            None => None,
        })
        .map(serde_json::Value::from)
        .collect();
    serde_json::json!({
        "digest": stored.digest.as_str(),
        "verified": true,
        "consistent": receipt.validate().is_ok(),
        "prepared": {
            "outcome": outcome,
            "outcome_recorded_at": "preparation",
            "mode": receipt.mode,
            "destination": receipt.destination.provider.as_str(),
            "output": receipt.final_context,
            "sources": receipt.sources,
            "security": receipt.security,
            "anchored": anchors.of(stored.digest.hex()).is_some(),
        },
        "observed": observed_json(stored, anchors),
        "trace": {
            "state": trace_state,
            "transport": sidecar.as_ref().map(|sidecar| sidecar.transport),
            "endpoint": sidecar.as_ref().map(|sidecar| sidecar.endpoint.as_str()),
            "sources": sources,
            "recovery": sidecar.as_ref().map(|sidecar| sidecar.recovery.as_slice()),
        },
        "egress": egress_lines(stored),
        "source_rights": source_rights(lineage.as_ref()),
        "lineage": lineage,
    })
}
