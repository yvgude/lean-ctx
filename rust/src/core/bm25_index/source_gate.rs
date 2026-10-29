// SPDX-License-Identifier: Apache-2.0
//! Current source rights for index entries at the moment they are served
//! (G04).
//!
//! An index chunk is text copied out of a source at build time. The index
//! knows which file it came from (`CodeChunk::file_path`) but nothing in the
//! chunk changes when that file is deleted, restricted, reclassified or
//! redacted by a rule that did not exist at the build. The digest an index is
//! bound to (`BM25Index::admission_policy`) covers the gateway policy only,
//! and a staleness check is a rebuild trigger, not an authorization.
//!
//! So an index its store loaded or handed out is asked again for every chunk
//! it is about to serve: the source is read NOW through the admitted rooted
//! reader (`tools::admitted_sources::read` — path jail, role, project policy,
//! gateway, decided on the original bytes), and the chunk is served only if
//! that read is storable and still contains the chunk's text. A source that
//! is gone, outside the root, withheld, restricted, lossily decoded, changed
//! or now rewritten at that place does not serve the chunk. This is the same
//! question `context_admission::stored::serve_stored` asks of a bound copy;
//! the reader is the one admission, not a second one.
//!
//! Where a source authority is required (`source_authority_required`, asked
//! for the gate's own project), a provider chunk (`scheme://…`) whose origin
//! the index has on record (`provider_origin`) is asked of its provider the
//! same way; without protection it follows the originless rule below. The item is
//! reacquired NOW through `ProviderOrigin::current_chunks` — the check the
//! knowledge store and the checkpoint import already use (provider tool
//! allowed for the role and the project policy, same project, providers
//! enabled, the provider's reuse binding unchanged, the item still returned
//! and unchanged, content under the project policy) — then passed through the
//! store admission it was written through, and the chunk is served only if
//! that text still contains it. One provider request per distinct item,
//! bounded per gate; over the bound, or without an answer, it is not served.
//!
//! The other chunks without a readable text file have an origin of their
//! own kind on record (`external_origin`), asked the same way:
//!
//! - delivered tool output (where a source authority is required): the
//!   origin is bound to this chunk and the lineage of its delivery holds
//!   (`archive::delivered::Lineage::still_admitted` — complete, every source
//!   re-read under the current policy and role and unchanged). Its sources
//!   are then published to the running call like a file read;
//! - downstream tool output (where a source authority is required): closed.
//!   A tool call cannot be replayed, so nothing proves the text current;
//!   it is not served, not ranked and not counted until downstream tools
//!   have a re-read contract (`external_origin::DownstreamOrigin`);
//! - an extracted document (in every mode, a changed file is stale for
//!   everyone): the rooted reader's path decision admits the file and its
//!   bytes are the ones the extraction read (`DocumentOrigin::unchanged`).
//!
//! In each case the chunk's text must also pass the content admission
//! unchanged. A served chunk of a provider or a document has no file
//! lineage: it marks the call's source set unknown.
//!
//! A chunk with no origin on record — a chunk written before origins were
//! kept, a recomputed source that records none — has no source authority
//! and follows the originless rule of `serve_stored`: withheld while
//! protection is active in any layer, re-admitted under the content rules in
//! Community developer mode (an extracted document must also still exist).
//!
//! ONE gateway snapshot per gate: the gateway rules are captured when the
//! gate is built (or handed in by a caller that already judged something
//! under a snapshot, [`SourceGate::with_admission`]) and every decision of
//! the gate uses them — file reads, provider re-admission, the content
//! admission of chunk text, originless decisions. A configuration that
//! changes and changes back during a call cannot admit a source the
//! snapshot refuses. The project pack and the role are those current at
//! each decision (a caller that needs them pinned holds a source view).
//!
//! The reads run inside the call's source capture and Decision Receipt: a
//! served chunk gives the result an ordinary file lineage, and a refusal is
//! counted like any other admission of that call. Nothing is cached beyond
//! one gate; a gate lives for one search.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::external_origin::ChunkOrigin;
use super::{BM25Index, ChunkOrigins, CodeChunk, SearchResult};
use crate::core::context_admission::stores::StoreAdmission;
use crate::core::providers::provenance::{ProviderOrigin, with_reuse_deadline_until};

/// Tool identity the sources of a search index are re-read for.
pub(super) const SEARCH_TOOL: &str = "ctx_search";

/// Delivery lineages one gate re-checks (each re-reads every source of one
/// delivery). Over it an undecided tool-output chunk is refused.
const MAX_LINEAGE_CHECKS: usize = 16;

/// Distinct sources one gate reads AND remembers — one bound for both. Over
/// it the gate refuses (fail closed): a source it has not decided yet is not
/// read, not served and not added to the memo; sources already decided keep
/// their decision. Fewer results, never an unchecked one, never unbounded
/// memory.
pub(crate) const MAX_SOURCES: usize = 128;

/// Provider items one gate reacquires (each is a provider request with the
/// provider's own deadline). Over it an undecided provider chunk is refused.
const MAX_PROVIDER_CHECKS: usize = 16;

/// Time one gate — one search — may spend on provider re-acquisition, counted
/// from its first provider request. A hard bound: no request is started
/// after it, and a request in flight ends at it
/// (`providers::provenance::with_reuse_deadline_until` — the earlier of this
/// deadline and the provider's own). The provider chunks left undecided are
/// refused (not remembered) and the search returns what was decided. Three
/// seconds keeps an interactive search answerable while leaving room for a
/// few items of a healthy provider.
const PROVIDER_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

/// The serve-time check of one search over one index.
pub(crate) struct SourceGate {
    root: PathBuf,
    tool: &'static str,
    /// Per source asked so far: the admitted text as the reader returns it
    /// now, with line endings normalised; `None` when it is not admitted.
    sources: HashMap<String, Option<String>>,
    /// Per source whose origin is not compared with a source text (delivered
    /// or downstream tool output, a document): whether its authority holds.
    authorities: HashMap<String, bool>,
    /// The gateway rules every decision of this gate is made under.
    admission: StoreAdmission,
    /// Origins the index has on record, by chunk source.
    origins: Arc<ChunkOrigins>,
    provider_checks: usize,
    lineage_checks: usize,
    /// End of this gate's provider budget, set at its first provider
    /// request ([`PROVIDER_BUDGET`]).
    provider_deadline: Option<std::time::Instant>,
    /// The root as the request scope names it (canonical when resolvable).
    scope_root: PathBuf,
    /// Whether a source authority is required for the project of this
    /// gate's root; asked once per gate.
    protected: Option<bool>,
    withheld: usize,
}

impl SourceGate {
    /// A gate that captures the gateway rules as they are configured now
    /// (`carrier::gateway_snapshot`, unobserved) and decides everything
    /// under them.
    pub(crate) fn new(root: &Path, tool: &'static str) -> Self {
        Self::with_admission(
            root,
            tool,
            &crate::core::context_admission::carrier::gateway_snapshot(),
        )
    }

    /// A gate that decides under the gateway snapshot its caller already
    /// holds: what the caller judged under `admission` and the sources this
    /// gate reads are then judged under the same gateway rules.
    pub(crate) fn with_admission(
        root: &Path,
        tool: &'static str,
        admission: &StoreAdmission,
    ) -> Self {
        Self {
            root: root.to_path_buf(),
            tool,
            sources: HashMap::new(),
            authorities: HashMap::new(),
            admission: admission.clone(),
            origins: Arc::default(),
            provider_checks: 0,
            lineage_checks: 0,
            provider_deadline: None,
            scope_root: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
            protected: None,
            withheld: 0,
        }
    }

    /// Run `operation` in the project scope of this gate's root: the gate
    /// decides for the index it belongs to, whatever project the calling
    /// request or the working directory names. A request already bound to
    /// this project keeps its scope untouched (and with it an open source
    /// view). Under a request bound to ANOTHER project with a source view
    /// open, `policy::runtime::active()` answers deny-all for the rebound
    /// scope and invalidates that view: the gate then sees protection, and
    /// the enclosing operation ends with an error — closed, never the other
    /// project's rules applied to this index.
    fn in_root_scope<T>(&self, operation: impl FnOnce() -> T) -> T {
        use crate::core::policy::runtime::REQUEST_PROJECT;
        let bound = REQUEST_PROJECT
            .try_with(|slot| slot.borrow().clone())
            .ok()
            .flatten();
        if bound.is_some_and(|bound| {
            bound == self.scope_root || bound.canonicalize().is_ok_and(|b| b == self.scope_root)
        }) {
            return operation();
        }
        REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(self.scope_root.clone())),
            operation,
        )
    }

    /// Whether protection is active in some layer for this gate's project
    /// (`stored::source_authority_required_under`): the gateway side is read
    /// from THIS gate's snapshot — the mode cannot differ from the rules the
    /// gate admits under —, the pack side from the project policy in scope
    /// (pinned for the whole search by [`pinned`]). Asked once per gate.
    pub(crate) fn protected(&mut self) -> bool {
        if let Some(protected) = self.protected {
            return protected;
        }
        let protected = self.in_root_scope(|| {
            crate::core::context_admission::stored::source_authority_required_under(
                self.admission.admission_policy(),
            )
        });
        self.protected = Some(protected);
        protected
    }

    /// This gate for an index that has `origins` on record.
    #[must_use]
    pub(crate) fn with_origins(mut self, origins: &Arc<ChunkOrigins>) -> Self {
        self.origins = Arc::clone(origins);
        self
    }

    /// Whether this gate already holds as many decisions as it may.
    fn over_bound(&self) -> bool {
        self.sources.len() + self.authorities.len() >= MAX_SOURCES
    }

    /// The decision for a source whose origin is not compared with a source
    /// text, asked once per gate in the gate's project scope. Over the bound:
    /// refused, neither asked nor remembered.
    fn authority_holds(&mut self, source: &str, check: impl FnOnce(&Self) -> bool) -> bool {
        if let Some(&holds) = self.authorities.get(source) {
            return holds;
        }
        if self.over_bound() {
            return false;
        }
        let holds = self.in_root_scope(|| check(self));
        self.authorities.insert(source.to_owned(), holds);
        holds
    }

    /// Whether the content admission, under this gate's gateway snapshot and
    /// the current project policy, leaves `text` exactly as it is and lets
    /// it be kept: a copy the rules would withhold, restrict or rewrite is
    /// not the copy the index holds. `label` is the source the text was
    /// admitted under when it has one (a path, an `mcp://` reference).
    fn admits_verbatim(&self, text: &str, label: Option<&str>) -> bool {
        use crate::core::context_admission as admission;
        self.in_root_scope(|| {
            let admitted = match label {
                Some(label) => admission::admit_source_with_snapshot(
                    text,
                    label,
                    false,
                    admission::OutputCompleteness::complete(),
                    &self.admission,
                )
                .ok(),
                None => admission::carrier::admit_carrier_text_under(
                    text,
                    Some(self.admission.admission_policy()),
                )
                .ok(),
            };
            admitted.is_some_and(|admitted| admitted.storable && admitted.text == text)
        })
    }

    /// The originless rule (`serve_stored` without a binding) under this
    /// gate's snapshot: withheld where a source authority is required,
    /// otherwise the content admission alone.
    fn originless(&mut self, text: &str) -> bool {
        !self.protected() && self.admits_verbatim(text, None)
    }

    /// A chunk was served whose source the call's capture cannot name (a
    /// provider item, a downstream result, a document): where a source
    /// authority is required the call's source set is then unknown, never
    /// silently the files alone. Always `true`.
    fn served_without_file_lineage(&mut self) -> bool {
        if self.protected() {
            crate::core::archive::authority::mark_unbound();
        }
        true
    }

    /// The decision already on record for `text` of `source`, without a read
    /// and without counting: `None` while the source is undecided.
    pub(super) fn decided(&self, source: &str, text: &str) -> Option<bool> {
        let admitted = self.sources.get(source)?;
        Some(
            admitted
                .as_deref()
                .is_some_and(|current| current.contains(normalised(text).as_str())),
        )
    }

    /// The sources this gate decided are not admitted now.
    pub(crate) fn refused_sources(&self) -> impl Iterator<Item = &str> {
        self.sources
            .iter()
            .filter(|(_, admitted)| admitted.is_none())
            .map(|(source, _)| source.as_str())
    }

    /// Whether `text`, derived from `source` (a path relative to the root,
    /// or an external reference), may be served now.
    pub(crate) fn serves(&mut self, source: &str, text: &str) -> bool {
        let served = self.decide(source, text);
        if !served {
            self.withheld += 1;
        }
        served
    }

    /// Entries this gate refused. A count only: no path, no text.
    #[cfg(test)]
    pub(crate) fn withheld(&self) -> usize {
        self.withheld
    }

    fn decide(&mut self, source: &str, text: &str) -> bool {
        let external = source.contains("://");
        let document = !external && crate::core::binary_detect::has_binary_extension(source);
        let origins = Arc::clone(&self.origins);
        let recorded = origins.get(source);
        if let (true, Some(ChunkOrigin::Document(origin))) = (document, recorded) {
            // In every mode: a document whose bytes changed is stale.
            let label = self.root.join(source);
            return self.authority_holds(source, |gate| {
                origin.unchanged(&gate.root, source, gate.tool)
            }) && self.admits_verbatim(text, label.to_str())
                && self.served_without_file_lineage();
        }
        // An external origin is asked only where a source authority is
        // required. Without protection (Community developer mode) an
        // external chunk follows the originless rule: content re-admitted,
        // no provider round trip, search usable offline.
        let origin = match recorded {
            Some(recorded) if external && self.protected() => match recorded {
                ChunkOrigin::Provider(origin) => Some(&**origin),
                ChunkOrigin::Delivery(origin) => {
                    if !origin.binds(text) {
                        return false;
                    }
                    if !self.authorities.contains_key(source) {
                        if self.lineage_checks >= MAX_LINEAGE_CHECKS {
                            return false;
                        }
                        self.lineage_checks += 1;
                    }
                    let served = self.authority_holds(source, |_| origin.still_admitted())
                        && self.admits_verbatim(text, None);
                    if served {
                        origin.publish();
                    }
                    return served;
                }
                // Closed: a downstream result cannot be re-read, so nothing
                // proves it current. That its server and tool are still
                // allowed is not such a proof (`DownstreamOrigin`). The
                // refusal is the originless one, `store.no_source_authority`.
                ChunkOrigin::Downstream(_) => return false,
                ChunkOrigin::Document(_) => None,
            },
            _ => None,
        };
        if origin.is_none() && (external || document) {
            // No source that is asked: the originless rule, judged for this
            // gate's project. An extracted document must at least still exist.
            return (external || self.root.join(source).is_file()) && self.originless(text);
        }
        if let Some(served) = self.decided(source, text) {
            return served && (origin.is_none() || self.served_without_file_lineage());
        }
        if self.over_bound() {
            // Over the bound: refused, neither read nor remembered.
            return false;
        }
        let admitted = match origin {
            Some(origin) => {
                let deadline = *self
                    .provider_deadline
                    .get_or_insert_with(|| std::time::Instant::now() + PROVIDER_BUDGET);
                if self.provider_checks >= MAX_PROVIDER_CHECKS
                    || std::time::Instant::now() >= deadline
                {
                    // Over the budget: refused, neither asked nor remembered.
                    return false;
                }
                self.provider_checks += 1;
                let admitted = self.read_provider(origin, deadline);
                if admitted.is_none() && std::time::Instant::now() >= deadline {
                    // Cut off by the budget, not answered: not a decision.
                    return false;
                }
                admitted
            }
            None => self.read(source),
        };
        self.sources.insert(source.to_owned(), admitted);
        self.decided(source, text).unwrap_or(false)
            && (origin.is_none() || self.served_without_file_lineage())
    }

    /// What the provider returns for `origin` NOW, as the store admission
    /// would keep it; `None` when the item is not admitted to this reader.
    fn read_provider(
        &self,
        origin: &ProviderOrigin,
        deadline: std::time::Instant,
    ) -> Option<String> {
        let root = self.root.to_str()?;
        let view = match crate::core::policy::runtime::with_project_source_view(root, || {
            with_reuse_deadline_until(Some(deadline), || origin.current_chunks(root))
        }) {
            Ok(Ok(view)) => view,
            Ok(Err(refusal)) => {
                // The kind only, never the item: a search must not tell an
                // item it withholds from one that does not exist (K02). The
                // call's receipt records the refused re-check (C04).
                crate::core::context_admission::record_recheck_refusal(
                    &self.admission,
                    refusal.validity,
                );
                return None;
            }
            Err(_) => return None,
        };
        let mut current = String::new();
        for chunk in &view.chunks {
            let admitted = self
                .admission
                .admit(&chunk.content, Path::new(&chunk.file_path))?;
            current.push_str(&normalised(&admitted));
            current.push('\n');
        }
        Some(current)
    }

    /// The text of `source` as the admitted reader returns it now, when the
    /// original may be kept in a derived store (complete, not restricted).
    /// The gateway rules are this gate's snapshot; each read is bounded by
    /// the read cap (the gate never shared a byte budget across sources).
    fn read(&self, source: &str) -> Option<String> {
        let path = self.root.join(source);
        // NOTE the argument order of this entry: root first.
        let read = crate::tools::admitted_sources::read_with_admission(
            self.root.to_str()?,
            path.to_str()?,
            self.tool,
            &self.admission,
        );
        read.storable().map(normalised)
    }
}

/// Line endings are not content: an index keeps `\n`-joined lines of a
/// source that may use `\r\n`.
fn normalised(text: &str) -> String {
    text.lines().collect::<Vec<_>>().join("\n")
}

/// Whether `chunk` may be served through `gate`. Without a gate — a view
/// built in the running call from sources the reader returned then — every
/// chunk is served.
pub(crate) fn chunk_served(gate: &mut Option<SourceGate>, chunk: &CodeChunk) -> bool {
    gate.as_mut()
        .is_none_or(|gate| gate.serves(&chunk.file_path, &chunk.content))
}

impl BM25Index {
    /// This index as its store hands it out for `root`: every search over it
    /// is gated ([`Self::source_gate`]).
    #[must_use]
    pub(crate) fn rooted_at(mut self, root: &Path) -> Self {
        self.source_root = Some(root.to_path_buf());
        self
    }

    /// The gate of one search: `None` for an index without a root.
    pub(crate) fn source_gate(&self) -> Option<SourceGate> {
        self.source_root
            .as_deref()
            .map(|root| SourceGate::new(root, SEARCH_TOOL).with_origins(&self.provider_origins))
    }

    /// The `top_k` best chunks for `query` that may be served now. Pack and
    /// role are pinned for the whole search (`pinned`); if either changed
    /// while it ran, nothing is returned.
    pub fn search(&self, query: &str, top_k: usize) -> Vec<SearchResult> {
        pinned(self.source_root.as_deref(), || {
            self.search_gated(query, top_k, &mut self.source_gate())
        })
        .unwrap_or_default()
    }
}

/// Run one search over an index handed out for `root` with the project pack
/// and the role pinned from its first decision to its last
/// (`policy::runtime::with_project_source_view`): every gate decision inside
/// sees one pack and one role, and a change of either while the search ran
/// makes the view report it — the result is then `None` and the caller
/// returns nothing. Nested calls share the outermost view.
///
/// A request bound to ANOTHER project keeps its own view; the gate's
/// rebinding to its root then answers deny-all and invalidates that view
/// (`SourceGate::in_root_scope`), so such a search also ends in `None`. A
/// root that cannot be resolved has no view: `None`. Without a root (a view
/// built in the running call, no gate) the search runs as it is.
pub(crate) fn pinned<T>(root: Option<&Path>, search: impl FnOnce() -> T) -> Option<T> {
    let Some(root) = root else {
        return Some(search());
    };
    // An index of a root the request's Workspace does not contain (detached,
    // or never attached) serves nothing (#2120).
    if !crate::core::workspace_authority::root_authorized(root) {
        return None;
    }
    crate::core::policy::runtime::with_project_source_view(root.to_str()?, search).ok()
}

/// How many candidates a similarity search fetches to serve `top_k` through
/// a gate: a refused candidate must not cost a result slot, and the walk
/// stays bounded. More refusals than the surplus mean fewer results.
#[cfg(feature = "embeddings")]
pub(crate) fn overfetch(top_k: usize) -> usize {
    top_k.saturating_add(top_k.clamp(16, MAX_SOURCES))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::policy::runtime::TestPolicyOverride;

    /// A chunk of a persisted index is served while the admitted reader still
    /// returns its text, and is no longer served once the source is withdrawn:
    /// deleted, or rewritten so that the stored text is no longer in it. The
    /// next chunk takes its place; an index without a root is not asked.
    #[test]
    fn a_loaded_index_serves_a_chunk_only_while_its_source_admits_it() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = TestPolicyOverride::set(None);
        let project = tempfile::tempdir().expect("project");
        let root = project.path();
        let withdrawn = root.join("billing.rs");
        std::fs::write(
            &withdrawn,
            "pub fn settle_canary() -> u32 {\n    let reconciliation = 7319;\n    reconciliation\n}\n",
        )
        .expect("source");
        std::fs::write(
            root.join("ledger.rs"),
            "pub fn settle_ledger() -> u32 {\n    let reconciliation = 42;\n    reconciliation\n}\n",
        )
        .expect("source");

        let built = BM25Index::build_from_directory(root);
        built.save(root).expect("persisted");
        let files = |index: &BM25Index| -> Vec<String> {
            index
                .search("reconciliation", 10)
                .into_iter()
                .map(|hit| hit.file_path)
                .collect()
        };

        let loaded = BM25Index::load(root).expect("index of the current policy");
        assert!(loaded.source_root.is_some());
        let served = files(&loaded);
        assert!(served.iter().any(|file| file == "billing.rs"), "{served:?}");
        assert!(served.iter().any(|file| file == "ledger.rs"), "{served:?}");
        assert!(!loaded.search("settle_canary", 5).is_empty());
        // The same chunks without a root (taken before the source is
        // withdrawn: a search that finds it deleted evicts it from the store).
        let unrooted = BM25Index::load_persisted(root).expect("persisted file");

        // Rewritten: the stored text is no longer what the reader returns.
        std::fs::write(&withdrawn, "pub fn unrelated() {}\n").expect("rewrite");
        let mut gate = loaded.source_gate();
        let hits = loaded.search_gated("reconciliation", 10, &mut gate);
        assert!(hits.iter().all(|hit| hit.file_path == "ledger.rs"));
        assert!(!hits.is_empty());
        assert!(gate.as_ref().is_some_and(|gate| gate.withheld() >= 1));

        // Deleted: nothing of it is served, name and path included, and the
        // chunk of the other source takes the slot.
        std::fs::remove_file(&withdrawn).expect("delete");
        let served = files(&loaded);
        assert!(!served.is_empty());
        assert!(served.iter().all(|file| file == "ledger.rs"), "{served:?}");
        assert!(loaded.search("settle_canary", 5).is_empty());

        // Over the bound: an undecided source is refused and not remembered.
        let mut full = SourceGate::new(root, SEARCH_TOOL);
        for n in 0..MAX_SOURCES {
            full.sources.insert(format!("known-{n}.rs"), None);
        }
        assert!(!full.serves("ledger.rs", "reconciliation"));
        assert_eq!(full.sources.len(), MAX_SOURCES);

        // Without a root: a fresh view, not asked again.
        assert!(unrooted.source_root.is_none());
        assert!(files(&unrooted).iter().any(|file| file == "billing.rs"));
    }
}
