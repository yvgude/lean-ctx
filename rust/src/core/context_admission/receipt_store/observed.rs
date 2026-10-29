// SPDX-License-Identifier: Apache-2.0
//! What was OBSERVED of a delivery after its receipt was prepared (G08).
//!
//! A receipt and its trace sidecar are stored BEFORE a byte leaves: they
//! prove what the gateway prepared and released to a channel, never that the
//! bytes were handed over. What the Engine then observes is a second, small
//! record of its own, `<data>/gateway/observed/<receipt sha256>.json`,
//! written once (`create_new`) AFTER the hand-over and naming the receipt it
//! belongs to. The receipt and the sidecar are never rewritten.
//!
//! States, each saying exactly what was seen:
//!
//! - `handed_over` (CLI): `write_all` and `flush` of exactly the prepared
//!   bytes to the process's stdout or stderr succeeded
//!   ([`write_observed`]). For a command that ends itself afterwards, the
//!   exit status it is ended with is recorded with it. Not observed: that a
//!   terminal displayed the bytes or a consumer of the pipe read them.
//! - `returned_to_transport` (MCP): the tool handler returned exactly the
//!   prepared result to the MCP transport layer. The Engine does not observe
//!   the transport writing the response, and cannot observe that the host
//!   received it; this state is never called a delivery.
//! - `preparation_failed`: the receipt was stored, but a later step of its
//!   preparation failed (it did not read back verified, its sidecar or its
//!   audit anchor could not be written). The delivery was withheld; the
//!   stored receipt's V1 outcome does not describe a delivery.
//! - no record: **unconfirmed**. The hand-over was not observed (a writer
//!   that does not go through [`write_observed`], a failed write, a record
//!   that could not be stored). Bytes that were written stay written.
//!
//! Whose observation: an observation is written only through the handle of
//! the receipt it belongs to ([`Prepared`]), which the call that sealed the
//! receipt carries to the place where it hands the bytes over. Nothing is
//! ever matched by content across calls: two calls that prepare identical
//! bytes hold two handles, and each confirms its own receipt or none. The
//! byte digest is only a check on that one handle — other bytes than the
//! prepared ones record nothing.
//!
//! What the record is NOT: a signed proof. It is a LOCAL, UNSIGNED file. It
//! names the hash of the signed audit entry that anchors its receipt and
//! sidecar (`anchor_entry`), and the inspector checks that this link holds —
//! a consistency check that costs no further audit append, not a signature:
//! whoever can write the store can write such a file. The signed statement
//! is the receipt's anchor, and it says PREPARED. The proxy records what it
//! observes of the upstream request in the egress lifecycle
//! (`EgressTransportObservationV1`), not here.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use lean_ctx_protocol::Sha256Digest;
use serde::{Deserialize, Serialize};

use super::trace::Channel;

pub(crate) const SCHEMA: &str = "leanctx.gateway.delivery-observation";
pub(crate) const VERSION: u32 = 1;
const MAX_RECORD_BYTES: usize = 4 * 1024;

/// The step of a receipt's preparation that failed after it was stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PreparationStep {
    /// The stored receipt did not read back verified.
    Receipt,
    /// The trace sidecar could not be built or stored.
    Sidecar,
    /// The audit anchor of receipt and sidecar could not be appended.
    Anchor,
}

/// What the Engine observed. See the module documentation for what each
/// state proves and does not prove.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum Observed {
    HandedOver {
        channel: Channel,
        bytes: Sha256Digest,
        /// The status the process is ended with right after the write, for a
        /// command that ends itself there.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_status: Option<i32>,
    },
    ReturnedToTransport {
        bytes: Sha256Digest,
    },
    PreparationFailed {
        step: PreparationStep,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ObservationV1 {
    pub(crate) schema: String,
    pub(crate) version: u32,
    /// Canonical digest of the V1 receipt this observation belongs to.
    pub(crate) receipt: Sha256Digest,
    /// Hash of the signed audit entry that anchors the receipt and its
    /// sidecar, as the sealing call saw it. A link the inspector checks; it
    /// does not sign this record. Absent for a failed preparation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) anchor_entry: Option<String>,
    pub(crate) observed: Observed,
}

fn observation_path(dir: &Path, receipt: &Sha256Digest) -> PathBuf {
    dir.join("observed").join(format!("{}.json", receipt.hex()))
}

/// Write the one observation of `receipt`. An existing record is kept: the
/// first observation is never replaced.
fn record_in(
    dir: &Path,
    receipt: &Sha256Digest,
    anchor_entry: Option<&str>,
    observed: Observed,
) -> Result<(), String> {
    let record = ObservationV1 {
        schema: SCHEMA.to_owned(),
        version: VERSION,
        receipt: receipt.clone(),
        anchor_entry: anchor_entry.map(str::to_owned),
        observed,
    };
    let bytes = serde_json::to_vec(&record).map_err(|error| error.to_string())?;
    let path = observation_path(dir, receipt);
    // Written once through the store's one writer (G09). Its entry's
    // reservation covers it while that is held; a record that comes later
    // must fit under the ceiling itself. Refused, it is not written — the
    // hand-over then stays unconfirmed, never "delivered".
    super::write_new(dir, &path, &bytes).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            "an observation of this receipt is already recorded".to_owned()
        } else {
            error.to_string()
        }
    })
}

/// `true`: the observation is stored. `false`: it is not — the delivery then
/// stays unconfirmed, which is what the inspector says.
fn record(receipt: &Sha256Digest, anchor_entry: Option<&str>, observed: Observed) -> bool {
    let stored = super::gateway_dir()
        .ok_or_else(|| "no lean-ctx data directory".to_owned())
        .and_then(|dir| record_in(&dir, receipt, anchor_entry, observed));
    if let Err(error) = &stored {
        tracing::warn!("gateway delivery observation not recorded: {error}");
    }
    stored.is_ok()
}

/// An observation as it is found in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StoredObservation {
    /// Nothing was observed, or the observation could not be stored.
    Absent,
    /// A record is there but is not an observation of a known schema.
    Unreadable,
    /// A readable record that names another receipt.
    Unbound,
    /// Names this receipt: what was observed, and the audit entry the
    /// record says anchors its receipt.
    Bound {
        observed: Observed,
        anchor_entry: Option<String>,
    },
}

impl StoredObservation {
    /// The one state word every view of a receipt shows for it (inspector,
    /// dashboard). No record reads `unconfirmed`; for a proxy receipt the
    /// view names the egress lifecycle instead.
    pub(crate) fn state(&self) -> &'static str {
        match self {
            Self::Absent => "unconfirmed",
            Self::Unreadable => "UNREADABLE",
            Self::Unbound => "MISMATCH",
            Self::Bound { observed, .. } => match observed {
                Observed::HandedOver { .. } => "handed_over",
                Observed::ReturnedToTransport { .. } => "returned_to_transport",
                Observed::PreparationFailed { .. } => "preparation_failed",
            },
        }
    }
}

/// The observation filed under `receipt`.
pub(crate) fn load(receipt: &Sha256Digest) -> StoredObservation {
    match super::gateway_dir() {
        Some(dir) => load_in(&dir, receipt),
        None => StoredObservation::Absent,
    }
}

pub(crate) fn load_in(dir: &Path, receipt: &Sha256Digest) -> StoredObservation {
    let file = match std::fs::File::open(observation_path(dir, receipt)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return StoredObservation::Absent;
        }
        Err(_) => return StoredObservation::Unreadable,
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_RECORD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > MAX_RECORD_BYTES
    {
        return StoredObservation::Unreadable;
    }
    let Ok(record) = serde_json::from_slice::<ObservationV1>(&bytes) else {
        return StoredObservation::Unreadable;
    };
    if record.schema != SCHEMA || record.version != VERSION {
        return StoredObservation::Unreadable;
    }
    if record.receipt != *receipt {
        return StoredObservation::Unbound;
    }
    StoredObservation::Bound {
        observed: record.observed,
        anchor_entry: record.anchor_entry,
    }
}

/// A stored receipt whose preparation failed at `step`: mark it, so that it
/// is never read as a delivery. Written when the failure is detected.
pub(super) fn preparation_failed(receipt: &Sha256Digest, step: PreparationStep) {
    record(receipt, None, Observed::PreparationFailed { step });
}

/// The handle of ONE prepared delivery: the receipt that was sealed, the
/// audit entry that anchors it, and the channel and bytes it was prepared
/// for. Only [`super::seal`]'s caller makes one, and only through it is an
/// observation of that receipt written — a handle never stands for another
/// call's receipt, whatever bytes that call prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Prepared {
    receipt: Sha256Digest,
    anchor_entry: String,
    channel: Channel,
    bytes: Sha256Digest,
}

impl Prepared {
    pub(super) fn sealed(
        receipt: &Sha256Digest,
        anchor_entry: String,
        channel: Channel,
        bytes: Sha256Digest,
    ) -> Self {
        Self {
            receipt: receipt.clone(),
            anchor_entry,
            channel,
            bytes,
        }
    }

    /// MCP: the handler returns `returned` to the transport layer. Tool
    /// session: the response frame `returned` was written and flushed to
    /// the session transport. Recorded as `returned_to_transport` for THIS
    /// receipt, and only when `returned` is exactly what the receipt was
    /// prepared for. `false`: nothing was recorded (other bytes, another
    /// channel, or the store refused).
    pub(crate) fn returned_to_transport(&self, returned: &Sha256Digest) -> bool {
        matches!(self.channel, Channel::McpTransport | Channel::SessionFrame)
            && self.bytes == *returned
            && record(
                &self.receipt,
                Some(&self.anchor_entry),
                Observed::ReturnedToTransport {
                    bytes: self.bytes.clone(),
                },
            )
    }

    /// CLI: this thread is about to write the prepared bytes to its stdout
    /// or stderr. The handle waits for exactly the next matching write of
    /// this thread to that stream ([`write_observed`]); nothing else can
    /// confirm it. If the thread already holds an unwritten handle for the
    /// same stream with the SAME bytes, a write could not tell the two
    /// apart: this handle is then held as ambiguous and confirms nothing,
    /// and the earlier one is dropped — both stay unconfirmed. A handle for
    /// another channel is dropped.
    pub(crate) fn await_write(self) {
        let Some(stream) = stream_index(self.channel) else {
            return;
        };
        AWAITED.with(|awaited| {
            let mut awaited = awaited.borrow_mut();
            let ambiguous = awaited[stream]
                .as_ref()
                .is_some_and(|earlier| earlier.prepared.bytes == self.bytes);
            // One output goes to one channel: a handle still waiting on
            // another channel of this thread is from an earlier output and
            // stays where it is.
            awaited[stream] = Some(Awaited {
                prepared: self,
                ambiguous,
            });
        });
    }
}

/// The delivery this thread prepared last for one of its streams and has not
/// written yet.
struct Awaited {
    prepared: Prepared,
    ambiguous: bool,
}

thread_local! {
    /// Per stream (stdout, stderr): the handle waiting for this thread's
    /// next write. Thread-local on purpose — the writer of a CLI command is
    /// the thread that prepared the output; no other thread's write, and no
    /// other call's bytes, can reach a handle here.
    static AWAITED: RefCell<[Option<Awaited>; 4]> =
        const { RefCell::new([None, None, None, None]) };
}

/// The slot of a channel a CLI thread hands bytes to itself: its two
/// streams, a file it writes, the clipboard.
fn stream_index(channel: Channel) -> Option<usize> {
    match channel {
        Channel::Stdout => Some(0),
        Channel::Stderr => Some(1),
        Channel::File => Some(2),
        Channel::Clipboard => Some(3),
        Channel::McpTransport | Channel::SessionFrame | Channel::Upstream => None,
    }
}

/// The ONE observed write: `bytes` to `sink` with `write_all` and `flush`,
/// and — only when both succeeded — the observation that exactly these bytes
/// were handed to `channel`, for the one handle this thread holds for that
/// stream ([`Prepared::await_write`]). Bytes other than the prepared ones
/// record nothing and leave the handle waiting. A failed write returns its
/// error and records nothing: the delivery stays unconfirmed. `exit_status`:
/// the status the caller ends the process with right after this write, if
/// it does.
pub(crate) fn write_observed(
    sink: &mut dyn Write,
    channel: Channel,
    bytes: &[u8],
    exit_status: Option<i32>,
) -> std::io::Result<()> {
    sink.write_all(bytes)?;
    sink.flush()?;
    handed_over(channel, bytes, exit_status);
    Ok(())
}

/// The calling thread handed exactly `bytes` to `channel` and SAW IT
/// SUCCEED: record `handed_over` for the one handle this thread holds for
/// that channel, if `bytes` are the prepared ones. For a hand-over that is
/// not a stream write — a file written and closed, a clipboard hand-off that
/// returned success — called by the command after that success, never
/// before. What is observed is the success of that call; that the file is
/// read or the clipboard pasted is not.
pub(crate) fn handed_over(channel: Channel, bytes: &[u8], exit_status: Option<i32>) {
    let Some(stream) = stream_index(channel) else {
        return;
    };
    let written = AWAITED.with(|awaited| {
        let mut awaited = awaited.borrow_mut();
        let waiting = awaited[stream].as_ref()?;
        if waiting.prepared.bytes == super::output_digest(bytes) {
            awaited[stream].take()
        } else {
            None
        }
    });
    if let Some(Awaited {
        prepared,
        ambiguous: false,
    }) = written
    {
        record(
            &prepared.receipt,
            Some(&prepared.anchor_entry),
            Observed::HandedOver {
                channel,
                bytes: prepared.bytes,
                exit_status,
            },
        );
    }
}
