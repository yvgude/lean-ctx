//! Local reference transport for authenticated OCLA A2A capsule delivery (P11).
//!
//! In-process delivery queue that accepts signed, payload-free capsule manifests.
//! Bytes are measured exactly after serialization; the token count is a local
//! tokenizer proxy. This is delivery evidence only — never convertible into
//! compression savings or provider billing.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::core::context_capsule::SignedContextCapsuleV1;

const MAX_INBOX_SIZE: usize = 128;
const MAX_DELIVERY_ATTEMPTS: u8 = 3;
const RESERVATION_TTL: Duration = Duration::from_mins(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveredTokenCountKindV1 {
    LocalTokenizerProxy,
}

/// Delivery receipt measured after a concrete local queue write.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentRelayDeliveryV1 {
    pub relay_id: String,
    pub capsule_ref: String,
    pub recipient_agent_id: String,
    pub delivered_bytes: u64,
    pub delivered_tokens: u64,
    pub token_count_kind: DeliveredTokenCountKindV1,
}

#[derive(Clone)]
struct QueuedCapsule {
    signed_json: String,
    delivery: AgentRelayDeliveryV1,
    reservation_id: Option<String>,
    reserved_at: Option<Instant>,
    delivery_attempts: u8,
}

/// Bounded local transport. Deployments must place a durable, authenticated
/// transport behind the same receipt semantics before remote use.
pub struct LocalSignedCapsuleTransport {
    inboxes: Mutex<BTreeMap<String, VecDeque<QueuedCapsule>>>,
    dead_letters: Mutex<BTreeMap<String, VecDeque<QueuedCapsule>>>,
}

impl Default for LocalSignedCapsuleTransport {
    fn default() -> Self {
        Self {
            inboxes: Mutex::new(BTreeMap::new()),
            dead_letters: Mutex::new(BTreeMap::new()),
        }
    }
}

impl LocalSignedCapsuleTransport {
    /// Deliver a signed capsule to a recipient's inbox.
    pub fn deliver(
        &self,
        signed: &SignedContextCapsuleV1,
        recipient_agent_id: &str,
    ) -> Result<AgentRelayDeliveryV1, TransportError> {
        signed
            .validate_structure()
            .map_err(|e| TransportError::Validation(e.to_string()))?;

        let envelope = signed
            .capsule
            .agent_envelope(recipient_agent_id)
            .map_err(|e| TransportError::Validation(e.to_string()))?;

        let signed_json = serde_json::to_string(signed)
            .map_err(|e| TransportError::Serialization(e.to_string()))?;

        let delivered_bytes = u64::try_from(signed_json.len()).unwrap_or(u64::MAX);
        let delivered_tokens = estimate_tokens(&signed_json);

        let delivery = AgentRelayDeliveryV1 {
            relay_id: envelope.relay_id.clone(),
            capsule_ref: envelope.capsule_ref,
            recipient_agent_id: recipient_agent_id.to_string(),
            delivered_bytes,
            delivered_tokens,
            token_count_kind: DeliveredTokenCountKindV1::LocalTokenizerProxy,
        };

        let mut inboxes = self
            .inboxes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let inbox = inboxes.entry(recipient_agent_id.to_string()).or_default();

        let dead_letter_depth = self
            .dead_letters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(recipient_agent_id)
            .map_or(0, VecDeque::len);
        if inbox.len().saturating_add(dead_letter_depth) >= MAX_INBOX_SIZE {
            return Err(TransportError::Capacity);
        }

        inbox.push_back(QueuedCapsule {
            signed_json,
            delivery: delivery.clone(),
            reservation_id: None,
            reserved_at: None,
            delivery_attempts: 0,
        });

        Ok(delivery)
    }

    /// Receive the next capsule from an agent's inbox.
    pub fn receive(
        &self,
        agent_id: &str,
    ) -> Option<(SignedContextCapsuleV1, AgentRelayDeliveryV1)> {
        let mut inboxes = self
            .inboxes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let inbox = inboxes.get_mut(agent_id)?;
        if inbox.front()?.reservation_id.is_some() {
            return None;
        }
        let queued = inbox.pop_front()?;
        let signed: SignedContextCapsuleV1 = serde_json::from_str(&queued.signed_json).ok()?;
        Some((signed, queued.delivery))
    }

    /// Exclusively reserve the next capsule until acknowledged or negatively acknowledged.
    pub fn reserve(
        &self,
        agent_id: &str,
        reservation_id: &str,
    ) -> Result<Option<(SignedContextCapsuleV1, AgentRelayDeliveryV1)>, TransportError> {
        self.reserve_at(agent_id, reservation_id, Instant::now())
    }

    fn reserve_at(
        &self,
        agent_id: &str,
        reservation_id: &str,
        now: Instant,
    ) -> Result<Option<(SignedContextCapsuleV1, AgentRelayDeliveryV1)>, TransportError> {
        if reservation_id.is_empty() || reservation_id.len() > 256 {
            return Err(TransportError::Validation("invalid reservation id".into()));
        }
        let mut inboxes = self
            .inboxes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(inbox) = inboxes.get_mut(agent_id) else {
            return Ok(None);
        };
        let expired_exhausted = inbox.front().is_some_and(|front| {
            front.delivery_attempts >= MAX_DELIVERY_ATTEMPTS
                && front.reserved_at.is_some_and(|reserved_at| {
                    now.saturating_duration_since(reserved_at) >= RESERVATION_TTL
                })
        });
        if expired_exhausted {
            let exhausted = inbox.pop_front().expect("front checked above");
            self.dead_letters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(agent_id.to_string())
                .or_default()
                .push_back(exhausted);
            return Ok(None);
        }
        let Some(front) = inbox.front_mut() else {
            return Ok(None);
        };
        if front.reserved_at.is_some_and(|reserved_at| {
            now.saturating_duration_since(reserved_at) >= RESERVATION_TTL
        }) {
            front.reservation_id = None;
            front.reserved_at = None;
        }
        if front.reservation_id.is_some() {
            return Ok(None);
        }
        let signed: SignedContextCapsuleV1 = match serde_json::from_str(&front.signed_json) {
            Ok(signed) => signed,
            Err(error) => {
                let poisoned = inbox.pop_front().expect("front checked above");
                self.dead_letters
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .entry(agent_id.to_string())
                    .or_default()
                    .push_back(poisoned);
                return Err(TransportError::Serialization(error.to_string()));
            }
        };
        front.reservation_id = Some(reservation_id.to_string());
        front.reserved_at = Some(now);
        front.delivery_attempts = front.delivery_attempts.saturating_add(1);
        Ok(Some((signed, front.delivery.clone())))
    }

    pub fn acknowledge(&self, agent_id: &str, relay_id: &str, reservation_id: &str) -> bool {
        let mut inboxes = self
            .inboxes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(inbox) = inboxes.get_mut(agent_id) else {
            return false;
        };
        if inbox.front().is_some_and(|queued| {
            queued.delivery.relay_id == relay_id
                && queued.reservation_id.as_deref() == Some(reservation_id)
                && queued.reserved_at.is_some_and(|reserved_at| {
                    Instant::now().saturating_duration_since(reserved_at) < RESERVATION_TTL
                })
        }) {
            inbox.pop_front();
            return true;
        }
        false
    }

    /// Release a failed reservation; terminal retries move to a bounded dead-letter queue.
    pub fn negative_acknowledge(
        &self,
        agent_id: &str,
        relay_id: &str,
        reservation_id: &str,
    ) -> bool {
        let mut inboxes = self
            .inboxes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(inbox) = inboxes.get_mut(agent_id) else {
            return false;
        };
        let matches = inbox.front().is_some_and(|queued| {
            queued.delivery.relay_id == relay_id
                && queued.reservation_id.as_deref() == Some(reservation_id)
                && queued.reserved_at.is_some_and(|reserved_at| {
                    Instant::now().saturating_duration_since(reserved_at) < RESERVATION_TTL
                })
        });
        if !matches {
            return false;
        }
        if inbox
            .front()
            .is_some_and(|queued| queued.delivery_attempts >= MAX_DELIVERY_ATTEMPTS)
        {
            let exhausted = inbox.pop_front().expect("front checked above");
            self.dead_letters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(agent_id.to_string())
                .or_default()
                .push_back(exhausted);
        } else if let Some(front) = inbox.front_mut() {
            front.reservation_id = None;
            front.reserved_at = None;
        }
        true
    }

    pub fn dead_letter_depth(&self, agent_id: &str) -> usize {
        self.dead_letters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(agent_id)
            .map_or(0, VecDeque::len)
    }

    /// Quarantine a delivery whose committed consumer lost its ACK invariant.
    pub fn quarantine_reserved(
        &self,
        agent_id: &str,
        relay_id: &str,
        reservation_id: &str,
    ) -> bool {
        let mut inboxes = self
            .inboxes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(inbox) = inboxes.get_mut(agent_id) else {
            return false;
        };
        let Some(index) = inbox.iter().position(|queued| {
            queued.delivery.relay_id == relay_id
                && queued.reservation_id.as_deref() == Some(reservation_id)
        }) else {
            return false;
        };
        let Some(queued) = inbox.remove(index) else {
            return false;
        };
        self.dead_letters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(agent_id.to_string())
            .or_default()
            .push_back(queued);
        true
    }

    /// Peek at inbox depth for a given agent.
    pub fn inbox_depth(&self, agent_id: &str) -> usize {
        let inboxes = self
            .inboxes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inboxes.get(agent_id).map_or(0, VecDeque::len)
    }
}

/// Simple token estimation: ~4 chars per token (conservative for JSON).
fn estimate_tokens(text: &str) -> u64 {
    u64::try_from(text.len() / 4).unwrap_or(u64::MAX).max(1)
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("serialization failed: {0}")]
    Serialization(String),
    #[error("recipient capsule queue is at capacity")]
    Capacity,
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_capsule::*;

    fn test_signed_capsule() -> SignedContextCapsuleV1 {
        let mut capsule = ContextCapsuleV1 {
            schema_version: CONTEXT_CAPSULE_SCHEMA_VERSION,
            capsule_id: "capsule:pending".to_string(),
            request_id: "request:1".to_string(),
            session_id: "session:1".to_string(),
            agent_id: "sender-agent".to_string(),
            intent_ref: "intent:test".to_string(),
            task_ref: "task:1".to_string(),
            expected_outcome_ref: "outcome:pass".to_string(),
            acceptance_criteria_refs: vec!["criteria:ci-green".to_string()],
            references: vec![ContextCapsuleReferenceV1 {
                kind: CapsuleReferenceKindV1::File,
                content_ref: "blake3:file1".to_string(),
                freshness_ref: "freshness:1".to_string(),
                recovery_ref: None,
            }],
            finding_refs: vec![],
            decision_refs: vec![],
            uncertainty_refs: vec![],
            negative_result_refs: vec![],
            source_ref: "source:test".to_string(),
            policy_ref: "policy:default".to_string(),
            contract_ref: "contract:v1".to_string(),
            freshness_ref: "freshness:now".to_string(),
            sensitivity: CapsuleSensitivityV1::Internal,
            allowed_agent_ids: vec!["recipient-agent".to_string()],
            budget: ContextCapsuleBudgetV1 {
                tokens_used: 50,
                tokens_remaining: 950,
                cost_micros_used: 5,
                cost_micros_remaining: 95,
                latency_ms_used: 10,
                latency_ms_remaining: 90,
            },
            chain: ContextCapsuleChainV1 {
                chain_id: "chain:test".to_string(),
                parent_capsule_ref: None,
                owner_agent_id: "sender-agent".to_string(),
                attribution_ref: "attribution:1".to_string(),
                hop: 0,
            },
            quality_signal_refs: vec![],
            recovery_refs: vec![],
            delta_from: None,
        };
        capsule.assign_capsule_id().unwrap();
        let keypair = ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]);
        SignedContextCapsuleV1::sign(&capsule, &keypair).unwrap()
    }

    #[test]
    fn deliver_and_receive_roundtrip() {
        let transport = LocalSignedCapsuleTransport::default();
        let signed = test_signed_capsule();

        let delivery = transport.deliver(&signed, "recipient-agent").unwrap();
        assert!(delivery.delivered_bytes > 0);
        assert!(delivery.delivered_tokens > 0);
        assert_eq!(delivery.recipient_agent_id, "recipient-agent");
        assert_eq!(transport.inbox_depth("recipient-agent"), 1);

        let (received, receipt) = transport.receive("recipient-agent").unwrap();
        assert_eq!(received.capsule.capsule_id, signed.capsule.capsule_id);
        assert_eq!(receipt.relay_id, delivery.relay_id);
        assert_eq!(transport.inbox_depth("recipient-agent"), 0);
    }

    #[test]
    fn receive_from_empty_inbox_returns_none() {
        let transport = LocalSignedCapsuleTransport::default();
        assert!(transport.receive("nobody").is_none());
    }

    #[test]
    fn rejects_unauthorized_recipient() {
        let transport = LocalSignedCapsuleTransport::default();
        let signed = test_signed_capsule();
        assert!(transport.deliver(&signed, "unauthorized-agent").is_err());
    }

    #[test]
    fn reservation_is_exclusive_and_ack_is_fenced() {
        let transport = LocalSignedCapsuleTransport::default();
        let delivery = transport
            .deliver(&test_signed_capsule(), "recipient-agent")
            .unwrap();
        assert!(
            transport
                .reserve("recipient-agent", "worker:1")
                .unwrap()
                .is_some()
        );
        assert!(
            transport
                .reserve("recipient-agent", "worker:1")
                .unwrap()
                .is_none()
        );
        assert!(
            transport
                .reserve("recipient-agent", "worker:2")
                .unwrap()
                .is_none()
        );
        assert!(!transport.acknowledge("recipient-agent", &delivery.relay_id, "worker:2"));
        assert!(transport.acknowledge("recipient-agent", &delivery.relay_id, "worker:1"));
        assert_eq!(transport.inbox_depth("recipient-agent"), 0);
    }

    #[test]
    fn expired_reservation_is_recoverable_with_a_new_fence() {
        let transport = LocalSignedCapsuleTransport::default();
        let delivery = transport
            .deliver(&test_signed_capsule(), "recipient-agent")
            .unwrap();
        let now = Instant::now();
        assert!(
            transport
                .reserve_at("recipient-agent", "worker:stale", now)
                .unwrap()
                .is_some()
        );
        assert!(
            transport
                .reserve_at("recipient-agent", "worker:fresh", now + RESERVATION_TTL)
                .unwrap()
                .is_some()
        );
        assert!(!transport.quarantine_reserved(
            "recipient-agent",
            &delivery.relay_id,
            "worker:stale"
        ));
        assert!(transport.acknowledge("recipient-agent", &delivery.relay_id, "worker:fresh"));
    }

    #[test]
    fn legacy_receive_cannot_steal_a_reserved_capsule() {
        let transport = LocalSignedCapsuleTransport::default();
        let delivery = transport
            .deliver(&test_signed_capsule(), "recipient-agent")
            .unwrap();
        transport
            .reserve("recipient-agent", "worker:1")
            .unwrap()
            .unwrap();
        assert!(transport.receive("recipient-agent").is_none());
        assert!(transport.quarantine_reserved("recipient-agent", &delivery.relay_id, "worker:1"));
        assert_eq!(transport.inbox_depth("recipient-agent"), 0);
        assert_eq!(transport.dead_letter_depth("recipient-agent"), 1);
    }

    #[test]
    fn exhausted_negative_ack_moves_capsule_to_dead_letter_queue() {
        let transport = LocalSignedCapsuleTransport::default();
        let delivery = transport
            .deliver(&test_signed_capsule(), "recipient-agent")
            .unwrap();
        for attempt in 1..=MAX_DELIVERY_ATTEMPTS {
            let reservation = format!("worker:{attempt}");
            assert!(
                transport
                    .reserve("recipient-agent", &reservation)
                    .unwrap()
                    .is_some()
            );
            assert!(transport.negative_acknowledge(
                "recipient-agent",
                &delivery.relay_id,
                &reservation
            ));
        }
        assert_eq!(transport.inbox_depth("recipient-agent"), 0);
        assert_eq!(transport.dead_letter_depth("recipient-agent"), 1);
    }

    #[test]
    fn malformed_front_is_quarantined_instead_of_poisoning_inbox() {
        let transport = LocalSignedCapsuleTransport::default();
        transport
            .deliver(&test_signed_capsule(), "recipient-agent")
            .unwrap();
        transport
            .inboxes
            .lock()
            .unwrap()
            .get_mut("recipient-agent")
            .unwrap()
            .front_mut()
            .unwrap()
            .signed_json = "{".into();

        assert!(transport.reserve("recipient-agent", "worker:1").is_err());
        assert_eq!(transport.inbox_depth("recipient-agent"), 0);
        assert_eq!(transport.dead_letter_depth("recipient-agent"), 1);
    }

    #[test]
    fn full_inbox_rejects_delivery_without_evicting_existing_capsules() {
        let transport = LocalSignedCapsuleTransport::default();
        let signed = test_signed_capsule();
        for _ in 0..MAX_INBOX_SIZE {
            transport.deliver(&signed, "recipient-agent").unwrap();
        }
        assert!(matches!(
            transport.deliver(&signed, "recipient-agent"),
            Err(TransportError::Capacity)
        ));
        assert_eq!(transport.inbox_depth("recipient-agent"), MAX_INBOX_SIZE);
    }
}
