// SPDX-License-Identifier: Apache-2.0
//! Delivered-content lineage register (G03/G04).
//!
//! When the Engine releases source-bound text towards the model, it records
//! here what that exact text was derived from. A later holder of the same
//! bytes — the proxy, when the text comes back inside a provider request or
//! as sealed provider material — looks the lineage up by digest
//! ([`lineage_of`]) and asks [`Lineage::still_admitted`] whether every bound
//! source holds NOW. The register identifies; it authorizes nothing.
//!
//! A [`Lineage`] never mixes source kinds up:
//!
//! - **files** — project files the Engine itself read through the admitted
//!   rooted reader ([`ArchiveAuthority`]). Re-checked by re-reading each file
//!   under the current policy and role.
//! - **presented** — caller-supplied sources of the context plan
//!   ([`PresentedBinding`]). The Engine cannot re-read them and does not learn
//!   of changes outside; such a binding holds only while the caller keeps
//!   presenting exactly that content and the plan keeps admitting it under
//!   the current policy (`archive::presented`).
//!
//! Rules of the register:
//!
//! - **Key:** BLAKE3 of the delivered text, exact bytes, no normalisation
//!   ([`digest`]). A host that re-wraps, truncates or re-encodes the text
//!   changes the digest: no hit, unknown lineage.
//! - **Value:** lineage handles only. Never content.
//! - **Recorded:** only a complete lineage whose every source holds at the
//!   time of the record. Anything else records nothing: unknown stays
//!   unknown. Owner decision E3: a restricted source is in no lineage.
//! - **Same text, second lineage:** united per project ([`Lineage::join`]);
//!   the text is admitted only while every source of the union holds.
//!   Lineages of different projects are kept side by side.
//! - **Persistence and bounds:** see `archive::register`; 64 texts per shard
//!   (oldest evicted), [`MAX_LINEAGES_PER_TEXT`] projects per text,
//!   [`MAX_AGE_SECONDS`] age. The files name local paths and source ids: they
//!   never enter a receipt, a log line or client-visible text.
//! - **Call ids:** a tool delivery may additionally name the host's id of the
//!   tool call it answered ([`record_tool_delivery_for_call`]); at most
//!   [`MAX_CALLS_PER_LINEAGE`] per text and project, oldest evicted. They are
//!   host-supplied metadata, answer only [`delivered_to_call`], are never part
//!   of a lineage handle that leaves this module, and are never copied to a
//!   derived text.

use serde::{Deserialize, Serialize};

use super::authority::ArchiveAuthority;
use super::presented::{Presentation, PresentedBinding};
use super::register::{self, Register};

/// Lineages (one per project) one text can have.
const MAX_LINEAGES_PER_TEXT: usize = 4;
/// A record older than this is neither returned nor kept.
const MAX_AGE_SECONDS: i64 = 24 * 60 * 60;
/// Caller-supplied sources one lineage can name; beyond it the lineage is
/// unknown, never cut down to the first few.
const MAX_PRESENTED: usize = 64;
/// Call ids kept per text and project; the oldest goes first.
const MAX_CALLS_PER_LINEAGE: usize = 32;
/// Longest call id that is recorded or looked up.
const MAX_CALL_ID_BYTES: usize = 128;
/// Presenter references kept per text and project; the oldest goes first.
const MAX_PRESENTER_REFS_PER_LINEAGE: usize = 16;

const REGISTER: Register = Register {
    name: "delivered",
    shard_entries: 64,
    max_age_seconds: MAX_AGE_SECONDS,
};

/// What a delivered text derives from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Lineage {
    /// Project files the Engine itself read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    files: Option<ArchiveAuthority>,
    /// Caller-supplied sources.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    presented: Vec<PresentedBinding>,
    /// A source could not be bound, a bound was exceeded, or parts of
    /// different projects were united: the lineage is unknown.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    incomplete: bool,
    /// Host ids of the tool calls this exact text was delivered for, oldest
    /// first. Present only on the persisted entry: every handle this module
    /// returns has it empty, and [`Lineage::join`] never copies it. An entry
    /// written before the field existed has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    calls: Vec<String>,
    /// Content-free references of the presenter entries under which this
    /// exact text was recorded or released (`presented::PresenterCapability::
    /// reference`: a digest of a stored verifier, never of a capability),
    /// oldest first, for a lineage that names caller-supplied sources. They
    /// tie a record to "the same presenter entry"; they authorize nothing.
    /// Present only on a persisted entry and on the lineage stored with a
    /// recovery copy: every handle this module returns has it empty, and
    /// [`Lineage::join`] never copies it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    presenters: Vec<String>,
}

/// Why a lineage does not authorize its text now. Content-free: no path,
/// object reference, source id or digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineageRefusal {
    /// No lineage, or an incomplete one.
    Unknown,
    /// A caller-supplied source has no current presentation.
    SourceNotPresented,
    /// A file source or a presented source is refused under the current
    /// rights or policy (changed, gone, restricted, other project, caller or
    /// role, policy changed since it was admitted).
    SourceNoLongerAdmitted,
}

impl std::fmt::Display for LineageRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unknown => "lineage.unknown",
            Self::SourceNotPresented => "lineage.source_not_presented",
            Self::SourceNoLongerAdmitted => "lineage.source_no_longer_admitted",
        })
    }
}

impl Lineage {
    /// Lineage of text derived from project files only.
    #[must_use]
    pub(crate) fn of_files(authority: ArchiveAuthority) -> Self {
        Self {
            incomplete: authority.is_incomplete(),
            files: Some(authority),
            presented: Vec::new(),
            calls: Vec::new(),
            presenters: Vec::new(),
        }
    }

    /// Lineage of text derived from caller-supplied sources only. An empty
    /// set is not a lineage: it is unknown.
    #[must_use]
    pub(crate) fn of_presented(presented: Vec<PresentedBinding>) -> Self {
        let mut lineage = Self {
            files: None,
            presented: Vec::new(),
            incomplete: presented.is_empty(),
            calls: Vec::new(),
            presenters: Vec::new(),
        };
        for binding in presented {
            lineage.add_presented(binding);
        }
        lineage
    }

    fn add_presented(&mut self, binding: PresentedBinding) {
        if self
            .project()
            .is_some_and(|project| project != binding.project())
        {
            self.incomplete = true;
            return;
        }
        if let Some(known) = self
            .presented
            .iter_mut()
            .find(|known| known.same_source(&binding))
        {
            // The same source, admitted again later: one binding.
            *known = binding;
        } else if self.presented.len() < MAX_PRESENTED {
            self.presented.push(binding);
        } else {
            self.incomplete = true;
        }
    }

    fn project(&self) -> Option<&str> {
        self.files
            .as_ref()
            .map(ArchiveAuthority::project)
            .or_else(|| self.presented.first().map(PresentedBinding::project))
    }

    /// Union with another lineage of the same text. A source only one side
    /// names is added, never dropped; the text then needs every source of
    /// both. Parts of different projects make the lineage unknown. Call ids
    /// are not sources: the other side's are dropped, this side's stay.
    pub(crate) fn join(&mut self, other: Self) {
        self.incomplete |= other.incomplete;
        if let (Some(mine), Some(theirs)) = (self.project(), other.project())
            && mine != theirs
        {
            self.incomplete = true;
            return;
        }
        match (self.files.as_mut(), other.files) {
            (Some(mine), Some(theirs)) => {
                mine.absorb(theirs);
                self.incomplete |= mine.is_incomplete();
            }
            (None, Some(theirs)) => self.files = Some(theirs),
            (_, None) => {}
        }
        for binding in other.presented {
            self.add_presented(binding);
        }
    }

    /// Note that this entry's text was delivered for the call with this id.
    /// A known id moves to the newest place; beyond the bound the oldest goes.
    fn note_call(&mut self, call_id: &str) {
        self.calls.retain(|known| known != call_id);
        self.calls.push(call_id.to_owned());
        if self.calls.len() > MAX_CALLS_PER_LINEAGE {
            let excess = self.calls.len() - MAX_CALLS_PER_LINEAGE;
            self.calls.drain(..excess);
        }
    }

    /// Note the presenter of the request in scope on this entry, if the
    /// lineage names a caller-supplied source. A known reference moves to the
    /// newest place; beyond the bound the oldest goes.
    fn note_presenter(&mut self) {
        if self.presented.is_empty() {
            return;
        }
        let Some(reference) = super::presented::current_presenter_reference() else {
            return;
        };
        self.presenters.retain(|known| *known != reference);
        self.presenters.push(reference);
        if self.presenters.len() > MAX_PRESENTER_REFS_PER_LINEAGE {
            let excess = self.presenters.len() - MAX_PRESENTER_REFS_PER_LINEAGE;
            self.presenters.drain(..excess);
        }
    }

    /// This lineage as it is stored WITH a record made for the request in
    /// scope (a recovery copy): the same sources, and the reference of the
    /// presenter that request showed.
    #[must_use]
    pub(super) fn recorded_for_presenter(&self) -> Self {
        let mut recorded = self.clone();
        recorded.calls.clear();
        recorded.presenters.clear();
        recorded.note_presenter();
        recorded
    }

    /// The file authority of a lineage that names project files only (no
    /// caller-supplied source): it then IS the lineage.
    pub(super) fn files_only(&self) -> Option<&ArchiveAuthority> {
        self.files
            .as_ref()
            .filter(|_| self.presented.is_empty() && !self.incomplete)
    }

    /// Publish this lineage into the current call's source capture, for a
    /// declared tool whose reply is derived from text with this lineage and
    /// from nothing else (`ctx_expand` on a resolved recovery copy). The
    /// release point then records the reply as it is delivered.
    ///
    /// The capture carries project files only: a file-only lineage is
    /// published; a lineage that names a caller-presented source, and an
    /// incomplete one, mark the capture incomplete — that reply stays
    /// unknown rather than being recorded under its file part alone.
    pub(crate) fn publish_files(&self) {
        match self.files_only() {
            Some(files) => files.clone().publish(),
            None => super::authority::mark_unbound(),
        }
    }

    /// A stable, content-free identity of a complete lineage: the same
    /// sources — files with their admitted digests, presented bindings with
    /// the policy they were admitted under — give the same value, in whatever
    /// order they were joined. `None` for an incomplete lineage.
    pub(super) fn identity_digest(&self) -> Option<String> {
        if !self.is_complete() {
            return None;
        }
        let files = match &self.files {
            Some(files) => Some(files.identity_digest()?),
            None => None,
        };
        let mut presented: Vec<String> = self
            .presented
            .iter()
            .map(PresentedBinding::identity)
            .collect();
        presented.sort_unstable();
        let mut hash = blake3::Hasher::new();
        hash.update(b"leanctx-lineage-v1\0");
        hash.update(&serde_json::to_vec(&(files, presented)).ok()?);
        Some(hash.finalize().to_hex().to_string())
    }

    /// The ids of the sources of a lineage that names caller-supplied
    /// sources ONLY, sorted and distinct. `None` for every other lineage: a
    /// file source, an incomplete lineage, no source. The ids are
    /// content-free and authorize nothing — ask [`Lineage::still_admitted`].
    #[must_use]
    pub(crate) fn presented_source_ids(&self) -> Option<Vec<String>> {
        if self.files.is_some() || !self.is_complete() {
            return None;
        }
        let mut ids: Vec<String> = self
            .presented
            .iter()
            .map(|binding| binding.source_id().to_owned())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        Some(ids)
    }

    /// Whether the lineage names every source of its text.
    #[must_use]
    pub(crate) fn is_complete(&self) -> bool {
        !self.incomplete
            && (self.files.is_some() || !self.presented.is_empty())
            && self
                .files
                .as_ref()
                .is_none_or(|files| !files.is_incomplete())
    }

    /// Current authority of every bound source. Call inside the
    /// `REQUEST_PROJECT` scope of the request the text returns in.
    ///
    /// - files: every file is re-read through the admitted rooted reader
    ///   under the current policy and role and must be unchanged, complete
    ///   and storable;
    /// - presented: project, caller and role must be those of the request,
    ///   and a presentation of exactly that binding must exist that was
    ///   admitted under the current policy identity within the validity.
    ///   This says nothing about the caller's or a provider's own copy.
    pub(crate) fn still_admitted(&self) -> Result<(), LineageRefusal> {
        if !self.is_complete() {
            return Err(LineageRefusal::Unknown);
        }
        if self
            .files
            .as_ref()
            .is_some_and(|files| !files.still_admitted())
        {
            return Err(LineageRefusal::SourceNoLongerAdmitted);
        }
        let mut not_presented = false;
        for binding in &self.presented {
            if !binding.matches_request() {
                return Err(LineageRefusal::SourceNoLongerAdmitted);
            }
            match binding.presentation() {
                Presentation::Current => {}
                Presentation::OtherPolicy => return Err(LineageRefusal::SourceNoLongerAdmitted),
                Presentation::None => not_presented = true,
            }
        }
        if not_presented {
            return Err(LineageRefusal::SourceNotPresented);
        }
        Ok(())
    }
}

/// The register key of a delivered text: BLAKE3 over its exact bytes.
#[must_use]
pub(crate) fn digest(delivered_text: &str) -> String {
    blake3::hash(delivered_text.as_bytes()).to_hex().to_string()
}

/// Record that the text with this digest was released with this lineage.
///
/// Call inside the request's `REQUEST_PROJECT` scope, at the release point,
/// with the digest of the exact bytes delivered. Returns whether the lineage
/// is now on record; `false` leaves the text's lineage unknown (malformed
/// digest, incomplete lineage, a source that does not hold now — including
/// every restricted source —, no request project, a bound, or the lock).
pub(crate) fn record_lineage(delivered_text_digest: &str, lineage: &Lineage) -> bool {
    lineage.still_admitted().is_ok()
        && store_at(delivered_text_digest, lineage, None, register::now())
}

/// Record a text that was DERIVED from text with a known lineage (compressed,
/// or cut down to a stub that points at a recovery id) under that same
/// lineage, so the derived bytes stay returnable exactly as long as their
/// origin is. This is the one record a site other than an Engine release
/// point may make. Preconditions, all of them the caller's:
///
/// 1. `lineage` came from [`lineage_of`] for the exact original block, in the
///    same request, inside the same `REQUEST_PROJECT` scope;
/// 2. the derivation used ONLY that block (plus fixed text of the deriving
///    code itself, such as a recovery hint) — no other block, store or
///    session text;
/// 3. for a derivation from several blocks: the lineages of ALL of them,
///    united with [`Lineage::join`]; if one block has no lineage, do not call.
///
/// Checked here: the lineage is complete and every source holds at this
/// moment. Otherwise nothing is recorded and the derived text is unknown.
pub(crate) fn record_derived(derived_text_digest: &str, lineage: &Lineage) -> bool {
    record_lineage(derived_text_digest, lineage)
}

/// The lineage recorded for the text with this digest, for the request
/// project in scope (without a request project: the only lineage, if there is
/// exactly one). `None`: unknown — nothing on record, expired, incomplete.
///
/// The handle is returned whether or not its sources still hold — ask
/// [`Lineage::still_admitted`] inside the request's project scope.
#[must_use]
pub(crate) fn lineage_of(delivered_text_digest: &str) -> Option<Lineage> {
    lineage_at(delivered_text_digest, register::now())
}

/// Records the lineage of the text blocks actually delivered for one tool
/// call — the one place that holds the conditions, for the MCP pipeline and
/// the CLI alike: declared tool, no tool error, complete capture, every
/// source admitted now, nothing restricted. Anything else records nothing
/// (unknown stays unknown). Returns the number of blocks recorded.
///
/// `declares_every_source` is the tool's `publishes_every_source` for this
/// call ([`declares_every_source`]) AND the caller's own statement that it
/// mixed no text without source authority (session-derived context, hints
/// from derived stores) into the delivered blocks; a caller that did passes
/// `false`. `capture` is what `authority::capture` / `capture_sync` returned
/// around the handler. `delivered_blocks` are the exact delivered bytes,
/// after all changes. Call inside the request's `REQUEST_PROJECT` scope.
pub(crate) fn record_tool_delivery(
    declares_every_source: bool,
    is_error: bool,
    capture: Option<ArchiveAuthority>,
    delivered_blocks: &[&str],
) -> usize {
    record_tool_delivery_for_call(
        declares_every_source,
        is_error,
        capture,
        delivered_blocks,
        None,
    )
}

/// A call id as this register records and looks it up: 1 to
/// [`MAX_CALL_ID_BYTES`] bytes of `[A-Za-z0-9_-]`. Anything else is no id.
#[must_use]
pub(crate) fn valid_call_id(call_id: &str) -> bool {
    (1..=MAX_CALL_ID_BYTES).contains(&call_id.len())
        && call_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// [`record_tool_delivery`] for a call whose host named its own id for the
/// call (the MCP pipeline). What is recorded, and when, is exactly the same;
/// a valid `call_id` is additionally noted on each recorded block's entry of
/// the request project, so [`delivered_to_call`] can later tell that THIS
/// call was answered with THESE bytes by a process writing this store. An
/// absent or malformed id notes nothing. The id is metadata of the host: it
/// is persisted only in the register files and appears in no tool output,
/// receipt, log line or refusal.
pub(crate) fn record_tool_delivery_for_call(
    declares_every_source: bool,
    is_error: bool,
    capture: Option<ArchiveAuthority>,
    delivered_blocks: &[&str],
    call_id: Option<&str>,
) -> usize {
    if !declares_every_source || is_error {
        return 0;
    }
    let Some(authority) = capture.filter(|authority| !authority.is_incomplete()) else {
        return 0;
    };
    let lineage = Lineage::of_files(authority);
    // One admission pass for the call, not one per block.
    if lineage.still_admitted().is_err() {
        return 0;
    }
    let call_id = call_id.filter(|call_id| valid_call_id(call_id));
    let now = register::now();
    delivered_blocks
        .iter()
        .filter(|block| store_at(&digest(block), &lineage, call_id, now))
        .count()
}

/// Whether the text with this digest is on record, for the request project in
/// scope, as delivered for the tool call with this id. `true` only for a
/// complete, unexpired entry of that project that lists exactly this id;
/// without a request project, for a malformed id, and for every entry written
/// without an id (including all written before ids were recorded): `false`.
///
/// This identifies a delivery; like the rest of the register it authorizes
/// nothing — the lineage still has to hold ([`Lineage::still_admitted`]).
#[must_use]
pub(crate) fn delivered_to_call(delivered_text_digest: &str, call_id: &str) -> bool {
    if !valid_call_id(call_id) {
        return false;
    }
    let Some(project) = request_project() else {
        return false;
    };
    REGISTER
        .read::<Vec<Lineage>>(delivered_text_digest, register::now())
        .is_some_and(|lineages| {
            lineages.iter().any(|lineage| {
                lineage.is_complete()
                    && lineage.project() == Some(project.as_str())
                    && lineage.calls.iter().any(|known| known == call_id)
            })
        })
}

/// Whether the tool that runs for `name` + `args` declares that it reads
/// every source through the admitted readers (`McpTool::publishes_every_source`).
/// A `ctx_call` wrapper is unwrapped one level, as admission does; an unknown
/// tool or an unresolvable wrapper declares nothing.
#[must_use]
pub(crate) fn declares_every_source(
    registry: &crate::server::registry::ToolRegistry,
    name: &str,
    args: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    crate::tools::registered::ctx_call::resolve(name, args)
        .ok()
        .and_then(|(inner, inner_args)| {
            let tool = registry.get_arc(&inner)?;
            Some(tool.publishes_every_source(inner_args.as_ref()))
        })
        .unwrap_or(false)
}

/// For a caller that has verified `authority` in this very step (archive
/// resolution re-admits every source before it returns content).
pub(super) fn record_verified(delivered_text_digest: &str, authority: &ArchiveAuthority) -> bool {
    store_at(
        delivered_text_digest,
        &Lineage::of_files(authority.clone()),
        None,
        register::now(),
    )
}

fn request_project() -> Option<String> {
    crate::core::policy::runtime::REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
        .and_then(|path| crate::core::pathutil::canonicalize_secure(&path).ok())
        .and_then(|path| path.to_str().map(str::to_owned))
}

fn lineage_at(digest: &str, now: i64) -> Option<Lineage> {
    let mut lineages: Vec<Lineage> = REGISTER
        .read::<Vec<Lineage>>(digest, now)?
        .into_iter()
        .filter(Lineage::is_complete)
        .map(|mut lineage| {
            // A handle names sources; which calls were answered, and under
            // which presenter entries, stays here.
            lineage.calls.clear();
            lineage.presenters.clear();
            lineage
        })
        .collect();
    let scoped = crate::core::policy::runtime::REQUEST_PROJECT
        .try_with(|slot| slot.borrow().is_some())
        .unwrap_or(false);
    if scoped {
        let project = request_project()?;
        return lineages
            .into_iter()
            .find(|lineage| lineage.project() == Some(project.as_str()));
    }
    (lineages.len() == 1).then(|| lineages.remove(0))
}

/// `call_id`: already validated by the caller ([`valid_call_id`]), or `None`.
/// The ids of the stored entry are kept; those of `lineage` are never used.
fn store_at(digest: &str, lineage: &Lineage, call_id: Option<&str>, now: i64) -> bool {
    if !lineage.is_complete() {
        return false;
    }
    REGISTER.update::<Vec<Lineage>>(digest, now, |known| {
        let mut lineages = known.unwrap_or_default();
        let position = lineages
            .iter()
            .position(|known| known.project() == lineage.project());
        let entry = if let Some(position) = position {
            // Identical text from a second source set: the union, never the
            // weaker of the two. A union beyond a bound is kept as unknown —
            // the text is then not attributed to one of its sets.
            lineages[position].join(lineage.clone());
            &mut lineages[position]
        } else if lineages.len() < MAX_LINEAGES_PER_TEXT {
            let mut fresh = lineage.clone();
            fresh.calls.clear();
            fresh.presenters.clear();
            lineages.push(fresh);
            lineages.last_mut()?
        } else {
            return None;
        };
        if let Some(call_id) = call_id {
            entry.note_call(call_id);
        }
        entry.note_presenter();
        Some(lineages)
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::core::archive::presented::{self, record_presentation};
    use crate::core::policy::runtime::{REQUEST_PROJECT, TestPolicyOverride};

    const GOVERNED: &str = "[context_gateway]\nmode = \"governed\"\nclassification = \"warn\"\n";

    fn isolated(config: &str) -> crate::core::data_dir::IsolatedDataDir {
        let data = crate::core::data_dir::isolated_data_dir();
        write_config(config);
        data
    }

    fn write_config(config: &str) {
        let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
        std::fs::create_dir_all(&dir).expect("config dir");
        std::fs::write(dir.join("config.toml"), config).expect("config");
    }

    fn project() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("project");
        let root = directory.path().canonicalize().expect("canonical root");
        (directory, root)
    }

    fn scoped<T>(root: &Path, operation: impl FnOnce() -> T) -> T {
        REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(Some(root.to_path_buf())), operation)
    }

    /// The authority the admitted reader would publish for this file.
    fn authority_of(root: &Path, name: &str, content: &str) -> ArchiveAuthority {
        let path = root.join(name);
        std::fs::write(&path, content).expect("source");
        let admitted = crate::tools::ctx_read::read_file_for_tool_rooted(
            path.to_str().expect("utf-8 path"),
            root.to_str().expect("utf-8 root"),
            "ctx_read",
        )
        .expect("admitted read");
        ArchiveAuthority::file(
            root,
            path.to_str().expect("utf-8 path"),
            "ctx_read",
            &admitted,
        )
        .expect("protection is active: the read is bound")
    }

    /// Record a file-only lineage.
    fn record(delivered_text_digest: &str, authority: &ArchiveAuthority) -> bool {
        record_lineage(delivered_text_digest, &Lineage::of_files(authority.clone()))
    }

    /// The file part of a file-only lineage; `None` for any other lineage.
    fn lineage_for(delivered_text_digest: &str) -> Option<ArchiveAuthority> {
        lineage_of(delivered_text_digest)
            .filter(|lineage| lineage.presented.is_empty())
            .and_then(|lineage| lineage.files)
    }

    fn presented_now(root: &Path, object_ref: &str) -> PresentedBinding {
        PresentedBinding::admitted_now(root, object_ref, "source-one", "sha256:aa")
            .expect("policy identity available")
    }

    /// A complete file lineage is recorded and found by the digest of the
    /// exact bytes; the handle outlives its source and then refuses.
    #[test]
    fn a_complete_lineage_is_found_and_stops_admitting_when_its_source_is_gone() {
        let _data = isolated(GOVERNED);
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        scoped(&root, || {
            let authority = authority_of(&root, "kept.rs", "pub fn kept() {}\n");
            let delivered = digest("pub fn kept() {}\n");
            assert!(lineage_of(&delivered).is_none(), "nothing on record yet");
            assert!(record(&delivered, &authority));

            let found = lineage_of(&delivered).expect("recorded lineage");
            assert_eq!(found, Lineage::of_files(authority.clone()));
            assert_eq!(found.still_admitted(), Ok(()));
            assert_eq!(lineage_for(&delivered), Some(authority));
            // Exact bytes: any re-wrapping is a different text.
            assert!(lineage_of(&digest("pub fn kept() {}")).is_none());
            assert!(lineage_of("not-a-digest").is_none());

            std::fs::remove_file(root.join("kept.rs")).expect("delete the source");
            let found = lineage_of(&delivered).expect("the handle is still on record");
            assert_eq!(
                found.still_admitted(),
                Err(LineageRefusal::SourceNoLongerAdmitted)
            );
        });
    }

    /// Unknown stays unknown: an incomplete capture, a restricted source
    /// (E3), an empty lineage and a record outside any request project leave
    /// no entry. The shared tool-delivery entry applies the same conditions.
    #[tokio::test]
    async fn incomplete_and_restricted_lineages_are_not_recorded() {
        let _data = isolated(GOVERNED);
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();

        let ((), incomplete) = super::super::authority::capture(async {
            assert!(
                ArchiveAuthority::file(&root, "/outside/of/the/project.rs", "ctx_read", "x")
                    .is_none()
            );
        })
        .await;
        let incomplete = incomplete.expect("an unbound read is reported");
        assert!(incomplete.is_incomplete());

        scoped(&root, || {
            let unknown = digest("derived from an unbound source");
            assert!(!record(&unknown, &incomplete));
            assert!(!record_verified(&unknown, &incomplete));
            assert!(!record_lineage(
                &unknown,
                &Lineage::of_presented(Vec::new())
            ));
            assert_eq!(
                Lineage::of_files(incomplete.clone()).still_admitted(),
                Err(LineageRefusal::Unknown)
            );
            assert!(lineage_of(&unknown).is_none());

            // Delivered under `classification = "warn"`, but restricted: not
            // storable, so it has no reusable lineage.
            let restricted = authority_of(&root, "plan.md", "*** TOP SECRET ***\nlaunch window\n");
            let delivered = digest("launch window");
            assert!(!record(&delivered, &restricted));
            assert_eq!(
                record_tool_delivery(true, false, Some(restricted), &["launch window"]),
                0
            );
            assert!(lineage_of(&delivered).is_none());

            // The shared entry: each condition by itself records nothing.
            let clean = authority_of(&root, "tool.rs", "pub fn tool() {}\n");
            let block = "pub fn tool() {}\n";
            assert_eq!(
                record_tool_delivery(false, false, Some(clean.clone()), &[block]),
                0
            );
            assert_eq!(
                record_tool_delivery(true, true, Some(clean.clone()), &[block]),
                0
            );
            assert_eq!(record_tool_delivery(true, false, None, &[block]), 0);
            assert_eq!(
                record_tool_delivery(true, false, Some(incomplete.clone()), &[block]),
                0
            );
            assert!(lineage_of(&digest(block)).is_none());
            assert_eq!(
                record_tool_delivery(true, false, Some(clean), &[block, "second block"]),
                2
            );
            assert!(lineage_of(&digest(block)).is_some());
            assert!(lineage_of(&digest("second block")).is_some());
        });

        let clean = scoped(&root, || {
            authority_of(&root, "clean.rs", "pub fn clean() {}\n")
        });
        let delivered = digest("pub fn clean() {}\n");
        assert!(
            !record(&delivered, &clean),
            "no request project: nothing verifies the lineage"
        );
        assert!(lineage_of(&delivered).is_none());
    }

    /// Identical text from a second source set is admitted only while both
    /// sets hold; a re-read of the same source refreshes its digest; another
    /// project's lineage is kept apart.
    #[test]
    fn identical_text_from_two_source_sets_needs_both() {
        let _data = isolated(GOVERNED);
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let (_other_directory, other_root) = project();
        let delivered = digest("pub fn shared() {}\n");
        scoped(&root, || {
            let first = authority_of(&root, "a.rs", "pub fn shared() {}\n");
            let second = authority_of(&root, "b.rs", "pub fn shared() {}\n");
            assert!(record(&delivered, &first));
            assert!(record(&delivered, &second));
            let both = lineage_of(&delivered).expect("united lineage");
            assert_ne!(both, Lineage::of_files(first.clone()));
            assert_ne!(both, Lineage::of_files(second));
            assert_eq!(both.still_admitted(), Ok(()));

            // The same source in a newer state delivers the same text again.
            let refreshed = authority_of(&root, "a.rs", "pub fn shared() {}\n// note\n");
            assert!(record(&delivered, &refreshed));
            let both = lineage_of(&delivered).expect("united lineage");
            assert_eq!(both.still_admitted(), Ok(()), "superseded digest replaced");

            std::fs::remove_file(root.join("b.rs")).expect("delete one source");
            let both = lineage_of(&delivered).expect("still on record");
            assert_eq!(
                both.still_admitted(),
                Err(LineageRefusal::SourceNoLongerAdmitted),
                "one remaining set authorizes nothing"
            );
            assert!(refreshed.still_admitted());
        });
        scoped(&other_root, || {
            assert!(
                lineage_of(&delivered).is_none(),
                "not this project's lineage"
            );
            let foreign = authority_of(&other_root, "c.rs", "pub fn shared() {}\n");
            assert!(record(&delivered, &foreign));
            assert_eq!(lineage_for(&delivered), Some(foreign));
        });
        assert!(
            lineage_of(&delivered).is_none(),
            "two projects and none in scope: ambiguous"
        );
    }

    /// Caller-supplied sources: a binding holds only while it is presented
    /// and admitted under the current policy; kinds are never mixed up; a
    /// block from a file and a presented source needs both.
    #[test]
    fn a_presented_lineage_needs_a_current_presentation_and_a_mixed_one_needs_both() {
        let _data = isolated(GOVERNED);
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let (_other_directory, other_root) = project();
        scoped(&root, || {
            let binding = presented_now(&root, "ref-quartz");
            let lineage = Lineage::of_presented(vec![binding.clone()]);
            let delivered = digest("prepared context");
            assert_eq!(
                lineage.still_admitted(),
                Err(LineageRefusal::SourceNotPresented),
                "bound, but no presentation on record"
            );
            assert!(!record_lineage(&delivered, &lineage), "nothing recorded");

            assert!(record_presentation(&binding));
            assert_eq!(lineage.still_admitted(), Ok(()));
            assert!(record_lineage(&delivered, &lineage));
            assert_eq!(lineage_of(&delivered), Some(lineage.clone()));
            assert!(
                lineage_for(&delivered).is_none(),
                "a presented lineage is never passed off as a file lineage"
            );

            // Same object reference, other content: not the same binding.
            let other_content =
                PresentedBinding::admitted_now(&root, "ref-quartz", "source-one", "sha256:bb")
                    .expect("binding");
            assert_eq!(
                Lineage::of_presented(vec![other_content]).still_admitted(),
                Err(LineageRefusal::SourceNotPresented)
            );

            // The policy changes: the presentation on record was admitted
            // under another policy identity.
            write_config("[context_gateway]\nmode = \"sovereign\"\n");
            assert_eq!(
                lineage.still_admitted(),
                Err(LineageRefusal::SourceNoLongerAdmitted)
            );
            // Presented and admitted again under the new policy: the text
            // delivered under the OLD policy stays refused until it is
            // delivered and recorded again under the current one.
            let renewed = presented_now(&root, "ref-quartz");
            assert!(record_presentation(&renewed));
            assert_eq!(
                lineage.still_admitted(),
                Err(LineageRefusal::SourceNoLongerAdmitted)
            );
            let lineage = Lineage::of_presented(vec![renewed]);
            assert!(record_lineage(&delivered, &lineage));
            assert_eq!(
                lineage_of(&delivered).expect("on record").still_admitted(),
                Ok(())
            );

            // Mixed block: the file part and the presented part must hold.
            let file = authority_of(&root, "kept.rs", "pub fn kept() {}\n");
            let mut mixed = Lineage::of_files(file);
            mixed.join(lineage.clone());
            assert!(mixed.is_complete());
            assert_eq!(mixed.still_admitted(), Ok(()));
            let mixed_text = digest("file part and prepared part");
            assert!(record_lineage(&mixed_text, &mixed));
            std::fs::remove_file(root.join("kept.rs")).expect("delete the file source");
            assert_eq!(
                lineage_of(&mixed_text).expect("on record").still_admitted(),
                Err(LineageRefusal::SourceNoLongerAdmitted)
            );

            // Parts of two projects are no lineage.
            let mut crossed = lineage.clone();
            crossed.join(Lineage::of_presented(vec![presented_now(
                &other_root,
                "ref-quartz",
            )]));
            assert!(!crossed.is_complete());
            assert_eq!(crossed.still_admitted(), Err(LineageRefusal::Unknown));
        });
        // Another project's request: the binding is not its own.
        scoped(&other_root, || {
            let foreign = Lineage::of_presented(vec![presented_now(&root, "ref-quartz")]);
            assert_eq!(
                foreign.still_admitted(),
                Err(LineageRefusal::SourceNoLongerAdmitted)
            );
        });
        // No refusal names anything.
        for refusal in [
            LineageRefusal::Unknown,
            LineageRefusal::SourceNotPresented,
            LineageRefusal::SourceNoLongerAdmitted,
        ] {
            let text = refusal.to_string();
            assert!(
                text.starts_with("lineage.") && !text.contains("quartz"),
                "{text}"
            );
        }
        assert_eq!(presented::VALIDITY_SECONDS, 600);
    }

    /// What another process sees: only the persisted, owner-only shard — no
    /// in-memory state — read back through the public API. Age, entry and
    /// size bounds hold.
    #[test]
    fn the_persisted_register_is_bounded_private_and_readable_by_another_process() {
        let _data = isolated(GOVERNED);
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        scoped(&root, || {
            let authority = authority_of(&root, "kept.rs", "pub fn kept() {}\n");
            let lineage = Lineage::of_files(authority);
            let delivered = digest("pub fn kept() {}\n");
            assert!(record_lineage(&delivered, &lineage));

            // The register has no in-memory state: this lookup decodes the
            // file exactly as a second process would.
            let shard = REGISTER.shard_path(&delivered);
            let raw = std::fs::read_to_string(&shard).expect("persisted shard");
            assert!(!raw.contains("pub fn kept"), "no content in the register");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&shard)
                    .expect("shard")
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o600, "owner-only");
            }
            assert_eq!(lineage_of(&delivered), Some(lineage.clone()));

            // Age: an expired record is not returned.
            let start = register::now();
            assert!(lineage_at(&delivered, start + MAX_AGE_SECONDS + 1).is_none());

            // Entries: one shard never holds more than its bound; the oldest
            // record goes first. Digests share the shard prefix "ab".
            let in_shard = |index: usize| format!("ab{index:062x}");
            for index in 0..REGISTER.shard_entries + 3 {
                let at = start + i64::try_from(index).expect("small index");
                assert!(store_at(&in_shard(index), &lineage, None, at));
            }
            let later = start + 1_000;
            let newest = in_shard(REGISTER.shard_entries + 2);
            assert!(lineage_at(&in_shard(0), later).is_none(), "oldest evicted");
            assert!(lineage_at(&in_shard(3), later).is_some());
            assert!(lineage_at(&newest, later).is_some());

            // Size and format: an oversized or foreign shard is not read.
            let shard = REGISTER.shard_path(&newest);
            let too_large = usize::try_from(register::MAX_SHARD_BYTES).expect("fits usize") + 1;
            std::fs::write(&shard, " ".repeat(too_large)).expect("write");
            assert!(lineage_at(&newest, later).is_none());
            std::fs::write(&shard, "{\"version\":2,\"entries\":{}}").expect("write");
            assert!(lineage_at(&newest, later).is_none());
        });
    }

    /// A delivery recorded with the host's call id answers for exactly that
    /// id, in exactly that project; the id never rides on a lineage handle,
    /// is not lost when the same text is recorded again without one, and is
    /// not copied to a derived text.
    #[test]
    fn a_delivery_is_on_record_for_the_call_it_answered_and_for_no_other() {
        let _data = isolated(GOVERNED);
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let (_other_directory, other_root) = project();
        let block = "pub fn kept() {}\n";
        let delivered = digest(block);
        scoped(&root, || {
            let authority = authority_of(&root, "kept.rs", block);
            assert!(!delivered_to_call(&delivered, "toolu_X"), "nothing yet");
            assert_eq!(
                record_tool_delivery_for_call(
                    true,
                    false,
                    Some(authority.clone()),
                    &[block],
                    Some("toolu_X"),
                ),
                1
            );
            assert!(delivered_to_call(&delivered, "toolu_X"));
            assert!(!delivered_to_call(&delivered, "toolu_Y"));
            assert!(!delivered_to_call(&digest("other text"), "toolu_X"));
            assert!(!delivered_to_call("not-a-digest", "toolu_X"));

            // The handle names sources only.
            let handle = lineage_of(&delivered).expect("on record");
            assert!(handle.calls.is_empty());
            assert_eq!(handle, Lineage::of_files(authority.clone()));

            // The same bytes again without an id (CLI, another caller), and
            // through the plain lineage record: X stays, nothing is added.
            assert_eq!(
                record_tool_delivery(true, false, Some(authority.clone()), &[block]),
                1
            );
            assert!(record(&delivered, &authority));
            assert!(delivered_to_call(&delivered, "toolu_X"));
            assert!(!delivered_to_call(&delivered, ""));

            // A malformed id is not recorded; the delivery itself is.
            let second = "pub fn second() {}\n";
            let second_authority = authority_of(&root, "second.rs", second);
            let too_long = "x".repeat(MAX_CALL_ID_BYTES + 1);
            for malformed in ["", "has space", "slash/id", too_long.as_str()] {
                assert!(!valid_call_id(malformed), "{malformed:?}");
                assert_eq!(
                    record_tool_delivery_for_call(
                        true,
                        false,
                        Some(second_authority.clone()),
                        &[second],
                        Some(malformed),
                    ),
                    1
                );
                assert!(!delivered_to_call(&digest(second), malformed));
            }
            assert!(valid_call_id(&"x".repeat(MAX_CALL_ID_BYTES)));
            assert!(valid_call_id("call_Ab-9"));

            // A text derived from the block is not a delivery of that call.
            let derived = digest("[stub of the block]");
            assert!(record_derived(&derived, &handle));
            assert!(lineage_of(&derived).is_some());
            assert!(!delivered_to_call(&derived, "toolu_X"));

            // A refused delivery notes no id either.
            let refused = "pub fn refused() {}\n";
            assert_eq!(
                record_tool_delivery_for_call(
                    true,
                    true,
                    Some(authority.clone()),
                    &[refused],
                    Some("toolu_X"),
                ),
                0
            );
            assert!(!delivered_to_call(&digest(refused), "toolu_X"));
        });
        // Another project: the same bytes there were not delivered for X.
        scoped(&other_root, || {
            assert!(!delivered_to_call(&delivered, "toolu_X"));
            let foreign = authority_of(&other_root, "kept.rs", block);
            assert_eq!(
                record_tool_delivery_for_call(true, false, Some(foreign), &[block], Some("Z")),
                1
            );
            assert!(delivered_to_call(&delivered, "Z"));
            assert!(!delivered_to_call(&delivered, "toolu_X"));
        });
        scoped(&root, || {
            assert!(delivered_to_call(&delivered, "toolu_X"));
            assert!(!delivered_to_call(&delivered, "Z"));
        });
        assert!(
            !delivered_to_call(&delivered, "toolu_X"),
            "no request project: no answer"
        );
    }

    /// An entry written before call ids existed loads unchanged and answers
    /// for no call; the bound evicts the oldest id and a repeated id counts
    /// as the newest.
    #[test]
    fn an_entry_without_call_ids_answers_for_none_and_the_bound_evicts_the_oldest() {
        let _data = isolated(GOVERNED);
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        scoped(&root, || {
            let block = "pub fn kept() {}\n";
            let authority = authority_of(&root, "kept.rs", block);
            let delivered = digest(block);
            assert!(record(&delivered, &authority));

            // The persisted form of an entry without ids has no such field:
            // it is byte-for-byte the form written before the field existed.
            let shard = REGISTER.shard_path(&delivered);
            let raw = std::fs::read_to_string(&shard).expect("persisted shard");
            assert!(!raw.contains("\"calls\""), "{raw}");
            assert_eq!(
                lineage_of(&delivered),
                Some(Lineage::of_files(authority.clone()))
            );
            assert!(!delivered_to_call(&delivered, "toolu_X"));

            let record_for = |call_id: &str| {
                record_tool_delivery_for_call(
                    true,
                    false,
                    Some(authority.clone()),
                    &[block],
                    Some(call_id),
                )
            };
            for index in 0..MAX_CALLS_PER_LINEAGE {
                assert_eq!(record_for(&format!("call_{index}")), 1);
            }
            assert!(delivered_to_call(&delivered, "call_0"));
            // Repeating the oldest id makes it the newest …
            assert_eq!(record_for("call_0"), 1);
            // … so the next new id evicts `call_1`.
            assert_eq!(record_for("call_new"), 1);
            assert!(delivered_to_call(&delivered, "call_0"));
            assert!(!delivered_to_call(&delivered, "call_1"), "oldest evicted");
            assert!(delivered_to_call(&delivered, "call_2"));
            assert!(delivered_to_call(&delivered, "call_new"));
            let raw = std::fs::read_to_string(&shard).expect("persisted shard");
            assert!(raw.contains("\"calls\""));
            assert!(!raw.contains("pub fn kept"), "no content in the register");
        });
    }
}
