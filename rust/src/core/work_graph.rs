// SPDX-License-Identifier: Apache-2.0

//! Bounded Work Graph for multi-agent orchestration.
//!
//! The graph model lives in the `lean-ctx-workgraph` crate and is re-exported
//! here unchanged, so engine paths such as `crate::core::work_graph` stay
//! stable. Persistence lives in `core::work_graph_store`.

pub use lean_ctx_workgraph::work_graph::*;
