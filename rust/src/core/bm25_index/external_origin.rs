// SPDX-License-Identifier: Apache-2.0
//! Sources on record for index chunks that are neither a text file nor a
//! provider item (G01, G04).
//!
//! Three carriers used to be originless and were therefore withheld as a
//! whole wherever a source authority is required. Each now has a reference
//! the serve-time gate (`source_gate`) re-checks; a reference is never a
//! grant.
//!
//! - **Delivered tool output** ([`DeliveryOrigin`]): text an Engine tool call
//!   delivered, whose sources the call's capture named. The origin is the
//!   lineage the delivered-content register holds for exactly those bytes
//!   (`archive::delivered`) plus the digest of the chunk it is bound to. It
//!   is served only while `Lineage::still_admitted` holds — complete, every
//!   source re-read under the current policy and role and unchanged.
//! - **Downstream tool output** ([`DownstreamOrigin`]): text a foreign MCP
//!   server returned through the tool-catalog gateway. No file was read and
//!   the call cannot be replayed (its arguments are not kept, it may have
//!   side effects, MCP names no item revision), so the content cannot be
//!   re-acquired and nothing proves it current. Where a source authority is
//!   required such a chunk is therefore CLOSED: not served, not ranked, not
//!   counted — that its server and tool are still allowed is not a proof.
//!   The origin stays on record (server, tool, the server's transport
//!   identity, the chunk's digest) so the chunk becomes servable the day
//!   downstream tools get a re-read contract (what
//!   `ContextProvider::reacquire_item` and `reuse_binding` are for a
//!   provider); the check belongs in `source_gate` next to the provider arm.
//!   In Community developer mode the content rules alone decide, as for any
//!   chunk without a source authority.
//! - **Extracted document** ([`DocumentOrigin`]): text extracted from a
//!   binary project file (PDF). The origin is the digest and size of the
//!   bytes the extraction read. The path decision is the rooted reader's
//!   (`io_boundary::jail_and_check_path`: path jail and the role's
//!   secret-path rule), the open is its no-follow rooted open
//!   (`tools::ctx_read::open_rooted_source`), and a file whose bytes changed
//!   makes every chunk of it stale.
//!
//! The content of a chunk is admitted again by the gate under its own
//! gateway snapshot; nothing here admits content.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::archive::delivered::{self, Lineage};
use crate::core::policy::runtime::REQUEST_PROJECT;

/// Largest document that is extracted and re-checked (the artifact index's
/// own bound); a larger one has no origin and is not indexed.
pub(crate) const MAX_DOCUMENT_BYTES: u64 = 2_000_000;

/// The digest a chunk is identified by wherever its text is not at hand: in
/// an origin bound to one chunk, and in the row a remote vector store keeps
/// for it. BLAKE3 (hex) of the chunk text as the index stores it.
pub(crate) fn chunk_digest(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

/// The lineage of a delivered text, bound to one chunk of it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DeliveryOrigin {
    /// What the delivered text derives from. Handles only, no content.
    lineage: Lineage,
    /// BLAKE3 of the chunk text this origin is bound to.
    chunk: String,
}

impl DeliveryOrigin {
    /// The origin of `delivered_text` when the delivered-content register
    /// holds a complete lineage for exactly these bytes in `project_root`.
    /// `None`: the text was not delivered with a known lineage — it has no
    /// source on record and none is inferred.
    pub(crate) fn of_delivered(delivered_text: &str, project_root: &str) -> Option<Self> {
        let project = std::path::PathBuf::from(project_root);
        let lineage = REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(Some(project)), || {
            delivered::lineage_of(&delivered::digest(delivered_text))
        })?;
        lineage.is_complete().then(|| Self {
            lineage,
            chunk: String::new(),
        })
    }

    /// Whether this is the origin of exactly `chunk`.
    pub(super) fn binds(&self, chunk: &str) -> bool {
        self.chunk == chunk_digest(chunk)
    }

    /// Whether every source of the delivery holds now. A check, not a
    /// delivery: the re-reads stay out of the call's source capture
    /// ([`Self::publish`] follows a served chunk). Call inside the project
    /// scope of the index.
    pub(super) fn still_admitted(&self) -> bool {
        crate::core::archive::authority::without_capture(|| self.lineage.still_admitted().is_ok())
    }

    /// Give the running call the sources of a chunk that was just served.
    pub(super) fn publish(&self) {
        self.lineage.publish_files();
    }
}

/// The downstream server and tool a chunk was returned by.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DownstreamOrigin {
    server: String,
    tool: String,
    /// Digest of the server's transport identity ([`transport_identity`]).
    transport: String,
    /// BLAKE3 of the chunk text this origin is bound to.
    chunk: String,
}

/// What makes a configured server the same destination: its name and where
/// its calls go. No secret enters it — environment values, header values and
/// mementos are not part of the identity.
fn transport_identity(server: &crate::core::mcp_catalog::GatewayServer) -> Option<String> {
    use crate::core::mcp_catalog::TransportKind;
    let identity = match server.transport {
        TransportKind::Stdio => serde_json::json!({
            "name": server.name,
            "stdio": { "command": server.command, "args": server.args,
                       "binary_sha256": server.binary_sha256 },
        }),
        TransportKind::Http => serde_json::json!({
            "name": server.name,
            "http": { "url": server.url, "oauth": server.oauth },
        }),
    };
    let mut hash = blake3::Hasher::new();
    hash.update(b"leanctx-downstream-origin-v1\0");
    hash.update(&serde_json::to_vec(&identity).ok()?);
    Some(hash.finalize().to_hex().to_string())
}

impl DownstreamOrigin {
    /// The origin of a result `tool` of `server` returned.
    pub(crate) fn of(server: &crate::core::mcp_catalog::GatewayServer, tool: &str) -> Option<Self> {
        Some(Self {
            server: server.name.clone(),
            tool: tool.to_owned(),
            transport: transport_identity(server)?,
            chunk: String::new(),
        })
    }
}

/// The bytes a document's chunks were extracted from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DocumentOrigin {
    /// BLAKE3 of the file's bytes.
    digest: String,
    bytes: u64,
}

impl DocumentOrigin {
    /// Read the document `source` (relative to `root`) for `tool` the way the
    /// rooted admitted reader decides and opens a path — path jail and
    /// secret-path rule for the active role, then one no-follow open under
    /// the root — and return its bytes with the origin that identifies them.
    /// This is the one read for the extractor and for the serve-time check.
    /// `None`: refused by a path rule, not a regular file inside the root,
    /// unreadable, or larger than [`MAX_DOCUMENT_BYTES`].
    pub(crate) fn read(root: &Path, source: &str, tool: &str) -> Option<(Vec<u8>, Self)> {
        use std::io::Read;
        let path = root.join(source);
        // The rule of the rooted reader: a project the request bound is
        // kept, the root is the authority only when none is bound.
        let authority = REQUEST_PROJECT
            .try_with(|slot| slot.borrow().clone())
            .ok()
            .flatten()
            .unwrap_or_else(|| root.to_path_buf());
        REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(Some(authority)), || {
            crate::core::io_boundary::jail_and_check_path(tool, &path, root).ok()?;
            let file = crate::tools::ctx_read::open_rooted_source(&path, root).ok()?;
            let mut bytes = Vec::new();
            file.take(MAX_DOCUMENT_BYTES + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
                return None;
            }
            let origin = Self {
                digest: blake3::hash(&bytes).to_hex().to_string(),
                bytes: bytes.len() as u64,
            };
            Some((bytes, origin))
        })
    }

    /// Whether the document is still readable for `tool` and still holds the
    /// bytes its chunks were extracted from.
    pub(crate) fn unchanged(&self, root: &Path, source: &str, tool: &str) -> bool {
        Self::read(root, source, tool).is_some_and(|(_, now)| now == *self)
    }
}

/// Where a chunk without a readable text file came from. Kept per chunk
/// source beside the index (`provider_origin`), never in it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChunkOrigin {
    Provider(Box<crate::core::providers::provenance::ProviderOrigin>),
    Delivery(Box<DeliveryOrigin>),
    Downstream(DownstreamOrigin),
    Document(DocumentOrigin),
}

impl ChunkOrigin {
    /// Whether this kind of origin belongs to `source`: a document is a
    /// project path, everything else an external reference.
    pub(super) fn fits(&self, source: &str) -> bool {
        matches!(self, Self::Document(_)) != source.contains("://")
    }

    /// This origin bound to the text of the one chunk it is recorded for.
    /// Provider and document origins are compared with their source instead.
    #[must_use]
    pub(crate) fn bound_to(mut self, chunk: &str) -> Self {
        match &mut self {
            Self::Delivery(origin) => origin.chunk = chunk_digest(chunk),
            Self::Downstream(origin) => origin.chunk = chunk_digest(chunk),
            Self::Provider(_) | Self::Document(_) => {}
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bm25_index::{BM25Index, ChunkKind};
    use crate::core::content_chunk::ContentChunk;

    /// A tool-output chunk bound to the lineage of its delivery is served
    /// where a source authority is required while every source of that
    /// delivery is admitted, and no longer once one of them is withdrawn.
    /// Before, it had no source on record and was withheld as a whole.
    #[test]
    fn a_tool_output_chunk_is_served_only_while_its_delivery_lineage_is_admitted() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _live_policy = crate::core::policy::runtime::lock_test_override();
        let directory = tempfile::tempdir().expect("project");
        let root = directory.path().canonicalize().expect("canonical root");
        std::fs::create_dir_all(root.join(".lean-ctx")).expect("policy directory");
        std::fs::write(
            root.join(".lean-ctx").join("policy.toml"),
            "name='delivery-origin'\nversion='1.0.0'\ndescription='fixture'\n",
        )
        .expect("policy pack");
        let source = root.join("ledger.rs");
        std::fs::write(&source, "pub fn settle() -> u32 {\n    7319\n}\n").expect("source");
        let root_text = root.to_str().expect("utf-8 root");
        let source_text = source.to_str().expect("utf-8 path");

        // A tool call read the source through the admitted reader and
        // delivered this summary; the register holds its lineage.
        let delivered = "Quartzheron summary: settle returns the reconciliation constant";
        let scope = std::cell::RefCell::new(Some(root.clone()));
        let recorded = REQUEST_PROJECT.sync_scope(scope, || {
            let admitted = crate::tools::ctx_read::read_file_for_tool_rooted(
                source_text,
                root_text,
                "ctx_read",
            )
            .expect("admitted read");
            let authority = crate::core::archive::authority::ArchiveAuthority::file(
                &root,
                source_text,
                "ctx_read",
                &admitted,
            )
            .expect("protection is active: the read is bound");
            delivered::record_lineage(&delivered::digest(delivered), &Lineage::of_files(authority))
        });
        assert!(recorded, "lineage on record");
        let origin = DeliveryOrigin::of_delivered(delivered, root_text).expect("known lineage");

        let chunk = ContentChunk::from_provider(
            "engine",
            "tool_output",
            "summary",
            "summary",
            ChunkKind::ExternalOther,
            delivered.to_owned(),
            Vec::new(),
            None,
        );
        let uri = chunk.file_path.clone();
        // A result a foreign MCP server returned: its origin is on record,
        // but it cannot be re-read.
        let bridge = crate::core::mcp_catalog::GatewayServer {
            name: "bridge".into(),
            command: "bridge".into(),
            ..Default::default()
        };
        let foreign = ContentChunk::from_provider(
            "bridge",
            "tool_output",
            "lookup",
            "bridge::lookup",
            ChunkKind::ExternalOther,
            "Quartzheron ticket text from the bridge".to_owned(),
            Vec::new(),
            None,
        );
        let foreign_uri = foreign.file_path.clone();
        let downstream = DownstreamOrigin::of(&bridge, "lookup").expect("origin");
        let mut written = BM25Index::new();
        written.admission_policy = Some(
            crate::core::context_admission::stores::StoreAdmission::current()
                .digest()
                .to_owned(),
        );
        written.ingest_content_chunks([chunk, foreign]);
        written.bind_external_origins(vec![
            (uri.clone(), ChunkOrigin::Delivery(Box::new(origin))),
            (foreign_uri.clone(), ChunkOrigin::Downstream(downstream)),
        ]);
        written.save(&root).expect("persisted");

        let served = || -> Vec<String> {
            let loaded = BM25Index::load(&root).expect("index of the current policy");
            let hits = loaded.search("quartzheron", 5);
            hits.into_iter().map(|hit| hit.file_path).collect()
        };
        // Every source of the delivery is admitted; the downstream result
        // is closed under protection (`store.no_source_authority`).
        assert_eq!(served(), vec![uri]);

        // One source of the delivery is withdrawn: it no longer holds the
        // bytes the delivery derived from.
        std::fs::write(&source, "pub fn settle() -> u32 {\n    0\n}\n").expect("rewrite");
        assert!(served().is_empty(), "a source of the delivery changed");

        // Without protection the content rules alone decide the downstream
        // result; the delivery chunk follows the same rule there.
        std::fs::remove_file(root.join(".lean-ctx").join("policy.toml")).expect("pack removed");
        assert!(served().contains(&foreign_uri));
    }
}
