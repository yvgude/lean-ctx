//! Conversation identity for read-cache scoping.
//!
//! The `[unchanged]` re-read stub means *"you already have this in context"* —
//! which is only true within the **same conversation / context window**. The
//! read [`SessionCache`](crate::core::cache::SessionCache) is shared across all
//! chats served by one daemon, so without scoping a file delivered in chat A
//! could be stubbed for a re-read in chat B (which never received it).
//!
//! Cursor's hooks write the live conversation id to `active_transcript.json`;
//! hosts without one (Claude Code / Codex / CodeBuddy) supply a per-session
//! `session_id` instead, which `load_active_transcript` returns as the scope id
//! so a new session never inherits a prior one's stubs (#1004). Either way it
//! carries a 2h TTL and we read it via
//! [`crate::hook_handlers::load_active_transcript`] but
//! cache it behind a short TTL so the read hot path never stats+parses a file on
//! every call. The last-known-good value is retained across a transient refresh
//! miss, so a momentary read failure never spuriously invalidates valid stubs.
//!
//! A Cursor subagent (`CURSOR_TASK_ID` set) is given its own `task:{id}` scope,
//! so it is never served — nor records — a stub under another agent's identity;
//! this lets the stub gate replace the old blanket subagent force-fresh (#956).
//!
//! ## Concurrency hardening (#1040)
//!
//! `active_transcript.json` is a single, last-writer-wins slot, and an MCP
//! `ctx_read` call carries no caller identity (`ToolContext` has none), so with
//! **two concurrent top-level chats** the daemon
//! cannot prove which chat is asking: the resolved id may be the *other* chat's
//! (last writer) or a TTL-stale value. A matching id is therefore untrustworthy
//! while more than one conversation is live. Rather than risk serving chat B a
//! stub for content only chat A received, the gate **withholds every stub while
//! more than one conversation has been active recently** — correctness over the
//! re-read savings. Single-conversation daemons (the common case) keep the full
//! savings; sightings are sampled on each transcript refresh. Subagents never
//! feed this signal (they short-circuit on their `task:` scope), so a parent +
//! its subagents are not counted as concurrent.
//!
//! ### Zero detection lag, and the host limit (#1042)
//!
//! The stub decision resolves the caller with [`current_conversation_id_fresh`],
//! which re-samples `active_transcript.json` (bypassing the `REFRESH_TTL`
//! cache) and notes the writer *before* the gate runs. A freshly-appeared second
//! chat is therefore detected with no lag, closing the small window in which a
//! stub could still leak before the next refresh sampled it.
//!
//! What is *not* reachable as a pure lean-ctx change is recovering the stub
//! savings while chats run concurrently: that needs a per-call caller identity,
//! and Cursor exposes none. The MCP `tools/call` carries no conversation id (no
//! documented `_meta`), and a `beforeMCPExecution` hook can gate a call's
//! *permission* but not rewrite its *arguments*. So the daemon cannot prove
//! which chat a direct `ctx_read` belongs to, and withholding under concurrency
//! stays the correct ceiling until the host adds per-call identity.
//!
//! Disabled with `LEAN_CTX_CONVERSATION_SCOPE=0` (falls back to the legacy
//! process-scoped behavior).

use std::sync::OnceLock;
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// How long a resolved conversation id stays fresh before we re-read the file.
const REFRESH_TTL: Duration = Duration::from_secs(3);

/// How long a sighting of a conversation id keeps counting toward "concurrently
/// active". Sized to comfortably span a multi-tab session's think/act gaps so a
/// second chat that briefly goes quiet doesn't drop below the concurrency
/// threshold and re-open the cross-chat stub hazard (#1040).
const CONCURRENCY_WINDOW: Duration = Duration::from_secs(30);

struct Cached {
    value: Option<String>,
    refreshed_at: Instant,
}

fn store() -> &'static RwLock<Option<Cached>> {
    static STORE: OnceLock<RwLock<Option<Cached>>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(None))
}

/// Recent sightings of distinct conversation ids (`id` → last-seen instant),
/// fed by [`refresh`]. Drives [`multiple_conversations_recent`] so the stub gate
/// can tell when the daemon is multiplexing more than one chat (#1040).
fn seen_store() -> &'static RwLock<Vec<(String, Instant)>> {
    static SEEN: OnceLock<RwLock<Vec<(String, Instant)>>> = OnceLock::new();
    SEEN.get_or_init(|| RwLock::new(Vec::new()))
}

pub(crate) fn scope_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        !matches!(
            std::env::var("LEAN_CTX_CONVERSATION_SCOPE")
                .ok()
                .as_deref()
                .map(str::trim),
            Some("0" | "false" | "off")
        )
    })
}

/// Pure core of the Cursor `task:` scope — split out so the derivation is
/// unit-testable without touching the process environment.
fn subagent_scope_from(task_id: Option<&str>) -> Option<String> {
    task_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(|id| format!("task:{id}"))
}

/// Full scope resolver — pure core of [`subagent_scope`], testable without
/// touching the process environment.
///
/// Priority:
/// 1. `LEAN_CTX_SCOPE` — explicit operator/integration override
/// 2. `CURSOR_TASK_ID` — Cursor's per-subagent identifier
/// 3. `CLAUDECODE=1` — per-process scope for Claude Code (#1292). Claude Code
///    does not (yet) provide a per-subagent env var, so parent and sub-agent
///    share the same `session_id`. Each lean-ctx process gets a unique scope
///    instead, which isolates their caches because each MCP connection is a
///    separate stdio process.
fn resolve_scope(
    cursor_task_id: Option<&str>,
    claudecode: Option<&str>,
    explicit_scope: Option<&str>,
    process_id: &str,
) -> Option<String> {
    if let Some(scope) = explicit_scope.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(format!("custom:{scope}"));
    }
    if let Some(scope) = subagent_scope_from(cursor_task_id) {
        return Some(scope);
    }
    if claudecode.is_some_and(|v| v == "1") {
        return Some(format!("proc:{process_id}"));
    }
    None
}

/// A unique identifier for this lean-ctx process, stable for its lifetime.
/// Used as a scope discriminator when no explicit subagent ID is available
/// (Claude Code, #1292).
fn process_unique_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let pid = std::process::id();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        format!("{pid}-{ts}")
    })
}

/// A per-connection conversation scope that prevents cross-agent stub leakage.
///
/// Scope sources (first match wins):
/// - `LEAN_CTX_SCOPE` — explicit override for custom integrations
/// - `CURSOR_TASK_ID` — Cursor subagents (#952/#956)
/// - `CLAUDECODE=1` — per-process scope for Claude Code (#1292); each
///   lean-ctx stdio process gets a unique scope so a sub-agent's process
///   never inherits the parent's stub deliveries
///
/// Returns `None` only when no agent environment is detected (standalone
/// usage, plain Cursor without a subagent, etc.), preserving legacy
/// transcript-based scoping.
fn subagent_scope() -> Option<String> {
    static SCOPE: OnceLock<Option<String>> = OnceLock::new();
    SCOPE
        .get_or_init(|| {
            // #1801: the test binary must not inherit the developer's ambient
            // agent environment. Running the suite inside Claude Code resolves a
            // `proc:` scope for *every* test, which silently changes delivery
            // behaviour — while the same tests pass in CI, where `CLAUDECODE` is
            // unset. That divergence is invisible until it bites: the suite was
            // green in CI and 31 tests failed locally purely because of an
            // exported variable.
            //
            // Scope behaviour itself stays covered, through the pure cores
            // (`resolve_scope`, `allows_stub`, `allows_cold_stub`), which take
            // every input explicitly — the same reason the gate matrix is tested
            // that way rather than through the process-global recency store.
            if cfg!(test) {
                return None;
            }
            resolve_scope(
                std::env::var("CURSOR_TASK_ID").ok().as_deref(),
                std::env::var("CLAUDECODE").ok().as_deref(),
                std::env::var("LEAN_CTX_SCOPE").ok().as_deref(),
                process_unique_id(),
            )
        })
        .clone()
}

/// The current conversation id, or `None` when no conversation context is
/// available (hooks not installed, TTL expired with no prior value, or scoping
/// disabled). `None` preserves the legacy process-scoped cache behavior.
///
/// Answers from the `REFRESH_TTL` cache when warm, so the read hot path never
/// stats+parses a file on every call.
pub(crate) fn current_conversation_id() -> Option<String> {
    resolve_conversation_id(Freshness::Cached)
}

/// Like [`current_conversation_id`] but bypasses the `REFRESH_TTL` cache to
/// re-sample `active_transcript.json` now. Used only on the re-read stub decision
/// path: re-sampling notes a freshly-appeared second chat into the recency log
/// (via `refresh`) *before* the gate runs, so concurrency is detected with no
/// TTL lag and a stub can never leak to chat B in the window before detection
/// catches up (#1042). The cost is one tiny, OS-cached transcript read per
/// re-read — negligible against the source re-read it guards.
pub(crate) fn current_conversation_id_fresh() -> Option<String> {
    resolve_conversation_id(Freshness::Fresh)
}

/// Whether [`resolve_conversation_id`] may answer from the [`REFRESH_TTL`] cache
/// ([`Freshness::Cached`]) or must re-sample the transcript ([`Freshness::Fresh`]).
#[derive(Clone, Copy)]
enum Freshness {
    Cached,
    Fresh,
}

/// Shared resolver behind [`current_conversation_id`] and its `_fresh` variant.
/// The scope-off and subagent short-circuits are identical for both; only the
/// `Cached` arm consults the TTL cache before falling through to [`refresh`].
fn resolve_conversation_id(freshness: Freshness) -> Option<String> {
    if !scope_enabled() {
        return None;
    }
    // A subagent is its own scope (see `subagent_scope`) and that wins over the
    // transcript id, so a subagent never inherits the parent's delivery identity
    // — and never samples the transcript, so it can't feed the concurrency signal.
    if let Some(scope) = subagent_scope() {
        return Some(scope);
    }
    if let Ok(guard) = store().read()
        && let Some(cached) = guard.as_ref()
        && cache_is_usable(freshness, cached.refreshed_at.elapsed(), REFRESH_TTL)
    {
        return cached.value.clone();
    }
    refresh()
}

/// Pure core of the cache-vs-resample choice: a `Fresh` request always
/// re-samples; a `Cached` one reuses an entry younger than `ttl`.
fn cache_is_usable(freshness: Freshness, age: Duration, ttl: Duration) -> bool {
    matches!(freshness, Freshness::Cached) && age < ttl
}

fn refresh() -> Option<String> {
    let fresh = crate::hook_handlers::load_active_transcript().and_then(|(_, conv)| conv);
    if let Some(id) = fresh.as_deref() {
        note_conversation_seen(id);
    }
    if let Ok(mut guard) = store().write() {
        // Retain last-known-good: a transient miss (file briefly absent or
        // expired) must not flip a stable conversation to `None` and force
        // needless cold re-reads.
        if fresh.is_none()
            && let Some(existing) = guard.as_ref()
            && existing.value.is_some()
        {
            let kept = existing.value.clone();
            *guard = Some(Cached {
                value: kept.clone(),
                refreshed_at: Instant::now(),
            });
            return kept;
        }
        *guard = Some(Cached {
            value: fresh.clone(),
            refreshed_at: Instant::now(),
        });
    }
    fresh
}

/// Record a sighting of `id` in the recency log. Thin wrapper over the pure
/// [`note_into`] so the global store stays an implementation detail.
fn note_conversation_seen(id: &str) {
    if let Ok(mut v) = seen_store().write() {
        note_into(&mut v, id, Instant::now(), CONCURRENCY_WINDOW);
    }
}

/// Pure core of [`note_conversation_seen`]: upsert `id`'s last-seen timestamp and
/// drop sightings older than `window`. One entry per id, so the vec length is the
/// number of distinct recent conversations.
fn note_into(v: &mut Vec<(String, Instant)>, id: &str, now: Instant, window: Duration) {
    v.retain(|(_, t)| now.duration_since(*t) < window);
    if let Some(entry) = v.iter_mut().find(|(seen, _)| seen == id) {
        entry.1 = now;
    } else {
        v.push((id.to_string(), now));
    }
}

/// Pure core of [`multiple_conversations_recent`]: distinct ids seen within
/// `window` of `now`.
fn distinct_within(v: &[(String, Instant)], now: Instant, window: Duration) -> usize {
    v.iter()
        .filter(|(_, t)| now.duration_since(*t) < window)
        .count()
}

/// True when more than one conversation has been active within
/// [`CONCURRENCY_WINDOW`] — i.e. the daemon is multiplexing chats, so the shared
/// `active_transcript.json` id can't be trusted to name the current caller and no
/// stub is provably in-context (#1040).
pub(crate) fn multiple_conversations_recent() -> bool {
    seen_store()
        .read()
        .is_ok_and(|v| distinct_within(&v, Instant::now(), CONCURRENCY_WINDOW) > 1)
}

/// Whether the active scope can name an *individual* caller.
///
/// #1801: the `proc:` scope was introduced for Claude Code (#1292) on the
/// premise that "each MCP connection is a separate stdio process", so one
/// process meant one consumer. That premise does not hold — Claude Code
/// sub-agents reuse their parent's MCP connection and spawn no lean-ctx
/// process of their own, so the parent and every sub-agent resolve the
/// *identical* `proc:` id. A match therefore proves nothing about who is
/// asking, and a stub served on it reaches a sub-agent whose context window
/// never held the content.
///
/// This is the same hazard `multiple_conversations_recent` already fails
/// closed on, so it gets the same treatment. `task:` (Cursor) and `custom:`
/// (explicit override) do name one agent and stay unaffected; so does the
/// legacy transcript path, where one daemon serves one conversation.
pub(crate) fn scope_cannot_identify_caller() -> bool {
    subagent_scope().is_some_and(|scope| scope.starts_with("proc:"))
}

/// Whether a `[unchanged]` stub may be served for an entry that was delivered to
/// `delivered`, given the `current` conversation.
///
/// `current == None` (no conversation context) preserves the legacy
/// process-scoped behavior — stub allowed — **unless** the caller cannot be
/// identified: more than one conversation has been active recently (#1040), or
/// the scope is only process-derived (#1801). Then every stub is withheld.
pub(crate) fn conversation_allows_stub(current: Option<&str>, delivered: Option<&str>) -> bool {
    allows_stub(
        scope_enabled(),
        multiple_conversations_recent() || scope_cannot_identify_caller(),
        current,
        delivered,
    )
}

/// Pure decision core (no env / global reads) so the full matrix is unit-testable.
///
/// `unprovable` folds together every reason a matching id fails to prove the
/// content is in *this* caller's context: several live chats (#1040) and a
/// scope that cannot name one caller (#1801).
fn allows_stub(
    scope_on: bool,
    unprovable: bool,
    current: Option<&str>,
    delivered: Option<&str>,
) -> bool {
    if !scope_on {
        // Explicit legacy mode: one daemon == one conversation by contract.
        return true;
    }
    if unprovable {
        // A matching id can't be trusted to name THIS caller, so nothing is
        // provably in-context — withhold every stub (#1040, #1801).
        return false;
    }
    match (current, delivered) {
        (Some(c), Some(d)) => c == d,
        // Known caller, unknown delivery (pre-scoping entry) → can't prove → block.
        (Some(_), None) => false,
        // Unknown caller on a single-conversation daemon → legacy allow.
        (None, _) => true,
    }
}

/// Whether a *cold* `[unchanged]` stub may be served — i.e. one backed only by
/// the persisted index ([`crate::core::read_stub_index`]) after a daemon
/// restart, with no live in-memory entry.
///
/// Stricter than [`conversation_allows_stub`]: a cold stub crosses a process
/// boundary, so we serve it **only** when both sides name the *same, known*
/// conversation. Unlike the warm path there is no "no context → legacy" escape,
/// because without a current conversation id we cannot prove the content is in
/// the new process's context, and a wrong cold stub would resurrect exactly the
/// cross-chat hazard #954 closed. Also withheld under concurrency (#1040).
pub(crate) fn conversation_allows_cold_stub(
    current: Option<&str>,
    delivered: Option<&str>,
) -> bool {
    allows_cold_stub(
        multiple_conversations_recent() || scope_cannot_identify_caller(),
        current,
        delivered,
    )
}

/// Pure decision core of [`conversation_allows_cold_stub`]. `unprovable` carries
/// the same meaning as in [`allows_stub`].
fn allows_cold_stub(unprovable: bool, current: Option<&str>, delivered: Option<&str>) -> bool {
    !unprovable && matches!((current, delivered), (Some(c), Some(d)) if c == d)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The gate matrix is tested through the pure cores (`allows_stub` /
    // `allows_cold_stub`) so the assertions are deterministic regardless of the
    // process-global recency store, which other tests mutate in parallel.

    /// #1801: a `proc:` scope is shared by the parent and every Claude Code
    /// sub-agent, so identical ids prove nothing about who is asking. The gate
    /// must withhold exactly as it does for concurrent chats — otherwise a
    /// sub-agent receives a reference to content it has never seen.
    #[test]
    fn process_derived_scope_withholds_stubs_even_when_ids_match() {
        let scope = "proc:4242-1700000000";
        assert!(
            !allows_stub(true, true, Some(scope), Some(scope)),
            "a shared process scope must not authorize a stub"
        );
        assert!(
            !allows_cold_stub(true, Some(scope), Some(scope)),
            "the cold path must withhold for the same reason"
        );
    }

    /// A scope that *does* name one agent keeps its savings: Cursor's `task:`
    /// and an explicit `custom:` override are per-agent by construction.
    #[test]
    fn per_agent_scopes_still_allow_stubs() {
        for scope in ["task:abc123", "custom:my-worker"] {
            assert!(
                allows_stub(true, false, Some(scope), Some(scope)),
                "{scope} identifies one caller and must keep deduplicating"
            );
        }
    }

    #[test]
    fn only_process_scopes_are_treated_as_unidentifiable() {
        assert!(subagent_scope_from(Some("abc")).is_some_and(|s| !s.starts_with("proc:")));
        assert!(
            resolve_scope(None, Some("1"), None, "4242-1700000000")
                .is_some_and(|s| s.starts_with("proc:")),
            "Claude Code without a per-subagent id must resolve to a proc: scope"
        );
    }

    #[test]
    fn no_current_context_allows_stub_legacy() {
        assert!(allows_stub(true, false, None, None));
        assert!(allows_stub(true, false, None, Some("conv-a")));
    }

    #[test]
    fn scope_disabled_always_allows_stub() {
        // Explicit opt-out → legacy process scope, even under (irrelevant) concurrency.
        assert!(allows_stub(false, true, Some("conv-b"), Some("conv-a")));
        assert!(allows_stub(false, true, None, None));
    }

    #[test]
    fn same_conversation_allows_stub() {
        assert!(allows_stub(true, false, Some("conv-a"), Some("conv-a")));
    }

    #[test]
    fn different_conversation_blocks_stub() {
        assert!(!allows_stub(true, false, Some("conv-b"), Some("conv-a")));
    }

    #[test]
    fn unknown_delivering_conversation_blocks_stub() {
        // Entry delivered before scoping existed → cannot prove it is in context.
        assert!(!allows_stub(true, false, Some("conv-a"), None));
    }

    #[test]
    fn concurrency_withholds_every_warm_stub() {
        // #1040: while >1 chat is live, even a same-id match is untrustworthy.
        assert!(!allows_stub(true, true, Some("conv-a"), Some("conv-a")));
        assert!(!allows_stub(true, true, None, None));
        assert!(!allows_stub(true, true, None, Some("conv-a")));
    }

    #[test]
    fn cold_stub_requires_both_known_and_matching() {
        assert!(allows_cold_stub(false, Some("c"), Some("c")));
        assert!(!allows_cold_stub(false, Some("c"), Some("d")));
        // No "legacy" escape for the cold path: unknown either side → blocked.
        assert!(!allows_cold_stub(false, None, Some("c")));
        assert!(!allows_cold_stub(false, Some("c"), None));
        assert!(!allows_cold_stub(false, None, None));
    }

    #[test]
    fn cold_stub_withheld_under_concurrency() {
        // #1040: a matching cold stub is still withheld while chats are multiplexed.
        assert!(!allows_cold_stub(true, Some("c"), Some("c")));
    }

    #[test]
    fn fresh_request_resamples_cached_request_honors_ttl() {
        let ttl = Duration::from_secs(3);
        // `Fresh` ignores the cache even for a brand-new entry → always re-samples,
        // so the stub gate sees a just-appeared second chat with zero lag (#1042).
        assert!(!cache_is_usable(
            Freshness::Fresh,
            Duration::from_millis(0),
            ttl
        ));
        assert!(!cache_is_usable(Freshness::Fresh, ttl * 100, ttl));
        // `Cached` reuses a within-TTL entry but re-samples once it has expired
        // (age == ttl is already expired: the window is a strict `<`).
        assert!(cache_is_usable(
            Freshness::Cached,
            Duration::from_secs(1),
            ttl
        ));
        assert!(!cache_is_usable(Freshness::Cached, ttl, ttl));
        assert!(!cache_is_usable(Freshness::Cached, ttl * 2, ttl));
    }

    #[test]
    fn recency_counts_distinct_ids_and_dedupes_repeats() {
        let now = Instant::now();
        let win = Duration::from_secs(30);
        let mut v = Vec::new();
        note_into(&mut v, "a", now, win);
        assert_eq!(distinct_within(&v, now, win), 1);
        note_into(&mut v, "a", now, win); // same id refreshes, no new entry
        assert_eq!(distinct_within(&v, now, win), 1);
        note_into(&mut v, "b", now, win); // second chat → concurrency
        assert_eq!(distinct_within(&v, now, win), 2);
    }

    #[test]
    fn recency_prunes_sightings_older_than_window() {
        let win = Duration::from_secs(30);
        let t0 = Instant::now();
        let mut v = Vec::new();
        note_into(&mut v, "old", t0, win);
        // A sighting well past the window prunes the stale one — back to one chat.
        let t1 = t0 + win * 2;
        note_into(&mut v, "new", t1, win);
        assert_eq!(distinct_within(&v, t1, win), 1);
    }

    #[test]
    fn subagent_scope_derives_a_distinct_non_none_id_from_task() {
        assert_eq!(
            subagent_scope_from(Some("abc123")),
            Some("task:abc123".to_string())
        );
        assert_eq!(
            subagent_scope_from(Some("  abc  ")),
            Some("task:abc".to_string())
        );
        assert_eq!(subagent_scope_from(Some("")), None);
        assert_eq!(subagent_scope_from(Some("   ")), None);
        assert_eq!(subagent_scope_from(None), None);
    }

    #[test]
    fn subagent_scope_never_matches_a_plain_conversation() {
        let sub = subagent_scope_from(Some("xyz")).unwrap();
        assert!(!allows_stub(true, false, Some(&sub), Some("xyz")));
        assert!(allows_stub(true, false, Some(&sub), Some(&sub)));
    }

    // --- resolve_scope (multi-host) tests (#1292) ---

    #[test]
    fn resolve_scope_explicit_override_wins() {
        let s = resolve_scope(Some("task-1"), Some("1"), Some("my-scope"), "42-99");
        assert_eq!(s, Some("custom:my-scope".to_string()));
    }

    #[test]
    fn resolve_scope_cursor_task_id_beats_claude_code() {
        let s = resolve_scope(Some("task-1"), Some("1"), None, "42-99");
        assert_eq!(s, Some("task:task-1".to_string()));
    }

    #[test]
    fn resolve_scope_claude_code_gets_process_scope() {
        let s = resolve_scope(None, Some("1"), None, "100-111");
        assert_eq!(s, Some("proc:100-111".to_string()));
    }

    #[test]
    fn resolve_scope_no_agent_env_returns_none() {
        assert_eq!(resolve_scope(None, None, None, "42-99"), None);
    }

    #[test]
    fn resolve_scope_claude_code_different_processes_differ() {
        let s1 = resolve_scope(None, Some("1"), None, "100-111").unwrap();
        let s2 = resolve_scope(None, Some("1"), None, "200-222").unwrap();
        assert_ne!(
            s1, s2,
            "different lean-ctx processes must get different scopes"
        );
    }

    #[test]
    fn claude_code_process_scope_never_matches_session_id() {
        let scope = resolve_scope(None, Some("1"), None, "42-12345").unwrap();
        assert!(!allows_stub(true, false, Some(&scope), Some("sess-abc")));
        assert!(allows_stub(true, false, Some(&scope), Some(&scope)));
    }

    #[test]
    fn claude_code_parent_and_subagent_stubs_isolated() {
        let parent = resolve_scope(None, Some("1"), None, "100-111").unwrap();
        let child = resolve_scope(None, Some("1"), None, "200-222").unwrap();
        assert!(allows_stub(true, false, Some(&parent), Some(&parent)));
        assert!(!allows_stub(true, false, Some(&child), Some(&parent)));
        assert!(!allows_cold_stub(false, Some(&child), Some(&parent)));
    }

    #[test]
    fn resolve_scope_blank_explicit_override_ignored() {
        assert_eq!(resolve_scope(None, None, Some("  "), "42-99"), None);
        assert_eq!(resolve_scope(None, None, Some(""), "42-99"), None);
    }
}
