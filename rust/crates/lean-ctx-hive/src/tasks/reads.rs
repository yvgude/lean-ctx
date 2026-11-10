// SPDX-License-Identifier: Apache-2.0
//! Task reads shared by every process of this Instance (owner 09.11.: Hive
//! takes large request volumes and does not block at the first limit).
//!
//! Hive counts task reads per Instance (600 per 15 min). Thirteen Queens,
//! their workers, the relay catch-up and the sync each used to ask on their
//! own, every one a new process, so one busy hour used the budget up and
//! every caller got 429. Now:
//!
//! - A list or task younger than [`READ_TTL`] is answered from
//!   `<data>/hive/task-reads.json` (0600) without a request.
//! - One fetch per list or task at a time (single flight): callers that
//!   arrive while it runs wait for it under a file lock and take its answer.
//! - A 429 holds every task read of this Instance until its `Retry-After`
//!   (or [`DEFAULT_HOLD`]); meanwhile, and when Hive is not reachable, the
//!   last good answer is served marked [`Stale`] with its age. Without one
//!   the caller gets [`HiveError::RateLimited`] with the seconds left, never
//!   "not found".
//! - A change of this machine (claim, update, verify, propose…) or a task
//!   event from Hive outdates the vault's entries ([`outdate`]).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::HiveError;
use crate::state::{load_or_quarantine, lock_state, lock_state_within, write_private};

const READS_FILE: &str = "task-reads.json";
/// An answer younger than this is served without a request.
pub const READ_TTL: Duration = Duration::from_secs(15);
/// How long a 429 without `Retry-After` holds task reads.
pub const DEFAULT_HOLD: Duration = Duration::from_secs(60);
/// The longest hold one 429 sets (a wrong header never parks reads longer).
const MAX_HOLD: Duration = Duration::from_mins(15);
/// How long a caller waits for a fetch of the same entry already running.
const FLIGHT_WAIT: Duration = Duration::from_secs(7);
/// Entries older than this are dropped when the file is written.
const KEEP: Duration = Duration::from_hours(24);

/// Flight locks are spread over this many files (`task-fetch-<n>.lock`),
/// so they do not grow with every list query and task number read.
const FLIGHT_SLOTS: u64 = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stamped {
    /// When the fetch that brought the answer started: Hive may have built
    /// it from any state since then.
    at: DateTime<Utc>,
    /// A change after `at` (this machine's write, a task event).
    #[serde(default)]
    outdated: bool,
    value: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Reads {
    /// Hive URL → until when it refuses task reads of this Instance (429).
    #[serde(default)]
    holds: BTreeMap<String, DateTime<Utc>>,
    /// Vault id → when a change last outdated its answers: an answer whose
    /// fetch started before that is not current, also one stored after the
    /// change by a fetch that was already running.
    #[serde(default)]
    outdated_at: BTreeMap<String, DateTime<Utc>>,
    /// Vault id → entry key (`list?state=open`, `task#376`) → answer.
    #[serde(default)]
    vaults: BTreeMap<String, BTreeMap<String, Stamped>>,
}

/// Why an answer is not current.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stale {
    /// Seconds since Hive sent it.
    pub age_s: u64,
    /// `rate_limited` (Hive holds task reads) or `unavailable` (not reachable).
    pub reason: &'static str,
    /// Seconds until Hive takes task reads again (`rate_limited`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_s: Option<u64>,
}

/// An answer and, when it is not current, why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read<T> {
    pub value: T,
    pub stale: Option<Stale>,
}

impl<T> Read<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Read<U> {
        Read {
            value: f(self.value),
            stale: self.stale,
        }
    }
}

/// The value of a current answer; a stale one is the error it stands in
/// for (rate limited with the seconds left, or Hive not reachable), for
/// callers that must not act on old task state.
pub fn current<T>(read: Read<T>) -> Result<T, HiveError> {
    match read.stale {
        None => Ok(read.value),
        Some(Stale {
            reason: "rate_limited",
            retry_after_s,
            ..
        }) => Err(HiveError::RateLimited {
            retry_after: retry_after_s,
        }),
        Some(stale) => Err(HiveError::Offline(format!(
            "Hive not reachable; the last task answer is {}s old",
            stale.age_s
        ))),
    }
}

impl Stale {
    /// One line for people: `stale: Hive's answer from 40s ago (rate
    /// limited, reads again in 320s)`.
    #[must_use]
    pub fn note(&self) -> String {
        let why = match (self.reason, self.retry_after_s) {
            ("rate_limited", Some(s)) => format!("rate limited, reads again in {s}s"),
            ("rate_limited", None) => "rate limited".to_string(),
            _ => "Hive not reachable".to_string(),
        };
        format!("stale: Hive's answer from {}s ago ({why})", self.age_s)
    }
}

fn load(data_dir: &Path) -> Reads {
    load_or_quarantine(&data_dir.join(READS_FILE), |_: &Reads| true)
        .unwrap_or_else(|error| {
            tracing::warn!("hive task reads: {error}");
            None
        })
        .unwrap_or_default()
}

/// Change the file under its lock; never around a request.
fn mutate(data_dir: &Path, change: impl FnOnce(&mut Reads)) {
    let saved = lock_state(data_dir, READS_FILE).and_then(|_lock| {
        let mut reads = load(data_dir);
        change(&mut reads);
        let keep = chrono::Duration::from_std(KEEP).unwrap_or_else(|_| chrono::Duration::days(1));
        let oldest = Utc::now() - keep;
        for entries in reads.vaults.values_mut() {
            entries.retain(|_, e| e.at > oldest);
        }
        reads.vaults.retain(|_, entries| !entries.is_empty());
        let now = Utc::now();
        reads.outdated_at.retain(|_, at| *at > oldest);
        reads.holds.retain(|_, until| *until > now);
        let bytes = serde_json::to_vec(&reads).map_err(|e| HiveError::Local(e.to_string()))?;
        write_private(data_dir, READS_FILE, &bytes)
    });
    if let Err(error) = saved {
        tracing::debug!("hive task reads not saved: {error}");
    }
}

/// Outdate every answer kept for `vault_id`, and every answer a fetch
/// running now stores later: the next read asks Hive. Written even when
/// nothing is kept yet, since a first fetch may be running.
pub(crate) fn outdate(data_dir: &Path, vault_id: &str) {
    mutate(data_dir, |reads| {
        reads.outdated_at.insert(vault_id.to_string(), Utc::now());
        if let Some(entries) = reads.vaults.get_mut(vault_id) {
            for e in entries.values_mut() {
                e.outdated = true;
            }
        }
    });
}

/// Seconds until Hive at `hive` takes task reads again, `None` when it does.
fn held(reads: &Reads, hive: &str, now: DateTime<Utc>) -> Option<u64> {
    let until = *reads.holds.get(hive)?;
    (until > now).then(|| {
        u64::try_from((until - now).num_seconds())
            .unwrap_or(0)
            .max(1)
    })
}

fn age_s(entry: &Stamped, now: DateTime<Utc>) -> u64 {
    u64::try_from((now - entry.at).num_seconds()).unwrap_or(0)
}

/// Current: not outdated, its fetch started after the vault's last change
/// (`outdated_at`) and less than [`READ_TTL`] ago.
fn fresh(entry: &Stamped, outdated_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    !entry.outdated
        && outdated_at.is_none_or(|changed| entry.at > changed)
        && chrono::Duration::from_std(READ_TTL).is_ok_and(|ttl| entry.at + ttl > now)
        && entry.at <= now + chrono::Duration::seconds(5)
}

fn decode<T: DeserializeOwned>(entry: &Stamped) -> Option<T> {
    serde_json::from_value(entry.value.clone()).ok()
}

/// The kept answer for `key`, served stale for `reason`.
fn stale<T: DeserializeOwned>(
    entry: Option<&Stamped>,
    now: DateTime<Utc>,
    reason: &'static str,
    retry_after_s: Option<u64>,
) -> Option<Read<T>> {
    let entry = entry?;
    Some(Read {
        value: decode(entry)?,
        stale: Some(Stale {
            age_s: age_s(entry, now),
            reason,
            retry_after_s,
        }),
    })
}

/// The flight lock's name for one entry: one of [`FLIGHT_SLOTS`] fixed
/// names (`task-fetch-<n>`), so the lock files stay bounded. Two entries
/// that share a slot only take turns; each still checks its own answer.
fn flight(vault_id: &str, key: &str) -> String {
    let digest = blake3::hash(format!("{vault_id}\n{key}").as_bytes());
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest.as_bytes()[..8]);
    format!("task-fetch-{}", u64::from_le_bytes(first) % FLIGHT_SLOTS)
}

/// The answer for `key` of `vault_id`: kept and fresh, else fetched once
/// for every caller of this Instance, else (429, Hive not reachable) the
/// last good one marked stale. `hive` (the Hive URL) scopes a 429 hold to
/// the Hive that sent it.
pub(crate) fn read_through<T: Serialize + DeserializeOwned>(
    data_dir: &Path,
    hive: &str,
    vault_id: &str,
    key: &str,
    fetch: impl FnOnce() -> Result<T, HiveError>,
) -> Result<Read<T>, HiveError> {
    let lookup = |reads: &Reads| {
        reads
            .vaults
            .get(vault_id)
            .and_then(|entries| entries.get(key))
            .cloned()
    };
    let current = |reads: &Reads, now| -> Option<Read<T>> {
        let entry = lookup(reads)?;
        fresh(&entry, reads.outdated_at.get(vault_id).copied(), now)
            .then(|| decode(&entry))
            .flatten()
            .map(|value| Read { value, stale: None })
    };
    let held_answer = |reads: &Reads, now, left: u64| -> Result<Read<T>, HiveError> {
        stale(lookup(reads).as_ref(), now, "rate_limited", Some(left)).ok_or(
            HiveError::RateLimited {
                retry_after: Some(left),
            },
        )
    };

    let reads = load(data_dir);
    let now = Utc::now();
    if let Some(answer) = current(&reads, now) {
        return Ok(answer);
    }
    if let Some(left) = held(&reads, hive, now) {
        return held_answer(&reads, now, left);
    }
    // One fetch per entry: a caller arriving while it runs waits for it
    // and takes its answer. A fetch that outlives the wait is not waited
    // for longer: the caller serves the kept answer or asks itself.
    let _flight = lock_state_within(data_dir, &flight(vault_id, key), FLIGHT_WAIT);
    let reads = load(data_dir);
    let now = Utc::now();
    if let Some(answer) = current(&reads, now) {
        return Ok(answer);
    }
    if let Some(left) = held(&reads, hive, now) {
        return held_answer(&reads, now, left);
    }
    // Stamped with the start: a change while the request runs (another
    // process claims, `outdate`) leaves the stored answer not current.
    let started = Utc::now();
    match fetch() {
        Ok(value) => {
            if let Ok(json) = serde_json::to_value(&value) {
                mutate(data_dir, |reads| {
                    reads
                        .vaults
                        .entry(vault_id.to_string())
                        .or_default()
                        .insert(
                            key.to_string(),
                            Stamped {
                                at: started,
                                outdated: false,
                                value: json,
                            },
                        );
                });
            }
            Ok(Read { value, stale: None })
        }
        Err(HiveError::RateLimited { retry_after }) => {
            let hold = retry_after
                .map(Duration::from_secs)
                .unwrap_or(DEFAULT_HOLD)
                .clamp(Duration::from_secs(1), MAX_HOLD);
            let until = Utc::now()
                + chrono::Duration::from_std(hold)
                    .unwrap_or_else(|_| chrono::Duration::seconds(60));
            mutate(data_dir, |reads| {
                let hold = reads.holds.entry(hive.to_string()).or_insert(until);
                *hold = (*hold).max(until);
            });
            let left = hold.as_secs().max(1);
            held_answer(&reads, Utc::now(), left)
        }
        Err(error) if error.is_transient() => {
            stale(lookup(&reads).as_ref(), Utc::now(), "unavailable", None).ok_or(error)
        }
        Err(error) => Err(error),
    }
}

/// Seconds until Hive at `hive` takes this Instance's task reads again
/// (`None`: now).
pub fn hold_left(data_dir: &Path, hive: &str) -> Option<u64> {
    held(&load(data_dir), hive, Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    const HIVE: &str = "https://hive.example";

    fn age(dir: &Path, vault: &str, key: &str, secs: i64) {
        mutate(dir, |reads| {
            let e = reads.vaults.get_mut(vault).unwrap().get_mut(key).unwrap();
            e.at -= chrono::Duration::seconds(secs);
        });
    }

    #[test]
    fn a_fresh_answer_is_served_without_a_request() {
        let dir = tempfile::tempdir().unwrap();
        let asked = AtomicUsize::new(0);
        let fetch = || {
            asked.fetch_add(1, Ordering::SeqCst);
            Ok::<_, HiveError>(vec![1u64, 2])
        };
        let first = read_through(dir.path(), HIVE, "vlt_a", "list", fetch).unwrap();
        let second = read_through(dir.path(), HIVE, "vlt_a", "list", || {
            asked.fetch_add(1, Ordering::SeqCst);
            Ok::<_, HiveError>(vec![9u64])
        })
        .unwrap();
        assert_eq!(first.value, vec![1, 2]);
        assert_eq!(
            second,
            Read {
                value: vec![1, 2],
                stale: None
            }
        );
        assert_eq!(
            asked.load(Ordering::SeqCst),
            1,
            "the second read asked Hive again"
        );
        // Aged past the TTL, or outdated by a change: asked again.
        age(dir.path(), "vlt_a", "list", 16);
        read_through(dir.path(), HIVE, "vlt_a", "list", || {
            Ok::<_, HiveError>(vec![3u64])
        })
        .unwrap();
        outdate(dir.path(), "vlt_a");
        let after = read_through(dir.path(), HIVE, "vlt_a", "list", || {
            Ok::<_, HiveError>(vec![4u64])
        })
        .unwrap();
        assert_eq!(after.value, vec![4]);
    }

    #[test]
    fn callers_at_the_same_time_share_one_request() {
        let dir = Arc::new(tempfile::tempdir().unwrap());
        let asked = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (dir, asked, start) = (dir.clone(), asked.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    read_through(dir.path(), HIVE, "vlt_a", "list?state=open", || {
                        asked.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(300));
                        Ok::<_, HiveError>(vec![7u64])
                    })
                    .unwrap()
                })
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap().value, vec![7]);
        }
        assert_eq!(
            asked.load(Ordering::SeqCst),
            1,
            "eight callers, one request"
        );
    }

    #[test]
    fn a_429_serves_the_last_good_answer_stale_and_holds_reads() {
        let dir = tempfile::tempdir().unwrap();
        read_through(dir.path(), HIVE, "vlt_a", "task#376", || {
            Ok::<_, HiveError>(376u64)
        })
        .unwrap();
        age(dir.path(), "vlt_a", "task#376", 40);
        let limited = read_through(dir.path(), HIVE, "vlt_a", "task#376", || {
            Err::<u64, _>(HiveError::RateLimited {
                retry_after: Some(320),
            })
        })
        .unwrap();
        assert_eq!(limited.value, 376, "the last good answer, not an error");
        let why = limited.stale.expect("marked stale");
        assert_eq!(why.reason, "rate_limited");
        assert!(why.age_s >= 40);
        assert_eq!(why.retry_after_s, Some(320));
        // While the hold lasts nothing is asked, for any entry of the Instance.
        let asked = AtomicUsize::new(0);
        let held = read_through(dir.path(), HIVE, "vlt_b", "list", || {
            asked.fetch_add(1, Ordering::SeqCst);
            Ok::<_, HiveError>(1u64)
        });
        assert_eq!(asked.load(Ordering::SeqCst), 0);
        match held {
            Err(HiveError::RateLimited {
                retry_after: Some(s),
            }) => assert!((1..=320).contains(&s)),
            other => panic!("expected rate limited, got {other:?}"),
        }
        assert!(hold_left(dir.path(), HIVE).is_some());
    }

    #[test]
    fn a_429_without_a_kept_answer_is_rate_limited_not_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let answer = read_through(dir.path(), HIVE, "vlt_a", "task#9", || {
            Err::<u64, _>(HiveError::RateLimited { retry_after: None })
        });
        match answer {
            Err(HiveError::RateLimited {
                retry_after: Some(s),
            }) => {
                assert_eq!(s, DEFAULT_HOLD.as_secs());
            }
            other => panic!("expected rate limited, got {other:?}"),
        }
        let held = hold_left(dir.path(), HIVE).expect("held");
        assert!(held <= DEFAULT_HOLD.as_secs());
    }

    #[test]
    fn hive_not_reachable_serves_the_kept_answer_and_not_found_stays_an_error() {
        let dir = tempfile::tempdir().unwrap();
        read_through(dir.path(), HIVE, "vlt_a", "list", || {
            Ok::<_, HiveError>(5u64)
        })
        .unwrap();
        age(dir.path(), "vlt_a", "list", 30);
        let offline = read_through(dir.path(), HIVE, "vlt_a", "list", || {
            Err::<u64, _>(HiveError::Offline("down".into()))
        })
        .unwrap();
        assert_eq!(offline.value, 5);
        assert_eq!(offline.stale.unwrap().reason, "unavailable");
        let gone = read_through(dir.path(), HIVE, "vlt_a", "task#1", || {
            Err::<u64, _>(HiveError::NotFound)
        });
        assert!(matches!(gone, Err(HiveError::NotFound)));
        assert_eq!(hold_left(dir.path(), HIVE), None, "only a 429 holds reads");
    }

    /// Review 5!246 #6: the flight locks are a bounded set of plain file
    /// names, not one file per list query or task number.
    #[test]
    fn flight_lock_names_carry_no_path_characters_and_stay_bounded() {
        let name = flight("vlt_a/../b", "list?state=open&priority=high");
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        );
        let names: std::collections::BTreeSet<String> = (0..5_000)
            .map(|n| flight("vlt_a", &format!("task#{n}")))
            .collect();
        assert!(names.len() as u64 <= FLIGHT_SLOTS, "{}", names.len());
        assert_eq!(flight("vlt_a", "task#1"), flight("vlt_a", "task#1"));
        let dir = tempfile::tempdir().unwrap();
        for n in 0..300u64 {
            read_through(dir.path(), HIVE, "vlt_a", &format!("task#{n}"), || {
                Ok::<_, HiveError>(n)
            })
            .unwrap();
        }
        let locks = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("task-fetch-"))
            .count();
        assert!(locks as u64 <= FLIGHT_SLOTS, "{locks} lock files");
    }

    /// Review 5!246 #3: a change while a fetch runs (another process
    /// claims) leaves the answer that fetch stores not current: the next
    /// read asks Hive instead of serving it for the TTL.
    #[test]
    fn an_outdate_during_a_fetch_is_not_lost() {
        let dir = tempfile::tempdir().unwrap();
        let during = read_through(dir.path(), HIVE, "vlt_a", "task#5", || {
            // Another process writes and outdates the vault meanwhile.
            std::thread::sleep(Duration::from_millis(5));
            outdate(dir.path(), "vlt_a");
            Ok::<_, HiveError>("before the write".to_string())
        })
        .unwrap();
        assert_eq!(during.value, "before the write");
        let asked = AtomicUsize::new(0);
        let next = read_through(dir.path(), HIVE, "vlt_a", "task#5", || {
            asked.fetch_add(1, Ordering::SeqCst);
            Ok::<_, HiveError>("after the write".to_string())
        })
        .unwrap();
        assert_eq!(
            asked.load(Ordering::SeqCst),
            1,
            "served the pre-write answer"
        );
        assert_eq!(next.value, "after the write");
        // A fetch that starts after the change is current again.
        let cached = read_through(dir.path(), HIVE, "vlt_a", "task#5", || {
            Ok::<_, HiveError>("asked again".to_string())
        })
        .unwrap();
        assert_eq!(cached.value, "after the write");
    }

    /// Review 5!246 #4: `tasks::list`/`show` (and the relay catch-up) take
    /// only a current answer; a stale one is the error it stands in for,
    /// never task state from before a claim or close.
    #[test]
    fn a_stale_answer_is_not_current() {
        let dir = tempfile::tempdir().unwrap();
        read_through(dir.path(), HIVE, "vlt_a", "list", || {
            Ok::<_, HiveError>(1u64)
        })
        .unwrap();
        outdate(dir.path(), "vlt_a");
        let held = read_through(dir.path(), HIVE, "vlt_a", "list", || {
            Err::<u64, _>(HiveError::RateLimited {
                retry_after: Some(30),
            })
        })
        .unwrap();
        assert!(held.stale.is_some(), "the outdated answer is served stale");
        match current(held) {
            Err(HiveError::RateLimited {
                retry_after: Some(30),
            }) => {}
            other => panic!("expected rate limited, got {other:?}"),
        }
        let offline = Read {
            value: 1u64,
            stale: Some(Stale {
                age_s: 9,
                reason: "unavailable",
                retry_after_s: None,
            }),
        };
        assert!(matches!(current(offline), Err(HiveError::Offline(_))));
        assert_eq!(
            current(Read {
                value: 1u64,
                stale: None
            })
            .unwrap(),
            1
        );
    }

    /// Review 5!246 #7: a 429 of one Hive does not hold reads of another.
    #[test]
    fn a_hold_is_scoped_to_the_hive_that_sent_it() {
        let dir = tempfile::tempdir().unwrap();
        let _ = read_through(dir.path(), HIVE, "vlt_a", "list", || {
            Err::<u64, _>(HiveError::RateLimited {
                retry_after: Some(300),
            })
        });
        assert!(hold_left(dir.path(), HIVE).is_some());
        let other = "https://hive.other.example";
        assert_eq!(hold_left(dir.path(), other), None);
        let answer = read_through(dir.path(), other, "vlt_b", "list", || {
            Ok::<_, HiveError>(2u64)
        })
        .unwrap();
        assert_eq!(answer.value, 2);
    }
}
