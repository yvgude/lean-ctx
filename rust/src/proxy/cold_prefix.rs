//! Big-gap cold-prefix repack prediction (#480).
//!
//! The proxy is deliberately cache-safe: it never rewrites the client-cached
//! prefix (`history_prune::cached_prefix_len`), so provider prompt caches keep
//! hitting and cheap cache reads (~0.1x) never turn into full-price writes
//! (~1.25x) — the #448 invariant.
//!
//! That protection has one blind spot. Provider prompt caches EXPIRE after a TTL
//! of inactivity. After a long idle gap (the agent asked a question, the user
//! replies hours later) the cached prefix is already gone; the provider will
//! re-WRITE the whole prefix on the next request regardless. Staying in
//! "never touch the cached prefix" mode then writes the *uncompressed* prefix at
//! full price and re-seeds a fat cache for the rest of the session.
//!
//! This module makes a PRE-SEND prediction — purely from elapsed idle time vs
//! the provider's cache TTL — of whether the prefix is already cold. The trigger
//! must be a clock, not response feedback: hit/miss is only known *after* the
//! request that already (re-)cached the prefix, and by the next request the
//! cache is warm again, so feedback would bust the fresh cache.
//!
//! Safety is paramount because the cost of a wrong "cold" guess is asymmetric (a
//! cache write is ~12x a cache read). We therefore:
//!   * act only when the caller opted in (`repacks_cold_prefix()`),
//!   * act only on a measured idle gap well past expiry (`TTL × SAFETY_MARGIN`,
//!     with an absolute floor), skipping the ambiguous near-TTL zone entirely,
//!   * never act without a prior touch (the first sighting only sets a baseline),
//!   * and bias every ambiguity toward "warm" (do nothing).
//!
//! State is in-memory only (a per-conversation last-touch map). A proxy restart
//! during the idle gap loses the baseline and simply disables the optimization
//! for that conversation — a safe degradation that can never wrongly trigger.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// Multiplier applied to the resolved TTL before a prefix is declared cold. The
/// provider cache is a sliding inactivity window, so `idle > TTL` already implies
/// expiry; `× 2` keeps a safety buffer against clock skew and provider nuance.
const SAFETY_MARGIN: u64 = 2;
/// Absolute minimum idle (seconds) before any repack, regardless of a short
/// per-request TTL — never repack on a gap under 10 minutes.
const COLD_FLOOR_SECS: u64 = 600;
/// Anthropic default cache TTL when a `cache_control` marker carries no explicit
/// `ttl` (the API default is "5m").
const DEFAULT_TTL_SECS: u64 = 300;
/// Anthropic extended cache TTL (`"ttl":"1h"`).
const HOUR_TTL_SECS: u64 = 3600;
/// Hard cap on tracked conversations so a long-lived proxy can't grow the
/// last-touch map without bound; the oldest entry is evicted past this.
const MAX_TRACKED: usize = 4096;

fn store() -> &'static Mutex<HashMap<u64, u64>> {
    static STORE: OnceLock<Mutex<HashMap<u64, u64>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Stable per-conversation key: a hash of the first message. `messages[0]` never
/// changes across a conversation's turns (cache-aware keeps the head byte-stable)
/// yet differs between conversations (distinct opening turns). A collision can
/// only make the recorded last-touch *more recent* (either conversation refreshes
/// it), biasing toward "warm" — never toward a wrong "cold". Safe by
/// construction. `None` when there is no first message.
fn conversation_key(messages: &[Value]) -> Option<u64> {
    let first = messages.first()?;
    let bytes = serde_json::to_vec(first).ok()?;
    Some(hash_bytes(&bytes))
}

fn parse_ttl_str(s: &str) -> Option<u64> {
    match s.trim() {
        "1h" => Some(HOUR_TTL_SECS),
        "5m" => Some(DEFAULT_TTL_SECS),
        _ => None,
    }
}

/// Largest `cache_control.ttl` declared anywhere inside one message (message-,
/// block-, or nested text-level), in seconds. `None` when no parseable ttl.
fn max_ttl_in_message(msg: &Value) -> Option<u64> {
    let mut best: Option<u64> = None;
    collect_cc_ttl(msg, &mut best);
    best
}

fn collect_cc_ttl(v: &Value, best: &mut Option<u64>) {
    match v {
        Value::Object(map) => {
            if let Some(ttl) = map
                .get("cache_control")
                .and_then(|cc| cc.get("ttl"))
                .and_then(Value::as_str)
                .and_then(parse_ttl_str)
            {
                *best = Some(best.map_or(ttl, |b| b.max(ttl)));
            }
            for val in map.values() {
                collect_cc_ttl(val, best);
            }
        }
        Value::Array(arr) => {
            for val in arr {
                collect_cc_ttl(val, best);
            }
        }
        _ => {}
    }
}

/// Resolve the cache TTL (seconds) for the client-cached prefix `[0..cached)`.
/// Returns the largest TTL any cached message requested, defaulting to the "5m"
/// Anthropic default because a `cache_control` marker is present. `None` only
/// when `cached == 0` (no marker) — in which case there is nothing to repack.
fn resolved_ttl_secs(messages: &[Value], cached: usize) -> Option<u64> {
    if cached == 0 {
        return None;
    }
    let end = cached.min(messages.len());
    let mut ttl = DEFAULT_TTL_SECS;
    for msg in &messages[..end] {
        if let Some(t) = max_ttl_in_message(msg) {
            ttl = ttl.max(t);
        }
    }
    Some(ttl)
}

fn evict_oldest(map: &mut HashMap<u64, u64>) {
    if let Some(oldest_key) = map.iter().min_by_key(|(_, t)| **t).map(|(k, _)| *k) {
        map.remove(&oldest_key);
    }
}

/// Decide whether to repack the (predicted-cold) cached prefix for THIS request,
/// recording this request as the conversation's latest touch.
///
/// Returns `true` only when every condition holds: the request carries a
/// client-cached prefix (`cached > 0`), a prior touch exists (so the idle gap is
/// measurable), and the idle gap exceeds `TTL × SAFETY_MARGIN` and the absolute
/// floor. The caller is responsible for the opt-in gate; this is only ever called
/// when the operator enabled it, so updating the last-touch baseline here is the
/// intended side effect for the *next* request.
pub fn repack_decision(messages: &[Value], cached: usize) -> bool {
    let Some(key) = conversation_key(messages) else {
        return false;
    };
    let now = now_secs();

    let prev = {
        let mut map = store()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prev = map.insert(key, now);
        if map.len() > MAX_TRACKED {
            evict_oldest(&mut map);
        }
        prev
    };

    // First sighting: only establish a baseline, never repack.
    let Some(prev) = prev else {
        return false;
    };
    // No client-cached prefix → nothing to repack (and pruning already starts at
    // 0 for this provider).
    let Some(ttl) = resolved_ttl_secs(messages, cached) else {
        return false;
    };

    let idle = now.saturating_sub(prev);
    let threshold = ttl.saturating_mul(SAFETY_MARGIN).max(COLD_FLOOR_SECS);
    idle > threshold
}

/// Test-only: pre-seed a conversation's last-touch `secs_ago` seconds in the
/// past so a single `repack_decision` call observes a controlled idle gap
/// (the function overwrites last-touch with `now` on every call).
///
/// Tests must use a *unique* first message (hence a unique `conversation_key`)
/// so seeding one never disturbs another running in parallel — the global store
/// is shared, so there is deliberately no global "clear" that would race.
#[cfg(test)]
pub(crate) fn test_seed_last_touch(messages: &[Value], secs_ago: u64) {
    if let Some(key) = conversation_key(messages) {
        let when = now_secs().saturating_sub(secs_ago);
        store()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, when);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cached_body(first_text: &str, ttl: Option<&str>) -> Vec<Value> {
        let cc = ttl.map_or_else(
            || json!({"type": "ephemeral"}),
            |t| json!({"type": "ephemeral", "ttl": t}),
        );
        vec![
            json!({"role": "user", "content": [
                {"type": "text", "text": first_text, "cache_control": cc}
            ]}),
            json!({"role": "assistant", "content": "ok"}),
        ]
    }

    #[test]
    fn key_is_stable_across_turns_and_distinct_per_conversation() {
        let mut a1 = cached_body("conversation A opening", None);
        let a2 = {
            let mut m = a1.clone();
            m.push(json!({"role": "user", "content": "a follow-up turn"}));
            m
        };
        let b1 = cached_body("conversation B opening", None);

        let ka = conversation_key(&a1).unwrap();
        let ka2 = conversation_key(&a2).unwrap();
        let kb = conversation_key(&b1).unwrap();
        assert_eq!(ka, ka2, "key must be stable as the conversation grows");
        assert_ne!(ka, kb, "distinct conversations must get distinct keys");

        // Mutating messages[0] changes the key (different conversation head).
        a1[0] = json!({"role": "user", "content": "different head"});
        assert_ne!(conversation_key(&a1).unwrap(), ka);
    }

    #[test]
    fn ttl_resolves_from_marker_else_default_else_none() {
        let hour = cached_body("x", Some("1h"));
        assert_eq!(resolved_ttl_secs(&hour, 1), Some(HOUR_TTL_SECS));

        let five = cached_body("x", Some("5m"));
        assert_eq!(resolved_ttl_secs(&five, 1), Some(DEFAULT_TTL_SECS));

        // Marker present without an explicit ttl → Anthropic "5m" default.
        let bare = cached_body("x", None);
        assert_eq!(resolved_ttl_secs(&bare, 1), Some(DEFAULT_TTL_SECS));

        // No client-cached prefix → nothing to repack.
        assert_eq!(resolved_ttl_secs(&bare, 0), None);
    }

    use super::test_seed_last_touch as seed;

    #[test]
    fn first_sighting_only_sets_baseline() {
        let msgs = cached_body("first-sighting conversation", None);
        // No prior touch → never repack, but a baseline is now recorded.
        assert!(!repack_decision(&msgs, 1));
        // Immediately after, idle ≈ 0 → still warm.
        assert!(!repack_decision(&msgs, 1));
    }

    #[test]
    fn warm_prefix_is_never_repacked() {
        let msgs = cached_body("warm conversation", Some("5m"));
        seed(&msgs, 60); // 1 minute idle, TTL 5m → warm
        assert!(!repack_decision(&msgs, 1));
    }

    #[test]
    fn large_gap_triggers_repack() {
        let msgs = cached_body("cold conversation 5m", Some("5m"));
        seed(&msgs, 2 * 60 * 60); // 2h idle, threshold = max(600, 600) = 600
        assert!(repack_decision(&msgs, 1));
    }

    #[test]
    fn cached_zero_never_repacks_even_when_idle() {
        let msgs = cached_body("idle but uncached", None);
        seed(&msgs, 24 * 60 * 60);
        // cached == 0: there is no client-cached prefix to repack.
        assert!(!repack_decision(&msgs, 0));
    }

    #[test]
    fn hour_ttl_skips_the_ambiguous_zone() {
        let msgs = cached_body("cold conversation 1h", Some("1h"));
        // threshold = 3600 * 2 = 7200s. Just under → still protect.
        seed(&msgs, 7000);
        assert!(!repack_decision(&msgs, 1));
        // Well past → repack.
        seed(&msgs, 8000);
        assert!(repack_decision(&msgs, 1));
    }
}
