// SPDX-License-Identifier: Apache-2.0

//! Pure work-graph and outcome model for the LeanCTX Engine.
//!
//! [`work_graph`] bounds multi-agent delegation (budget inheritance, fan-out,
//! depth, stop conditions, execution fences, receipts), [`budget_cascade`]
//! splits a parent budget across children, and [`outcome`] evaluates local
//! signals against versioned task-class contracts. Storage, transport and
//! policy stay in the engine, which reaches this crate through its
//! `core::work_graph` and `core::outcome` facades. This crate never depends
//! on the engine.

pub mod budget_cascade;
pub mod outcome;
pub mod work_graph;
