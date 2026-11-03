// SPDX-License-Identifier: Apache-2.0
//! One persistence form for the small content-free registers next to the
//! archive (`delivered`, `presented`, and the proxy's register of
//! provider-sealed items): 256 JSON shards under
//! `<data dir>/archives/<name>/<2 hex>.json`, owner-only, replaced atomically
//! under a bounded file lock, with no in-memory state — a second process reads
//! exactly what the first one wrote.
//!
//! Keys are 64 lowercase hex characters. Values are whatever the register
//! stores; this module never sees content and logs nothing.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Upper bound of one shard file; a larger file is not read.
pub(super) const MAX_SHARD_BYTES: u64 = 4 * 1024 * 1024;
/// How long an update waits for the register lock before it gives up.
const LOCK_WAIT: Duration = Duration::from_millis(250);

const SHARD_VERSION: u8 = 1;

/// The bounds of one register.
#[derive(Debug, Clone, Copy)]
pub struct Register {
    /// Directory name under the archive store.
    pub name: &'static str,
    /// Keys per shard; the oldest record is evicted first.
    pub shard_entries: usize,
    /// A record older than this is neither returned nor kept.
    pub max_age_seconds: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Shard<V> {
    version: u8,
    entries: BTreeMap<String, Stamped<V>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stamped<V> {
    recorded_at: i64,
    value: V,
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub(super) fn valid_key(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

impl Register {
    fn dir(&self) -> PathBuf {
        super::archive_base_dir().join(self.name)
    }

    pub(super) fn shard_path(&self, key: &str) -> PathBuf {
        self.dir().join(format!("{}.json", &key[..2]))
    }

    fn fresh<V>(&self, entry: &Stamped<V>, now: i64) -> bool {
        now.checked_sub(entry.recorded_at)
            .is_some_and(|age| (0..=self.max_age_seconds).contains(&age))
    }

    /// A missing shard is empty. An oversized, foreign or undecodable one is
    /// `None`: nothing is read from it.
    fn load<V: DeserializeOwned>(&self, key: &str) -> Option<Shard<V>> {
        let path = self.shard_path(key);
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Some(Shard {
                    version: SHARD_VERSION,
                    entries: BTreeMap::new(),
                });
            }
            Err(_) => return None,
            Ok(metadata) if metadata.len() > MAX_SHARD_BYTES => return None,
            Ok(_) => {}
        }
        let raw =
            crate::core::atomic_fs::read_store_file(&super::archive_base_dir(), &path).ok()?;
        if raw.len() as u64 > MAX_SHARD_BYTES {
            return None;
        }
        serde_json::from_str::<Shard<V>>(&raw)
            .ok()
            .filter(|shard| shard.version == SHARD_VERSION)
    }

    /// The value recorded under `key`, unless it is missing or expired.
    /// Readers take no lock: a shard is only ever replaced atomically.
    pub(super) fn read<V: DeserializeOwned>(&self, key: &str, now: i64) -> Option<V> {
        if !valid_key(key) {
            return None;
        }
        self.load::<V>(key)?
            .entries
            .remove(key)
            .filter(|entry| self.fresh(entry, now))
            .map(|entry| entry.value)
    }

    /// Replace the value under `key` with `merge(current)`. `merge` returning
    /// `None` records nothing and leaves the register as it is. `false`: not
    /// recorded (malformed key, a bound, the lock, the file system).
    ///
    /// An unreadable shard is started afresh: unknown is the safe state of
    /// everything it held.
    pub(super) fn update<V: Serialize + DeserializeOwned>(
        &self,
        key: &str,
        now: i64,
        merge: impl FnOnce(Option<V>) -> Option<V>,
    ) -> bool {
        if !valid_key(key) {
            return false;
        }
        let path = self.shard_path(key);
        self.locked(|| {
            let mut shard = self.load::<V>(key).unwrap_or(Shard {
                version: SHARD_VERSION,
                entries: BTreeMap::new(),
            });
            shard.entries.retain(|_, entry| self.fresh(entry, now));
            let current = shard.entries.remove(key).map(|entry| entry.value);
            let Some(value) = merge(current) else {
                return false;
            };
            shard.entries.insert(
                key.to_owned(),
                Stamped {
                    recorded_at: now,
                    value,
                },
            );
            while shard.entries.len() > self.shard_entries {
                let Some(oldest) = shard
                    .entries
                    .iter()
                    .filter(|(candidate, _)| candidate.as_str() != key)
                    .min_by_key(|(candidate, entry)| (entry.recorded_at, (*candidate).clone()))
                    .map(|(candidate, _)| candidate.clone())
                else {
                    break;
                };
                shard.entries.remove(&oldest);
            }
            let Ok(encoded) = serde_json::to_vec(&shard) else {
                return false;
            };
            encoded.len() as u64 <= MAX_SHARD_BYTES && write_private(&path, &encoded)
        })
        .unwrap_or(false)
    }

    /// Record many values at once under one lock, one write per touched
    /// shard: `merge(key, current, new)` gives the value kept under a key
    /// that already holds one. Keys that are malformed are skipped. `false`: the
    /// lock was not obtained or a shard could not be written (its keys are
    /// then not recorded; unknown is the safe state).
    pub fn update_many<V: Serialize + DeserializeOwned>(
        &self,
        now: i64,
        items: Vec<(String, V)>,
        merge: impl Fn(&str, V, V) -> V,
    ) -> bool {
        let mut by_shard: BTreeMap<String, Vec<(String, V)>> = BTreeMap::new();
        for (key, value) in items {
            if valid_key(&key) {
                by_shard
                    .entry(key[..2].to_owned())
                    .or_default()
                    .push((key, value));
            }
        }
        if by_shard.is_empty() {
            return true;
        }
        self.locked(|| {
            let mut written = true;
            for values in by_shard.into_values() {
                let Some((first, _)) = values.first() else {
                    continue;
                };
                let path = self.shard_path(first);
                let mut shard = self.load::<V>(first).unwrap_or(Shard {
                    version: SHARD_VERSION,
                    entries: BTreeMap::new(),
                });
                shard.entries.retain(|_, entry| self.fresh(entry, now));
                for (key, value) in values {
                    let value = match shard.entries.remove(&key) {
                        Some(current) => merge(&key, current.value, value),
                        None => value,
                    };
                    shard.entries.insert(
                        key,
                        Stamped {
                            recorded_at: now,
                            value,
                        },
                    );
                }
                // Bounded in count and in bytes: the oldest record goes first
                // until the shard fits, so one large shard never stalls every
                // later write to it until its records age out.
                let encoded = loop {
                    if shard.entries.len() <= self.shard_entries
                        && let Ok(encoded) = serde_json::to_vec(&shard)
                        && encoded.len() as u64 <= MAX_SHARD_BYTES
                    {
                        break Some(encoded);
                    }
                    let Some(oldest) = shard
                        .entries
                        .iter()
                        .min_by_key(|(candidate, entry)| (entry.recorded_at, (*candidate).clone()))
                        .map(|(candidate, _)| candidate.clone())
                    else {
                        break None;
                    };
                    shard.entries.remove(&oldest);
                };
                written &= encoded.is_some_and(|encoded| write_private(&path, &encoded));
            }
            written
        })
        .unwrap_or(false)
    }

    /// Every fresh record of every shard with the time it was recorded, for
    /// a holder that keeps the register in memory and loads it once. A shard
    /// that is oversized, foreign or undecodable contributes nothing.
    pub fn read_all<V: DeserializeOwned>(&self, now: i64) -> Vec<(String, i64, V)> {
        let mut out = Vec::new();
        for shard in 0..=u8::MAX {
            let probe = format!("{shard:02x}{}", "0".repeat(62));
            let Some(loaded) = self.load::<V>(&probe) else {
                continue;
            };
            for (key, entry) in loaded.entries {
                if valid_key(&key) && key[..2] == probe[..2] && self.fresh(&entry, now) {
                    out.push((key, entry.recorded_at, entry.value));
                }
            }
        }
        out
    }

    /// Run `operation` under the register's exclusive file lock, waiting at
    /// most [`LOCK_WAIT`]. `None`: directory or lock unavailable, or the
    /// budget ran out.
    fn locked<T>(&self, operation: impl FnOnce() -> T) -> Option<T> {
        use fs2::FileExt;

        let directory = self.dir();
        std::fs::create_dir_all(&directory).ok()?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let lock = options.open(directory.join(".lock")).ok()?;
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            if FileExt::try_lock_exclusive(&lock).is_ok() {
                break;
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let result = operation();
        let _ = FileExt::unlock(&lock);
        Some(result)
    }
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> bool {
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        Some(std::fs::Permissions::from_mode(0o600))
    };
    #[cfg(not(unix))]
    let permissions: Option<std::fs::Permissions> = None;
    crate::core::atomic_fs::try_atomic_write(path, bytes, permissions.as_ref()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shard over the byte bound evicts its oldest records until it fits,
    /// instead of refusing every later write until its records age out.
    #[test]
    fn a_shard_over_the_byte_bound_evicts_oldest_until_it_fits() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let store = Register {
            name: "bound-test",
            shard_entries: 64,
            max_age_seconds: 3600,
        };
        let large = "x".repeat(1024 * 1024);
        let key = |n: u8| format!("aa{n:062}");
        let now = now();
        let old: Vec<(String, String)> = (0..3).map(|n| (key(n), large.clone())).collect();
        assert!(store.update_many(now - 10, old, |_, _, new| new));
        let new: Vec<(String, String)> = (3..5).map(|n| (key(n), large.clone())).collect();
        assert!(
            store.update_many(now, new, |_, _, new| new),
            "the write fits after eviction"
        );
        let kept: Vec<String> = store
            .read_all::<String>(now)
            .into_iter()
            .map(|(key, _, _)| key)
            .collect();
        assert!(kept.contains(&key(3)) && kept.contains(&key(4)), "{kept:?}");
        assert!(!kept.contains(&key(0)), "the oldest record went first");
    }
}
