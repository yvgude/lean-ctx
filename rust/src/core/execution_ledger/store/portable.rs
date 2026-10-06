//! Path-based compatibility backend for platforms without Unix `openat`.
//!
//! Every operation validates the trusted root, relative parent components,
//! and leaf metadata before and after I/O. Unix keeps the stronger
//! descriptor-relative backend in `platform`.

use super::{
    APPEND_JOURNAL_SCHEMA, AppendJournalV1, Arc, DirectoryIdentity, ExecutionEvent,
    ExecutionLedgerError, ExecutionLedgerStore, File, FileExt, GENESIS, LEDGER_RECORD_KIND,
    LEDGER_RECORD_SCHEMA, LedgerRecordV1, MAX_APPEND_JOURNAL_BYTES, MAX_LEDGER_RECORD_BYTES, Mutex,
    Path, PathBuf, Read, Result, Seek, SeekFrom, Write, file_is_newline_terminated,
    file_len_and_sha256, fs, hash_event, parse_canonical_event, read_bounded_file,
    read_complete_events_and_tail, read_events_from_file, sha256, sha256_prefix,
    validate_relative_path, validate_root_path, verify_events,
};
#[cfg(windows)]
use std::ffi::c_void;
use std::fs::OpenOptions;
#[cfg(windows)]
use std::mem::size_of;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle};
#[cfg(windows)]
use std::ptr::{null, null_mut};
#[cfg(windows)]
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_DISPOSITION_DELETE, FILE_DISPOSITION_INFORMATION_EX, FileDispositionInformationEx,
    NtSetInformationFile,
};
#[cfg(windows)]
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FileAttributeTagInfo, GetFileInformationByHandleEx, GetFinalPathNameByHandleW,
    GetLongPathNameW, OPEN_EXISTING, SYNCHRONIZE, VOLUME_NAME_DOS,
};
#[cfg(windows)]
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

pub(super) fn capture_directory_identity(path: &Path) -> Result<DirectoryIdentity> {
    validate_directory(path)?;
    Ok(DirectoryIdentity)
}

pub(super) fn ensure_default_data_root(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(invalid("execution ledger data root must be absolute"));
    }
    fs::create_dir_all(path)?;
    validate_directory(path)
}

pub(super) fn new_verified(
    trusted_root: PathBuf,
    relative_path: PathBuf,
) -> Result<ExecutionLedgerStore> {
    validate_root_path(&trusted_root)?;
    validate_relative_path(&relative_path)?;
    if !trusted_root.is_absolute() {
        return Err(invalid("execution ledger trusted root must be absolute"));
    }
    validate_directory(&trusted_root)?;
    let parent = relative_path.parent().unwrap_or_else(|| Path::new(""));
    let parent_identity = if parent.as_os_str().is_empty() {
        Some(DirectoryIdentity)
    } else {
        validate_relative_directories(&trusted_root, parent, false)?.then_some(DirectoryIdentity)
    };
    Ok(ExecutionLedgerStore {
        path: trusted_root.join(&relative_path),
        trusted_root,
        relative_path,
        trusted_root_identity: Some(DirectoryIdentity),
        trusted_parent_identity: Arc::new(Mutex::new(parent_identity)),
    })
}

pub(super) fn append_if_new(
    store: &ExecutionLedgerStore,
    mut event: ExecutionEvent,
) -> Result<bool> {
    validate_store_parent(store, true)?;
    let mut file = match secure_open_options()
        .read(true)
        .append(true)
        .open(&store.path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ensure_no_pending_append(&store.path)?;
            let file = secure_open_options()
                .create_new(true)
                .read(true)
                .append(true)
                .open(&store.path)?;
            if let Err(error) = ensure_no_pending_append(&store.path) {
                drop(file);
                remove_checked(&store.path)?;
                return Err(error);
            }
            file
        }
        Err(error) => return Err(error.into()),
    };
    validate_regular_file(&store.path, &file, "execution ledger")?;
    file.lock_exclusive()?;
    let result = (|| {
        recover_pending_append(&mut file, store)?;
        let events = read_events_from_file(&file)?;
        if !verify_events(&events)? {
            return Err(ExecutionLedgerError::InvalidChain(
                "cannot append to an invalid chain".to_owned(),
            ));
        }
        if let Some(identity) = event.idempotency_key()
            && let Some(existing) = events
                .iter()
                .find(|existing| existing.idempotency_key() == Some(identity))
        {
            return if existing.payload_json()? == event.payload_json()? {
                Ok(false)
            } else {
                Err(invalid(
                    "event identity already exists with different payload",
                ))
            };
        }

        let previous_hash = match events.last() {
            Some(previous) => hash_event(previous)?,
            None => GENESIS.to_owned(),
        };
        let sequence_number = events
            .last()
            .map_or(1, ExecutionEvent::sequence_number)
            .checked_add(u64::from(!events.is_empty()))
            .ok_or_else(|| invalid("execution ledger sequence number overflow"))?;
        event.set_chain_fields(sequence_number, previous_hash);
        let entry_hash = hash_event(&event)?;
        event.set_entry_hash(entry_hash.clone());
        let mut candidate = events;
        candidate.push(event);
        if !verify_events(&candidate)? {
            return Err(ExecutionLedgerError::InvalidChain(
                "event violates execution lifecycle".to_owned(),
            ));
        }
        let event = candidate.pop().expect("candidate contains appended event");
        let line = serde_json::to_string(&LedgerRecordV1 {
            schema: LEDGER_RECORD_SCHEMA.to_owned(),
            kind: LEDGER_RECORD_KIND.to_owned(),
            event,
            entry_hash,
        })?;
        let (previous_len, previous_sha256) = file_len_and_sha256(&file)?;
        write_append_journal_path(&store.path, previous_len, &previous_sha256, &line)?;
        file.seek(SeekFrom::End(0))?;
        writeln!(file, "{line}")?;
        file.flush()?;
        file.sync_data()?;
        validate_store_parent(store, false)?;
        validate_regular_file(&store.path, &file, "execution ledger")?;
        clear_append_journal(&store.path)?;
        validate_regular_file(&store.path, &file, "execution ledger")?;
        Ok(true)
    })();
    let _ = FileExt::unlock(&file);
    result
}

pub(super) fn verify_chain(store: &ExecutionLedgerStore) -> Result<bool> {
    match load(store, false) {
        Ok(events) => verify_events(&events),
        Err(ExecutionLedgerError::InvalidChain(_)) => Ok(false),
        Err(error) => Err(error),
    }
}

pub(super) fn load(
    store: &ExecutionLedgerStore,
    require_verified: bool,
) -> Result<Vec<ExecutionEvent>> {
    validate_store_parent(store, false)?;
    ensure_no_pending_append(&store.path)?;
    let file = match secure_open_options().read(true).open(&store.path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    validate_regular_file(&store.path, &file, "execution ledger")?;
    file.lock_shared()?;
    let result = read_events_from_file(&file).and_then(|events| {
        validate_regular_file(&store.path, &file, "execution ledger")?;
        if require_verified && !verify_events(&events)? {
            return Err(ExecutionLedgerError::InvalidChain(
                "execution ledger chain or lifecycle is invalid".to_owned(),
            ));
        }
        Ok(events)
    });
    let _ = FileExt::unlock(&file);
    result
}

pub(super) fn write_append_journal_path(
    path: &Path,
    previous_len: u64,
    previous_sha256: &str,
    record: &str,
) -> Result<()> {
    if record.len() > MAX_LEDGER_RECORD_BYTES {
        return Err(invalid("execution ledger record exceeds byte limit"));
    }
    let journal = AppendJournalV1 {
        schema: APPEND_JOURNAL_SCHEMA.to_owned(),
        previous_len,
        previous_sha256: previous_sha256.to_owned(),
        record: record.to_owned(),
        record_sha256: sha256(record.as_bytes()),
    };
    let bytes = serde_json::to_vec(&journal)?;
    if bytes.len() > MAX_APPEND_JOURNAL_BYTES {
        return Err(invalid("append journal exceeds byte limit"));
    }
    let published = sibling(path, ".append-journal")?;
    let mut file = secure_open_options()
        .create_new(true)
        .write(true)
        .open(&published)?;
    let result = (|| {
        validate_regular_file(&published, &file, "append journal")?;
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_all()?;
        validate_regular_file(&published, &file, "append journal")?;
        let journal_file = secure_open_options().read(true).open(&published)?;
        if read_bounded_file(journal_file, MAX_APPEND_JOURNAL_BYTES, "append journal")? != bytes {
            return Err(invalid("published append journal changed while opening"));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = remove_checked(&published);
    }
    result
}

fn recover_pending_append(file: &mut File, store: &ExecutionLedgerStore) -> Result<()> {
    let Some(journal) = read_append_journal(&store.path)? else {
        if !file_is_newline_terminated(file)? {
            return Err(invalid(
                "unterminated ledger tail has no matching append journal",
            ));
        }
        remove_checked(&sibling(&store.path, ".append-journal.tmp")?)?;
        return Ok(());
    };
    let current_len = file.metadata()?.len();
    if current_len < journal.previous_len {
        return Err(invalid("ledger is shorter than append journal predecessor"));
    }
    if sha256_prefix(file, journal.previous_len)? != journal.previous_sha256 {
        return Err(invalid("ledger predecessor does not match append journal"));
    }
    let mut prefix = file.try_clone()?;
    prefix.seek(SeekFrom::Start(0))?;
    let (mut events, tail) = read_complete_events_and_tail(prefix.take(journal.previous_len))?;
    if !tail.is_empty() {
        return Err(invalid(
            "append journal predecessor is not newline-terminated",
        ));
    }
    if !verify_events(&events)? {
        return Err(ExecutionLedgerError::InvalidChain(
            "append journal predecessor chain is invalid".to_owned(),
        ));
    }
    events.push(parse_canonical_event(&journal.record)?);
    if !verify_events(&events)? {
        return Err(ExecutionLedgerError::InvalidChain(
            "prepared append violates execution lifecycle".to_owned(),
        ));
    }
    let mut expected = journal.record.into_bytes();
    expected.push(b'\n');
    let suffix_len = current_len - journal.previous_len;
    let expected_len =
        u64::try_from(expected.len()).map_err(|_| invalid("prepared append exceeds platform"))?;
    if suffix_len > expected_len {
        return Err(invalid("ledger contains bytes after prepared append"));
    }
    let suffix_len = usize::try_from(suffix_len)
        .map_err(|_| invalid("prepared ledger suffix exceeds platform"))?;
    let mut suffix = vec![0; suffix_len];
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(journal.previous_len))?;
    reader.read_exact(&mut suffix)?;
    if !expected.starts_with(&suffix) {
        return Err(invalid("ledger tail does not match prepared append"));
    }
    if suffix_len < expected.len() {
        file.seek(SeekFrom::End(0))?;
        file.write_all(&expected[suffix_len..])?;
    }
    file.flush()?;
    file.sync_data()?;
    validate_store_parent(store, false)?;
    validate_regular_file(&store.path, file, "execution ledger")?;
    if !verify_events(&read_events_from_file(file)?)? {
        return Err(ExecutionLedgerError::InvalidChain(
            "completed prepared append is invalid".to_owned(),
        ));
    }
    clear_append_journal(&store.path)
}

fn read_append_journal(path: &Path) -> Result<Option<AppendJournalV1>> {
    let journal_path = sibling(path, ".append-journal")?;
    let file = match secure_open_options().read(true).open(&journal_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    validate_regular_file(&journal_path, &file, "append journal")?;
    let bytes = read_bounded_file(file, MAX_APPEND_JOURNAL_BYTES, "append journal")?;
    let journal: AppendJournalV1 = serde_json::from_slice(&bytes)?;
    if journal.schema != APPEND_JOURNAL_SCHEMA
        || journal.record.len() > MAX_LEDGER_RECORD_BYTES
        || journal.record_sha256 != sha256(journal.record.as_bytes())
        || !journal.previous_sha256.starts_with("sha256:")
    {
        return Err(invalid("append journal is invalid"));
    }
    Ok(Some(journal))
}

fn clear_append_journal(path: &Path) -> Result<()> {
    remove_checked(&sibling(path, ".append-journal")?)?;
    remove_checked(&sibling(path, ".append-journal.tmp")?)
}

fn ensure_no_pending_append(path: &Path) -> Result<()> {
    for suffix in [".append-journal", ".append-journal.tmp"] {
        let candidate = sibling(path, suffix)?;
        match fs::symlink_metadata(&candidate) {
            Ok(_) => return Err(invalid("execution ledger has a pending append journal")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn validate_store_parent(store: &ExecutionLedgerStore, create: bool) -> Result<()> {
    validate_directory(&store.trusted_root)?;
    let parent = store
        .relative_path
        .parent()
        .unwrap_or_else(|| Path::new(""));
    validate_relative_directories(&store.trusted_root, parent, create).map(drop)
}

fn validate_relative_directories(root: &Path, relative: &Path, create: bool) -> Result<bool> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(invalid("execution ledger relative path is invalid"));
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(_) => validate_directory(&current)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                fs::create_dir(&current)?;
                validate_directory(&current)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(true)
}

fn validate_directory(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let directory = open_windows_path(
            path,
            FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_FLAG_OPEN_REPARSE_POINT
                | windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS,
        )?;
        validate_windows_handle(path, &directory, true)?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(invalid("execution ledger trusted path is not a directory"));
        }
        Ok(())
    }
}

fn validate_regular_file(path: &Path, file: &File, label: &str) -> Result<()> {
    #[cfg(windows)]
    {
        validate_windows_handle(path, file, false)
            .map_err(|_| invalid(&format!("{label} changed while opening")))
    }
    #[cfg(not(windows))]
    {
        let path_metadata = fs::symlink_metadata(path)?;
        let file_metadata = file.metadata()?;
        if path_metadata.file_type().is_symlink()
            || !path_metadata.is_file()
            || !file_metadata.is_file()
            || path_metadata.len() != file_metadata.len()
        {
            return Err(invalid(&format!("{label} changed while opening")));
        }
        Ok(())
    }
}

fn remove_checked(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        let file = open_windows_path(
            path,
            DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_FLAG_OPEN_REPARSE_POINT,
        )?;
        validate_windows_handle(path, &file, false)?;
        mark_windows_delete(&file)
    }
    #[cfg(not(windows))]
    {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
                invalid("execution ledger cleanup target is not a regular file"),
            ),
            Ok(_) => fs::remove_file(path).map_err(Into::into),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn secure_open_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(windows)]
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    options
}

#[cfg(windows)]
fn open_windows_path(path: &Path, access: u32, flags: u32) -> Result<File> {
    let path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: path is NUL-terminated and the returned handle is owned below.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            flags,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: successful CreateFileW handle transfers to File exactly once.
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[cfg(windows)]
fn validate_windows_handle(path: &Path, file: &File, directory: bool) -> Result<()> {
    let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: file handle is live and tag points to writable storage.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileAttributeTagInfo,
            (&raw mut tag).cast::<c_void>(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    let is_directory = tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    if ok == 0
        || tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || is_directory != directory
        || !windows_handle_matches_path(file, path)?
    {
        return Err(invalid("execution ledger handle identity is invalid"));
    }
    Ok(())
}

#[cfg(windows)]
fn windows_handle_matches_path(file: &File, expected: &Path) -> Result<bool> {
    let mut buffer = vec![0_u16; 512];
    let actual = loop {
        let capacity = u32::try_from(buffer.len())
            .map_err(|_| invalid("execution ledger path exceeds platform"))?;
        // SAFETY: file handle is live and buffer is writable for capacity units.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                capacity,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        if length == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let length = usize::try_from(length)
            .map_err(|_| invalid("execution ledger path exceeds platform"))?;
        if length < buffer.len() {
            buffer.truncate(length);
            break normalize_windows_path(buffer);
        }
        buffer.resize(
            length
                .checked_add(1)
                .ok_or_else(|| invalid("execution ledger path exceeds platform"))?,
            0,
        );
    };
    let expected = long_windows_path(expected)?;
    Ok(actual.len() == expected.len()
        && actual.iter().zip(expected).all(|(left, right)| {
            match (u8::try_from(*left), u8::try_from(right)) {
                (Ok(left_byte), Ok(right_byte)) => left_byte.eq_ignore_ascii_case(&right_byte),
                _ => *left == right,
            }
        }))
}

#[cfg(windows)]
fn long_windows_path(path: &Path) -> Result<Vec<u16>> {
    let source: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut buffer = vec![0_u16; 512];
    loop {
        let capacity = u32::try_from(buffer.len())
            .map_err(|_| invalid("execution ledger path exceeds platform"))?;
        // SAFETY: source is NUL-terminated and buffer is writable.
        let length = unsafe { GetLongPathNameW(source.as_ptr(), buffer.as_mut_ptr(), capacity) };
        if length == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let length = usize::try_from(length)
            .map_err(|_| invalid("execution ledger path exceeds platform"))?;
        if length < buffer.len() {
            buffer.truncate(length);
            return Ok(normalize_windows_path(buffer));
        }
        buffer.resize(
            length
                .checked_add(1)
                .ok_or_else(|| invalid("execution ledger path exceeds platform"))?,
            0,
        );
    }
}

#[cfg(windows)]
fn normalize_windows_path(mut path: Vec<u16>) -> Vec<u16> {
    const VERBATIM: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    const VERBATIM_UNC: &[u16] = &[
        b'\\' as u16,
        b'\\' as u16,
        b'?' as u16,
        b'\\' as u16,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        b'\\' as u16,
    ];
    if path.starts_with(VERBATIM_UNC) {
        path.splice(..VERBATIM_UNC.len(), [b'\\' as u16, b'\\' as u16]);
    } else if path.starts_with(VERBATIM) {
        path.drain(..VERBATIM.len());
    }
    for unit in &mut path {
        if *unit == b'/' as u16 {
            *unit = b'\\' as u16;
        }
    }
    path
}

#[cfg(windows)]
fn mark_windows_delete(file: &File) -> Result<()> {
    let mut info = FILE_DISPOSITION_INFORMATION_EX {
        Flags: FILE_DISPOSITION_DELETE,
    };
    let mut status = IO_STATUS_BLOCK::default();
    // SAFETY: file handle has DELETE access and both buffers are valid.
    let result = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &raw mut status,
            (&raw mut info).cast::<c_void>(),
            size_of::<FILE_DISPOSITION_INFORMATION_EX>() as u32,
            FileDispositionInformationEx,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(invalid("execution ledger cleanup failed"))
    }
}

fn sibling(path: &Path, suffix: &str) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .ok_or_else(|| invalid("execution ledger path has no file name"))?;
    let mut name = file_name.to_os_string();
    name.push(suffix);
    let mut sibling = path.to_path_buf();
    sibling.set_file_name(name);
    Ok(sibling)
}

fn invalid(message: &str) -> ExecutionLedgerError {
    ExecutionLedgerError::InvalidRecord(message.to_owned())
}
