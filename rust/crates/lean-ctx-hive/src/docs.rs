// SPDX-License-Identifier: Apache-2.0
//! Docs in Hive (C0 2.2): the engine adapter of [`lean_ctx_doc`].
//!
//! The crate holds the model (pages, sections, notes, anchors, the
//! lowest-seq fold and the doc state); this module feeds it the replica's
//! verified events and turns its [`EventSpec`]s into signed class-W events,
//! queued in the replica like every other write, so offline is fine and the
//! next push sends them. Every string of every body passes the outbound
//! screen first; one blocked text refuses the whole batch and nothing is
//! queued.

use lean_ctx_doc::replica::{DocSpace, LogEvent, assemble};
use lean_ctx_doc::write::{self as doc_write, EventSpec};
use serde_json::{Map, Value, json};

use crate::HiveError;
use crate::capture::GATE_VERSION;
use crate::events::build::EVENT_SCHEMA;
use crate::events::hlc::{self, Stamp};
use crate::events::{ids, sign};
use crate::knowledge::screening::{SCANNER_VERSION, Screen};
use crate::replica::Replica;
use crate::review::maintain::{Target, queue};

fn local(e: impl std::fmt::Display) -> HiveError {
    HiveError::Local(e.to_string())
}

/// The replica's verified events in the crate's shape, then this engine's
/// queued events in HLC order after them: a write right after another one
/// (an edit, then a confirm) bases its CAS on the queued head, which Hive
/// has by then, and reads show the engine's own writes at once.
pub fn log_events(replica: &Replica) -> Result<Vec<LogEvent>, HiveError> {
    let mut out: Vec<LogEvent> = replica
        .fold_events()?
        .into_iter()
        .filter(|e| !e.sig_invalid)
        .map(|e| LogEvent {
            seq: e.seq,
            event_id: e.event_id,
            op: e.op,
            target: e.target,
            node_type: e.node_type,
            body: e.body,
        })
        .collect();
    let mut seq = out.iter().map(|e| e.seq).max().unwrap_or(0);
    for (event_id, bytes) in replica.pending()? {
        if out.iter().any(|e| e.event_id == event_id) {
            continue;
        }
        let Ok(event) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let text = |k: &str| event.get(k).and_then(Value::as_str).map(str::to_owned);
        let Some(op) = text("op") else {
            continue;
        };
        seq += 1;
        out.push(LogEvent {
            seq,
            event_id,
            op,
            target: text("target"),
            node_type: text("node_type"),
            body: event.get("body").cloned(),
        });
    }
    Ok(out)
}

/// The doc pages of the place as the replica holds them.
pub fn space(replica: &Replica) -> Result<DocSpace, HiveError> {
    Ok(assemble(&log_events(replica)?))
}

/// A fresh id for the crate's writers: `n_…` for nodes, `e_…` for edges.
pub fn new_id(prefix: &str) -> String {
    // `new_random_id` only fails when the OS RNG does; fall back to an event
    // id, which is just as unique.
    ids::new_random_id(prefix).unwrap_or_else(|_| {
        let ms = chrono::Utc::now().timestamp_millis().unsigned_abs();
        format!(
            "{prefix}_{}",
            ids::new_event_id(ms).unwrap_or_default().to_lowercase()
        )
    })
}

/// Every string in `value`, for the screen.
fn strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) => out.push(s),
        Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
        Value::Object(m) => m.values().for_each(|v| strings(v, out)),
        _ => {}
    }
}

/// Screens every body of `specs`; the first blocked text refuses all.
pub fn screen_all(specs: &[EventSpec], screen: &mut Screen) -> Result<(), HiveError> {
    for spec in specs {
        let mut texts = Vec::new();
        for v in spec.body.values() {
            strings(v, &mut texts);
        }
        for t in texts {
            screen.check(t).map_err(|why| {
                HiveError::Refused(format!(
                    "screened: nothing of this doc write was queued ({why})"
                ))
            })?;
        }
    }
    Ok(())
}

fn reasons_hash(spec: &EventSpec) -> String {
    let kind = spec
        .node_type
        .or_else(|| spec.body.get("field").and_then(Value::as_str))
        .or_else(|| spec.body.get("kind").and_then(Value::as_str))
        .unwrap_or("doc");
    blake3::hash(format!("leanctx.hive.docs.v1:{}:{kind}", spec.op).as_bytes())
        .to_hex()
        .to_string()
}

/// The signed event of `spec` with `event_id` at `stamp`.
fn build(
    target: &Target<'_>,
    spec: &EventSpec,
    event_id: &str,
    stamp: Stamp,
) -> Result<Value, HiveError> {
    let place = target.place;
    let mut body: Map<String, Value> = spec.body.clone();
    body.insert("body_nonce".into(), Value::String(sign::new_body_nonce()?));
    let body = Value::Object(body);
    let body_commit = sign::body_commit(&place.pk_id, &body).map_err(local)?;
    // An engine's own adds are `own`; edits of a page anyone may have
    // written are the workspace's, as every curation.
    let claim = if matches!(spec.op, "node.add" | "edge.add") {
        "own"
    } else {
        "workspace"
    };
    let mut event = json!({
        "schema": EVENT_SCHEMA,
        "event_id": event_id,
        "place": {"kind": place.kind, "id": place.id},
        "op": spec.op,
        "target": spec.target,
        "body": body,
        "body_commit": body_commit,
        "hlc": {"wall_ms": stamp.wall_ms, "counter": stamp.counter, "node": target.signer.hlc_node},
        "causal_base": place.causal_base,
        "actor": {"kind": "worker"},
        "worker_id": target.signer.worker_id,
        "provenance_claim": claim,
        "capture": {
            "gate_version": GATE_VERSION,
            "evidence": "observed",
            "reasons_passed_hash": reasons_hash(spec),
        },
        "screening": {
            "counts": {"forbidden": 0, "local_path": 0, "pii": 0, "redaction": 0, "secret": 0},
            "scanner_version": SCANNER_VERSION,
        },
    });
    if let Some(t) = spec.node_type {
        event["node_type"] = Value::from(t);
    }
    if let Some(base) = &spec.base_event {
        event["base_event"] = Value::from(base.as_str());
    }
    sign::sign(&mut event, target.signer.key).map_err(local)?;
    Ok(event)
}

/// Screens, chains, signs and queues `specs` in order. Returns the queued
/// event ids; nothing is queued when one body is screened out.
pub fn queue_specs(
    target: &Target<'_>,
    specs: Vec<EventSpec>,
    screen: &mut Screen,
    now_ms: u64,
) -> Result<Vec<String>, HiveError> {
    queue_specs_at(target, specs, screen, now_ms, Stamp::default()).map(|(ids, _)| ids)
}

/// [`queue_specs`] continuing the HLC `clock` of events queued before it in
/// the same run (the sync's re-checks); returns the clock after the last.
pub fn queue_specs_at(
    target: &Target<'_>,
    mut specs: Vec<EventSpec>,
    screen: &mut Screen,
    now_ms: u64,
    clock: Stamp,
) -> Result<(Vec<String>, Stamp), HiveError> {
    screen_all(&specs, screen)?;
    let mut stamp = clock;
    let mut stamps = Vec::with_capacity(specs.len());
    let mut event_ids = Vec::with_capacity(specs.len());
    for _ in &specs {
        stamp = hlc::send(stamp, now_ms);
        event_ids.push(ids::new_event_id(stamp.wall_ms)?);
        stamps.push(stamp);
    }
    doc_write::chain(&mut specs, &event_ids);
    let events = specs
        .iter()
        .zip(&event_ids)
        .zip(&stamps)
        .map(|((spec, id), stamp)| build(target, spec, id, *stamp).map(|e| (e, *stamp)))
        .collect::<Result<Vec<_>, _>>()?;
    for (event, stamp) in events {
        queue(target, &event, stamp)?;
    }
    Ok((event_ids, stamp))
}

/// The files git tracks in the checkout at `root` (repo-relative, `/`), the
/// file set every C0 2.2 anchor digest is computed over; `None` outside git.
pub fn tracked_files(root: &str) -> Option<Vec<String>> {
    let out = std::process::Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let mut files: Vec<String> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    files.sort();
    Some(files)
}

/// The commit the checkout is at, for an anchor's `commit`.
pub fn head_commit(root: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The digest `anchor` has in the checkout now (C0 2.2
/// `doc_anchor_digest`); `None` when a `file:` or `dir:` target is gone.
pub fn digest_now(root: &str, files: &[String], anchor: &lean_ctx_doc::Anchor) -> Option<String> {
    let dir_exists = std::path::Path::new(root).join(&anchor.path).is_dir();
    let mut read = |p: &str| {
        std::fs::read(std::path::Path::new(root).join(p))
            .ok()
            .map(|b| lean_ctx_doc::anchor::file_digest(&b))
    };
    digest_with(files, anchor, dir_exists, &mut read)
}

/// The digest of `anchor` over `files`, with `file_digest_of` giving each
/// file's digest (from the working tree or from a commit).
fn digest_with(
    files: &[String],
    anchor: &lean_ctx_doc::Anchor,
    dir_exists: bool,
    file_digest_of: &mut dyn FnMut(&str) -> Option<String>,
) -> Option<String> {
    use lean_ctx_doc::anchor::{glob_match, shape_digest, tree_digest};
    use lean_ctx_doc::{AnchorKind, AnchorMode};
    let selected: Vec<&String> = match anchor.kind {
        AnchorKind::File => {
            return files
                .contains(&anchor.path)
                .then(|| file_digest_of(&anchor.path))
                .flatten();
        }
        AnchorKind::Dir => {
            if !dir_exists {
                return None;
            }
            files
                .iter()
                .filter(|f| anchor.matches(f) && **f != anchor.path)
                .collect()
        }
        AnchorKind::Glob => files
            .iter()
            .filter(|f| glob_match(&anchor.path, f))
            .collect(),
    };
    // dir: relpaths are relative to the anchor dir, glob: to the repo root.
    let rel = |f: &str| -> String {
        if anchor.kind == AnchorKind::Dir && !anchor.path.is_empty() {
            f[anchor.path.trim_end_matches('/').len() + 1..].to_owned()
        } else {
            f.to_owned()
        }
    };
    Some(match anchor.mode {
        AnchorMode::Shape => shape_digest(&selected.iter().map(|f| rel(f)).collect::<Vec<_>>()),
        AnchorMode::Content => tree_digest(
            &selected
                .iter()
                .filter_map(|f| file_digest_of(f).map(|d| (rel(f), d)))
                .collect::<Vec<_>>(),
        ),
    })
}

/// Anchor digests at one commit: the tracked files of its tree, each file's
/// bytes read once through a single `git cat-file --batch`. The migrator pins
/// an emspace page at the commit emspace pinned it at, so the first re-check
/// against `HEAD` reproduces emspace's stale set.
pub struct GitAt {
    files: Vec<String>,
    blobs: std::collections::HashMap<String, String>,
    cache: std::collections::HashMap<String, Option<String>>,
    child: std::process::Child,
}

impl GitAt {
    /// `None` when `commit` is not in the checkout at `root`.
    pub fn open(root: &str, commit: &str) -> Option<Self> {
        use std::process::{Command, Stdio};
        let tree = Command::new("git")
            .args(["ls-tree", "-r", "-z", commit])
            .current_dir(root)
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        let mut files = Vec::new();
        let mut blobs = std::collections::HashMap::new();
        for entry in tree.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            // `<mode> blob <sha>\t<path>`
            let line = String::from_utf8_lossy(entry);
            let Some((meta, path)) = line.split_once('\t') else {
                continue;
            };
            let mut parts = meta.split(' ');
            if let (Some(_), Some("blob"), Some(sha)) = (parts.next(), parts.next(), parts.next()) {
                files.push(path.to_owned());
                blobs.insert(path.to_owned(), sha.to_owned());
            }
        }
        files.sort();
        let child = Command::new("git")
            .args(["cat-file", "--batch"])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        Some(Self {
            files,
            blobs,
            cache: std::collections::HashMap::new(),
            child,
        })
    }

    fn file_digest(&mut self, path: &str) -> Option<String> {
        use std::io::{BufRead, Read, Write};
        if let Some(d) = self.cache.get(path) {
            return d.clone();
        }
        let sha = self.blobs.get(path)?.clone();
        let digest = (|| {
            let stdin = self.child.stdin.as_mut()?;
            writeln!(stdin, "{sha}").ok()?;
            stdin.flush().ok()?;
            let stdout = self.child.stdout.as_mut()?;
            let mut reader = std::io::BufReader::new(stdout.by_ref());
            let mut header = String::new();
            reader.read_line(&mut header).ok()?;
            let size: usize = header.split_whitespace().nth(2)?.parse().ok()?;
            let mut body = vec![0u8; size + 1];
            reader.read_exact(&mut body).ok()?;
            body.truncate(size);
            Some(lean_ctx_doc::anchor::file_digest(&body))
        })();
        self.cache.insert(path.to_owned(), digest.clone());
        digest
    }

    /// The digest of `anchor` at this commit; `None` for a gone target.
    pub fn digest(&mut self, anchor: &lean_ctx_doc::Anchor) -> Option<String> {
        let prefix = format!("{}/", anchor.path.trim_end_matches('/'));
        let dir_exists =
            anchor.path.is_empty() || self.files.iter().any(|f| f.starts_with(&prefix));
        let files = std::mem::take(&mut self.files);
        let out = digest_with(&files, anchor, dir_exists, &mut |p| self.file_digest(p));
        self.files = files;
        out
    }
}

impl Drop for GitAt {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

/// Parses `file:…`, `dir:…` or `glob:…` (role `derived`, mode `content`;
/// `shape:` is a `dir:` in shape mode) and pins it at the checkout's digest.
pub fn pin(
    root: &str,
    files: &[String],
    reference: &str,
) -> Result<lean_ctx_doc::Anchor, HiveError> {
    use lean_ctx_doc::{Anchor, AnchorKind, AnchorMode, AnchorRole};
    let (scheme, path) = reference.split_once(':').ok_or_else(|| {
        HiveError::Config(format!(
            "anchor {reference}: use file:, dir:, glob: or shape:"
        ))
    })?;
    if path.contains("..") || path.starts_with('/') || path.contains(':') {
        return Err(HiveError::Config(format!(
            "anchor {reference}: repo-relative paths only"
        )));
    }
    let (kind, mode) = match scheme {
        "file" => (AnchorKind::File, AnchorMode::Content),
        "dir" => (AnchorKind::Dir, AnchorMode::Content),
        "shape" => (AnchorKind::Dir, AnchorMode::Shape),
        "glob" => (AnchorKind::Glob, AnchorMode::Content),
        _ => {
            return Err(HiveError::Config(format!(
                "anchor {reference}: use file:, dir:, glob: or shape:"
            )));
        }
    };
    let mut anchor = Anchor {
        kind,
        mode,
        role: AnchorRole::Derived,
        path: path.trim_end_matches('/').to_owned(),
        digest: String::new(),
        commit: head_commit(root),
    };
    anchor.digest = digest_now(root, files, &anchor)
        .ok_or_else(|| HiveError::Config(format!("anchor {reference}: not in the checkout")))?;
    Ok(anchor)
}

/// The anchors of a page as stored (`anchors` field), parsed back.
pub fn page_anchors(page: &lean_ctx_doc::replica::PageView) -> Vec<lean_ctx_doc::Anchor> {
    page.page
        .fields
        .get("anchors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(lean_ctx_doc::write::anchor_from_value)
        .collect()
}

/// The recheck of every page's derived anchors against the checkout: one
/// `anchor_state` + `drift` pair (CAS-exempt) per page whose verdict changed.
/// Matching bytes never write `fresh`: only a confirm does.
pub fn recheck(root: &str, space: &DocSpace) -> Vec<EventSpec> {
    let Some(files) = tracked_files(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for page in space.pages.values() {
        let mut drift: Vec<Value> = Vec::new();
        let mut gone = false;
        for a in page_anchors(page)
            .iter()
            .filter(|a| a.role == lean_ctx_doc::AnchorRole::Derived)
        {
            let now = digest_now(root, &files, a);
            if now.as_deref() != Some(a.digest.as_str()) {
                gone |= now.is_none();
                drift.push(json!({ "ref": a.reference(), "observed": now }));
            }
        }
        if drift.is_empty() {
            continue;
        }
        let state = if gone { "gone" } else { "to_verify" };
        let same_state = page.page.str("anchor_state") == Some(state);
        let same_drift = page.page.fields.get("drift") == Some(&Value::Array(drift.clone()));
        if same_state && same_drift {
            continue;
        }
        for (field, value) in [
            ("anchor_state", Value::from(state)),
            ("drift", Value::Array(drift)),
        ] {
            let mut body = Map::new();
            body.insert("field".into(), Value::from(field));
            body.insert("value".into(), value);
            out.push(EventSpec {
                op: "field.set",
                target: page.page.id.clone(),
                node_type: None,
                body,
                base_event: None,
            });
        }
    }
    out
}

/// The linked vault of a checkout, opened for reading and writing docs.
pub struct Vault<'a> {
    /// `@ns/name`, for messages.
    pub label: String,
    pub target: Target<'a>,
}

/// Doc writes need a C0 2.2 Hive (doc node types, `base_event` CAS): a
/// 2.0 or 2.1 Hive refuses them. The engine records the answered minor at
/// start (`relay_v2::protocol_minor`).
pub fn require_docs_protocol(store: &crate::HiveStore) -> Result<(), HiveError> {
    if crate::bus::relay_v2::speaks_2_2(store) {
        return Ok(());
    }
    let answered = crate::bus::relay_v2::protocol_minor(store).map_or_else(
        || "no 2.x version yet".to_owned(),
        |minor| format!("2.{minor}"),
    );
    Err(HiveError::Refused(format!(
        "doc pages need Hive-Protocol 2.2 and this Hive answered {answered}; the instance records it at start (restart it after a Hive upgrade)"
    )))
}

/// Opens the vault linked to the checkout at `root` and runs `f` on it.
/// Local only: nothing here contacts Hive.
pub fn with_vault<R>(
    root: &str,
    f: impl FnOnce(&Vault<'_>) -> Result<R, HiveError>,
) -> Result<R, HiveError> {
    use crate::sync::{place, writer};
    let store = crate::HiveStore::open()?;
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let record = crate::resolution::vault_for_root(store.data_dir(), std::path::Path::new(root))?
        .ok_or_else(|| {
            HiveError::Config(
                "this checkout is not linked to a Hive vault (`lean-ctx hive link <workspace>/<vault>`)"
                    .into(),
            )
        })?;
    let dir = crate::replica::store::place_dir(store.data_dir(), &record.vault_id);
    if !dir.join("replica.db").exists() {
        return Err(HiveError::Config(format!(
            "@{}/{} has no replica here yet: run `lean-ctx hive sync` first",
            record.ns, record.name
        )));
    }
    let config = lean_ctx::core::config::Config::load().hive.clone();
    let trust = crate::root_cert::trust_root(&store, &config, &instance.url)?;
    let session = crate::auth::Session::load(store.clone(), trust)?;
    let (mk, _) = crate::replica::crypt::master_key(store.config_dir(), false)?;
    let (replica, _) = Replica::open(store.data_dir(), &record.vault_id, &mk)?;
    let vault_writer = writer::VaultWriter::open(&session, place::CAPTURE_ROLE)?;
    let vault_place = writer::vault_place(&record, &replica)?;
    let signer = vault_writer.signer();
    let vault = Vault {
        label: format!("@{}/{}", record.ns, record.name),
        target: Target {
            replica: &replica,
            place: &vault_place,
            signer: &signer,
        },
    };
    f(&vault)
}

/// Sends the queued events of `root`'s vault (one push). The tool hands this
/// to [`crate::background`], so a doc write never waits for Hive.
pub fn push_queued(root: &str) -> Result<String, HiveError> {
    use crate::sync::{driver, place, writer};
    let store = crate::HiveStore::open()?;
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let record = crate::resolution::vault_for_root(store.data_dir(), std::path::Path::new(root))?
        .ok_or_else(|| {
        HiveError::Config("this checkout is not linked to a Hive vault".into())
    })?;
    let config = lean_ctx::core::config::Config::load().hive.clone();
    let trust = crate::root_cert::trust_root(&store, &config, &instance.url)?;
    let session = crate::auth::Session::load(store.clone(), trust)?;
    let (mk, _) = crate::replica::crypt::master_key(store.config_dir(), false)?;
    let (replica, _) = Replica::open(store.data_dir(), &record.vault_id, &mk)?;
    let api = place::SessionApi(&session);
    let push = driver::push_pending(&api, &replica, &writer::vault_ref(&record), &[])?;
    let label = format!("@{}/{}", record.ns, record.name);
    Ok(if push.failed.is_empty() {
        format!("docs: sent {} event(s) to {label}", push.logged)
    } else {
        let why: Vec<&str> = push.failed.iter().map(|(_, w)| w.as_str()).collect();
        format!(
            "docs: {} event(s) refused by {label} ({}); re-read the page and write again",
            push.failed.len(),
            why.join(", ")
        )
    })
}

#[cfg(test)]
#[path = "docs_tests.rs"]
mod tests;
