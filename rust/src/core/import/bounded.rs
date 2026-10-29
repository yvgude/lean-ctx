// SPDX-License-Identifier: Apache-2.0

//! The bounded import read: a transcript above the read cap, admitted in
//! units and never held in memory as a whole.
//!
//! The file is opened ONCE through the rooted admitted reader's path decision
//! and no-follow open (`tools::ctx_read::open_rooted_unit_source`) and read
//! from that descriptor in small pieces. The pieces are grouped into UNITS of
//! complete records (lines), each at most [`unit_bytes`]; every unit passes
//! the admission a whole file gets, on its original bytes, before anything of
//! it is parsed. Nothing is skipped unadmitted: the units cover the file from
//! its first byte to its last.
//!
//! Unit boundaries must not make admission weaker than admitting the whole
//! text. Three things keep that true, by rule class:
//!
//! - rules that cannot match across a line end find in a unit what they find
//!   in the whole text — units are complete lines;
//! - the built-in rules that CAN cross a line end with a bounded match are
//!   covered by a SEAM: for every boundary, the end of the earlier unit and
//!   the start of the later one are admitted once more as one text
//!   ([`SEAM_SIDE_BYTES`]). A seam is checked only, never spooled or parsed;
//! - whatever has no bound — a free pattern of a policy pack or of the
//!   configuration that is not proven single-line, a semantic detector stage,
//!   a unit that ends inside a built-in rule spanning any distance — makes
//!   the reader refuse unit admission as a whole
//!   ([`SourceRefusal::UnitAdmissionUnsupported`]); see
//!   `RootedUnitSource::admit_unit`.
//!
//! The rule for a refused value is the one a transcript has always had — the
//! SOURCE is refused, not a record: a unit or seam that is withheld, or
//! delivered for a reader only, ends the import of the whole file. To keep
//! that true without holding the file in memory, the admitted text of each
//! unit is spooled to an unlinked private file and parsing starts only after
//! the last unit was admitted AND the spool was read back and found to be
//! exactly what was written ([`verified`]).
//!
//! Memory is bounded by the unit size (one unit of raw bytes, one record
//! being assembled, one admitted text, one seam), disk by
//! [`MAX_TRANSCRIPT_BYTES`].

use std::io::{Read, Seek, SeekFrom, Write};

use super::{HistorySource, SourceRefusal};
use crate::core::context_admission::cross_line;
use crate::tools::ctx_read::RootedUnitSource;

/// Total cap for one transcript read in units.
///
/// Sized for what the importers read: a Claude Code, Codex or Cursor session
/// is one JSONL file that grows by one record per message or tool result for
/// as long as the session is resumed, with pasted images and tool output
/// inline. The read cap (4 MiB) is passed by an ordinary working day; 512 MiB
/// leaves two orders of magnitude above it and still bounds the time and the
/// spool space one file can take. A larger file is skipped whole and counted.
pub(crate) const MAX_TRANSCRIPT_BYTES: u64 = 512 * 1024 * 1024;

/// Size of one raw read from the descriptor.
const READ_CHUNK_BYTES: usize = 64 * 1024;

/// How much of each unit next to a boundary is guaranteed to be in the seam
/// (minus at most three bytes for a character boundary).
///
/// Derived from the ONE statement of what the built-in rules can match
/// across a line end (`context_admission::cross_line`, shared with the
/// gateway's chunked scan): the longest reach that has a bound is
/// [`BOUNDED_REACH_BYTES`](cross_line::BOUNDED_REACH_BYTES) per side.
/// The seam takes sixteen times that (4 KiB), so that for ordinary records
/// whole lines stand on both sides; at the smallest read cap (1,024 bytes) a
/// side is exactly the reach. Rules without a bound do not rely on the seam
/// (see the module text and `RootedUnitSource::admit_unit`).
pub(super) const SEAM_SIDE_BYTES: usize = 16 * cross_line::BOUNDED_REACH_BYTES;

/// Bound of one admission unit and therefore of one record: the read cap —
/// no text larger than the one a whole-file read may hand to admission —
/// and never above what an active policy pack inspects at all.
pub(super) fn unit_bytes() -> usize {
    crate::core::limits::max_read_bytes()
        .min(crate::core::policy::content::MAX_PROTECTED_CONTENT_BYTES)
}

/// A transcript whose every unit and seam was admitted as storable: the
/// admitted unit texts in file order, spooled and read back once, with their
/// lengths.
pub(super) struct AdmittedTranscript {
    spool: std::fs::File,
    units: Vec<usize>,
    /// The source the units were admitted from: it holds the gateway rules
    /// they were admitted under, asked once more when the last unit was read.
    reader: RootedUnitSource,
}

// Test seam: the read of the spool fails before the unit with this index.
#[cfg(test)]
thread_local! {
    pub(super) static FAIL_BEFORE_UNIT: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn spool_fault_before(unit: usize) -> bool {
    FAIL_BEFORE_UNIT.with(std::cell::Cell::get) == Some(unit)
}

#[cfg(not(test))]
const fn spool_fault_before(_unit: usize) -> bool {
    false
}

impl AdmittedTranscript {
    /// Hand each admitted unit — complete lines, in file order — to `each`.
    ///
    /// `Ok` only when every unit was read and handed out AND the gateway
    /// rules are still the ones the units were admitted under. On `Err` the
    /// caller must take over NOTHING of this source: `each` may already have
    /// seen lines (the spool was read back in full before this value
    /// existed, so a read error here is a new fault of the local disk — but
    /// it can happen). The importer therefore lets `each` fill a pending set
    /// and takes it over only after `Ok` (`super::Pending`).
    pub(super) fn for_each_unit(self, mut each: impl FnMut(&str)) -> Result<(), SourceRefusal> {
        let Self {
            mut spool,
            units,
            reader,
        } = self;
        spool
            .seek(SeekFrom::Start(0))
            .map_err(|_| SourceRefusal::Unreadable)?;
        for (index, length) in units.into_iter().enumerate() {
            if spool_fault_before(index) {
                return Err(SourceRefusal::Unreadable);
            }
            let mut bytes = vec![0_u8; length];
            spool
                .read_exact(&mut bytes)
                .map_err(|_| SourceRefusal::Unreadable)?;
            let text = String::from_utf8(bytes).map_err(|_| SourceRefusal::Unreadable)?;
            each(&text);
        }
        if reader.gateway_rules_unchanged() {
            Ok(())
        } else {
            Err(SourceRefusal::Withheld)
        }
    }
}

/// Phase one: what was admitted and written so far.
struct Admitting {
    spool: std::fs::File,
    units: Vec<usize>,
    /// Running digest and length of the bytes written to the spool.
    digest: blake3::Hasher,
    written: u64,
    /// The end of the last admitted unit: the earlier half of the next seam.
    tail: Vec<u8>,
    seam_side: usize,
}

/// Admit the transcript at `source` unit by unit.
///
/// `Err` and nothing imported when: a path rule withholds it; it is above
/// [`MAX_TRANSCRIPT_BYTES`] (by its descriptor's size, or while it is read);
/// one record alone is above [`unit_bytes`] ([`SourceRefusal::RecordTooLarge`]
/// — a record is never cut, and an uninspected record could hold the marking
/// that withholds the file); a unit is not UTF-8 text; admission withholds a
/// unit or a seam or delivers it for a reader only; the active rules cannot
/// be applied unit by unit ([`SourceRefusal::UnitAdmissionUnsupported`]); or
/// the spool does not read back as written.
///
/// Must run inside a source view (`super::pinned`): the pack is then one
/// snapshot for every unit and seam; without one the reader refuses to open.
pub(super) fn admit(
    source: &HistorySource,
    gateway: &super::staging::Gateway,
) -> Result<AdmittedTranscript, SourceRefusal> {
    let unit_cap = unit_bytes();
    // `gateway` is the ONE snapshot of this source: every unit and seam is
    // admitted under it, as its label and its built facts are.
    let mut reader = crate::tools::ctx_read::open_rooted_unit_source(
        &source.path.to_string_lossy(),
        &source.root.to_string_lossy(),
        "ctx_knowledge",
        gateway.clone(),
    )
    .map_err(|error| refusal(&error))?;
    if reader.byte_len() > MAX_TRANSCRIPT_BYTES {
        return Err(SourceRefusal::TooLarge);
    }
    let spool = super::scratch_parent()
        .and_then(tempfile::tempfile_in)
        .map_err(|_| SourceRefusal::Unreadable)?;
    let mut admitting = Admitting {
        spool,
        units: Vec::new(),
        digest: blake3::Hasher::new(),
        written: 0,
        tail: Vec::new(),
        // A half is at most twice its side (`seam_tail`, `seam_head`), and
        // both halves together are one admitted text: at most a unit.
        seam_side: SEAM_SIDE_BYTES.min(unit_cap / 4),
    };

    // `unit`: complete records not yet admitted. `record`: the record being
    // assembled. Both stay at or below `unit_cap`.
    let mut unit = Vec::new();
    let mut record = Vec::new();
    let mut chunk = vec![0_u8; READ_CHUNK_BYTES];
    let mut total = 0_u64;
    loop {
        let read = reader
            .read_raw(&mut chunk)
            .map_err(|_| SourceRefusal::Unreadable)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > MAX_TRANSCRIPT_BYTES {
            return Err(SourceRefusal::TooLarge);
        }
        let mut rest = &chunk[..read];
        // A record ends at a line feed. That byte never occurs inside a
        // multi-byte UTF-8 character, so a unit never divides a character.
        while let Some(newline) = rest.iter().position(|byte| *byte == b'\n') {
            let (head, tail) = rest.split_at(newline + 1);
            extend_record(&mut record, head, unit_cap)?;
            close_record(&reader, &mut unit, &mut record, unit_cap, &mut admitting)?;
            rest = tail;
        }
        extend_record(&mut record, rest, unit_cap)?;
    }
    // The last record may end without a line feed.
    close_record(&reader, &mut unit, &mut record, unit_cap, &mut admitting)?;
    admit_unit(&reader, &mut unit, &mut admitting)?;
    verified(admitting, reader)
}

fn extend_record(record: &mut Vec<u8>, bytes: &[u8], cap: usize) -> Result<(), SourceRefusal> {
    if record.len() + bytes.len() > cap {
        return Err(SourceRefusal::RecordTooLarge);
    }
    record.extend_from_slice(bytes);
    Ok(())
}

/// Move the finished `record` into `unit`, admitting the unit first when the
/// record no longer fits into it.
fn close_record(
    reader: &RootedUnitSource,
    unit: &mut Vec<u8>,
    record: &mut Vec<u8>,
    cap: usize,
    admitting: &mut Admitting,
) -> Result<(), SourceRefusal> {
    if unit.len() + record.len() > cap {
        admit_unit(reader, unit, admitting)?;
    }
    unit.append(record);
    Ok(())
}

/// Admit `unit` — first the seam with the unit before it, then the unit — and
/// spool the unit's admitted text. Only what may feed a derived store is kept
/// (E3); any other outcome, of the seam as of the unit, refuses the source.
fn admit_unit(
    reader: &RootedUnitSource,
    unit: &mut Vec<u8>,
    admitting: &mut Admitting,
) -> Result<(), SourceRefusal> {
    if unit.is_empty() {
        return Ok(());
    }
    let bytes = std::mem::take(unit);
    if !admitting.tail.is_empty() {
        let mut seam = std::mem::take(&mut admitting.tail);
        seam.extend_from_slice(seam_head(&bytes, admitting.seam_side));
        let seam = reader.admit_seam(seam).map_err(|error| refusal(&error))?;
        if !seam.storable {
            return Err(SourceRefusal::Withheld);
        }
    }
    admitting.tail = seam_tail(&bytes, admitting.seam_side).to_vec();
    let unit = reader.admit_unit(bytes).map_err(|error| refusal(&error))?;
    if !unit.storable {
        return Err(SourceRefusal::Withheld);
    }
    admitting
        .spool
        .write_all(unit.content.as_bytes())
        .map_err(|_| SourceRefusal::Unreadable)?;
    admitting.digest.update(unit.content.as_bytes());
    admitting.written += unit.content.len() as u64;
    admitting.units.push(unit.content.len());
    Ok(())
}

/// The end of a unit for a seam: at least its last `side` bytes, extended
/// back to the start of the record they begin in — complete records, so a
/// line anchor sees what it sees in the file. When that would be more than
/// twice `side` (one long record), the last `side` bytes instead, moved
/// forward to a character boundary (at most three bytes less).
fn seam_tail(unit: &[u8], side: usize) -> &[u8] {
    if unit.len() <= side {
        return unit;
    }
    let start = unit.len() - side;
    let record = unit[..start]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |newline| newline + 1);
    if unit.len() - record <= 2 * side {
        return &unit[record..];
    }
    let mut start = start;
    while start < unit.len() && unit[start] & 0xC0 == 0x80 {
        start += 1;
    }
    &unit[start..]
}

/// The start of a unit for a seam: at least its first `side` bytes, extended
/// to the end of the record they end in. When that would be more than twice
/// `side`, the first `side` bytes instead, moved back to a character boundary.
fn seam_head(unit: &[u8], side: usize) -> &[u8] {
    if unit.len() <= side {
        return unit;
    }
    let record = unit[side - 1..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(unit.len(), |newline| side + newline);
    if record <= 2 * side {
        return &unit[..record];
    }
    let mut end = side;
    while end > 0 && unit[end] & 0xC0 == 0x80 {
        end -= 1;
    }
    &unit[..end]
}

/// Phase one ends here: read the spool back in full and compare it with what
/// was written — length and BLAKE3. Only a spool that reads back exactly is
/// handed to phase two, so no line is parsed from a spool that is short,
/// unreadable or altered.
fn verified(
    admitting: Admitting,
    reader: RootedUnitSource,
) -> Result<AdmittedTranscript, SourceRefusal> {
    let Admitting {
        mut spool,
        units,
        digest,
        written,
        ..
    } = admitting;
    spool
        .seek(SeekFrom::Start(0))
        .map_err(|_| SourceRefusal::Unreadable)?;
    let mut read_back = blake3::Hasher::new();
    let mut length = 0_u64;
    let mut chunk = vec![0_u8; READ_CHUNK_BYTES];
    loop {
        let read = spool
            .read(&mut chunk)
            .map_err(|_| SourceRefusal::Unreadable)?;
        if read == 0 {
            break;
        }
        read_back.update(&chunk[..read]);
        length += read as u64;
    }
    if length != written || read_back.finalize() != digest.finalize() {
        return Err(SourceRefusal::Unreadable);
    }
    Ok(AdmittedTranscript {
        spool,
        units,
        reader,
    })
}

/// The typed refusal of a reader error: unit admission not applicable,
/// withheld by a rule, or not readable.
fn refusal(error: &std::io::Error) -> SourceRefusal {
    match error.kind() {
        std::io::ErrorKind::Unsupported => SourceRefusal::UnitAdmissionUnsupported,
        std::io::ErrorKind::PermissionDenied => SourceRefusal::Withheld,
        _ => SourceRefusal::Unreadable,
    }
}
