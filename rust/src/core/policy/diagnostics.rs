// SPDX-License-Identifier: Apache-2.0
//! Project-bound diagnostic storage. Inspect complete fields before rendering
//! previews, and resolve current rules only after the writer owns its lock.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use super::{content, runtime};

pub(crate) struct Target {
    pub(crate) path: PathBuf,
    root: PathBuf,
    scoped: bool,
}

pub(crate) fn request_project() -> Option<PathBuf> {
    runtime::REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
}

pub(crate) fn project() -> Option<PathBuf> {
    request_project().or_else(|| std::env::current_dir().ok())
}

pub(crate) fn target(base: PathBuf) -> Option<Target> {
    let root = project()?;
    let policy = runtime::for_project(&root).ok()?;
    let canonical = crate::core::pathutil::safe_canonicalize_bounded(&root, 2000);
    let key = blake3::hash(canonical.as_os_str().as_encoded_bytes()).to_hex();
    let projects = base.parent()?.join("projects");
    let project_dir = projects.join(key.as_str());
    // Windows reports ENOENT for a descendant below a regular file as well as
    // for a genuinely missing path. Validate existing scope parents first so
    // a corrupt `projects` entry cannot be mistaken for an absent scoped log
    // and silently fall back to the readable global log.
    for parent in [&projects, &project_dir] {
        match std::fs::symlink_metadata(parent) {
            Ok(metadata) if is_plain_directory(&metadata) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Ok(_) | Err(_) => return None,
        }
    }
    let scoped_path = project_dir.join(base.file_name()?);
    let scoped_present = match std::fs::symlink_metadata(&scoped_path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return None,
    };
    let scoped = policy.is_some() || scoped_present;
    Some(Target {
        path: if scoped { scoped_path } else { base },
        root,
        scoped,
    })
}

fn is_plain_directory(metadata: &std::fs::Metadata) -> bool {
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    true
}

impl Target {
    fn policy(&self, tool: Option<&str>) -> Result<Option<Arc<runtime::ActivePolicy>>, ()> {
        let policy = runtime::for_project(&self.root).map_err(|_| ())?;
        if policy
            .as_ref()
            .is_some_and(|p| !self.scoped || tool.is_some_and(|tool| !p.tool_allowed(tool)))
        {
            return Err(());
        }
        Ok(policy)
    }

    pub(crate) fn inspect(&self, tool: Option<&str>, value: &Value) -> Option<Value> {
        inspect(value, self.policy(tool).ok()?.as_deref())
    }

    /// Diagnostic contention loses an optional log entry, never a security rule.
    pub(crate) fn with_lock(&self, action: impl FnOnce()) {
        use fs2::FileExt;
        let Some(parent) = self.path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        #[cfg(unix)]
        if self.scoped {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).is_err() {
                return;
            }
        }
        let lock_path = self.path.with_extension("lock");
        let Ok(lock) = open_append(&lock_path) else {
            return;
        };
        let deadline = Instant::now() + Duration::from_millis(50);
        loop {
            match lock.try_lock_exclusive() {
                Ok(()) => break,
                Err(e) if crate::core::file_lock::is_contended(&e) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return,
            }
        }
        if regular_or_absent(&self.path) && self.refresh_existing() {
            action();
        }
        let _ = FileExt::unlock(&lock);
    }

    pub(crate) fn read(&self) -> Option<String> {
        self.policy(None).ok()?;
        let text = Self::read_file(&self.path)?;
        let Value::String(text) = self.inspect(None, &Value::String(text))? else {
            return None;
        };
        Some(text)
    }

    fn refresh_existing(&self) -> bool {
        use std::io::Write;
        let Ok(policy) = self.policy(None) else {
            return false;
        };
        if policy.is_none() || !self.path.exists() {
            return true;
        }
        let Some(original) = Self::read_file(&self.path) else {
            return false;
        };
        let Some(Value::String(safe)) = self.inspect(None, &Value::String(original.clone())) else {
            return false;
        };
        if safe == original {
            return true;
        }
        let Some(parent) = self.path.parent() else {
            return false;
        };
        let Ok(mut file) = tempfile::NamedTempFile::new_in(parent) else {
            return false;
        };
        file.write_all(safe.as_bytes()).is_ok() && file.persist(&self.path).is_ok()
    }

    pub(crate) fn read_file(path: &Path) -> Option<String> {
        use std::io::Read;
        if !regular_or_absent(path) {
            return None;
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        no_follow(&mut options);
        let file = options.open(path).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() {
            return None;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x0400 != 0 {
                return None;
            }
        }
        let mut text = String::new();
        file.take(content::MAX_PROTECTED_CONTENT_BYTES as u64 + 1)
            .read_to_string(&mut text)
            .ok()?;
        if text.len() > content::MAX_PROTECTED_CONTENT_BYTES {
            return None;
        }
        Some(text)
    }
}

fn regular_or_absent(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata.is_file(),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

fn no_follow(options: &mut std::fs::OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
}

pub(crate) fn open_append(path: &Path) -> std::io::Result<std::fs::File> {
    if !regular_or_absent(path) {
        return Err(std::io::Error::other("diagnostic target is not regular"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    // An append-only Windows handle lacks FILE_READ_ATTRIBUTES (so the
    // metadata check below fails) and the read/write access LockFileEx needs
    // for the lock files opened through here. Appends still go to the end.
    #[cfg(windows)]
    options.read(true);
    no_follow(&mut options);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x0400 != 0 {
            return Err(std::io::Error::other("diagnostic reparse target rejected"));
        }
    }
    if !metadata.is_file() {
        return Err(std::io::Error::other("diagnostic target is not regular"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

/// Bound borrowed inputs before allocating a diagnostic record. All fields
/// remain intact for a second, current-policy inspection inside the writer.
pub(crate) fn fields(text: &[(&str, &str)], args: Option<&Map<String, Value>>) -> Option<Value> {
    let mut inspector = Inspector {
        policy: None,
        bytes: 0,
        nodes: 0,
    };
    let mut result = Map::new();
    for (key, value) in text {
        result.insert((*key).into(), Value::String(inspector.text(value)?));
    }
    if let Some(args) = args {
        result.insert("args".into(), inspector.object(args, 1)?);
    }
    Some(Value::Object(result))
}

pub(crate) fn inspect(value: &Value, policy: Option<&runtime::ActivePolicy>) -> Option<Value> {
    Inspector {
        policy,
        bytes: 0,
        nodes: 0,
    }
    .value(value, 0)
}

struct Inspector<'a> {
    policy: Option<&'a runtime::ActivePolicy>,
    bytes: usize,
    nodes: usize,
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;

impl Inspector<'_> {
    fn object(&mut self, values: &Map<String, Value>, depth: usize) -> Option<Value> {
        let mut result = Map::new();
        for (key, value) in values {
            let key = self.text(key)?;
            if result.insert(key, self.value(value, depth + 1)?).is_some() {
                return None;
            }
        }
        Some(Value::Object(result))
    }
    fn text(&mut self, text: &str) -> Option<String> {
        self.bytes = self.bytes.checked_add(text.len())?;
        if self.bytes > content::MAX_PROTECTED_CONTENT_BYTES {
            return None;
        }
        match self.policy {
            Some(policy) => {
                let result = content::evaluate_text(text, policy);
                self.bytes = self
                    .bytes
                    .checked_add(result.text.len().saturating_sub(text.len()))?;
                (!result.blocked && self.bytes <= content::MAX_PROTECTED_CONTENT_BYTES)
                    .then_some(result.text)
            }
            None => Some(text.to_owned()),
        }
    }

    fn value(&mut self, value: &Value, depth: usize) -> Option<Value> {
        self.nodes += 1;
        if self.nodes > 10_000 || depth > 64 {
            return None;
        }
        match value {
            Value::String(text) => Some(Value::String(self.text(text)?)),
            Value::Array(values) => values
                .iter()
                .map(|v| self.value(v, depth + 1))
                .collect::<Option<Vec<_>>>()
                .map(Value::Array),
            Value::Object(values) => self.object(values, depth),
            _ => {
                let text = value.to_string();
                let safe = self.text(&text)?;
                Some(if safe == text {
                    value.clone()
                } else {
                    Value::String(safe)
                })
            }
        }
    }
}
