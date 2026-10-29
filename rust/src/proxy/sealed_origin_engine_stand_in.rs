// SPDX-License-Identifier: Apache-2.0
//! TEST seam to the Engine's delivered-content register, compiled only into
//! the test build of `sealed_origin` in place of the production seam.
//!
//! A lookup first asks a STAND-IN table a test can fill directly, then the
//! Engine's REAL register (`core::archive::delivered`). A test that fills
//! only the real register therefore runs the production path end to end; a
//! test that fills the stand-in exercises the proxy's flow — classification,
//! lookup per text block, join, storage with the sealed item, re-admission in
//! the admission's project scope, mapping to typed refusals — and proves
//! nothing about the Engine's own re-admission of a source.
//!
//! In the stand-in a file source holds while the request project in scope is
//! the one it was delivered for and the file has the bytes it had; a
//! caller-presented source holds while the test says the Engine sees it
//! presented and admitted.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::core::archive::delivered;
pub(crate) use crate::core::archive::delivered::LineageRefusal;

#[derive(Clone, PartialEq, Eq)]
enum Source {
    /// A project file and the digest of the bytes it had when delivered.
    File {
        project: PathBuf,
        path: PathBuf,
        content: String,
    },
    /// A source a caller presented to the Engine, by its reference.
    Presented(String),
}

/// A stand-in source set.
#[derive(Clone)]
pub(crate) struct StandIn {
    sources: Vec<Source>,
    complete: bool,
}

/// The source set of one delivered text: the Engine's own, or a stand-in.
#[derive(Clone)]
pub(crate) enum Sources {
    Engine(delivered::Lineage),
    StandIn(StandIn),
}

/// How the stand-in Engine currently sees a caller-presented source.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Presentation {
    Admitted,
    Refused,
}

struct Table {
    delivered: Vec<(String, StandIn)>,
    presented: Vec<(String, Presentation)>,
}

static TABLE: Mutex<Table> = Mutex::new(Table {
    delivered: Vec::new(),
    presented: Vec::new(),
});

fn table() -> std::sync::MutexGuard<'static, Table> {
    TABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn file_digest(path: &Path) -> Option<String> {
    std::fs::read(path)
        .ok()
        .map(|bytes| blake3::hash(&bytes).to_hex().to_string())
}

fn project_in_scope() -> Option<PathBuf> {
    crate::core::policy::runtime::REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
        .and_then(|project| project.canonicalize().ok())
}

impl StandIn {
    fn still_admitted(&self) -> Result<(), LineageRefusal> {
        if !self.complete {
            return Err(LineageRefusal::Unknown);
        }
        for source in &self.sources {
            match source {
                Source::File {
                    project,
                    path,
                    content,
                } => {
                    if project_in_scope().as_ref() != Some(project)
                        || file_digest(path).as_ref() != Some(content)
                    {
                        return Err(LineageRefusal::SourceNoLongerAdmitted);
                    }
                }
                Source::Presented(reference) => {
                    let seen = table()
                        .presented
                        .iter()
                        .find(|(known, _)| known == reference)
                        .map(|(_, presentation)| *presentation);
                    match seen {
                        None => return Err(LineageRefusal::SourceNotPresented),
                        Some(Presentation::Refused) => {
                            return Err(LineageRefusal::SourceNoLongerAdmitted);
                        }
                        Some(Presentation::Admitted) => {}
                    }
                }
            }
        }
        Ok(())
    }
}

impl Sources {
    /// Stand-in: the file `path` of `project` as it is now.
    pub(crate) fn file(project: &Path, path: &Path) -> Self {
        Self::StandIn(StandIn {
            sources: vec![Source::File {
                project: project.canonicalize().expect("the project exists"),
                path: path.to_path_buf(),
                content: file_digest(path).expect("the source exists"),
            }],
            complete: true,
        })
    }

    /// Stand-in: a source a caller presented under `reference`.
    pub(crate) fn presented(reference: &str) -> Self {
        Self::StandIn(StandIn {
            sources: vec![Source::Presented(reference.to_owned())],
            complete: true,
        })
    }

    /// Union of two source sets. A real and a stand-in set do not unite: the
    /// result is incomplete.
    pub(crate) fn join(&mut self, other: Self) {
        match (&mut *self, other) {
            (Self::Engine(mine), Self::Engine(theirs)) => mine.join(theirs),
            (Self::StandIn(mine), Self::StandIn(theirs)) => {
                mine.complete &= theirs.complete;
                for source in theirs.sources {
                    if !mine.sources.contains(&source) {
                        mine.sources.push(source);
                    }
                }
            }
            (Self::StandIn(mine), Self::Engine(_)) => mine.complete = false,
            (Self::Engine(_), Self::StandIn(_)) => {
                *self = Self::StandIn(StandIn {
                    sources: Vec::new(),
                    complete: false,
                });
            }
        }
    }

    pub(crate) fn is_complete(&self) -> bool {
        match self {
            Self::Engine(lineage) => lineage.is_complete(),
            Self::StandIn(stand_in) => stand_in.complete,
        }
    }

    pub(crate) fn still_admitted(&self) -> Result<(), LineageRefusal> {
        match self {
            Self::Engine(lineage) => lineage.still_admitted(),
            Self::StandIn(stand_in) => stand_in.still_admitted(),
        }
    }
}

/// Forget everything the stand-in delivered and was presented. The Engine's
/// real register lives in the test's isolated data directory.
pub(crate) fn reset() {
    let mut table = table();
    table.delivered.clear();
    table.presented.clear();
}

/// Stand-in: the Engine releases `text`, derived from `sources`. A set of the
/// real register is not accepted here — that register is filled through its
/// own API — and delivers nothing.
pub(crate) fn deliver(text: &str, sources: Sources) {
    let Sources::StandIn(sources) = sources else {
        return;
    };
    let digest = delivered::digest(text);
    let mut table = table();
    table.delivered.retain(|(known, _)| *known != digest);
    table.delivered.push((digest, sources));
}

/// Stand-in: how the Engine sees the source `reference` from now on; `None`:
/// the caller does not present it.
pub(crate) fn present(reference: &str, presentation: Option<Presentation>) {
    let mut table = table();
    table.presented.retain(|(known, _)| known != reference);
    if let Some(presentation) = presentation {
        table.presented.push((reference.to_owned(), presentation));
    }
}

/// The stand-in table first, then the Engine's real register.
pub(crate) fn lineage_of(digest: &str) -> Option<Sources> {
    let stand_in = table()
        .delivered
        .iter()
        .find(|(known, _)| known == digest)
        .map(|(_, sources)| sources.clone());
    match stand_in {
        Some(sources) => Some(Sources::StandIn(sources)),
        None => delivered::lineage_of(digest).map(Sources::Engine),
    }
}
