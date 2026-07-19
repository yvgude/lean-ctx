//! Deterministic adapter from verified savings-ledger entries to Settlement Evidence V2.
//!
//! This module deliberately does not extend [`SavingsEvent`], replace
//! `SignedSavingsBatchV1`, invent trust, calculate a settlement price, or decide
//! eligibility. It binds already reconciled, payload-free source references to a
//! verified ledger snapshot and emits the existing settlement attribution items.
//! The settlement verifier and an operator-supplied trust store remain the only
//! authority for structural eligibility.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use serde::{Deserialize, Serialize};

use super::SignedSavingsBatchV1;
use super::event::{MECHANISM_CACHING, MECHANISM_COMPRESSION, MECHANISM_ROUTING, SavingsEvent};
use super::store::GENESIS;
use crate::core::billing::settlement_evidence::{
    EvidenceStateV2, EvidenceTrustV2, SettlementEvidenceClaimV2, SettlementEvidenceClassV2,
    SettlementEvidenceItemV2, SettlementEvidenceManifestV2, SettlementPeriodV2,
};

/// Same global evidence-row bound as Settlement Evidence V2.
pub const MAX_LEDGER_EVIDENCE_ROWS_V2: usize = 1_000;
pub const MAX_LEDGER_SNAPSHOT_BYTES_V2: u64 = 4 * 1024 * 1024;
pub const MAX_LEDGER_PROJECTION_BYTES_V2: u64 = 4 * 1024 * 1024;
pub const MAX_LEDGER_EVIDENCE_STRING_BYTES_V2: usize = 256;
const ARTIFACT_ADDRESS_PREFIX: &str = "artifact:blake3:";
const ANCHOR_ADDRESS_PREFIX: &str = "anchor:blake3:";
const PROJECTION_SCHEMA_VERSION: u16 = 2;
const PROJECTION_KIND: &str = "lean-ctx.unified-ledger-evidence-projection";
const PENDING_PROJECTION_ID: &str = "artifact:pending";

/// An immutable, chain-verified in-memory ledger view.
///
/// Construction verifies every link before exposing the type. It does not claim
/// that a caller read a live file atomically; callers must supply one stable file
/// snapshot rather than combining a separate `verify()` and `load()` operation.
#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedLedgerSnapshotV2 {
    snapshot_id: String,
    first_entry_hash: String,
    last_entry_hash: String,
    events: Vec<SavingsEvent>,
}

impl VerifiedLedgerSnapshotV2 {
    /// Verify an ordered event snapshot and bind its exact chain order.
    pub fn try_from_events(events: Vec<SavingsEvent>) -> Result<Self, LedgerProjectionErrorV2> {
        if events.len() > MAX_LEDGER_EVIDENCE_ROWS_V2 {
            return Err(LedgerProjectionErrorV2::TooManyEvents);
        }

        let mut expected_previous = GENESIS.to_string();
        let mut seen_hashes = BTreeSet::new();
        for (index, event) in events.iter().enumerate() {
            if !is_sha256_hex(&event.entry_hash) {
                return Err(LedgerProjectionErrorV2::InvalidEntryHash { index });
            }
            if !seen_hashes.insert(event.entry_hash.clone()) {
                return Err(LedgerProjectionErrorV2::DuplicateEntryHash {
                    entry_hash: event.entry_hash.clone(),
                });
            }
            if event.prev_hash != expected_previous || !event.hash_matches(&expected_previous) {
                return Err(LedgerProjectionErrorV2::BrokenChain { index });
            }
            if !event.saved_usd.is_finite() || !event.unit_price_per_m_usd.is_finite() {
                return Err(LedgerProjectionErrorV2::NonFiniteMoney { index });
            }
            validate_event_semantics(event, index)?;
            expected_previous.clone_from(&event.entry_hash);
        }

        let first_entry_hash = events
            .first()
            .map_or_else(|| GENESIS.to_string(), |event| event.entry_hash.clone());
        let last_entry_hash = events
            .last()
            .map_or_else(|| GENESIS.to_string(), |event| event.entry_hash.clone());
        let snapshot_id = snapshot_address(&events);

        Ok(Self {
            snapshot_id,
            first_entry_hash,
            last_entry_hash,
            events,
        })
    }

    #[must_use]
    pub fn snapshot_id(&self) -> &str {
        &self.snapshot_id
    }

    #[must_use]
    pub fn first_entry_hash(&self) -> &str {
        &self.first_entry_hash
    }

    #[must_use]
    pub fn last_entry_hash(&self) -> &str {
        &self.last_entry_hash
    }

    #[must_use]
    pub const fn event_count(&self) -> usize {
        self.events.len()
    }
}

/// One externally reconciled source assignment for a positive ledger observation.
///
/// `source_evidence_id` and `attribution_group_id` must be content addresses.
/// `trust` is copied into the generated settlement item, but it cannot make the
/// item eligible without an exact out-of-band trust-store decision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerAttributionLinkV2 {
    pub ledger_entry_hash: String,
    pub source_evidence_id: String,
    pub attribution_group_id: String,
    pub attributed_tokens: u64,
    pub attributed_minor_units: u64,
    pub trust: EvidenceTrustV2,
}

/// Audit mapping retained alongside projected settlement items.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerEvidenceSourceBindingV2 {
    pub ledger_entry_hash: String,
    pub source_evidence_id: String,
    pub attribution_group_id: String,
    pub projected_source_id: String,
    pub mechanism_id: String,
    pub attributed_tokens: u64,
    pub attributed_minor_units: u64,
    pub trust: EvidenceTrustV2,
}

/// Payload-free projection result. This is an adapter result, not a settlement
/// manifest and not an eligibility decision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerEvidenceProjectionV2 {
    pub projection_id: String,
    pub schema_version: u16,
    pub kind: String,
    pub signed_batch_id: String,
    pub signer_public_key: String,
    pub subject_id: String,
    pub ledger_snapshot_id: String,
    pub first_entry_hash: String,
    pub last_entry_hash: String,
    pub event_count: usize,
    pub bindings: Vec<LedgerEvidenceSourceBindingV2>,
    pub settlement_attribution_items: Vec<SettlementEvidenceItemV2>,
}

impl LedgerEvidenceProjectionV2 {
    /// Verify the artifact's canonical address, signed batch binding and internal mapping.
    pub fn verify(
        &self,
        batch: &SignedSavingsBatchV1,
        snapshot: &VerifiedLedgerSnapshotV2,
    ) -> Result<(), LedgerProjectionErrorV2> {
        if self.schema_version != PROJECTION_SCHEMA_VERSION || self.kind != PROJECTION_KIND {
            return Err(LedgerProjectionErrorV2::InvalidProjectionIdentity);
        }
        if !is_address(&self.subject_id, "subject:blake3:") {
            return Err(LedgerProjectionErrorV2::InvalidSubjectId);
        }
        if self.ledger_snapshot_id != snapshot.snapshot_id
            || self.event_count != snapshot.events.len()
            || self.first_entry_hash != snapshot.first_entry_hash
            || self.last_entry_hash != snapshot.last_entry_hash
        {
            return Err(LedgerProjectionErrorV2::SnapshotMismatch);
        }
        validate_batch_fields(
            batch,
            snapshot.events.len(),
            &snapshot.first_entry_hash,
            &snapshot.last_entry_hash,
        )?;
        if self.signed_batch_id != signed_batch_address(batch)?
            || batch.signer_public_key.as_deref() != Some(self.signer_public_key.as_str())
        {
            return Err(LedgerProjectionErrorV2::SignedBatchMismatch);
        }
        self.verify_bindings_and_items(snapshot)?;
        if self.projection_id != self.computed_projection_id()? {
            return Err(LedgerProjectionErrorV2::InvalidProjectionId);
        }
        Ok(())
    }

    /// Canonical JSON after all offline checks pass.
    pub fn canonical_json(
        &self,
        batch: &SignedSavingsBatchV1,
        snapshot: &VerifiedLedgerSnapshotV2,
    ) -> Result<String, LedgerProjectionErrorV2> {
        self.verify(batch, snapshot)?;
        let mut canonical = self.clone();
        canonical.canonicalize();
        serde_json::to_string(&canonical)
            .map_err(|error| LedgerProjectionErrorV2::ProjectionSerialization(error.to_string()))
    }

    fn canonicalize(&mut self) {
        self.bindings
            .sort_by(|left, right| left.ledger_entry_hash.cmp(&right.ledger_entry_hash));
        self.settlement_attribution_items
            .sort_by(|left, right| left.evidence_id.cmp(&right.evidence_id));
    }

    fn computed_projection_id(&self) -> Result<String, LedgerProjectionErrorV2> {
        let mut identity = self.clone();
        identity.projection_id = PENDING_PROJECTION_ID.to_string();
        identity.canonicalize();
        let bytes = serde_json::to_vec(&identity)
            .map_err(|error| LedgerProjectionErrorV2::ProjectionSerialization(error.to_string()))?;
        if bytes.len() as u64 > MAX_LEDGER_PROJECTION_BYTES_V2 {
            return Err(LedgerProjectionErrorV2::ProjectionTooLarge);
        }
        Ok(format!(
            "{ARTIFACT_ADDRESS_PREFIX}{}",
            blake3::hash(&bytes).to_hex()
        ))
    }

    fn verify_bindings_and_items(
        &self,
        snapshot: &VerifiedLedgerSnapshotV2,
    ) -> Result<(), LedgerProjectionErrorV2> {
        if self.bindings.len() > MAX_LEDGER_EVIDENCE_ROWS_V2
            || self.settlement_attribution_items.len() > 3
        {
            return Err(LedgerProjectionErrorV2::TooManyLinks);
        }
        let mut entries = BTreeSet::new();
        let mut sources = BTreeSet::new();
        let mut groups = BTreeSet::new();
        let events_by_hash: BTreeMap<_, _> = snapshot
            .events
            .iter()
            .map(|event| (event.entry_hash.as_str(), event))
            .collect();
        let mut required_entries = BTreeSet::new();
        for event in &snapshot.events {
            if event.bounce_adjustment > 0 || event.saved_usd < 0.0 {
                return Err(LedgerProjectionErrorV2::AdjustmentRequiresReconciliation {
                    entry_hash: event.entry_hash.clone(),
                });
            }
            if event.saved_tokens > 0 || event.saved_usd > 0.0 {
                required_entries.insert(event.entry_hash.as_str());
            }
        }

        let mut by_mechanism: BTreeMap<&str, (u64, u64, Vec<&str>, &EvidenceTrustV2)> =
            BTreeMap::new();
        for binding in &self.bindings {
            let Some(event) = events_by_hash
                .get(binding.ledger_entry_hash.as_str())
                .copied()
            else {
                return Err(LedgerProjectionErrorV2::InvalidProjectionBinding);
            };
            let expected_mechanism_id = mechanism_address(&event.mechanism)?;
            let token_ceiling = if event.mechanism == MECHANISM_COMPRESSION {
                event.saved_tokens.saturating_sub(event.bounce_adjustment)
            } else {
                event.baseline_tokens
            };
            if !is_sha256_hex(&binding.ledger_entry_hash)
                || !is_artifact_address(&binding.source_evidence_id)
                || !is_artifact_address(&binding.attribution_group_id)
                || binding.mechanism_id != expected_mechanism_id
                || binding.projected_source_id
                    != projected_source_address_from_parts(
                        &self.ledger_snapshot_id,
                        &self.signed_batch_id,
                        &binding.ledger_entry_hash,
                        &binding.source_evidence_id,
                        &binding.attribution_group_id,
                    )
                || binding.attributed_tokens == 0
                || binding.attributed_minor_units == 0
                || binding.attributed_tokens > token_ceiling
                || !is_artifact_address(&binding.trust.trust_decision_id)
                || !is_address(&binding.trust.trust_anchor_id, ANCHOR_ADDRESS_PREFIX)
            {
                return Err(LedgerProjectionErrorV2::InvalidProjectionBinding);
            }
            if !entries.insert(binding.ledger_entry_hash.as_str())
                || !sources.insert(binding.source_evidence_id.as_str())
                || !groups.insert(binding.attribution_group_id.as_str())
            {
                return Err(LedgerProjectionErrorV2::InvalidProjectionBinding);
            }
            let aggregate = by_mechanism
                .entry(binding.mechanism_id.as_str())
                .or_insert((0, 0, Vec::new(), &binding.trust));
            if aggregate.3 != &binding.trust {
                return Err(LedgerProjectionErrorV2::InvalidProjectionBinding);
            }
            aggregate.0 = aggregate
                .0
                .checked_add(binding.attributed_tokens)
                .ok_or(LedgerProjectionErrorV2::ArithmeticOverflow)?;
            aggregate.1 = aggregate
                .1
                .checked_add(binding.attributed_minor_units)
                .ok_or(LedgerProjectionErrorV2::ArithmeticOverflow)?;
            aggregate.2.push(binding.projected_source_id.as_str());
        }

        if entries != required_entries {
            return Err(LedgerProjectionErrorV2::InvalidProjectionBinding);
        }

        if by_mechanism.len() != self.settlement_attribution_items.len() {
            return Err(LedgerProjectionErrorV2::InvalidProjectionBinding);
        }
        for item in &self.settlement_attribution_items {
            let SettlementEvidenceClaimV2::Attribution {
                mechanism_id,
                exclusive,
                attributed_tokens,
                attributed_minor_units,
                source_evidence_ids,
            } = &item.claim
            else {
                return Err(LedgerProjectionErrorV2::InvalidProjectionBinding);
            };
            let Some((tokens, minor, mut sources, trust)) =
                by_mechanism.remove(mechanism_id.as_str())
            else {
                return Err(LedgerProjectionErrorV2::InvalidProjectionBinding);
            };
            sources.sort_unstable();
            let mut item_sources: Vec<_> = source_evidence_ids.iter().map(String::as_str).collect();
            item_sources.sort_unstable();
            let expected_method_id = format!(
                "{ARTIFACT_ADDRESS_PREFIX}{}",
                blake3::hash(b"settlement-exclusive-attribution-v2").to_hex()
            );
            if !exclusive
                || *attributed_tokens != tokens
                || *attributed_minor_units != minor
                || item_sources != sources
                || item.subject_id != self.subject_id
                || item.state != EvidenceStateV2::Active
                || item.trust != *trust
                || item.measurement.evidence_class != SettlementEvidenceClassV2::Reconciled
                || item.measurement.method_artifact_id != expected_method_id
                || !item.supersedes.is_empty()
                || item.correction_reason_id.is_some()
            {
                return Err(LedgerProjectionErrorV2::InvalidProjectionBinding);
            }
        }
        if !by_mechanism.is_empty() {
            return Err(LedgerProjectionErrorV2::InvalidProjectionBinding);
        }

        let claimed_minor_units = self
            .settlement_attribution_items
            .iter()
            .try_fold(0_u64, |total, item| match &item.claim {
                SettlementEvidenceClaimV2::Attribution {
                    attributed_minor_units,
                    ..
                } => total.checked_add(*attributed_minor_units),
                _ => None,
            })
            .ok_or(LedgerProjectionErrorV2::ArithmeticOverflow)?;
        let integrity_manifest = SettlementEvidenceManifestV2::new(
            self.subject_id.clone(),
            SettlementPeriodV2 {
                start_epoch_seconds: 0,
                end_epoch_seconds: 1,
            },
            "CHF".to_string(),
            claimed_minor_units,
            self.settlement_attribution_items.clone(),
        )
        .map_err(|error| LedgerProjectionErrorV2::SettlementEvidence(error.to_string()))?;
        integrity_manifest
            .canonical_json()
            .map_err(|error| LedgerProjectionErrorV2::SettlementEvidence(error.to_string()))?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct MechanismAggregate {
    mechanism_id: String,
    trust: EvidenceTrustV2,
    attributed_tokens: u64,
    attributed_minor_units: u64,
    projected_sources: Vec<String>,
}

/// Project a complete, exclusive attribution mapping into Settlement Evidence V2 items.
///
/// Every positive event must have exactly one link. Reused event hashes, source IDs,
/// or attribution groups fail closed. Negative/bounce adjustments require a separate
/// accepted reconciliation and are never silently dropped from a positive claim.
pub fn project_settlement_attribution_v2(
    snapshot: &VerifiedLedgerSnapshotV2,
    batch: &SignedSavingsBatchV1,
    subject_id: &str,
    links: &[LedgerAttributionLinkV2],
) -> Result<LedgerEvidenceProjectionV2, LedgerProjectionErrorV2> {
    if links.len() > MAX_LEDGER_EVIDENCE_ROWS_V2 {
        return Err(LedgerProjectionErrorV2::TooManyLinks);
    }
    if !is_address(subject_id, "subject:blake3:") {
        return Err(LedgerProjectionErrorV2::InvalidSubjectId);
    }
    validate_batch_fields(
        batch,
        snapshot.events.len(),
        &snapshot.first_entry_hash,
        &snapshot.last_entry_hash,
    )?;
    let signed_batch_id = signed_batch_address(batch)?;
    let signer_public_key = batch
        .signer_public_key
        .clone()
        .ok_or(LedgerProjectionErrorV2::InvalidSignedBatch)?;

    let mut links_by_entry = BTreeMap::new();
    for link in links {
        validate_link_addresses(link)?;
        if links_by_entry
            .insert(link.ledger_entry_hash.as_str(), link)
            .is_some()
        {
            return Err(LedgerProjectionErrorV2::DuplicateLink {
                entry_hash: link.ledger_entry_hash.clone(),
            });
        }
    }

    let mut used_entries = BTreeSet::new();
    let mut used_sources = BTreeSet::new();
    let mut used_groups = BTreeSet::new();
    let mut aggregates: BTreeMap<String, MechanismAggregate> = BTreeMap::new();
    let mut bindings = Vec::new();

    for event in &snapshot.events {
        let mechanism_id = mechanism_address(&event.mechanism)?;
        if event.bounce_adjustment > 0 || event.saved_usd < 0.0 {
            return Err(LedgerProjectionErrorV2::AdjustmentRequiresReconciliation {
                entry_hash: event.entry_hash.clone(),
            });
        }
        if event.saved_tokens == 0 && event.saved_usd == 0.0 {
            continue;
        }

        let Some(link) = links_by_entry.get(event.entry_hash.as_str()).copied() else {
            return Err(LedgerProjectionErrorV2::MissingLink {
                entry_hash: event.entry_hash.clone(),
            });
        };
        used_entries.insert(event.entry_hash.as_str());

        if !used_sources.insert(link.source_evidence_id.as_str()) {
            return Err(LedgerProjectionErrorV2::DuplicateSource {
                source_evidence_id: link.source_evidence_id.clone(),
            });
        }
        if !used_groups.insert(link.attribution_group_id.as_str()) {
            return Err(LedgerProjectionErrorV2::DuplicateAttributionGroup {
                attribution_group_id: link.attribution_group_id.clone(),
            });
        }
        if link.attributed_tokens == 0 || link.attributed_minor_units == 0 {
            return Err(LedgerProjectionErrorV2::NonPositiveAttribution {
                entry_hash: event.entry_hash.clone(),
            });
        }

        let token_ceiling = if event.mechanism == MECHANISM_COMPRESSION {
            event.saved_tokens.saturating_sub(event.bounce_adjustment)
        } else {
            event.baseline_tokens
        };
        if link.attributed_tokens > token_ceiling {
            return Err(LedgerProjectionErrorV2::AttributionExceedsObservation {
                entry_hash: event.entry_hash.clone(),
            });
        }

        let projected_source_id = projected_source_address(snapshot, &signed_batch_id, event, link);
        bindings.push(LedgerEvidenceSourceBindingV2 {
            ledger_entry_hash: event.entry_hash.clone(),
            source_evidence_id: link.source_evidence_id.clone(),
            attribution_group_id: link.attribution_group_id.clone(),
            projected_source_id: projected_source_id.clone(),
            mechanism_id: mechanism_id.clone(),
            attributed_tokens: link.attributed_tokens,
            attributed_minor_units: link.attributed_minor_units,
            trust: link.trust.clone(),
        });

        let aggregate = aggregates
            .entry(event.mechanism.clone())
            .or_insert_with(|| MechanismAggregate {
                mechanism_id,
                trust: link.trust.clone(),
                attributed_tokens: 0,
                attributed_minor_units: 0,
                projected_sources: Vec::new(),
            });
        if aggregate.trust != link.trust {
            return Err(LedgerProjectionErrorV2::AmbiguousMechanismTrust {
                mechanism: event.mechanism.clone(),
            });
        }
        aggregate.attributed_tokens = aggregate
            .attributed_tokens
            .checked_add(link.attributed_tokens)
            .ok_or(LedgerProjectionErrorV2::ArithmeticOverflow)?;
        aggregate.attributed_minor_units = aggregate
            .attributed_minor_units
            .checked_add(link.attributed_minor_units)
            .ok_or(LedgerProjectionErrorV2::ArithmeticOverflow)?;
        aggregate.projected_sources.push(projected_source_id);
    }

    if let Some(unused) = links
        .iter()
        .find(|link| !used_entries.contains(link.ledger_entry_hash.as_str()))
    {
        return Err(LedgerProjectionErrorV2::UnknownLedgerEntry {
            entry_hash: unused.ledger_entry_hash.clone(),
        });
    }
    if aggregates.is_empty() {
        return Err(LedgerProjectionErrorV2::NoAttributableEvidence);
    }

    let mut settlement_attribution_items = Vec::with_capacity(aggregates.len());
    for aggregate in aggregates.into_values() {
        let item = SettlementEvidenceItemV2::new(
            subject_id.to_string(),
            SettlementEvidenceClaimV2::Attribution {
                mechanism_id: aggregate.mechanism_id,
                exclusive: true,
                attributed_tokens: aggregate.attributed_tokens,
                attributed_minor_units: aggregate.attributed_minor_units,
                source_evidence_ids: aggregate.projected_sources,
            },
            aggregate.trust,
        )
        .map_err(|error| LedgerProjectionErrorV2::SettlementEvidence(error.to_string()))?;
        settlement_attribution_items.push(item);
    }

    bindings.sort_by(|left, right| left.ledger_entry_hash.cmp(&right.ledger_entry_hash));
    settlement_attribution_items.sort_by(|left, right| left.evidence_id.cmp(&right.evidence_id));

    let mut projection = LedgerEvidenceProjectionV2 {
        projection_id: PENDING_PROJECTION_ID.to_string(),
        schema_version: PROJECTION_SCHEMA_VERSION,
        kind: PROJECTION_KIND.to_string(),
        signed_batch_id,
        signer_public_key,
        subject_id: subject_id.to_string(),
        ledger_snapshot_id: snapshot.snapshot_id.clone(),
        first_entry_hash: snapshot.first_entry_hash.clone(),
        last_entry_hash: snapshot.last_entry_hash.clone(),
        event_count: snapshot.events.len(),
        bindings,
        settlement_attribution_items,
    };
    projection.projection_id = projection.computed_projection_id()?;
    projection.verify(batch, snapshot)?;
    Ok(projection)
}

fn validate_link_addresses(link: &LedgerAttributionLinkV2) -> Result<(), LedgerProjectionErrorV2> {
    if !is_artifact_address(&link.source_evidence_id)
        || !is_artifact_address(&link.attribution_group_id)
    {
        return Err(LedgerProjectionErrorV2::InvalidLinkAddress);
    }
    if !is_artifact_address(&link.trust.trust_decision_id)
        || !is_address(&link.trust.trust_anchor_id, ANCHOR_ADDRESS_PREFIX)
    {
        return Err(LedgerProjectionErrorV2::InvalidLinkAddress);
    }
    if !is_sha256_hex(&link.ledger_entry_hash) {
        return Err(LedgerProjectionErrorV2::InvalidLinkedEntryHash);
    }
    Ok(())
}

fn snapshot_address(events: &[SavingsEvent]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lean-ctx.unified-ledger-snapshot-v2\0");
    for event in events {
        hasher.update(event.entry_hash.as_bytes());
        hasher.update(b"\0");
    }
    format!("{ARTIFACT_ADDRESS_PREFIX}{}", hasher.finalize().to_hex())
}

fn mechanism_address(mechanism: &str) -> Result<String, LedgerProjectionErrorV2> {
    if !matches!(
        mechanism,
        MECHANISM_COMPRESSION | MECHANISM_ROUTING | MECHANISM_CACHING
    ) {
        return Err(LedgerProjectionErrorV2::UnknownMechanism {
            mechanism: mechanism.to_string(),
        });
    }
    Ok(format!(
        "mechanism:blake3:{}",
        blake3::hash(format!("lean-ctx.savings-mechanism-v1:{mechanism}").as_bytes()).to_hex()
    ))
}

fn projected_source_address(
    snapshot: &VerifiedLedgerSnapshotV2,
    signed_batch_id: &str,
    event: &SavingsEvent,
    link: &LedgerAttributionLinkV2,
) -> String {
    projected_source_address_from_parts(
        &snapshot.snapshot_id,
        signed_batch_id,
        &event.entry_hash,
        &link.source_evidence_id,
        &link.attribution_group_id,
    )
}

fn projected_source_address_from_parts(
    snapshot_id: &str,
    signed_batch_id: &str,
    entry_hash: &str,
    source_evidence_id: &str,
    attribution_group_id: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lean-ctx.unified-ledger-source-v2\0");
    for component in [
        snapshot_id,
        signed_batch_id,
        entry_hash,
        source_evidence_id,
        attribution_group_id,
    ] {
        hasher.update(component.as_bytes());
        hasher.update(b"\0");
    }
    format!("{ARTIFACT_ADDRESS_PREFIX}{}", hasher.finalize().to_hex())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_artifact_address(value: &str) -> bool {
    is_address(value, ARTIFACT_ADDRESS_PREFIX)
}

fn is_address(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(is_sha256_hex)
}

fn validate_event_semantics(
    event: &SavingsEvent,
    index: usize,
) -> Result<(), LedgerProjectionErrorV2> {
    for value in [
        event.ts.as_str(),
        event.tool.as_str(),
        event.mechanism.as_str(),
        event.model_id.as_str(),
        event.tokenizer.as_str(),
        event.repo_hash.as_str(),
        event.agent_id.as_str(),
        event.prev_hash.as_str(),
        event.entry_hash.as_str(),
        event.version.as_str(),
    ] {
        if value.len() > MAX_LEDGER_EVIDENCE_STRING_BYTES_V2 {
            return Err(LedgerProjectionErrorV2::OversizedEventField { index });
        }
    }
    if event.unit_price_per_m_usd < 0.0 {
        return Err(LedgerProjectionErrorV2::InvalidObservation { index });
    }

    if event.bounce_adjustment > 0 {
        if event.tool != "bounce"
            || event.saved_tokens != 0
            || event.actual_tokens != event.baseline_tokens
            || event.bounce_adjustment > event.baseline_tokens
            || event.saved_usd >= 0.0
        {
            return Err(LedgerProjectionErrorV2::InvalidObservation { index });
        }
        return Ok(());
    }

    let valid = match event.mechanism.as_str() {
        MECHANISM_COMPRESSION => {
            event.saved_usd >= 0.0
                && event
                    .actual_tokens
                    .checked_add(event.saved_tokens)
                    .is_some_and(|total| total == event.baseline_tokens)
        }
        MECHANISM_ROUTING => {
            event.saved_tokens == 0
                && event.actual_tokens == event.baseline_tokens
                && event.saved_usd != 0.0
        }
        MECHANISM_CACHING => {
            event.saved_tokens == 0
                && event.actual_tokens == event.baseline_tokens
                && event.saved_usd > 0.0
        }
        _ => false,
    };
    if !valid {
        return Err(LedgerProjectionErrorV2::InvalidObservation { index });
    }
    Ok(())
}

fn validate_batch_fields(
    batch: &SignedSavingsBatchV1,
    event_count: usize,
    first_entry_hash: &str,
    last_entry_hash: &str,
) -> Result<(), LedgerProjectionErrorV2> {
    let verification = batch.verify();
    if batch.schema_version != 1
        || batch.kind != "lean-ctx.savings-batch"
        || !batch.chain_valid
        || batch.totals.total_events != event_count
        || batch.first_entry_hash != first_entry_hash
        || batch.last_entry_hash != last_entry_hash
        || !verification.signature_valid
        || verification.signer_public_key.as_deref() != batch.signer_public_key.as_deref()
    {
        return Err(LedgerProjectionErrorV2::InvalidSignedBatch);
    }
    Ok(())
}

fn signed_batch_address(batch: &SignedSavingsBatchV1) -> Result<String, LedgerProjectionErrorV2> {
    let bytes = serde_json::to_vec(batch)
        .map_err(|error| LedgerProjectionErrorV2::ProjectionSerialization(error.to_string()))?;
    if bytes.len() as u64 > MAX_LEDGER_PROJECTION_BYTES_V2 {
        return Err(LedgerProjectionErrorV2::ProjectionTooLarge);
    }
    Ok(format!(
        "{ARTIFACT_ADDRESS_PREFIX}{}",
        blake3::hash(&bytes).to_hex()
    ))
}

/// Bounded, no-follow offline projection load followed by full batch-bound verification.
pub fn load_projection_artifact_v2(
    path: &Path,
    batch: &SignedSavingsBatchV1,
    snapshot: &VerifiedLedgerSnapshotV2,
) -> Result<LedgerEvidenceProjectionV2, LedgerProjectionErrorV2> {
    let link_metadata = std::fs::symlink_metadata(path)
        .map_err(|error| LedgerProjectionErrorV2::ArtifactIo(error.to_string()))?;
    if link_metadata.file_type().is_symlink() || !link_metadata.file_type().is_file() {
        return Err(LedgerProjectionErrorV2::ArtifactNotRegular);
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options
        .open(path)
        .map_err(|error| LedgerProjectionErrorV2::ArtifactIo(error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| LedgerProjectionErrorV2::ArtifactIo(error.to_string()))?;
    if !metadata.file_type().is_file() {
        return Err(LedgerProjectionErrorV2::ArtifactNotRegular);
    }
    if metadata.len() > MAX_LEDGER_PROJECTION_BYTES_V2 {
        return Err(LedgerProjectionErrorV2::ProjectionTooLarge);
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_LEDGER_PROJECTION_BYTES_V2 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| LedgerProjectionErrorV2::ArtifactIo(error.to_string()))?;
    if bytes.len() as u64 > MAX_LEDGER_PROJECTION_BYTES_V2 {
        return Err(LedgerProjectionErrorV2::ProjectionTooLarge);
    }
    let projection: LedgerEvidenceProjectionV2 = serde_json::from_slice(&bytes)
        .map_err(|error| LedgerProjectionErrorV2::ProjectionSerialization(error.to_string()))?;
    projection.verify(batch, snapshot)?;
    Ok(projection)
}

/// Fail-closed adapter errors. They are evidence-state outcomes, not billing decisions.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LedgerProjectionErrorV2 {
    #[error("ledger snapshot exceeds evidence bound")]
    TooManyEvents,
    #[error("attribution links exceed evidence bound")]
    TooManyLinks,
    #[error("invalid ledger entry hash at index {index}")]
    InvalidEntryHash { index: usize },
    #[error("duplicate ledger entry hash")]
    DuplicateEntryHash { entry_hash: String },
    #[error("ledger hash chain is invalid at index {index}")]
    BrokenChain { index: usize },
    #[error("ledger contains non-finite monetary observation at index {index}")]
    NonFiniteMoney { index: usize },
    #[error("ledger event field exceeds evidence bound at index {index}")]
    OversizedEventField { index: usize },
    #[error("ledger event counters or mechanism semantics are invalid at index {index}")]
    InvalidObservation { index: usize },
    #[error("invalid attribution link address")]
    InvalidLinkAddress,
    #[error("invalid linked ledger entry hash")]
    InvalidLinkedEntryHash,
    #[error("invalid settlement subject id")]
    InvalidSubjectId,
    #[error("duplicate link for ledger entry")]
    DuplicateLink { entry_hash: String },
    #[error("positive ledger observation has no reconciled evidence link")]
    MissingLink { entry_hash: String },
    #[error("link references an unknown or non-positive ledger entry")]
    UnknownLedgerEntry { entry_hash: String },
    #[error("source evidence is attributed more than once")]
    DuplicateSource { source_evidence_id: String },
    #[error("attribution group is used more than once")]
    DuplicateAttributionGroup { attribution_group_id: String },
    #[error("attribution amount must be positive")]
    NonPositiveAttribution { entry_hash: String },
    #[error("attribution exceeds the measured ledger observation")]
    AttributionExceedsObservation { entry_hash: String },
    #[error("negative or bounce adjustment requires explicit reconciliation")]
    AdjustmentRequiresReconciliation { entry_hash: String },
    #[error("unknown savings mechanism")]
    UnknownMechanism { mechanism: String },
    #[error("one mechanism carries conflicting trust decisions")]
    AmbiguousMechanismTrust { mechanism: String },
    #[error("attribution arithmetic overflow")]
    ArithmeticOverflow,
    #[error("snapshot has no attributable evidence")]
    NoAttributableEvidence,
    #[error("settlement evidence construction failed: {0}")]
    SettlementEvidence(String),
    #[error("signed savings batch is invalid or does not bind the snapshot")]
    InvalidSignedBatch,
    #[error("signed savings batch does not match the projection")]
    SignedBatchMismatch,
    #[error("verified ledger snapshot does not match the projection")]
    SnapshotMismatch,
    #[error("projection schema or kind is invalid")]
    InvalidProjectionIdentity,
    #[error("projection content address is invalid")]
    InvalidProjectionId,
    #[error("projection binding is invalid")]
    InvalidProjectionBinding,
    #[error("projection artifact exceeds byte bound")]
    ProjectionTooLarge,
    #[error("projection serialization failed: {0}")]
    ProjectionSerialization(String),
    #[error("projection artifact is not a regular file")]
    ArtifactNotRegular,
    #[error("projection artifact I/O failed: {0}")]
    ArtifactIo(String),
}
