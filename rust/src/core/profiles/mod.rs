//! Agent profiles — workload-specific configurations.
//!
//! Profiles compose existing configuration knobs into named presets. They are
//! loaded from three tiers (highest priority first):
//! 1. Project: `.lean-ctx/profiles/<name>.toml`
//! 2. User: `~/.config/lean-ctx/profiles/<name>.toml`
//! 3. Built-in defaults
//!
//! A profile can extend another via `extends = "parent"`. Child fields override
//! parent fields; unset fields inherit.

mod builtins;
mod engine_config;
mod loading;
pub mod types;

pub(crate) use loading::*;
pub use types::*;

#[cfg(test)]
mod tests;
