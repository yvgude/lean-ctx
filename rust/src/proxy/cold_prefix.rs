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
//! State persists across restarts (`{data_dir}/cold_prefix_touch.json`, atomic
//! write, throttled) so an idle gap that straddles a daemon recycle is still
//! detected — a stale on-disk timestamp is exactly what proves the gap and can
//! only ever bias toward "warm" if lost (#499). A missing/corrupt file simply
//! disables the optimization until a fresh baseline is recorded — a safe
//! degradation that can never wrongly trigger.
//!
//! Once a conversation is judged cold and repacked, the decision is *sticky*:
//! every later turn keeps applying the same deterministic prefix compression, so
//! the warm follow-ups that resume active use hit the compressed prefix written
//! at the cold turn instead of re-sending the uncompressed original and busting
//! the freshly-seeded cache (#499). Deterministic re-compression is prefix-
//! stable, so the latch stays cache-safe for the rest of the session.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
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
/// Minimum seconds between disk persists. The on-disk baseline only needs to be
/// "fresh enough" to prove a multi-minute gap, so throttling keeps the hot path
/// off the disk on every request without weakening the long-gap guarantee.
const PERSIST_MIN_INTERVAL_SECS: u64 = 30;
/// Cross-restart baseline store, in the shared data dir.
const TOUCH_FILE: &str = "cold_prefix_touch.json";

/// Per-conversation tracking state. `last_touch` is the Unix-seconds timestamp of
/// the most recent request; `repacking` latches on once a cold gap triggered a
/// repack, so subsequent turns stay cache-stable on the compressed prefix (#499).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct ConvState {
    last_touch: u64,
    repacking: bool,
}

fn store() -> &'static Mutex<HashMap<u64, ConvState>> {
    static STORE: OnceLock<Mutex<HashMap<u64, ConvState>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Wall-clock seconds of the last successful disk persist (throttle gate).
fn last_persist() -> &'static AtomicU64 {
    static LAST: AtomicU64 = AtomicU64::new(0);
    &LAST
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

/// Stable per-conversation key: a hash of the first message with every
/// `cache_control` marker stripped first. `messages[0]` is byte-stable across a
/// conversation's turns *except* for its volatile cache breakpoint — clients move
/// or retune the `cache_control` (`ephemeral`/`ttl`) marker as the prompt grows.
/// Hashing the raw message would then change the key mid-conversation → a
/// permanent "first sighting" that never repacks (#499). Stripping the marker
/// keys on stable content only. Distinct conversations still differ (distinct
/// opening turns); a collision can only make the recorded last-touch *more
/// recent*, biasing toward "warm" — never a wrong "cold". `None` when there is no
/// first message.
fn conversation_key(messages: &[Value]) -> Option<u64> {
    let mut first = messages.first()?.clone();
    strip_cache_control(&mut first);
    let bytes = serde_json::to_vec(&first).ok()?;
    Some(hash_bytes(&bytes))
}

/// Recursively remove every `cache_control` field so a moving cache breakpoint
/// can't change the conversation key. The marker always nests `type:ephemeral`
/// and any `ttl` inside `cache_control`, so dropping that one field suffices.
fn strip_cache_control(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.remove("cache_control");
            for val in map.values_mut() {
                strip_cache_control(val);
            }
        }
        Value::Array(arr) => {
            for val in arr {
                strip_cache_control(val);
            }
        }
        _ => {}
    }
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

fn evict_oldest(map: &mut HashMap<u64, ConvState>) {
    if let Some(oldest_key) = map
        .iter()
        .min_by_key(|(_, s)| s.last_touch)
        .map(|(k, _)| *k)
    {
        map.remove(&oldest_key);
    }
}

/// Decide whether to repack the (predicted-cold) cached prefix for THIS request,
/// recording this request as the conversation's latest touch.
///
/// Returns `true` when the conversation is already in the sticky repacking state
/// (a prior turn went cold — keep the compressed prefix stable, #499) or when a
/// fresh cold gap is detected: a client-cached prefix exists (`cached > 0`), a
/// prior touch exists (so the idle gap is measurable), and the idle gap exceeds
/// `TTL × SAFETY_MARGIN` and the absolute floor. The caller owns the opt-in gate;
/// this is only ever called when the operator enabled it, so updating the
/// last-touch baseline here is the intended side effect for the *next* request.
pub fn repack_decision(messages: &[Value], cached: usize) -> bool {
    let Some(key) = conversation_key(messages) else {
        return false;
    };
    let now = now_secs();
    let ttl = resolved_ttl_secs(messages, cached);

    let (decision, changed) = {
        let mut map = store()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prev = map.get(&key).copied();
        let was_first = prev.is_none();
        let already_repacking = prev.is_some_and(|s| s.repacking);

        // A fresh cold gap: a measurable idle past `TTL × margin` (and the floor)
        // on a turn that actually carries a client-cached prefix.
        let fresh_cold = match (prev, ttl) {
            (Some(p), Some(t)) if cached > 0 => {
                let idle = now.saturating_sub(p.last_touch);
                idle > t.saturating_mul(SAFETY_MARGIN).max(COLD_FLOOR_SECS)
            }
            _ => false,
        };

        // Sticky latch: once cold→repacked, stay repacking. Deterministic re-
        // compression keeps the prefix byte-stable so warm follow-ups hit the
        // cache written at the cold turn instead of busting it (#499).
        let repacking = already_repacking || fresh_cold;
        map.insert(
            key,
            ConvState {
                last_touch: now,
                repacking,
            },
        );
        if map.len() > MAX_TRACKED {
            evict_oldest(&mut map);
        }

        // Persist eagerly when the latch first engages or on a first sighting
        // (both define a baseline that must survive an immediate restart);
        // otherwise let the throttle decide.
        let changed = was_first || (repacking && !already_repacking);
        // Repack only when we are in the repacking state AND there is a cached
        // prefix to act on this turn (a `cached == 0` turn prunes from 0 anyway).
        (repacking && cached > 0, changed)
    };

    maybe_persist(changed, now);
    decision
}

/// On-disk shape of the cross-restart baselines. `ts` is advisory (debugging);
/// the per-conversation `last_touch` values are what prove an idle gap.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedTouch {
    ts: u64,
    conversations: HashMap<u64, ConvState>,
}

fn touch_path() -> Option<std::path::PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join(TOUCH_FILE))
}

/// Seeds the in-memory baselines from disk on proxy startup so an idle gap that
/// straddles a restart is still detected. Merges by most-recent `last_touch` and
/// OR-s the sticky `repacking` latch, so a re-seed can only bias toward "warm"
/// (or keep a latch), never toward a wrong "cold".
pub fn resume_from_disk() {
    let Some(path) = touch_path() else {
        return;
    };
    let Ok(data) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(persisted) = serde_json::from_str::<PersistedTouch>(&data) else {
        return;
    };
    let mut map = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (key, state) in persisted.conversations {
        let entry = map.entry(key).or_insert(state);
        if state.last_touch > entry.last_touch {
            entry.last_touch = state.last_touch;
        }
        entry.repacking |= state.repacking;
    }
    while map.len() > MAX_TRACKED {
        evict_oldest(&mut map);
    }
}

/// Persist when forced (a baseline/latch change) or when the throttle window has
/// elapsed. The disk write happens outside the store lock.
fn maybe_persist(force: bool, now: u64) {
    let last = last_persist().load(Ordering::Relaxed);
    if !force && now.saturating_sub(last) < PERSIST_MIN_INTERVAL_SECS {
        return;
    }
    last_persist().store(now, Ordering::Relaxed);
    persist_now(now);
}

/// Atomically writes the current baselines to disk (`.tmp` + rename).
fn persist_now(now: u64) {
    let Some(path) = touch_path() else {
        return;
    };
    let conversations = {
        let map = store()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.clone()
    };
    let payload = PersistedTouch {
        ts: now,
        conversations,
    };
    let Ok(json) = serde_json::to_string(&payload) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
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
            .insert(
                key,
                ConvState {
                    last_touch: when,
                    repacking: false,
                },
            );
    }
}

/// Test-only: drop a single conversation's in-memory baseline (simulates a proxy
/// restart losing RAM for that key). Single-key removal stays race-free with the
/// other tests that share the global store.
#[cfg(test)]
fn test_remove(messages: &[Value]) {
    if let Some(key) = conversation_key(messages) {
        store()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
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

    #[test]
    fn key_ignores_cache_control_marker() {
        // #499 (3): the same opening content with a different — or absent —
        // cache_control marker must map to the SAME conversation key, so a moving
        // cache breakpoint never causes a permanent first-sighting.
        let none = cached_body("marker-invariant conversation", None);
        let hour = cached_body("marker-invariant conversation", Some("1h"));
        let five = cached_body("marker-invariant conversation", Some("5m"));
        let k = conversation_key(&none).unwrap();
        assert_eq!(k, conversation_key(&hour).unwrap());
        assert_eq!(k, conversation_key(&five).unwrap());
        // Different opening content still yields a different key.
        let other = cached_body("a different opening", Some("1h"));
        assert_ne!(k, conversation_key(&other).unwrap());
    }

    #[test]
    fn sticky_repack_persists_into_warm_followups() {
        // #499 (1): the N→N+1 interaction the original tests never covered.
        let msgs = cached_body("sticky cold-then-warm conversation", Some("5m"));
        // Turn N: a long idle gap → cold → repack fires and latches.
        seed(&msgs, 2 * 60 * 60);
        assert!(
            repack_decision(&msgs, 1),
            "a cold gap must trigger the repack"
        );
        // Turn N+1, seconds later (idle ≈ 0): pre-fix this fell back to protecting
        // the prefix and re-sent the uncompressed original, busting the cache
        // written at turn N. The latch must keep repacking so the cold-turn cache
        // is hit.
        assert!(
            repack_decision(&msgs, 1),
            "an immediate warm follow-up must stay sticky and keep repacking"
        );
        assert!(
            repack_decision(&msgs, 1),
            "stickiness persists across the rest of the session"
        );
    }

    #[test]
    fn cold_baseline_survives_restart_via_disk() {
        // #499 (2): a baseline recorded before a restart must be recoverable from
        // disk so the long gap is still detected (RAM-only loses it).
        let _iso = crate::core::data_dir::isolated_data_dir();
        let msgs = cached_body("restart-survival conversation", Some("5m"));
        seed(&msgs, 3 * 60 * 60);
        persist_now(now_secs());
        // Simulate a proxy restart: the in-memory baseline is gone.
        test_remove(&msgs);
        // resume_from_disk restores it; the persisted cold gap still triggers.
        resume_from_disk();
        assert!(
            repack_decision(&msgs, 1),
            "a persisted cold baseline must survive a restart and still repack"
        );
    }
}
