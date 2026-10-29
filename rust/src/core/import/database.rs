// SPDX-License-Identifier: Apache-2.0

//! A history DATABASE bound to the descriptor it was verified on.
//!
//! `admit_history_database` decides on a PATH. SQLite opens by path too, so a
//! file could be exchanged between that decision and the open. Here the file
//! is opened ONCE ([`verify_history_database`]: the rooted no-follow open of
//! the admitted reader), every check runs on that descriptor (regular file,
//! size bound, identity), and the engine never opens the history path at
//! all: the verified descriptor's bytes are copied into a private snapshot
//! inside lean-ctx's own data directory and THAT file is opened read-only
//! ([`VerifiedDatabase::into_snapshot`]). What the importer reads is what was
//! verified, whatever happens to the path afterwards.
//!
//! After the copy the descriptor and the path are checked again — same file
//! (device and inode on Unix), same size, same modification time. A mismatch
//! refuses the database ([`SourceRefusal::Changed`]): it was exchanged, or
//! written while it was copied, and a copy of a database in the middle of a
//! write is not a database state.
//!
//! Side files. `-wal`: when present it is opened and verified the same way
//! and copied next to the snapshot, so committed but not yet checkpointed
//! history is read. `-shm` and `-journal` are never opened, followed or
//! copied; a non-empty `-journal` means a write is in flight and refuses the
//! database. The engine creates its own side files only next to the snapshot,
//! in the private directory — never in the history root, and it takes no lock
//! on the live database.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, OpenFlags};

use super::{SourceRefusal, admit_history_database};

/// Cap for one history database, write-ahead log included.
///
/// A database holds every session of one tool (OpenCode: all projects in one
/// file, tool output inline), so it is sized well above a transcript; the
/// bound is on the disk space and time of the private copy, not on memory —
/// the copy is streamed. A larger database is skipped whole and counted.
pub(crate) const MAX_HISTORY_DATABASE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

const SNAPSHOT_PREFIX: &str = "import-snapshot-";
const SNAPSHOT_FILE: &str = "history.db";
/// A snapshot directory outlives its import only if the process was killed.
const STALE_SNAPSHOT: Duration = Duration::from_hours(24);

/// What is compared between the verified descriptor and a later look at the
/// descriptor or the path. Device and inode exist on Unix only; elsewhere the
/// comparison is type, size and modification time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    regular: bool,
    len: u64,
    modified: Option<SystemTime>,
}

impl Identity {
    fn of(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            regular: metadata.is_file(),
            len: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }
}

/// One file opened under the root without following a link, with the
/// identity read from its descriptor.
#[derive(Debug)]
struct Verified {
    file: File,
    path: PathBuf,
    identity: Identity,
}

impl Verified {
    fn open(path: &Path, root: &Path) -> Result<Self, SourceRefusal> {
        let file = crate::tools::ctx_read::open_rooted_source(path, root)
            .map_err(|_| SourceRefusal::Unreadable)?;
        let identity = file
            .metadata()
            .map(|metadata| Identity::of(&metadata))
            .map_err(|_| SourceRefusal::Unreadable)?;
        if !identity.regular {
            return Err(SourceRefusal::Unreadable);
        }
        Ok(Self {
            file,
            path: path.to_path_buf(),
            identity,
        })
    }

    /// The descriptor and the path (not following a link) still show the file
    /// that was verified.
    fn unchanged(&self) -> bool {
        let seen = |metadata: std::io::Result<std::fs::Metadata>| {
            metadata.ok().map(|metadata| Identity::of(&metadata))
        };
        seen(self.file.metadata()) == Some(self.identity)
            && seen(std::fs::symlink_metadata(&self.path)) == Some(self.identity)
    }

    /// Copy exactly the verified length from the descriptor into a new
    /// private file. A descriptor that yields more or less was written to.
    fn copy_to(&mut self, target: &Path) -> Result<(), SourceRefusal> {
        use std::io::{Read, Seek, SeekFrom};
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut copy = options
            .open(target)
            .map_err(|_| SourceRefusal::Unreadable)?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| SourceRefusal::Unreadable)?;
        let mut bounded = self.file.by_ref().take(self.identity.len + 1);
        let copied =
            std::io::copy(&mut bounded, &mut copy).map_err(|_| SourceRefusal::Unreadable)?;
        if copied == self.identity.len {
            Ok(())
        } else {
            Err(SourceRefusal::Changed)
        }
    }
}

/// A history database after the path decision, opened once and checked on
/// its descriptor. It holds the descriptor; nothing was read yet.
#[derive(Debug)]
pub(crate) struct VerifiedDatabase {
    main: Verified,
    wal: Option<Verified>,
}

/// The path decision ([`admit_history_database`]), then ONE open of the file
/// it was made for, and every further check on that descriptor: a regular
/// file inside the root, reached without following a link, at most
/// [`MAX_HISTORY_DATABASE_BYTES`] together with its write-ahead log.
pub(crate) fn verify_history_database(
    history_root: &Path,
    path: &Path,
) -> Result<VerifiedDatabase, SourceRefusal> {
    let source = admit_history_database(history_root, path)?;
    let main = Verified::open(&source.path, &source.root)?;
    let wal_path = side_file(&source.path, "-wal");
    // Absence is not a decision: a log that appears later is seen by the
    // check after the copy. A log that is there must verify, or nothing is read.
    let wal = if std::fs::symlink_metadata(&wal_path).is_ok() {
        Some(Verified::open(&wal_path, &source.root)?)
    } else {
        None
    };
    let total = wal
        .as_ref()
        .map_or(0, |wal| wal.identity.len)
        .saturating_add(main.identity.len);
    if total > MAX_HISTORY_DATABASE_BYTES {
        return Err(SourceRefusal::TooLarge);
    }
    Ok(VerifiedDatabase { main, wal })
}

impl VerifiedDatabase {
    /// Copy the verified descriptors into a private snapshot and confirm that
    /// neither the files nor their paths changed meanwhile.
    pub(crate) fn into_snapshot(mut self) -> Result<DatabaseSnapshot, SourceRefusal> {
        let directory = snapshot_directory().map_err(|_| SourceRefusal::Unreadable)?;
        let path = directory.path().join(SNAPSHOT_FILE);
        self.main.copy_to(&path)?;
        if let Some(wal) = self.wal.as_mut().filter(|wal| wal.identity.len > 0) {
            wal.copy_to(&side_file(&path, "-wal"))?;
        }
        if !self.unchanged() {
            return Err(SourceRefusal::Changed);
        }
        Ok(DatabaseSnapshot {
            source_path: self.main.path,
            path,
            _directory: directory,
        })
    }

    fn unchanged(&self) -> bool {
        let wal_unchanged = match &self.wal {
            Some(wal) => wal.unchanged(),
            None => !non_empty(&side_file(&self.main.path, "-wal")),
        };
        self.main.unchanged()
            && wal_unchanged
            && !non_empty(&side_file(&self.main.path, "-journal"))
    }
}

/// The private copy of a verified history database. The directory and
/// everything the engine puts next to the copy are removed when this is
/// dropped — keep it alive for as long as the connection is open.
#[derive(Debug)]
pub(crate) struct DatabaseSnapshot {
    source_path: PathBuf,
    path: PathBuf,
    _directory: tempfile::TempDir,
}

impl DatabaseSnapshot {
    /// The history path the snapshot was taken from — for labels and
    /// messages only; it is never opened.
    pub(crate) fn source_path(&self) -> &Path {
        &self.source_path
    }

    /// Open the snapshot read-only (the flags the importers have always used).
    pub(crate) fn open(&self) -> rusqlite::Result<Connection> {
        Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)
    }
}

/// [`verify_history_database`] and [`VerifiedDatabase::into_snapshot`] in
/// one step: the only way an importer gets a database to read.
pub(crate) fn snapshot_history_database(
    history_root: &Path,
    path: &Path,
) -> Result<DatabaseSnapshot, SourceRefusal> {
    verify_history_database(history_root, path)?.into_snapshot()
}

fn side_file(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn non_empty(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.len() > 0)
}

/// A fresh directory only this user can enter, inside lean-ctx's own data
/// directory.
fn snapshot_directory() -> std::io::Result<tempfile::TempDir> {
    let parent = super::scratch_parent()?;
    remove_stale_snapshots(&parent);
    let mut builder = tempfile::Builder::new();
    builder.prefix(SNAPSHOT_PREFIX);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir_in(parent)
}

/// Remove snapshot directories a killed import left behind: a copy of a
/// history database must not stay in the data directory.
fn remove_stale_snapshots(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .file_name()
            .to_string_lossy()
            .starts_with(SNAPSHOT_PREFIX)
            && entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age > STALE_SNAPSHOT));
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}
