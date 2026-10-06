// SPDX-License-Identifier: Apache-2.0
//! Source identity travels with an archive; a stored verdict is never authorization.
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveAuthority {
    version: u8,
    project: String,
    path: String,
    tool: String,
    admitted_digest: String,
}

pub(crate) type Capture = Arc<Mutex<Option<ArchiveAuthority>>>;

tokio::task_local! {
    pub(crate) static CAPTURE: Option<Capture>;
}

pub(crate) fn current_capture() -> Option<Capture> {
    CAPTURE.try_with(Clone::clone).ok().flatten()
}

pub(crate) async fn capture<T>(
    operation: impl std::future::Future<Output = T>,
) -> (T, Option<ArchiveAuthority>) {
    let capture = Arc::new(Mutex::new(None));
    let result = CAPTURE.scope(Some(capture.clone()), operation).await;
    let authority = capture.try_lock().ok().and_then(|mut slot| slot.take());
    (result, authority)
}

impl ArchiveAuthority {
    /// Called with the canonical path and bytes returned by the admitted rooted read.
    pub(crate) fn file(project: &Path, path: &str, tool: &str, admitted: &str) -> Option<Self> {
        if !crate::core::policy::runtime::is_active() {
            return None;
        }
        let project = canonical_archive_path(project)?;
        let path = canonical_archive_path(Path::new(path))?;
        if !path.starts_with(&project) {
            return None;
        }
        Some(Self {
            version: 1,
            project: project.to_str()?.to_owned(),
            path: path.to_str()?.to_owned(),
            tool: tool.to_owned(),
            admitted_digest: blake3::hash(admitted.as_bytes()).to_hex().to_string(),
        })
    }

    pub(crate) fn publish(self) {
        if let Some(capture) = current_capture()
            && let Ok(mut slot) = capture.try_lock()
        {
            *slot = Some(self);
        }
    }

    pub(crate) fn id(&self, content: &str) -> Option<String> {
        let mut hash = blake3::Hasher::new();
        hash.update(b"leanctx-source-archive-v1\0");
        hash.update(&serde_json::to_vec(self).ok()?);
        hash.update(b"\0");
        hash.update(content.as_bytes());
        Some(hash.finalize().to_hex().to_string())
    }

    pub(crate) fn admitted(&self) -> bool {
        self.admitted_budgeted(&mut crate::core::limits::max_read_bytes())
    }

    pub(super) fn matches_project(&self) -> bool {
        if self.version != 1 {
            return false;
        }
        let Some(project) = crate::core::policy::runtime::REQUEST_PROJECT
            .try_with(|slot| slot.borrow().clone())
            .ok()
            .flatten()
            .and_then(|path| crate::core::pathutil::canonicalize_secure(&path).ok())
        else {
            return false;
        };
        project == Path::new(&self.project)
    }

    pub(super) fn admitted_budgeted(&self, remaining: &mut usize) -> bool {
        if !self.matches_project() || *remaining == 0 {
            return false;
        }
        crate::tools::ctx_read::read_file_for_tool_rooted_budgeted(
            &self.path,
            &self.project,
            &self.tool,
            remaining,
        )
        .is_ok_and(|content| {
            blake3::hash(content.as_bytes()).to_hex().as_str() == self.admitted_digest
        })
    }
}

fn canonical_archive_path(path: &Path) -> Option<std::path::PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    crate::core::pathutil::canonicalize_secure(path)
        .or_else(|error| {
            let simplified = crate::core::pathutil::strip_verbatim(path.to_path_buf());
            if simplified.as_path() == path {
                Err(error)
            } else {
                crate::core::pathutil::canonicalize_secure(&simplified)
            }
        })
        .ok()
}
