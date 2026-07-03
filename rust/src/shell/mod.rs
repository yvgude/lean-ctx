pub(crate) mod agent_wrapper;
pub mod compress;
mod exec;
mod interactive;
pub mod output_policy;
pub(crate) mod platform;
mod redact;
pub(crate) mod reentry;
pub(crate) mod tee_policy;

pub use compress::compress_if_beneficial_pub;
pub(crate) use exec::shell_timeout_with_override;
pub(crate) use exec::{STDERR_LABEL, combine_streams};
pub use exec::{exec, exec_argv};
pub use interactive::interactive;
pub use output_policy::{OutputPolicy, classify as classify_output};
pub use platform::{
    decode_output, is_container, is_non_interactive, join_command, join_command_for,
    shell_and_flag, shell_name,
};
pub(crate) use redact::cleanup_old_tee_logs;
pub use redact::save_tee;
