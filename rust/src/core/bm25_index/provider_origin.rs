// SPDX-License-Identifier: Apache-2.0
//! The source on record of a provider chunk (G04).
//!
//! A chunk that came from a provider (an issue, a ticket, a database row, an
//! MCP-bridge or REST result) has no file the admitted reader could return.
//! What the pipeline knows about it is its `ProviderOrigin`: provider id,
//! resource, the provider's reuse binding, the request parameters, the item
//! id and the digest of the item — references, no content, and never an
//! authorization. The index keeps that origin per chunk URI so that the
//! serve-time gate (`source_gate`) can ask the provider again; without one a
//! provider chunk stays originless.
//!
//! The origins are NOT part of the index file. The index file is positional
//! postcard, read by older binaries and — as the same `BM25Index` layout —
//! by the hosted index V2 wire format with its own byte-level preflight; a
//! new field in `CodeChunk` or `BM25Index` would be misread by every one of
//! them. They live in a JSON file beside the index:
//!
//! - an older binary never opens it and reads the index file unchanged;
//! - an index without the file (written before, or by an older binary) has
//!   no origins: its provider chunks are originless, nothing is rebuilt;
//! - an entry that no longer fits its chunk cannot serve anything: the gate
//!   compares the chunk with what the provider returns NOW.
//!
//! The same table keeps the origins of the other chunks without a readable
//! text file (`external_origin`: delivered tool output, downstream tool
//! output, extracted documents), one entry per chunk source.
//!
//! Index and origins are two files and are not written atomically together.
//! The index is renamed into place first, the origins after it; a reader
//! that finds them disagreeing is safe by construction: a chunk without an
//! entry is originless, an entry without a chunk is ignored at load and
//! pruned at the next save, and an entry that belongs to another text than
//! its chunk now holds serves nothing (provider and document origins are
//! compared with their source, the others are bound to the chunk's digest).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::BM25Index;
use super::external_origin::ChunkOrigin;
use crate::core::content_chunk::ContentChunk;

const FILE: &str = "bm25_provider_origins.json";
/// Origins of the artifact index, beside `bm25_artifacts_index.json`.
const ARTIFACT_FILE: &str = "bm25_artifact_origins.json";
/// 2: an entry names its kind. A file of another version is ignored — its
/// chunks are originless until they are ingested or extracted again.
const VERSION: u8 = 2;
/// Origins one index keeps. A chunk over the bound is ingested without one
/// (originless), never with a truncated one.
const MAX_ORIGINS: usize = 4096;
/// Serialized size of one origin (`ProviderOrigin::current_chunks` accepts
/// 16 KiB of parameters and a 4 KiB item id; a delivery lineage names at
/// most 64 sources).
const MAX_ORIGIN_BYTES: usize = 32 * 1024;
/// Size of the origins file; a larger one is neither written nor read.
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Recorded {
    version: u8,
    origins: BTreeMap<String, ChunkOrigin>,
}

/// The origins an index has on record, by chunk source (`CodeChunk::file_path`).
/// References for the serve-time gate; never an authorization.
#[derive(Debug, Clone, Default)]
pub struct ChunkOrigins(HashMap<String, ChunkOrigin>);

impl ChunkOrigins {
    /// Number of sources with an origin on record.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn get(&self, source: &str) -> Option<&ChunkOrigin> {
        self.0.get(source)
    }
}

impl BM25Index {
    /// Ingest external `ContentChunk`s into the BM25 index.
    /// Converts each chunk to a `CodeChunk` (backward-compatible) and
    /// rebuilds the inverted index. A chunk's provider origin is kept on
    /// record under the chunk's URI. Returns the number of chunks ingested.
    pub fn ingest_content_chunks(
        &mut self,
        chunks: impl IntoIterator<Item = ContentChunk>,
    ) -> usize {
        let mut count = 0usize;
        for mut chunk in chunks {
            if let Some(origin) = chunk.origin.take() {
                self.record_origin(&chunk.file_path, ChunkOrigin::Provider(Box::new(origin)));
            }
            self.add_chunk(chunk.into());
            count += 1;
        }
        if count > 0 {
            self.finalize();
        }
        count
    }

    /// Number of chunks originating from external providers.
    pub fn external_chunk_count(&self) -> usize {
        self.chunks
            .iter()
            .filter(|c| c.file_path.contains("://"))
            .count()
    }

    /// Keep `origin` on record for the chunks of `source`. An origin that
    /// does not fit the source, exceeds a bound or cannot be serialized is
    /// not kept and removes an earlier one: the source is then originless.
    pub(crate) fn record_origin(&mut self, source: &str, origin: ChunkOrigin) {
        let bounded = origin.fits(source)
            && (self.provider_origins.len() < MAX_ORIGINS
                || self.provider_origins.get(source).is_some())
            && serde_json::to_vec(&origin).is_ok_and(|bytes| bytes.len() <= MAX_ORIGIN_BYTES);
        let origins = &mut Arc::make_mut(&mut self.provider_origins).0;
        if bounded {
            origins.insert(source.to_owned(), origin);
        } else {
            origins.remove(source);
        }
    }

    /// Bind each origin to the ONE chunk this index holds for its source and
    /// keep it on record. The binding is taken from the chunk as the index
    /// stores it (after admission and the snippet cut), so the gate later
    /// compares like with like. A source with no chunk or with several gets
    /// no origin: which text it would authorize is not known.
    pub(crate) fn bind_external_origins(&mut self, origins: Vec<(String, ChunkOrigin)>) {
        for (source, origin) in origins {
            let mut held = self.chunks.iter().filter(|chunk| chunk.file_path == source);
            let bound = match (held.next(), held.next()) {
                (Some(chunk), None) => Some(origin.bound_to(&chunk.content)),
                _ => None,
            };
            match bound {
                Some(origin) => self.record_origin(&source, origin),
                None => {
                    Arc::make_mut(&mut self.provider_origins).0.remove(&source);
                }
            }
        }
    }

    /// This index with the origins recorded beside the index file in `dir`.
    #[must_use]
    pub(super) fn with_provider_origins(self, dir: &Path) -> Self {
        self.with_origins_at(&dir.join(FILE))
    }

    /// Write the origins of the provider chunks this index holds beside the
    /// index file in `dir`; none (or a failed write) leaves no file, which
    /// is the originless state.
    pub(super) fn save_provider_origins(&self, dir: &Path) {
        self.save_origins_at(dir, FILE);
    }

    /// [`Self::with_provider_origins`] for the artifact index stored at
    /// `index_file`.
    #[must_use]
    pub(crate) fn with_artifact_origins(self, index_file: &Path) -> Self {
        self.with_origins_at(&index_file.with_file_name(ARTIFACT_FILE))
    }

    /// [`Self::save_provider_origins`] for the artifact index stored at
    /// `index_file`. Call after the index file is in place.
    pub(crate) fn save_artifact_origins(&self, index_file: &Path) {
        if let Some(dir) = index_file.parent() {
            self.save_origins_at(dir, ARTIFACT_FILE);
        }
    }

    fn with_origins_at(mut self, file: &Path) -> Self {
        let held: HashSet<&str> = self
            .chunks
            .iter()
            .map(|chunk| chunk.file_path.as_str())
            .collect();
        // An entry without a chunk is ignored.
        let origins: HashMap<String, ChunkOrigin> = read(file)
            .unwrap_or_default()
            .into_iter()
            .filter(|(source, origin)| held.contains(source.as_str()) && origin.fits(source))
            .collect();
        self.provider_origins = Arc::new(ChunkOrigins(origins));
        self
    }

    fn save_origins_at(&self, dir: &Path, name: &str) {
        let path = dir.join(name);
        let held: HashSet<&str> = if self.provider_origins.is_empty() {
            HashSet::new()
        } else {
            self.chunks
                .iter()
                .map(|chunk| chunk.file_path.as_str())
                .collect()
        };
        let origins: BTreeMap<String, ChunkOrigin> = self
            .provider_origins
            .0
            .iter()
            .filter(|(source, _)| held.contains(source.as_str()))
            .map(|(source, origin)| (source.clone(), origin.clone()))
            .collect();
        let written = !origins.is_empty()
            && serde_json::to_vec(&Recorded {
                version: VERSION,
                origins,
            })
            .ok()
            .filter(|bytes| bytes.len() <= MAX_FILE_BYTES)
            .is_some_and(|bytes| {
                let tmp = dir.join(format!("{name}.tmp"));
                std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, &path).is_ok()
            });
        if !written {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn read(path: &Path) -> Option<BTreeMap<String, ChunkOrigin>> {
    if std::fs::metadata(path).ok()?.len() > MAX_FILE_BYTES as u64 {
        return None;
    }
    let recorded: Recorded = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (recorded.version == VERSION && recorded.origins.len() <= MAX_ORIGINS)
        .then_some(recorded.origins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::providers::config_provider::{ConfigProvider, schema::ProviderConfig};
    use crate::core::providers::hardened_http::redirect_tests::Server;
    use crate::core::providers::provenance::BoundResult;
    use crate::core::providers::provider_trait::ProviderParams;

    /// A provider chunk ingested with its origin is served from the loaded
    /// index while the provider still returns that item to this reader under
    /// the project's policy pack, and no longer once the pack denies the
    /// provider tool or the provider withdraws the item. Before, the origin
    /// was dropped at ingest and the chunk had no source on record.
    #[test]
    fn a_provider_chunk_is_served_only_while_its_origin_is_still_admitted() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _live_policy = crate::core::policy::runtime::lock_test_override();
        let directory = tempfile::tempdir().expect("project");
        let root = directory.path().canonicalize().expect("canonical root");
        // Project-local providers load only for a trusted workspace.
        crate::core::workspace_trust::trust(&root).unwrap();
        let configs = root.join(".lean-ctx/providers");
        std::fs::create_dir_all(&configs).expect("provider directory");
        let policy = root.join(".lean-ctx/policy.toml");
        let pack = "name='index-origin'\nversion='1.0.0'\ndescription='fixture'\n";
        std::fs::write(&policy, pack).expect("policy pack");
        let body =
            r#"[{"id":"1","title":"Orchidquartz outage","body":"Orchidquartz login path fails"}]"#;
        let server = Server::new(200, None, body);
        let config_text = format!(
            "id='index-origin-fixture'\nname='Fixture'\nbase_url='{}'\n[auth]\ntype='none'\n\
             [resources.issues]\nmethod='GET'\npath='/items'\n\
             [resources.issues.response.mapping]\nid='id'\ntitle='title'\nbody='body'\n",
            server.url
        );
        std::fs::write(configs.join("fixture.toml"), &config_text).expect("provider config");
        let config: ProviderConfig = toml::from_str(&config_text).expect("config");
        let provider = ConfigProvider::from_config(config).expect("provider");
        let acquired = BoundResult::execute(&provider, "issues", &ProviderParams::default())
            .expect("acquired");
        let chunks = acquired.chunks(&acquired.result);
        assert!(chunks.iter().all(|chunk| chunk.origin.is_some()));

        let mut written = BM25Index::new();
        written.admission_policy = Some(
            crate::core::context_admission::stores::StoreAdmission::current()
                .digest()
                .to_owned(),
        );
        assert_eq!(written.ingest_content_chunks(chunks), 1);
        written.save(&root).expect("persisted");

        let loaded = BM25Index::load(&root).expect("index of the current policy");
        assert_eq!(loaded.provider_origins.len(), 1);
        let served = || !loaded.search("orchidquartz", 5).is_empty();
        assert!(served(), "admitted origin: served under the pack");

        // The pack is tightened: the provider tool is no longer allowed.
        std::fs::write(
            &policy,
            format!("{pack}[context]\ndeny_tools = ['ctx_provider']\n"),
        )
        .expect("tightened pack");
        assert!(!served(), "the pack denies the provider");
        std::fs::write(&policy, pack).expect("pack restored");
        assert!(served());

        // The provider withdraws the item from this reader.
        server.respond(403, "denied");
        assert!(!served(), "the provider no longer returns the item");

        // No pack (Community developer mode): no source authority is
        // required, the provider is not asked and the originless rule serves
        // the chunk — search stays usable offline.
        std::fs::remove_file(&policy).expect("pack removed");
        let asked = server.count();
        assert!(served());
        assert_eq!(server.count(), asked, "no provider round trip");
    }
}
