// SPDX-License-Identifier: Apache-2.0
//! Pushing own events to a place (E5 `POST /v1/places/{p}/events`,
//! SPEC-ENGINE §3.4.4, C0 2.0 `push_request` / `push_response`).
//!
//! A batch is not atomic: the server decides every event on its own. The
//! engine keeps an event pending until the tail delivers it (the server log
//! is the only source of truth); what a push answers only decides whether
//! the event is sent again, waits, or moves to the failed queue.

use serde_json::{Value, json};

use crate::core::hive::capture::CaptureCounts;

/// Events per push.
pub const MAX_BATCH: usize = 200;

/// The E5 request body. Counts go along only with a complete window.
pub fn request(events: &[Value], counts: Option<&CaptureCounts>, purge_acks: &[Value]) -> Value {
    let mut body = json!({ "events": events });
    if !purge_acks.is_empty() {
        body["purge_acks"] = Value::Array(purge_acks.to_vec());
    }
    if let Some(c) = counts
        && let (Some(from), Some(to)) = (c.window_from, c.window_to)
    {
        body["capture_counts"] = json!({
            "gate_version": c.gate_version,
            "window_from": from,
            "window_to": to,
            "considered": c.considered,
            "captured": c.captured,
            "rejected": c.rejected,
        });
    }
    body
}

/// What the server decided for one pushed event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// In the log at `seq`; the tail will deliver it.
    Accepted { seq: u64 },
    /// Already in the log (an earlier push got through).
    Duplicate { seq: u64 },
    /// Waiting for review; stays pending ("held for review").
    Held { review_id: Option<String> },
    /// Not taken; what the engine does next is [`Next`].
    Refused {
        code: String,
        reason: Option<String>,
        /// JSON Pointer of the first failing member (`invalid_event` with
        /// reason `shape`), e.g. `/body/statement`.
        field: Option<String>,
        retry_after_s: Option<u64>,
    },
}

/// What the engine does with a refused event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Stamp the HLC again and push once more (`hlc_skew`).
    Restamp,
    /// Keep it queued and retry after the server's delay.
    RetryLater,
    /// Move it to `outbox/failed/events`; nothing retries it.
    Fail,
}

impl Outcome {
    /// Whether the event is in the server log now.
    pub fn logged(&self) -> bool {
        matches!(self, Self::Accepted { .. } | Self::Duplicate { .. })
    }

    pub fn next(&self) -> Option<Next> {
        let Self::Refused { code, .. } = self else {
            return None;
        };
        Some(match code.as_str() {
            "hlc_skew" => Next::Restamp,
            "rate_limited"
            | "retract_rate_limited"
            | "keystore_unavailable"
            | "segment_unavailable"
            | "vault_migrating"
            | "conversion_pending" => Next::RetryLater,
            _ => Next::Fail,
        })
    }
}

/// The per-event results of an E5 response, by event id, and the place
/// epoch it reports.
pub fn results(response: &Value) -> Result<(Vec<(String, Outcome)>, Option<u64>), String> {
    let list = response
        .get("results")
        .and_then(Value::as_array)
        .ok_or("push response without results")?;
    let mut out = Vec::with_capacity(list.len());
    for r in list {
        let id = r
            .get("event_id")
            .and_then(Value::as_str)
            .ok_or("push result without event_id")?
            .to_string();
        let seq = r.get("seq").and_then(Value::as_u64);
        let s = |k: &str| r.get(k).and_then(Value::as_str).map(str::to_string);
        let outcome = match r.get("status").and_then(Value::as_str) {
            Some("accepted") => Outcome::Accepted {
                seq: seq.ok_or("accepted without seq")?,
            },
            Some("duplicate") => Outcome::Duplicate {
                seq: seq.ok_or("duplicate without seq")?,
            },
            Some("held") => Outcome::Held {
                review_id: s("review_id"),
            },
            Some("refused") => Outcome::Refused {
                code: s("code").ok_or("refused without code")?,
                reason: s("reason"),
                field: s("field"),
                retry_after_s: r.get("retry_after_s").and_then(Value::as_u64),
            },
            other => return Err(format!("push result status {other:?}")),
        };
        out.push((id, outcome));
    }
    Ok((out, response.get("epoch").and_then(Value::as_u64)))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn fixture(rel: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2/routes")
            .join(rel);
        serde_json::from_slice(&std::fs::read(path).expect("fixture")).expect("json")
    }

    #[test]
    fn the_request_matches_the_contract_shape() {
        let fx = fixture("events_push.request.json");
        let c = &fx["capture_counts"];
        let mut counts = CaptureCounts {
            gate_version: 1,
            window_from: c["window_from"].as_i64(),
            window_to: c["window_to"].as_i64(),
            considered: 14,
            captured: 3,
            ..CaptureCounts::default()
        };
        for _ in 0..5 {
            counts.reject(crate::core::hive::capture::ReasonCode::Duplicate);
        }
        for _ in 0..6 {
            counts.reject(crate::core::hive::capture::ReasonCode::TooShort);
        }
        let events = fx["events"].as_array().unwrap().clone();
        assert_eq!(request(&events, Some(&counts), &[]), fx);
        let bare = request(&events, None, &[]);
        assert!(bare.get("capture_counts").is_none());

        let acked = fixture("events_push.purge_acks.request.json");
        let acks = acked["purge_acks"].as_array().unwrap().clone();
        let events = acked["events"].as_array().unwrap().clone();
        assert_eq!(request(&events, None, &acks), acked);
    }

    #[test]
    fn every_result_kind_is_read() {
        let (ok, epoch) = results(&fixture("events_push.response.json")).unwrap();
        assert_eq!(epoch, Some(0));
        assert_eq!(ok[0].1, Outcome::Accepted { seq: 9 });
        assert_eq!(ok[2].1, Outcome::Duplicate { seq: 8 });
        assert!(ok.iter().all(|(_, o)| o.logged() && o.next().is_none()));

        let (limited, _) = results(&fixture(
            "events_push.refused.retract_rate_limited.response.json",
        ))
        .unwrap();
        assert_eq!(
            limited[1].1,
            Outcome::Refused {
                code: "retract_rate_limited".into(),
                reason: None,
                field: None,
                retry_after_s: Some(1260)
            }
        );
        assert_eq!(limited[1].1.next(), Some(Next::RetryLater));
        assert!(limited[2].1.logged(), "the batch continues after a refusal");

        let (invalid, _) =
            results(&fixture("events_push.refused.target_state.response.json")).unwrap();
        assert!(invalid.iter().all(|(_, o)| o.next() == Some(Next::Fail)));

        let (held, _) = results(&fixture("events_push.refused.held.response.json")).unwrap();
        assert!(matches!(&held[0].1, Outcome::Held { review_id: Some(_) }));
        assert!(!held[0].1.logged());

        let (shape, _) = results(&fixture("events_push.refused.shape.response.json")).unwrap();
        assert!(shape.iter().any(|(_, o)| matches!(
            o,
            Outcome::Refused { reason: Some(r), field: Some(f), .. } if r == "shape" && f.starts_with('/')
        )));
        assert!(
            shape
                .iter()
                .all(|(_, o)| o.logged() || o.next() == Some(Next::Fail))
        );

        let skew = Outcome::Refused {
            code: "hlc_skew".into(),
            reason: None,
            field: None,
            retry_after_s: None,
        };
        assert_eq!(skew.next(), Some(Next::Restamp));
    }
}
