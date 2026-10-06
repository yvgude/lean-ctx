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
    valid: Arc<AtomicBool>,
    live: Arc<AtomicBool>,
}

struct SourceViewLifetime(Arc<AtomicBool>);

/// Revalidate buffered CLI data at its output and persistence boundaries.
/// This does not lock policy files against a concurrent administrator write.
#[derive(Debug)]
pub(crate) struct PublicationAuthority {
    project: PathBuf,
    fingerprint: blake3::Hash,
    role_fingerprint: blake3::Hash,
    valid: Arc<AtomicBool>,
}

impl PublicationAuthority {
    pub(crate) fn capture() -> Result<Self, String> {
        let view = current_source_view().ok_or("source authority unavailable")?;
        Ok(Self {
            project: view.project,
            fingerprint: view.fingerprint,
            role_fingerprint: view.role_fingerprint,
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
                Ok(())
            })
        })
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
            view.valid.store(false, Ordering::Release);
            crate::core::roles::closed_source_view_role()
        } else {
            view.role
        }
    })
}

pub(crate) fn bind_request_project(project: Option<&str>) {
    if current_source_view().is_some() {
        return;
    }
    let _ = REQUEST_PROJECT.try_with(|slot| {
        *slot.borrow_mut() = project.map(PathBuf::from);
    });
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
    REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(Some(project)), || {
        with_source_view(operation)
    })
}

/// Run a synchronous source operation against one freshly verified policy
/// snapshot. Publish the returned value only if the same authority still holds.
/// Nested calls reuse the parent snapshot; they never refresh into a weaker view.
pub(crate) fn with_source_view<T>(operation: impl FnOnce() -> T) -> Result<T, String> {
    if let Some(view) = current_source_view() {
        let project = REQUEST_PROJECT
            .try_with(|slot| slot.borrow().clone())
            .ok()
            .flatten();
        if !view.live.load(Ordering::Acquire)
            || !view.valid.load(Ordering::Acquire)
            || project.as_ref() != Some(&view.project)
        {
            view.valid.store(false, Ordering::Release);
            return Err("source policy view is no longer active".into());
        }
        return Ok(operation());
    }

    let project = REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
        .or_else(|| std::env::current_dir().ok())
        .ok_or("policy project root unavailable")?;
    REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(Some(project.clone())), || {
        let verified = refresh::current_verified(project.clone())?;
        let role = crate::core::roles::active_role();
        let pinned_role_fingerprint = role_fingerprint(&role)?;
        let view = SourceView {
            project: project.clone(),
            policy: source_view_policy(verified.policy),
            fingerprint: verified.fingerprint,
            role,
            role_fingerprint: pinned_role_fingerprint,
            valid: Arc::new(AtomicBool::new(true)),
            live: Arc::new(AtomicBool::new(true)),
        };

        let lifetime = SourceViewLifetime(view.live.clone());
        let value = SOURCE_VIEW.sync_scope(std::cell::RefCell::new(Some(view.clone())), operation);
        drop(lifetime);

        // SOURCE_VIEW has ended, while REQUEST_PROJECT still identifies the
        // same request root; re-resolve the complete role before publishing.
        let project_unchanged = REQUEST_PROJECT
            .try_with(|slot| slot.borrow().as_ref() == Some(&view.project))
            .unwrap_or(false);
        let latest_role = crate::core::roles::active_role();
        let latest_role_fingerprint = role_fingerprint(&latest_role)?;
        let latest = refresh::current_verified(project)?;
        if !project_unchanged
            || latest.fingerprint != view.fingerprint
            || latest_role_fingerprint != view.role_fingerprint
            || !view.valid.load(Ordering::Acquire)
        {
            return Err("source policy view changed during operation".into());
        }
        Ok(value)
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
    pub(crate) content_valid: bool,
    /// Compiled inbound content filters (GL #675), built once from the pack's
    /// `[filters]` section.
    pub filters: FilterConfig,
    /// Compiled egress/output DLP config (GL #676), built once from the pack's
    /// `[egress]` section.
    pub egress: crate::core::egress::EgressConfig,
}

impl ActivePolicy {
    pub(crate) fn from_resolved(resolved: ResolvedPolicy) -> Self {
        use super::content::{MAX_REDACTION_RULES, MAX_RULE_LABEL_BYTES, MAX_RULE_PATTERN_BYTES};
        let bounded = resolved.redaction.len() + resolved.filters.blocked_patterns.len()
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
            content_valid,
            filters,
            egress,
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
        if !self.content_valid {
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
    apply_org_floor(local, crate::core::policy::org::active_resolved_checked())
}

fn apply_org_floor(
    local: Option<ResolvedPolicy>,
    org: Result<Option<ResolvedPolicy>, String>,
) -> Option<Arc<ActivePolicy>> {
    // Invalid configured org policy must not silently remove the org floor.
    let effective = match org {
        Ok(Some(org)) => crate::core::policy::floor::merge_floor(&org, local.as_ref()),
        Ok(None) => local?,
        Err(error) => {
            tracing::error!("org policy rejected; enforcing deny-all: {error}");
            return Some(Arc::new(ActivePolicy::deny_all()));
        }
    };
    Some(Arc::new(ActivePolicy::from_resolved(effective)))
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
    if let Some(view) = current_source_view() {
        if !view.live.load(Ordering::Acquire) || !view.valid.load(Ordering::Acquire) {
            return Some(Arc::new(ActivePolicy::deny_all()));
        }
        let project = REQUEST_PROJECT
            .try_with(|slot| slot.borrow().clone())
            .ok()
            .flatten();
        if project.as_ref() != Some(&view.project) {
            view.valid.store(false, Ordering::Release);
            return Some(Arc::new(ActivePolicy::deny_all()));
        }
        return view.policy;
    }
    #[cfg(test)]
    {
        let state = test_override()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let OverrideState::Fixed(policy) = &*state {
            return policy.clone();
        }
    }
    let scoped = REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten();
    match scoped
        .map_or_else(std::env::current_dir, Ok)
        .map_err(|_| "policy project root unavailable".into())
        .and_then(refresh::current)
    {
        Ok(policy) => policy,
        Err(_) => Some(Arc::new(ActivePolicy::deny_all())),
    }
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

#[cfg(test)]
pub(crate) fn lock_test_override() -> std::sync::MutexGuard<'static, ()> {
    test_override_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Process-wide test override held under a shared lock for its whole lifetime.
#[cfg(test)]
pub struct TestPolicyOverride {
    previous: OverrideState,
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl TestPolicyOverride {
    pub fn set(resolved: Option<ResolvedPolicy>) -> Self {
        let lock = test_override_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;

    struct CurrentDirGuard {
        previous: std::path::PathBuf,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl CurrentDirGuard {
        fn enter(dir: &std::path::Path) -> Self {
            static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
            let lock = LOCK.get_or_init(|| std::sync::Mutex::new(()));
            let guard = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous = std::env::current_dir().expect("read current directory");
            std::env::set_current_dir(dir).expect("enter temporary directory");
            Self {
                previous,
                _lock: guard,
            }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.previous).expect("restore current directory");
        }
    }

    fn rp(allow: Option<Vec<&str>>, deny: Vec<&str>, redaction: &[(&str, &str)]) -> ResolvedPolicy {
        ResolvedPolicy {
            name: "test".into(),
            version: "1.0.0".into(),
            description: "t".into(),
            chain: vec![],
            default_read_mode: None,
            allow_tools: allow.map(|a| a.into_iter().map(String::from).collect()),
            deny_tools: deny.into_iter().map(String::from).collect(),
            max_context_tokens: None,
            audit_retention_days: None,
            redaction: redaction
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect::<BTreeMap<_, _>>(),
            filters: crate::core::policy::FilterRules::default(),
            egress: crate::core::policy::EgressRules::default(),
            routing: crate::core::policy::RoutingPolicyRules::default(),
            budgets: crate::core::policy::BudgetRules::default(),
        }
    }

    fn write_policy(root: &std::path::Path, name: &str) -> std::path::PathBuf {
        let directory = root.join(".lean-ctx");
        fs::create_dir_all(&directory).expect("create policy directory");
        let path = directory.join("policy.toml");
        fs::write(
            &path,
            format!("name = {name:?}\nversion = \"1.0.0\"\ndescription = \"source view test\"\n"),
        )
        .expect("write policy");
        path
    }

    #[test]
    fn deny_list_blocks_listed_tool() {
        let p = ActivePolicy::from_resolved(rp(None, vec!["ctx_url_read"], &[]));
        assert!(!p.tool_allowed("ctx_url_read"));
        assert!(p.tool_allowed("ctx_read"));
    }

    #[test]
    fn allow_list_is_exclusive() {
        let p = ActivePolicy::from_resolved(rp(Some(vec!["ctx_read"]), vec![], &[]));
        assert!(p.tool_allowed("ctx_read"));
        assert!(!p.tool_allowed("ctx_shell"));
    }

    #[test]
    fn compiles_redaction_patterns() {
        let p = ActivePolicy::from_resolved(rp(None, vec![], &[("emp", r"EMP-\d{4}")]));
        assert_eq!(p.redaction.len(), 1);
        assert_eq!(p.redaction[0].0, "emp");
    }

    #[test]
    fn load_from_disk_fails_closed_on_invalid_pack() {
        let temp = tempfile::tempdir().expect("create temporary directory");
        let policy_dir = temp.path().join(".lean-ctx");
        fs::create_dir(&policy_dir).expect("create policy directory");
        fs::write(policy_dir.join("policy.toml"), "[policy\n").expect("write invalid policy");
        let _cwd = CurrentDirGuard::enter(temp.path());

        let policy = load_from_disk().expect("invalid pack activates deny-all");

        assert!(!policy.tool_allowed("ctx_read"));
        assert!(!policy.tool_allowed("ctx_shell"));
    }

    #[test]
    fn invalid_org_policy_cannot_fall_back_to_permissive_local_policy() {
        for local in [None, Some(rp(None, vec![], &[]))] {
            let policy = apply_org_floor(local, Err("invalid org signature".into())).unwrap();
            assert!(!policy.tool_allowed("ctx_read"));
            assert!(!policy.tool_allowed("ctx_shell"));
        }
        assert!(apply_org_floor(None, Ok(None)).is_none());
    }

    #[test]
    fn explicit_project_policy_does_not_leak_between_roots() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let denied = tempfile::tempdir().unwrap();
        let absent = tempfile::tempdir().unwrap();
        let directory = denied.path().join(".lean-ctx");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("policy.toml"),
            "name = \"project\"\nversion = \"1.0.0\"\ndescription = \"test\"\n[context]\ndeny_tools = [\"ctx_work_graph\"]\n").unwrap();
        assert!(
            !for_project(denied.path())
                .unwrap()
                .unwrap()
                .tool_allowed("ctx_work_graph")
        );
        assert!(for_project(absent.path()).unwrap().is_none());
        fs::write(directory.join("policy.toml"), "invalid policy").unwrap();
        assert!(for_project(denied.path()).is_err());
        assert!(for_project(&absent.path().join("missing")).is_err());
    }

    #[test]
    fn source_view_pins_active_policy_and_rejects_persistent_change() {
        let _override_guard = lock_test_override();
        let _data = crate::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().expect("create project");
        let path = write_policy(root.path(), "view-a");

        let result = REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(root.path().to_path_buf())),
            || {
                with_source_view(|| {
                    assert_eq!(active().unwrap().resolved.name, "view-a");
                    write_policy(root.path(), "view-b");
                    assert_eq!(active().unwrap().resolved.name, "view-a");
                })
            },
        );

        assert!(result.is_err());
        assert!(path.exists());
    }

    #[test]
    fn source_view_a_to_b_to_a_never_uses_transient_policy_b() {
        let _override_guard = lock_test_override();
        let _data = crate::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().expect("create project");
        write_policy(root.path(), "view-a");

        let result = REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(root.path().to_path_buf())),
            || {
                with_source_view(|| {
                    write_policy(root.path(), "view-b");
                    assert_eq!(active().unwrap().resolved.name, "view-a");
                    write_policy(root.path(), "view-a");
                    active().unwrap().resolved.name.clone()
                })
            },
        );

        assert_eq!(result.unwrap(), "view-a");
    }

    #[test]
    fn source_view_pins_explicit_no_policy_and_rejects_policy_creation() {
        let _override_guard = lock_test_override();
        let _data = crate::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().expect("create project");

        let result = REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(root.path().to_path_buf())),
            || {
                with_source_view(|| {
                    assert!(active().is_none());
                    write_policy(root.path(), "new-policy");
                    assert!(active().is_none());
                })
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn source_view_rejects_nested_project_rebinding() {
        let _override_guard = lock_test_override();
        let _data = crate::core::data_dir::isolated_data_dir();
        let first = tempfile::tempdir().expect("create first project");
        let second = tempfile::tempdir().expect("create second project");
        write_policy(first.path(), "first-project");
        write_policy(second.path(), "second-project");

        let result = REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(first.path().to_path_buf())),
            || {
                with_source_view(|| {
                    REQUEST_PROJECT.sync_scope(
                        std::cell::RefCell::new(Some(second.path().to_path_buf())),
                        || assert!(!active().unwrap().tool_allowed("ctx_read")),
                    );
                })
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn source_view_rejects_policy_removal_and_invalid_initial_policy() {
        let _override_guard = lock_test_override();
        let _data = crate::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().expect("create project");
        let path = write_policy(root.path(), "view-a");

        let removed = REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(root.path().to_path_buf())),
            || {
                with_source_view(|| {
                    assert_eq!(active().unwrap().resolved.name, "view-a");
                    fs::remove_file(&path).expect("remove policy");
                    assert_eq!(active().unwrap().resolved.name, "view-a");
                })
            },
        );
        assert!(removed.is_err());

        fs::write(&path, "[policy\n").expect("write invalid policy");
        let called = std::cell::Cell::new(false);
        let invalid = REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(root.path().to_path_buf())),
            || with_source_view(|| called.set(true)),
        );
        assert!(invalid.is_err());
        assert!(!called.get());
    }

    fn fixture_role(
        name: &str,
        boundary_mode: &str,
        allow_secret_paths: bool,
        allowed: &[&str],
    ) -> crate::core::roles::Role {
        use crate::core::roles::{IoPolicy, Role, RoleLimits, RoleMeta, ToolPolicy};
        Role {
            role: RoleMeta {
                name: name.into(),
                inherits: None,
                description: "source view test".into(),
                shell_policy: "deny".into(),
            },
            tools: ToolPolicy {
                allowed: allowed.iter().map(|tool| (*tool).into()).collect(),
                denied: Vec::new(),
            },
            io: IoPolicy {
                boundary_mode: boundary_mode.into(),
                allow_secret_paths,
                ..Default::default()
            },
            limits: RoleLimits::default(),
        }
    }

    #[test]
    fn source_view_pins_full_role_through_a_to_b_to_a_and_rejects_persistent_change() {
        let _override_guard = lock_test_override();
        let _data = crate::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().expect("create project");
        let role_a = fixture_role("role-a", "warn", true, &["ctx_read"]);
        // Secret-path visibility stays fixed so the regression exercises the
        // full role fingerprint (tool and boundary policy), not only that bit.
        let role_b = fixture_role("role-b", "enforce", true, &["ctx_shell"]);

        let transient = crate::core::roles::with_test_active_role(role_a.clone(), || {
            with_project_source_view(root.path().to_str().unwrap(), || {
                with_source_view(|| {
                    assert_eq!(
                        role_fingerprint(&crate::core::roles::active_role()).unwrap(),
                        role_fingerprint(&role_a).unwrap()
                    );
                    crate::core::roles::set_test_active_role(role_b.clone());
                    let pinned = crate::core::roles::active_role();
                    assert_eq!(pinned.role.name, "role-a");
                    assert_eq!(pinned.io.boundary_mode, "warn");
                    assert!(pinned.io.allow_secret_paths);
                    assert!(pinned.is_tool_allowed("ctx_read"));
                    assert!(!pinned.is_tool_allowed("ctx_shell"));
                    crate::core::roles::set_test_active_role(role_a.clone());
                })
            })
        });
        assert!(transient.is_ok(), "a transient role B is never observed");

        let persistent = crate::core::roles::with_test_active_role(role_a.clone(), || {
            with_project_source_view(root.path().to_str().unwrap(), || {
                with_source_view(|| {
                    crate::core::roles::set_test_active_role(role_b.clone());
                    assert_eq!(crate::core::roles::active_role().role.name, "role-a");
                })
            })
        });
        assert!(
            persistent.is_err(),
            "a changed full role invalidates the view"
        );
    }

    #[test]
    fn closed_inherited_source_view_returns_a_denying_pinned_role() {
        let root = tempfile::tempdir().expect("create project");
        let role = fixture_role("role-a", "warn", true, &["ctx_read"]);
        let role_fingerprint = role_fingerprint(&role).unwrap();
        let view = SourceView {
            project: root.path().to_path_buf(),
            policy: None,
            fingerprint: blake3::hash(b"test"),
            role,
            role_fingerprint,
            valid: Arc::new(AtomicBool::new(true)),
            live: Arc::new(AtomicBool::new(false)),
        };

        REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(root.path().to_path_buf())),
            || {
                SOURCE_VIEW.sync_scope(std::cell::RefCell::new(Some(view)), || {
                    let closed = crate::core::roles::active_role();
                    assert_eq!(closed.role.name, "closed-source-view");
                    assert!(!closed.is_tool_allowed("ctx_read"));
                    assert!(!closed.io.allow_secret_paths);
                });
            },
        );
    }

    #[test]
    fn project_source_view_uses_fallback_and_preserves_bound_project() {
        let _override_guard = lock_test_override();
        let _data = crate::core::data_dir::isolated_data_dir();
        let fallback = tempfile::tempdir().expect("create fallback project");
        let bound = tempfile::tempdir().expect("create bound project");
        write_policy(fallback.path(), "fallback-project");
        write_policy(bound.path(), "bound-project");
        let fallback_root = fallback.path().to_str().unwrap();

        let unbound =
            with_project_source_view(fallback_root, || active().unwrap().resolved.name.clone());
        assert_eq!(unbound.unwrap(), "fallback-project");

        let already_bound = REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(bound.path().to_path_buf())),
            || with_project_source_view(fallback_root, || active().unwrap().resolved.name.clone()),
        );
        assert_eq!(already_bound.unwrap(), "bound-project");
    }
}
