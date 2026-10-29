// SPDX-License-Identifier: Apache-2.0
//! Redacted secrets per detector (K21 after the K2 review, H1): how often a
//! protection fired, by detector name. No fingerprint, hash or any other
//! value-derived datum is recorded or sent.
//!
//! While engine metrics are on, a switch file under
//! `<data_dir>/hive/metrics-v2/` (0600) lets the redaction path append
//! `{ts, detector, project}` to `secret-hits.jsonl` there (bounded). The
//! switch is checked on every record (K2 L1) and removed with the buffer
//! when sharing turns off.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lean_ctx_protocol::hive::metrics_v2::DetectorName;
use serde::{Deserialize, Serialize};

use super::buffer::dir_in;
use crate::core::hive::HiveError;
use crate::core::hive::state::write_private;

const SWITCH_FILE: &str = "secret-hits.on";
pub(crate) const HITS_FILE: &str = "secret-hits.jsonl";
/// The log stops growing here; the collector prunes what it has read.
const MAX_HITS_BYTES: u64 = 4 * 1024 * 1024;
/// How long the metrics directory (not the switch) is cached.
const DIR_CACHE: Duration = Duration::from_secs(60);

/// A redaction label (`AWS access key`, `Bearer token`, …) as one of the
/// contract's detectors; anything else is `other`.
pub(crate) fn detector(label: &str) -> DetectorName {
    let l = label.to_ascii_lowercase();
    let has = |w: &str| l.contains(w);
    let name = if has("aws") && has("secret") {
        "aws_secret_key"
    } else if has("aws") {
        "aws_access_key"
    } else if has("gcp") || has("google") {
        "gcp_key"
    } else if has("azure") {
        "azure_key"
    } else if has("github") {
        "github_token"
    } else if has("gitlab") {
        "gitlab_token"
    } else if has("slack") {
        "slack_token"
    } else if has("stripe") {
        "stripe_key"
    } else if has("openai") {
        "openai_key"
    } else if has("anthropic") {
        "anthropic_key"
    } else if has("private key") {
        "private_key"
    } else if has("jwt") {
        "jwt"
    } else if has("password") {
        "password_assignment"
    } else if has("database") || has("url credentials") || has("connection") {
        "connection_string"
    } else if has("api key") || has("token") || has("secret") || has("key") {
        "generic_api_key"
    } else {
        "other"
    };
    DetectorName::or_other(name)
}

/// Let the redaction path record (sharing on).
pub(crate) fn enable(hive_data: &Path) -> Result<(), HiveError> {
    write_private(&dir_in(hive_data), SWITCH_FILE, b"on\n")
}

pub(crate) fn enabled(hive_data: &Path) -> bool {
    dir_in(hive_data).join(SWITCH_FILE).is_file()
}

/// Remove switch and log (sharing off, delete).
pub(crate) fn remove(hive_data: &Path) {
    let dir = dir_in(hive_data);
    let _ = std::fs::remove_file(dir.join(SWITCH_FILE));
    let _ = std::fs::remove_file(dir.join(HITS_FILE));
    let _ = std::fs::remove_file(dir.join(super::signals::SIGNALS_FILE));
}

/// One recorded hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Hit {
    pub(crate) ts: String,
    pub(crate) detector: DetectorName,
    #[serde(default)]
    pub(crate) project: Option<String>,
}

/// The store and whether the slower checks allowed recording, cached.
type Cached = (Instant, crate::core::hive::HiveStore, bool);
static STORE: Mutex<Option<Cached>> = Mutex::new(None);

/// The metrics dir, only while the switch file exists, this engine is
/// connected and not revoked (K2 review 2, N1: checked on every record),
/// the `[hive]` switches allow it and the lease has not lapsed (K2 review
/// 3, NEW-3: checked with the cached store, at most a minute old; the
/// redaction path is hot). The switch also lets the K10/K12/K13 counters
/// record ([`super::signals`]).
pub(super) fn sink() -> Option<PathBuf> {
    // Engine metrics are Hive's: while the Hive release switch is off nothing
    // records, and the hot shell path never opens the Hive store.
    if !crate::core::hive::available_locally() {
        return None;
    }
    let mut cache = STORE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (store, allowed) = match cache.as_ref() {
        Some((at, store, allowed)) if at.elapsed() < DIR_CACHE => (store.clone(), *allowed),
        _ => {
            let store = crate::core::hive::HiveStore::open().ok()?;
            let allowed = store.recording_allowed(&crate::core::config::Config::load().hive);
            *cache = Some((Instant::now(), store.clone(), allowed));
            (store, allowed)
        }
    };
    drop(cache);
    let dir = dir_in(store.data_dir());
    (allowed && dir.join(SWITCH_FILE).is_file() && store.may_record()).then_some(dir)
}

/// Record redacted secrets by their labels. A no-op unless engine metrics
/// are on. Never fails the redaction; never sees the secret value.
pub(crate) fn note(labels: &[&str]) {
    if labels.is_empty() {
        return;
    }
    let Some(dir) = sink() else {
        return;
    };
    let path = dir.join(HITS_FILE);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_HITS_BYTES) {
        return;
    }
    let project = crate::core::savings_ledger::project_scope::current_project_key();
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut lines = String::new();
    for label in labels {
        let hit = Hit {
            ts: ts.clone(),
            detector: detector(label),
            project: project.clone(),
        };
        if let Ok(line) = serde_json::to_string(&hit) {
            lines.push_str(&line);
            lines.push('\n');
        }
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path);
    if let Ok(mut file) = file {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        let _ = file.write_all(lines.as_bytes());
    }
}

/// Every hit recorded under `hive_data`.
pub(crate) fn hits(hive_data: &Path) -> Vec<(chrono::DateTime<chrono::Utc>, Hit)> {
    let Ok(text) = std::fs::read_to_string(dir_in(hive_data).join(HITS_FILE)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<Hit>(line).ok())
        .filter_map(|hit| {
            let at = chrono::DateTime::parse_from_rfc3339(&hit.ts)
                .ok()?
                .with_timezone(&chrono::Utc);
            Some((at, hit))
        })
        .collect()
}

/// Drop hits older than `before` (closed hours are built once).
pub(crate) fn prune(hive_data: &Path, before: chrono::DateTime<chrono::Utc>) {
    let path = dir_in(hive_data).join(HITS_FILE);
    let kept: String = hits(hive_data)
        .into_iter()
        .filter(|(at, _)| *at >= before)
        .filter_map(|(_, hit)| serde_json::to_string(&hit).ok())
        .map(|line| line + "\n")
        .collect();
    if kept.is_empty() {
        let _ = std::fs::remove_file(path);
    } else {
        let _ = write_private(&dir_in(hive_data), HITS_FILE, kept.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_map_to_closed_detectors_and_nothing_value_derived_is_kept() {
        assert_eq!(detector("AWS access key").as_str(), "aws_access_key");
        assert_eq!(detector("Generic long secret").as_str(), "generic_api_key");
        assert_eq!(
            detector("Database credentials").as_str(),
            "connection_string"
        );
        assert_eq!(detector("Private key block").as_str(), "private_key");
        assert_eq!(detector("中文").as_str(), "other");
        let hit = Hit {
            ts: "2026-10-26T16:00:00Z".into(),
            detector: detector("GitHub token"),
            project: None,
        };
        let json = serde_json::to_value(&hit).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 3, "{json}");
    }
}
