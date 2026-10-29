// SPDX-License-Identifier: Apache-2.0
//! Owner-only secret files.

/// Writes a secret file atomically (tmp + rename) with owner-only permissions
/// (0o600 on Unix), so secrets are never world-readable — not even
/// transiently between create and chmod.
// Used by the Hive connector's state store (`core::hive::state`).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn write_secret_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("secret path has no parent directory"))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("secret path has no file name"))?
        .to_string_lossy();
    // A random part keeps two threads of one process writing the same path
    // from sharing a temporary file.
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).map_err(std::io::Error::other)?;
    let tmp = parent.join(format!(
        ".{name}.tmp.{}.{}",
        std::process::id(),
        hex::encode(nonce)
    ));

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }

    let result = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        // One atomic replace on every platform. Removing the target first on
        // Windows raced with concurrent writers: a rename onto a target whose
        // delete is still pending fails with ACCESS_DENIED.
        crate::core::atomic_fs::replace_file(&tmp, path)
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::write_secret_file;

    #[test]
    fn concurrent_writers_of_one_path_all_succeed_and_leave_one_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.json");
        let writers: Vec<_> = (0..8u8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || write_secret_file(&path, &[i; 64]))
            })
            .collect();
        for writer in writers {
            writer.join().unwrap().unwrap();
        }
        let written = std::fs::read(&path).unwrap();
        assert_eq!(written.len(), 64);
        assert!(written.iter().all(|byte| *byte == written[0]));
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["secret.json"], "no temporary file is left");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
