use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::knowledge::{ConsolidatedInsight, KnowledgeFact, ProjectPattern};

use super::graph_model::ContextGraph;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PackageContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge: Option<KnowledgeLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<GraphLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patterns: Option<PatternsLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gotchas: Option<GotchasLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_graph: Option<ContextGraph>,
    /// `kind=addon` payload (GH #724/#726): the embedded addon manifest.
    /// Absent for every other kind — enforced by
    /// [`super::verify::validate_kind_coherence`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addon: Option<AddonContent>,
    /// `kind=skills` payload (GH #724/#727): named, verified content blobs.
    /// Absent for every other kind — enforced by
    /// [`super::verify::validate_kind_coherence`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documents: Option<DocumentsContent>,
}

/// Distribution view of an addon (unified distribution, GH #726): the
/// authoring `lean-ctx-addon.toml` embedded **verbatim**. The TOML is the
/// single source of truth — MCP wiring, capabilities, `[install]` bootstrap
/// and the per-platform `[artifacts]` tables (GH #725) all live inside it,
/// so nothing is duplicated at the pack layer that could drift.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddonContent {
    /// Verbatim `lean-ctx-addon.toml` text (authoring contract
    /// `docs/contracts/addon-manifest-v1.md`).
    pub manifest_toml: String,
}

/// `kind=skills` payload (GH #727): a set of named, verified content blobs
/// (markdown/scripts). **No execution semantics in lean-ctx** — skills are
/// verified *content*; interpretation belongs to the consumer (an addon like
/// lean-md, or the agent itself).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DocumentsContent {
    /// Sorted by `path` (byte order) — deterministic pack bytes (#498).
    pub files: Vec<DocumentBlob>,
}

/// Body encoding marker for [`DocumentBlob::body`]. The only supported value;
/// a field (not an enum) so future encodings fail with "unsupported encoding"
/// on old readers instead of a serde parse error.
pub const DOCUMENT_ENCODING_ZSTD_B64: &str = "zstd+base64";

/// Per-file caps (plaintext bytes) — a skills pack is documentation and
/// scripts, not a media archive.
pub const MAX_DOCUMENT_FILES: usize = 256;
pub const MAX_DOCUMENT_FILE_BYTES: usize = 1024 * 1024;
pub const MAX_DOCUMENTS_TOTAL_BYTES: usize = 8 * 1024 * 1024;

/// One named blob: `path` + SHA-256 of the **plaintext** + compressed body.
/// The hash pins the decoded bytes, so tampering with the stored body (or a
/// decompression bug) is detected before anything lands on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentBlob {
    /// Relative, `/`-separated path inside the pack (e.g. `skills/review.md`).
    pub path: String,
    /// SHA-256 (lowercase hex) of the plaintext bytes.
    pub sha256: String,
    /// Body encoding — currently always [`DOCUMENT_ENCODING_ZSTD_B64`].
    pub encoding: String,
    /// base64(zstd(plaintext)).
    pub body: String,
}

impl DocumentBlob {
    /// Build a blob from plaintext bytes (deterministic: fixed zstd level).
    pub fn from_plaintext(path: &str, bytes: &[u8]) -> Result<Self, String> {
        let compressed =
            zstd::encode_all(bytes, 3).map_err(|e| format!("zstd compress {path}: {e}"))?;
        Ok(Self {
            path: path.to_string(),
            sha256: sha256_hex_of(bytes),
            encoding: DOCUMENT_ENCODING_ZSTD_B64.to_string(),
            body: base64_encode(&compressed),
        })
    }

    /// Decode and verify the body against its `sha256` pin. Any mismatch —
    /// tampered body, wrong hash, corrupt compression — is an error; callers
    /// never see unverified bytes.
    pub fn decode_verified(&self) -> Result<Vec<u8>, String> {
        if self.encoding != DOCUMENT_ENCODING_ZSTD_B64 {
            return Err(format!(
                "`{}`: unsupported encoding `{}` (newer lean-ctx required)",
                self.path, self.encoding
            ));
        }
        let compressed = base64_decode(&self.body)
            .map_err(|e| format!("`{}`: body is not valid base64: {e}", self.path))?;
        // Cap the decompressed size before allocating: a hostile blob must
        // not zstd-bomb the installer.
        let plain = zstd::bulk::decompress(&compressed, MAX_DOCUMENT_FILE_BYTES + 1)
            .map_err(|e| format!("`{}`: zstd decompress failed: {e}", self.path))?;
        if plain.len() > MAX_DOCUMENT_FILE_BYTES {
            return Err(format!(
                "`{}`: decoded size exceeds the {} byte cap",
                self.path, MAX_DOCUMENT_FILE_BYTES
            ));
        }
        let actual = sha256_hex_of(&plain);
        if !actual.eq_ignore_ascii_case(&self.sha256) {
            return Err(format!(
                "`{}`: content hash mismatch — expected {}, got {actual} (tampered blob)",
                self.path, self.sha256
            ));
        }
        Ok(plain)
    }
}

fn sha256_hex_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    crate::core::agent_identity::hex_encode(&h.finalize())
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeLayer {
    pub facts: Vec<KnowledgeFact>,
    pub patterns: Vec<ProjectPattern>,
    pub insights: Vec<ConsolidatedInsight>,
    pub exported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphLayer {
    pub nodes: Vec<GraphNodeExport>,
    pub edges: Vec<GraphEdgeExport>,
    pub exported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNodeExport {
    pub kind: String,
    pub name: String,
    pub file_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdgeExport {
    pub source_path: String,
    pub source_name: String,
    pub target_path: String,
    pub target_name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLayer {
    pub task_description: Option<String>,
    pub findings: Vec<SessionFinding>,
    pub decisions: Vec<SessionDecision>,
    pub next_steps: Vec<String>,
    pub files_touched: Vec<String>,
    pub exported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFinding {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDecision {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternsLayer {
    pub patterns: Vec<ProjectPattern>,
    pub exported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GotchasLayer {
    pub gotchas: Vec<GotchaExport>,
    pub exported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GotchaExport {
    pub id: String,
    pub category: String,
    pub severity: String,
    pub trigger: String,
    pub resolution: String,
    #[serde(default)]
    pub file_patterns: Vec<String>,
    pub confidence: f32,
}

impl PackageContent {
    pub fn active_layer_count(&self) -> usize {
        let mut n = 0;
        if self.knowledge.is_some() {
            n += 1;
        }
        if self.graph.is_some() {
            n += 1;
        }
        if self.session.is_some() {
            n += 1;
        }
        if self.patterns.is_some() {
            n += 1;
        }
        if self.gotchas.is_some() {
            n += 1;
        }
        if self.context_graph.is_some() {
            n += 1;
        }
        if self.addon.is_some() {
            n += 1;
        }
        if self.documents.is_some() {
            n += 1;
        }
        n
    }

    pub fn is_empty(&self) -> bool {
        self.active_layer_count() == 0
    }

    pub fn estimated_token_count(&self) -> usize {
        let json = serde_json::to_string(self).unwrap_or_default();
        json.len() / 4
    }
}
