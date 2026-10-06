pub(crate) mod agent_wrapper;
pub mod compress;
pub(crate) mod exec;
pub(crate) mod exit_status;
mod interactive;
pub mod output_policy;
mod pipeline;
pub(crate) mod platform;
pub(crate) mod process_tree;
mod redact;
pub(crate) mod reentry;
pub(crate) mod tee_policy;

pub use compress::compress_if_beneficial_pub;
pub(crate) use exec::shell_timeout_with_override;
pub(crate) use exec::{STDERR_LABEL, combine_streams};
pub(crate) use exec::{
    ShellDispatchObservation, execute_prepared, prepare_exec, prepare_exec_argv,
};
pub use exec::{exec, exec_argv};
pub use interactive::interactive;
pub use output_policy::{OutputPolicy, classify as classify_output};
pub use platform::{
    decode_output, is_container, is_non_interactive, join_command, join_command_for,
    resolve_carriage_returns, shell_and_flag, shell_name,
};
pub(crate) use redact::cleanup_old_tee_logs;
pub use redact::save_tee;
