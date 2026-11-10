// SPDX-License-Identifier: Apache-2.0
//! Source lineage of reusable tool results (G04): idempotent replay and the
//! MCP response cache.
//!
//! The lifecycle keeps a finished output per idempotency key and hands it out
//! again for a repeated request; the response cache does the same per request
//! shape. Replay admission re-runs tool, role and egress checks, and the
//! release point re-checks the result *text* — neither can see that the source
//! a neutral-looking excerpt was derived from has been deleted, restricted or
//! reclassified since. This module keeps what the dispatch acquired, in one of
//! three states that are never merged:
//!
//! - **source set captured** — the admitted readers published the sources and
//!   the tool declares that it reads every source through them
//!   (`McpTool::publishes_every_source`); re-authorized on reuse, every source;
//! - **attested source-free** — the tool itself declares that this invocation
//!   reads no source (`McpTool::attests_source_free`) and none was observed;
//! - **unknown** — everything else: nothing observed and nothing attested (an
//!   uninstrumented reader looks exactly like this), a source that could not
//!   be bound, or sources captured from a tool that does not declare the set
//!   complete. Absence of evidence is not stored as "no source".
//!
//! Under protection (a project policy pack, or the global gateway in governed
//! or sovereign mode) an unknown lineage is not reused: a replay gets the
//! typed current denial, a cache entry is neither stored nor served. The key
//! is never changed and a completed operation is never executed again.
//! Community developer mode replays an unknown lineage as before.
//!
//! A replayed output keeps the outside origins it was produced from (pz21
//! §1.5): the dispatch's origin ledger is stored with the lineage and noted
//! again in the call that serves the output, so its release fences and
//! taints exactly as the first one did. Without a capture to note them in,
//! such an output is not replayed.
//!
//! In-process only: relay or remote replay and any on-disk replay store are
//! not covered here.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use rmcp::model::{CallToolResult, ContentBlock};

use lean_ctx::core::archive::authority::ArchiveAuthority;
use lean_ctx::core::context_admission::capture::{self, OriginLedger};
use lean_ctx::core::context_admission::carrier::Withheld;

/// Same order of magnitude as the lifecycle's own finished-output retention.
const CAPACITY: usize = 4_096;

/// One MCP call: its idempotency key, and what its dispatch acquired.
#[derive(Debug)]
struct CallScope {
    key: Option<String>,
    lineage: Mutex<Option<Lineage>>,
}

tokio::task_local! {
    static CALL: Arc<CallScope>;
}

/// What a tool declares about one invocation (`McpTool`).
#[derive(Debug, Clone, Copy, Default)]
pub(in crate::server::call_tool) struct Declaration {
    /// `publishes_every_source`: a captured source set is the whole set.
    pub publishes_every_source: bool,
    /// `attests_source_free`: the output derives from caller arguments only.
    pub attests_source_free: bool,
}

/// What the dispatch of one call was derived from.
#[derive(Debug, Clone)]
enum Lineage {
    /// The admitted readers published these sources during the dispatch, and
    /// the tool declares that it reads every source through them.
    Sources(Box<ArchiveAuthority>),
    /// The tool attests this invocation reads no source, and none was seen.
    /// Governed by content rules at release only — weaker than source lineage.
    AttestedSourceFree,
    /// Not established (see the module documentation).
    Unknown,
}

impl Lineage {
    fn of(authority: Option<&ArchiveAuthority>, declared: Declaration) -> Self {
        match authority {
            Some(authority) if authority.is_incomplete() => Self::Unknown,
            // A captured source always wins over an attestation; it counts
            // only when the tool declares the captured set complete.
            Some(authority) if declared.publishes_every_source => {
                Self::Sources(Box::new(authority.clone()))
            }
            None if declared.attests_source_free => Self::AttestedSourceFree,
            Some(_) | None => Self::Unknown,
        }
    }

    /// Whether this lineage authorizes reuse of the output NOW: every source
    /// re-read under the current policy and role and unchanged, or attested.
    fn reauthorized(&self) -> Result<(), Withheld> {
        match self {
            Self::Sources(authority) => {
                let admitted =
                    lean_ctx::core::policy::runtime::with_source_view(|| authority.admitted())
                        .unwrap_or(false);
                if admitted {
                    Ok(())
                } else {
                    Err(Withheld::source_unavailable())
                }
            }
            Self::AttestedSourceFree => Ok(()),
            Self::Unknown => Err(Withheld::no_source_authority()),
        }
    }
}

#[derive(Debug, Clone)]
struct Entry {
    lineage: Lineage,
    /// How often a dispatch ran for this key. A replay never increments it.
    dispatches: u32,
    /// Digest of the gateway policy the output was produced under (response
    /// cache entries only; a replay is re-checked by the release point).
    gateway_digest: Option<String>,
    /// The outside origins of the stored output (empty: project-only).
    origins: OriginLedger,
}

/// The content-free reason a replay of an output with outside origins is
/// refused when the serving call has no capture to note them in.
pub(in crate::server::call_tool) const UNTRUSTED_REPLAY: &str = "untrusted_replay";

struct Store {
    order: VecDeque<String>,
    entries: HashMap<String, Entry>,
}

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| {
        Mutex::new(Store {
            order: VecDeque::new(),
            entries: HashMap::new(),
        })
    })
}

/// Run one MCP call with its idempotency key in scope.
pub(in crate::server::call_tool) async fn scope<T>(
    key: Option<String>,
    call: impl std::future::Future<Output = T>,
) -> T {
    let call_scope = Arc::new(CallScope {
        key,
        lineage: Mutex::new(None),
    });
    CALL.scope(call_scope, call).await
}

fn current_call() -> Option<Arc<CallScope>> {
    CALL.try_with(Clone::clone).ok()
}

/// Record the lineage of the dispatch that just ran for the call in scope.
/// `authority` is what `archive::authority::capture` returned for it;
/// `declared` is the tool's own declaration for this invocation.
pub(in crate::server::call_tool) fn record(
    authority: Option<&ArchiveAuthority>,
    declared: Declaration,
) {
    let Some(call) = current_call() else {
        return;
    };
    let lineage = Lineage::of(authority, declared);
    if let Some(key) = &call.key {
        record_for(key, lineage.clone(), None);
        let origins = capture::current()
            .map(|capture| capture.output_origins())
            .unwrap_or_default();
        record_origins(key, origins);
    }
    *call.lineage.lock().unwrap_or_else(PoisonError::into_inner) = Some(lineage);
}

/// Store the outside origins of the output just recorded for `key`.
fn record_origins(key: &str, origins: OriginLedger) {
    let mut store = store().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(entry) = store.entries.get_mut(key) {
        entry.origins = origins;
    }
}

fn record_for(key: &str, lineage: Lineage, gateway_digest: Option<String>) {
    let mut store = store().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(entry) = store.entries.get_mut(key) {
        entry.lineage = lineage;
        entry.gateway_digest = gateway_digest;
        entry.origins = OriginLedger::default();
        entry.dispatches = entry.dispatches.saturating_add(1);
        return;
    }
    store.entries.insert(
        key.to_owned(),
        Entry {
            lineage,
            dispatches: 1,
            gateway_digest,
            origins: OriginLedger::default(),
        },
    );
    store.order.push_back(key.to_owned());
    if store.order.len() > CAPACITY
        && let Some(oldest) = store.order.pop_front()
    {
        store.entries.remove(&oldest);
    }
}

/// Replay admission for the keyed call in scope: `Some(denial)` replaces the
/// stored output; `None` lets the stored output through to the release
/// point, with its outside origins noted in the serving call's capture.
pub(in crate::server::call_tool) fn denial() -> Option<CallToolResult> {
    let key = current_call()?.key.clone()?;
    if let Some(text) = denial_text_for(&key) {
        return Some(CallToolResult::error(vec![ContentBlock::text(text)]));
    }
    renote_origins(&key).map(|text| CallToolResult::error(vec![ContentBlock::text(text)]))
}

/// Bring the stored output's outside origins into the serving call, so the
/// release point fences and taints the replay. `Some(text)`: there is no
/// capture to note them in, and the output is refused instead.
fn renote_origins(key: &str) -> Option<String> {
    let origins = entry(key).map(|entry| entry.origins).unwrap_or_default();
    if origins.is_empty() {
        return None;
    }
    match capture::current() {
        Some(capture) => {
            capture.renote_origins(&origins);
            None
        }
        None => Some(Withheld::new(&[UNTRUSTED_REPLAY]).message("replayed result")),
    }
}

/// Typed outcome of a keyed replay whose operation was dispatched earlier and
/// whose stored result is not released now. Distinct from a refusal: the
/// operation HAS run (a write has happened) and is not run again; only its
/// original result is withheld. The caller must not retry under a new key to
/// "get it through". Always followed by the reason the result is withheld.
pub(in crate::server::call_tool) const COMPLETED_NOT_RELEASED: &str =
    "replay.completed_result_not_released";

/// The text of a replay denial. Carries reason codes only — nothing of the
/// original result, its size or whether it was an error.
///
/// - a dispatch is recorded for the key: [`COMPLETED_NOT_RELEASED`];
/// - none is recorded (the call ended before its dispatch, or the record was
///   evicted): the plain refusal, which claims nothing about an execution.
fn denial_text_for(key: &str) -> Option<String> {
    let withheld = denial_for(key)?;
    Some(if dispatches_for(key) > 0 {
        format!(
            "operation already executed under this idempotency key and not executed again; \
             its original result is not released ({COMPLETED_NOT_RELEASED}, {})",
            withheld.codes().join(", ")
        )
    } else {
        withheld.message("replayed result")
    })
}

fn entry(key: &str) -> Option<Entry> {
    store()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entries
        .get(key)
        .cloned()
}

fn denial_for(key: &str) -> Option<Withheld> {
    match entry(key).map(|entry| entry.lineage) {
        // A captured source set is re-authorized in every mode.
        Some(lineage @ Lineage::Sources(_)) => lineage.reauthorized().err(),
        Some(Lineage::AttestedSourceFree) => None,
        // Unknown lineage, or no record at all (finished without a dispatch,
        // evicted): protected deployments do not replay it.
        Some(Lineage::Unknown) | None => {
            lean_ctx::core::context_admission::stored::source_authority_required()
                .then(Withheld::no_source_authority)
        }
    }
}

fn cache_entry_key(cache_id: &str) -> String {
    format!("response-cache:{cache_id}")
}

fn current_gateway_digest() -> String {
    lean_ctx::core::context_admission::stores::StoreAdmission::current()
        .digest()
        .to_owned()
}

/// Bind a response-cache entry to the lineage of the dispatch that produced
/// it. `true`: the result may be cached under protection — its lineage is a
/// complete source set or an attested source-free output, and it is recorded
/// together with the gateway policy it was produced under. `false`: the
/// lineage is unknown; the result must not be cached.
pub(in crate::server::call_tool) fn bind_cache_entry(cache_id: &str) -> bool {
    let lineage = current_call().and_then(|call| {
        call.lineage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    });
    bind_cache_entry_to(cache_id, lineage)
}

fn bind_cache_entry_to(cache_id: &str, lineage: Option<Lineage>) -> bool {
    match lineage {
        Some(lineage @ (Lineage::Sources(_) | Lineage::AttestedSourceFree)) => {
            record_for(
                &cache_entry_key(cache_id),
                lineage,
                Some(current_gateway_digest()),
            );
            true
        }
        Some(Lineage::Unknown) | None => false,
    }
}

/// Whether a response-cache entry may be served under protection NOW: it was
/// bound when it was stored, the gateway policy is the one it was produced
/// under, and every source of its lineage re-admits (present, unchanged,
/// permitted for the current project and role). Anything else is a miss.
pub(in crate::server::call_tool) fn cache_entry_current(cache_id: &str) -> bool {
    entry(&cache_entry_key(cache_id)).is_some_and(|entry| {
        entry.gateway_digest.as_deref() == Some(current_gateway_digest().as_str())
            && entry.lineage.reauthorized().is_ok()
    })
}

/// How often a dispatch ran for this key (0: none recorded).
fn dispatches_for(key: &str) -> u32 {
    store()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entries
        .get(key)
        .map_or(0, |entry| entry.dispatches)
}

#[cfg(test)]
mod tests;
