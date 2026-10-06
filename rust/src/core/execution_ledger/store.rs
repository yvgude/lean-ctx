//! File-backed append-only execution ledger.
//! The append journal proves local crash continuity; signed canonical receipts,
//! not this unkeyed sidecar, provide adversarial authenticity.

use std::fs;
use std::fs::File;
#[cfg(test)]
use std::fs::OpenOptions;
use std::io::Write;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fs2::FileExt;
use serde::de::Error as _;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};
use sha2::{Digest, Sha256};

use super::event::ExecutionEvent;
use super::verify::verify_events;
use super::verify::{GENESIS, hash_event};
use super::{ExecutionLedgerError, Result};

mod platform;
#[cfg(not(unix))]
mod portable;
#[cfg(test)]
use platform::path_parts;
#[cfg(unix)]
use platform::{
    Operation, capture_relative_parent_identity, create_regular_operation, directory_identity,
    hard_link_operation, open_ledger_for_append_operation, open_nofollow_operation,
    open_regular_nofollow_operation, operation_stat, relative_sibling, remove_file_operation,
    unix_open_operation, unix_open_root, unlink_operation, validate_directory_identity,
    validate_operation_file,
};
#[cfg(unix)]
use platform::{capture_directory_identity, ensure_default_data_root};
#[cfg(not(unix))]
use portable::{capture_directory_identity, ensure_default_data_root};

const LEDGER_RECORD_SCHEMA: &str = "lean-ctx.execution-ledger-record.v1";
const LEDGER_RECORD_KIND: &str = "execution_event";
const APPEND_JOURNAL_SCHEMA: &str = "lean-ctx.execution-ledger-append-journal.v1";
const MAX_LEDGER_RECORD_BYTES: usize = 1024 * 1024;
const MAX_APPEND_JOURNAL_BYTES: usize = MAX_LEDGER_RECORD_BYTES * 2 + 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LedgerRecordV1 {
    schema: String,
    kind: String,
    event: ExecutionEvent,
    entry_hash: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AppendJournalV1 {
    schema: String,
    previous_len: u64,
    previous_sha256: String,
    record: String,
    record_sha256: String,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    dev: u64,
    ino: u64,
}

#[cfg(not(unix))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity;

/// Default execution-ledger location: `<data_dir>/execution/ledger.jsonl`.
pub fn default_path() -> Option<PathBuf> {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
    Some(data_dir.join("execution").join("ledger.jsonl"))
}

/// A process-independent handle to an execution ledger file.
#[derive(Debug)]
pub struct ExecutionLedgerStore {
    path: PathBuf,
    trusted_root: PathBuf,
    relative_path: PathBuf,
    trusted_root_identity: Option<DirectoryIdentity>,
    trusted_parent_identity: Arc<Mutex<Option<DirectoryIdentity>>>,
}

impl Clone for ExecutionLedgerStore {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            trusted_root: self.trusted_root.clone(),
            relative_path: self.relative_path.clone(),
            trusted_root_identity: self.trusted_root_identity,
            trusted_parent_identity: Arc::clone(&self.trusted_parent_identity),
        }
    }
}

impl PartialEq for ExecutionLedgerStore {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
            && self.trusted_root == other.trusted_root
            && self.relative_path == other.relative_path
            && self.trusted_root_identity == other.trusted_root_identity
    }
}

impl Eq for ExecutionLedgerStore {}

impl ExecutionLedgerStore {
    /// Creates a store backed by `path`.
    ///
    /// This compatibility constructor treats the lexical parent of `path` as
    /// the trusted root.  Every operation still opens each component from a
    /// descriptor with no-follow semantics; callers that receive an external
    /// path should prefer [`Self::new_verified`], which makes the root and
    /// relative path explicit and rejects traversal components up front.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let requested = path.into();
        let path = if requested.is_absolute() {
            requested
        } else {
            std::env::current_dir()
                .map(|directory| directory.join(&requested))
                .unwrap_or(requested)
        };
        let trusted_root = path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let relative_path = path
            .file_name()
            .map_or_else(|| PathBuf::from(""), PathBuf::from);
        let trusted_root_identity = capture_directory_identity(&trusted_root).ok();
        Self {
            path,
            trusted_root,
            relative_path,
            trusted_root_identity,
            trusted_parent_identity: Arc::new(Mutex::new(trusted_root_identity)),
        }
    }

    /// Creates a store rooted at `trusted_root` and addressed by `relative`.
    ///
    /// The root is never traversed through a symlink and `relative` may not be
    /// absolute or contain `..`; opened descriptors are checked again after
    /// acquisition so a concurrent rename cannot redirect a read.
    pub fn new_verified(
        trusted_root: impl Into<PathBuf>,
        relative: impl Into<PathBuf>,
    ) -> Result<Self> {
        let trusted_root = trusted_root.into();
        let relative_path = relative.into();
        validate_root_path(&trusted_root)?;
        validate_relative_path(&relative_path)?;
        if !trusted_root.is_absolute() {
            return Err(ExecutionLedgerError::InvalidRecord(
                "execution ledger trusted root must be absolute".to_owned(),
            ));
        }
        #[cfg(unix)]
        {
            let trusted_root = normalize_absolute_root_path(&trusted_root)?;
            let trusted_root_directory = unix_open_root(&trusted_root)?;
            let trusted_root_identity = directory_identity(&trusted_root_directory)?;
            let trusted_parent_identity =
                capture_relative_parent_identity(&trusted_root, &relative_path)?;
            Ok(Self {
                path: trusted_root.join(&relative_path),
                trusted_root,
                relative_path,
                trusted_root_identity: Some(trusted_root_identity),
                trusted_parent_identity: Arc::new(Mutex::new(trusted_parent_identity)),
            })
        }
        #[cfg(not(unix))]
        {
            portable::new_verified(trusted_root, relative_path)
        }
    }

    /// Creates a store at the configured default location.
    pub fn from_default() -> Result<Self> {
        let data_dir = crate::core::data_dir::lean_ctx_data_dir().map_err(|_| {
            ExecutionLedgerError::InvalidRecord("data directory unavailable".to_owned())
        })?;
        ensure_default_data_root(&data_dir)?;
        Self::new_verified(data_dir, Path::new("execution/ledger.jsonl"))
    }

    #[cfg(unix)]
    fn open_operation(&self, create_parent: bool) -> Result<Operation> {
        let expected_parent = *self.trusted_parent_identity.lock().map_err(|_| {
            ExecutionLedgerError::InvalidRecord(
                "execution ledger identity lock poisoned".to_owned(),
            )
        })?;
        unix_open_operation(
            &self.trusted_root,
            &self.relative_path,
            self.trusted_root_identity,
            expected_parent,
            create_parent,
        )
        .and_then(|operation| {
            let identity = directory_identity(&operation.parent)?;
            let mut expected = self.trusted_parent_identity.lock().map_err(|_| {
                ExecutionLedgerError::InvalidRecord(
                    "execution ledger identity lock poisoned".to_owned(),
                )
            })?;
            if let Some(previous) = *expected {
                if previous != identity {
                    return Err(ExecutionLedgerError::InvalidRecord(
                        "execution ledger parent changed while opening".to_owned(),
                    ));
                }
            } else {
                *expected = Some(identity);
            }
            Ok(operation)
        })
    }

    #[cfg(unix)]
    fn open_operation_for_read(&self) -> Result<Option<Operation>> {
        if self.trusted_root_identity.is_some() {
            // Validate the constructor-captured identity before attempting any
            // descriptor acquisition; a replaced root is never treated as empty.
            validate_directory_identity(&self.trusted_root, &unix_open_root(&self.trusted_root)?)?;
        }
        let parent_pinned = *self.trusted_parent_identity.lock().map_err(|_| {
            ExecutionLedgerError::InvalidRecord(
                "execution ledger identity lock poisoned".to_owned(),
            )
        })?;
        match self.open_operation(false) {
            Ok(operation) => Ok(Some(operation)),
            Err(ExecutionLedgerError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound && parent_pinned.is_none() =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// Returns the backing path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one event, assigning its next sequence number and chain link.
    pub fn append(&self, event: ExecutionEvent) -> Result<()> {
        self.append_if_new(event).map(drop)
    }

    /// Appends one event and reports whether durable state changed.
    pub(crate) fn append_if_new(&self, event: ExecutionEvent) -> Result<bool> {
        #[cfg(not(unix))]
        {
            portable::append_if_new(self, event)
        }
        #[cfg(unix)]
        {
            let mut event = event;
            let operation = self.open_operation(true)?;
            let mut file = open_ledger_for_append_operation(&operation)?;
            file.lock_exclusive()?;

            let result = (|| {
                validate_operation_file(
                    &operation,
                    &self.relative_path,
                    &file,
                    "execution ledger",
                    true,
                )?;
                recover_pending_append_operation(&mut file, &operation)?;
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
                        Err(ExecutionLedgerError::InvalidRecord(
                            "event identity already exists with different payload".to_owned(),
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
                    .ok_or_else(|| {
                        ExecutionLedgerError::InvalidRecord(
                            "execution ledger sequence number overflow".to_owned(),
                        )
                    })?;
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
                #[cfg(unix)]
                write_append_journal_operation(&operation, previous_len, &previous_sha256, &line)?;
                file.seek(SeekFrom::End(0))?;
                writeln!(file, "{line}")?;
                file.flush()?;
                file.sync_data()?;
                #[cfg(unix)]
                operation.sync_parent()?;
                #[cfg(unix)]
                validate_operation_file(
                    &operation,
                    &self.relative_path,
                    &file,
                    "execution ledger",
                    true,
                )?;
                #[cfg(unix)]
                clear_append_journal_operation(&operation)?;
                #[cfg(unix)]
                validate_operation_file(
                    &operation,
                    &self.relative_path,
                    &file,
                    "execution ledger",
                    true,
                )?;
                Ok(true)
            })();

            let _ = FileExt::unlock(&file);
            result
        }
    }

    /// Verifies the complete on-disk chain under a shared file lock.
    pub fn verify_chain(&self) -> Result<bool> {
        #[cfg(not(unix))]
        {
            portable::verify_chain(self)
        }
        #[cfg(unix)]
        {
            let Some(operation) = self.open_operation_for_read()? else {
                return Ok(true);
            };
            ensure_no_pending_append_operation(&operation)?;
            let Some(file) = open_regular_nofollow_operation(&operation, &self.relative_path)?
            else {
                return Ok(true);
            };
            file.lock_shared()?;
            let result = match ensure_no_pending_append_operation(&operation)
                .and_then(|()| read_events_from_file(&file))
            {
                Ok(events) => validate_operation_file(
                    &operation,
                    &self.relative_path,
                    &file,
                    "execution ledger",
                    true,
                )
                .and_then(|()| verify_events(&events)),
                Err(ExecutionLedgerError::InvalidChain(_)) => Ok(false),
                Err(error) => Err(error),
            };
            let _ = FileExt::unlock(&file);
            result
        }
    }

    /// Loads all well-formed events in file order.
    pub fn load(&self) -> Result<Vec<ExecutionEvent>> {
        #[cfg(not(unix))]
        {
            portable::load(self, false)
        }
        #[cfg(unix)]
        {
            let Some(operation) = self.open_operation_for_read()? else {
                return Ok(Vec::new());
            };
            ensure_no_pending_append_operation(&operation)?;
            let Some(file) = open_regular_nofollow_operation(&operation, &self.relative_path)?
            else {
                return Ok(Vec::new());
            };
            file.lock_shared()?;
            let result = ensure_no_pending_append_operation(&operation)
                .and_then(|()| read_events_from_file(&file))
                .and_then(|events| {
                    validate_operation_file(
                        &operation,
                        &self.relative_path,
                        &file,
                        "execution ledger",
                        true,
                    )?;
                    Ok(events)
                });
            let _ = FileExt::unlock(&file);
            result
        }
    }

    /// Loads one snapshot only when its record digests, chain, and lifecycle verify.
    pub fn load_verified(&self) -> Result<Vec<ExecutionEvent>> {
        #[cfg(not(unix))]
        {
            portable::load(self, true)
        }
        #[cfg(unix)]
        {
            let Some(operation) = self.open_operation_for_read()? else {
                return Ok(Vec::new());
            };
            ensure_no_pending_append_operation(&operation)?;
            let Some(file) = open_regular_nofollow_operation(&operation, &self.relative_path)?
            else {
                return Ok(Vec::new());
            };
            file.lock_shared()?;
            let result = ensure_no_pending_append_operation(&operation)
                .and_then(|()| read_events_from_file(&file))
                .and_then(|events| {
                    validate_operation_file(
                        &operation,
                        &self.relative_path,
                        &file,
                        "execution ledger",
                        true,
                    )?;
                    if verify_events(&events)? {
                        Ok(events)
                    } else {
                        Err(ExecutionLedgerError::InvalidChain(
                            "execution ledger chain or lifecycle is invalid".to_owned(),
                        ))
                    }
                });
            let _ = FileExt::unlock(&file);
            result
        }
    }

    /// Returns verified events associated with `task_id`, propagating unavailable state.
    pub fn by_task_verified(&self, task_id: &str) -> Result<Vec<ExecutionEvent>> {
        Ok(self
            .load_verified()?
            .into_iter()
            .filter(|event| event.task_id() == task_id)
            .collect())
    }

    /// Compatibility projection; exact consumers must use [`Self::by_task_verified`].
    #[must_use]
    pub fn by_task(&self, task_id: &str) -> Vec<ExecutionEvent> {
        self.by_task_verified(task_id).unwrap_or_default()
    }

    /// Returns the verified last sequence number, or zero for an empty/missing file.
    pub fn last_sequence_verified(&self) -> Result<u64> {
        Ok(self
            .load_verified()?
            .last()
            .map_or(0, ExecutionEvent::sequence_number))
    }

    /// Compatibility projection; exact consumers must use [`Self::last_sequence_verified`].
    #[must_use]
    pub fn last_sequence(&self) -> u64 {
        self.last_sequence_verified().unwrap_or(0)
    }
}

fn validate_root_path(root: &Path) -> Result<()> {
    if root.as_os_str().is_empty()
        || root
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(ExecutionLedgerError::InvalidRecord(
            "execution ledger trusted root is invalid".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn normalize_absolute_root_path(root: &Path) -> Result<PathBuf> {
    if !root.is_absolute() {
        return Err(ExecutionLedgerError::InvalidRecord(
            "execution ledger trusted root must be absolute".to_owned(),
        ));
    }
    let mut components = root.components();
    let mut normalized = PathBuf::from(std::path::MAIN_SEPARATOR.to_string());
    let first = components.next();
    let first_name = first.and_then(|component| match component {
        std::path::Component::RootDir => components.next(),
        _ => None,
    });
    if let Some(std::path::Component::Normal(name)) = first_name
        && (name == "var" || name == "tmp")
    {
        let alias = Path::new("/").join(name);
        if let Ok(target) = fs::read_link(alias) {
            if target.is_absolute() {
                normalized = target;
            } else {
                normalized.push(target);
            }
        } else {
            normalized.push(name);
        }
    } else if let Some(std::path::Component::Normal(name)) = first_name {
        normalized.push(name);
    }
    for component in components {
        match component {
            std::path::Component::Normal(name) => normalized.push(name),
            std::path::Component::CurDir | std::path::Component::RootDir => {}
            std::path::Component::ParentDir | std::path::Component::Prefix(_) => {
                return Err(ExecutionLedgerError::InvalidRecord(
                    "execution ledger trusted root is invalid".to_owned(),
                ));
            }
        }
    }
    Ok(normalized)
}

fn validate_relative_path(relative: &Path) -> Result<()> {
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        || !relative
            .components()
            .any(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return Err(ExecutionLedgerError::InvalidRecord(
            "execution ledger relative path is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn read_events_from_file(file: &File) -> Result<Vec<ExecutionEvent>> {
    let mut source = file.try_clone()?;
    source.seek(SeekFrom::Start(0))?;
    let (events, tail) = read_complete_events_and_tail(source)?;
    if !tail.is_empty() {
        return Err(ExecutionLedgerError::InvalidRecord(
            "execution ledger has an unterminated final record".to_owned(),
        ));
    }
    Ok(events)
}

#[cfg(unix)]
fn ensure_no_pending_append_operation(operation: &Operation) -> Result<()> {
    for suffix in [".append-journal", ".append-journal.tmp"] {
        let pending = relative_sibling(&operation.relative, suffix)?;
        if open_nofollow_operation(operation, &pending)?.is_some() {
            return Err(ExecutionLedgerError::InvalidRecord(
                "execution ledger has a pending append journal".to_owned(),
            ));
        }
    }
    Ok(())
}

fn read_complete_events_and_tail(source: impl Read) -> Result<(Vec<ExecutionEvent>, Vec<u8>)> {
    let mut reader = BufReader::new(source);
    let mut events = Vec::new();
    let mut line_number = 0;
    loop {
        let Some((mut line, terminated)) = read_bounded_line(&mut reader)? else {
            return Ok((events, Vec::new()));
        };
        if !terminated {
            return Ok((events, line));
        }
        line.pop();
        let line = std::str::from_utf8(&line).map_err(|_| {
            ExecutionLedgerError::InvalidRecord(format!(
                "ledger record at index {line_number} is not UTF-8"
            ))
        })?;
        if line.trim().is_empty() {
            return Err(ExecutionLedgerError::InvalidRecord(format!(
                "empty line at index {line_number}"
            )));
        }
        let event = parse_canonical_event(line)?;
        events.push(event);
        line_number += 1;
    }
}

fn read_bounded_line(reader: &mut impl BufRead) -> Result<Option<(Vec<u8>, bool)>> {
    let mut line = Vec::new();
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some((line, false)))
            };
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(buffer.len(), |index| index + 1);
        if line.len().saturating_add(consumed) > MAX_LEDGER_RECORD_BYTES + 1 {
            return Err(ExecutionLedgerError::InvalidRecord(
                "execution ledger record exceeds byte limit".to_owned(),
            ));
        }
        line.extend_from_slice(&buffer[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some((line, true)));
        }
    }
}

#[cfg(unix)]
fn recover_pending_append_operation(file: &mut File, operation: &Operation) -> Result<()> {
    let Some(journal) = read_append_journal_operation(operation)? else {
        if !file_is_newline_terminated(file)? {
            return Err(ExecutionLedgerError::InvalidRecord(
                "unterminated ledger tail has no matching append journal".to_owned(),
            ));
        }
        let temporary = relative_sibling(&operation.relative, ".append-journal.tmp")?;
        if remove_file_operation(operation, &temporary)? {
            operation.sync_parent()?;
        }
        return Ok(());
    };

    let current_len = file.metadata()?.len();
    if current_len < journal.previous_len {
        return Err(ExecutionLedgerError::InvalidRecord(
            "ledger is shorter than append journal predecessor".to_owned(),
        ));
    }
    if sha256_prefix(file, journal.previous_len)? != journal.previous_sha256 {
        return Err(ExecutionLedgerError::InvalidRecord(
            "ledger predecessor does not match append journal".to_owned(),
        ));
    }
    let mut prefix = file.try_clone()?;
    prefix.seek(SeekFrom::Start(0))?;
    let (mut previous_events, tail) =
        read_complete_events_and_tail(prefix.take(journal.previous_len))?;
    if !tail.is_empty() {
        return Err(ExecutionLedgerError::InvalidRecord(
            "append journal predecessor is not newline-terminated".to_owned(),
        ));
    }
    if !verify_events(&previous_events)? {
        return Err(ExecutionLedgerError::InvalidChain(
            "append journal predecessor chain is invalid".to_owned(),
        ));
    }
    previous_events.push(parse_canonical_event(&journal.record)?);
    if !verify_events(&previous_events)? {
        return Err(ExecutionLedgerError::InvalidChain(
            "prepared append violates execution lifecycle".to_owned(),
        ));
    }
    let mut expected = journal.record.into_bytes();
    expected.push(b'\n');
    let suffix_len = current_len - journal.previous_len;
    let expected_len = u64::try_from(expected.len()).map_err(|_| {
        ExecutionLedgerError::InvalidRecord("prepared append exceeds platform".to_owned())
    })?;
    if suffix_len > expected_len {
        return Err(ExecutionLedgerError::InvalidRecord(
            "ledger contains bytes after prepared append".to_owned(),
        ));
    }
    let suffix_len = usize::try_from(suffix_len).map_err(|_| {
        ExecutionLedgerError::InvalidRecord("prepared ledger suffix exceeds platform".to_owned())
    })?;
    let mut suffix = vec![0; suffix_len];
    let mut suffix_reader = file.try_clone()?;
    suffix_reader.seek(SeekFrom::Start(journal.previous_len))?;
    suffix_reader.read_exact(&mut suffix)?;
    if !expected.starts_with(&suffix) {
        return Err(ExecutionLedgerError::InvalidRecord(
            "ledger tail does not match prepared append".to_owned(),
        ));
    }
    if suffix_len < expected.len() {
        file.seek(SeekFrom::End(0))?;
        file.write_all(&expected[suffix_len..])?;
    }
    file.flush()?;
    file.sync_data()?;
    operation.sync_parent()?;
    validate_operation_file(
        operation,
        &operation.relative,
        file,
        "execution ledger",
        true,
    )?;
    if !verify_events(&read_events_from_file(file)?)? {
        return Err(ExecutionLedgerError::InvalidChain(
            "completed prepared append is invalid".to_owned(),
        ));
    }
    clear_append_journal_operation(operation)
}

fn file_is_newline_terminated(file: &File) -> Result<bool> {
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(true);
    }
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(len - 1))?;
    let mut last = [0];
    reader.read_exact(&mut last)?;
    Ok(last[0] == b'\n')
}

fn file_len_and_sha256(file: &File) -> Result<(u64, String)> {
    let len = file.metadata()?.len();
    Ok((len, sha256_prefix(file, len)?))
}

fn sha256_prefix(file: &File, len: u64) -> Result<String> {
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    let mut remaining = len;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    while remaining > 0 {
        let limit = usize::try_from(remaining)
            .unwrap_or(buffer.len())
            .min(buffer.len());
        let read = reader.read(&mut buffer[..limit])?;
        if read == 0 {
            return Err(ExecutionLedgerError::InvalidRecord(
                "ledger ended before append journal predecessor".to_owned(),
            ));
        }
        digest.update(&buffer[..read]);
        remaining -= u64::try_from(read).map_err(|_| {
            ExecutionLedgerError::InvalidRecord("ledger read length overflow".to_owned())
        })?;
    }
    Ok(encode_sha256(digest.finalize()))
}

#[cfg(test)]
fn append_journal_path(path: &Path) -> PathBuf {
    let mut journal = path.as_os_str().to_os_string();
    journal.push(".append-journal");
    PathBuf::from(journal)
}

#[cfg(test)]
fn append_journal_temp_path(path: &Path) -> PathBuf {
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(".append-journal.tmp");
    PathBuf::from(temporary)
}

#[cfg(test)]
fn write_append_journal(
    path: &Path,
    previous_len: u64,
    previous_sha256: &str,
    record: &str,
) -> Result<()> {
    let (root, relative) = path_parts(path)?;
    #[cfg(unix)]
    {
        let root_identity = capture_directory_identity(&root)?;
        let parent_identity = capture_relative_parent_identity(&root, &relative)?;
        let operation = unix_open_operation(
            &root,
            &relative,
            Some(root_identity),
            parent_identity,
            false,
        )?;
        write_append_journal_operation(&operation, previous_len, previous_sha256, record)
    }
    #[cfg(not(unix))]
    {
        portable::write_append_journal_path(
            &root.join(relative),
            previous_len,
            previous_sha256,
            record,
        )
    }
}

#[cfg(unix)]
fn write_append_journal_operation(
    operation: &Operation,
    previous_len: u64,
    previous_sha256: &str,
    record: &str,
) -> Result<()> {
    if record.len() > MAX_LEDGER_RECORD_BYTES {
        return Err(ExecutionLedgerError::InvalidRecord(
            "execution ledger record exceeds byte limit".to_owned(),
        ));
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
        return Err(ExecutionLedgerError::InvalidRecord(
            "append journal exceeds byte limit".to_owned(),
        ));
    }
    let temporary = relative_sibling(&operation.relative, ".append-journal.tmp")?;
    let journal = relative_sibling(&operation.relative, ".append-journal")?;
    if remove_file_operation(operation, &temporary)? {
        operation.sync_parent()?;
    }
    let mut file = create_regular_operation(operation, &temporary, "append journal temporary")?;
    let mut published = false;
    let result = (|| {
        operation.validate_paths()?;
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_all()?;
        validate_operation_file(
            operation,
            &temporary,
            &file,
            "append journal temporary",
            true,
        )?;
        hard_link_operation(operation, &temporary, &journal)?;
        published = true;
        operation.sync_parent()?;
        let journal_file = open_nofollow_operation(operation, &journal)?.ok_or_else(|| {
            ExecutionLedgerError::InvalidRecord("published append journal disappeared".to_owned())
        })?;
        validate_journal_link_pair_operation(
            operation,
            &journal,
            &temporary,
            &journal_file,
            &file,
        )?;
        unlink_operation(operation, &temporary)?;
        operation.sync_parent()
    })();
    if result.is_err() {
        if published {
            let _ = clear_append_journal_operation(operation);
        } else {
            let _ = remove_file_operation(operation, &temporary);
        }
    }
    result
}

fn read_bounded_file(mut file: File, max_bytes: usize, label: &str) -> Result<Vec<u8>> {
    let max_len = u64::try_from(max_bytes).map_err(|_| {
        ExecutionLedgerError::InvalidRecord(format!("{label} byte limit exceeds platform"))
    })?;
    if file.metadata()?.len() > max_len {
        return Err(ExecutionLedgerError::InvalidRecord(format!(
            "{label} exceeds byte limit"
        )));
    }
    let capacity = usize::try_from(file.metadata()?.len()).unwrap_or(max_bytes);
    let mut bytes = Vec::with_capacity(capacity.min(max_bytes));
    std::io::Read::by_ref(&mut file)
        .take(max_len.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(ExecutionLedgerError::InvalidRecord(format!(
            "{label} exceeds byte limit"
        )));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn validate_journal_link_pair_operation(
    operation: &Operation,
    journal_path: &Path,
    temporary_path: &Path,
    journal: &File,
    temporary: &File,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let journal_metadata = journal.metadata()?;
    let temporary_metadata = temporary.metadata()?;
    let same_inode = journal_metadata.dev() == temporary_metadata.dev()
        && journal_metadata.ino() == temporary_metadata.ino();
    if !journal_metadata.is_file()
        || !temporary_metadata.is_file()
        || journal_metadata.nlink() != 2
        || temporary_metadata.nlink() != 2
        || !same_inode
    {
        return Err(ExecutionLedgerError::InvalidRecord(
            "append journal link pair identity is invalid".to_owned(),
        ));
    }
    let journal_stat = operation_stat(operation, journal_path)?;
    let temporary_stat = operation_stat(operation, temporary_path)?;
    if journal_stat.st_mode as libc::mode_t & libc::S_IFMT != libc::S_IFREG
        || temporary_stat.st_mode as libc::mode_t & libc::S_IFMT != libc::S_IFREG
        || journal_stat.st_nlink as u64 != 2
        || temporary_stat.st_nlink as u64 != 2
        || journal_stat.st_dev as u64 != journal_metadata.dev()
        || journal_stat.st_ino as u64 != journal_metadata.ino()
        || temporary_stat.st_dev as u64 != journal_metadata.dev()
        || temporary_stat.st_ino as u64 != journal_metadata.ino()
    {
        return Err(ExecutionLedgerError::InvalidRecord(
            "append journal link pair changed while reading".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn read_append_journal_operation(operation: &Operation) -> Result<Option<AppendJournalV1>> {
    let journal = relative_sibling(&operation.relative, ".append-journal")?;
    let temporary = relative_sibling(&operation.relative, ".append-journal.tmp")?;
    let Some(file) = open_nofollow_operation(operation, &journal)? else {
        return Ok(None);
    };
    let bytes = read_bounded_file(
        file.try_clone()?,
        MAX_APPEND_JOURNAL_BYTES,
        "append journal",
    )?;
    use std::os::unix::fs::MetadataExt;
    if file.metadata()?.nlink() == 2 {
        let Some(temp_file) = open_nofollow_operation(operation, &temporary)? else {
            return Err(ExecutionLedgerError::InvalidRecord(
                "append journal link pair is incomplete".to_owned(),
            ));
        };
        let temp_bytes = read_bounded_file(
            temp_file.try_clone()?,
            MAX_APPEND_JOURNAL_BYTES,
            "append journal temporary",
        )?;
        if bytes != temp_bytes {
            return Err(ExecutionLedgerError::InvalidRecord(
                "append journal link pair content differs".to_owned(),
            ));
        }
        validate_journal_link_pair_operation(operation, &journal, &temporary, &file, &temp_file)?;
    } else {
        validate_operation_file(operation, &journal, &file, "append journal", true)?;
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        ExecutionLedgerError::InvalidRecord("append journal is not UTF-8".to_owned())
    })?;
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let value = StrictJson::deserialize(&mut deserializer)
        .map_err(serde_json::Error::custom)?
        .into_value();
    deserializer.end()?;
    let journal: AppendJournalV1 = serde_json::from_value(value)?;
    if journal.schema != APPEND_JOURNAL_SCHEMA
        || serde_json::to_vec(&journal)? != bytes
        || journal.record.len() > MAX_LEDGER_RECORD_BYTES
        || sha256(journal.record.as_bytes()) != journal.record_sha256
        || parse_canonical_event(&journal.record).is_err()
    {
        return Err(ExecutionLedgerError::InvalidRecord(
            "append journal is invalid or non-canonical".to_owned(),
        ));
    }
    Ok(Some(journal))
}

#[cfg(unix)]
fn clear_append_journal_operation(operation: &Operation) -> Result<()> {
    let journal = relative_sibling(&operation.relative, ".append-journal")?;
    let temporary = relative_sibling(&operation.relative, ".append-journal.tmp")?;
    if let Some(journal_file) = open_nofollow_operation(operation, &journal)? {
        use std::os::unix::fs::MetadataExt;
        if journal_file.metadata()?.nlink() == 2 {
            let temporary_file =
                open_nofollow_operation(operation, &temporary)?.ok_or_else(|| {
                    ExecutionLedgerError::InvalidRecord(
                        "append journal link pair is incomplete".to_owned(),
                    )
                })?;
            validate_journal_link_pair_operation(
                operation,
                &journal,
                &temporary,
                &journal_file,
                &temporary_file,
            )?;
            unlink_operation(operation, &journal)?;
            unlink_operation(operation, &temporary)?;
        } else {
            remove_file_operation(operation, &journal)?;
            let _ = remove_file_operation(operation, &temporary)?;
        }
    } else {
        let _ = remove_file_operation(operation, &temporary)?;
    }
    operation.sync_parent()
}

fn sha256(bytes: &[u8]) -> String {
    encode_sha256(Sha256::digest(bytes))
}

fn encode_sha256(digest: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in digest.as_ref() {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

enum StrictJson {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Self>),
    Object(serde_json::Map<String, Value>),
}

impl StrictJson {
    fn into_value(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(value) => Value::Bool(value),
            Self::Number(value) => Value::Number(value),
            Self::String(value) => Value::String(value),
            Self::Array(values) => Value::Array(values.into_iter().map(Self::into_value).collect()),
            Self::Object(value) => Value::Object(value),
        }
    }
}

impl<'de> Deserialize<'de> for StrictJson {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StrictVisitor;

        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = StrictJson;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("strict JSON without duplicate keys or floats")
            }

            fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(StrictJson::Null)
            }

            fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(StrictJson::Null)
            }

            fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
                Ok(StrictJson::Bool(value))
            }

            fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
                Ok(StrictJson::Number(Number::from(value)))
            }

            fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
                Ok(StrictJson::Number(Number::from(value)))
            }

            fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                Err(E::custom("floating-point ledger numbers are forbidden"))
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
                Ok(StrictJson::String(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
                Ok(StrictJson::String(value))
            }

            fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element::<StrictJson>()? {
                    values.push(value);
                }
                Ok(StrictJson::Array(values))
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom(format!(
                            "duplicate ledger JSON key '{key}'"
                        )));
                    }
                    values.insert(key, map.next_value::<StrictJson>()?.into_value());
                }
                Ok(StrictJson::Object(values))
            }
        }

        deserializer.deserialize_any(StrictVisitor)
    }
}

fn parse_canonical_event(line: &str) -> Result<ExecutionEvent> {
    let mut deserializer = serde_json::Deserializer::from_str(line);
    let value = StrictJson::deserialize(&mut deserializer)
        .map_err(serde_json::Error::custom)?
        .into_value();
    deserializer.end()?;
    let record: LedgerRecordV1 = serde_json::from_value(value)?;
    if record.schema != LEDGER_RECORD_SCHEMA || record.kind != LEDGER_RECORD_KIND {
        return Err(ExecutionLedgerError::InvalidRecord(
            "unsupported execution ledger record schema or kind".to_owned(),
        ));
    }
    if serde_json::to_string(&record)? != line {
        return Err(ExecutionLedgerError::InvalidRecord(
            "execution ledger record is not canonical JSON".to_owned(),
        ));
    }
    if hash_event(&record.event)? != record.entry_hash {
        return Err(ExecutionLedgerError::InvalidChain(
            "execution ledger record digest mismatch".to_owned(),
        ));
    }
    Ok(record.event)
}

#[cfg(test)]
mod tests;
