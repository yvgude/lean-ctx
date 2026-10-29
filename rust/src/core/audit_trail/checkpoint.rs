// SPDX-License-Identifier: Apache-2.0
//! Verified-prefix checkpoint for the audit hash chain.
//!
//! A full pass over a large trail costs seconds per call, and hot callers
//! (billing status, Cockpit, drift reconstruction) verify on every request.
//! After a successful pass the verifier stores the chain state at the end of
//! the file, so the next call reads only the appended suffix.
//!
//! Integrity guarantee — what a resumed (incremental) verification proves:
//! * every appended entry is verified exactly like a full pass would, against
//!   the stored chain head;
//! * the file is the same object (device + inode on Unix), it has not shrunk,
//!   and a same-length file has an unchanged mtime (no in-place rewrite
//!   without an append);
//! * the first and last [`EDGE_BYTES`] of the verified prefix are byte-identical
//!   (SHA-256), so truncation, rotation, replacement and edits near the start
//!   or the append boundary fall back to a full pass;
//! * the checkpoint is signed with the local `lean-ctx` audit key and bound to
//!   the trail path, so editing it is as hard as forging a signed drift entry.
//!
//! What it does NOT prove: an in-place edit strictly inside the prefix, away
//! from both edges, that is followed by an append (or whose writer restores
//! the mtime), is not seen by a resumed pass.
//! Re-reading the prefix is the only way to see it, so it is bounded instead:
//! a checkpoint whose last FULL pass is older than [`FULL_REVERIFY_SECS`]
//! (24 h) is ignored, and explicit audit commands force a full pass
//! (`verify_chain_full`). Note the chain itself is an unkeyed SHA-256 chain:
//! a writer with file access can always rebuild it end to end; only drift
//! and seal entries carry signatures. The checkpoint does not weaken that model.
//!
//! Each file of a segmented trail (see `segments`) has its own checkpoint,
//! which records the chain state it starts from: it resumes only after the
//! files before it ended in exactly that state. A sealed segment is never
//! written again, so its checkpoint is not bound to the 24 h full pass;
//! instead the verifier re-reads one sealed segment per day in rotation.

use super::ChainState;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Bump on any change to the stored `ChainState`: an older checkpoint must
/// never resume with a field defaulted (v2 added the signed-tally evidence,
/// v3 the security evidence of every kind, v4 the start link and the seal).
const VERSION: u32 = 4;
/// Bytes hashed at the start and at the end of the verified prefix.
const EDGE_BYTES: u64 = 64 * 1024;
/// A checkpoint whose last full pass is older than this is not trusted.
pub(super) const FULL_REVERIFY_SECS: i64 = 24 * 60 * 60;
/// Tolerated clock step backwards before a checkpoint counts as "future".
const CLOCK_SKEW_SECS: i64 = 300;
/// Checkpoints are tiny (chain head + per-agent drift); refuse anything big.
const MAX_CHECKPOINT_BYTES: u64 = 1024 * 1024;
const KEY_ID: &str = "lean-ctx";

/// Identity and length of the open trail, taken under the shared lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FileStamp {
    pub(super) size: u64,
    dev: u64,
    ino: u64,
    mtime_secs: u64,
    mtime_nanos: u32,
}

impl FileStamp {
    pub(super) fn of(file: &fs::File) -> Option<Self> {
        let meta = file.metadata().ok()?;
        let mtime = meta
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        #[cfg(unix)]
        let (dev, ino) = {
            use std::os::unix::fs::MetadataExt;
            (meta.dev(), meta.ino())
        };
        // lean-ctx: no stable file id in std on Windows; the edge digests and
        // the 24 h full pass carry rotation detection there.
        #[cfg(not(unix))]
        let (dev, ino) = (0, 0);
        Some(Self {
            size: meta.len(),
            dev,
            ino,
            mtime_secs: mtime.as_secs(),
            mtime_nanos: mtime.subsec_nanos(),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct Payload {
    version: u32,
    trail: String,
    /// The trail as verified; `stamp.size` is the verified byte offset and
    /// always ends on a line boundary.
    stamp: FileStamp,
    head_digest: String,
    tail_digest: String,
    /// Unix time of the last pass that read this file from its start.
    full_verified_at: i64,
    /// Chain head and entry count the file starts from.
    start_hash: String,
    start_total: usize,
    state: ChainState,
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    payload: String,
    signature: String,
}

pub(super) struct Resumed {
    pub(super) offset: u64,
    pub(super) state: ChainState,
    pub(super) full_verified_at: i64,
}

/// Checkpoint location for one trail: `<data_dir>/audit/verified/<id>.json`.
pub(super) struct Store {
    path: PathBuf,
    trail: String,
}

impl Store {
    /// Only trails inside the data dir get a checkpoint; an arbitrary path
    /// (tests, exported copies) is always verified in full.
    pub(super) fn for_trail(trail: &Path) -> Option<Self> {
        let data = crate::core::data_dir::lean_ctx_data_dir().ok()?;
        if !trail.starts_with(&data) {
            return None;
        }
        let trail = trail.to_string_lossy().into_owned();
        let id = crate::core::agent_identity::hex_encode(&Sha256::digest(trail.as_bytes()));
        Some(Self {
            path: data
                .join("audit")
                .join("verified")
                .join(format!("{}.json", &id[..32])),
            trail,
        })
    }

    /// The stored prefix state, if the checkpoint is authentic, starts from
    /// `start`, is fresh (a full pass within `max_age` seconds; `None` for a
    /// sealed segment) and still describes a prefix of `file`. `None` means
    /// "verify in full".
    pub(super) fn resume(
        &self,
        file: &fs::File,
        stamp: &FileStamp,
        start: &ChainState,
        max_age: Option<i64>,
        now: i64,
    ) -> Option<Resumed> {
        let payload = self.authentic_prefix_of(file, stamp)?;
        if payload.start_hash != start.prev_hash || payload.start_total != start.total {
            return None;
        }
        let age = now - payload.full_verified_at;
        if age < -CLOCK_SKEW_SECS || max_age.is_some_and(|max_age| age >= max_age) {
            return None;
        }
        Some(Resumed {
            offset: payload.stamp.size,
            state: payload.state,
            full_verified_at: payload.full_verified_at,
        })
    }

    /// Whether a pass of this build verified a non-empty prefix of `file`.
    pub(super) fn holds_prefix_of(&self, file: &fs::File, stamp: &FileStamp) -> bool {
        self.authentic_prefix_of(file, stamp)
            .is_some_and(|payload| payload.stamp.size > 0)
    }

    /// The checkpoint, if it is authentic and still describes a prefix of
    /// `file`.
    fn authentic_prefix_of(&self, file: &fs::File, stamp: &FileStamp) -> Option<Payload> {
        let raw = fs::File::open(&self.path).ok()?;
        let mut bytes = Vec::new();
        raw.take(MAX_CHECKPOINT_BYTES)
            .read_to_end(&mut bytes)
            .ok()?;
        let envelope: Envelope = serde_json::from_slice(&bytes).ok()?;
        let signature = crate::core::agent_identity::hex_decode(&envelope.signature).ok()?;
        let public_key = crate::core::agent_identity::load_public_key(KEY_ID).ok()?;
        if !crate::core::agent_identity::verify_signature(
            public_key.as_bytes(),
            envelope.payload.as_bytes(),
            &signature,
        ) {
            return None;
        }
        let payload: Payload = serde_json::from_str(&envelope.payload).ok()?;
        if payload.version != VERSION || payload.trail != self.trail {
            return None;
        }
        let offset = payload.stamp.size;
        if (stamp.dev, stamp.ino) != (payload.stamp.dev, payload.stamp.ino) || stamp.size < offset {
            return None;
        }
        // Same length but touched: rewritten in place, nothing appended.
        if stamp.size == offset && *stamp != payload.stamp {
            return None;
        }
        let (head, tail) = edge_digests(file, offset)?;
        if head != payload.head_digest || tail != payload.tail_digest {
            return None;
        }
        Some(payload)
    }

    /// Persist `state` as verified from `start` through `stamp.size` (0600,
    /// atomic). Best effort: a missing key or a failed write only costs speed.
    pub(super) fn save(
        &self,
        file: &fs::File,
        stamp: &FileStamp,
        start: &ChainState,
        state: &ChainState,
        full_verified_at: i64,
    ) {
        if stamp.size == 0 {
            return;
        }
        let Some((head_digest, tail_digest)) = edge_digests(file, stamp.size) else {
            return;
        };
        let Ok(key) = crate::core::agent_identity::get_stored_signing_key(KEY_ID) else {
            return;
        };
        let payload = Payload {
            version: VERSION,
            trail: self.trail.clone(),
            stamp: stamp.clone(),
            head_digest,
            tail_digest,
            full_verified_at,
            start_hash: start.prev_hash.clone(),
            start_total: start.total,
            state: state.clone(),
        };
        let Ok(payload) = serde_json::to_string(&payload) else {
            return;
        };
        let signature = crate::core::agent_identity::hex_encode(
            &crate::core::agent_identity::sign_bytes_with(&key, payload.as_bytes()),
        );
        let Ok(bytes) = serde_json::to_vec(&Envelope { payload, signature }) else {
            return;
        };
        let Some(dir) = self.path.parent() else {
            return;
        };
        if fs::create_dir_all(dir).is_err() {
            return;
        }
        crate::core::data_dir::ensure_dir_permissions(dir);
        #[cfg(unix)]
        let perms = {
            use std::os::unix::fs::PermissionsExt;
            Some(fs::Permissions::from_mode(0o600))
        };
        #[cfg(not(unix))]
        let perms: Option<fs::Permissions> = None;
        if let Err(error) =
            crate::core::atomic_fs::try_atomic_write(&self.path, &bytes, perms.as_ref())
        {
            tracing::debug!("audit checkpoint not saved: {error}");
        }
    }

    /// Drop the checkpoint after a failed full pass.
    pub(super) fn discard(&self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// SHA-256 of the first and of the last `EDGE_BYTES` of `file[..offset]`.
fn edge_digests(file: &fs::File, offset: u64) -> Option<(String, String)> {
    let window = offset.min(EDGE_BYTES);
    let digest_at = |start: u64| -> Option<String> {
        let mut f = file;
        f.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = vec![0u8; usize::try_from(window).ok()?];
        f.read_exact(&mut buf).ok()?;
        Some(crate::core::agent_identity::hex_encode(&Sha256::digest(
            &buf,
        )))
    };
    Some((digest_at(0)?, digest_at(offset - window)?))
}
