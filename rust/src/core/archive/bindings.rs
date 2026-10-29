// SPDX-License-Identifier: Apache-2.0
//! Source bindings of stored items whose own record has no place for one:
//! knowledge facts and session findings.
//!
//! The binding is kept beside the store, in a register of the archive's one
//! persistence form (`archive::register`: owner-only shards, replaced
//! atomically under a bounded lock, bounded in entries and age), keyed by the
//! item's identity. The store's own format is unchanged; an item written
//! before bindings existed, an item whose binding was evicted or has expired,
//! and an item whose text no longer is the bound text are all the same thing:
//! originless.
//!
//! A binding is written only from sources that exist AT THE WRITE: files the
//! writer named and the admitted reader returned then ([`sources_at`]), or the
//! complete source set of the very call that produced the item
//! ([`sources_of_call`]). Nothing is inferred later. Owner decision E3: a
//! restricted source or text binds nothing.

use super::authority::ArchiveAuthority;
use super::register::{self, Register};
use crate::core::context_admission::carrier::Withheld;
use crate::core::context_admission::stored::{self, Lineage, SourceRef, StoredBinding};

const REGISTER: Register = Register {
    name: "bindings",
    shard_entries: 128,
    max_age_seconds: 180 * 24 * 60 * 60,
};

/// The identity of a stored item.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Item<'a> {
    /// A knowledge fact: project root as its store names it, category, key.
    KnowledgeFact {
        project: &'a str,
        category: &'a str,
        key: &'a str,
    },
    /// A session finding: session id and the file it was recorded for.
    SessionFinding {
        session: &'a str,
        file: Option<&'a str>,
    },
}

impl Item<'_> {
    fn origin(&self) -> &'static str {
        match self {
            Self::KnowledgeFact { .. } => "knowledge:fact",
            Self::SessionFinding { .. } => "session:finding",
        }
    }

    /// Register key. A finding has no key of its own, so its text is part of
    /// its identity; a fact keeps its key when its value is replaced, and the
    /// binding's content digest then tells the old value from the new one.
    fn key(&self, text: &str) -> String {
        let parts: [&str; 4] = match *self {
            Self::KnowledgeFact {
                project,
                category,
                key,
            } => ["fact", project, category, key],
            Self::SessionFinding { session, file } => {
                ["finding", session, file.unwrap_or_default(), text]
            }
        };
        let mut hash = blake3::Hasher::new();
        hash.update(b"leanctx-item-binding-v1");
        for part in parts {
            hash.update(b"\0");
            hash.update(&(part.len() as u64).to_le_bytes());
            hash.update(part.as_bytes());
        }
        hash.finalize().to_hex().to_string()
    }
}

/// The sources a writer names for an item, each read NOW through the admitted
/// rooted reader for `tool`. A path relative to the project is resolved in
/// it. `None` when no path is named or any of them cannot be bound (missing,
/// outside the project, not permitted, restricted, only lossily readable):
/// an item is bound to all the sources it names or to none.
pub(crate) fn sources_at<S: AsRef<str>>(
    project: &str,
    paths: &[S],
    tool: &str,
) -> Option<Vec<SourceRef>> {
    if paths.is_empty() {
        return None;
    }
    paths
        .iter()
        .map(|path| {
            let path = std::path::Path::new(project).join(path.as_ref());
            SourceRef::read(path.to_str()?, project, tool)
                .ok()
                .map(|(_, source)| source)
        })
        .collect()
}

/// The sources of an item that was derived from the output of the current
/// call: the call's captured source set, which the caller passes only for a
/// tool that declares every source. Each file is read again now and must
/// deliver what the call was given. `None` for an incomplete set.
pub(crate) fn sources_of_call(capture: &ArchiveAuthority) -> Option<Vec<SourceRef>> {
    let project = capture.project();
    capture
        .sources()?
        .into_iter()
        .map(|(path, tool, admitted_digest)| {
            SourceRef::read_as_admitted(path, project, tool, admitted_digest)
        })
        .collect()
}

/// Bind `text`, as its store keeps it, to `sources`. Returns whether the item
/// is bound now. `false` — the item stays originless — when the text or a
/// source is restricted or withheld, when admission would rewrite the text
/// (the binding would be of another text), without a project under
/// protection, or when the register refuses the write.
pub(crate) fn bind(item: &Item<'_>, text: &str, project: &str, sources: Vec<SourceRef>) -> bool {
    prepare(item, text, project, sources).is_some_and(|binding| write(item, text, binding))
}

/// The binding [`bind`] would write, not yet written. `None` for every case
/// in which [`bind`] leaves the item originless before the register is asked.
fn prepare(
    item: &Item<'_>,
    text: &str,
    project: &str,
    sources: Vec<SourceRef>,
) -> Option<StoredBinding> {
    if sources.is_empty() {
        return None;
    }
    let lineage = Lineage::of(sources).ok()?;
    let (admitted, binding) =
        stored::admit_and_bind(text, item.origin(), Some(project), lineage).ok()?;
    (admitted == text).then_some(binding)
}

fn write(item: &Item<'_>, text: &str, binding: StoredBinding) -> bool {
    REGISTER.update::<StoredBinding>(&item.key(text), register::now(), |_| Some(binding))
}

/// A binding prepared for an item that is derived from bound items, to be
/// written once the item's store has kept the text ([`carry`]).
pub(crate) struct Carried(StoredBinding);

impl Carried {
    /// Write the binding for `item`, whose store keeps `text` now. `false`
    /// when `text` is not the text the binding was prepared for or the
    /// register refuses the write: the item is then NOT bound, and its store
    /// must not keep a text that was derived from bound items.
    pub(crate) fn commit(self, item: &Item<'_>, text: &str) -> bool {
        self.0.binds(text) && write(item, text, self.0)
    }
}

/// The sources a binding names, each read again now. `Err` when the bound
/// item is not served now (`stored::serve_stored`) or a source no longer
/// delivers what the binding was made from; `Ok(None)` for a binding that
/// names no file (caller-supplied text, no project).
fn held_sources(
    binding: &StoredBinding,
    text: &str,
    reader_project: Option<&str>,
) -> Result<Option<(String, Vec<SourceRef>)>, Withheld> {
    stored::serve_stored(text, Some(binding), reader_project)?;
    let Some((project, files)) = binding.file_sources() else {
        return Ok(None);
    };
    let sources = files
        .into_iter()
        .map(|(path, tool, admitted_digest)| {
            SourceRef::read_as_admitted(path, project, tool, admitted_digest)
                .ok_or_else(Withheld::source_unavailable)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some((project.to_owned(), sources)))
}

/// The binding of an item whose `text` a store DERIVES from other stored
/// items (`origins`, each with the text its store keeps): a knowledge fact
/// built from a session finding. The derived text is not the origin's text,
/// so the origin's binding cannot be copied; the derived item is bound to the
/// same files, read again now through the admitted reader, exactly as a
/// writer that names them would bind it ([`sources_at`], [`bind`]).
///
/// - `Ok(Some(_))`: every origin has a register entry that holds now. The
///   prepared binding names the joined sources; the caller stores the text
///   and then calls [`Carried::commit`].
/// - `Ok(None)`: an origin has no register entry (never bound, evicted,
///   expired) or one that names no file. The derived item is originless, as
///   its origin is; nothing is inferred from a path the origin mentions.
/// - `Err`, while protection is active: an origin is bound and is not served
///   now, a source no longer admits, the origins belong to different
///   projects, or the derived text cannot be bound (restricted, rewritten by
///   admission). The derived item must NOT be stored — it would be an unbound
///   copy of text whose source is withheld.
///
/// Without protection nothing is refused: what cannot be carried is
/// originless, as every item is in that mode.
pub(crate) fn carry(
    derived: &Item<'_>,
    text: &str,
    origins: &[(Item<'_>, &str)],
    reader_project: Option<&str>,
) -> Result<Option<Carried>, Withheld> {
    let enforced = stored::source_authority_required();
    let mut project: Option<String> = None;
    let mut sources = Vec::new();
    let mut bound = !origins.is_empty();
    for (origin, origin_text) in origins {
        let Some(binding) = binding_of(origin, origin_text) else {
            bound = false;
            continue;
        };
        let held = held_sources(&binding, origin_text, reader_project).and_then(|held| {
            let foreign = held.as_ref().is_some_and(|(root, _)| {
                project
                    .as_deref()
                    .is_some_and(|joined| joined != root.as_str())
            });
            if foreign {
                Err(Withheld::no_source_authority())
            } else {
                Ok(held)
            }
        });
        match held {
            Ok(Some((root, files))) => {
                project = Some(root);
                sources.extend(files);
            }
            Err(withheld) if enforced => return Err(withheld),
            Ok(None) | Err(_) => bound = false,
        }
    }
    let Some(project) = project.filter(|_| bound) else {
        return Ok(None);
    };
    carried(derived, text, &project, sources, enforced)
}

/// The last step of [`carry`] and [`carry_from`]: the binding of the derived
/// `text` to the sources its origins hold. While protection is active a text
/// that cannot be bound to them is refused, never stored unbound.
fn carried(
    derived: &Item<'_>,
    text: &str,
    project: &str,
    sources: Vec<SourceRef>,
    enforced: bool,
) -> Result<Option<Carried>, Withheld> {
    match prepare(derived, text, project, sources) {
        Some(binding) => Ok(Some(Carried(binding))),
        None if enforced => Err(Withheld::no_source_authority()),
        None => Ok(None),
    }
}

/// [`carry`] for ONE origin whose binding is not kept in this register: a
/// handoff ledger, whose binding lies beside the ledger file. `origin` is
/// that binding and `origin_text` the text it binds. The derived item — a
/// finding or fact restored from the ledger — is bound to EVERY file the
/// origin names, read again now; the three outcomes are those of [`carry`].
///
/// This over-approximates per item: the origin has no per-item spans, so an
/// item is bound to all the origin's files, not only to those its own text
/// came from. More sources must hold, never fewer.
pub(crate) fn carry_from(
    derived: &Item<'_>,
    text: &str,
    origin: &StoredBinding,
    origin_text: &str,
    reader_project: Option<&str>,
) -> Result<Option<Carried>, Withheld> {
    let enforced = stored::source_authority_required();
    let held = match held_sources(origin, origin_text, reader_project) {
        Ok(held) => held,
        Err(withheld) if enforced => return Err(withheld),
        Err(_) => None,
    };
    let Some((project, sources)) = held else {
        return Ok(None);
    };
    carried(derived, text, &project, sources, enforced)
}

/// What a store that COPIES `text` of `item` into another stored unit (a
/// handoff ledger) has to know about it, without delivering it:
///
/// - `Ok(Some((project, sources)))`: the item is bound and holds now — served
///   to `reader_project`, every file re-read against the digest it was bound
///   to. The copy inherits exactly these sources;
/// - `Ok(None)`: no register entry for this text, or one that names no file.
///   The copy is originless, as the item is;
/// - `Err`: the item is bound and is not served now, or a source no longer
///   delivers what it was bound to. The caller decides by the protection
///   state whether the text may be copied at all.
///
/// Nothing is published into the call's capture ([`serve`] does that): the
/// re-reads go through the admitted reader, which publishes every read, so
/// they run outside the capture. A check that ends in a refusal — or an item
/// its caller then does not copy — leaves no source behind.
pub(crate) fn held(
    item: &Item<'_>,
    text: &str,
    reader_project: Option<&str>,
) -> Result<Option<(String, Vec<SourceRef>)>, Withheld> {
    let Some(binding) = binding_of(item, text) else {
        return Ok(None);
    };
    crate::core::archive::authority::without_capture(|| {
        held_sources(&binding, text, reader_project)
    })
}

/// Whether `item`, bound in ANOTHER checkout of the same project, holds in
/// the reader's `checkout` (AP01 §4, `content` class): every file its
/// binding names, at the same path relative to the checkout it was bound
/// in, delivers in `checkout` exactly the bytes it was bound to — read now
/// through the admitted reader for `checkout`, under its policy and role.
/// Held, those files of `checkout` are published into the call's capture,
/// so the reply carries the lineage of the reader's own files.
///
/// `false` — the item must not be served there — for an item without a
/// binding of exactly `text`, a binding that names no file, a path that
/// does not lie in the bound checkout, and any file that is missing,
/// changed (another branch or revision) or no longer admitted. Refused
/// re-reads leave nothing in the capture.
pub(crate) fn holds_in_checkout(item: &Item<'_>, text: &str, checkout: &str) -> bool {
    use std::path::{Component, Path, PathBuf};
    let Some(binding) = binding_of(item, text) else {
        return false;
    };
    let Some((bound_root, files)) = binding.file_sources() else {
        return false;
    };
    let canonical = |path: &str| crate::core::pathutil::safe_canonicalize_or_self(Path::new(path));
    let bound_roots = [PathBuf::from(bound_root), canonical(bound_root)];
    let relative = |path: &str| -> Option<PathBuf> {
        let candidates = [PathBuf::from(path), canonical(path)];
        let relative = candidates.iter().find_map(|candidate| {
            bound_roots
                .iter()
                .find_map(|root| candidate.strip_prefix(root).ok())
        })?;
        relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
            .then(|| relative.to_path_buf())
    };
    let checkout_root = canonical(checkout);
    let Some(checkout_str) = checkout_root.to_str() else {
        return false;
    };
    let held = crate::core::archive::authority::without_capture(|| {
        files
            .iter()
            .map(|&(path, tool, digest)| {
                let relative = relative(path)?;
                let target = checkout_root.join(&relative);
                SourceRef::read_as_admitted(target.to_str()?, checkout_str, tool, digest)?;
                Some((relative, tool, digest))
            })
            .collect::<Option<Vec<_>>>()
    });
    let Some(held) = held else {
        return false;
    };
    for (relative, tool, digest) in held {
        if let Some(source) = ArchiveAuthority::admitted_source(
            &checkout_root,
            relative.to_str().unwrap_or_default(),
            tool,
            digest,
        ) {
            source.publish();
        }
    }
    true
}

/// The binding of `item` if it is a binding of exactly `text`.
fn binding_of(item: &Item<'_>, text: &str) -> Option<StoredBinding> {
    REGISTER
        .read::<StoredBinding>(&item.key(text), register::now())
        .filter(|binding| binding.binds(text))
}

/// For the store that is about to deliver `text` of `item` to a reader in
/// `reader_project`, while protection is active:
///
/// - a bound item is re-authorized (`stored::serve_stored`: project, every
///   source re-read, classification, content rules). `Err`: the item must NOT
///   be delivered — a bound item is as revocable as its sources. `Ok`: its
///   files are published into the call's source capture;
/// - an originless item marks the capture incomplete and returns `Ok`: whether
///   it is delivered stays the decision of its store, as before.
///
/// Without protection nothing is checked or published.
pub(crate) fn serve(
    item: &Item<'_>,
    text: &str,
    reader_project: Option<&str>,
) -> Result<(), Withheld> {
    if !stored::source_authority_required() {
        return Ok(());
    }
    let binding = binding_of(item, text);
    if let Some(binding) = &binding {
        stored::serve_stored(text, Some(binding), reader_project)?;
    }
    stored::publish_served(binding.as_ref());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::archive::delivered::LineageRefusal;
    use crate::core::context_admission::stored::lineage_support::{
        deliver, governed, project, reusable,
    };
    use crate::core::policy::runtime::TestPolicyOverride;

    fn fact<'a>(project: &'a str, key: &'a str) -> Item<'a> {
        Item::KnowledgeFact {
            project,
            category: "architecture",
            key,
        }
    }

    /// Deliver one fact the way a recall formats it.
    fn recall(root: &str, item: &Item<'_>, value: &str) -> (String, usize) {
        deliver(root, || match serve(item, value, Some(root)) {
            Ok(()) => format!("[architecture] {value}"),
            Err(withheld) => withheld.notice(),
        })
    }

    /// A fact written with a source the admitted reader returned is bound,
    /// its delivery is registered and fails with the source; the bound fact
    /// itself is then withheld. A fact written without a source, and one
    /// whose value was replaced after the binding, are originless.
    #[test]
    fn a_fact_is_bound_to_the_sources_named_at_its_write_only() {
        let _data = governed();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        std::fs::write(format!("{root}/auth.rs"), "pub fn verify_rs256() {}\n").expect("source");
        let value = "tokens are verified with RS256";
        let bound_fact = fact(&root, "token-check");
        let sources = sources_at(&root, &["auth.rs"], "ctx_knowledge").expect("readable source");
        assert!(bind(&bound_fact, value, &root, sources));

        let (reply, recorded) = recall(&root, &bound_fact, value);
        assert!(reply.contains("RS256"), "{reply}");
        assert_eq!(recorded, 1);
        assert_eq!(reusable(&root, &reply), Ok(()));

        // No source named: nothing is bound, the delivery is unknown.
        let plain = fact(&root, "typed-note");
        assert!(sources_at::<&str>(&root, &[], "ctx_knowledge").is_none());
        assert!(!bind(&plain, "typed by the agent", &root, Vec::new()));
        let (typed, recorded) = recall(&root, &plain, "typed by the agent");
        assert!(typed.contains("typed by the agent"), "{typed}");
        assert_eq!(recorded, 0);
        assert_eq!(reusable(&root, &typed), Err(LineageRefusal::Unknown));

        // The value is replaced without sources: the old binding is not a
        // binding of the new text.
        let (replaced, recorded) = recall(&root, &bound_fact, "tokens are opaque now");
        assert!(replaced.contains("opaque"), "{replaced}");
        assert_eq!(recorded, 0);

        // A named source that cannot be bound binds nothing (E3, and never a
        // partial lineage).
        std::fs::write(format!("{root}/plan.md"), "*** TOP SECRET ***\nlaunch\n").expect("marked");
        assert!(sources_at(&root, &["auth.rs", "plan.md"], "ctx_knowledge").is_none());
        assert!(sources_at(&root, &["missing.rs"], "ctx_knowledge").is_none());
        assert!(sources_at(&root, &["../outside.rs"], "ctx_knowledge").is_none());

        // The source goes: the registered reply is refused, and the bound
        // fact is no longer served.
        std::fs::remove_file(format!("{root}/auth.rs")).expect("delete");
        assert_eq!(
            reusable(&root, &reply),
            Err(LineageRefusal::SourceNoLongerAdmitted)
        );
        let (withheld, recorded) = recall(&root, &bound_fact, value);
        assert!(!withheld.contains("RS256"), "{withheld}");
        assert_eq!(recorded, 0);
    }

    /// `held` is a check, never a delivery. Its re-reads went through the
    /// publishing reader inside the call's capture: an item that held left
    /// its files there even when the next one was refused (or when its caller
    /// did not copy it), so a refusal carried a file lineage.
    #[test]
    fn a_held_check_leaves_the_capture_untouched() {
        use crate::core::archive::authority::capture_sync;
        use crate::core::context_admission::stored::lineage_support::scoped;
        let _data = governed();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        for name in ["auth.rs", "pool.rs"] {
            std::fs::write(format!("{root}/{name}"), format!("pub fn {name}() {{}}\n"))
                .expect("source");
        }
        let (holding, lapsed) = (fact(&root, "token-check"), fact(&root, "pool-size"));
        for (item, value, file) in [
            (&holding, "tokens are verified with RS256", "auth.rs"),
            (&lapsed, "the pool holds eight leases", "pool.rs"),
        ] {
            let sources = sources_at(&root, &[file], "ctx_knowledge").expect("readable source");
            assert!(bind(item, value, &root, sources));
        }
        std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");

        let ((first, second), capture) = scoped(&root, || {
            capture_sync(|| {
                (
                    held(&holding, "tokens are verified with RS256", Some(&root)),
                    held(&lapsed, "the pool holds eight leases", Some(&root)),
                )
            })
        });
        let (project, sources) = first.expect("holds").expect("bound to a file");
        assert_eq!(project, root);
        assert_eq!(sources.len(), 1);
        assert!(second.is_err(), "its source is gone");
        assert!(
            capture.is_none(),
            "a check publishes nothing, held or refused"
        );
    }

    /// A store written before bindings existed has no register entry: its
    /// items are originless, and nothing is on disk for them.
    #[test]
    fn an_item_without_a_register_entry_is_originless() {
        let data = governed();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let finding = Item::SessionFinding {
            session: "session-1",
            file: Some("src/lib.rs"),
        };
        let (reply, recorded) = deliver(&root, || {
            serve(&finding, "an old finding", Some(&root))
                .expect("originless items are not refused");
            "finding: an old finding".to_owned()
        });
        assert_eq!(recorded, 0);
        assert_eq!(reusable(&root, &reply), Err(LineageRefusal::Unknown));
        assert!(!data.path().join("archives").join("bindings").exists());
    }

    /// A finding derived from the output of a call is bound to that call's
    /// complete source set.
    #[test]
    fn a_finding_is_bound_to_the_sources_of_the_call_that_produced_it() {
        let _data = governed();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let path = format!("{root}/build.rs");
        std::fs::write(&path, "fn main() { panic!(\"linker\") }\n").expect("source");
        let admitted = crate::tools::ctx_read::read_file_for_tool_rooted(&path, &root, "ctx_read")
            .expect("admitted read");
        // What the admitted reader publishes into the call's capture.
        let capture =
            ArchiveAuthority::file(std::path::Path::new(&root), &path, "ctx_read", &admitted)
                .expect("protection is active: the read is bound");
        let finding = Item::SessionFinding {
            session: "session-1",
            file: Some("build.rs"),
        };
        let summary = "build.rs panics in the linker step";
        let sources = sources_of_call(&capture).expect("complete source set");
        assert!(bind(&finding, summary, &root, sources));
        let (reply, recorded) = deliver(&root, || {
            serve(&finding, summary, Some(&root)).expect("bound and admitted");
            format!("finding: {summary}")
        });
        assert_eq!(recorded, 1);
        assert_eq!(reusable(&root, &reply), Ok(()));

        std::fs::write(&path, "fn main() {}\n").expect("the source changes");
        assert_eq!(
            reusable(&root, &reply),
            Err(LineageRefusal::SourceNoLongerAdmitted)
        );
    }
}
