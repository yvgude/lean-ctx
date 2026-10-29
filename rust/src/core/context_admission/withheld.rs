// SPDX-License-Identifier: Apache-2.0
//! Withheld egress blocks: stable ids, a local explanation and the human
//! override (`lean-ctx gateway why|list|allow-once|revoke`).
//!
//! A block the gateway withholds from a model request is replaced in place by
//! a notice naming its id. The id is `w-` plus the first 12 hex digits of the
//! keyed object digest (HMAC-SHA256 under the installation's receipt key, the
//! receipt v2 object digest): the same bytes get the same id on every re-send
//! of a conversation, and the remote side cannot confirm a guess with it.
//!
//! What is stored here is metadata only — reason codes, position class,
//! destination, mode — never content and never a timestamp in a body. It
//! lives outside the receipt store, so a full evidence store cannot take the
//! explanation away. A grant lets exactly one block go to the provider it was
//! withheld from, until it expires; only the destination rule is waived and
//! only in developer mode. Grants are written by the CLI after a human
//! confirmed on a terminal and are bound to the receipt key by a MAC.

use std::path::PathBuf;

use lean_ctx_protocol::Sha256Digest;
use serde::{Deserialize, Serialize};

/// The reason recorded when a grant let a restricted block go remote.
pub(crate) const ALLOWED_ONCE: &str = "destination.remote_allowed_once";
/// At most this many explanations are kept; the oldest files go first.
const MAX_RECORDS: usize = 1024;
/// Id length in hex digits after `w-`.
const ID_HEX: usize = 12;

/// The id of a withheld block, from its plain object digest. `None` without a
/// usable receipt key: the notice then names no id.
pub(crate) fn block_id(object: &Sha256Digest) -> Option<String> {
    let key = super::receipt_store::keyed::object_key().ok()?;
    let keyed = key.object(object);
    Some(format!("w-{}", &keyed.hex()[..ID_HEX]))
}

/// Whether `id` has the shape of a block id (`w-` + 12 lowercase hex).
#[must_use]
pub fn valid_id(id: &str) -> bool {
    id.strip_prefix("w-").is_some_and(|hex| {
        hex.len() == ID_HEX && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// What the gateway knows about one withheld block. Content-free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WithheldBlock {
    pub id: String,
    pub reason_codes: Vec<String>,
    /// `system`, `message 3`, `tools`, … — where in the request it was.
    pub position: String,
    /// Destination provider and locality it was withheld from.
    pub provider: String,
    pub locality: String,
    pub mode: String,
}

/// A human's decision to let one block go to one provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub id: String,
    pub provider: String,
    pub expires_unix: u64,
    /// MAC under the receipt key over id, provider and expiry.
    mac: String,
}

fn dir() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|dir| dir.join("gateway").join("withheld"))
}

fn grants_dir() -> Option<PathBuf> {
    dir().map(|dir| dir.join("grants"))
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or(std::io::ErrorKind::NotFound)?;
    std::fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("record");
    let staged = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&staged)?;
        std::io::Write::write_all(&mut file, bytes)?;
    }
    std::fs::rename(&staged, path)
}

/// Record the explanation of a withheld block (best effort: an unwritable
/// store never changes the decision). Unchanged records are not rewritten.
pub(crate) fn record(block: &WithheldBlock) {
    let Some(dir) = dir() else { return };
    let path = dir.join(format!("{}.json", block.id));
    let Ok(bytes) = serde_json::to_vec_pretty(block) else {
        return;
    };
    let existed = match std::fs::read(&path) {
        Ok(current) if current == bytes => return,
        Ok(_) => true,
        Err(_) => false,
    };
    if write_private(&path, &bytes).is_err() {
        tracing::warn!("lean-ctx gateway: withheld block explanation not stored");
        return;
    }
    if !existed {
        prune(&dir);
    }
}

fn prune(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut records: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect();
    if records.len() <= MAX_RECORDS {
        return;
    }
    records.sort();
    let excess = records.len() - MAX_RECORDS;
    for (_, path) in records.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

/// The recorded explanation of `id`.
#[must_use]
pub fn lookup(id: &str) -> Option<WithheldBlock> {
    if !valid_id(id) {
        return None;
    }
    let bytes = std::fs::read(dir()?.join(format!("{id}.json"))).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Every recorded explanation, ordered by id (deterministic output).
#[must_use]
pub fn list() -> Vec<WithheldBlock> {
    let Some(dir) = dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut blocks: Vec<WithheldBlock> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            lookup(name.strip_suffix(".json")?)
        })
        .collect();
    blocks.sort_by(|a, b| a.id.cmp(&b.id));
    blocks
}

fn grant_mac(id: &str, provider: &str, expires_unix: u64) -> Option<String> {
    super::receipt_store::keyed::grant_ref(&[
        id.as_bytes(),
        provider.as_bytes(),
        &expires_unix.to_be_bytes(),
    ])
    .ok()
}

/// Store a grant for `block` lasting `hours`. Called only by the human path
/// of the CLI, after its presence checks.
pub fn write_grant(block: &WithheldBlock, hours: u64) -> Result<Grant, String> {
    store_grant(block, now_unix().saturating_add(hours.saturating_mul(3600)))
}

pub(crate) fn store_grant(block: &WithheldBlock, expires_unix: u64) -> Result<Grant, String> {
    let mac = grant_mac(&block.id, &block.provider, expires_unix)
        .ok_or("the gateway receipt key is unavailable; nothing was allowed")?;
    let grant = Grant {
        id: block.id.clone(),
        provider: block.provider.clone(),
        expires_unix,
        mac,
    };
    let path = grants_dir()
        .ok_or("no lean-ctx data directory")?
        .join(format!("{}.json", block.id));
    let bytes = serde_json::to_vec_pretty(&grant).map_err(|e| e.to_string())?;
    write_private(&path, &bytes).map_err(|e| format!("grant not stored: {e}"))?;
    Ok(grant)
}

/// Withdraw the grant of `id`; `Ok(false)` when there was none.
pub fn revoke(id: &str) -> Result<bool, String> {
    if !valid_id(id) {
        return Err(format!("not a block id: {id}"));
    }
    let path = grants_dir()
        .ok_or("no lean-ctx data directory")?
        .join(format!("{id}.json"));
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

/// The current, authentic grant of `id`, if any.
#[must_use]
pub fn grant(id: &str) -> Option<Grant> {
    if !valid_id(id) {
        return None;
    }
    let bytes = std::fs::read(grants_dir()?.join(format!("{id}.json"))).ok()?;
    let grant: Grant = serde_json::from_slice(&bytes).ok()?;
    let authentic = grant.id == id
        && grant_mac(&grant.id, &grant.provider, grant.expires_unix).as_deref()
            == Some(grant.mac.as_str());
    (authentic && grant.expires_unix > now_unix()).then_some(grant)
}

/// Whether a human allowed block `id` to go to `provider`.
pub(crate) fn allowed(id: &str, provider: &str) -> bool {
    grant(id).is_some_and(|grant| grant.provider == provider)
}

#[cfg(test)]
pub(crate) mod tests_support {
    use super::{WithheldBlock, block_id, record};

    /// An isolated data directory (and with it a fresh receipt key).
    pub(crate) fn isolated() -> crate::core::data_dir::IsolatedDataDir {
        crate::core::data_dir::isolated_data_dir()
    }

    /// Record a withheld block with `codes` and return its id.
    pub(crate) fn seed_block(codes: &[&str]) -> String {
        let object = lean_ctx_protocol::Sha256Digest::new(format!("sha256:{}", "ab".repeat(32)))
            .expect("digest");
        let id = block_id(&object).expect("receipt key");
        record(&WithheldBlock {
            id: id.clone(),
            reason_codes: codes.iter().map(|code| (*code).to_owned()).collect(),
            position: "message 1".into(),
            provider: "anthropic".into(),
            locality: "remote".into(),
            mode: "developer".into(),
        });
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_keyed_and_well_formed() {
        let _env = tests_support::isolated();
        let object = lean_ctx_protocol::Sha256Digest::new(format!("sha256:{}", "cd".repeat(32)))
            .expect("digest");
        let id = block_id(&object).expect("id");
        assert!(valid_id(&id), "{id}");
        assert_eq!(block_id(&object).as_deref(), Some(id.as_str()), "stable");
        assert!(
            !id.contains(&"cd".repeat(6)),
            "keyed, not a prefix of the plain digest"
        );
        assert!(!valid_id("w-0123"));
        assert!(!valid_id("w-0123456789AB"));
        assert!(!valid_id("../../etc/passwd"));
    }

    #[test]
    fn records_are_content_free_and_listed_in_id_order() {
        let _env = tests_support::isolated();
        let id = tests_support::seed_block(&["destination.remote_restricted"]);
        let block = lookup(&id).expect("recorded");
        assert_eq!(block.position, "message 1");
        assert_eq!(list(), vec![block]);
        assert!(lookup("w-000000000000").is_none());
    }

    #[test]
    fn a_grant_holds_for_its_provider_until_it_expires_or_is_revoked() {
        let _env = tests_support::isolated();
        let id = tests_support::seed_block(&["destination.remote_restricted"]);
        let block = lookup(&id).expect("recorded");
        assert!(!allowed(&id, "anthropic"));
        write_grant(&block, 1).expect("grant");
        assert!(allowed(&id, "anthropic"));
        assert!(!allowed(&id, "openai"), "bound to the provider");
        assert_eq!(revoke(&id), Ok(true));
        assert!(!allowed(&id, "anthropic"));
        assert_eq!(revoke(&id), Ok(false));
        store_grant(&block, 1).expect("expired grant");
        assert!(
            !allowed(&id, "anthropic"),
            "an expired grant allows nothing"
        );
    }

    #[test]
    fn a_hand_written_grant_is_not_authentic() {
        let _env = tests_support::isolated();
        let id = tests_support::seed_block(&["destination.remote_restricted"]);
        let path = grants_dir().expect("dir").join(format!("{id}.json"));
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        let forged = serde_json::json!({
            "id": id, "provider": "anthropic", "expires_unix": u64::MAX, "mac": "blake3k:00"
        });
        std::fs::write(&path, forged.to_string()).expect("forged");
        assert!(!allowed(&id, "anthropic"));
    }
}
