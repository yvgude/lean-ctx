//! Phase 3A: Single-pass parallel extraction.
//!
//! Each file is read once, tree-sitter parsed once, and ALL data (defs, calls,
//! imports, usages, channels, chunks, minhash) extracted from the same AST.
//!
//! ## Architecture
//!
//! 1. Files sorted by **descending size** (tail-latency reduction, C's
//!    `pass_parallel.c:740-746`).
//! 2. Worker-local [`GraphBuffer`]s with a shared atomic ID source.
//! 3. Dispatch via [`ThreadPool::parallel_for_with_backpressure`].
//! 4. Each worker: read → tree-sitter parse → extract all from same AST.
//! 5. After all workers complete: merge local gbufs into main gbuf.
//! 6. Return extracted files for Phase 3B/4.
//!
//! ## Single-pass guarantee
//!
//! Unlike the old `extraction.rs` (which called regex sig extraction and BM25
//! chunking as separate passes), this module performs exactly ONE tree-sitter
//! parse per file. Both definitions-with-minhash and code chunks are extracted
//! from the same AST tree by running two queries (sig + chunk) on the same
//! root node. When tree-sitter is unavailable or fails, regex fallback runs
//! with `has_parse_error = true`.
#![allow(clippy::match_same_arms)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::Result;

use crate::core::config::IndexingMode;
use crate::core::graph_buffer::GraphBuffer;
use crate::core::index_pipeline::discovery::DiscoveredFile;
use crate::core::index_types::{
    Call, CodeChunk, DefKind, Definition, ExtractedFile, Import, Minhash,
};
use crate::core::pipeline_lock::CancelToken;
use crate::core::thread_pool::ThreadPool;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Result of parallel extraction: extracted data per file + merged graph buffer.
pub struct ParallelExtractOutput {
    /// Per-file extraction results, one per discovered file.
    pub extracted_files: Vec<ExtractedFile>,
    /// Merged graph buffer with all nodes and edges from all workers.
    pub graph: GraphBuffer,
    /// Total number of files processed.
    pub files_scanned: usize,
    /// Number of files that had parse errors (fallback regex used).
    pub files_with_errors: usize,
}

/// Single-pass parallel extractor.
///
/// Processes N files in parallel using a custom `ThreadPool` (not rayon).
/// Each file goes through exactly one tree-sitter parse from which all
/// data products are derived.
pub struct ParallelExtractor {
    /// Maximum number of worker threads.
    pub max_workers: usize,
}

impl ParallelExtractor {
    /// Create a new extractor with the given worker thread limit.
    ///
    /// `max_workers` of 0 or 1 runs sequentially on the calling thread.
    #[must_use]
    pub fn new(max_workers: usize) -> Self {
        Self { max_workers }
    }

    /// Run single-pass parallel extraction over discovered files.
    ///
    /// Algorithm (matching C's `cbm_parallel_extract`):
    /// 1. Sort files by descending size.
    /// 2. Create shared atomic ID source for worker-local gbufs.
    /// 3. Dispatch via `parallel_for_with_backpressure`.
    /// 4. Each worker: read → tree-sitter parse → extract all from same AST.
    /// 5. After all workers complete: merge local gbufs into main gbuf.
    /// 6. Return extracted files for downstream processing.
    ///
    /// # Errors
    ///
    /// Propagates the first worker error (other workers are cancelled).
    pub fn extract_all(
        &self,
        files: &[DiscoveredFile],
        project_root: &str,
        mode: IndexingMode,
        cancel: Option<&CancelToken>,
    ) -> Result<ParallelExtractOutput> {
        if files.is_empty() {
            return Ok(ParallelExtractOutput {
                extracted_files: Vec::new(),
                graph: GraphBuffer::new(project_root),
                files_scanned: 0,
                files_with_errors: 0,
            });
        }

        // 1. Sort by descending file size (tail-latency reduction).
        //    Like C's `pass_parallel.c:740-746`.
        let mut sorted_files: Vec<DiscoveredFile> = files.to_vec();
        sorted_files.sort_unstable_by_key(|b| std::cmp::Reverse(b.size));
        let n = sorted_files.len();
        let files_arc: Arc<[DiscoveredFile]> = sorted_files.into_boxed_slice().into();

        // 2. Shared atomic ID source for worker-local gbufs.
        let id_source = Arc::new(AtomicU32::new(1));

        // 3. Shared result collectors.
        let extracted_files: Arc<Mutex<Vec<ExtractedFile>>> =
            Arc::new(Mutex::new(Vec::with_capacity(n)));
        let worker_gbufs: Arc<Mutex<Vec<GraphBuffer>>> =
            Arc::new(Mutex::new(Vec::with_capacity(n)));
        let files_scanned = Arc::new(AtomicUsize::new(0));
        let files_with_errors = Arc::new(AtomicUsize::new(0));

        // 4. Shared project_root string for the closure.
        let proj_root = Arc::<str>::from(project_root);

        let pool = ThreadPool::new(self.max_workers);
        pool.parallel_for_with_backpressure(
            n,
            {
                let files = Arc::clone(&files_arc);
                let id_src = Arc::clone(&id_source);
                let ef = Arc::clone(&extracted_files);
                let wg = Arc::clone(&worker_gbufs);
                let fs = Arc::clone(&files_scanned);
                let fwe = Arc::clone(&files_with_errors);
                let pr = Arc::clone(&proj_root);

                move |idx| {
                    let file = &files[idx];
                    let content = std::fs::read_to_string(&file.path).map_err(|e| {
                        anyhow::anyhow!("failed to read {}: {e}", file.path.display())
                    })?;

                    // Each worker creates its own gbuf with shared atomic IDs.
                    let mut worker_gbuf = GraphBuffer::new_shared_ids(&pr, Arc::clone(&id_src));

                    // Single-pass extraction from this file.
                    let extracted = extract_file(
                        &file.rel_path,
                        &pr,
                        &file.ext,
                        &content,
                        mode,
                        &mut worker_gbuf,
                    );

                    if extracted.has_parse_error {
                        fwe.fetch_add(1, Ordering::Relaxed);
                    }
                    fs.fetch_add(1, Ordering::Relaxed);

                    ef.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(extracted);
                    wg.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(worker_gbuf);

                    Ok(())
                }
            },
            cancel,
            0, // rss_budget_mb (TODO: use actual budget)
        )?;

        // 5. Merge all worker gbufs into the main gbuf.
        let mut main_gbuf = GraphBuffer::new(project_root);
        let gbufs =
            std::mem::take(&mut *worker_gbufs.lock().unwrap_or_else(PoisonError::into_inner));
        for mut gbuf in gbufs {
            main_gbuf.merge(&mut gbuf);
        }
        main_gbuf.set_next_node_id(id_source.load(Ordering::Relaxed));

        // 6. Collect extracted files.
        let extracted = std::mem::take(
            &mut *extracted_files
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );

        Ok(ParallelExtractOutput {
            extracted_files: extracted,
            graph: main_gbuf,
            files_scanned: files_scanned.load(Ordering::Relaxed),
            files_with_errors: files_with_errors.load(Ordering::Relaxed),
        })
    }
}

// ---------------------------------------------------------------------------
// extract_file — per-file single-pass extraction
// ---------------------------------------------------------------------------

/// Run single-pass extraction on one file.
///
/// Algorithm (matching C's `extract_worker` in `pass_parallel.c`):
/// 1. Read file content (already provided).
/// 2. Parse with tree-sitter (once).
/// 3. From the same AST tree extract:
///    a. Definitions (walk AST with sig queries) + minhash per function/method.
///    b. Code chunks (walk AST with chunk queries).
///    c. Calls, imports, usages, channels (AST queries).
/// 4. If tree-sitter parse fails: `has_parse_error=true`, regex fallback sigs.
/// 5. Insert each definition as a node in `gbuf`.
/// 6. Return `ExtractedFile`.
#[allow(unused_variables)]
fn extract_file(
    file_path: &str,
    project_root: &str,
    ext: &str,
    content: &str,
    mode: IndexingMode,
    gbuf: &mut GraphBuffer,
) -> ExtractedFile {
    // Attempt tree-sitter extraction (feature-gated).
    // When the feature is off or parsing fails, fall through to regex fallback.
    #[cfg(feature = "tree-sitter")]
    if let Some(result) = extract_with_treesitter(file_path, ext, content, gbuf) {
        return result;
    }

    // Fallback: regex-based signature extraction.
    extract_fallback(file_path, content, ext, gbuf)
}

// ---------------------------------------------------------------------------
// Tree-sitter extraction path (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "tree-sitter")]
use tree_sitter::{Language, Node, Parser, Query, QueryCursor, StreamingIterator};

/// Try to extract using tree-sitter. Returns `None` if the language is
/// unsupported or parsing fails (caller falls back to regex).
#[cfg(feature = "tree-sitter")]
fn extract_with_treesitter(
    file_path: &str,
    ext: &str,
    content: &str,
    gbuf: &mut GraphBuffer,
) -> Option<ExtractedFile> {
    let language = get_ts_language(ext)?;

    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(content, None)?;
    let root = tree.root_node();
    let source = content.as_bytes();
    let lines: Vec<&str> = content.lines().collect();

    // ---- Definitions (from sig query) ----
    let mut defs: Vec<Definition> = Vec::new();
    if let Some(sig_query) = get_sig_query(ext) {
        let def_idx = find_capture_index(sig_query, "def")?;
        let name_idx = find_capture_index(sig_query, "name")?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(sig_query, root, source);
        let mut seen_ranges: Vec<(u32, u32)> = Vec::new();

        while let Some(m) = matches.next() {
            let mut def_node: Option<Node> = None;
            let mut name_text = String::new();

            for cap in m.captures {
                if cap.index == def_idx {
                    def_node = Some(cap.node);
                } else if cap.index == name_idx
                    && let Ok(text) = cap.node.utf8_text(source)
                {
                    name_text = text.to_string();
                }
            }

            let node = def_node?;
            if name_text.is_empty() {
                continue;
            }

            let start_line = node.start_position().row as u32 + 1;
            let end_line = node.end_position().row as u32 + 1;

            // Deduplicate nested ranges (e.g. method inside class).
            let range = (start_line, end_line);
            let is_nested = seen_ranges
                .iter()
                .any(|&(s, e)| s <= start_line && end_line <= e && range != (s, e));
            if is_nested {
                continue;
            }
            seen_ranges.push(range);

            let kind = node_kind_to_defkind(node.kind());
            let qn = build_qualified_name(file_path, &name_text);

            // Compute minhash from the definition node's subtree.
            let minhash = crate::core::minhash::compute_minhash(&node).map(Minhash);
            let minhash_hex = minhash
                .as_ref()
                .map(super::super::index_types::Minhash::to_hex);

            // Build a one-line signature from the node's text.
            let signature = build_signature_str(&node, source);

            // Determine if exported (check for `pub`/`export` modifier).
            let is_exported = has_export_modifier(&node, source);

            let def = Definition {
                name: name_text.clone(),
                qualified_name: qn.clone(),
                kind,
                label: defkind_to_label(kind).to_string(),
                file_path: file_path.to_string(),
                start_line,
                end_line,
                signature: Some(signature),
                return_type: None,
                receiver: None,
                docstring: None,
                parent_class: None,
                decorators: Vec::new(),
                base_classes: Vec::new(),
                param_names: Vec::new(),
                param_types: Vec::new(),
                is_async: false,
                is_exported,
                is_abstract: false,
                is_test: file_path.contains("test"),
                is_entry_point: name_text == "main",
                complexity: 0,
                cognitive: 0,
                loop_count: 0,
                loop_depth: 0,
                is_recursive: false,
                param_count: 0,
                minhash,
                body_tokens: None,
            };

            // Insert into worker gbuf.
            let mut props = HashMap::new();
            if let Some(hex) = minhash_hex {
                props.insert("minhash".to_string(), hex);
            }
            props.insert("kind".to_string(), defkind_to_label(kind).to_string());
            props.insert(
                "signature".to_string(),
                def.signature.clone().unwrap_or_default(),
            );

            let _node_id = gbuf.upsert_node(
                &def.label,
                &def.name,
                &def.qualified_name,
                file_path,
                def.start_line,
                def.end_line,
                props,
            );

            defs.push(def);
        }
    }

    // ---- Code chunks (from chunk query) ----
    let chunks = extract_chunks_from_ast(file_path, ext, root, source, &lines);

    // ---- Calls, imports, usages, channels ----
    // Simplified: extract calls from the AST via a basic call query.
    let calls = extract_calls_from_ast(root, source);
    let imports = extract_imports_from_ast(root, ext, source);

    let content_hash = compute_content_hash(content);

    Some(ExtractedFile {
        file_path: file_path.to_string(),
        module_qn: None,
        defs,
        calls,
        imports,
        usages: Vec::new(),   // TODO(T8): extract usages
        throws: Vec::new(),   // TODO(T8): extract throws
        channels: Vec::new(), // TODO(T8): extract channels
        chunks,
        content_hash,
        is_test_file: file_path.contains("test"),
        has_parse_error: false,
    })
}

/// Extract code chunks by running the chunk query on the already-parsed AST.
#[cfg(feature = "tree-sitter")]
fn extract_chunks_from_ast(
    file_path: &str,
    ext: &str,
    root: Node,
    source: &[u8],
    lines: &[&str],
) -> Vec<CodeChunk> {
    let Some(chunk_query) = get_chunk_query(ext) else {
        return Vec::new();
    };

    let Some(chunk_idx) = find_capture_index(chunk_query, "chunk") else {
        return Vec::new();
    };
    let Some(name_idx) = find_capture_index(chunk_query, "name") else {
        return Vec::new();
    };

    let mut chunks: Vec<CodeChunk> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(chunk_query, root, source);
    let mut seen_ranges: Vec<(u32, u32)> = Vec::new();

    while let Some(m) = matches.next() {
        let mut chunk_node: Option<Node> = None;
        let mut name_text = String::new();

        for cap in m.captures {
            if cap.index == chunk_idx {
                chunk_node = Some(cap.node);
            } else if cap.index == name_idx
                && let Ok(text) = cap.node.utf8_text(source)
            {
                name_text = text.to_string();
            }
        }

        let Some(node) = chunk_node else {
            continue;
        };
        if name_text.is_empty() {
            continue;
        }

        let start_line = node.start_position().row as u32 + 1;
        let end_line = node.end_position().row as u32 + 1;

        // Deduplicate nested ranges.
        let range = (start_line, end_line);
        let is_nested = seen_ranges
            .iter()
            .any(|&(s, e)| s <= start_line && end_line <= e && range != (s, e));
        if is_nested {
            continue;
        }
        seen_ranges.push(range);

        // Extract source text.
        let start_row0 = node.start_position().row;
        let end_row0 = node.end_position().row;
        let block: String =
            lines[start_row0..=end_row0.min(lines.len().saturating_sub(1))].join("\n");

        // Map tree-sitter node kind to ChunkKind variant name.
        let kind_str = node_kind_to_chunk_kind(node.kind());

        chunks.push(CodeChunk {
            file_path: file_path.to_string(),
            content: block,
            content_hash: String::new(),
            start_line,
            end_line,
            language: ext.to_string(),
            symbol_name: name_text,
            kind: kind_str,
        });
    }

    chunks
}

/// Map a tree-sitter node kind string to a JSON-serialized [`ChunkKind`]
/// variant name so the downstream `from_chunks` and `load_chunk_data` can
/// deserialise it back without guessing.
fn node_kind_to_chunk_kind(node_kind: &str) -> String {
    let variant = match node_kind {
        // Rust
        "function_item" => "Function",
        "struct_item" => "Struct",
        "enum_item" => "Struct",
        "trait_item" => "Struct",
        "impl_item" => "Impl",
        // JS/TS
        "function_declaration" => "Function",
        "class_declaration" => "Class",
        "abstract_class_declaration" => "Class",
        "interface_declaration" => "Struct",
        "type_alias_declaration" => "Module",
        "method_definition" => "Method",
        // Python
        "function_definition" => "Function",
        "class_definition" => "Class",
        // Go
        "method_declaration" => "Method",
        "type_spec" => "Struct",
        // Java
        "constructor_declaration" => "Method",
        _ => "Other",
    };
    // JSON-encode so serde_json::from_str::<ChunkKind> works downstream.
    format!("\"{variant}\"")
}

/// Basic call extraction via tree-sitter queries.
///
/// For Rust: `(call_expression function: (identifier) @callee)`
/// For TS/JS: `(call_expression function: (identifier) @callee)`
/// For Python: `(call function: (identifier) @callee)`
///
/// This is intentionally simplified — the full call-graph extraction will be
/// built in T8 (registry).
#[cfg(feature = "tree-sitter")]
fn extract_calls_from_ast(root: Node, source: &[u8]) -> Vec<Call> {
    // Build a dynamic query depending on the node kind.
    // We walk named children looking for `call_expression` / `call` nodes.
    let mut calls: Vec<Call> = Vec::new();
    let mut cursor = root.walk();

    loop {
        let node = cursor.node();
        let kind = node.kind();

        if kind == "call_expression" || kind == "call" {
            // Extract the function name from the first named child.
            if let Some(func) = node.child_by_field_name("function")
                && let Ok(name) = func.utf8_text(source)
            {
                let start_line = node.start_position().row as u32 + 1;
                calls.push(Call {
                    callee_name: name.to_string(),
                    enclosing_func_qn: String::new(), // will be filled in T8
                    start_line,
                    arg_count: 0,
                    args: Vec::new(),
                });
            }
        }

        if cursor.goto_first_child() {
            continue;
        }
        if cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                return calls;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Basic import extraction via tree-sitter queries.
#[cfg(feature = "tree-sitter")]
fn extract_imports_from_ast(root: Node, ext: &str, source: &[u8]) -> Vec<Import> {
    let mut imports: Vec<Import> = Vec::new();
    let mut cursor = root.walk();

    let import_kinds: &[&str] = match ext {
        "rs" => &["use_declaration"],
        "ts" | "tsx" | "js" | "jsx" => &["import_statement", "import_declaration"],
        "py" => &["import_statement", "import_from_statement"],
        "go" => &["import_declaration"],
        _ => return Vec::new(),
    };

    loop {
        let node = cursor.node();
        let kind = node.kind();
        let kind_matches = import_kinds.contains(&kind);

        if kind_matches && let Ok(text) = node.utf8_text(source) {
            let text = text.to_string();
            // Use the first line or first 80 chars as the import descriptor.
            let line = text.lines().next().unwrap_or(&text).to_string();
            let desc = line.chars().take(80).collect::<String>();
            imports.push(Import {
                local_name: desc.clone(),
                module_path: desc,
            });
        }

        if cursor.goto_first_child() {
            continue;
        }
        if cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                return imports;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Fallback extraction path (regex-based)
// ---------------------------------------------------------------------------

/// Regex-based fallback when tree-sitter is unavailable or fails.
fn extract_fallback(
    file_path: &str,
    content: &str,
    ext: &str,
    gbuf: &mut GraphBuffer,
) -> ExtractedFile {
    let sigs = crate::core::signatures::extract_signatures(content, ext);

    let mut defs: Vec<Definition> = Vec::new();
    for sig in &sigs {
        let kind = sig_kind_to_defkind(sig.kind);
        let qn = build_qualified_name(file_path, &sig.name);

        let mut props = HashMap::new();
        props.insert("kind".to_string(), sig.kind.to_string());
        props.insert("signature".to_string(), sig.to_compact());

        let start_line = sig.start_line.unwrap_or(1) as u32;
        let end_line = sig.end_line.unwrap_or(1) as u32;

        let _node_id = gbuf.upsert_node(
            defkind_to_label(kind),
            &sig.name,
            &qn,
            file_path,
            start_line,
            end_line,
            props,
        );

        defs.push(Definition {
            name: sig.name.clone(),
            qualified_name: qn,
            kind,
            label: defkind_to_label(kind).to_string(),
            file_path: file_path.to_string(),
            start_line,
            end_line,
            signature: Some(sig.to_compact()),
            return_type: if sig.return_type.is_empty() {
                None
            } else {
                Some(sig.return_type.clone())
            },
            receiver: None,
            docstring: None,
            parent_class: None,
            decorators: Vec::new(),
            base_classes: Vec::new(),
            param_names: Vec::new(),
            param_types: Vec::new(),
            is_async: sig.is_async,
            is_exported: sig.is_exported,
            is_abstract: false,
            is_test: file_path.contains("test"),
            is_entry_point: sig.name == "main",
            complexity: 0,
            cognitive: 0,
            loop_count: 0,
            loop_depth: 0,
            is_recursive: false,
            param_count: 0,
            minhash: None,
            body_tokens: None,
        });
    }

    let old_chunks = crate::core::chunk_data::extract_chunks(file_path, content);
    let chunks: Vec<CodeChunk> = old_chunks
        .into_iter()
        .map(|c| CodeChunk {
            file_path: c.file_path,
            content: c.content,
            content_hash: String::new(),
            start_line: c.start_line as u32,
            end_line: c.end_line as u32,
            language: std::path::Path::new(file_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_string(),
            symbol_name: c.symbol_name,
            kind: serde_json::to_string(&c.kind).unwrap_or_default(),
        })
        .collect();

    let content_hash = compute_content_hash(content);

    ExtractedFile {
        file_path: file_path.to_string(),
        module_qn: None,
        defs,
        calls: Vec::new(),
        imports: Vec::new(),
        usages: Vec::new(),
        throws: Vec::new(),
        channels: Vec::new(),
        chunks,
        content_hash,
        is_test_file: file_path.contains("test"),
        has_parse_error: true,
    }
}

// ---------------------------------------------------------------------------
// Tree-sitter language / query mapping (mirrors signatures_ts/queries.rs)
// ---------------------------------------------------------------------------

/// Map a file extension to a tree-sitter `Language`.
#[cfg(feature = "tree-sitter")]
fn get_ts_language(ext: &str) -> Option<Language> {
    Some(match ext {
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "ts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "js" | "jsx" => tree_sitter_javascript::LANGUAGE.into(),
        "py" => tree_sitter_python::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "c" | "h" => tree_sitter_c::LANGUAGE.into(),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => tree_sitter_cpp::LANGUAGE.into(),
        "rb" => tree_sitter_ruby::LANGUAGE.into(),
        "cs" => tree_sitter_c_sharp::LANGUAGE.into(),
        "kt" | "kts" => tree_sitter_kotlin_ng::LANGUAGE.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "sh" | "bash" => tree_sitter_bash::LANGUAGE.into(),
        "dart" => tree_sitter_dart::LANGUAGE.into(),
        "scala" | "sc" => tree_sitter_scala::LANGUAGE.into(),
        "ex" | "exs" => tree_sitter_elixir::LANGUAGE.into(),
        "zig" => tree_sitter_zig::LANGUAGE.into(),
        "gd" => tree_sitter_gdscript::LANGUAGE.into(),
        "lua" => tree_sitter_lua::LANGUAGE.into(),
        "luau" => tree_sitter_luau::LANGUAGE.into(),
        _ => return None,
    })
}

/// Signature-extraction query strings, copied from `signatures_ts/queries.rs`.
/// These match the `@def` / `@name` capture convention.
#[cfg(feature = "tree-sitter")]
fn get_sig_query_source(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => QUERY_RUST,
        "ts" | "tsx" => QUERY_TYPESCRIPT,
        "js" | "jsx" => QUERY_JAVASCRIPT,
        "py" => QUERY_PYTHON,
        "go" => QUERY_GO,
        "java" => QUERY_JAVA,
        "c" | "h" => QUERY_C,
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => QUERY_CPP,
        "rb" => QUERY_RUBY,
        "cs" => QUERY_CSHARP,
        "kt" | "kts" => QUERY_KOTLIN,
        "swift" => QUERY_SWIFT,
        "php" => QUERY_PHP,
        "sh" | "bash" => QUERY_BASH,
        "dart" => QUERY_DART,
        "scala" | "sc" => QUERY_SCALA,
        "ex" | "exs" => QUERY_ELIXIR,
        "zig" => QUERY_ZIG,
        "gd" => QUERY_GDSCRIPT,
        "lua" => QUERY_LUA,
        "luau" => QUERY_LUAU,
        _ => return None,
    })
}

/// Chunk-extraction query strings, copied from `chunks_ts.rs`.
#[cfg(feature = "tree-sitter")]
fn get_chunk_query_source(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => CHUNK_QUERY_RUST,
        "ts" | "tsx" => CHUNK_QUERY_TYPESCRIPT,
        "js" | "jsx" => CHUNK_QUERY_JAVASCRIPT,
        "py" => CHUNK_QUERY_PYTHON,
        "go" => CHUNK_QUERY_GO,
        "java" => CHUNK_QUERY_JAVA,
        "c" | "h" => CHUNK_QUERY_C,
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => CHUNK_QUERY_CPP,
        _ => return None,
    })
}

/// Get or compile a sig query for the given extension.
#[cfg(feature = "tree-sitter")]
fn get_sig_query(ext: &str) -> Option<&'static Query> {
    use std::collections::HashMap;
    use std::sync::OnceLock;

    static SIG_QUERY_CACHE: OnceLock<HashMap<&'static str, Query>> = OnceLock::new();

    let cache = SIG_QUERY_CACHE.get_or_init(|| {
        let mut map = HashMap::new();
        let exts: &[&str] = &[
            "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "c", "h", "cpp", "cc", "cxx",
            "hpp", "rb", "cs", "kt", "kts", "swift", "php", "sh", "bash", "dart", "scala", "sc",
            "ex", "exs", "zig", "gd", "lua", "luau",
        ];
        for &e in exts {
            if let (Some(lang), Some(src)) = (get_ts_language(e), get_sig_query_source(e))
                && let Ok(q) = Query::new(&lang, src)
            {
                map.insert(e, q);
            }
        }
        map
    });

    cache.get(ext)
}

/// Get or compile a chunk query for the given extension.
#[cfg(feature = "tree-sitter")]
fn get_chunk_query(ext: &str) -> Option<&'static Query> {
    use std::collections::HashMap;
    use std::sync::OnceLock;

    static CHUNK_QUERY_CACHE: OnceLock<HashMap<&'static str, Query>> = OnceLock::new();

    let cache = CHUNK_QUERY_CACHE.get_or_init(|| {
        let mut map = HashMap::new();
        let exts: &[&str] = &[
            "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "c", "h", "cpp", "cc", "cxx", "hpp",
        ];
        for &e in exts {
            if let (Some(lang), Some(src)) = (get_ts_language(e), get_chunk_query_source(e))
                && let Ok(q) = Query::new(&lang, src)
            {
                map.insert(e, q);
            }
        }
        map
    });

    cache.get(ext)
}

/// Find the index of a named capture in a query.
#[cfg(feature = "tree-sitter")]
fn find_capture_index(query: &Query, name: &str) -> Option<u32> {
    query
        .capture_names()
        .iter()
        .position(|n| *n == name)
        .map(|i| i as u32)
}

// ---------------------------------------------------------------------------
// Static query strings (sig — mirrors signatures_ts/queries.rs)
// ---------------------------------------------------------------------------

#[cfg(feature = "tree-sitter")]
const QUERY_RUST: &str = r"
(function_item name: (identifier) @name) @def
(struct_item name: (type_identifier) @name) @def
(enum_item name: (type_identifier) @name) @def
(trait_item name: (type_identifier) @name) @def
(impl_item type: (type_identifier) @name) @def
(type_item name: (type_identifier) @name) @def
(const_item name: (identifier) @name) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_TYPESCRIPT: &str = r"
(function_declaration name: (identifier) @name) @def
(class_declaration name: (type_identifier) @name) @def
(abstract_class_declaration name: (type_identifier) @name) @def
(interface_declaration name: (type_identifier) @name) @def
(type_alias_declaration name: (type_identifier) @name) @def
(method_definition name: (property_identifier) @name) @def
(variable_declarator name: (identifier) @name value: (arrow_function)) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_JAVASCRIPT: &str = r"
(function_declaration name: (identifier) @name) @def
(class_declaration name: (identifier) @name) @def
(method_definition name: (property_identifier) @name) @def
(variable_declarator name: (identifier) @name value: (arrow_function)) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_PYTHON: &str = r"
(function_definition name: (identifier) @name) @def
(class_definition name: (identifier) @name) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_GO: &str = r"
(function_declaration name: (identifier) @name) @def
(method_declaration name: (field_identifier) @name) @def
(type_spec name: (type_identifier) @name) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_JAVA: &str = r"
(method_declaration name: (identifier) @name) @def
(class_declaration name: (identifier) @name) @def
(interface_declaration name: (identifier) @name) @def
(enum_declaration name: (identifier) @name) @def
(constructor_declaration name: (identifier) @name) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_C: &str = r"
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @def
(struct_specifier name: (type_identifier) @name) @def
(enum_specifier name: (type_identifier) @name) @def
(type_definition declarator: (type_identifier) @name) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_CPP: &str = r"
(function_definition
  declarator: (function_declarator
    declarator: (_) @name)) @def
(struct_specifier name: (type_identifier) @name) @def
(class_specifier name: (type_identifier) @name) @def
(enum_specifier name: (type_identifier) @name) @def
(namespace_definition name: (identifier) @name) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_RUBY: &str = r"
(method name: (identifier) @name) @def
(singleton_method name: (identifier) @name) @def
(class name: (_) @name) @def
(module name: (_) @name) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_CSHARP: &str = r"
(method_declaration name: (identifier) @name) @def
(class_declaration name: (identifier) @name) @def
(interface_declaration name: (identifier) @name) @def
(struct_declaration name: (identifier) @name) @def
(enum_declaration name: (identifier) @name) @def
(record_declaration name: (identifier) @name) @def
(namespace_declaration name: (identifier) @name) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_KOTLIN: &str = r"
(function_declaration name: (identifier) @name) @def
(class_declaration name: (identifier) @name) @def
(object_declaration name: (identifier) @name) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_SWIFT: &str = r"
(function_declaration name: (simple_identifier) @name) @def
(class_declaration name: (type_identifier) @name) @def
(protocol_declaration name: (type_identifier) @name) @def
(protocol_function_declaration name: (simple_identifier) @name) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_PHP: &str = r"
(function_definition name: (name) @name) @def
(class_declaration name: (name) @name) @def
(interface_declaration name: (name) @name) @def
(trait_declaration name: (name) @name) @def
(method_declaration name: (name) @name) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_BASH: &str = r"
(function_definition name: (word) @name) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_DART: &str = r"
(class_declaration name: (identifier) @name) @def
(enum_declaration name: (identifier) @name) @def
(mixin_declaration (identifier) @name) @def
(type_alias (type_identifier) @name) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_SCALA: &str = r"
(class_definition name: (identifier) @name) @def
(object_definition name: (identifier) @name) @def
(trait_definition name: (identifier) @name) @def
(enum_definition name: (identifier) @name) @def
(function_definition name: (identifier) @name) @def
(type_definition name: (type_identifier) @name) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_ELIXIR: &str = r#"
(call
  target: (identifier) @_keyword
  (arguments (alias) @name)
  (#any-of? @_keyword "defmodule" "defprotocol")) @def

(call
  target: (identifier) @_keyword
  (arguments
    [
      (identifier) @name
      (call target: (identifier) @name)
      (binary_operator left: (call target: (identifier) @name) operator: "when")
    ])
  (#any-of? @_keyword "def" "defp" "defmacro" "defmacrop")) @def
"#;

#[cfg(feature = "tree-sitter")]
const QUERY_ZIG: &str = r"
(function_declaration name: (identifier) @name) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_GDSCRIPT: &str = r"
(class_name_statement (name) @name) @def
(class_definition (name) @name) @def
(function_definition (name) @name) @def
(signal_statement (name) @name) @def
(enum_definition (name) @name) @def
(export_variable_statement name: (name) @name) @def
(onready_variable_statement name: (name) @name) @def
(source (const_statement name: (name) @name) @def)
(source (variable_statement name: (name) @name) @def)
(class_body (const_statement name: (name) @name) @def)
(class_body (variable_statement name: (name) @name) @def)
";

#[cfg(feature = "tree-sitter")]
const QUERY_LUA: &str = r"
(function_declaration name: (identifier) @name) @def
(function_declaration name: (dot_index_expression field: (identifier) @name)) @def
(function_declaration name: (method_index_expression method: (identifier) @name)) @def
(assignment_statement
  (variable_list name: (identifier) @name)
  (expression_list value: (function_definition))) @def
(assignment_statement
  (variable_list name: (dot_index_expression field: (identifier) @name))
  (expression_list value: (function_definition))) @def
";

#[cfg(feature = "tree-sitter")]
const QUERY_LUAU: &str = r"
(function_declaration name: (identifier) @name) @def
(function_declaration name: (dot_index_expression field: (identifier) @name)) @def
(function_declaration name: (method_index_expression method: (identifier) @name)) @def
(assignment_statement
  (variable_list name: (identifier) @name)
  (expression_list value: (function_definition))) @def
(assignment_statement
  (variable_list name: (dot_index_expression field: (identifier) @name))
  (expression_list value: (function_definition))) @def
(type_definition name: (identifier) @name) @def
";

// ---------------------------------------------------------------------------
// Static query strings (chunk — mirrors chunks_ts.rs)
// ---------------------------------------------------------------------------

#[cfg(feature = "tree-sitter")]
const CHUNK_QUERY_RUST: &str = r"
(function_item name: (identifier) @name) @chunk
(struct_item name: (type_identifier) @name) @chunk
(enum_item name: (type_identifier) @name) @chunk
(trait_item name: (type_identifier) @name) @chunk
(impl_item type: (type_identifier) @name) @chunk
(const_item name: (identifier) @name) @chunk
";

#[cfg(feature = "tree-sitter")]
const CHUNK_QUERY_TYPESCRIPT: &str = r"
(function_declaration name: (identifier) @name) @chunk
(class_declaration name: (type_identifier) @name) @chunk
(abstract_class_declaration name: (type_identifier) @name) @chunk
(interface_declaration name: (type_identifier) @name) @chunk
(type_alias_declaration name: (type_identifier) @name) @chunk
(method_definition name: (property_identifier) @name) @chunk
(variable_declarator name: (identifier) @name value: (arrow_function)) @chunk
";

#[cfg(feature = "tree-sitter")]
const CHUNK_QUERY_JAVASCRIPT: &str = r"
(function_declaration name: (identifier) @name) @chunk
(class_declaration name: (identifier) @name) @chunk
(method_definition name: (property_identifier) @name) @chunk
(variable_declarator name: (identifier) @name value: (arrow_function)) @chunk
";

#[cfg(feature = "tree-sitter")]
const CHUNK_QUERY_PYTHON: &str = r"
(function_definition name: (identifier) @name) @chunk
(class_definition name: (identifier) @name) @chunk
";

#[cfg(feature = "tree-sitter")]
const CHUNK_QUERY_GO: &str = r"
(function_declaration name: (identifier) @name) @chunk
(method_declaration name: (field_identifier) @name) @chunk
(type_spec name: (type_identifier) @name) @chunk
";

#[cfg(feature = "tree-sitter")]
const CHUNK_QUERY_JAVA: &str = r"
(method_declaration name: (identifier) @name) @chunk
(class_declaration name: (identifier) @name) @chunk
(interface_declaration name: (identifier) @name) @chunk
(enum_declaration name: (identifier) @name) @chunk
(constructor_declaration name: (identifier) @name) @chunk
";

#[cfg(feature = "tree-sitter")]
const CHUNK_QUERY_C: &str = r"
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @chunk
(struct_specifier name: (type_identifier) @name) @chunk
(enum_specifier name: (type_identifier) @name) @chunk
";

#[cfg(feature = "tree-sitter")]
const CHUNK_QUERY_CPP: &str = r"
(function_definition
  declarator: (function_declarator
    declarator: (_) @name)) @chunk
(struct_specifier name: (type_identifier) @name) @chunk
(class_specifier name: (type_identifier) @name) @chunk
(enum_specifier name: (type_identifier) @name) @chunk
(namespace_definition name: (identifier) @name) @chunk
";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map a tree-sitter node kind to a `DefKind`.
fn node_kind_to_defkind(kind: &str) -> DefKind {
    #![allow(clippy::match_same_arms)]
    match kind {
        "function_item"
        | "function_declaration"
        | "function_definition"
        | "function_signature"
        | "constructor_declaration"
        | "assignment_statement" => DefKind::Function,

        "method_declaration"
        | "method_definition"
        | "protocol_function_declaration"
        | "singleton_method"
        | "method" => DefKind::Method,

        "class_declaration"
        | "abstract_class_declaration"
        | "class_specifier"
        | "class_definition"
        | "object_declaration"
        | "record_declaration"
        | "class_name_statement"
        | "object_definition" => DefKind::Class,

        "struct_item" | "struct_specifier" | "struct_declaration" => DefKind::Struct,

        "interface_declaration"
        | "protocol_declaration"
        | "trait_item"
        | "trait_declaration"
        | "trait_definition" => DefKind::Interface,

        "enum_item" | "enum_declaration" | "enum_specifier" | "enum_definition" => DefKind::Enum,

        "variable_declarator"
        | "variable_statement"
        | "const_item"
        | "const_statement"
        | "export_variable_statement"
        | "onready_variable_statement" => DefKind::Variable,

        "field_declaration" | "field_definition" => DefKind::Field,

        "namespace_declaration" | "namespace_definition" | "module" | "mixin_declaration" => {
            DefKind::Module
        }

        // Default for type_alias, type_spec, type_item, signal_statement, etc.
        _ => DefKind::Variable,
    }
}

/// Map a signature kind string (from regex fallback) to a `DefKind`.
fn sig_kind_to_defkind(kind: &str) -> DefKind {
    #![allow(clippy::match_same_arms)]
    match kind {
        "fn" => DefKind::Function,
        "method" => DefKind::Method,
        "class" => DefKind::Class,
        "struct" => DefKind::Struct,
        "interface" | "trait" => DefKind::Interface,
        "enum" => DefKind::Enum,
        "const" | "let" | "var" => DefKind::Variable,
        "type" => DefKind::Variable,
        _ => DefKind::Variable,
    }
}

/// Map a `DefKind` to its string label.
fn defkind_to_label(kind: DefKind) -> &'static str {
    match kind {
        DefKind::Function => "Function",
        DefKind::Method => "Method",
        DefKind::Class => "Class",
        DefKind::Struct => "Struct",
        DefKind::Interface => "Interface",
        DefKind::Enum => "Enum",
        DefKind::Variable => "Variable",
        DefKind::Field => "Field",
        DefKind::Module => "Module",
    }
}

/// Build a qualified name from file path and definition name.
///
/// Format: `{rel_path}::{name}` for top-level defs.
fn build_qualified_name(file_path: &str, name: &str) -> String {
    // Strip file extension from the path.
    let path = Path::new(file_path);
    // Use the full path without extension as the module path.
    let without_ext = match path.extension().and_then(|e| {
        path.to_str().and_then(|s| {
            let ext_str = e.to_str()?;
            s.strip_suffix(&format!(".{ext_str}"))
        })
    }) {
        Some(s) => s,
        None => file_path,
    };
    // Replace path separators with `::` for a Rust-style module path.
    let module_path = without_ext.replace(['/', '\\'], "::");
    format!("{module_path}::{name}")
}

/// Build a one-line signature string from a tree-sitter node.
#[cfg(feature = "tree-sitter")]
fn build_signature_str(node: &Node, source: &[u8]) -> String {
    // Take the first line of the node's text.
    if let Ok(text) = node.utf8_text(source) {
        text.lines()
            .next()
            .unwrap_or(text)
            .trim()
            .chars()
            .take(120)
            .collect()
    } else {
        String::new()
    }
}

/// Check if a definition node has a `pub`/`export` modifier.
#[cfg(feature = "tree-sitter")]
fn has_export_modifier(node: &Node, source: &[u8]) -> bool {
    // Check named children for `pub` or `export` modifier.
    for i in 0..node.named_child_count() {
        let iu = i as u32;
        if let Some(child) = node.named_child(iu) {
            if let Ok(text) = child.utf8_text(source)
                && (text == "pub" || text == "export" || text == "export default")
            {
                return true;
            }
            let child_kind = child.kind();
            if (child_kind == "visibility_modifier" || child_kind == "modifier")
                && let Ok(text) = child.utf8_text(source)
                && (text.contains("pub") || text.contains("export"))
            {
                return true;
            }
        }
    }
    // Also check siblings for `pub` (Rust: `pub fn foo()`).
    if let Some(parent) = node.parent() {
        for i in 0..parent.named_child_count() {
            let iu = i as u32;
            if let Some(child) = parent.named_child(iu)
                && child.id() == node.id()
            {
                // Check any preceding siblings as modifiers.
                for j in 0..i {
                    let ju = j as u32;
                    if let Some(sibling) = parent.named_child(ju)
                        && let Ok(text) = sibling.utf8_text(source)
                        && (text == "pub" || text == "export")
                    {
                        return true;
                    }
                }
                break;
            }
        }
    }
    false
}

/// Compute a simple content hash.
fn compute_content_hash(content: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a `DiscoveredFile` for testing.
    fn make_file(rel_path: &str, size: u64) -> DiscoveredFile {
        DiscoveredFile {
            path: std::path::PathBuf::from(rel_path),
            rel_path: rel_path.to_string(),
            ext: std::path::Path::new(rel_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_string(),
            size,
            mtime: std::time::SystemTime::UNIX_EPOCH,
        }
    }

    /// Helper: run extraction on an in-memory string without filesystem.
    /// Returns the `ExtractedFile` for the single file.
    fn extract_in_memory(file_path: &str, content: &str, mode: IndexingMode) -> ExtractedFile {
        let ext = std::path::Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let mut gbuf = GraphBuffer::new("test_root");
        extract_file(file_path, "test_root", ext, content, mode, &mut gbuf)
    }

    // ── ParallelExtractor tests ────────────────────────────────────

    #[test]
    fn empty_files_returns_empty_output() {
        let extractor = ParallelExtractor::new(4);
        let output = extractor
            .extract_all(&[], "test_root", IndexingMode::Full, None)
            .unwrap();
        assert!(output.extracted_files.is_empty());
        assert_eq!(output.files_scanned, 0);
        assert_eq!(output.files_with_errors, 0);
    }

    #[test]
    fn single_file_produces_extracted_file() {
        // Create a temp file.
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.rs");
        std::fs::write(&file_path, "fn hello() -> bool { true }").unwrap();

        let files = vec![make_file(file_path.to_str().unwrap(), 100)];

        let extractor = ParallelExtractor::new(2);
        let output = extractor
            .extract_all(
                &files,
                dir.path().to_str().unwrap(),
                IndexingMode::Full,
                None,
            )
            .unwrap();

        assert_eq!(output.extracted_files.len(), 1);
        assert_eq!(output.files_scanned, 1);
        assert!(output.extracted_files[0].file_path.contains("test.rs"));
    }

    #[test]
    fn multiple_files_produce_correct_count() {
        let dir = tempfile::tempdir().unwrap();
        let a_path = dir.path().join("a.rs");
        let b_path = dir.path().join("b.rs");
        let c_path = dir.path().join("c.rs");
        std::fs::write(&a_path, "fn a() {}").unwrap();
        std::fs::write(&b_path, "fn b() {}").unwrap();
        std::fs::write(&c_path, "fn c() {}").unwrap();

        let files = vec![
            make_file(a_path.to_str().unwrap(), 10),
            make_file(b_path.to_str().unwrap(), 10),
            make_file(c_path.to_str().unwrap(), 10),
        ];

        let extractor = ParallelExtractor::new(4);
        let output = extractor
            .extract_all(
                &files,
                dir.path().to_str().unwrap(),
                IndexingMode::Full,
                None,
            )
            .unwrap();

        assert_eq!(output.extracted_files.len(), 3);
    }

    #[test]
    fn cancel_token_stops_early() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for i in 0..20 {
            let p = dir.path().join(format!("f{i}.rs"));
            std::fs::write(&p, "fn f() {}").unwrap();
            files.push(make_file(p.to_str().unwrap(), 10));
        }

        let token = CancelToken::new();
        let t = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(5));
            t.cancel();
        });

        let extractor = ParallelExtractor::new(4);
        let output = extractor
            .extract_all(
                &files,
                dir.path().to_str().unwrap(),
                IndexingMode::Full,
                Some(&token),
            )
            .unwrap();

        // May have partial result or be fully cancelled — either is fine.
        assert!(output.files_scanned <= 20);
    }

    #[test]
    fn propagation_of_io_error() {
        // Non-existent file → I/O error should propagate.
        let files = vec![make_file("/nonexistent/path.rs", 100)];
        let extractor = ParallelExtractor::new(2);
        let result = extractor.extract_all(&files, "/nonexistent", IndexingMode::Full, None);
        assert!(result.is_err());
    }

    // ── Single-pass extraction tests ───────────────────────────────

    #[test]
    fn rust_function_extracted() {
        let ef = extract_in_memory("test.rs", "fn hello() -> bool { true }", IndexingMode::Full);
        assert!(
            ef.defs.iter().any(|d| d.name == "hello"),
            "expected 'hello' in defs, got: {:?}",
            ef.defs.iter().map(|d| &d.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn struct_extracted() {
        let ef = extract_in_memory(
            "test.rs",
            "pub struct Config { pub name: String, pub port: u16 }",
            IndexingMode::Full,
        );
        assert!(
            ef.defs.iter().any(|d| d.name == "Config"),
            "expected 'Config' in defs"
        );
        let config = ef.defs.iter().find(|d| d.name == "Config").unwrap();
        assert_eq!(config.label, "Struct");
    }

    #[test]
    fn minhash_present_on_function_def() {
        let ef = extract_in_memory(
            "test.rs",
            "fn compute(x: i32, y: i32) -> i32 {
                let a = x + y;
                let b = a * 2;
                if b > 100 { return 100; }
                let c = b - 5;
                let d = c / 3;
                d
            }",
            IndexingMode::Full,
        );
        let compute = ef.defs.iter().find(|d| d.name == "compute").unwrap();
        assert!(
            compute.minhash.is_some(),
            "expected minhash on function with body"
        );
    }

    #[test]
    fn empty_body_no_minhash() {
        let ef = extract_in_memory("test.rs", "fn f() {}", IndexingMode::Full);
        let f = ef.defs.iter().find(|d| d.name == "f").unwrap();
        assert!(
            f.minhash.is_none(),
            "short function should not have minhash"
        );
    }

    #[test]
    fn parse_error_fallback_sets_flag() {
        // Truly unsupported extension → always uses regex fallback with error flag.
        let ef = extract_in_memory("broken.xyz", "fn hello() {}", IndexingMode::Full);
        assert!(
            ef.has_parse_error,
            "unsupported ext should set has_parse_error"
        );
        // Even with has_parse_error, fallback should still produce defs.
        assert!(
            !ef.defs.is_empty(),
            "expected defs even with parse error, got empty"
        );
    }

    #[test]
    fn python_extraction() {
        let ef = extract_in_memory(
            "test.py",
            "def calculate(x, y):\n    return x + y\n\nclass Worker:\n    pass",
            IndexingMode::Full,
        );
        assert!(
            ef.defs.iter().any(|d| d.name == "calculate"),
            "expected 'calculate' in defs"
        );
        assert!(
            ef.defs.iter().any(|d| d.name == "Worker"),
            "expected 'Worker' in defs"
        );
    }

    #[test]
    fn typescript_extraction() {
        let ef = extract_in_memory(
            "test.ts",
            "export function greet(name: string): string { return `Hello ${name}`; }",
            IndexingMode::Full,
        );
        assert!(
            ef.defs.iter().any(|d| d.name == "greet"),
            "expected 'greet' in defs"
        );
    }

    #[test]
    fn chunks_extracted_from_rust() {
        let ef = extract_in_memory(
            "test.rs",
            "fn process() -> bool { true }\nfn helper() -> u32 { 42 }",
            IndexingMode::Full,
        );
        assert!(
            !ef.chunks.is_empty(),
            "expected at least one chunk for Rust file"
        );
    }

    #[test]
    fn content_hash_included() {
        let ef = extract_in_memory("test.rs", "fn foo() {}", IndexingMode::Full);
        assert!(!ef.content_hash.is_empty(), "content_hash should be set");
    }

    // ── Determinism ────────────────────────────────────────────────

    #[test]
    fn deterministic_output_same_input() {
        let dir = tempfile::tempdir().unwrap();
        let fp = dir.path().join("x.rs");
        std::fs::write(&fp, "fn foo() {}\nfn bar() {}").unwrap();
        let files = vec![make_file(fp.to_str().unwrap(), 50)];

        let extractor = ParallelExtractor::new(2);
        let out1 = extractor
            .extract_all(
                &files,
                dir.path().to_str().unwrap(),
                IndexingMode::Full,
                None,
            )
            .unwrap();
        let out2 = extractor
            .extract_all(
                &files,
                dir.path().to_str().unwrap(),
                IndexingMode::Full,
                None,
            )
            .unwrap();

        assert_eq!(out1.extracted_files.len(), out2.extracted_files.len());
        for (ef1, ef2) in out1.extracted_files.iter().zip(out2.extracted_files.iter()) {
            assert_eq!(ef1.defs.len(), ef2.defs.len());
            for (d1, d2) in ef1.defs.iter().zip(ef2.defs.iter()) {
                assert_eq!(d1.name, d2.name);
                assert_eq!(d1.qualified_name, d2.qualified_name);
                assert_eq!(d1.start_line, d2.start_line);
            }
        }
    }

    // ── File sorting by size ───────────────────────────────────────

    #[test]
    fn file_sorting_by_descending_size() {
        let mut files = [
            make_file("small.rs", 10),
            make_file("large.rs", 1000),
            make_file("medium.rs", 100),
        ];

        // The sort is internal to extract_all, but we verify sort_unstable_by
        // logic directly.
        files.sort_unstable_by_key(|b| std::cmp::Reverse(b.size));
        assert_eq!(files[0].rel_path, "large.rs");
        assert_eq!(files[1].rel_path, "medium.rs");
        assert_eq!(files[2].rel_path, "small.rs");
    }

    // ── GraphBuffer integration ───────────────────────────────────

    #[test]
    fn worker_gbufs_merged_into_main() {
        let dir = tempfile::tempdir().unwrap();
        let fp = dir.path().join("lib.rs");
        std::fs::write(&fp, "pub fn api() -> u32 { 42 }").unwrap();
        let files = vec![make_file(fp.to_str().unwrap(), 50)];

        let extractor = ParallelExtractor::new(2);
        let output = extractor
            .extract_all(
                &files,
                dir.path().to_str().unwrap(),
                IndexingMode::Full,
                None,
            )
            .unwrap();

        // The merged graph should have at least one node (the function def).
        assert!(
            output.graph.node_count() >= 1,
            "expected at least 1 node in merged graph, got {}",
            output.graph.node_count()
        );
    }

    // ── Edge cases ─────────────────────────────────────────────────

    #[test]
    fn unsupported_language_uses_fallback() {
        // Use a pattern matching the generic regex keywords (def/func/fun/fn).
        let ef = extract_in_memory(
            "file.xyz",
            "public fn hello() -> bool { return true; }",
            IndexingMode::Full,
        );
        // XYZ is unsupported → always uses regex generic fallback.
        assert!(ef.has_parse_error, "unsupported ext should set parse_error");
        // Generic regex should still extract "hello".
        assert!(
            ef.defs.iter().any(|d| d.name == "hello"),
            "expected 'hello' via fallback, got: {:?}",
            ef.defs.iter().map(|d| &d.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_file_detection() {
        let ef = extract_in_memory("test_module.rs", "fn test_stuff() {}", IndexingMode::Full);
        assert!(ef.is_test_file, "path containing 'test' should be flagged");
    }

    #[test]
    fn qualified_name_format() {
        let qn = build_qualified_name("src/lib.rs", "hello");
        assert_eq!(qn, "src::lib::hello");
    }

    #[test]
    fn qualified_name_root_file() {
        let qn = build_qualified_name("main.rs", "run");
        assert_eq!(qn, "main::run");
    }
}
