// SPDX-License-Identifier: Apache-2.0
//! Source bindings of generated rule files (G04).
//!
//! A rule file is text distilled from knowledge facts, and a fact may be
//! bound to the files it was derived from (`archive::bindings`). Nothing in
//! a rule file changes when one of those files is deleted, restricted or
//! reclassified, so a rule is bound to the same files when it is written and
//! asked again whenever lean-ctx is about to use it (`list`, `promote`).
//!
//! The `.mdc` file is a host contract — Cursor loads its frontmatter and
//! body as they are, and users edit it by hand — so the binding is NOT in
//! it. It is a sidecar beside the store, under
//! `<data dir>/skillify/authority/`, keyed by the rule file's path: the form
//! the handoff ledger uses for its `.authority` sidecar. The rule file format
//! is unchanged.
//!
//! One rule, the existing one (`context_admission::stored`):
//!
//! - a rule whose every part is a bound fact is bound to ALL their files;
//!   it is used only while `serve_stored` re-authorizes each of them;
//! - a rule with a part that has no source on record (an unbound fact, a
//!   diary entry), a rule written before bindings existed, a rule on another
//!   machine and a rule edited by hand (the binding is of another text) are
//!   originless: withheld while protection is active in any layer,
//!   re-admitted under today's content rules in Community developer mode.
//!
//! A host that loads `.cursor/rules` reads the file itself, not through
//! lean-ctx. So every skillify operation that walks the rules ([`review`])
//! takes a rule out of the host's directory when — and only when — protection
//! is active, lean-ctx created the file, the file still holds exactly the
//! bytes lean-ctx wrote, and its binding is no longer served. It is MOVED to
//! `<data dir>/skillify/quarantine/`, never deleted, and comes back only by
//! being derived again through admission. A file edited by hand, or one
//! lean-ctx has no binding for, is never moved or changed: it is reported.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::candidate::Derivation;
use super::rule_file;
use crate::core::archive::authority::without_capture;
use crate::core::archive::bindings::{self, Item};
use crate::core::context_admission::carrier::Withheld;
use crate::core::context_admission::stored::{self, Lineage, SourceRef, StoredBinding};

/// `StoredBinding::origin` of a rule file.
const ORIGIN: &str = "skillify:rule";
/// A larger sidecar is not read: no binding.
const MAX_SIDECAR_BYTES: u64 = 256 * 1024;
/// Rule files, and sidecars, one [`review`] looks at.
const MAX_WALK: usize = 256;

/// What [`review`] reports about a rule file it did not move. A notice is
/// never a rule: a reason code and a fixed sentence, nothing of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Notice {
    /// Edited by hand after lean-ctx wrote it; a source of the text lean-ctx
    /// wrote is no longer admitted.
    Edited,
    /// No binding on record: lean-ctx cannot say what it was derived from.
    /// Under protection lean-ctx does not write such a file ([`prepare`]),
    /// so this concerns a file that existed before, or one it did not write.
    Unbound,
    /// Should have been moved and could not be.
    NotMoved,
}

impl Notice {
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::Edited => "rule.edited_source_withdrawn",
            Self::Unbound => "rule.no_source_on_record",
            Self::NotMoved => "rule.not_moved",
        }
    }

    pub(super) fn sentence(self) -> &'static str {
        match self {
            Self::Edited => "source no longer admitted, file edited by hand — remove it yourself",
            Self::Unbound => "no source on record, lean-ctx cannot check it — review it yourself",
            Self::NotMoved => {
                "source no longer admitted, file could not be moved — remove it yourself"
            }
        }
    }
}

/// The sidecar of one rule file.
#[derive(Serialize, Deserialize)]
struct Authority {
    /// The rule file the binding belongs to, as lean-ctx wrote it.
    rule: PathBuf,
    binding: StoredBinding,
}

fn store_dir(name: &str) -> Option<PathBuf> {
    Some(
        crate::core::data_dir::lean_ctx_data_dir()
            .ok()?
            .join("skillify")
            .join(name),
    )
}

/// Content-free name of everything kept for the rule file at `rule`: a
/// digest of its path.
fn key(rule: &Path) -> String {
    let identity = crate::core::pathutil::canonicalize_secure_or_self(rule);
    blake3::hash(identity.to_string_lossy().as_bytes())
        .to_hex()
        .to_string()
}

fn sidecar(rule: &Path) -> Option<PathBuf> {
    Some(store_dir("authority")?.join(format!("{}.authority", key(rule))))
}

/// Where a rule taken out of the host's directory is kept: the copy and the
/// record of its original path.
pub(super) fn quarantine_paths(rule: &Path) -> Option<(PathBuf, PathBuf)> {
    let directory = store_dir("quarantine")?;
    let key = key(rule);
    Some((
        directory.join(format!("{key}.mdc")),
        directory.join(format!("{key}.origin")),
    ))
}

/// The sidecar at `path`. One that is missing, a link, oversized or
/// undecodable is no binding at all.
fn load(path: &Path) -> Option<Authority> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_SIDECAR_BYTES {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The binding of `rule` if it is a binding of exactly `text`.
fn binding_of(rule: &Path, text: &str) -> Option<StoredBinding> {
    load(&sidecar(rule)?)
        .map(|authority| authority.binding)
        .filter(|binding| binding.binds(text))
}

/// The files every part of `derivation` is bound to, each read again now
/// (`archive::bindings::held`), for a reader working in `reader`.
///
/// - `Ok(Some(_))`: every part is a bound fact that holds now;
/// - `Ok(None)`: a part has no source on record — the rule is originless;
/// - `Err`, while protection is active: a bound fact is not served now.
fn sources(
    derivation: &Derivation,
    reader: &str,
    enforced: bool,
) -> Result<Option<(String, Vec<SourceRef>)>, Withheld> {
    if derivation.unbound || derivation.facts.is_empty() {
        return Ok(None);
    }
    let mut project: Option<String> = None;
    let mut sources = Vec::new();
    for (category, key, value) in &derivation.facts {
        let item = Item::KnowledgeFact {
            project: &derivation.store,
            category,
            key,
        };
        match bindings::held(&item, value, Some(reader)) {
            Ok(Some((root, files))) => {
                if project
                    .as_deref()
                    .is_some_and(|joined| joined != root.as_str())
                {
                    return if enforced {
                        Err(Withheld::no_source_authority())
                    } else {
                        Ok(None)
                    };
                }
                project = Some(root);
                sources.extend(files);
            }
            Err(withheld) if enforced => return Err(withheld),
            Ok(None) | Err(_) => return Ok(None),
        }
    }
    Ok(project.map(|project| (project, sources)))
}

/// The binding of a rule file that is about to hold `text`, distilled from
/// `derivation`. Decided BEFORE the file is written:
///
/// - `Ok(Some(_))`: bound to every file its facts are bound to. Write the
///   file, then [`record`] the binding;
/// - `Ok(None)`, only without protection: originless. The file may be
///   written; nothing is recorded;
/// - `Err`, while protection is active: a part has no source on record (an
///   unbound fact, a diary entry), a bound fact is not served now, or
///   the text cannot be bound to its files (more files than a lineage holds,
///   restricted, rewritten by admission). The file must NOT be written — it
///   would be an unbound copy of text whose source is withheld.
///
/// Without protection nothing is refused: what cannot be bound is originless.
pub(super) fn prepare(
    text: &str,
    derivation: &Derivation,
    reader: &str,
) -> Result<Option<StoredBinding>, Withheld> {
    let enforced = stored::source_authority_required();
    let Some((project, sources)) = sources(derivation, reader, enforced)? else {
        // No source on record. Under protection such a rule could never be
        // re-authorized, and the host would load it all the same: it is not
        // written. Without protection it is originless.
        return if enforced {
            Err(Withheld::no_source_authority())
        } else {
            Ok(None)
        };
    };
    let bound = Lineage::of(sources)
        .and_then(|lineage| stored::admit_and_bind(text, ORIGIN, Some(&project), lineage))
        .and_then(|(admitted, binding)| {
            if admitted == text {
                Ok(binding)
            } else {
                Err(Withheld::new(&["policy.content_redacted"]))
            }
        });
    match bound {
        Ok(binding) => Ok(Some(binding)),
        Err(withheld) if enforced => Err(withheld),
        Err(_) => Ok(None),
    }
}

/// Keep `binding` for the rule file at `rule`, which holds the bound text
/// now. An `Err` leaves the rule without a binding: originless.
pub(super) fn record(rule: &Path, binding: &StoredBinding) -> Result<(), String> {
    let path = sidecar(rule).ok_or("cannot resolve the skillify authority directory")?;
    let authority = Authority {
        rule: crate::core::pathutil::canonicalize_secure_or_self(rule),
        binding: binding.clone(),
    };
    let encoded = serde_json::to_string(&authority).map_err(|error| error.to_string())?;
    crate::config_io::write_atomic(&path, &encoded)
}

/// Whether the rule file at `rule`, holding `text`, may be used now by a
/// reader working in `reader` — listed, or copied out of the project. The
/// refusal is typed and content-free. A served rule publishes its files into
/// the call's source capture (`stored::publish_served`).
pub(super) fn served(rule: &Path, text: &str, reader: &str) -> Result<(), Withheld> {
    let binding = binding_of(rule, text);
    let project = binding.as_ref().map(|_| reader);
    stored::serve_stored(text, binding.as_ref(), project)?;
    stored::publish_served(binding.as_ref());
    Ok(())
}

/// Hand the binding of `from` on to `to`, a byte-identical copy of `text`
/// (`promote`). Without a binding of that text the copy is originless, as
/// its original is.
pub(super) fn carry_over(from: &Path, to: &Path, text: &str) -> Result<(), String> {
    match binding_of(from, text) {
        Some(binding) => record(to, &binding),
        None => Ok(()),
    }
}

/// The rule at `rule` was derived again through admission and written: the
/// copy that was taken out of the host's directory earlier is superseded.
pub(super) fn release(rule: &Path) {
    if let Some((copy, origin)) = quarantine_paths(rule) {
        let _ = std::fs::remove_file(copy);
        let _ = std::fs::remove_file(origin);
    }
}

/// The check every skillify operation that walks the rules runs first
/// (`mine`, `list`, `promote`), for the rules directory under `output_root`
/// and a reader working in `reader`. No sweeper, no watcher.
///
/// In every mode, sidecars of rule files that no longer exist are removed.
/// Then, only while protection is active, each generated rule file is asked:
///
/// - lean-ctx wrote it, it still holds exactly those bytes, and its binding
///   is no longer served → MOVED to the quarantine directory;
/// - edited by hand since, and the sources of its binding no longer admit →
///   left alone, reported ([`EDITED`]);
/// - no binding on record → left alone, reported ([`UNBOUND`]).
///
/// Returns `(file stem, notice)` of the reported files: content-free. In
/// Community developer mode nothing is moved and nothing is reported; a rule
/// that is not served is simply not listed. Bounded: [`MAX_WALK`] files.
pub(super) fn review(output_root: &Path, reader: &str) -> Vec<(PathBuf, Notice)> {
    forget_missing();
    let mut reported = Vec::new();
    if !stored::source_authority_required() {
        return reported;
    }
    let Ok(entries) = std::fs::read_dir(rule_file::rules_dir(output_root)) else {
        return reported;
    };
    for entry in entries.flatten().take(MAX_WALK) {
        let path = entry.path();
        // A regular file of ours inside the output root; a link is never
        // followed, moved or reported.
        let generated = entry.file_type().is_ok_and(|kind| kind.is_file())
            && path.extension().and_then(|e| e.to_str()) == Some("mdc")
            && crate::core::pathjail::jail_path(&path, output_root).is_ok();
        let ours = generated
            && path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem.starts_with(rule_file::SLUG_PREFIX));
        if !ours {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let notice = match sidecar(&path).and_then(|sidecar| load(&sidecar)) {
            None => Some(Notice::Unbound),
            Some(authority) if authority.binding.binds(&text) => {
                (withdrawn(&text, &authority.binding, reader) && quarantine(&path, &text).is_err())
                    .then_some(Notice::NotMoved)
            }
            Some(authority) => (!sources_hold(&authority.binding)).then_some(Notice::Edited),
        };
        if let Some(notice) = notice {
            reported.push((path, notice));
        }
    }
    // Stable order for every caller (#498).
    reported.sort_by(|a, b| a.0.cmp(&b.0));
    reported
}

/// Whether a binding of exactly `text` is no longer served to `reader`
/// because of its SOURCES or its content. A reader in another project is not
/// a withdrawal: the rule is not that reader's to move.
fn withdrawn(text: &str, binding: &StoredBinding, reader: &str) -> bool {
    without_capture(|| stored::serve_stored(text, Some(binding), Some(reader))).is_err_and(
        |withheld| {
            !withheld
                .codes()
                .iter()
                .any(|code| code.starts_with("store.project_"))
        },
    )
}

/// Whether every file `binding` names still delivers what it was bound to.
fn sources_hold(binding: &StoredBinding) -> bool {
    binding.file_sources().is_none_or(|(project, files)| {
        without_capture(|| {
            files.into_iter().all(|(path, tool, digest)| {
                SourceRef::read_as_admitted(path, project, tool, digest).is_some()
            })
        })
    })
}

/// Move the rule file at `rule`, which holds `text`, out of the host's
/// directory: the bytes and the record of the original path are written
/// under the data directory first, and only then is the original removed —
/// and only if it still is a regular file holding exactly `text`. Its
/// sidecar goes with it. Reversible by hand; lean-ctx itself brings a rule
/// back only by deriving it again.
fn quarantine(rule: &Path, text: &str) -> Result<(), String> {
    let (copy, origin) = quarantine_paths(rule).ok_or("no skillify quarantine directory")?;
    let authority = sidecar(rule);
    let original = serde_json::to_string(&rule.to_string_lossy()).map_err(|e| e.to_string())?;
    crate::config_io::write_atomic(&copy, text)?;
    crate::config_io::write_atomic(&origin, &original)?;
    let unchanged = std::fs::symlink_metadata(rule).is_ok_and(|metadata| metadata.is_file())
        && std::fs::read_to_string(rule).is_ok_and(|current| current == text);
    if !unchanged {
        return Err("rule file changed during the move".to_owned());
    }
    std::fs::remove_file(rule).map_err(|error| error.to_string())?;
    if let Some(authority) = authority {
        let _ = std::fs::remove_file(authority);
    }
    Ok(())
}

/// Remove the sidecars of rule files that no longer exist. Bounded:
/// [`MAX_WALK`] sidecars per run.
fn forget_missing() {
    let Some(entries) = store_dir("authority").and_then(|dir| std::fs::read_dir(dir).ok()) else {
        return;
    };
    for entry in entries.flatten().take(MAX_WALK) {
        let path = entry.path();
        let gone = load(&path).is_some_and(|authority| {
            std::fs::symlink_metadata(&authority.rule)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        });
        if gone {
            let _ = std::fs::remove_file(&path);
        }
    }
}
