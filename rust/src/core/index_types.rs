//! Core domain types for the index pipeline rewrite.
//!
//! These types are shared across all pipeline modules (GraphBuffer,
//! ThreadPool, extraction, registry, resolution, dump). This module
//! has NO internal dependencies — it is the foundation layer.
//!
//! Compatibility notes:
//! - `Minhash` matches the `[u32; 64]` fingerprint from `minhash::compute_minhash`.
//! - `Definition` maps to C's `CBMDefinition` (~30 fields).
//! - `ExtractedFile` maps to C's `CBMFileResult`.
//! - Existing `bm25_index::CodeChunk` is superseded by the `CodeChunk` here.

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Newtype wrappers
// ---------------------------------------------------------------------------

/// Opaque identifier for a graph node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

/// Opaque identifier for a graph edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EdgeId(pub u32);

// ---------------------------------------------------------------------------
// DefKind — exactly 9 variants
// ---------------------------------------------------------------------------

/// The kind of a definition (symbol).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    Function,
    Method,
    Class,
    Struct,
    Interface,
    Enum,
    Variable,
    Field,
    Module,
}

// ---------------------------------------------------------------------------
// Minhash
// ---------------------------------------------------------------------------

/// Structural fingerprint: 64 × u32 MinHash values.
///
/// Compatible with `minhash::compute_minhash` which returns `Option<[u32; 64]>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Minhash(pub [u32; 64]);

impl Minhash {
    /// Format as a 512-character lowercase hex string (64 × 8 hex digits).
    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(512);
        for &val in &self.0 {
            out.push_str(&format!("{val:08x}"));
        }
        out
    }

    /// Parse a 512-character lowercase hex string back into a `Minhash`.
    ///
    /// Returns `None` when the input is not exactly 512 ASCII hex characters.
    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 512 {
            return None;
        }
        let bytes = s.as_bytes();
        let mut arr = [0u32; 64];
        for (i, slot) in arr.iter_mut().enumerate() {
            let start = i * 8;
            // Safety: we already checked len == 512, so start + 8 is in bounds.
            let chunk = unsafe { bytes.get_unchecked(start..start + 8) };
            let hex = core::str::from_utf8(chunk).ok()?;
            let u = u32::from_str_radix(hex, 16).ok()?;
            *slot = u;
        }
        Some(Minhash(arr))
    }
}

// ---------------------------------------------------------------------------
// Definition  (maps to CBMDefinition in cbm.h:178-217)
// ---------------------------------------------------------------------------

/// A single definition/symbol extracted from source code.
///
/// Field mapping to `CBMDefinition`:
/// - name              → name
/// - qualified_name    → qualified_name
/// - kind              → kind (DefKind)
/// - label             → label string ("Function", "Method", etc.)
/// - file_path         → file_path
/// - start_line        → start_line
/// - end_line          → end_line
/// - signature         → signature (optional)
/// - return_type       → return_type (optional)
/// - receiver          → receiver (optional, e.g. "(self)" in Go)
/// - docstring         → docstring (optional)
/// - parent_class      → parent_class (optional)
/// - decorators        → decorators list
/// - base_classes      → base_classes list
/// - param_names       → param_names list
/// - param_types       → param_types list
/// - is_async          → is_async
/// - is_exported       → is_exported
/// - is_abstract       → is_abstract
/// - is_test           → is_test
/// - is_entry_point    → is_entry_point
/// - complexity        → cyclomatic_complexity
/// - cognitive         → cognitive_complexity
/// - loop_count        → loop_count
/// - loop_depth        → loop_depth
/// - is_recursive      → is_recursive
/// - param_count       → param_count
/// - minhash           → minhash (optional)
/// - body_tokens       → body_tokens (optional, raw token sequence)
#[derive(Debug, Clone)]
pub struct Definition {
    pub name: String,
    pub qualified_name: String,
    pub kind: DefKind,
    pub label: String,
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub signature: Option<String>,
    pub return_type: Option<String>,
    pub receiver: Option<String>,
    pub docstring: Option<String>,
    pub parent_class: Option<String>,
    pub decorators: Vec<String>,
    pub base_classes: Vec<String>,
    pub param_names: Vec<String>,
    pub param_types: Vec<String>,
    pub is_async: bool,
    pub is_exported: bool,
    pub is_abstract: bool,
    pub is_test: bool,
    pub is_entry_point: bool,
    pub complexity: u32,
    pub cognitive: u32,
    pub loop_count: u32,
    pub loop_depth: u32,
    pub is_recursive: bool,
    pub param_count: u32,
    pub minhash: Option<Minhash>,
    pub body_tokens: Option<String>,
}

// ---------------------------------------------------------------------------
// Call, Import, Usage, ThrowEdge, Channel
// ---------------------------------------------------------------------------

/// A call from one function to another.
#[derive(Debug, Clone)]
pub struct Call {
    pub callee_name: String,
    pub enclosing_func_qn: String,
    pub start_line: u32,
    pub arg_count: u32,
    pub args: Vec<String>,
}

/// An import statement.
#[derive(Debug, Clone)]
pub struct Import {
    pub local_name: String,
    pub module_path: String,
}

/// A reference to a name (variable / type) within a function body.
#[derive(Debug, Clone)]
pub struct Usage {
    pub ref_name: String,
    pub enclosing_func_qn: String,
}

/// An exception that a function may throw.
#[derive(Debug, Clone)]
pub struct ThrowEdge {
    pub exception_name: String,
    pub enclosing_func_qn: String,
}

/// A channel send or receive operation.
#[derive(Debug, Clone)]
pub struct Channel {
    pub channel_name: String,
    pub enclosing_func_qn: String,
    pub is_write: bool,
}

// ---------------------------------------------------------------------------
// CodeChunk  (replaces bm25_index::CodeChunk)
// ---------------------------------------------------------------------------

/// A contiguous chunk of code, used for BM25 indexing.
#[derive(Debug, Clone)]
pub struct CodeChunk {
    pub file_path: String,
    pub content: String,
    pub content_hash: String,
    pub start_line: u32,
    pub end_line: u32,
    pub language: String,
}

// ---------------------------------------------------------------------------
// ExtractedFile  (maps to CBMFileResult in cbm.h:420-461)
// ---------------------------------------------------------------------------

/// The complete extraction result for a single file.
///
/// Contains all definitions, calls, imports, usages, throws, channels,
/// and code chunks discovered during extraction.
#[derive(Debug, Clone)]
pub struct ExtractedFile {
    pub file_path: String,
    pub module_qn: Option<String>,
    pub defs: Vec<Definition>,
    pub calls: Vec<Call>,
    pub imports: Vec<Import>,
    pub usages: Vec<Usage>,
    pub throws: Vec<ThrowEdge>,
    pub channels: Vec<Channel>,
    pub chunks: Vec<CodeChunk>,
    pub content_hash: String,
    pub is_test_file: bool,
    pub has_parse_error: bool,
}

// ---------------------------------------------------------------------------
// GbufNode / GbufEdge  (owned by GraphBuffer)
// ---------------------------------------------------------------------------

/// A graph node in the buffer, awaiting commit to the property graph.
#[derive(Debug, Clone)]
pub struct GbufNode {
    pub id: NodeId,
    pub label: String,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub properties: HashMap<String, String>,
}

/// A graph edge in the buffer, awaiting commit to the property graph.
#[derive(Debug, Clone)]
pub struct GbufEdge {
    pub id: EdgeId,
    pub source_id: NodeId,
    pub target_id: NodeId,
    pub edge_type: String,
    pub properties: HashMap<String, String>,
}
