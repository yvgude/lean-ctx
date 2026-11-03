// SPDX-License-Identifier: Apache-2.0
use std::path::Path;

use crate::tools::CrpMode;
use lean_ctx::core::cache::SessionCache;
use lean_ctx::core::compressor;
use lean_ctx::core::deps;
use lean_ctx::core::entropy;
use lean_ctx::core::protocol;
use lean_ctx::core::signatures;
use lean_ctx::core::symbol_map::{self, SymbolMap};
use lean_ctx::core::tokens::count_tokens;
// `pub(crate)`: the conformance suite renders modes directly for its
// accuracy invariants (GL#441).
pub mod dedup_hook;
mod helpers;
pub use helpers::graph_related_hint;
use helpers::{detect_project_root, find_similar_and_update_semantic_index};
pub use lean_ctx::core::instruction_files::{
    instruction_file_forces_full, is_instruction_file, is_user_excluded,
};
mod fallback_banner;
mod kernel;
pub(crate) use kernel::kernel_trailer;
mod outline_budget;
pub(crate) mod render;
/// Type-safe read-mode vocabulary (#528): single source of truth for which
/// modes exist and how each is classified.
pub(crate) use lean_ctx::core::read_mode as mode;
pub use mode::{ReadMode, canonicalize_tail_mode};
pub use render::*;

mod types;
pub use types::*;
mod disk_window;
pub(crate) use disk_window::try_disk_anchored_window;
pub use lean_ctx::core::rooted_read::*;
pub mod dispatch;
pub use dispatch::*;
mod core_logic;
#[allow(unreachable_pub, unused_imports)]
pub use core_logic::*;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_delta;
#[cfg(test)]
mod tests_windowed;
