// SPDX-License-Identifier: Apache-2.0
//! Which MCP stdio servers this installation may clean up.
//!
//! A starting server used to terminate every process named `lean-ctx` whose
//! parent was PID 0/1 and whose first argument was absent or `mcp` — on the
//! whole machine, whatever data directory or binary it belonged to. A process
//! name and a parent PID prove nothing about ownership.
//!
//! Now each server records itself under `<data dir>/mcp-instances/<pid>.json`
//! with its OS process identity (creation marker + executable) and the parent
//! it was started by. Cleanup only ever looks at those records. A recorded
//! process is terminated only if all of this holds: the identity still matches
//! (no PID reuse), it runs the same executable as the cleaning server, it was
//! started by a real parent, and it has since lost that parent. Anything
//! unproven is left alone.
//!
//! What a record proves: this data directory and this executable — the same
//! installation. It does not prove the same session; a server of another
//! session of this installation that lost its parent is cleaned up, that is the
//! purpose. A live session (parent present), a process without a record, a
//! server of another binary sharing the data directory, and every process of
//! another data directory are never signalled.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ipc::process::{self, ProcessIdentity};

const DIRECTORY: &str = "mcp-instances";
const MAX_RECORD_BYTES: u64 = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Instance {
    pid: u32,
    parent_pid: u32,
    identity: ProcessIdentity,
}

/// Removes this server's record on a normal return. An exit that skips it
/// leaves a record the next cleanup recognises as stale and forgets.
pub(super) struct Registration {
    path: PathBuf,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn directory() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|data| data.join(DIRECTORY))
}

fn record_path(directory: &Path, pid: u32) -> PathBuf {
    directory.join(format!("{pid}.json"))
}

/// Record the current process. `None` when the OS exposes no provable identity
/// or the record cannot be written: such a server is simply never cleaned up
/// by another one.
pub(super) fn register() -> Option<Registration> {
    let pid = std::process::id();
    let directory = directory()?;
    std::fs::create_dir_all(&directory).ok()?;
    let path = record_path(&directory, pid);
    write(
        &path,
        &Instance {
            pid,
            parent_pid: std::os::unix::process::parent_id(),
            identity: process::identity(pid)?,
        },
    )?;
    Some(Registration { path })
}

fn write(path: &Path, instance: &Instance) -> Option<()> {
    let bytes = serde_json::to_vec(instance).ok()?;
    crate::core::atomic_fs::try_atomic_write(path, &bytes, None).ok()
}

fn read(path: &Path) -> Option<Instance> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_RECORD_BYTES {
        return None;
    }
    let instance: Instance = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    // A record speaks only for the PID it is filed under.
    (path.file_name()?.to_str()? == format!("{}.json", instance.pid)).then_some(instance)
}

/// Terminate recorded servers of this installation that lost their parent.
/// `lost_parent` is asked only for a process whose recorded identity was just
/// re-proven.
pub(super) fn terminate_own_orphans(lost_parent: impl Fn(u32) -> bool) {
    // Without a provable identity of our own there is nothing to compare with.
    if let Some(own) = process::identity(std::process::id()) {
        terminate_orphans_of(&own.executable, lost_parent);
    }
}

fn terminate_orphans_of(executable: &str, lost_parent: impl Fn(u32) -> bool) {
    let Some(directory) = directory() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return;
    };
    let me = std::process::id();
    for path in entries.flatten().map(|entry| entry.path()) {
        let Some(instance) = read(&path) else {
            continue;
        };
        if instance.pid == me {
            continue;
        }
        if !process::is_alive(instance.pid) {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        match process::identity(instance.pid) {
            // Not provable on this platform right now: no signal, keep the record.
            None => continue,
            // The PID now belongs to another process.
            Some(live) if live != instance.identity => {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            Some(_) => {}
        }
        // Another binary sharing this data directory looks after its own servers.
        if instance.identity.executable != executable {
            continue;
        }
        // Started without a parent on purpose (service manager, nohup): not an orphan.
        if instance.parent_pid <= 1 || !lost_parent(instance.pid) {
            continue;
        }
        // The check above took time; prove the identity once more at the signal.
        if process::matches_identity(instance.pid, &instance.identity) {
            tracing::info!(
                "[orphan-cleanup] terminating own orphaned MCP server {}",
                instance.pid
            );
            let _ = process::terminate_gracefully(instance.pid);
        }
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    fn sleeper() -> Child {
        long_running("sleep", &["60"])
    }

    fn long_running(program: &str, arguments: &[&str]) -> Child {
        Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("process starts")
    }

    fn record(directory: &Path, child: &Child, change: impl FnOnce(&mut Instance)) -> PathBuf {
        let mut instance = Instance {
            pid: child.id(),
            parent_pid: std::process::id(),
            identity: process::identity(child.id()).expect("child identity"),
        };
        change(&mut instance);
        let path = record_path(directory, child.id());
        write(&path, &instance).expect("record written");
        path
    }

    fn exited(child: &mut Child) -> bool {
        // Generous: it returns as soon as the child is gone, and a signalled
        // child on a loaded host (load ~60 in the R05 export run) can need
        // more than a few seconds to be reaped.
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if child.try_wait().expect("child state").is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// Real processes; `sleep` stands for "our executable", `tail` for another
    /// binary sharing the data directory. Even when the shape check says
    /// "orphan" for everything, only a process with a matching record, of our
    /// executable, that was started by a parent is signalled.
    #[test]
    fn only_recorded_identity_proven_parented_servers_are_terminated() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let directory = directory().expect("data directory");
        std::fs::create_dir_all(&directory).unwrap();

        let mut unrecorded = sleeper();
        let mut reused_pid = sleeper();
        let mut started_without_parent = sleeper();
        let mut other_binary = long_running("tail", &["-f", "/dev/null"]);
        let mut own_orphan = sleeper();
        let ours = process::identity(own_orphan.id())
            .expect("child identity")
            .executable;
        let stale = record(&directory, &reused_pid, |instance| {
            instance.identity.start_marker = instance.identity.start_marker.wrapping_add(1);
        });
        let kept = record(&directory, &started_without_parent, |instance| {
            instance.parent_pid = 1;
        });
        let foreign = record(&directory, &other_binary, |_| {});
        let orphan = record(&directory, &own_orphan, |_| {});
        let own = register().expect("own record");

        terminate_orphans_of(&ours, |_| true);

        assert!(exited(&mut own_orphan), "the proven own orphan is ended");
        assert!(!orphan.exists());
        for (name, child) in [
            ("no record", &mut unrecorded),
            ("record of another process generation", &mut reused_pid),
            (
                "recorded as started without a parent",
                &mut started_without_parent,
            ),
            ("recorded orphan of another executable", &mut other_binary),
        ] {
            assert!(
                child.try_wait().unwrap().is_none(),
                "{name}: must not be signalled"
            );
        }
        assert!(!stale.exists(), "a record for a reused PID is forgotten");
        assert!(kept.exists() && foreign.exists());
        assert!(own.path.exists(), "the running server keeps its own record");

        // Two normal recorded sessions of our executable that still have their
        // parent stay, and so do their records.
        let sessions = [
            record(&directory, &unrecorded, |_| {}),
            record(&directory, &reused_pid, |_| {}),
        ];
        terminate_orphans_of(&ours, |_| false);
        assert!(unrecorded.try_wait().unwrap().is_none());
        assert!(reused_pid.try_wait().unwrap().is_none());
        assert!(sessions.iter().all(|path| path.exists()));

        // The production entry point compares with this process's executable:
        // none of the recorded children is one of ours.
        terminate_own_orphans(|_| true);
        assert!(unrecorded.try_wait().unwrap().is_none());
        assert!(other_binary.try_wait().unwrap().is_none());

        drop(own);
        for child in [
            &mut unrecorded,
            &mut reused_pid,
            &mut started_without_parent,
            &mut other_binary,
        ] {
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }
}
