// SPDX-License-Identifier: Apache-2.0
//! Source identity travels with an archive; a stored verdict is never authorization.
//!
//! An authority names **every** source a result was derived from. The first
//! source keeps the version-1 scalar layout (so single-source records and
//! their IDs are byte-identical to before); further sources of the same call
//! are added to `more`. A read is authorized only when every one of them is
//! still admitted — one remaining source never authorizes the whole result.
//! Capture is active whenever protection is active in any layer (project
//! policy pack, or the global gateway in governed or sovereign mode); see
//! `context_admission::stored::source_authority_required`.
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Upper bound of sources one authority can name. A derivation from more
/// sources is not truncated to the first few: its authority becomes
/// unverifiable and the result is withheld from every source-bound store.
pub(crate) const MAX_SOURCES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveAuthority {
    version: u8,
    project: String,
    path: String,
    tool: String,
    admitted_digest: String,
    /// Further sources of the same derivation, deduplicated, at most
    /// `MAX_SOURCES - 1`. Absent on disk for a single-source record.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    more: Vec<AdditionalSource>,
    /// Set when a source could not be added (bound exceeded, other project):
    /// the authority can then never be admitted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    incomplete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AdditionalSource {
    path: String,
    tool: String,
    admitted_digest: String,
}

/// What one call acquired, as the capture saw it.
#[derive(Debug, Default)]
pub(crate) struct CaptureSlot {
    authority: Mutex<Option<ArchiveAuthority>>,
    /// A source was read whose authority could not be built or bound. The
    /// captured set is then not the whole acquisition.
    unbound: std::sync::atomic::AtomicBool,
}

/// How long a publication or the final read waits for the slot. The slot's
/// own critical sections are a few assignments, so real contention is far
/// below this; the budget exists so that no path can wait without end.
const SLOT_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

impl CaptureSlot {
    /// Acquire the slot within [`SLOT_WAIT`]. `None`: the budget ran out. The
    /// capture is then marked incomplete before returning, so the caller can
    /// simply give up: the acquisition is unknown from here on (never a
    /// silently missing source, never an endless wait). A poisoned lock still
    /// yields the state.
    fn lock_within(
        &self,
        budget: std::time::Duration,
    ) -> Option<std::sync::MutexGuard<'_, Option<ArchiveAuthority>>> {
        let deadline = std::time::Instant::now() + budget;
        loop {
            match self.authority.try_lock() {
                Ok(guard) => return Some(guard),
                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                    return Some(poisoned.into_inner());
                }
                Err(std::sync::TryLockError::WouldBlock)
                    if std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_micros(200));
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    self.unbound
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    return None;
                }
            }
        }
    }

    /// Add `authority` to the captured set, within `budget`.
    fn publish_within(&self, authority: ArchiveAuthority, budget: std::time::Duration) {
        if let Some(mut slot) = self.lock_within(budget) {
            match slot.as_mut() {
                Some(existing) => existing.join(authority),
                None => *slot = Some(authority),
            }
        }
    }

    /// What was captured: `None` nothing observed; `Some` a source set, which
    /// is incomplete when a source could not be bound or the slot could not
    /// be acquired in time (by a publication, or by this read).
    fn finish_within(&self, budget: std::time::Duration) -> Option<ArchiveAuthority> {
        let mut authority = self.lock_within(budget).and_then(|mut slot| slot.take());
        if self.unbound.load(std::sync::atomic::Ordering::SeqCst) {
            match authority.as_mut() {
                Some(authority) => authority.incomplete = true,
                None => authority = Some(ArchiveAuthority::unbound()),
            }
        }
        authority
    }
}

pub(crate) type Capture = Arc<CaptureSlot>;

tokio::task_local! {
    pub(crate) static CAPTURE: Option<Capture>;
}

pub(crate) fn current_capture() -> Option<Capture> {
    CAPTURE.try_with(Clone::clone).ok().flatten()
}

/// Run `operation` without the call's source capture: reads inside it are not
/// published. For work that inspects sources without deriving the result from
/// them (a scan) — the caller publishes what did contribute afterwards.
pub(crate) fn without_capture<T>(operation: impl FnOnce() -> T) -> T {
    CAPTURE.sync_scope(None, operation)
}

/// Mark the current call's acquisition as incomplete.
pub(crate) fn mark_unbound() {
    if let Some(capture) = current_capture() {
        capture
            .unbound
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Publish one admitted source read into the current call's capture.
///
/// Called by the admitted readers themselves (`tools::ctx_read::file_io`), so
/// a tool that reads through them publishes every source without code of its
/// own. `project` is the root the read was authorized for, `canonical_path`
/// the path the reader resolved, `admitted` the text it returned.
///
/// No capture, or no protection in any layer: nothing happens (and nothing is
/// hashed). Under protection a source that cannot be bound — outside the
/// project, project not resolvable — marks the capture incomplete: the call's
/// source set is then unknown, never silently smaller.
pub(crate) fn publish_read(project: &Path, canonical_path: &str, tool: &str, admitted: &str) {
    // G08: the call's receipt trace names this source the way its reader
    // shows it — the path relative to the project. Independent of the source
    // capture below; a no-op without an admission capture.
    crate::core::context_admission::capture::note_source(project, canonical_path);
    if current_capture().is_none() {
        return;
    }
    if let Some(authority) = ArchiveAuthority::file(project, canonical_path, tool, admitted) {
        authority.publish();
    }
}

/// Run `operation` and return what it acquired. Three outcomes:
///
/// - `Some(authority)` with [`ArchiveAuthority::is_incomplete`] `false`: these
///   sources, and — as far as the capture can tell — no others;
/// - `Some(authority)` that is incomplete: at least one source could not be
///   bound (or the set exceeded its bound). It never admits; the acquisition
///   is unknown;
/// - `None`: no source was published. This is an observation, not an
///   attestation — an uninstrumented reader publishes nothing either.
pub(crate) async fn capture<T>(
    operation: impl std::future::Future<Output = T>,
) -> (T, Option<ArchiveAuthority>) {
    let capture = Arc::new(CaptureSlot::default());
    let result = CAPTURE.scope(Some(capture.clone()), operation).await;
    (result, capture.finish_within(SLOT_WAIT))
}

/// [`capture`] for a synchronous caller (the CLI tool path): run `operation`
/// on this thread and return what it acquired, with the same three outcomes.
/// Reads on other threads or tasks spawned inside are not seen.
pub(crate) fn capture_sync<T>(operation: impl FnOnce() -> T) -> (T, Option<ArchiveAuthority>) {
    let capture = Arc::new(CaptureSlot::default());
    let result = CAPTURE.sync_scope(Some(capture.clone()), operation);
    (result, capture.finish_within(SLOT_WAIT))
}

impl ArchiveAuthority {
    /// Called with the canonical path and bytes returned by the admitted rooted read.
    pub(crate) fn file(project: &Path, path: &str, tool: &str, admitted: &str) -> Option<Self> {
        if !crate::core::context_admission::stored::source_authority_required() {
            return None;
        }
        let authority = Self::bind(project, path, tool, admitted);
        if authority.is_none() {
            // A source was read under protection and cannot be bound: the
            // call's source set is no longer known.
            mark_unbound();
        }
        authority
    }

    /// The authority of a source a STORED item was derived from: the same
    /// facts the admitted reader publishes (`project`, `path`, `tool`), with
    /// the digest of the text it returned when the item was written instead
    /// of the text itself. For `context_admission::stored::publish_served`,
    /// which calls it only after the item's own re-authorization passed.
    ///
    /// Gate and failure as [`Self::file`]: `None` without protection; under
    /// protection a source that cannot be bound (digest not a BLAKE3 hex
    /// value, path outside the project, project not resolvable) marks the
    /// capture incomplete. A path relative to the project is resolved in it.
    pub(crate) fn admitted_source(
        project: &Path,
        path: &str,
        tool: &str,
        admitted_digest: &str,
    ) -> Option<Self> {
        if !crate::core::context_admission::stored::source_authority_required() {
            return None;
        }
        let authority = Self::bind_digest(project, path, tool, admitted_digest);
        if authority.is_none() {
            mark_unbound();
        }
        authority
    }

    fn bind_digest(project: &Path, path: &str, tool: &str, admitted_digest: &str) -> Option<Self> {
        let well_formed = admitted_digest.len() == 64
            && admitted_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
        if !well_formed {
            return None;
        }
        let project = canonical_archive_path(project)?;
        // The reader publishes the path it resolved; name the same one, so a
        // source read and a stored item derived from it are one source.
        let path = canonical_archive_path(&project.join(path))?;
        if !path.starts_with(&project) {
            return None;
        }
        Some(Self {
            version: 1,
            project: project.to_str()?.to_owned(),
            path: path.to_str()?.to_owned(),
            tool: tool.to_owned(),
            admitted_digest: admitted_digest.to_owned(),
            more: Vec::new(),
            incomplete: false,
        })
    }

    /// The authority of an acquisition whose sources are not known. It names
    /// no source and never admits.
    fn unbound() -> Self {
        Self {
            version: 1,
            project: String::new(),
            path: String::new(),
            tool: String::new(),
            admitted_digest: String::new(),
            more: Vec::new(),
            incomplete: true,
        }
    }

    /// Whether the source set is unknown (a source could not be bound, or the
    /// set exceeded its bound). Such an authority never admits.
    pub(crate) fn is_incomplete(&self) -> bool {
        self.incomplete
    }

    fn bind(project: &Path, path: &str, tool: &str, admitted: &str) -> Option<Self> {
        let project = canonical_archive_path(project)?;
        let path = canonical_archive_path(Path::new(path))?;
        if !path.starts_with(&project) {
            return None;
        }
        Some(Self {
            version: 1,
            project: project.to_str()?.to_owned(),
            path: path.to_str()?.to_owned(),
            tool: tool.to_owned(),
            admitted_digest: blake3::hash(admitted.as_bytes()).to_hex().to_string(),
            more: Vec::new(),
            incomplete: false,
        })
    }

    /// Add this source to the call's authority. A second source joins the
    /// set; it never replaces the first. If the slot cannot be acquired within
    /// its budget the capture becomes incomplete instead of losing the source.
    pub(crate) fn publish(self) {
        if let Some(capture) = current_capture() {
            capture.publish_within(self, SLOT_WAIT);
        }
    }

    /// Union of two authorities of one derivation.
    pub(crate) fn join(&mut self, other: Self) {
        if other.project != self.project {
            self.incomplete = true;
            return;
        }
        self.incomplete |= other.incomplete;
        let first = AdditionalSource {
            path: other.path,
            tool: other.tool,
            admitted_digest: other.admitted_digest,
        };
        for source in std::iter::once(first).chain(other.more) {
            let primary = source.path == self.path
                && source.tool == self.tool
                && source.admitted_digest == self.admitted_digest;
            if primary || self.more.contains(&source) {
                continue;
            }
            if self.more.len() + 1 >= MAX_SOURCES {
                self.incomplete = true;
                return;
            }
            self.more.push(source);
        }
    }

    /// Canonical root of the project every source of this authority is in.
    pub(super) fn project(&self) -> &str {
        &self.project
    }

    /// Every source of a complete set as (path, tool, admitted digest);
    /// `None` for an incomplete one, which names no usable set.
    pub(super) fn sources(&self) -> Option<Vec<(&str, &str, &str)>> {
        if self.incomplete {
            return None;
        }
        Some(
            std::iter::once((&self.path, &self.tool, &self.admitted_digest))
                .chain(
                    self.more
                        .iter()
                        .map(|source| (&source.path, &source.tool, &source.admitted_digest)),
                )
                .map(|(path, tool, digest)| (path.as_str(), tool.as_str(), digest.as_str()))
                .collect(),
        )
    }

    /// Union with a LATER observation of the same delivered text
    /// (`archive::delivered`). Like [`Self::join`] — a source only one side
    /// names is added, never dropped — except that a source both name (same
    /// path and tool) takes the later admitted digest: it is the same source
    /// in its current state, admitted just now, and keeping the superseded
    /// digest next to it would make the set unadmittable for good.
    pub(super) fn absorb(&mut self, later: Self) {
        if later.project != self.project {
            self.incomplete = true;
            return;
        }
        self.incomplete |= later.incomplete;
        let first = AdditionalSource {
            path: later.path,
            tool: later.tool,
            admitted_digest: later.admitted_digest,
        };
        for source in std::iter::once(first).chain(later.more) {
            if source.path == self.path && source.tool == self.tool {
                self.admitted_digest = source.admitted_digest;
                continue;
            }
            if let Some(known) = self
                .more
                .iter_mut()
                .find(|known| known.path == source.path && known.tool == source.tool)
            {
                known.admitted_digest = source.admitted_digest;
                continue;
            }
            if self.more.len() + 1 >= MAX_SOURCES {
                self.incomplete = true;
                return;
            }
            self.more.push(source);
        }
    }

    pub(crate) fn id(&self, content: &str) -> Option<String> {
        let mut hash = blake3::Hasher::new();
        hash.update(b"leanctx-source-archive-v1\0");
        hash.update(&serde_json::to_vec(self).ok()?);
        hash.update(b"\0");
        hash.update(content.as_bytes());
        Some(hash.finalize().to_hex().to_string())
    }

    pub(crate) fn admitted(&self) -> bool {
        self.admitted_budgeted(&mut crate::core::limits::max_read_bytes())
    }

    /// The lineage handle for holders outside the tool pipeline (a register of
    /// sealed items, a recovery handle): whether every source this authority
    /// names is admitted NOW for the request project and role in scope.
    ///
    /// Takes its own fresh policy view, so it may be called from anywhere
    /// inside a `REQUEST_PROJECT` scope. `false` when no request project is
    /// bound, when it is not the authority's project, when the policy cannot
    /// be verified, when the set is incomplete, or when any source is gone,
    /// changed, restricted or no longer permitted. Never an error, never
    /// content.
    #[must_use]
    pub(crate) fn still_admitted(&self) -> bool {
        crate::core::policy::runtime::with_source_view(|| self.admitted()).unwrap_or(false)
    }

    /// A stable, content-free identity of this lineage: the same sources
    /// (project, paths, tools, admitted digests) always give the same value.
    /// It identifies; it authorizes nothing — ask [`Self::still_admitted`].
    /// `None` for an incomplete set, which has no identity to store.
    #[must_use]
    pub(crate) fn identity_digest(&self) -> Option<String> {
        if self.incomplete {
            return None;
        }
        // Independent of the order in which the sources were published.
        let mut sources: Vec<(&str, &str, &str)> =
            std::iter::once((&self.path, &self.tool, &self.admitted_digest))
                .chain(
                    self.more
                        .iter()
                        .map(|source| (&source.path, &source.tool, &source.admitted_digest)),
                )
                .map(|(path, tool, digest)| (path.as_str(), tool.as_str(), digest.as_str()))
                .collect();
        sources.sort_unstable();
        let mut hash = blake3::Hasher::new();
        hash.update(b"leanctx-source-lineage-v1\0");
        hash.update(&serde_json::to_vec(&(self.version, &self.project, sources)).ok()?);
        Some(hash.finalize().to_hex().to_string())
    }

    pub(super) fn matches_project(&self) -> bool {
        if self.version != 1 {
            return false;
        }
        let Some(project) = crate::core::policy::runtime::REQUEST_PROJECT
            .try_with(|slot| slot.borrow().clone())
            .ok()
            .flatten()
            .and_then(|path| crate::core::pathutil::canonicalize_secure(&path).ok())
        else {
            return false;
        };
        project == Path::new(&self.project)
    }

    /// Every source is re-read through the admitted rooted reader under the
    /// current policy and role and must deliver the bytes it delivered then.
    pub(super) fn admitted_budgeted(&self, remaining: &mut usize) -> bool {
        if self.incomplete || !self.matches_project() {
            return false;
        }
        let primary = (&self.path, &self.tool, &self.admitted_digest);
        std::iter::once(primary)
            .chain(
                self.more
                    .iter()
                    .map(|source| (&source.path, &source.tool, &source.admitted_digest)),
            )
            .all(|(path, tool, digest)| {
                *remaining != 0
                    && crate::tools::ctx_read::read_file_for_tool_rooted_admitted(
                        path,
                        &self.project,
                        tool,
                        remaining,
                    )
                    .is_ok_and(|read| {
                        // The ORIGINAL decides, not its delivered text: a source
                        // that is only lossily decodable, or restricted by its
                        // content or by its path, authorizes no stored copy.
                        // A role's permission to READ a secret-like path is
                        // not a permission to retain what was derived from it
                        // (owner decision E3).
                        read.complete
                            && read.storable
                            && blake3::hash(read.content.as_bytes()).to_hex().as_str()
                                == digest.as_str()
                    })
            })
    }
}

fn canonical_archive_path(path: &Path) -> Option<std::path::PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    crate::core::pathutil::canonicalize_secure(path)
        .or_else(|error| {
            let simplified = crate::core::pathutil::strip_verbatim(path.to_path_buf());
            if simplified.as_path() == path {
                Err(error)
            } else {
                crate::core::pathutil::canonicalize_secure(&simplified)
            }
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(path: &str) -> ArchiveAuthority {
        ArchiveAuthority {
            version: 1,
            project: "/project".to_owned(),
            path: path.to_owned(),
            tool: "ctx_read".to_owned(),
            admitted_digest: blake3::hash(path.as_bytes()).to_hex().to_string(),
            more: Vec::new(),
            incomplete: false,
        }
    }

    use std::time::Duration;

    /// Hold the slot from another thread until `release` is dropped.
    fn hold(slot: &Arc<CaptureSlot>) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (held, is_held) = std::sync::mpsc::channel::<()>();
        let slot = slot.clone();
        let holder = std::thread::spawn(move || {
            let guard = slot.lock_within(Duration::from_secs(5)).expect("free slot");
            held.send(()).expect("signal");
            let _ = wait.recv();
            drop(guard);
        });
        is_held.recv().expect("the holder owns the slot");
        (release, holder)
    }

    /// The wait has a real budget. A slot held beyond it makes the capture
    /// incomplete — the source is neither silently lost (the old `try_lock`
    /// dropped it) nor waited for without end. Contention that ends inside
    /// the budget captures normally.
    #[test]
    fn slot_contention_is_bounded_and_never_loses_a_source_silently() {
        // Held beyond the budget: the publication gives up in bounded time.
        let slot = Arc::new(CaptureSlot::default());
        let (release, holder) = hold(&slot);
        let started = std::time::Instant::now();
        slot.publish_within(source("/project/a.rs"), Duration::from_millis(20));
        assert!(started.elapsed() < Duration::from_secs(2), "bounded wait");
        drop(release);
        holder.join().expect("holder");
        let captured = slot
            .finish_within(Duration::from_secs(5))
            .expect("an unknown acquisition is reported, not dropped");
        assert!(captured.is_incomplete(), "exceeded budget → incomplete");
        assert!(!captured.admitted(), "an incomplete authority never admits");

        // Short contention inside the budget: both sources are captured.
        let slot = Arc::new(CaptureSlot::default());
        let (release, holder) = hold(&slot);
        let publisher = {
            let slot = slot.clone();
            std::thread::spawn(move || {
                slot.publish_within(source("/project/a.rs"), Duration::from_secs(5));
                slot.publish_within(source("/project/b.rs"), Duration::from_secs(5));
            })
        };
        std::thread::sleep(Duration::from_millis(20));
        drop(release);
        holder.join().expect("holder");
        publisher.join().expect("publisher");
        let captured = slot
            .finish_within(Duration::from_secs(5))
            .expect("captured");
        assert_eq!(captured.more.len(), 1, "the second source joined the first");
        assert!(!captured.is_incomplete());
    }
}
