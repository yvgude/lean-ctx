// SPDX-License-Identifier: Apache-2.0
//! Recovery copies bound to a [`Lineage`] (G02).
//!
//! A text that is compressed or cut down on its way to the model keeps a
//! recovery copy of the original only if the original's lineage is known,
//! and the copy is handed back only under the authority that holds at that
//! moment. The copy lives in the archive store, with its bounds and cleanup:
//!
//! - a lineage of project files only is the existing source-bound archive
//!   entry (`store_with_authority` / `retrieve_checked`), with the same id —
//!   so the pipeline's archive copy of a tool result and a recovery copy of
//!   the same text and sources are one entry;
//! - any other lineage (caller-supplied sources, or files and such sources)
//!   is an entry that carries the lineage itself and no file authority. Every
//!   other read path refuses such an entry; only
//!   [`retrieve_recovery_checked`] serves it.
//!
//! Ids are content-addressed and deterministic (#498): the same text under
//! the same lineage always has the same id. The policy a caller-supplied
//! source was admitted under is part of that identity, so a copy stored
//! under one policy is never served under another.

use super::delivered::{self, Lineage, LineageRefusal};

/// Address of a recovery copy: 64 lowercase hex characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveryId(String);

impl RecoveryId {
    /// `None` for anything that is not a canonical id: an untrusted handle
    /// never supplies path components.
    #[must_use]
    pub(crate) fn parse(id: &str) -> Option<Self> {
        (id.len() == 64
            && id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
        .then(|| Self(id.to_owned()))
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why no recovery copy is stored or returned. Content-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryRefusal {
    /// No lineage, an incomplete one, or an entry without one.
    UnknownLineage,
    /// A caller-supplied source has no current presentation.
    SourceNotPresented,
    /// A source is refused under the current rights or policy, or the stored
    /// text is withheld by the current content rules.
    SourceNoLongerAdmitted,
    /// The text may not enter a store (restricted, withheld or rewritten by
    /// the current rules, larger than the archive bound, archive disabled).
    NotStorable,
    /// No such copy (never stored, evicted, or not intact).
    Missing,
}

impl From<LineageRefusal> for RecoveryRefusal {
    fn from(refusal: LineageRefusal) -> Self {
        match refusal {
            LineageRefusal::Unknown => Self::UnknownLineage,
            LineageRefusal::SourceNotPresented => Self::SourceNotPresented,
            LineageRefusal::SourceNoLongerAdmitted => Self::SourceNoLongerAdmitted,
        }
    }
}

impl std::fmt::Display for RecoveryRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::UnknownLineage => "recovery.unknown_lineage",
            Self::SourceNotPresented => "recovery.source_not_presented",
            Self::SourceNoLongerAdmitted => "recovery.source_no_longer_admitted",
            Self::NotStorable => "recovery.not_storable",
            Self::Missing => "recovery.missing",
        })
    }
}

/// Whether exactly `original` — not a rewritten form of it — may enter a
/// store under the current project and gateway rules (E3: restricted or
/// withheld text never does). A recovery copy that differs from the original
/// would not be a recovery of it.
fn storable_exactly(original: &str) -> bool {
    !original.is_empty()
        && original.len() <= super::MAX_ARCHIVE_SIZE
        && crate::core::policy::content::protect_active(original)
            .is_ok_and(|protected| &*protected == original)
        && crate::core::context_admission::recovery::admit_for_storage(original)
            .is_some_and(|admitted| admitted == original)
}

fn lineage_id(lineage: &Lineage, content: &str) -> Option<String> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"leanctx-lineage-archive-v1\0");
    hash.update(lineage.identity_digest()?.as_bytes());
    hash.update(b"\0");
    hash.update(content.as_bytes());
    Some(hash.finalize().to_hex().to_string())
}

/// Stores a recovery copy of `original` bound to `lineage`. Call inside the
/// request's `REQUEST_PROJECT` scope. Stores nothing unless the lineage is
/// complete, every source holds now ([`Lineage::still_admitted`], which for a
/// file source includes "storable") and the text itself may enter a store
/// unchanged. Storing the same text under the same lineage again returns the
/// same id.
pub(crate) fn store_recovery_with_lineage(
    original: &str,
    lineage: &Lineage,
) -> Result<RecoveryId, RecoveryRefusal> {
    lineage.still_admitted()?;
    let storable = crate::core::policy::runtime::with_source_view(|| storable_exactly(original))
        .unwrap_or(false);
    if !storable || !super::is_enabled() {
        return Err(RecoveryRefusal::NotStorable);
    }
    if let Some(files) = lineage.files_only() {
        return super::store_with_authority(
            "recovery",
            "recovery-copy",
            original,
            None,
            Some(files),
        )
        .filter(|stored| !stored.truncated)
        .and_then(|stored| RecoveryId::parse(&stored.id))
        .inspect(issued)
        .ok_or(RecoveryRefusal::NotStorable);
    }
    let id = lineage_id(lineage, original).ok_or(RecoveryRefusal::UnknownLineage)?;
    let stored = super::with_archive_lock(|| {
        let content_path = super::content_path(&id);
        if content_path.exists() && super::meta_path(&id).exists() {
            return Some(());
        }
        std::fs::create_dir_all(super::entry_dir(&id)).ok()?;
        if !content_path.exists() {
            crate::core::atomic_fs::try_atomic_write(&content_path, original.as_bytes(), None)
                .ok()?;
            #[cfg(unix)]
            super::set_private_file_perms(&content_path);
        }
        // No FTS row and no by-content pointer: the copy is reachable by its
        // id only, through `retrieve_recovery_checked`.
        let mut entry = super::new_entry(
            &id,
            "recovery",
            "recovery-copy",
            original,
            None,
            chrono::Utc::now(),
        );
        // With the content-free reference of the presenter this copy was
        // first stored for (a digest of a verifier; authorizes nothing).
        entry.lineage = Some(lineage.recorded_for_presenter());
        if super::write_metadata(&entry).is_none() {
            let _ = std::fs::remove_file(content_path);
            return None;
        }
        Some(())
    })
    .flatten();
    match stored {
        Some(()) => RecoveryId::parse(&id)
            .inspect(issued)
            .ok_or(RecoveryRefusal::NotStorable),
        None => Err(RecoveryRefusal::NotStorable),
    }
}

/// G08: a recovery copy is stored under `id` — from here on a reference a
/// host can pass back. The receipt trace of a delivery whose bytes carry it
/// lists it (`receipt_store::references`): the id, never the copy.
fn issued(id: &RecoveryId) {
    crate::core::context_admission::receipt_store::references::note_recovery(id.as_str());
}

/// Resolves a recovery copy under CURRENT authority only. Call inside the
/// `REQUEST_PROJECT` scope of the request that asks for it. The lineage
/// stored with the copy must hold now; the stored text must still pass the
/// current content rules unchanged. The released text is registered as
/// delivered under that lineage.
pub(crate) fn retrieve_recovery_checked(id: &RecoveryId) -> Result<String, RecoveryRefusal> {
    let entry = super::read_entry(id.as_str()).ok_or(RecoveryRefusal::Missing)?;
    let Some(lineage) = entry.lineage else {
        // A file-bound copy: the archive's own checked read, which re-admits
        // every source itself (one pass, the same check as ever) and records
        // the released text as delivered.
        if entry.authority.is_none() {
            return Err(RecoveryRefusal::UnknownLineage);
        }
        return super::retrieve_checked(id.as_str()).map_err(|error| match error {
            super::ArchiveResolveError::Refused(_) => RecoveryRefusal::SourceNoLongerAdmitted,
            super::ArchiveResolveError::Missing | super::ArchiveResolveError::Malformed => {
                RecoveryRefusal::Missing
            }
        });
    };
    lineage.still_admitted()?;
    let content = crate::core::atomic_fs::read_store_file(
        &super::archive_base_dir(),
        &super::content_path(id.as_str()),
    )
    .map_err(|_| RecoveryRefusal::Missing)?;
    if content.len() > super::MAX_ARCHIVE_SIZE
        || lineage_id(&lineage, &content).as_deref() != Some(id.as_str())
    {
        // Not the text that was stored under this lineage.
        return Err(RecoveryRefusal::Missing);
    }
    // Project policy first, then the gateway's own decision on the stored
    // text — both with the equality rule, both under current rules.
    let released = crate::core::policy::runtime::with_source_view(|| {
        crate::core::policy::content::protect_active(&content)
            .is_ok_and(|protected| *protected == content)
            && readmitted(id, &content)
    })
    .unwrap_or(false);
    if !released {
        return Err(RecoveryRefusal::SourceNoLongerAdmitted);
    }
    delivered::record_lineage(&delivered::digest(&content), &lineage);
    Ok(content)
}

/// G08: the copy is admitted again in the call that asks for it. A lineage
/// that still holds says its SOURCES are still admitted; whether the stored
/// TEXT passes the Context Gateway's current rules is decided here, recorded
/// in the call's open capture and named by the recovery id — so the receipt
/// of the call carries that decision instead of none.
///
/// In every mode. The copy is bound to its id by its content, so a change
/// the gateway would make is unrepresentable: a text it would mask or
/// withhold is not released, and the bound admission records that as a
/// refusal (`source.bytes_not_preserved`), never as a redaction.
fn readmitted(id: &RecoveryId, content: &str) -> bool {
    let unchanged = crate::core::context_admission::admit_bound_context_source(content).is_ok();
    if unchanged {
        let reference = format!("recovery:{}", id.as_str());
        crate::core::context_admission::capture::note_source_id(&reference);
    }
    unchanged
}

/// The lineage stored with a recovery copy, read without resolving the copy:
/// nothing is checked, released or recorded. For a caller that must decide
/// whether the copy is its request's BEFORE the text is read.
#[must_use]
pub(crate) fn recovery_lineage(id: &RecoveryId) -> Option<Lineage> {
    super::read_entry(id.as_str())?.lineage
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::core::archive::authority::ArchiveAuthority;
    use crate::core::archive::presented::{PresentedBinding, record_presentation};
    use crate::core::policy::runtime::{REQUEST_PROJECT, TestPolicyOverride};

    const GOVERNED: &str = "[context_gateway]\nmode = \"governed\"\nclassification = \"warn\"\n";
    const ORIGINAL: &str = "pub fn kept() {}\n";

    fn isolated(config: &str) -> crate::core::data_dir::IsolatedDataDir {
        let data = crate::core::data_dir::isolated_data_dir();
        let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
        std::fs::create_dir_all(&dir).expect("config dir");
        std::fs::write(dir.join("config.toml"), config).expect("config");
        data
    }

    fn project() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("project");
        let root = directory.path().canonicalize().expect("canonical root");
        (directory, root)
    }

    fn scoped<T>(root: &Path, operation: impl FnOnce() -> T) -> T {
        REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(Some(root.to_path_buf())), operation)
    }

    fn file_lineage(root: &Path, name: &str, content: &str) -> Lineage {
        let path = root.join(name);
        std::fs::write(&path, content).expect("source");
        let admitted = crate::tools::ctx_read::read_file_for_tool_rooted(
            path.to_str().expect("utf-8 path"),
            root.to_str().expect("utf-8 root"),
            "ctx_read",
        )
        .expect("admitted read");
        Lineage::of_files(
            ArchiveAuthority::file(
                root,
                path.to_str().expect("utf-8 path"),
                "ctx_read",
                &admitted,
            )
            .expect("protection is active: the read is bound"),
        )
    }

    fn stored_files(data: &crate::core::data_dir::IsolatedDataDir) -> usize {
        fn count(directory: &Path) -> usize {
            std::fs::read_dir(directory).map_or(0, |entries| {
                entries
                    .flatten()
                    .map(|entry| {
                        let path = entry.path();
                        if path.is_dir() {
                            count(&path)
                        } else {
                            usize::from(path.extension().is_some_and(|ext| ext == "txt"))
                        }
                    })
                    .sum()
            })
        }
        count(&data.path().join("archives"))
    }

    /// File lineage: stored, resolved, deterministic id, the archive's own
    /// entry. Source deleted: refused, nothing returned. Before, a recovery
    /// copy existed under protection only if a tool call had archived exactly
    /// this text.
    #[test]
    fn a_file_bound_recovery_copy_resolves_only_while_its_source_holds() {
        let _data = isolated(GOVERNED);
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        scoped(&root, || {
            let lineage = file_lineage(&root, "kept.rs", ORIGINAL);
            let id = store_recovery_with_lineage(ORIGINAL, &lineage).expect("stored");
            assert_eq!(
                store_recovery_with_lineage(ORIGINAL, &lineage).as_ref(),
                Ok(&id),
                "identical input, identical id"
            );
            assert_eq!(retrieve_recovery_checked(&id).as_deref(), Ok(ORIGINAL));
            assert_eq!(
                crate::core::archive::retrieve_checked(id.as_str()).as_deref(),
                Ok(ORIGINAL),
                "one entry for the archive and the recovery path"
            );

            // A text derived from the original keeps the original's lineage
            // and fails together with it.
            let stub = format!("[compressed; recover with {}]", id.as_str());
            let known = delivered::lineage_of(&delivered::digest(ORIGINAL)).expect("on record");
            assert!(delivered::record_derived(&delivered::digest(&stub), &known));
            assert_eq!(
                delivered::lineage_of(&delivered::digest(&stub)),
                Some(known)
            );

            std::fs::remove_file(root.join("kept.rs")).expect("delete the source");
            assert_eq!(
                retrieve_recovery_checked(&id),
                Err(RecoveryRefusal::SourceNoLongerAdmitted)
            );
            assert_eq!(
                delivered::lineage_of(&delivered::digest(&stub))
                    .expect("still on record")
                    .still_admitted(),
                Err(LineageRefusal::SourceNoLongerAdmitted)
            );
            assert_eq!(
                store_recovery_with_lineage(ORIGINAL, &lineage),
                Err(RecoveryRefusal::SourceNoLongerAdmitted)
            );
        });
    }

    /// Nothing reaches the disk for an unknown lineage or a restricted
    /// source, and a malformed or unknown id resolves to nothing.
    #[test]
    fn unknown_or_restricted_lineage_stores_nothing() {
        let data = isolated(GOVERNED);
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        scoped(&root, || {
            assert_eq!(
                store_recovery_with_lineage(ORIGINAL, &Lineage::of_presented(Vec::new())),
                Err(RecoveryRefusal::UnknownLineage)
            );

            // Delivered under `classification = "warn"`, but restricted: the
            // source is not storable (E3).
            let marked = "*** TOP SECRET ***\nlaunch window\n";
            let restricted = file_lineage(&root, "plan.md", marked);
            assert!(matches!(
                store_recovery_with_lineage("launch window\n", &restricted),
                Err(RecoveryRefusal::SourceNoLongerAdmitted | RecoveryRefusal::NotStorable)
            ));
            // A clean source does not make restricted TEXT storable.
            let clean = file_lineage(&root, "kept.rs", ORIGINAL);
            assert_eq!(
                store_recovery_with_lineage(marked, &clean),
                Err(RecoveryRefusal::NotStorable)
            );
            assert_eq!(stored_files(&data), 0, "nothing on disk");

            assert!(RecoveryId::parse("../../etc/passwd").is_none());
            let absent = RecoveryId::parse(&"0".repeat(64)).expect("canonical id");
            assert_eq!(
                retrieve_recovery_checked(&absent),
                Err(RecoveryRefusal::Missing)
            );
        });
        // No request project: no lineage can be verified.
        let lineage = scoped(&root, || file_lineage(&root, "kept.rs", ORIGINAL));
        assert!(store_recovery_with_lineage(ORIGINAL, &lineage).is_err());
        assert_eq!(stored_files(&data), 0);
    }

    /// Caller-supplied sources: stored and resolved only while a current
    /// presentation exists; the copy is not reachable through the ordinary
    /// archive read; a copy stored under one policy is never served under
    /// another, whatever is presented afterwards.
    #[test]
    fn a_presented_recovery_copy_needs_a_current_presentation() {
        let data = isolated("");
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let prepared = "## leanctx-source-v1\ninvoice ledger\n";
        scoped(&root, || {
            let binding =
                PresentedBinding::admitted_now(&root, "ref-quartz", "source-one", "sha256:aa")
                    .expect("policy identity available");
            let lineage = Lineage::of_presented(vec![binding.clone()]);
            assert_eq!(
                store_recovery_with_lineage(prepared, &lineage),
                Err(RecoveryRefusal::SourceNotPresented),
                "bound, but never presented"
            );
            assert_eq!(stored_files(&data), 0);

            assert!(record_presentation(&binding));
            let id = store_recovery_with_lineage(prepared, &lineage).expect("stored");
            assert_eq!(
                store_recovery_with_lineage(prepared, &lineage).as_ref(),
                Ok(&id)
            );
            assert_eq!(retrieve_recovery_checked(&id).as_deref(), Ok(prepared));
            assert!(
                crate::core::archive::retrieve_checked(id.as_str()).is_err(),
                "the ordinary archive read never serves a lineage-bound copy"
            );
            assert!(
                crate::core::archive::list_entries(None)
                    .iter()
                    .all(|entry| entry.id != id.as_str()),
                "and does not list it"
            );

            // The presentation lapses (what an elapsed validity leaves).
            let presented = data
                .path()
                .join("archives")
                .join(crate::core::archive::presented::REGISTER_DIRECTORY);
            std::fs::remove_dir_all(&presented).expect("presentation register");
            assert_eq!(
                retrieve_recovery_checked(&id),
                Err(RecoveryRefusal::SourceNotPresented)
            );
            // Presented again under the same policy: readable again.
            assert!(record_presentation(&binding));
            assert_eq!(retrieve_recovery_checked(&id).as_deref(), Ok(prepared));

            // The policy changes: this copy is never served again, also not
            // after a new presentation under the new policy.
            let dir = crate::core::paths::config_dir_read_only().expect("config dir");
            std::fs::write(
                dir.join("config.toml"),
                "[context_gateway]\nmode = \"governed\"\n",
            )
            .expect("config");
            assert_eq!(
                retrieve_recovery_checked(&id),
                Err(RecoveryRefusal::SourceNoLongerAdmitted)
            );
            let renewed =
                PresentedBinding::admitted_now(&root, "ref-quartz", "source-one", "sha256:aa")
                    .expect("policy identity available");
            assert!(record_presentation(&renewed));
            assert_eq!(
                retrieve_recovery_checked(&id),
                Err(RecoveryRefusal::SourceNoLongerAdmitted)
            );
            // The same text under the new policy is another copy, another id.
            let renewed_id =
                store_recovery_with_lineage(prepared, &Lineage::of_presented(vec![renewed]))
                    .expect("stored under the current policy");
            assert_ne!(renewed_id, id);
            assert_eq!(
                retrieve_recovery_checked(&renewed_id).as_deref(),
                Ok(prepared)
            );
        });
    }

    /// G08: under protection the call that expands a presented copy admits
    /// its text again — the decision is in that call's capture, named by the
    /// recovery id — and a text the gateway would mask is never handed out,
    /// whether the store took it or not.
    #[test]
    fn a_presented_copy_is_readmitted_in_the_call_that_expands_it() {
        use crate::core::context_admission::capture::{
            ADMISSIONS, AdmissionCapture, CallSnapshot, SourceName,
        };
        let _data = isolated(GOVERNED);
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let prepared = "## leanctx-source-v1\ninvoice ledger\n";
        scoped(&root, || {
            let binding =
                PresentedBinding::admitted_now(&root, "ref-quartz", "source-one", "sha256:aa")
                    .expect("policy identity available");
            assert!(record_presentation(&binding));
            let lineage = Lineage::of_presented(vec![binding]);
            let id = store_recovery_with_lineage(prepared, &lineage).expect("stored");
            let capture = AdmissionCapture::for_call(CallSnapshot::take());
            let expanded =
                ADMISSIONS.sync_scope(Some(capture.clone()), || retrieve_recovery_checked(&id));
            assert_eq!(expanded.as_deref(), Ok(prepared));
            let (named, _) = capture.source_references();
            assert_eq!(named.len(), 1, "the expansion's own decision");
            assert_eq!(
                named[0].1,
                SourceName::Id(format!("recovery:{}", id.as_str()))
            );

            let masked = "## leanctx-source-v1\npaid with card 4111 1111 1111 1111\n";
            match store_recovery_with_lineage(masked, &lineage) {
                Ok(id) => assert_eq!(
                    retrieve_recovery_checked(&id),
                    Err(RecoveryRefusal::SourceNoLongerAdmitted)
                ),
                Err(refusal) => assert!(matches!(
                    refusal,
                    RecoveryRefusal::NotStorable | RecoveryRefusal::SourceNoLongerAdmitted
                )),
            }
        });
    }
}
