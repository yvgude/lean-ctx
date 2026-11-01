//! Runtime view of the active context policy pack (GL #673 / #489 enforcement).
//!
//! [`active`] refreshes bounded policy and organization trust bytes on every
//! admission. Identical snapshots reuse prepared rules; changed or unreadable
//! inputs never reuse a stale authorization. One bounded worker serializes file
//! resolution and compilation without accumulating timed-out reader threads.
//!
//! **Opt-in & backward-compatible:** with no project pack present, [`active`]
//! returns `None` and nothing is gated — existing behavior is preserved
//! exactly. An invalid local pack activates a deny-all policy so enforcement
//! never fails open.
//!
//! **Local-Free Invariant:** enforcement derived from this view only ever
//! constrains the *agent* pipeline; it never gates a human's own local reads.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::{OnceLock, RwLock};

use regex::Regex;

use super::ResolvedPolicy;
#[cfg(test)]
use super::{parse_file, resolve};
use crate::core::input_filters::{FilterAction, FilterConfig};

mod refresh;
/// Process-scoped pin set by explicit protected launchers; absent in Community.
pub(crate) const REQUIRED_POLICY_ROOT_ENV: &str = "LEAN_CTX_REQUIRED_POLICY_ROOT";
pub(crate) const REQUIRED_POLICY_DIGEST_ENV: &str = "LEAN_CTX_REQUIRED_POLICY_DIGEST";
pub(crate) use refresh::{protected_authority_paths, protected_policy_digest};
#[cfg(test)]
pub(crate) mod held_horizon_tests;
#[cfg(test)]
mod refresh_tests;

tokio::task_local! {
    /// Per-request project authority; never mutate process CWD for MCP roots.
    pub(crate) static REQUEST_PROJECT: std::cell::RefCell<Option<PathBuf>>;
    /// Immutable policy view for one synchronous source-admission operation.
    pub(crate) static SOURCE_VIEW: std::cell::RefCell<Option<SourceView>>;
}

#[derive(Clone)]
pub(crate) struct SourceView {
    project: PathBuf,
    policy: Option<Arc<ActivePolicy>>,
    fingerprint: blake3::Hash,
    role: crate::core::roles::Role,
    role_fingerprint: blake3::Hash,
    /// Set when the pinned policy rests on a verifier's decision.
    ///
    /// Two different bounds, deliberately: the request deadline bounded the
    /// decision itself — asking, recording, handing out — and ended with the
    /// refresh that produced this view. The operation running on the view is
    /// bounded by the signed horizon instead, checked on a fresh clock at
    /// every use of the held policy, at every nested admission and last,
    /// after all closing waits, before anything is published. A lifted
    /// requirement has no horizon: it is durable marker state, and the view
    /// stays bound to it through the fingerprint.
    lifetime: Option<super::org::Lifetime>,
    /// What the verifier's answer stated, from the same verification the
    /// pinned policy rests on. Never read again for this view.
    observation: Option<super::org::Observation>,
    valid: Arc<AtomicBool>,
    live: Arc<AtomicBool>,
}

impl SourceView {
    /// Whether the signed horizon still holds; a view past it is invalid for
    /// good, like one whose authority changed.
    fn within_horizon(&self) -> bool {
        let holds = self
            .lifetime
            .is_none_or(|lifetime| lifetime.check_horizon().is_ok());
        if !holds {
            self.valid.store(false, Ordering::Release);
        }
        holds
    }

    /// Invalidate the view for its owner — but only while it is still live.
    /// A straggler that merely finds the view ended (a timed-out worker whose
    /// result its caller already discarded) must not poison the owner's
    /// closing check: that turned complete, unchanged `ctx_compose` results
    /// into "source authority changed" refusals (~3 % of calls, focus
    /// analysis 2026-10-15). Inconsistencies seen during the view's life
    /// still invalidate it.
    fn invalidate_if_live(&self) {
        if self.live.load(Ordering::Acquire) {
            self.valid.store(false, Ordering::Release);
        }
    }
}

struct SourceViewLifetime(Arc<AtomicBool>);

/// Revalidate buffered CLI data at its output and persistence boundaries.
/// This does not lock policy files against a concurrent administrator write.
#[derive(Debug)]
pub(crate) struct PublicationAuthority {
    project: PathBuf,
    fingerprint: blake3::Hash,
    role_fingerprint: blake3::Hash,
    /// The horizon of the view the buffered data was produced under.
    lifetime: Option<super::org::Lifetime>,
    valid: Arc<AtomicBool>,
}

impl PublicationAuthority {
    pub(crate) fn capture() -> Result<Self, String> {
        let view = current_source_view().ok_or("source authority unavailable")?;
        if !view.within_horizon() {
            return Err("source authority unavailable".into());
        }
        Ok(Self {
            project: view.project,
            fingerprint: view.fingerprint,
            role_fingerprint: view.role_fingerprint,
            lifetime: view.lifetime,
            valid: view.valid,
        })
    }

    pub(crate) fn verify(&self) -> Result<(), String> {
        if !self.valid.load(Ordering::Acquire)
            || REQUEST_PROJECT
                .try_with(|slot| {
                    slot.borrow()
                        .as_ref()
                        .is_some_and(|root| root != &self.project)
                })
                .unwrap_or(false)
        {
            return Err("source authority changed before publication".into());
        }
        REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(Some(self.project.clone())), || {
            SOURCE_VIEW.sync_scope(std::cell::RefCell::new(None), || {
                let latest = refresh::current_verified(self.project.clone())?;
                let role = role_fingerprint(&crate::core::roles::active_role())?;
                if latest.fingerprint != self.fingerprint || role != self.role_fingerprint {
                    return Err("source authority changed before publication".into());
                }
                // Last, after the waits above: the buffered data was made
                // under a floor signed up to a horizon, and is not published
                // past it even though the authority verifies afresh.
                if let Some(lifetime) = &self.lifetime {
                    lifetime.check_horizon()?;
                }
                Ok(())
            })
        })
    }

    /// Fingerprint used to bind derived decisions to this exact project and
    /// policy snapshot without exposing any policy contents.
    pub(crate) fn fingerprint_bytes(&self) -> [u8; 32] {
        *self.fingerprint.as_bytes()
    }

    /// Role authorization is a separate part of source authority and must be
    /// bound independently into persisted provenance metadata.
    #[cfg(any(test, feature = "http-server"))]
    pub(crate) fn role_fingerprint_bytes(&self) -> [u8; 32] {
        *self.role_fingerprint.as_bytes()
    }
}

impl Drop for SourceViewLifetime {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn current_source_view() -> Option<SourceView> {
    SOURCE_VIEW
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
}

/// Capture the pinned view when propagating a synchronous worker scope.
pub(crate) fn inherited_source_view() -> Option<SourceView> {
    current_source_view()
}

/// Resolve the project root for a detached worker without retaining request-
/// lifetime policy or capture state. The worker must reopen a fresh source view
/// from this root when it stores output.
pub(crate) fn detached_project_root(fallback: &Path) -> Result<PathBuf, String> {
    let request_project = REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten();
    if let Some(view) = current_source_view() {
        if !view.live.load(Ordering::Acquire)
            || !view.valid.load(Ordering::Acquire)
            || request_project.as_ref() != Some(&view.project)
        {
            view.invalidate_if_live();
            return Err("source policy view is no longer active".into());
        }
        return Ok(view.project);
    }
    Ok(request_project.unwrap_or_else(|| fallback.to_path_buf()))
}

/// Restore a previously captured source view in a detached worker.
pub(crate) fn with_inherited_source_view<T>(
    view: Option<SourceView>,
    operation: impl FnOnce() -> T,
) -> T {
    match view {
        Some(view) => SOURCE_VIEW.sync_scope(std::cell::RefCell::new(Some(view)), operation),
        None => operation(),
    }
}

/// The role paired with the active source view. A closed or rebound inherited
/// view returns a deny-all role instead of exposing its stale privileges.
pub(crate) fn pinned_role() -> Option<crate::core::roles::Role> {
    current_source_view().map(|view| {
        let project = REQUEST_PROJECT
            .try_with(|slot| slot.borrow().clone())
            .ok()
            .flatten();
        if !view.live.load(Ordering::Acquire)
            || !view.valid.load(Ordering::Acquire)
            || project.as_ref() != Some(&view.project)
        {
            view.invalidate_if_live();
            crate::core::roles::closed_source_view_role()
        } else {
            view.role
        }
    })
}

/// Run `operation` as part of a request for `project`.
pub(crate) fn in_project<T>(project: impl Into<PathBuf>, operation: impl FnOnce() -> T) -> T {
    REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(Some(project.into())), operation)
}

/// Start `operation` on its own thread as part of a request for `project`.
/// A spawned thread inherits no task-local: without this it reads the
/// configuration and admission state of the process's own project (AP04-G3).
pub(crate) fn spawn_for_project<T: Send + 'static>(
    project: impl Into<PathBuf>,
    operation: impl FnOnce() -> T + Send + 'static,
) -> std::thread::JoinHandle<T> {
    let project = project.into();
    std::thread::spawn(move || in_project(project, operation))
}

pub(crate) fn bind_request_project(project: Option<&str>) {
    if current_source_view().is_some() {
        return;
    }
    let _ = REQUEST_PROJECT.try_with(|slot| {
        *slot.borrow_mut() = project.map(PathBuf::from);
    });
}

/// The one spelling of a request project: resolved, and on Windows without
/// the verbatim `\\?\` prefix (`pathutil::canonicalize_secure`), as the proxy
/// (`request_project`) and the policy refresh resolve it. A source view
/// pinned to another spelling of the same root no longer matched the request
/// project and fell to deny-all on every governed Windows request.
fn canonical_project_root(project: &Path) -> Result<PathBuf, String> {
    let project = crate::core::pathutil::canonicalize_secure(project)
        .map_err(|_| "policy project root unavailable")?;
    if !project.is_dir() {
        return Err("policy project root unavailable".into());
    }
    Ok(project)
}

/// Whether the bound request project (if any) is `root`, a root already in
/// the one spelling of `canonical_project_root`. The bound side is resolved
/// the same way first: a caller may have bound another spelling of the same
/// directory (on Windows the verbatim `\\?\` form of `std::fs::canonicalize`).
/// No bound request project: `true`, the operation's own root governs.
pub(crate) fn request_project_is(root: &Path) -> bool {
    match REQUEST_PROJECT.try_with(|slot| slot.borrow().clone()) {
        Ok(Some(bound)) => canonical_project_root(&bound).is_ok_and(|bound| bound == root),
        _ => true,
    }
}

/// Run a source operation with an explicit fallback project root. An existing
/// request project remains authoritative; the fallback only fills an unbound
/// request scope.
pub(crate) fn with_project_source_view<T>(
    project_root: &str,
    operation: impl FnOnce() -> T,
) -> Result<T, String> {
    let project = REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
        .or_else(|| current_source_view().map(|view| view.project))
        .unwrap_or_else(|| PathBuf::from(project_root));
    let project = canonical_project_root(&project)?;
    REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(Some(project)), || {
        with_source_view(operation)
    })
}

/// Run an egress preparation for one explicitly selected project. Unlike the
/// source-view fallback helper, this rejects a conflicting trusted request root
/// and revalidates authority after the operation before its result is published.
pub(crate) fn with_explicit_project_source_view<T>(
    project_root: &Path,
    operation: impl FnOnce(&PublicationAuthority) -> T,
) -> Result<T, String> {
    let project = canonical_project_root(project_root)?;

    let bound = REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten();
    if let Some(bound) = bound {
        let bound = crate::core::pathutil::canonicalize_secure(&bound)
            .map_err(|_| "request project authority unavailable")?;
        if bound != project {
            return Err("request project authority does not match egress root".into());
        }
    }
    if let Some(view) = current_source_view()
        && (!view.live.load(Ordering::Acquire)
            || !view.valid.load(Ordering::Acquire)
            || view.project != project)
    {
        view.invalidate_if_live();
        return Err("source policy view does not match egress root".into());
    }

    REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(Some(project)), || {
        with_source_view(|| {
            let authority = PublicationAuthority::capture()?;
            let result = operation(&authority);
            authority.verify()?;
            Ok(result)
        })?
    })
}

/// Run a synchronous source operation against one freshly verified policy
/// snapshot. Publish the returned value only if the same authority still holds.
/// Nested calls reuse the parent snapshot; they never refresh into a weaker view.
pub(crate) fn with_source_view<T>(operation: impl FnOnce() -> T) -> Result<T, String> {
    with_source_view_when(|_, _| true, operation)?
        .ok_or_else(|| "source policy view is no longer active".into())
}

/// [`with_source_view`], entered only when the verification the view is
/// pinned to established something to act under: a policy, or an observed
/// organization decision (a signed unpin leaves no policy and is one all the
/// same). `Ok(None)`: it established neither, no view was opened and
/// `operation` did not run.
///
/// The decision whether to enter and the view are made of ONE verification:
/// there is no probe first and a second resolution after, so what the caller
/// branched on is what its view holds. Everything else is `with_source_view`
/// — the nested-view rules, the closing recheck and the horizon. Inside,
/// [`is_active`] and [`active_observed`] read the held view.
#[cfg(feature = "http-server")]
pub(crate) fn with_governed_source_view<T>(
    operation: impl FnOnce() -> T,
) -> Result<Option<T>, String> {
    with_source_view_when(|policy, observed| policy || observed, operation)
}

/// `enter(policy present, organization decision observed)` is asked once, of
/// the view that would be used; `false` leaves without running `operation`.
fn with_source_view_when<T>(
    enter: impl FnOnce(bool, bool) -> bool,
    operation: impl FnOnce() -> T,
) -> Result<Option<T>, String> {
    if let Some(view) = current_source_view() {
        let project = REQUEST_PROJECT
            .try_with(|slot| slot.borrow().clone())
            .ok()
            .flatten()
            .ok_or("source policy view is no longer active")?;
        let project = canonical_project_root(&project)
            .map_err(|_| "source policy view is no longer active")?;
        if !view.live.load(Ordering::Acquire)
            || !view.valid.load(Ordering::Acquire)
            || project != view.project
            || !view.within_horizon()
        {
            view.invalidate_if_live();
            return Err("source policy view is no longer active".into());
        }
        if !enter(view.policy.is_some(), view.observation.is_some()) {
            return Ok(None);
        }
        return REQUEST_PROJECT
            .sync_scope(std::cell::RefCell::new(Some(view.project.clone())), || {
                Ok(Some(operation()))
            });
    }

    // A request bound to no project gets no source view: its project is not
    // invented from the process's working directory (AP01 §3). Only work
    // outside any request (the CLI) acts for the directory it runs in.
    let project = match REQUEST_PROJECT.try_with(|slot| slot.borrow().clone()) {
        Ok(project) => project.ok_or("project_unresolved")?,
        Err(_) => std::env::current_dir().map_err(|_| "policy project root unavailable")?,
    };
    let project = canonical_project_root(&project)?;
    REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(Some(project.clone())), || {
        let verified = refresh::current_verified_settled(&project)?;
        let lifetime = verified.lifetime;
        let observation = verified.observation;
        let role = crate::core::roles::active_role();
        let pinned_role_fingerprint = role_fingerprint(&role)?;
        let view = SourceView {
            project: project.clone(),
            policy: source_view_policy(verified.policy),
            fingerprint: verified.fingerprint,
            role,
            role_fingerprint: pinned_role_fingerprint,
            lifetime,
            observation,
            valid: Arc::new(AtomicBool::new(true)),
            live: Arc::new(AtomicBool::new(true)),
        };

        // The reads since the verification took time: the operation is not
        // entered on a floor whose horizon has already passed.
        if !view.within_horizon() {
            return Err("source policy view is no longer active".into());
        }
        // Asked of this very verification, before anything runs on it.
        if !enter(view.policy.is_some(), view.observation.is_some()) {
            return Ok(None);
        }
        let view_lifetime = SourceViewLifetime(view.live.clone());
        let value = SOURCE_VIEW.sync_scope(std::cell::RefCell::new(Some(view.clone())), operation);
        drop(view_lifetime);

        // SOURCE_VIEW has ended, while REQUEST_PROJECT still identifies the
        // same request root; re-resolve the complete role before publishing.
        let project_unchanged = REQUEST_PROJECT
            .try_with(|slot| slot.borrow().as_ref() == Some(&view.project))
            .unwrap_or(false);
        let latest_role = crate::core::roles::active_role();
        let latest_role_fingerprint = role_fingerprint(&latest_role)?;
        let latest = refresh::current_verified_settled(&project)?;
        if !project_unchanged
            || latest.fingerprint != view.fingerprint
            || latest_role_fingerprint != view.role_fingerprint
            || !view.valid.load(Ordering::Acquire)
        {
            return Err("source policy view changed during operation".into());
        }
        // Last, after every closing wait: the operation ran on a floor signed
        // up to a horizon. If that passed in the meantime its result is not
        // published, whatever the fresh verification above said.
        if !view.within_horizon() {
            return Err("source policy view changed during operation".into());
        }
        Ok(Some(value))
    })
}

pub(crate) fn role_fingerprint(role: &crate::core::roles::Role) -> Result<blake3::Hash, String> {
    serde_json::to_vec(role)
        .map(|bytes| blake3::hash(&bytes))
        .map_err(|_| "source role view unavailable".into())
}

#[cfg(test)]
fn source_view_policy(fresh: Option<Arc<ActivePolicy>>) -> Option<Arc<ActivePolicy>> {
    let state = test_override()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match &*state {
        OverrideState::Live => fresh,
        OverrideState::Fixed(policy) => policy.clone(),
    }
}

#[cfg(not(test))]
fn source_view_policy(fresh: Option<Arc<ActivePolicy>>) -> Option<Arc<ActivePolicy>> {
    fresh
}

/// Project-local pack location, relative to the working directory (matches the
/// `lean-ctx policy` CLI's `PROJECT_PACK_PATH`).
const PROJECT_PACK_PATH: &str = ".lean-ctx/policy.toml";

/// A resolved policy plus its precompiled redaction regexes — the cached,
/// hot-path-ready form.
pub struct ActivePolicy {
    pub resolved: ResolvedPolicy,
    /// `(label, compiled regex)` — labels are the pack's `[redaction]` keys.
    /// Invalid/over-budget compilation makes the whole policy fail closed.
    pub redaction: Vec<(String, Regex)>,
    pub(crate) blocked_patterns: Vec<(String, Regex)>,
    /// How many of `redaction` + `blocked_patterns` a Hive filter pack
    /// added; they have their own budget next to the pack's.
    pub(crate) hive_rules: usize,
    pub(crate) content_valid: bool,
    /// Compiled inbound content filters (GL #675), built once from the pack's
    /// `[filters]` section.
    pub filters: FilterConfig,
    /// Compiled egress/output DLP config (GL #676), built once from the pack's
    /// `[egress]` section.
    pub egress: crate::core::egress::EgressConfig,
    /// The signed instant this policy's organization floor runs out at, set
    /// before the policy is shared and never changed. `None` for every policy
    /// that rests on no verifier decision: local, version one, or a lifted
    /// requirement. The fields above describe the policy; they authorize
    /// nothing once this instant has passed — see [`Self::within_horizon`].
    horizon: Option<chrono::DateTime<chrono::Utc>>,
    /// Set the first time the horizon is observed to have passed, and never
    /// cleared. It lives in the policy itself, so every holder of the same
    /// shared policy sees it: once expired, a clock set back revives nothing.
    expired: AtomicBool,
}

impl ActivePolicy {
    /// Whether this policy may still be acted on: read on the real clock at
    /// every use, so a policy that is kept — in a view, a snapshot or a
    /// caller's variable — ends with its signed horizon and not with the
    /// request that obtained it. Exclusive, like the verifier's own check.
    /// An expiry once observed is final for this policy.
    #[must_use]
    pub fn within_horizon(&self) -> bool {
        if self.expired.load(Ordering::Acquire) {
            return false;
        }
        let holds = self
            .horizon
            .is_none_or(|horizon| chrono::Utc::now() < horizon);
        if !holds {
            self.expired.store(true, Ordering::Release);
        }
        holds
    }

    /// This policy, ending at `horizon`. Only for a policy not yet shared.
    fn until(mut self, horizon: Option<chrono::DateTime<chrono::Utc>>) -> Self {
        self.horizon = horizon;
        self
    }

    pub(crate) fn from_resolved(resolved: ResolvedPolicy) -> Self {
        Self::from_resolved_with_hive(resolved, &crate::core::hive::policy::FilterRules::default())
    }

    /// As [`Self::from_resolved`], with the rules of the installed Hive
    /// filter packs added under their own budget. A Hive rule over its
    /// bounds fails the policy closed like a pack rule; installation
    /// refuses such sets, so this is defensive.
    pub(crate) fn from_resolved_with_hive(
        mut resolved: ResolvedPolicy,
        hive: &crate::core::hive::policy::FilterRules,
    ) -> Self {
        use super::content::{MAX_REDACTION_RULES, MAX_RULE_LABEL_BYTES, MAX_RULE_PATTERN_BYTES};
        let hive_rules = hive.redaction.len() + hive.blocked.len();
        let hive_bounded = hive_rules <= crate::core::hive::policy::HIVE_MAX_RULES
            && hive
                .redaction
                .iter()
                .chain(&hive.blocked)
                .all(|(label, pattern)| {
                    label.starts_with(super::HIVE_LABEL_PREFIX)
                        && label.len() <= MAX_RULE_LABEL_BYTES
                        && !label.chars().any(char::is_control)
                        && pattern.len() <= MAX_RULE_PATTERN_BYTES
                });
        let bounded = hive_bounded
            && resolved.redaction.len() + resolved.filters.blocked_patterns.len()
                <= MAX_REDACTION_RULES
            && resolved.egress.forbidden_patterns.len() <= MAX_REDACTION_RULES
            && resolved
                .egress
                .forbidden_patterns
                .iter()
                .all(|pattern| pattern.len() <= MAX_RULE_PATTERN_BYTES)
            && resolved
                .redaction
                .iter()
                .chain(&resolved.filters.blocked_patterns)
                .all(|(label, pattern)| {
                    !label.is_empty()
                        && label.len() <= MAX_RULE_LABEL_BYTES
                        && !label.chars().any(char::is_control)
                        && pattern.len() <= MAX_RULE_PATTERN_BYTES
                });
        // Pack labels never start with the Hive prefix (pack validation), so
        // a Hive rule neither replaces nor is replaced by a pack rule.
        resolved.redaction.extend(hive.redaction.iter().cloned());
        resolved
            .filters
            .blocked_patterns
            .extend(hive.blocked.iter().cloned());
        let compiled = bounded.then(|| {
            resolved
                .redaction
                .iter()
                .map(|(label, pattern)| {
                    regex::RegexBuilder::new(pattern)
                        .size_limit(1024 * 1024)
                        .dfa_size_limit(1024 * 1024)
                        .build()
                        .map(|re| (label.clone(), re))
                })
                .collect::<Result<Vec<_>, _>>()
        });
        let content_valid = matches!(&compiled, Some(Ok(_)));
        let redaction = compiled.and_then(Result::ok).unwrap_or_default();
        let compiled_blocks = bounded.then(|| {
            resolved
                .filters
                .blocked_patterns
                .iter()
                .map(|(label, pattern)| {
                    regex::RegexBuilder::new(pattern)
                        .size_limit(1024 * 1024)
                        .dfa_size_limit(1024 * 1024)
                        .build()
                        .map(|re| (label.clone(), re))
                })
                .collect::<Result<Vec<_>, _>>()
        });
        let content_valid = content_valid && matches!(&compiled_blocks, Some(Ok(_)));
        let blocked_patterns = compiled_blocks.and_then(Result::ok).unwrap_or_default();
        let filters = FilterConfig::new(
            filter_action(resolved.filters.pii.as_ref()),
            filter_action(resolved.filters.classification.as_ref()),
            filter_action(resolved.filters.injection.as_ref()),
            &resolved.filters.blocked_labels,
        );
        let egress = crate::core::egress::EgressConfig::new(
            &resolved.egress.forbidden_patterns,
            resolved.egress.block_secrets.unwrap_or(false),
            resolved.egress.max_writes_per_min,
        );
        Self {
            resolved,
            redaction,
            blocked_patterns,
            hive_rules,
            content_valid,
            filters,
            egress,
            horizon: None,
            expired: AtomicBool::new(false),
        }
    }

    /// Block every tool after a local policy pack fails to load.
    pub(crate) fn deny_all() -> Self {
        let mut active = Self::from_resolved(ResolvedPolicy {
            name: "invalid-local-policy".into(),
            version: "0.0.0".into(),
            description: "deny all because the local policy pack is invalid".into(),
            chain: Vec::new(),
            default_read_mode: None,
            allow_tools: Some(Vec::new()),
            deny_tools: Vec::new(),
            max_context_tokens: None,
            audit_retention_days: None,
            redaction: std::collections::BTreeMap::new(),
            filters: crate::core::policy::FilterRules::default(),
            egress: crate::core::policy::EgressRules::default(),
            routing: crate::core::policy::RoutingPolicyRules::default(),
            budgets: crate::core::policy::BudgetRules::default(),
        });
        active.content_valid = false;
        active
    }

    /// Whether `tool` is permitted by this policy's allow/deny lists.
    /// `deny_tools` always wins; an `allow_tools` allowlist, when set, is
    /// exclusive (only listed tools pass).
    #[must_use]
    pub fn tool_allowed(&self, tool: &str) -> bool {
        if !self.content_valid || !self.within_horizon() {
            return false;
        }
        if self.resolved.deny_tools.iter().any(|t| t == tool) {
            return false;
        }
        match &self.resolved.allow_tools {
            Some(allow) => allow.iter().any(|t| t == tool),
            None => true,
        }
    }
}

/// Map a resolved `[filters]` action string to a [`FilterAction`]. Absent or
/// (defensively) unparseable ⇒ `Off`; validation already rejects bad tokens.
fn filter_action(opt: Option<&String>) -> FilterAction {
    opt.map(String::as_str)
        .and_then(FilterAction::parse)
        .unwrap_or(FilterAction::Off)
}

#[cfg(test)]
fn load_from_disk() -> Option<Arc<ActivePolicy>> {
    let local = match load_local_pack() {
        Ok(local) => local,
        Err(msg) => {
            tracing::error!(
                "policy: failed to load invalid {PROJECT_PACK_PATH} ({msg}); enforcing deny-all"
            );
            return Some(Arc::new(ActivePolicy::deny_all()));
        }
    };
    apply_org_floor(
        local,
        crate::core::policy::org::active_resolved_checked(),
        None,
        &crate::core::hive::policy::FilterRules::default(),
        None,
    )
}

/// `horizon` is the signed end of the organization floor, when a verifier
/// decided it; it is attached here, before the policy is shared.
/// `hive_floor` is the Hive effective policy (Release B): folded under
/// everything else, so nothing local can loosen it.
fn apply_org_floor(
    local: Option<ResolvedPolicy>,
    org: Result<Option<ResolvedPolicy>, String>,
    horizon: Option<chrono::DateTime<chrono::Utc>>,
    hive: &crate::core::hive::policy::FilterRules,
    hive_floor: Option<&ResolvedPolicy>,
) -> Option<Arc<ActivePolicy>> {
    // Invalid configured org policy must not silently remove the org floor.
    let effective = match org {
        Ok(Some(org)) => Some(crate::core::policy::floor::merge_floor(
            &org,
            local.as_ref(),
        )),
        Ok(None) => local,
        Err(error) => {
            tracing::error!("org policy rejected; enforcing deny-all: {error}");
            return Some(Arc::new(ActivePolicy::deny_all()));
        }
    };
    let effective = match hive_floor {
        Some(floor) => Some(crate::core::hive::effective_floor::apply(
            floor,
            effective.as_ref(),
        )),
        None => effective,
    };
    let effective = match effective {
        Some(effective) => effective,
        None if hive.redaction.is_empty() && hive.blocked.is_empty() => return None,
        // Hive filter packs alone: content rules only, no tool or path rule.
        None => hive_filters_only(),
    };
    Some(Arc::new(
        ActivePolicy::from_resolved_with_hive(effective, hive).until(horizon),
    ))
}

/// The policy that carries nothing but the rules of Hive filter packs.
fn hive_filters_only() -> ResolvedPolicy {
    ResolvedPolicy {
        name: "hive-filters".into(),
        version: "1.0.0".into(),
        description: "content rules of the installed Hive filter packs".into(),
        chain: Vec::new(),
        default_read_mode: None,
        allow_tools: None,
        deny_tools: Vec::new(),
        max_context_tokens: None,
        audit_retention_days: None,
        redaction: std::collections::BTreeMap::new(),
        filters: crate::core::policy::FilterRules::default(),
        egress: crate::core::policy::EgressRules::default(),
        routing: crate::core::policy::RoutingPolicyRules::default(),
        budgets: crate::core::policy::BudgetRules::default(),
    }
}

/// The project-local pack (`.lean-ctx/policy.toml`), resolved. `None` only when
/// the file is absent; malformed packs return an error so callers fail closed.
#[cfg(test)]
fn load_local_pack() -> Result<Option<ResolvedPolicy>, String> {
    let path = PathBuf::from(PROJECT_PACK_PATH);
    load_local_pack_at(&path)
}

#[cfg(test)]
fn load_local_pack_at(path: &Path) -> Result<Option<ResolvedPolicy>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    }
    match parse_file(path).and_then(|p| resolve(&p)) {
        Ok(resolved) => Ok(Some(resolved)),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Resolve the explicit runtime project without using the process-global cache.
/// Configured local/org errors remain errors rather than granting admission.
pub fn for_project(project_root: &Path) -> Result<Option<Arc<ActivePolicy>>, String> {
    refresh::current(project_root.to_path_buf())
}

/// The active resolved policy, or `None` when no project or org pack exists.
/// Fresh source bytes are mandatory even when compiled rules are cached.
#[must_use]
pub fn active() -> Option<Arc<ActivePolicy>> {
    active_observed().0
}

/// [`active`], together with the organization decision the verifier stated
/// in the very verification that policy comes from — one read, one answer,
/// no second look at policy, trust store or marker. For whoever records what
/// it decided under: the observation it files is the one of its own
/// snapshot, never a later one.
///
/// The observation is `None` whenever no verifier was asked (no organization
/// policy, a version-one one) and whenever the policy is the deny-all that
/// stands for an authority that could not be established: a refusal records
/// no organization decision it did not get. A lifted requirement is observed
/// although it leaves no floor, and so possibly no policy at all.
#[must_use]
pub(crate) fn active_observed() -> (Option<Arc<ActivePolicy>>, Option<super::org::Observation>) {
    let deny_all = || (Some(Arc::new(ActivePolicy::deny_all())), None);
    if let Some(view) = current_source_view() {
        if !view.live.load(Ordering::Acquire) || !view.valid.load(Ordering::Acquire) {
            return deny_all();
        }
        let project = REQUEST_PROJECT
            .try_with(|slot| slot.borrow().clone())
            .ok()
            .flatten();
        if project.as_ref() != Some(&view.project) || !view.within_horizon() {
            view.invalidate_if_live();
            return deny_all();
        }
        return (view.policy, view.observation);
    }
    #[cfg(test)]
    {
        let state = test_override()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let OverrideState::Fixed(policy) = &*state {
            return (policy.clone(), None);
        }
    }
    let scoped = REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten();
    match scoped
        .map_or_else(std::env::current_dir, Ok)
        .map_err(|_| "policy project root unavailable".into())
        .and_then(refresh::current_verified)
    {
        Ok(verified) => (verified.policy, verified.observation),
        Err(_) => deny_all(),
    }
}

/// Run `operation` under a pinned view of `project` whose verification
/// observed `observation` and left no policy — the state after a signed
/// unpin on an endpoint without a project pack. Tests only: a real view comes
/// from `with_source_view` and nowhere else.
#[cfg(test)]
pub(crate) fn with_observed_view_for_test<T>(
    project: &Path,
    observation: Option<super::org::Observation>,
    operation: impl FnOnce() -> T,
) -> T {
    let project = canonical_project_root(project).expect("test project root");
    let role = crate::core::roles::active_role();
    // The authority as it really verifies for this root (no organization
    // artifact in a test's data directory), so that a holder's own closing
    // verification finds the same fingerprint; only the observation is given.
    let verified = refresh::current_verified(project.clone()).expect("test authority");
    let view = SourceView {
        project: project.clone(),
        policy: verified.policy,
        fingerprint: verified.fingerprint,
        role_fingerprint: role_fingerprint(&role).expect("test role"),
        role,
        lifetime: None,
        observation,
        valid: Arc::new(AtomicBool::new(true)),
        live: Arc::new(AtomicBool::new(true)),
    };
    REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(Some(project)), || {
        SOURCE_VIEW.sync_scope(std::cell::RefCell::new(Some(view)), operation)
    })
}

/// What a source view that is already held says about the organization.
pub(crate) enum HeldObservation {
    /// No usable view is held here: nothing can be said, and nothing is.
    NotHeld,
    /// The held view's own observation (`None`: its verification asked no
    /// verifier).
    Held(Option<super::org::Observation>),
}

/// The organization observation of the source view the caller already runs
/// under — and nothing else. It never resolves, refreshes or reads: without a
/// held view that is live, valid, of the request's own project and within its
/// signed horizon, the answer is [`HeldObservation::NotHeld`]. For a path
/// that must not start a verification of its own but may record the decision
/// its caller's view was verified with.
pub(crate) fn held_observation() -> HeldObservation {
    let Some(view) = current_source_view() else {
        return HeldObservation::NotHeld;
    };
    let project = REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten();
    if !view.live.load(Ordering::Acquire)
        || !view.valid.load(Ordering::Acquire)
        || project.as_ref() != Some(&view.project)
        || !view.within_horizon()
    {
        return HeldObservation::NotHeld;
    }
    HeldObservation::Held(view.observation)
}

/// Current "is a policy active?" probe; refresh failure requires protection.
#[must_use]
pub fn is_active() -> bool {
    active().is_some()
}

/// Re-read the project pack (e.g. after a `policy` edit). Idempotent.
pub fn reload() {
    let _ = active();
}

#[cfg(test)]
enum OverrideState {
    Live,
    Fixed(Option<Arc<ActivePolicy>>),
}

#[cfg(test)]
fn test_override() -> &'static RwLock<OverrideState> {
    static OVERRIDE: OnceLock<RwLock<OverrideState>> = OnceLock::new();
    OVERRIDE.get_or_init(|| RwLock::new(OverrideState::Live))
}

#[cfg(test)]
fn test_override_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// The policy-override lock together with the test environment lock, in the
/// one order every test takes them: environment first, then policy. Held as
/// one value so that neither outlives the other; the fields are declared in
/// release order — policy before environment.
#[cfg(test)]
pub(crate) struct TestOverrideGuard {
    _policy: std::sync::MutexGuard<'static, ()>,
    _environment: crate::core::data_dir::TestEnvGuard,
}

#[cfg(test)]
pub(crate) fn lock_test_override() -> TestOverrideGuard {
    let environment = crate::core::data_dir::test_env_lock();
    let policy = test_override_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    TestOverrideGuard {
        _policy: policy,
        _environment: environment,
    }
}

/// Process-wide test override held under a shared lock for its whole lifetime.
#[cfg(test)]
pub struct TestPolicyOverride {
    previous: OverrideState,
    _lock: TestOverrideGuard,
}

#[cfg(test)]
impl TestPolicyOverride {
    pub fn set(resolved: Option<ResolvedPolicy>) -> Self {
        let lock = lock_test_override();
        let previous = std::mem::replace(
            &mut *test_override()
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            OverrideState::Fixed(
                resolved.map(|policy| Arc::new(ActivePolicy::from_resolved(policy))),
            ),
        );
        Self {
            previous,
            _lock: lock,
        }
    }
}

#[cfg(test)]
impl Drop for TestPolicyOverride {
    fn drop(&mut self) {
        *test_override()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            std::mem::replace(&mut self.previous, OverrideState::Live);
    }
}

#[cfg(test)]
#[path = "runtime/tests.rs"]
mod tests;
