// SPDX-License-Identifier: Apache-2.0
//! Request-scoped collection of admission decisions and the one Decision
//! Receipt per tool call built from them (G4).
//!
//! The MCP dispatcher opens a capture around each call; the read choke point
//! appends one content-free decision per admitted object, including from the
//! detached read workers (`TaskSpine::spawn_thread` carries the capture). When
//! the call's final result exists, [`finish`] turns the decisions into a
//! validated `ContextDecisionReceiptV1` whose digest binds exactly the bytes
//! that were returned. Nothing here holds or persists content.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use lean_ctx_protocol::context_gateway::{
    ContextDecisionReceiptV1, ContextDecisionV1, ContextDispositionV1, DeliveryOutcomeV1,
    DestinationLocalityV1, DestinationV1, GatewayModeV1, MAX_DECISIONS, PolicyRefV1,
    PrincipalKindV1, PrincipalV1, SecurityCountsV1, SourceCountsV1, TokenAccountV1,
};
use lean_ctx_protocol::{PolicyId, ProtocolReference, Sha256Digest, TaskId, V1_SCHEMA_VERSION};

use super::AdmissionCounts;

tokio::task_local! {
    pub(crate) static ADMISSIONS: Option<Arc<AdmissionCapture>>;
}

/// The capture of the call running on this task or worker, if any.
pub(crate) fn current() -> Option<Arc<AdmissionCapture>> {
    ADMISSIONS.try_with(Clone::clone).ok().flatten()
}

#[derive(Debug, Clone)]
struct Recorded {
    decision: ContextDecisionV1,
    counts: AdmissionCounts,
}

/// Everything one call contributes to its receipt.
#[derive(Debug)]
pub(crate) struct AdmissionCapture {
    started: Instant,
    records: Mutex<Vec<Recorded>>,
    /// Decisions beyond `MAX_DECISIONS` are counted in full, never dropped.
    overflow: Mutex<Overflow>,
    /// Original tokens the tool measured for its sources (pre-optimization).
    original_tokens: Mutex<Option<u64>>,
    policy: Mutex<Option<(GatewayModeV1, Sha256Digest)>>,
    /// The task the handler ran under and its tenant/project scope, bound
    /// where admission happens.
    task: Mutex<Option<(String, String)>>,
}

impl Default for AdmissionCapture {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            records: Mutex::new(Vec::new()),
            overflow: Mutex::new(Overflow::default()),
            original_tokens: Mutex::new(None),
            policy: Mutex::new(None),
            task: Mutex::new(None),
        }
    }
}

impl AdmissionCapture {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(super) fn record(&self, decision: &ContextDecisionV1, counts: AdmissionCounts) {
        if let Some(envelope) = crate::core::task_spine::TaskSpine::current() {
            let mut task = self.task.lock().unwrap_or_else(PoisonError::into_inner);
            if task.is_none() {
                *task = Some((
                    envelope.task_id.as_str().to_owned(),
                    crate::core::context_store::task_scope(
                        envelope.tenant_id.as_ref(),
                        &envelope.project_id,
                    ),
                ));
            }
        }
        let incoming = Recorded {
            decision: decision.clone(),
            counts,
        };
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        if records.len() < MAX_DECISIONS {
            records.push(incoming);
            return;
        }
        // The receipt holds at most MAX_DECISIONS decisions. A notable one
        // (changed, withheld, flagged) displaces a clean one, so every
        // decision the receipt is about stays itemized; whatever is not
        // itemized still counts in full.
        let displaced = if incoming.is_clean() {
            incoming
        } else if let Some(position) = records.iter().position(Recorded::is_clean) {
            std::mem::replace(&mut records[position], incoming)
        } else {
            incoming
        };
        self.overflow
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .add(&displaced);
    }

    pub(super) fn bind_policy(&self, mode: GatewayModeV1, digest: Sha256Digest) {
        *self.policy.lock().unwrap_or_else(PoisonError::into_inner) = Some((mode, digest));
    }

    /// The tenant/project scope of the call's task, for its receipt index.
    pub(crate) fn task_scope(&self) -> Option<String> {
        self.task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|(_, scope)| scope.clone())
    }

    /// The measured original token count reported by the tool for this call.
    pub(crate) fn set_original_tokens(&self, tokens: u64) {
        *self
            .original_tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(tokens);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }

    /// Whether an admitted source carried prompt-injection signals; the
    /// dispatcher's own output scan must then not count the same text again.
    pub(crate) fn flagged_injection(&self) -> bool {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|record| record.counts.injection > 0)
    }

    /// The call's security actions, in the event model the status line and
    /// `lean-ctx value` prove from the audit trail. Only what actually
    /// happened counts: a detected-but-delivered secret is not "kept out".
    pub(crate) fn security_tally(&self) -> crate::core::security_events::SecurityCounts {
        let mut tally = self
            .overflow
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .security;
        for record in self
            .records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            tally.merge(&record.security());
        }
        tally
    }
}

impl Recorded {
    /// Delivered unchanged, nothing flagged, fully inspected.
    fn is_clean(&self) -> bool {
        self.decision.disposition == ContextDispositionV1::Allow
            && self.decision.reason_codes.is_empty()
    }

    /// This decision's security actions. Only what actually happened counts:
    /// a detected-but-delivered secret is not "kept out".
    fn security(&self) -> crate::core::security_events::SecurityCounts {
        use crate::core::security_events::{SecurityCounts, SecurityKind};
        let mut tally = SecurityCounts::default();
        let has = |code: &str| {
            self.decision
                .reason_codes
                .iter()
                .any(|reason| reason.as_str() == code)
        };
        if has("secret.redacted") {
            tally.add(SecurityKind::SecretRedacted, u64::from(self.counts.secrets));
        }
        if has("pii.redacted") {
            tally.add(SecurityKind::PiiRedacted, u64::from(self.counts.pii));
        }
        if self.counts.injection > 0 {
            tally.add(
                SecurityKind::InjectionFlagged,
                u64::from(self.counts.injection),
            );
        }
        if self.decision.disposition == ContextDispositionV1::Deny {
            tally.add(SecurityKind::ContentWithheld, 1);
        } else if self.counts.incomplete > 0 {
            tally.add(SecurityKind::CoverageIncomplete, 1);
        }
        tally
    }

    fn redactions(&self) -> u32 {
        if self.decision.disposition == ContextDispositionV1::AllowRedacted {
            self.counts.secrets.saturating_add(self.counts.pii)
        } else {
            0
        }
    }
}

/// Everything about the decisions a receipt counts but does not itemize.
#[derive(Debug, Default)]
struct Overflow {
    objects: u32,
    permitted: u32,
    blocked: u32,
    redactions: u32,
    injection: u32,
    incomplete: u32,
    security: crate::core::security_events::SecurityCounts,
}

impl Overflow {
    fn add(&mut self, record: &Recorded) {
        let disposition = record.decision.disposition;
        self.objects = self.objects.saturating_add(1);
        if disposition.delivers_content() {
            self.permitted = self.permitted.saturating_add(1);
        }
        if disposition == ContextDispositionV1::Deny {
            self.blocked = self.blocked.saturating_add(1);
        }
        self.redactions = self.redactions.saturating_add(record.redactions());
        self.injection = self.injection.saturating_add(record.counts.injection);
        if record.counts.incomplete > 0 {
            self.incomplete = self.incomplete.saturating_add(1);
        }
        self.security.merge(&record.security());
    }
}

/// Identity of the call the receipt describes.
pub(crate) struct CallIdentity<'a> {
    pub(crate) agent_id: Option<&'a str>,
    /// Where the context goes, when the caller knows it (the proxy does).
    pub(crate) destination: Option<DestinationV1>,
}

/// The delivered result as the host receives it.
pub(crate) struct Delivered<'a> {
    pub(crate) text: &'a str,
    pub(crate) is_error: bool,
    pub(crate) tokens: u64,
}

fn reference(prefix: &str, raw: &str) -> Option<ProtocolReference> {
    // Opaque references must stay bounded and printable; anything else is
    // reduced to a digest rather than guessed at.
    ProtocolReference::new(format!("{prefix}{raw}"))
        .ok()
        .or_else(|| {
            let digest = blake3::hash(raw.as_bytes()).to_hex();
            ProtocolReference::new(format!("{prefix}{}", &digest[..32])).ok()
        })
}

fn sha256(bytes: &[u8]) -> Sha256Digest {
    use sha2::{Digest, Sha256};
    let hex = crate::core::agent_identity::hex_encode(&Sha256::digest(bytes));
    Sha256Digest::new(format!("sha256:{hex}")).expect("SHA-256 digest is canonical")
}

fn saturating_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Build the call's receipt, or `None` when no governed source was admitted.
/// The receipt is validated before it is returned; an inconsistent receipt is
/// never produced.
pub(crate) fn finish(
    capture: &AdmissionCapture,
    identity: &CallIdentity<'_>,
    delivered: &Delivered<'_>,
) -> Option<ContextDecisionReceiptV1> {
    let records = capture
        .records
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if records.is_empty() {
        return None;
    }
    let overflow = capture
        .overflow
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let (mode, policy_digest) = capture
        .policy
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()?;

    let decisions: Vec<ContextDecisionV1> = records
        .iter()
        .map(|record| record.decision.clone())
        .collect();
    let count = |disposition: ContextDispositionV1| {
        decisions
            .iter()
            .filter(|decision| decision.disposition == disposition)
            .count()
    };
    // `blocked_objects`/`quarantined_objects` must equal the itemized
    // decisions (contract); every other count includes what was not
    // itemized, so a large request is described in full.
    let blocked = count(ContextDispositionV1::Deny);
    let quarantined = count(ContextDispositionV1::Quarantine);
    let permitted = saturating_u32(
        decisions
            .iter()
            .filter(|decision| decision.disposition.delivers_content())
            .count(),
    )
    .saturating_add(overflow.permitted);
    let redactions: u32 = records
        .iter()
        .map(Recorded::redactions)
        .fold(overflow.redactions, u32::saturating_add);
    let injection: u32 = records
        .iter()
        .map(|record| record.counts.injection)
        .fold(overflow.injection, u32::saturating_add);
    let incomplete = saturating_u32(
        records
            .iter()
            .filter(|record| record.counts.incomplete > 0)
            .count(),
    )
    .saturating_add(overflow.incomplete);

    let outcome = if permitted == 0 {
        DeliveryOutcomeV1::Withheld
    } else if delivered.is_error {
        DeliveryOutcomeV1::Failed
    } else {
        DeliveryOutcomeV1::Delivered
    };
    let final_context =
        (outcome == DeliveryOutcomeV1::Delivered).then(|| sha256(delivered.text.as_bytes()));
    let principal = identity
        .agent_id
        .and_then(|agent| reference("agent:", agent))
        .and_then(|id| PrincipalV1::new(PrincipalKindV1::Agent, id).ok())
        .unwrap_or_else(PrincipalV1::unknown);
    let receipt_seed = format!(
        "{}|{}|{}|{:?}",
        policy_digest.as_str(),
        decisions
            .iter()
            .map(|decision| decision.object.as_str())
            .collect::<Vec<_>>()
            .join(","),
        final_context.as_ref().map_or("", Sha256Digest::as_str),
        capture.started
    );
    let receipt = ContextDecisionReceiptV1 {
        schema_version: V1_SCHEMA_VERSION,
        receipt_id: reference(
            "rcpt-",
            &blake3::hash(receipt_seed.as_bytes()).to_hex()[..32],
        )?,
        mode,
        principal,
        task: capture
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(|(task, _)| TaskId::new(task.as_str()).ok()),
        // An MCP call serves the host; where the host sends the context is
        // not known there, so locality is honestly unknown. The proxy names
        // the real model destination (G6).
        destination: identity
            .destination
            .clone()
            .unwrap_or_else(|| DestinationV1 {
                provider: ProtocolReference::new("mcp-host").expect("valid provider ref"),
                model: None,
                locality: DestinationLocalityV1::Unknown,
                organization_managed: false,
                account_ref: None,
                region: None,
            }),
        policy: Some(PolicyRefV1 {
            id: PolicyId::new("context-gateway").expect("valid policy id"),
            version: None,
            digest: policy_digest,
        }),
        sources: SourceCountsV1 {
            inspected: saturating_u32(decisions.len()).saturating_add(overflow.objects),
            permitted,
            selected: permitted,
            blocked: saturating_u32(blocked).saturating_add(overflow.blocked),
        },
        decisions,
        security: SecurityCountsV1 {
            redactions,
            blocked_objects: saturating_u32(blocked),
            quarantined_objects: saturating_u32(quarantined),
            injection_signals: injection,
            incomplete_coverage: incomplete,
        },
        tokens: TokenAccountV1 {
            // The tool's own tokenizer measurement of its sources; a tool that
            // reports none delivered its sources as they were.
            original: capture
                .original_tokens
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .filter(|tokens| *tokens > 0)
                .unwrap_or(delivered.tokens),
            delivered: delivered.tokens,
        },
        final_context,
        outcome,
        duration_us: u64::try_from(capture.started.elapsed().as_micros()).unwrap_or(u64::MAX),
        quality: None,
    };
    receipt.validate().ok()?;
    Some(receipt)
}
