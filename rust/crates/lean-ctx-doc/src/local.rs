// SPDX-License-Identifier: Apache-2.0

//! A project's docs without Hive: an append-only journal of the same C0 2.2
//! doc events a vault holds (`node.add`, `field.set`, `edge.add`,
//! `retract`), folded with [`crate::replica::assemble`], so a local page reads
//! exactly like a vault page.
//!
//! The journal is one JSON object per line (`journal.jsonl` in the store
//! directory the caller names). Each write is one revision: its events share
//! a `rev`, a time and an author. Nothing is ever rewritten; history is the
//! journal itself, and the page as of any revision is the fold of the
//! journal up to it. Writes take an exclusive file lock and plan against the
//! state they read under it, so two writers never build on a stale head.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::model::{Note, Page, PageStatus, Section};
use crate::replica::{DocSpace, LogEvent, assemble};
use crate::write::EventSpec;

/// The journal's file name inside the store directory.
pub const JOURNAL: &str = "journal.jsonl";
/// A store refuses writes once its journal reaches this size, and refuses to
/// read a larger one: a project's docs stay a bounded read.
pub const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;
/// The most events one revision may carry.
pub const MAX_EVENTS_PER_REVISION: usize = 4096;
/// The longest journal line a read accepts.
const MAX_LINE_BYTES: usize = 256 * 1024;

/// Who wrote a revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    /// `agent` or `person`.
    pub kind: String,
    /// The agent id or the person's name, as the writer gave it.
    pub label: String,
}

impl Author {
    pub fn agent(label: impl Into<String>) -> Self {
        Self {
            kind: "agent".into(),
            label: label.into(),
        }
    }

    pub fn person(label: impl Into<String>) -> Self {
        Self {
            kind: "person".into(),
            label: label.into(),
        }
    }
}

/// One journal line: a doc event and the revision it belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// The revision (one write); 1-based, increasing.
    pub rev: u64,
    /// The event's position in the journal; 1-based, increasing.
    pub seq: u64,
    /// RFC 3339 UTC time of the revision.
    pub at: String,
    pub author: Author,
    /// What the write was (`write`, `edit`, `note`, `move`, `delete`, …).
    pub action: String,
    pub op: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_type: Option<String>,
    pub body: Map<String, Value>,
}

impl Entry {
    /// The event id a later CAS write names as its base.
    pub fn event_id(&self) -> String {
        format!("local_{:012}", self.seq)
    }

    fn log_event(&self) -> LogEvent {
        LogEvent {
            seq: self.seq,
            event_id: self.event_id(),
            op: self.op.clone(),
            target: Some(self.target.clone()),
            node_type: self.node_type.clone(),
            body: Some(Value::Object(self.body.clone())),
        }
    }
}

/// Why the store refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalError {
    Io(String),
    /// Line `line` (1-based) of the journal is not a journal entry.
    Corrupt {
        line: usize,
    },
    /// The journal reached [`MAX_JOURNAL_BYTES`].
    Full,
    /// A revision with too many events.
    TooLarge(usize),
}

impl std::fmt::Display for LocalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "local docs: {e}"),
            Self::Corrupt { line } => write!(f, "local docs: journal line {line} is unreadable"),
            Self::Full => write!(
                f,
                "local docs: the journal reached {} MiB",
                MAX_JOURNAL_BYTES / (1024 * 1024)
            ),
            Self::TooLarge(n) => write!(
                f,
                "local docs: {n} events in one write (at most {MAX_EVENTS_PER_REVISION})"
            ),
        }
    }
}

impl std::error::Error for LocalError {}

fn io(e: &std::io::Error) -> LocalError {
    LocalError::Io(e.to_string())
}

/// One revision of a page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Revision {
    pub rev: u64,
    pub at: String,
    pub author: Author,
    pub action: String,
    /// The events of the revision that touch the page.
    pub events: usize,
}

/// What a write did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    /// The new revision; `None` when the plan had nothing to write.
    pub rev: Option<u64>,
    pub events: usize,
}

/// The doc store of one project, at a directory the caller chose.
#[derive(Debug, Clone)]
pub struct LocalDocs {
    dir: PathBuf,
}

impl LocalDocs {
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn journal(&self) -> PathBuf {
        self.dir.join(JOURNAL)
    }

    /// Every entry, in journal order; empty when the store was never written.
    ///
    /// # Errors
    /// An unreadable or oversized journal, or a line that is not an entry.
    pub fn entries(&self) -> Result<Vec<Entry>, LocalError> {
        match File::open(self.journal()) {
            Ok(mut f) => read_entries(&mut f),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(io(&e)),
        }
    }

    /// The pages as of now.
    ///
    /// # Errors
    /// As [`Self::entries`].
    pub fn space(&self) -> Result<DocSpace, LocalError> {
        Ok(space_of(&self.entries()?, None, &[]))
    }

    /// The live pages as [`Page`] values, by slug: what a vault takes on
    /// link ([`crate::write::create_page`]). Section bodies keep their
    /// storage form (`hive:` links name local page ids).
    ///
    /// # Errors
    /// As [`Self::entries`].
    pub fn pages(&self) -> Result<Vec<Page>, LocalError> {
        Ok(pages_of(&self.space()?))
    }

    /// Runs `plan` against the current pages under the store's lock and
    /// appends its events as one revision by `author` at `at`.
    ///
    /// # Errors
    /// `plan`'s own error; a full journal or too many events; I/O.
    pub fn commit<E: From<LocalError>>(
        &self,
        author: &Author,
        at: &str,
        action: &str,
        plan: impl FnOnce(&DocSpace) -> Result<Vec<EventSpec>, E>,
    ) -> Result<Committed, E> {
        create_private_dir(&self.dir)?;
        let mut file = open_journal(&self.journal())?;
        let _lock = Lock::exclusive(&file)?;
        let len = file.metadata().map_err(|e| io(&e))?.len();
        if len >= MAX_JOURNAL_BYTES {
            return Err(LocalError::Full.into());
        }
        file.seek(SeekFrom::Start(0)).map_err(|e| io(&e))?;
        let entries = read_entries(&mut file)?;
        let mut specs = plan(&space_of(&entries, None, &[]))?;
        if specs.is_empty() {
            return Ok(Committed {
                rev: None,
                events: 0,
            });
        }
        if specs.len() > MAX_EVENTS_PER_REVISION {
            return Err(LocalError::TooLarge(specs.len()).into());
        }
        let rev = entries.last().map_or(1, |e| e.rev + 1);
        let first = entries.last().map_or(1, |e| e.seq + 1);
        let ids: Vec<String> = (0..specs.len())
            .map(|i| format!("local_{:012}", first + i as u64))
            .collect();
        crate::write::chain(&mut specs, &ids);
        let mut out = String::new();
        for (i, spec) in specs.iter().enumerate() {
            let entry = Entry {
                rev,
                seq: first + i as u64,
                at: at.to_owned(),
                author: author.clone(),
                action: action.to_owned(),
                op: spec.op.to_owned(),
                target: spec.target.clone(),
                node_type: spec.node_type.map(str::to_owned),
                body: spec.body.clone(),
            };
            let line = serde_json::to_string(&entry).map_err(|e| LocalError::Io(e.to_string()))?;
            out.push_str(&line);
            out.push('\n');
        }
        if len + out.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(LocalError::Full.into());
        }
        // A torn tail (a crash mid-append) is dropped before the next append,
        // so the journal stays one entry per line.
        let keep = complete_len(&mut file, len)?;
        if keep != len {
            file.set_len(keep).map_err(|e| io(&e))?;
        }
        file.seek(SeekFrom::Start(keep)).map_err(|e| io(&e))?;
        file.write_all(out.as_bytes()).map_err(|e| io(&e))?;
        file.sync_data().map_err(|e| io(&e))?;
        Ok(Committed {
            rev: Some(rev),
            events: specs.len(),
        })
    }
}

/// The pages folded from `entries` up to revision `upto` (all when `None`),
/// with `extra` applied on top (a read-time recheck that is never stored).
pub fn space_of(entries: &[Entry], upto: Option<u64>, extra: &[EventSpec]) -> DocSpace {
    let mut log: Vec<LogEvent> = entries
        .iter()
        .filter(|e| upto.is_none_or(|r| e.rev <= r))
        .map(Entry::log_event)
        .collect();
    let next = log.last().map_or(1, |e| e.seq + 1);
    log.extend(extra.iter().enumerate().map(|(i, s)| LogEvent {
        seq: next + i as u64,
        event_id: format!("check_{i}"),
        op: s.op.to_owned(),
        target: Some(s.target.clone()),
        node_type: s.node_type.map(str::to_owned),
        body: Some(Value::Object(s.body.clone())),
    }));
    assemble(&log)
}

/// The ids of `page` and of every node that ever belonged to it (sections
/// and notes, retracted ones too).
fn page_nodes(entries: &[Entry], page: &str) -> BTreeSet<String> {
    let mut nodes = BTreeSet::from([page.to_owned()]);
    let mut notes_on: Vec<(String, String)> = Vec::new();
    for e in entries.iter().filter(|e| e.op == "edge.add") {
        let get = |k: &str| e.body.get(k).and_then(Value::as_str).unwrap_or_default();
        match get("kind") {
            "part_of" if get("to") == page => {
                nodes.insert(get("from").to_owned());
            }
            "about" => notes_on.push((get("from").to_owned(), get("to").to_owned())),
            _ => {}
        }
    }
    for (note, about) in notes_on {
        if nodes.contains(&about) {
            nodes.insert(note);
        }
    }
    nodes
}

/// The revisions that touched `page` (an id), oldest first.
pub fn history(entries: &[Entry], page: &str) -> Vec<Revision> {
    let nodes = page_nodes(entries, page);
    let touches = |e: &Entry| {
        nodes.contains(&e.target)
            || (e.op == "edge.add"
                && ["from", "to"].iter().any(|k| {
                    e.body
                        .get(*k)
                        .and_then(Value::as_str)
                        .is_some_and(|v| nodes.contains(v))
                }))
    };
    let mut out: BTreeMap<u64, Revision> = BTreeMap::new();
    for e in entries.iter().filter(|e| touches(e)) {
        out.entry(e.rev)
            .or_insert_with(|| Revision {
                rev: e.rev,
                at: e.at.clone(),
                author: e.author.clone(),
                action: e.action.clone(),
                events: 0,
            })
            .events += 1;
    }
    out.into_values().collect()
}

/// The last revision that touched each page (by page id), in one pass.
pub fn last_revisions(entries: &[Entry]) -> BTreeMap<String, Revision> {
    let mut owner: BTreeMap<&str, &str> = BTreeMap::new();
    for e in entries.iter().filter(|e| e.op == "edge.add") {
        let get = |k: &str| e.body.get(k).and_then(Value::as_str);
        if let (Some("part_of" | "about"), Some(from), Some(to)) =
            (get("kind"), get("from"), get("to"))
        {
            let page = owner.get(to).copied().unwrap_or(to);
            owner.insert(from, page);
        }
    }
    let mut out: BTreeMap<String, Revision> = BTreeMap::new();
    for e in entries {
        let page = match e.op.as_str() {
            "edge.add" => e
                .body
                .get("from")
                .and_then(Value::as_str)
                .map(|from| owner.get(from).copied().unwrap_or(from)),
            _ => Some(owner.get(e.target.as_str()).copied().unwrap_or(&e.target)),
        };
        let Some(page) = page else { continue };
        let slot = out.entry(page.to_owned()).or_insert_with(|| Revision {
            rev: 0,
            at: String::new(),
            author: e.author.clone(),
            action: String::new(),
            events: 0,
        });
        if slot.rev != e.rev {
            *slot = Revision {
                rev: e.rev,
                at: e.at.clone(),
                author: e.author.clone(),
                action: e.action.clone(),
                events: 0,
            };
        }
        slot.events += 1;
    }
    out
}

/// The id of the page `slug` names now or named last (a deleted or moved
/// page's history stays reachable by its old slug).
pub fn page_id_of(entries: &[Entry], slug: &str) -> Option<String> {
    let mut last: Option<&str> = None;
    for e in entries {
        let named = match e.op.as_str() {
            "node.add" if e.node_type.as_deref() == Some("page") => e.body.get("slug"),
            "field.set" if e.body.get("field").and_then(Value::as_str) == Some("slug") => {
                e.body.get("value")
            }
            _ => None,
        };
        if named.and_then(Value::as_str) == Some(slug) {
            last = Some(&e.target);
        }
    }
    last.map(str::to_owned)
}

fn pages_of(space: &DocSpace) -> Vec<Page> {
    let mut out: Vec<Page> = space
        .pages
        .values()
        .map(|p| Page {
            slug: p.slug().to_owned(),
            title: p.title().to_owned(),
            kind: p.page.str("type").unwrap_or("reference").to_owned(),
            status: p
                .page
                .str("status")
                .and_then(PageStatus::parse)
                .unwrap_or(PageStatus::Draft),
            sections: p
                .sections
                .iter()
                .map(|s| Section {
                    order: s.order,
                    text: s.body.clone(),
                })
                .collect(),
            anchors: p
                .page
                .fields
                .get("anchors")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(crate::write::anchor_from_value)
                .collect(),
            notes: p
                .notes
                .iter()
                .filter(|n| n.open)
                .map(|n| Note {
                    body: n.body.clone(),
                    covers: n.covers.iter().map(|c| c.reference.clone()).collect(),
                })
                .collect(),
        })
        .collect();
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

fn read_entries(file: &mut File) -> Result<Vec<Entry>, LocalError> {
    let len = file.metadata().map_err(|e| io(&e))?.len();
    if len > MAX_JOURNAL_BYTES {
        return Err(LocalError::Full);
    }
    let mut raw = String::new();
    file.take(MAX_JOURNAL_BYTES)
        .read_to_string(&mut raw)
        .map_err(|e| io(&e))?;
    let mut out = Vec::new();
    let mut lines = raw.split_inclusive('\n').enumerate().peekable();
    while let Some((i, line)) = lines.next() {
        // A last line without its newline is a torn append: not part of the
        // journal (the next write drops it).
        if !line.ends_with('\n') && lines.peek().is_none() {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_LINE_BYTES {
            return Err(LocalError::Corrupt { line: i + 1 });
        }
        let entry: Entry =
            serde_json::from_str(line).map_err(|_| LocalError::Corrupt { line: i + 1 })?;
        if out
            .last()
            .is_some_and(|p: &Entry| entry.seq <= p.seq || entry.rev < p.rev)
        {
            return Err(LocalError::Corrupt { line: i + 1 });
        }
        out.push(entry);
    }
    Ok(out)
}

/// The length of the journal up to its last complete line.
fn complete_len(file: &mut File, len: u64) -> Result<u64, LocalError> {
    if len == 0 {
        return Ok(0);
    }
    let mut last = [0u8; 1];
    file.seek(SeekFrom::Start(len - 1)).map_err(|e| io(&e))?;
    file.read_exact(&mut last).map_err(|e| io(&e))?;
    if last[0] == b'\n' {
        return Ok(len);
    }
    let mut raw = Vec::new();
    file.seek(SeekFrom::Start(0)).map_err(|e| io(&e))?;
    file.read_to_end(&mut raw).map_err(|e| io(&e))?;
    Ok(raw
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |i| i as u64 + 1))
}

fn create_private_dir(dir: &Path) -> Result<(), LocalError> {
    fs::create_dir_all(dir).map_err(|e| io(&e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|e| io(&e))?;
    }
    Ok(())
}

fn open_journal(path: &Path) -> Result<File, LocalError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|e| io(&e))
}

/// An exclusive advisory lock on the journal, released on drop (and by the
/// OS when the process ends).
/// Holds a duplicate of the journal's handle (same open file description),
/// so the caller keeps using its own handle while locked.
struct Lock(File);

impl Lock {
    fn exclusive(file: &File) -> Result<Self, LocalError> {
        let held = file.try_clone().map_err(|e| io(&e))?;
        fs2::FileExt::lock_exclusive(&held).map_err(|e| io(&e))?;
        Ok(Self(held))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
