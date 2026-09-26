//! #1867: only the command that receives a pipe is a pipe target.
//!
//! Split out of `tests.rs` to keep that file under the LOC gate.

use super::*;

/// An interpreter after `;`/`&&`/`||` is not a pipe target and must not be
/// flagged; a real pipe into it still is.
#[test]
fn gh1867_interpreter_after_sequence_is_not_a_pipe_target() {
    for cmd in [
        "git --version | head -1; python --version",
        "ls | wc -l && python3",
        "echo hi | cat || node",
    ] {
        assert!(check_pipe_to_bare_interpreter(cmd, true).is_ok(), "{cmd}");
    }
    assert!(check_pipe_to_bare_interpreter("cat x | python", true).is_err());
    assert!(check_pipe_to_bare_interpreter("true; cat x | python", true).is_err());
}
