mod compress;
mod exec;
mod interactive;
mod platform;
mod redact;

pub use compress::compress_if_beneficial_pub;
pub use exec::{exec, exec_argv};
pub use interactive::interactive;
pub use platform::{
    decode_output, is_container, is_non_interactive, join_command, shell_and_flag, shell_name,
};
pub use redact::save_tee;
