use std::collections::HashMap;
use std::fmt;

/// Compact, `Copy`-able handle for a repository-relative file path.
///
/// Backed by a `u32` index into the owning [`PathInterner`]'s table, so
/// equality / hashing / cloning are all O(1) with zero allocation.
/// Resolve back to a `&str` via [`PathInterner::resolve`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(u32);

#[allow(dead_code)]
impl FileId {
    /// Sentinel for "no file" / placeholder contexts. Never returned by
    /// [`PathInterner::intern`]; safe to use as a default or error marker.
    pub const NONE: Self = Self(u32::MAX);

    #[inline]
    pub(crate) fn raw(self) -> u32 {
        self.0
    }
}

impl fmt::Debug for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FileId({})", self.0)
    }
}

/// Graph-local path interner: owns every distinct repository-relative path
/// exactly once and hands out [`FileId`] handles.
///
/// Lifetime is tied to the [`ProjectIndex`](super::ProjectIndex) that owns it.
/// This avoids a process-global interner and keeps the interned set
/// garbage-collectable when an index is dropped.
#[derive(Debug, Clone)]
pub struct PathInterner {
    to_id: HashMap<String, FileId>,
    to_path: Vec<String>,
}

impl PathInterner {
    pub fn new() -> Self {
        Self {
            to_id: HashMap::new(),
            to_path: Vec::new(),
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            to_id: HashMap::with_capacity(cap),
            to_path: Vec::with_capacity(cap),
        }
    }

    /// Intern a path, returning its [`FileId`]. If the path was already
    /// interned, returns the existing id without allocating.
    pub fn intern(&mut self, path: &str) -> FileId {
        if let Some(&id) = self.to_id.get(path) {
            return id;
        }
        let id = FileId(self.to_path.len() as u32);
        self.to_path.push(path.to_owned());
        self.to_id.insert(path.to_owned(), id);
        id
    }

    #[allow(dead_code)]
    /// Intern an already-owned `String`, avoiding a clone when the path is new.
    pub fn intern_owned(&mut self, path: String) -> FileId {
        if let Some(&id) = self.to_id.get(&path) {
            return id;
        }
        let id = FileId(self.to_path.len() as u32);
        self.to_path.push(path.clone());
        self.to_id.insert(path, id);
        id
    }

    #[allow(dead_code)]
    /// Look up a path without interning it.
    pub fn get(&self, path: &str) -> Option<FileId> {
        self.to_id.get(path).copied()
    }

    #[allow(dead_code)]
    /// Resolve a [`FileId`] back to its repository-relative path.
    ///
    /// # Panics
    /// Panics if `id` was not produced by this interner (including `FileId::NONE`).
    #[inline]
    pub fn resolve(&self, id: FileId) -> &str {
        &self.to_path[id.0 as usize]
    }

    #[allow(dead_code)]
    /// Non-panicking resolve — returns `None` for out-of-range ids.
    pub fn try_resolve(&self, id: FileId) -> Option<&str> {
        self.to_path.get(id.0 as usize).map(String::as_str)
    }

    #[allow(dead_code)]
    /// Number of distinct interned paths.
    pub fn len(&self) -> usize {
        self.to_path.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.to_path.is_empty()
    }

    #[allow(dead_code)]
    /// Iterate over all `(FileId, path)` pairs in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (FileId, &str)> {
        self.to_path
            .iter()
            .enumerate()
            .map(|(i, p)| (FileId(i as u32), p.as_str()))
    }
}

impl Default for PathInterner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_returns_same_id_for_same_path() {
        let mut interner = PathInterner::new();
        let a = interner.intern("src/main.rs");
        let b = interner.intern("src/main.rs");
        assert_eq!(a, b);
        assert_eq!(interner.len(), 1);
    }

    #[test]
    fn distinct_paths_get_distinct_ids() {
        let mut interner = PathInterner::new();
        let a = interner.intern("src/main.rs");
        let b = interner.intern("src/lib.rs");
        assert_ne!(a, b);
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn resolve_round_trips() {
        let mut interner = PathInterner::new();
        let id = interner.intern("src/core/graph_index/mod.rs");
        assert_eq!(interner.resolve(id), "src/core/graph_index/mod.rs");
    }

    #[test]
    fn intern_owned_avoids_double_allocation() {
        let mut interner = PathInterner::new();
        let id1 = interner.intern_owned("src/foo.rs".to_owned());
        let id2 = interner.intern("src/foo.rs");
        assert_eq!(id1, id2);
        assert_eq!(interner.len(), 1);
    }

    #[test]
    fn get_returns_none_for_unknown() {
        let interner = PathInterner::new();
        assert_eq!(interner.get("nonexistent.rs"), None);
    }

    #[test]
    fn get_returns_some_for_interned() {
        let mut interner = PathInterner::new();
        let id = interner.intern("src/lib.rs");
        assert_eq!(interner.get("src/lib.rs"), Some(id));
    }

    #[test]
    fn try_resolve_returns_none_for_invalid_id() {
        let interner = PathInterner::new();
        assert_eq!(interner.try_resolve(FileId(999)), None);
        assert_eq!(interner.try_resolve(FileId::NONE), None);
    }

    #[test]
    fn file_id_none_is_never_returned_by_intern() {
        let mut interner = PathInterner::new();
        for i in 0..100 {
            let id = interner.intern(&format!("file_{i}.rs"));
            assert_ne!(id, FileId::NONE);
        }
    }

    #[test]
    fn iter_yields_insertion_order() {
        let mut interner = PathInterner::new();
        interner.intern("b.rs");
        interner.intern("a.rs");
        interner.intern("c.rs");
        let paths: Vec<&str> = interner.iter().map(|(_, p)| p).collect();
        assert_eq!(paths, &["b.rs", "a.rs", "c.rs"]);
    }

    #[test]
    fn file_id_is_copy_and_small() {
        assert_eq!(std::mem::size_of::<FileId>(), 4);
        let id = FileId(42);
        let copy = id;
        assert_eq!(id, copy);
    }

    #[test]
    fn file_id_ordering_is_stable() {
        let a = FileId(1);
        let b = FileId(2);
        assert!(a < b);
        let mut ids = vec![b, a];
        ids.sort();
        assert_eq!(ids, vec![a, b]);
    }

    #[test]
    fn with_capacity_works() {
        let interner = PathInterner::with_capacity(1000);
        assert!(interner.is_empty());
        assert_eq!(interner.len(), 0);
    }
}
