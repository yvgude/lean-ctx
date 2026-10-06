mod env_guard;
mod first_run;
mod index_build;
mod intelligence;
pub(crate) use intelligence::configure_runtime;
mod interactive;
mod onboard;
mod options;
mod paths;
mod with_options;

// Shared imports for sibling submodules that use `use super::*` (mcp, helpers).
use crate::core::editor_registry::{ConfigType, EditorTarget, WriteAction, WriteOptions};
use crate::core::portable_binary::resolve_agent_binary;
use std::path::PathBuf;

mod mcp;
pub use mcp::*;
mod helpers;
pub use helpers::*;

#[cfg(test)]
pub(crate) use env_guard::EnvVarGuard;
pub use interactive::*;
pub use onboard::*;
pub use options::*;
pub use paths::*;
pub use with_options::*;
