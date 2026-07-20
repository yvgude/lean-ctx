//! CompressionContentPort — resolves content refs to bounded byte slices and
//! persists compressed results as content-addressed BLAKE3 refs.
//!
//! Security invariants:
//! - All reads go through PathJail (no traversal outside project root)
//! - Symlinks rejected via lstat on every component of the relative path
//! - Reads are bounded to MAX_CONTENT_BYTES (prevents OOM on large files)
//! - Persisted refs use BLAKE3 content-addressing (collision-resistant)

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::core::ocla::OclaError;
use crate::core::ocla::types::OclaResult;
use crate::core::pathjail;

const MAX_CONTENT_BYTES: usize = 512 * 1024;
const MAX_CACHE_ENTRIES: usize = 256;

pub struct CompressionContentPort {
    project_root: PathBuf,
    cache: Mutex<ContentCache>,
}

struct ContentCache {
    entries: Vec<CacheEntry>,
}

struct CacheEntry {
    ref_key: String,
    data: Vec<u8>,
}

impl Default for ContentCache {
    fn default() -> Self {
        Self {
            entries: Vec::with_capacity(64),
        }
    }
}

impl CompressionContentPort {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            cache: Mutex::new(ContentCache::default()),
        }
    }

    /// Resolve a `file:<relative_path>` ref to bounded bytes.
    /// Rejects symlinks at any component, path traversal, and oversized files.
    pub fn resolve(&self, content_ref: &str) -> OclaResult<Vec<u8>> {
        let rel_path = content_ref
            .strip_prefix("file:")
            .ok_or_else(|| OclaError::InvalidRequest("content_ref must use file: scheme".into()))?;

        // Validate containment via PathJail (traversal safety)
        let _jailed = pathjail::jail_path(Path::new(rel_path), &self.project_root)
            .map_err(|e| OclaError::InvalidRequest(format!("path jail: {e}")))?;

        // Canonicalize root to handle /tmp → /private/tmp on macOS
        let canonical_root = self
            .project_root
            .canonicalize()
            .unwrap_or_else(|_| self.project_root.clone());

        // Walk each component and reject symlinks via symlink_metadata (lstat)
        let mut current = canonical_root.clone();
        for component in Path::new(rel_path).components() {
            current = current.join(component);
            let meta = current
                .symlink_metadata()
                .map_err(|e| OclaError::InvalidRequest(format!("resolve: {e}")))?;
            if meta.file_type().is_symlink() {
                return Err(OclaError::InvalidRequest(format!(
                    "symlink detected at: {}",
                    component.as_os_str().to_string_lossy()
                )));
            }
        }

        // At this point, `current` is the final path with no symlinks traversed
        let meta = current
            .symlink_metadata()
            .map_err(|e| OclaError::InvalidRequest(format!("metadata: {e}")))?;

        if !meta.file_type().is_file() {
            return Err(OclaError::InvalidRequest("not a regular file".into()));
        }

        if meta.len() > MAX_CONTENT_BYTES as u64 {
            return Err(OclaError::InvalidRequest(format!(
                "file exceeds {MAX_CONTENT_BYTES} byte limit"
            )));
        }

        let mut file = fs::File::open(&current)
            .map_err(|e| OclaError::InvalidRequest(format!("open: {e}")))?;

        let mut data = Vec::with_capacity(meta.len() as usize);
        file.read_to_end(&mut data)
            .map_err(|e| OclaError::InvalidRequest(format!("read: {e}")))?;

        if data.len() > MAX_CONTENT_BYTES {
            return Err(OclaError::InvalidRequest(format!(
                "file exceeds {MAX_CONTENT_BYTES} byte limit"
            )));
        }

        Ok(data)
    }

    /// Persist compressed bytes and return a `blake3:<hex>` content-addressed ref.
    pub fn persist(&self, data: &[u8]) -> OclaResult<String> {
        if data.len() > MAX_CONTENT_BYTES {
            return Err(OclaError::InvalidRequest(
                "compressed output exceeds size limit".into(),
            ));
        }

        let hash = blake3::hash(data);
        let ref_key = format!("blake3:{}", hash.to_hex());

        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if cache.entries.iter().any(|e| e.ref_key == ref_key) {
            return Ok(ref_key);
        }

        if cache.entries.len() >= MAX_CACHE_ENTRIES {
            let quarter = cache.entries.len() / 4;
            cache.entries.drain(..quarter);
        }

        cache.entries.push(CacheEntry {
            ref_key: ref_key.clone(),
            data: data.to_vec(),
        });

        Ok(ref_key)
    }

    /// Retrieve previously persisted content by its BLAKE3 ref.
    pub fn retrieve(&self, ref_key: &str) -> OclaResult<Vec<u8>> {
        let cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        cache
            .entries
            .iter()
            .find(|e| e.ref_key == ref_key)
            .map(|e| e.data.clone())
            .ok_or_else(|| OclaError::InvalidRequest(format!("ref not found: {ref_key}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_port() -> (tempfile::TempDir, CompressionContentPort) {
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let port = CompressionContentPort::new(canonical);
        (dir, port)
    }

    #[test]
    fn resolve_reads_file_within_jail() {
        let (dir, port) = make_port();
        fs::write(dir.path().join("hello.txt"), b"world").unwrap();
        let data = port.resolve("file:hello.txt").unwrap();
        assert_eq!(data, b"world");
    }

    #[test]
    fn resolve_rejects_traversal() {
        let (_dir, port) = make_port();
        let err = port.resolve("file:../etc/passwd").unwrap_err();
        assert!(err.to_string().contains("jail"));
    }

    #[test]
    fn resolve_rejects_oversized_file() {
        let (dir, port) = make_port();
        let big = vec![0u8; MAX_CONTENT_BYTES + 1];
        fs::write(dir.path().join("big.bin"), &big).unwrap();
        let err = port.resolve("file:big.bin").unwrap_err();
        assert!(err.to_string().contains("limit"));
    }

    #[test]
    fn resolve_requires_file_scheme() {
        let (_dir, port) = make_port();
        let err = port.resolve("http://evil.com").unwrap_err();
        assert!(err.to_string().contains("file: scheme"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_rejects_symlink_final_component() {
        let (dir, port) = make_port();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("real.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink(root.join("real.txt"), root.join("link.txt")).unwrap();

        let err = port.resolve("file:link.txt").unwrap_err();
        assert!(
            err.to_string().contains("symlink"),
            "expected symlink rejection, got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_rejects_symlink_intermediate_dir() {
        let (dir, port) = make_port();
        let root = dir.path().canonicalize().unwrap();
        let real_dir = root.join("real_dir");
        fs::create_dir(&real_dir).unwrap();
        fs::write(real_dir.join("target.txt"), b"hidden").unwrap();
        std::os::unix::fs::symlink(&real_dir, root.join("sym_dir")).unwrap();

        let result = port.resolve("file:sym_dir/target.txt");
        assert!(result.is_err(), "should reject symlinked intermediate dir");
        assert!(
            result.unwrap_err().to_string().contains("symlink"),
            "error should mention symlink"
        );
    }

    #[test]
    fn persist_and_retrieve_roundtrip() {
        let (_dir, port) = make_port();
        let data = b"compressed content";
        let ref_key = port.persist(data).unwrap();
        assert!(ref_key.starts_with("blake3:"));
        let retrieved = port.retrieve(&ref_key).unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn persist_deduplicates() {
        let (_dir, port) = make_port();
        let data = b"same content";
        let ref1 = port.persist(data).unwrap();
        let ref2 = port.persist(data).unwrap();
        assert_eq!(ref1, ref2);
    }

    #[test]
    fn cache_evicts_when_full() {
        let (_dir, port) = make_port();
        for i in 0..MAX_CACHE_ENTRIES + 10 {
            let data = format!("entry-{i}");
            port.persist(data.as_bytes()).unwrap();
        }
        let cache = port.cache.lock().unwrap();
        assert!(cache.entries.len() <= MAX_CACHE_ENTRIES);
    }
}
