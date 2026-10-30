mod applied_edits;
mod implementation;
mod reindent;

pub use implementation::*;

#[cfg(test)]
use crate::core::cache::SessionCache;
#[cfg(all(test, unix))]
use crate::tools::edit_io::write_atomic_bytes_with_permissions;
#[cfg(test)]
use std::path::Path;

#[cfg(test)]
include!("tests.rs");
