mod boundaries;
mod classification;
#[allow(dead_code)]
pub mod engine;
mod footer;
mod passthrough;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_engine;

pub use engine::compress_if_beneficial_pub;
pub use footer::shell_savings_footer;

pub(super) use classification::is_excluded_command;
pub(super) use engine::compress_and_measure;

pub use classification::has_structural_output;
pub use classification::is_verbatim_output;

/// Size of the built-in passthrough list, for the docs drift check (#1896).
#[cfg(test)]
pub(crate) fn builtin_passthrough_count() -> usize {
    passthrough::BUILTIN_PASSTHROUGH.len()
}
