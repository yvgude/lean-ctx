// SPDX-License-Identifier: Apache-2.0

//! Whether LeanCTX Code, the free desktop app, manages this machine
//! (contract: `docs/contracts/app-presence-v1.md`).
//!
//! The app writes one small marker file into the LeanCTX data directory on
//! every start that finds a local LeanCTX, and never removes it: a stale
//! marker still reads as "managed". Nothing is guessed from processes, ports
//! or environment variables: no marker, or a marker that does not match the
//! v1 schema, means "not managed". The marker carries no path, account or
//! secret, only the schema and the app id. `open_url` is reserved for a later
//! version (v1 writers omit it); a value that passes `valid_open_url` is
//! passed through, anything else is dropped.

use std::io::Read;
use std::path::{Path, PathBuf};

/// The schema the marker and the `/api/app` answer carry.
pub const SCHEMA: &str = "leanctx.app-presence/v1";
/// The app id written into the marker.
pub const APP_ID: &str = "leanctx-code";
/// The app's public name.
pub const APP_NAME: &str = "LeanCTX Code";
/// The only scheme a reserved `open_url` may use (not registered in v1).
pub const OPEN_URL_SCHEME: &str = "leanctx://";

/// Markers larger than this are not read.
const MAX_MARKER_BYTES: u64 = 4096;
const MAX_OPEN_URL_BYTES: usize = 256;

/// LeanCTX Code manages this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPresence {
    /// A `leanctx://` link that opens the app. Reserved: always `None` for a
    /// v1 marker, which omits `open_url`.
    pub open_url: Option<String>,
}

#[derive(serde::Deserialize)]
struct Marker {
    schema: String,
    app: String,
    #[serde(default)]
    open_url: Option<String>,
}

/// Where the marker lives inside a data directory.
#[must_use]
pub fn marker_path_in(data_dir: &Path) -> PathBuf {
    data_dir.join("app").join("leanctx-code.json")
}

/// The presence recorded in `data_dir`, or `None` without a valid marker.
#[must_use]
pub fn detect_in(data_dir: &Path) -> Option<AppPresence> {
    let path = marker_path_in(data_dir);
    // The marker is the app's own file: a link is never followed.
    if !std::fs::symlink_metadata(&path).ok()?.file_type().is_file() {
        return None;
    }
    // One open handle for the type check, the size check and a bounded read.
    let file = open_no_follow(&path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_MARKER_BYTES {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_MARKER_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > MAX_MARKER_BYTES {
        return None;
    }
    let marker: Marker = serde_json::from_str(&text).ok()?;
    if marker.schema != SCHEMA || marker.app != APP_ID {
        return None;
    }
    Some(AppPresence {
        open_url: marker.open_url.filter(|url| valid_open_url(url)),
    })
}

/// Opens `path` for reading without following a symlink in its last component
/// (`O_NOFOLLOW` on Unix).
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NOFOLLOW);
    options.open(path)
}

/// The presence recorded in this installation's data directory.
#[must_use]
pub fn detect() -> Option<AppPresence> {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
    detect_in(&data_dir)
}

/// A `leanctx://` link of plain URL characters, safe to put into an HTML
/// attribute as it is.
#[must_use]
pub fn valid_open_url(url: &str) -> bool {
    url.len() <= MAX_OPEN_URL_BYTES
        && url.starts_with(OPEN_URL_SCHEME)
        && url
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-?=:".contains(&b))
}

/// The `/api/app` answer: whether LeanCTX Code manages this machine and the
/// link that opens it. A deterministic function of the marker.
#[must_use]
pub fn status_json(presence: Option<&AppPresence>) -> String {
    serde_json::json!({
        "schema": SCHEMA,
        "managed": presence.is_some(),
        "app": APP_NAME,
        "open_url": presence.and_then(|p| p.open_url.clone()),
    })
    .to_string()
}
