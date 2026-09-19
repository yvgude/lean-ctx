use std::path::Path;

use crate::core::cache::SessionCache;
use crate::core::compressor;
use crate::core::deps;
use crate::core::entropy;
use crate::core::protocol;
use crate::core::signatures;
use crate::core::symbol_map::{self, SymbolMap};
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;
// `pub(crate)`: the conformance suite renders modes directly for its
// accuracy invariants (GL#441).
pub mod dedup_hook;
mod helpers;
use helpers::{detect_project_root, find_similar_and_update_semantic_index};
pub use helpers::{graph_related_hint, is_instruction_file};
mod kernel;
pub(crate) mod render;
pub(crate) use render::*;
/// Type-safe read-mode vocabulary (#528): single source of truth for which
/// modes exist and how each is classified.
pub(crate) mod mode;
pub(crate) use mode::{ReadMode, canonicalize_tail_mode};

mod types;
pub use types::*;
mod file_io;
pub use file_io::*;
mod dispatch;
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
