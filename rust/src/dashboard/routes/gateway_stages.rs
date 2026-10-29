// SPDX-License-Identifier: Apache-2.0
//! The Control and Deliver stages of the cockpit.
//!
//! `GET /api/gateway/control` — what decides here: the gateway's mode and
//! detectors, the policy layers (organization artifact, project pack, the
//! effective rules) and the reasons the newest receipts recorded for what
//! they withheld, redacted or flagged. Rules are shown by name and count:
//! redaction patterns and blocked patterns can name what they protect, so
//! their text never leaves here.
//!
//! `GET /api/gateway/deliver` — how far each delivery is known to have gone,
//! per route, in the doc 19 receipt states (prepared, handed off, observed
//! model egress, rejected, partial): an egress the proxy or a host observed
//! is kept apart from context that was only prepared. And what this host
//! keeps of shared team artifacts.
//!
//! Both read the receipt indexes the receipts route reads; no source text.

use std::collections::BTreeMap;

use crate::core::context_admission::receipt_store::{self, view};

use super::helpers::detect_project_root_for_dashboard;

/// Receipts read per index for the tallies.
const RECEIPTS_PER_INDEX: usize = 50;

fn serde_word<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The newest stored receipts of every index this dashboard reads, once each.
fn newest_receipts() -> Vec<receipt_store::StoredReceipt> {
    let mut seen = std::collections::HashSet::new();
    let mut stored = Vec::new();
    for (_, key) in super::gateway_receipts::indexes() {
        for (hex, loaded) in receipt_store::latest(&key, RECEIPTS_PER_INDEX) {
            if seen.insert(hex) {
                if let Ok(receipt) = loaded {
                    stored.push(receipt);
                }
            }
        }
    }
    stored
}

/// The effective rules by name and count, never their patterns.
fn policy_summary(policy: &crate::core::policy::ResolvedPolicy) -> serde_json::Value {
    serde_json::json!({
        "name": policy.name,
        "version": policy.version,
        "chain": policy.chain,
        "allow_tools": policy.allow_tools,
        "deny_tools": policy.deny_tools,
        "max_context_tokens": policy.max_context_tokens,
        "audit_retention_days": policy.audit_retention_days,
        "redaction_rules": policy.redaction.len(),
        "filters": {
            "pii": policy.filters.pii,
            "classification": policy.filters.classification,
            "injection": policy.filters.injection,
            "blocked_labels": policy.filters.blocked_labels.len(),
            "blocked_patterns": policy.filters.blocked_patterns.len(),
        },
        "egress_rules": !policy.egress.is_empty(),
    })
}

/// `GET /api/gateway/detectors`: what content is screened for, without the
/// receipt read of the control document.
fn detectors_document() -> serde_json::Value {
    // `pii.sources`: what every source is screened for (the gateway's `pii`
    // setting); `pii.policy_filters`: what a policy pack's `[filters] pii`
    // screens for.
    serde_json::json!({
        "pii": {
            "sources": crate::core::input_filters::pii::SOURCE_DETECTOR_NAMES,
            "policy_filters": crate::core::input_filters::pii::DETECTOR_NAMES,
        },
        "filter_packs": hive_filter_labels(),
    })
}

fn hive_filter_labels() -> Vec<String> {
    let rules = crate::core::hive::policy::installed_filter_rules();
    rules
        .redaction
        .into_iter()
        .chain(rules.blocked)
        .map(|(label, _)| label)
        .collect()
}

fn control_document(project: &str, receipts: &[receipt_store::StoredReceipt]) -> serde_json::Value {
    let config = crate::core::config::Config::load();
    let gateway = serde_json::to_value(&config.context_gateway).unwrap_or_default();
    let org = crate::core::policy::org::status();
    let pack_present = std::path::Path::new(project)
        .join(".lean-ctx/policy.toml")
        .is_file();
    let effective = match crate::core::policy::runtime::for_project(std::path::Path::new(project)) {
        Ok(Some(active)) => {
            serde_json::json!({"state": "active", "policy": policy_summary(&active.resolved)})
        }
        Ok(None) => serde_json::json!({"state": "none"}),
        // A policy that cannot be resolved denies; the dashboard says so.
        Err(_) => serde_json::json!({"state": "unresolvable"}),
    };
    let mut reasons: BTreeMap<String, u64> = BTreeMap::new();
    let mut dispositions: BTreeMap<String, u64> = BTreeMap::new();
    let mut security = [0u64; 5];
    for stored in receipts {
        let receipt = &stored.receipt;
        for decision in &receipt.decisions {
            *dispositions
                .entry(serde_word(&decision.disposition))
                .or_default() += 1;
            for code in &decision.reason_codes {
                *reasons.entry(code.as_str().to_owned()).or_default() += 1;
            }
        }
        let counts = &receipt.security;
        for (slot, value) in security.iter_mut().zip([
            counts.redactions,
            counts.blocked_objects,
            counts.quarantined_objects,
            counts.injection_signals,
            counts.incomplete_coverage,
        ]) {
            *slot += u64::from(value);
        }
    }
    serde_json::json!({
        "gateway": gateway,
        "org": {
            "present": org.present,
            "org": org.org,
            "version": org.policy_version,
            "issued_at": org.issued_at,
            "signature_valid": org.signature_valid,
            "trusted": org.trusted,
            "enforced": org.enforced,
            "applied": org.applied,
            "resolve_error": org.resolve_error.is_some(),
            "pinned_anchors": org.pinned_anchors,
        },
        "project_pack": pack_present,
        "effective": effective,
        // What content is screened for: the built-in PII classes and the
        // rules of the installed Hive filter packs, by label.
        "detectors": detectors_document(),
        "receipts_read": receipts.len(),
        "dispositions": dispositions,
        "reasons": reasons,
        "security": {
            "redactions": security[0],
            "blocked_objects": security[1],
            "quarantined_objects": security[2],
            "injection_signals": security[3],
            "incomplete_coverage": security[4],
        },
    })
}

fn deliver_document(receipts: &[receipt_store::StoredReceipt]) -> serde_json::Value {
    // route → receipt state → count, in the doc 19 vocabulary.
    let mut routes: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut without_trace = 0u64;
    for stored in receipts {
        if let Some(lineage) = view::lineage(stored) {
            *routes
                .entry(serde_word(&lineage.request.route))
                .or_default()
                .entry(serde_word(&lineage.receipt_state))
                .or_default() += 1;
        } else {
            // No trace sidecar (developer mode, older receipts): the route is
            // unknown, and so is any hand-over; what the receipt itself says
            // decides between prepared, partial and rejected.
            without_trace += 1;
            let receipt = &stored.receipt;
            let withheld = receipt.decisions.iter().any(|decision| {
                !lean_ctx_protocol::context_lineage::ContextDecisionOutcomeV1::from(
                    decision.disposition,
                )
                .delivers_content()
            }) || receipt.security.incomplete_coverage > 0;
            let state = lean_ctx_protocol::context_lineage::ContextReceiptStateV1::derive(
                receipt.outcome,
                lean_ctx_protocol::context_lineage::IntegrationRouteV1::Mcp,
                false,
                0,
                withheld,
            );
            *routes
                .entry("not_recorded".to_owned())
                .or_default()
                .entry(serde_word(&state))
                .or_default() += 1;
        }
    }
    let kept_artifacts = crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|dir| dir.join("shared-artifacts"))
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .map_or(0, |entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                .count()
        });
    serde_json::json!({
        "receipts_read": receipts.len(),
        "routes": routes,
        "without_trace": without_trace,
        "kept_team_artifacts": kept_artifacts,
    })
}

pub(super) fn handle(
    path: &str,
    _query: &str,
    method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    let document = match path {
        "/api/gateway/control" | "/api/gateway/deliver" | "/api/gateway/detectors"
            if !method.eq_ignore_ascii_case("GET") =>
        {
            return Some((
                "405 Method Not Allowed",
                "application/json",
                r#"{"error":"method not allowed"}"#.to_string(),
            ));
        }
        "/api/gateway/control" => {
            control_document(&detect_project_root_for_dashboard(), &newest_receipts())
        }
        "/api/gateway/deliver" => deliver_document(&newest_receipts()),
        "/api/gateway/detectors" => detectors_document(),
        _ => return None,
    };
    Some(("200 OK", "application/json", document.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detectors_name_the_builtin_pii_classes_and_installed_packs() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let (status, _, body) = handle("/api/gateway/detectors", "", "GET", "").unwrap();
        assert_eq!(status, "200 OK");
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["pii"]["sources"], serde_json::json!(["card"]));
        assert_eq!(
            value["pii"]["policy_filters"],
            serde_json::json!(["card", "email", "phone"])
        );
        assert_eq!(value["filter_packs"], serde_json::json!([]));
        assert_eq!(
            handle("/api/gateway/detectors", "", "POST", "").unwrap().0,
            "405 Method Not Allowed"
        );
    }

    /// Control names the rules of the active pack by count and never prints a
    /// pattern; it tallies the reasons of what the gateway withheld.
    #[test]
    fn control_shows_rules_by_count_and_never_their_patterns() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let project = tempfile::tempdir().expect("project");
        let root = project.path().canonicalize().expect("canonical root");
        std::fs::create_dir_all(root.join(".lean-ctx")).expect("pack dir");
        std::fs::write(
            root.join(".lean-ctx/policy.toml"),
            "name='control'\nversion='1.0.0'\ndescription='fixture'\n\
             [redaction]\ncustomer='ZQX7319'\n\
             [filters.blocked_patterns]\ncodename='PROJECT-ORCHID'\n",
        )
        .expect("pack");
        let document = control_document(&root.to_string_lossy(), &[]);
        assert_eq!(document["project_pack"], true);
        assert_eq!(document["effective"]["state"], "active");
        assert_eq!(document["effective"]["policy"]["redaction_rules"], 1);
        assert_eq!(
            document["effective"]["policy"]["filters"]["blocked_patterns"],
            1
        );
        let text = document.to_string();
        assert!(
            !text.contains("ZQX7319") && !text.contains("PROJECT-ORCHID"),
            "{text}"
        );
    }
}
