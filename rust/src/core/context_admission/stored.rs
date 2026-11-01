// SPDX-License-Identifier: Apache-2.0
//! Current rights for stored and derived copies (G03, G04).
//!
//! A persisted copy is never its own authorization, and neither is a digest
//! of it. What decides a read is the **lineage**: the sources the copy was
//! derived from. A derived summary can lose the marking of its original, and
//! nothing in the copy changes when the original is deleted, restricted or
//! reclassified — so re-detecting the stored text proves nothing about the
//! reader's rights today. Every read therefore
//!
//! 1. resolves the project authority (bound root against the reader's root),
//! 2. re-reads **each source** through the admitted rooted reader under the
//!    policy and role current *now* and compares it with what the copy was
//!    derived from — a source that is gone, changed, reclassified, outside the
//!    reader's project or no longer permitted withholds the copy,
//! 3. checks the classification the original was admitted with, and
//! 4. only then re-admits the stored text itself as an additional check.
//!
//! Two weaker cases are explicit, never implied:
//!
//! - [`Lineage::CallerSupplied`]: a value an agent or user typed (a bus
//!   message, a manually recorded note). No source exists to re-authorize; the
//!   copy is governed by content rules and by the project authority of its
//!   writer. That is *not* a source-bound guarantee.
//! - no binding at all (a record written before bindings existed): withheld
//!   under an active project policy and in governed or sovereign mode with the
//!   tee store's wording; Community developer mode re-admits it, exactly as
//!   the originless tee rule (`core/ccr/storage.rs`) does.

use lean_ctx_protocol::context_gateway::ClassificationV1;
use serde::{Deserialize, Serialize};

use super::carrier::{RESTRICTED_CODE, Withheld, admit_carrier_text_under, current_policy};
use super::{AdmissionPolicy, fingerprint_digest};

const BINDING_VERSION: u8 = 2;
const BINDING_MISMATCH_CODE: &str = "store.binding_mismatch";
const PROJECT_MISMATCH_CODE: &str = "store.project_mismatch";
const PROJECT_UNRESOLVED_CODE: &str = "store.project_unresolved";
const SOURCE_CHANGED_CODE: &str = "store.source_changed";
const LINEAGE_UNBOUNDED_CODE: &str = "store.lineage_unbounded";
/// Same bound as `archive::authority::MAX_SOURCES`.
const MAX_SOURCES: usize = crate::core::archive::authority::MAX_SOURCES;

/// One source a stored copy was derived from: the identity the rooted reader
/// resolved, what it delivered, and how it was classified then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceRef {
    /// Path as it was handed to the rooted reader (inside the bound project).
    path: String,
    /// Tool identity the read was authorized for (path jail, role rules).
    tool: String,
    /// BLAKE3 of the admitted text the reader returned.
    admitted_digest: String,
    /// Classification of the original at derivation time.
    classification: ClassificationV1,
}

impl SourceRef {
    /// Read `path` under `root` through the admitted rooted reader and bind
    /// what it returned. This is the only way to obtain a source reference:
    /// a path alone, or text the caller read some other way, is not one.
    pub(crate) fn read(path: &str, root: &str, tool: &str) -> Result<(String, Self), Withheld> {
        let mut remaining = crate::core::limits::max_read_bytes();
        let read = Self::current(path, root, tool, &mut remaining)?;
        let source = Self {
            path: path.to_owned(),
            tool: tool.to_owned(),
            admitted_digest: content_digest(&read.content),
            classification: read.classification,
        };
        Ok((read.content, source))
    }

    /// [`Self::read`] for a source the current call has already been given:
    /// bound only if the reader returns now what it returned then
    /// (`admitted_digest`, BLAKE3 hex of that text).
    pub(crate) fn read_as_admitted(
        path: &str,
        root: &str,
        tool: &str,
        admitted_digest: &str,
    ) -> Option<Self> {
        Self::read(path, root, tool)
            .ok()
            .map(|(_, source)| source)
            .filter(|source| source.admitted_digest == admitted_digest)
    }

    /// [`Self::read_as_admitted`] against a budget shared by several sources,
    /// as [`serve_stored`] re-reads a whole lineage: a set of sources that
    /// fits one by one but not together is not bound.
    pub(crate) fn read_as_admitted_within(
        path: &str,
        root: &str,
        tool: &str,
        admitted_digest: &str,
        remaining: &mut usize,
    ) -> Option<Self> {
        let read = Self::current(path, root, tool, remaining).ok()?;
        let source = Self {
            path: path.to_owned(),
            tool: tool.to_owned(),
            admitted_digest: content_digest(&read.content),
            classification: read.classification,
        };
        (source.admitted_digest == admitted_digest).then_some(source)
    }

    /// The common rooted read, with the outcome of admitting the ORIGINAL
    /// bytes. Classification and storability are inherited from that outcome,
    /// never inferred from the delivered (possibly redacted) text: a source
    /// that is restricted or only lossily decodable is not a storable source.
    fn current(
        path: &str,
        root: &str,
        tool: &str,
        remaining: &mut usize,
    ) -> Result<crate::tools::ctx_read::AdmittedRootedRead, Withheld> {
        let read =
            crate::tools::ctx_read::read_file_for_tool_rooted_admitted(path, root, tool, remaining)
                .map_err(|_| Withheld::source_unavailable())?;
        if !read.storable || !read.complete {
            return Err(Withheld::new(&[RESTRICTED_CODE]));
        }
        Ok(read)
    }

    /// The reader's rights to this source *now*: still present, still inside
    /// the project, still permitted for the current role and policy, still
    /// delivering the same admitted bytes, original still storable and not
    /// classified higher than when the copy was derived.
    fn reauthorize(&self, root: &str, remaining: &mut usize) -> Result<(), Withheld> {
        let read = Self::current(&self.path, root, &self.tool, remaining)?;
        if content_digest(&read.content) != self.admitted_digest
            || read.classification > self.classification
        {
            return Err(Withheld::new(&[SOURCE_CHANGED_CODE]));
        }
        Ok(())
    }
}

/// Where a stored copy comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Lineage {
    /// Typed by a caller; there is no source to re-authorize (weaker).
    CallerSupplied,
    /// Derived from these sources; all of them are re-authorized on a read.
    Sources { sources: Vec<SourceRef> },
}

impl Lineage {
    /// Lineage of a derivation: caller-supplied when it used no source.
    /// Sources are deduplicated. More than [`MAX_SOURCES`] is refused — a
    /// lineage is never truncated to the sources that happened to come first.
    pub(crate) fn of(mut sources: Vec<SourceRef>) -> Result<Self, Withheld> {
        let mut unique: Vec<SourceRef> = Vec::with_capacity(sources.len());
        for source in sources.drain(..) {
            if !unique.contains(&source) {
                unique.push(source);
            }
        }
        if unique.len() > MAX_SOURCES {
            return Err(Withheld::new(&[LINEAGE_UNBOUNDED_CODE]));
        }
        Ok(if unique.is_empty() {
            Self::CallerSupplied
        } else {
            Self::Sources { sources: unique }
        })
    }
}

/// What a derived store keeps next to an admitted copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredBinding {
    version: u8,
    /// Who produced the copy (`agent:<id>`, `handoff`, `hook:<tool>`).
    origin: String,
    /// Canonical project root the copy belongs to, when one was bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    /// Role fingerprint of the writer (provenance; the reader's own rights
    /// are established by re-reading the sources under the reader's role).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    writer_role: Option<String>,
    /// Hex digest of the gateway policy snapshot the copy was admitted under.
    policy_digest: String,
    /// BLAKE3 of the admitted text, so an edited record is not served.
    content_digest: String,
    /// Most sensitive classification over the copy and all its sources.
    classification: ClassificationV1,
    lineage: Lineage,
}

impl StoredBinding {
    /// Whether this is a binding of exactly `text`. For a store that keeps
    /// bindings apart from its items (`archive::bindings`): a binding of
    /// another text is not this item's binding.
    pub(crate) fn binds(&self, text: &str) -> bool {
        self.version == BINDING_VERSION && self.content_digest == content_digest(text)
    }

    /// Who produced the copy, as its writer named it ([`admit_and_bind`]). A
    /// store that keeps more than one kind of copy tells them apart by it
    /// (`handoff_ledger`). It selects a rule of that store; it is never an
    /// authorization and never widens what [`serve_stored`] decides.
    pub(crate) fn origin(&self) -> &str {
        &self.origin
    }

    /// The files this binding names and the project they are read in, as
    /// `(project, [(path, tool, admitted digest)])`, for a store that derives
    /// a new item from the bound one (`archive::bindings::carry`). `None` for
    /// caller-supplied text and for a binding without a project. Read only:
    /// what is returned is not a source until the admitted reader returns it
    /// again ([`SourceRef::read_as_admitted`]).
    pub(crate) fn file_sources(&self) -> Option<(&str, Vec<(&str, &str, &str)>)> {
        let (Lineage::Sources { sources }, Some(project)) =
            (&self.lineage, self.project.as_deref())
        else {
            return None;
        };
        let files = sources
            .iter()
            .map(|source| {
                (
                    source.path.as_str(),
                    source.tool.as_str(),
                    source.admitted_digest.as_str(),
                )
            })
            .collect();
        Some((project, files))
    }

    /// Whether the copy is bound to sources (and not merely caller-supplied).
    #[cfg(test)]
    pub(crate) fn source_bound(&self) -> bool {
        matches!(self.lineage, Lineage::Sources { .. })
    }
}

/// Publish what a stored item that was just SERVED derives from into the
/// current call's source capture (`archive::authority`), so that a tool
/// result delivering the item has an ordinary file lineage
/// (`archive::delivered`). The one function for every store.
///
/// Call it only after [`serve_stored`] returned `Ok` for this `binding`: that
/// is the stricter check (project authority, every source re-read, no
/// classification raise, content rules). An item it refuses is not served
/// and publishes nothing. What is published is then re-checked like any
/// file source — unchanged, complete, storable under the current policy and
/// role; a later classification raise below `Restricted` is caught by
/// `serve_stored` on the next read of the item, not by the file lineage.
///
/// - source-bound item with a project: every source is published;
/// - no binding (originless), caller-supplied text, or no bound project:
///   the capture is marked incomplete. Such an item contributes no lineage,
///   and a result that contains it stays unknown. No source is ever inferred.
///
/// Without a capture, or without protection in any layer, nothing happens.
pub(crate) fn publish_served(binding: Option<&StoredBinding>) {
    use crate::core::archive::authority::{self, ArchiveAuthority};
    if authority::current_capture().is_none() || !source_authority_required() {
        return;
    }
    let bound = binding.and_then(|binding| match (&binding.lineage, &binding.project) {
        (Lineage::Sources { sources }, Some(project)) => Some((project, sources)),
        _ => None,
    });
    let Some((project, sources)) = bound else {
        authority::mark_unbound();
        return;
    };
    for source in sources {
        // A source that cannot be bound marks the capture incomplete itself.
        if let Some(source) = ArchiveAuthority::admitted_source(
            std::path::Path::new(project),
            &source.path,
            &source.tool,
            &source.admitted_digest,
        ) {
            source.publish();
        }
    }
}

fn canonical_project(root: &str) -> Option<String> {
    crate::core::pathutil::canonicalize_secure(std::path::Path::new(root))
        .ok()
        .and_then(|path| path.to_str().map(str::to_owned))
}

fn resolve_project(project: Option<&str>) -> Option<String> {
    project
        .filter(|root| !root.is_empty())
        .map(|root| canonical_project(root).unwrap_or_else(|| root.to_owned()))
}

fn content_digest(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

fn policy_digest(policy: &AdmissionPolicy) -> String {
    fingerprint_digest(&policy.fingerprint()).hex().to_owned()
}

/// The one "source authority required" predicate: protection is active in
/// some layer — a project policy pack, the global gateway in governed or
/// sovereign mode, or a global configuration that could not be loaded (mode
/// unknown). Then every stored or derived copy needs a current source
/// authority to be written or served. Otherwise (Community, developer mode)
/// originless copies keep the behaviour of the tee store.
#[must_use]
pub(crate) fn source_authority_required() -> bool {
    protected(&current_policy())
}

/// [`source_authority_required`] under a policy snapshot the caller holds,
/// so that one operation decides protection and content under the same rules.
#[must_use]
pub(crate) fn source_authority_required_under(policy: &AdmissionPolicy) -> bool {
    protected(policy)
}

fn protected(policy: &AdmissionPolicy) -> bool {
    policy.config_unloadable()
        || policy.detectors_mandatory()
        || crate::core::policy::runtime::is_active()
}

/// Admit `text` for a derived store and bind the admitted copy to `lineage`.
///
/// Admission and binding use one policy snapshot: the digest in the binding
/// is the policy the text was admitted under. Restricted content — in the
/// text or in any source — is never stored. Under an active project policy
/// and in governed or sovereign mode a copy without a project is refused: it
/// could never be re-authorized.
pub(crate) fn admit_and_bind(
    text: &str,
    origin: &str,
    project: Option<&str>,
    lineage: Lineage,
) -> Result<(String, StoredBinding), Withheld> {
    let policy = current_policy();
    let project = resolve_project(project);
    if project.is_none() && protected(&policy) {
        return Err(Withheld::new(&[PROJECT_UNRESOLVED_CODE]));
    }
    let admission = super::semantic_deferred::for_stored_context(|| {
        admit_carrier_text_under(text, Some(&policy))
    })?;
    let mut classification = admission.classification;
    if let Lineage::Sources { sources } = &lineage {
        for source in sources {
            classification = classification.join(source.classification);
        }
    }
    if !admission.storable || classification >= ClassificationV1::Restricted {
        return Err(Withheld::new(&[RESTRICTED_CODE]));
    }
    let binding = StoredBinding {
        version: BINDING_VERSION,
        origin: origin.to_owned(),
        project,
        writer_role: crate::core::policy::runtime::role_fingerprint(
            &crate::core::roles::active_role(),
        )
        .ok()
        .map(|fingerprint| fingerprint.to_hex().to_string()),
        policy_digest: policy_digest(&policy),
        content_digest: content_digest(&admission.text),
        classification,
        lineage,
    };
    Ok((admission.text, binding))
}

/// Serve a stored copy as a new delivery to a reader working in `project`.
/// See the module documentation for the order of checks.
pub(crate) fn serve_stored(
    text: &str,
    binding: Option<&StoredBinding>,
    project: Option<&str>,
) -> Result<String, Withheld> {
    let policy = current_policy();
    let protected = protected(&policy);
    let Some(binding) = binding else {
        // Legacy record: the originless rule.
        if protected {
            return Err(Withheld::no_source_authority());
        }
        return admit_stored_text(text, &policy);
    };
    if binding.version != BINDING_VERSION || binding.content_digest != content_digest(text) {
        return Err(Withheld::new(&[BINDING_MISMATCH_CODE]));
    }
    // Project authority. Protected deployments refuse anything unresolved;
    // Community developer mode refuses only a proven mismatch.
    let reader = resolve_project(project);
    match (binding.project.as_deref(), reader.as_deref()) {
        (Some(bound), Some(reader)) if bound == reader => {}
        (Some(_), Some(_)) => return Err(Withheld::new(&[PROJECT_MISMATCH_CODE])),
        _ if protected => return Err(Withheld::new(&[PROJECT_UNRESOLVED_CODE])),
        _ => {}
    }
    // Source authority: the proof of current rights.
    if let Lineage::Sources { sources } = &binding.lineage {
        let Some(root) = binding.project.as_deref() else {
            return Err(Withheld::no_source_authority());
        };
        let mut remaining = crate::core::limits::max_read_bytes();
        // A check, not a delivery: the re-reads stay out of the call's source
        // capture. A refused item publishes nothing — otherwise the sources
        // that passed before the failing one would give the refusal itself a
        // file lineage — and a served one is published by `publish_served`.
        crate::core::archive::authority::without_capture(|| {
            sources
                .iter()
                .try_for_each(|source| source.reauthorize(root, &mut remaining))
        })?;
    }
    if binding.classification >= ClassificationV1::Restricted {
        return Err(Withheld::new(&[RESTRICTED_CODE]));
    }
    // Additional check, never the proof: the copy under today's content rules.
    admit_stored_text(text, &policy)
}

fn admit_stored_text(text: &str, policy: &AdmissionPolicy) -> Result<String, Withheld> {
    let admission = super::semantic_deferred::for_stored_context(|| {
        admit_carrier_text_under(text, Some(policy))
    })?;
    if admission.storable {
        Ok(admission.text)
    } else {
        Err(Withheld::new(&[RESTRICTED_CODE]))
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod lineage_support;
