pub mod compress;
mod exec;
mod interactive;
pub mod output_policy;
pub(crate) mod platform;
mod redact;

pub use compress::compress_if_beneficial_pub;
pub(crate) use exec::heavy_timeout;
pub use exec::{exec, exec_argv};
pub use interactive::interactive;
pub use output_policy::{classify as classify_output, OutputPolicy};
pub use platform::{
    decode_output, is_container, is_non_interactive, join_command, join_command_for,
    shell_and_flag, shell_name,
};
pub use redact::save_tee;
