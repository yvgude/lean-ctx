// SPDX-License-Identifier: Apache-2.0

//! The newest lean-ctx version that wrote the agent setup.
//!
//! An update replaces the binary while MCP servers and daemons of the old
//! build keep running; their silent hook refresh then rewrites the setup the
//! new build just wrote (3.12.1: a still-running dogfood MCP server put the
//! standard hooks and the Grep/Glob deny back over a governor install that had
//! no MCP entry, and sessions started in between had no search tool). Every
//! explicit setup or init records its version here; the silent refresh of an
//! older binary sees a newer writer and leaves the setup alone. Binaries from
//! before this check (3.12.1 and older) still refresh until they restart.

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn stamp_path() -> Option<std::path::PathBuf> {
    lean_ctx::core::paths::state_dir()
        .ok()
        .map(|d| d.join("setup-writer"))
}

fn recorded() -> Option<String> {
    stamp_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn write_stamp() {
    if let Some(path) = stamp_path() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, VERSION);
    }
}

/// An explicit setup or init ran with this binary: it is the setup's writer,
/// also after a downgrade (the user chose this build).
pub fn record() {
    write_stamp();
}

/// A silent refresh ran with this binary: record it unless a newer build
/// already wrote the setup.
pub fn record_if_newest() {
    if newer_writer().is_none() {
        write_stamp();
    }
}

/// The recorded writer's version when it is newer than this binary.
pub fn newer_writer() -> Option<String> {
    recorded().filter(|v| lean_ctx::core::version_check::is_newer(v, VERSION))
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_older_binary_yields_to_a_newer_writer() {
        let _iso = lean_ctx::core::data_dir::isolated_data_dir();
        assert_eq!(super::newer_writer(), None, "no writer yet");
        super::record_if_newest();
        assert_eq!(
            super::newer_writer(),
            None,
            "this binary is no newer writer"
        );

        let path = super::stamp_path().unwrap();
        std::fs::write(&path, "999.0.0").unwrap();
        assert_eq!(super::newer_writer().as_deref(), Some("999.0.0"));
        super::record_if_newest();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "999.0.0",
            "a silent refresh of an older binary never lowers the stamp"
        );
        // An explicit setup with the older binary (a downgrade) takes over,
        // so its later refreshes run again.
        super::record();
        assert_eq!(super::newer_writer(), None);

        // A prerelease of this version is older than this binary.
        std::fs::write(&path, format!("{}-dogfood.9", super::VERSION)).unwrap();
        assert_eq!(super::newer_writer(), None);
    }
}
