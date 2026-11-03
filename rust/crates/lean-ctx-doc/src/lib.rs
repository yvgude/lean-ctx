// SPDX-License-Identifier: Apache-2.0

//! The documentation model of lean-ctx.
//!
//! A page is a title and an ordered list of sections; notes are addenda that
//! never touch the body; anchors bind a page to the code it describes, so a
//! change to that code turns the page stale. Everything here is pure: the
//! engine reads and writes the replica through its own adapter, and LeanCTX |
//! Hive replicates the pages as place nodes (`page`, `section`, `note`).

pub mod anchor;
pub mod emspace;
pub mod fold;
pub mod links;
pub mod model;
pub mod replica;
pub mod sections;
pub mod state;
pub mod write;

pub use anchor::{Anchor, AnchorKind, AnchorMode, AnchorRole, AnchorState, Coverage};
pub use model::{Note, Page, PageStatus, Section};

/// The largest section text the splitter produces, in bytes of Markdown. It
/// leaves room for JSON escaping under [`MAX_DOC_BODY_BYTES`].
pub const SECTION_MAX_BYTES: usize = 20_000;

/// C0 2.2: the canonical JSON of a doc node's plaintext body is at most this
/// many bytes, so it still fits a 32 KiB event once sealed.
pub const MAX_DOC_BODY_BYTES: usize = 24_000;

/// The distance between the `order` of two adjacent sections when a page is
/// laid out fresh, so an insert between them rarely forces a renumbering.
pub const ORDER_GAP: u32 = 1024;
