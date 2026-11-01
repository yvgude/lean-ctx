// SPDX-License-Identifier: Apache-2.0

//! Deterministic text compressors for the LeanCTX Engine.
//!
//! Every compressor is a pure text transform: file content (plus an optional
//! extension hint) goes in, a shorter rendering (or `None`) comes out. Output is
//! a function of the input bytes only (#498). Engine policy, such as
//! intent-aware compression, stays in the engine, which reaches this crate
//! through its `core::compressor`, `core::html_crush`, `core::tabular_crush` and
//! `core::yaml_crush` facades. This crate never depends on the engine.

pub mod compressor;
pub mod csv;
pub mod html_crush;
mod markdown_compact;
pub mod structured_compact;
pub mod tabular_crush;
pub mod yaml_crush;
