// SPDX-License-Identifier: Apache-2.0
//! The binary this process started from, so a long-running MCP server can say
//! when an install replaced it on disk. An MCP server keeps running the engine
//! it started with: on 25.10. every agent session still ran a two-day-old
//! engine after three installs, and fixes reached nobody until a reconnect.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::SystemTime;

/// What identifies the executable file: a replaced file has another inode
/// (atomic install), and in any case another size or modification time.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
    inode: u64,
}

static AT_START: OnceLock<Option<Identity>> = OnceLock::new();

fn current() -> Option<Identity> {
    let path = std::env::current_exe().ok()?;
    let meta = std::fs::metadata(&path).ok()?;
    #[cfg(unix)]
    let inode = std::os::unix::fs::MetadataExt::ino(&meta);
    #[cfg(not(unix))]
    let inode = 0;
    Some(Identity {
        path,
        len: meta.len(),
        modified: meta.modified().ok(),
        inode,
    })
}

/// Record the executable as it is now; call once, early in `main`.
pub fn record_start() {
    let _ = AT_START.get_or_init(current);
}

/// The note for an agent when the binary on disk is no longer the one this
/// process started from; `None` while they match (or nothing was recorded).
#[must_use]
pub fn replaced_note() -> Option<String> {
    let start = AT_START.get()?.as_ref()?;
    note_for(start, &current()?)
}

fn note_for(start: &Identity, now: &Identity) -> Option<String> {
    (start != now).then(|| {
        "Note: lean-ctx was updated on disk after this MCP server started; it still runs the \
         old engine. Reconnect the MCP server (Claude Code: /mcp, then reconnect lean-ctx) to \
         use the new one. A reconnect gives this session a new bus id: register your role \
         again and repost your open claims under the new id.\n"
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(inode: u64, len: u64) -> Identity {
        Identity {
            path: PathBuf::from("/usr/local/bin/lean-ctx"),
            len,
            modified: Some(SystemTime::UNIX_EPOCH),
            inode,
        }
    }

    #[test]
    fn a_replaced_binary_is_named_and_an_unchanged_one_is_not() {
        let start = identity(7, 100);
        assert_eq!(note_for(&start, &identity(7, 100)), None);
        let replaced = note_for(&start, &identity(8, 100)).expect("new inode");
        assert!(replaced.contains("Reconnect the MCP server"), "{replaced}");
        assert!(
            note_for(&start, &identity(7, 101)).is_some(),
            "rewritten in place"
        );
    }

    #[test]
    fn the_running_test_binary_matches_itself() {
        record_start();
        assert_eq!(replaced_note(), None);
    }
}
