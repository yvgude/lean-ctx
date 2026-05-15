mod classification;
pub(crate) mod engine;
mod footer;
mod passthrough;
#[cfg(test)]
mod tests;

pub use engine::compress_if_beneficial_pub;
pub use footer::shell_savings_footer;

pub(super) use classification::is_excluded_command;
pub(super) use engine::compress_and_measure;

pub use classification::has_structural_output;
pub use classification::is_verbatim_output;
