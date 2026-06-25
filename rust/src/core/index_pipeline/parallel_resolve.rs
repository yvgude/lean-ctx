//! Phase 4: Serial resolution — create CALLS, USAGES, THROWS, CHANNEL edges.
//!
//! This is a serial pass (no ThreadPool) because all resolution functions
//! take `&mut GraphBuffer` which prevents parallel access.  The C reference
//! achieves parallel resolution through a different architecture (atomic
//! shared IDs + per-worker edge buffers); the Rust GraphBuffer API
//! currently requires exclusive access.
//!
//! Consumes cached [`ExtractedFile`] results and resolves each call, usage,
//! throw, and channel into graph edges by looking up target symbols in the
//! [`Registry`].
//!
//! ## Design
//!
//! - **Read-only registry**: `Registry` is passed by `&` — no mutation.
//! - **Silent skip**: Unresolvable names (not in registry, not in graph buffer)
//!   produce no edges. No panics.
//! - **Deterministic**: Output depends only on (extracted_files, registry,
//!   graph_buffer contents) — no timestamps, counters, or randomness.
//!
//! ## Edge types produced
//!
//! | Edge type     | Source              | Target                  | Condition          |
//! |---------------|---------------------|-------------------------|--------------------|
//! | `CALLS`       | Function node       | Function/Method node    | `ef.calls`         |
//! | `USES`        | Function node       | Any definition node     | `ef.usages`        |
//! | `THROWS`      | Function node       | Exception/class node    | `ef.throws`        |
//! | `EMITS`       | Function node       | Channel node            | `channel.is_write` |
//! | `LISTENS_ON`  | Function node       | Channel node            | `!channel.is_write`|
//!
//! Channel nodes use the qualified-name convention `__channel__::{name}` to
//! avoid collisions with regular definition nodes.
//!
//! ## C reference
//!
//! Maps to `resolve_worker` in
//! `/tmp/codebase-memory-mcp/src/pipeline/pass_parallel.c:972-1053`.

use std::collections::HashMap;

use anyhow::Result;

use crate::core::graph_buffer::GraphBuffer;
use crate::core::index_pipeline::registry_build::Registry;
use crate::core::index_types::{ExtractedFile, NodeId};

/// Resolve calls, usages, throws, and channels from cached extracted files.
///
/// # Arguments
///
/// * `extracted_files` — Per-file extraction results (output of Phase 3A).
/// * `registry` — Symbol registry (output of Phase 3B), queried read-only.
/// * `gbuf` — Graph buffer containing definition nodes (from Phases 1–3).
///
/// # Returns
///
/// `Ok(())` on success. The function is infallible under normal operation;
/// unresolvable names are silently skipped.
///
/// # Edge types produced
///
/// See [module-level documentation](self) for the complete table.
pub fn serial_resolve(
    extracted_files: &[ExtractedFile],
    registry: &Registry,
    gbuf: &mut GraphBuffer,
) -> Result<()> {
    for ef in extracted_files {
        process_file(ef, registry, gbuf);
    }
    Ok(())
}

/// Process a single extracted file, emitting all applicable edges.
///
/// # Panics
///
/// Does not panic. Unresolvable calls/usages/throws/channels are silently
/// skipped.
fn process_file(ef: &ExtractedFile, registry: &Registry, gbuf: &mut GraphBuffer) {
    // ── CALLS edges ────────────────────────────────────────────────
    for call in &ef.calls {
        // Extract src_id (Copy) before any gbuf closure borrow.
        let src_id = gbuf.find_by_qn(&call.enclosing_func_qn).map(|n| n.id);
        if let Some(sid) = src_id {
            let targets: Vec<NodeId> = registry
                .lookup(&call.callee_name)
                .into_iter()
                .flatten()
                .filter_map(|qn| gbuf.find_by_qn(qn))
                .map(|n| n.id)
                .collect();
            for tgt in targets {
                let mut props = HashMap::new();
                props.insert("line".to_string(), call.start_line.to_string());
                gbuf.insert_edge(sid, tgt, "CALLS", props);
            }
        }
    }

    // ── USAGE edges ────────────────────────────────────────────────
    for usage in &ef.usages {
        let src_id = gbuf.find_by_qn(&usage.enclosing_func_qn).map(|n| n.id);
        if let Some(sid) = src_id {
            let targets: Vec<NodeId> = registry
                .lookup(&usage.ref_name)
                .into_iter()
                .flatten()
                .filter_map(|qn| gbuf.find_by_qn(qn))
                .map(|n| n.id)
                .collect();
            for tgt in targets {
                gbuf.insert_edge(sid, tgt, "USES", HashMap::new());
            }
        }
    }

    // ── THROWS edges ───────────────────────────────────────────────
    for throw in &ef.throws {
        let src_id = gbuf.find_by_qn(&throw.enclosing_func_qn).map(|n| n.id);
        if let Some(sid) = src_id {
            let targets: Vec<NodeId> = registry
                .lookup(&throw.exception_name)
                .into_iter()
                .flatten()
                .filter_map(|qn| gbuf.find_by_qn(qn))
                .map(|n| n.id)
                .collect();
            for tgt in targets {
                gbuf.insert_edge(sid, tgt, "THROWS", HashMap::new());
            }
        }
    }

    // ── CHANNEL edges ──────────────────────────────────────────────
    for channel in &ef.channels {
        let src_id = gbuf.find_by_qn(&channel.enclosing_func_qn).map(|n| n.id);
        if let Some(sid) = src_id {
            let channel_qn = format!("__channel__::{}", channel.channel_name);
            let ch_id = gbuf.upsert_node(
                "Channel",
                &channel.channel_name,
                &channel_qn,
                "",
                0,
                0,
                HashMap::new(),
            );
            let etype = if channel.is_write {
                "EMITS"
            } else {
                "LISTENS_ON"
            };
            gbuf.insert_edge(sid, ch_id, etype, HashMap::new());
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::index_types::{Call, Channel, DefKind, Definition};

    // ── Test helpers ──────────────────────────────────────────────

    fn make_def(name: &str, qn: &str, label: &str, parent: Option<&str>) -> Definition {
        Definition {
            name: name.into(),
            qualified_name: qn.into(),
            kind: DefKind::Function,
            label: label.into(),
            file_path: "test.rs".into(),
            start_line: 1,
            end_line: 10,
            signature: None,
            return_type: None,
            receiver: None,
            docstring: None,
            parent_class: parent.map(String::from),
            decorators: vec![],
            base_classes: vec![],
            param_names: vec![],
            param_types: vec![],
            is_async: false,
            is_exported: false,
            is_abstract: false,
            is_test: false,
            is_entry_point: false,
            complexity: 0,
            cognitive: 0,
            loop_count: 0,
            loop_depth: 0,
            is_recursive: false,
            param_count: 0,
            minhash: None,
            body_tokens: None,
        }
    }

    // ── Tests ─────────────────────────────────────────────────────

    #[test]
    fn empty_files_no_edges() {
        let mut g = GraphBuffer::new("t");
        let r = Registry::new();
        serial_resolve(&[], &r, &mut g).unwrap();
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn calls_edge_created() {
        let mut g = GraphBuffer::new("t");
        g.upsert_node(
            "Function",
            "caller",
            "mod.rs::caller",
            "mod.rs",
            1,
            5,
            HashMap::new(),
        );
        g.upsert_node(
            "Function",
            "callee",
            "lib.rs::callee",
            "lib.rs",
            1,
            3,
            HashMap::new(),
        );

        let mut r = Registry::new();
        r.register(&make_def("callee", "lib.rs::callee", "Function", None));

        let ef = ExtractedFile {
            file_path: "mod.rs".into(),
            module_qn: None,
            defs: vec![],
            calls: vec![Call {
                callee_name: "callee".into(),
                enclosing_func_qn: "mod.rs::caller".into(),
                start_line: 3,
                arg_count: 1,
                args: vec![],
            }],
            imports: vec![],
            usages: vec![],
            throws: vec![],
            channels: vec![],
            chunks: vec![],
            content_hash: "a".into(),
            is_test_file: false,
            has_parse_error: false,
        };
        serial_resolve(&[ef], &r, &mut g).unwrap();
        assert_eq!(g.edge_count(), 1);
    }

    #[test]
    fn unresolvable_call_no_edge() {
        let mut g = GraphBuffer::new("t");
        g.upsert_node("Function", "f", "mod.rs::f", "mod.rs", 1, 5, HashMap::new());
        let r = Registry::new();
        let ef = ExtractedFile {
            file_path: "mod.rs".into(),
            module_qn: None,
            defs: vec![],
            calls: vec![Call {
                callee_name: "nonexistent".into(),
                enclosing_func_qn: "mod.rs::f".into(),
                start_line: 3,
                arg_count: 0,
                args: vec![],
            }],
            imports: vec![],
            usages: vec![],
            throws: vec![],
            channels: vec![],
            chunks: vec![],
            content_hash: "a".into(),
            is_test_file: false,
            has_parse_error: false,
        };
        serial_resolve(&[ef], &r, &mut g).unwrap();
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn channel_creates_node_and_edge() {
        let mut g = GraphBuffer::new("t");
        g.upsert_node(
            "Function",
            "pub",
            "mod.rs::pub",
            "mod.rs",
            1,
            5,
            HashMap::new(),
        );
        let r = Registry::new();
        let ef = ExtractedFile {
            file_path: "mod.rs".into(),
            module_qn: None,
            defs: vec![],
            calls: vec![],
            imports: vec![],
            usages: vec![],
            throws: vec![],
            channels: vec![Channel {
                channel_name: "events".into(),
                enclosing_func_qn: "mod.rs::pub".into(),
                is_write: true,
            }],
            chunks: vec![],
            content_hash: "a".into(),
            is_test_file: false,
            has_parse_error: false,
        };
        serial_resolve(&[ef], &r, &mut g).unwrap();
        assert_eq!(g.edge_count(), 1);
        assert!(
            g.find_by_qn("__channel__::events").is_some(),
            "channel node missing"
        );
    }
}
