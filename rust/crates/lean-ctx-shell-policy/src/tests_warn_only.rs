// SPDX-License-Identifier: Apache-2.0
use super::{allowlist_block_message, warn_only_reason};

/// #1874: at `shell_security = "warn"` the command runs, so its log line must
/// not carry the enforce wording that tells the agent it was stopped for good.
#[test]
fn gh1874_warn_only_reason_drops_the_enforce_wording() {
    let message = allowlist_block_message("pdfinfo", &[], &[]);
    assert_eq!(
        warn_only_reason(&message),
        "'pdfinfo' is not in the shell allowlist."
    );
}

#[test]
fn gh1874_warn_only_reason_keeps_messages_without_the_enforce_prefix() {
    assert_eq!(
        warn_only_reason("plain reason\nsecond line"),
        "plain reason"
    );
    assert_eq!(warn_only_reason(""), "");
}
