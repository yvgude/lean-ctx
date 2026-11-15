// SPDX-License-Identifier: Apache-2.0
//! Opt-in, content-addressed cache of pure parse derivations, shared across
//! worktrees and data homes (GitHub #2247).
//!
//! An index build parses every file: BM25 chunks, the deep analysis behind the
//! graph's import and type edges and the call graph, and the graph scan's
//! signatures. A fresh worktree of the same repository parses almost the same
//! files again. This store keeps those parse results by what they are a
//! function of, so a second worktree reuses the unchanged ones and parses only
//! its edited files.
//!
//! What it keeps and what it does not:
//! - An entry is keyed by BLAKE3 over the build identity (version, commit and
//!   the running binary), the derivation kind, its selector (language, and for
//!   chunks the repository-relative path they name) and the exact content the
//!   derivation read. Content that went through admission is hashed after
//!   admission, so a policy that changes the text changes the key.
//! - It never keeps a project index, a root, an absolute path, a retrieval
//!   result or a permission decision. Each root reads, admits and indexes its
//!   own files; only the pure parse of identical text is shared.
//! - A result produced while a language is blocklisted after a parser panic is
//!   neither stored nor served, so a regex fallback is never persisted as the
//!   parser's output.
//!
//! The store is off by default. `[cache] derivations_enabled = true` or
//! `LEAN_CTX_DERIVATION_CACHE=1` turns it on; `LEAN_CTX_DERIVATION_CACHE=0`
//! turns it off. `derivations_dir` / `LEAN_CTX_DERIVATION_CACHE_DIR` name a
//! directory shared by several data homes (default `<cache dir>/derivations`).
//! The setting is read once per process: a running daemon picks up a change
//! when it restarts.
//!
//! Safety: the directory must be a real directory owned by the current user
//! and not writable by group or others, or the store stays off. It is checked
//! when the store opens and then used by path, so a configured directory
//! belongs under a parent only this user can write to (the default is). Entries are
//! opened without following links and must be regular files; FIFOs, links and
//! oversized files are refused. Writes go to a private temp file and are
//! renamed into place. A header carries the full key and a hash of the
//! payload; any mismatch, truncation or decode error is a miss and the entry is
//! removed. The total size is bounded and the least recently used entries are
//! evicted.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::core::bm25_index::{ChunkKind, CodeChunk};
use crate::core::deep_queries::{
    CallSite, DeepAnalysis, ExtMethodDef, ImportInfo, ImportKind, TypeDef, TypeDefKind, TypeUse,
};
use crate::core::signatures::Signature;

const MAGIC: &[u8; 8] = b"LCDERIV1";
const HEADER_LEN: usize = 8 + 32 + 4 + 32;
/// Largest payload stored or decoded. A file of the BM25 size cap yields far
/// less; larger results are recomputed every time.
pub(crate) const MAX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_BYTES: u64 = 512 * 1024 * 1024;
/// Entries walked by one eviction sweep; a larger store is trimmed over sweeps.
const SWEEP_MAX_ENTRIES: usize = 500_000;
/// A hit refreshes the entry's mtime (its LRU age) at most this often.
const TOUCH_AFTER: Duration = Duration::from_hours(6);
const STALE_TMP: Duration = Duration::from_secs(3600);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Chunks,
    DeepAnalysis,
    Signatures,
}

impl Kind {
    fn tag(self) -> &'static [u8] {
        match self {
            Kind::Chunks => b"chunks/1",
            Kind::DeepAnalysis => b"deep-analysis/1",
            Kind::Signatures => b"signatures/1",
        }
    }
}

/// One opened, validated store directory.
#[derive(Debug)]
pub(crate) struct Store {
    dir: PathBuf,
    identity: [u8; 32],
    max_bytes: u64,
    written_since_sweep: AtomicU64,
    sweeping: AtomicBool,
    tmp_counter: AtomicU64,
}

impl Store {
    /// Open (creating it privately if needed) the store under `root`. Returns
    /// `None` when the directory is not a safe, same-user directory. Opening
    /// does not sweep; the caller schedules the first [`Store::sweep`].
    pub(crate) fn open(root: &Path, identity: [u8; 32], max_bytes: u64) -> Option<Self> {
        let dir = root.join("v1");
        create_private_dir(root)?;
        create_private_dir(&dir)?;
        Some(Self {
            dir,
            identity,
            max_bytes: max_bytes.max(1024 * 1024),
            written_since_sweep: AtomicU64::new(0),
            sweeping: AtomicBool::new(false),
            tmp_counter: AtomicU64::new(0),
        })
    }

    fn key(&self, kind: Kind, selector: &str, content: &str) -> [u8; 32] {
        let mut h = blake3::Hasher::new_derive_key("lean-ctx parse derivation cache v1");
        h.update(&self.identity);
        for part in [kind.tag(), selector.as_bytes(), content.as_bytes()] {
            h.update(&(part.len() as u64).to_le_bytes());
            h.update(part);
        }
        *h.finalize().as_bytes()
    }

    fn entry_path(&self, key: &[u8; 32]) -> PathBuf {
        let hex = blake3::Hash::from(*key).to_hex();
        self.dir.join(&hex[..2]).join(format!("{}.lcd", &hex[2..]))
    }

    fn load(&self, key: &[u8; 32]) -> Option<Vec<u8>> {
        let path = self.entry_path(key);
        let bytes = read_entry(&path)?;
        if let Some(payload) = decode_entry(&bytes, key) {
            touch(&path);
            Some(payload.to_vec())
        } else {
            let _ = fs::remove_file(&path);
            None
        }
    }

    fn save(&self, key: &[u8; 32], payload: &[u8]) {
        if payload.len() > MAX_PAYLOAD_BYTES {
            return;
        }
        let path = self.entry_path(key);
        let Some(shard) = path.parent() else { return };
        if create_private_dir(shard).is_none() {
            return;
        }
        let mut bytes = Vec::with_capacity(HEADER_LEN + payload.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(key);
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(blake3::hash(payload).as_bytes());
        bytes.extend_from_slice(payload);

        let n = self.tmp_counter.fetch_add(1, Ordering::Relaxed);
        let tmp = shard.join(format!(".{}.{n}.tmp", std::process::id()));
        if write_private_new(&tmp, &bytes).is_err() || fs::rename(&tmp, &path).is_err() {
            let _ = fs::remove_file(&tmp);
            return;
        }
        let written = self
            .written_since_sweep
            .fetch_add(bytes.len() as u64, Ordering::Relaxed)
            + bytes.len() as u64;
        if written >= self.max_bytes / 8 {
            self.sweep();
        }
    }

    /// Keep the store under its budget: remove stale temp files, then the
    /// least recently used entries until the total is at most 80 % of it.
    /// A sweep already running makes this a no-op that keeps the written
    /// count, so the next save triggers a sweep again.
    fn sweep(&self) {
        if self.sweeping.swap(true, Ordering::AcqRel) {
            return;
        }
        self.written_since_sweep.store(0, Ordering::Relaxed);
        let now = SystemTime::now();
        let mut entries: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        let mut total: u64 = 0;
        'walk: for shard in fs::read_dir(&self.dir).into_iter().flatten().flatten() {
            let Ok(meta) = shard.metadata() else { continue };
            if !meta.is_dir() {
                continue;
            }
            for file in fs::read_dir(shard.path()).into_iter().flatten().flatten() {
                let Ok(meta) = file.metadata() else { continue };
                if !meta.is_file() {
                    continue;
                }
                let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                let path = file.path();
                if path.extension().is_some_and(|e| e == "tmp") {
                    if now.duration_since(modified).unwrap_or_default() > STALE_TMP {
                        let _ = fs::remove_file(&path);
                    }
                    continue;
                }
                if path.extension().is_none_or(|e| e != "lcd") {
                    continue;
                }
                total += meta.len();
                entries.push((modified, meta.len(), path));
                if entries.len() >= SWEEP_MAX_ENTRIES {
                    break 'walk;
                }
            }
        }
        if total > self.max_bytes {
            entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
            let target = self.max_bytes / 10 * 8;
            for (_, len, path) in entries {
                if total <= target {
                    break;
                }
                if fs::remove_file(&path).is_ok() {
                    total = total.saturating_sub(len);
                }
            }
        }
        self.sweeping.store(false, Ordering::Release);
    }

    /// Serve `kind` for `content` from the store, or compute it. `compute`
    /// returns the value and whether it may be stored.
    pub(crate) fn get_or_compute<T, S>(
        &self,
        kind: Kind,
        selector: &str,
        content: &str,
        decode: impl FnOnce(S) -> Option<T>,
        encode: impl FnOnce(&T) -> Option<S>,
        compute: impl FnOnce() -> (T, bool),
    ) -> T
    where
        S: Serialize + for<'de> Deserialize<'de>,
    {
        let key = self.key(kind, selector, content);
        if let Some(payload) = self.load(&key) {
            if let Some(value) = postcard::from_bytes::<S>(&payload).ok().and_then(decode) {
                return value;
            }
            let _ = fs::remove_file(self.entry_path(&key));
        }
        let (value, storable) = compute();
        if storable
            && let Some(stored) = encode(&value)
            && let Ok(payload) = postcard::to_stdvec(&stored)
        {
            self.save(&key, &payload);
        }
        value
    }
}

fn create_private_dir(dir: &Path) -> Option<()> {
    if fs::symlink_metadata(dir).is_err() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir).ok()?;
    }
    let meta = fs::symlink_metadata(dir).ok()?;
    if !meta.file_type().is_dir() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        if meta.uid() != euid || meta.mode() & 0o022 != 0 {
            return None;
        }
    }
    Some(())
}

fn read_entry(path: &Path) -> Option<Vec<u8>> {
    let limit = (HEADER_LEN + MAX_PAYLOAD_BYTES) as u64;
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() > limit {
        return None;
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    let opened = file.metadata().ok()?;
    if !opened.is_file() || opened.len() > limit {
        return None;
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

fn decode_entry<'a>(bytes: &'a [u8], key: &[u8; 32]) -> Option<&'a [u8]> {
    if bytes.len() < HEADER_LEN || &bytes[..8] != MAGIC || &bytes[8..40] != key {
        return None;
    }
    let len = u32::from_le_bytes(bytes[40..44].try_into().ok()?) as usize;
    let payload = &bytes[HEADER_LEN..];
    if len > MAX_PAYLOAD_BYTES || payload.len() != len {
        return None;
    }
    (blake3::hash(payload).as_bytes() == &bytes[44..76]).then_some(payload)
}

fn write_private_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)
}

fn touch(path: &Path) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    let old = meta
        .modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .is_some_and(|age| age > TOUCH_AFTER);
    if !old {
        return;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    if let Ok(file) = options.open(path)
        && file.metadata().is_ok_and(|m| m.is_file())
    {
        let _ = file.set_modified(SystemTime::now());
    }
}

/// The build identity: a cached derivation is only valid for the parser that
/// produced it. The running binary's size and mtime change on every build or
/// install, so a development build never serves another build's results.
#[cfg_attr(test, allow(dead_code))]
fn build_identity() -> [u8; 32] {
    let mut h = blake3::Hasher::new_derive_key("lean-ctx parse derivation build identity v1");
    h.update(env!("CARGO_PKG_VERSION").as_bytes());
    h.update(b"\0");
    h.update(option_env!("LEANCTX_BUILD_COMMIT").unwrap_or("").as_bytes());
    h.update(b"\0");
    h.update(option_env!("LEANCTX_BUILD_DIRTY").unwrap_or("").as_bytes());
    h.update(&[u8::from(cfg!(feature = "tree-sitter"))]);
    if let Ok(meta) = std::env::current_exe().and_then(fs::metadata) {
        h.update(&meta.len().to_le_bytes());
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());
        h.update(&mtime.to_le_bytes());
    }
    *h.finalize().as_bytes()
}

#[cfg_attr(test, allow(dead_code))]
fn env_flag(name: &str) -> Option<bool> {
    match std::env::var(name)
        .ok()?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg_attr(test, allow(dead_code))]
fn open_configured() -> Option<Arc<Store>> {
    let config = crate::core::config::Config::load().cache;
    if !env_flag("LEAN_CTX_DERIVATION_CACHE").unwrap_or(config.derivations_enabled) {
        return None;
    }
    let dir = std::env::var("LEAN_CTX_DERIVATION_CACHE_DIR")
        .ok()
        .filter(|d| !d.trim().is_empty())
        .or(config.derivations_dir)
        .map(PathBuf::from)
        .or_else(|| {
            crate::core::paths::cache_dir()
                .ok()
                .map(|d| d.join("derivations"))
        })?;
    let max_bytes = if config.derivations_max_bytes == 0 {
        DEFAULT_MAX_BYTES
    } else {
        config.derivations_max_bytes
    };
    let Some(store) = Store::open(&dir, build_identity(), max_bytes) else {
        tracing::warn!(
            "[derivation cache: {} is not a private directory of this user — parsing without the cache]",
            dir.display()
        );
        return None;
    };
    // The first sweep (stale temp files, a budget lowered since the last run)
    // walks the whole store: keep it off the indexing path.
    let store = Arc::new(store);
    let sweeper = Arc::clone(&store);
    let _ = std::thread::Builder::new()
        .name("lean-ctx-derivation-sweep".into())
        .spawn(move || sweeper.sweep());
    Some(store)
}

#[cfg(test)]
thread_local! {
    static TEST_OVERRIDE: std::cell::RefCell<Option<Arc<Store>>> =
        const { std::cell::RefCell::new(None) };
}

fn active() -> Option<Arc<Store>> {
    #[cfg(test)]
    {
        TEST_OVERRIDE.with(|o| o.borrow().clone())
    }
    #[cfg(not(test))]
    {
        static STORE: std::sync::OnceLock<Option<Arc<Store>>> = std::sync::OnceLock::new();
        STORE.get_or_init(open_configured).clone()
    }
}

/// Run `f` with `store` as the store (tests only). The store is visible only
/// to `f`'s thread and a private rayon pool `f` runs in, so parallel tests of
/// other modules never read or write it.
#[cfg(test)]
pub(crate) fn with_store<R: Send>(store: Option<Arc<Store>>, f: impl FnOnce() -> R + Send) -> R {
    let for_workers = store.clone();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(rayon::current_num_threads().clamp(2, 8))
        .start_handler(move |_| {
            TEST_OVERRIDE.with(|o| *o.borrow_mut() = for_workers.clone());
        })
        .build()
        .expect("test rayon pool");
    pool.install(|| {
        TEST_OVERRIDE.with(|o| *o.borrow_mut() = store);
        f()
    })
}

// ---------------------------------------------------------------------------
// Derivations
// ---------------------------------------------------------------------------

/// BM25 chunks of `content` at repository-relative `rel`
/// (`bm25_index::chunking::extract_chunks`).
pub(crate) fn chunks(rel: &str, content: &str) -> Vec<CodeChunk> {
    let compute = || crate::core::bm25_index::extract_chunks(rel, content);
    let Some(store) = active() else {
        return compute();
    };
    store.get_or_compute(
        Kind::Chunks,
        rel,
        content,
        |stored: Vec<StoredChunk>| {
            stored
                .into_iter()
                .map(|c| c.into_chunk(rel))
                .collect::<Option<Vec<_>>>()
        },
        |chunks: &Vec<CodeChunk>| chunks.iter().map(StoredChunk::from_chunk).collect(),
        || (compute(), true),
    )
}

/// `deep_queries::analyze(content, ext)`.
pub(crate) fn deep_analysis(content: &str, ext: &str) -> DeepAnalysis {
    let compute = || crate::core::deep_queries::analyze(content, ext);
    let Some(store) = active() else {
        return compute();
    };
    store.get_or_compute(
        Kind::DeepAnalysis,
        ext,
        content,
        StoredAnalysis::into_analysis,
        |a: &DeepAnalysis| Some(StoredAnalysis::from_analysis(a)),
        || (compute(), true),
    )
}

/// `signatures::extract_signatures(content, ext)`.
pub(crate) fn signatures(content: &str, ext: &str) -> Vec<Signature> {
    let blocked = crate::core::signatures::tree_sitter_language_blocked;
    let Some(store) = active().filter(|_| !blocked(ext)) else {
        return crate::core::signatures::extract_signatures(content, ext);
    };
    let mut parsed = false;
    let (sigs, tree_sitter) = store.get_or_compute(
        Kind::Signatures,
        ext,
        content,
        StoredSignatures::into_signatures,
        |(sigs, ts): &(Vec<Signature>, bool)| Some(StoredSignatures::from_signatures(sigs, *ts)),
        || {
            parsed = true;
            let (sigs, backend) = crate::core::signatures::extract_signatures_counted(content, ext);
            let ts = matches!(backend, crate::core::signatures::SigBackend::TreeSitter);
            // A panic during this parse blocklists the language and returns
            // the regex fallback: that result is not the parser's output.
            ((sigs, ts), !blocked(ext))
        },
    );
    if !parsed {
        // A served result counts like the extraction it replaces.
        crate::core::signatures::count_signature_backend(if tree_sitter {
            crate::core::signatures::SigBackend::TreeSitter
        } else {
            crate::core::signatures::SigBackend::Regex
        });
    }
    crate::core::grammar_usage::record(ext, tree_sitter);
    sigs
}

// ---------------------------------------------------------------------------
// Stored forms: plain data, decoded strictly; anything unknown is a miss.
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct StoredChunk {
    symbol_name: String,
    kind: u8,
    start_line: usize,
    end_line: usize,
    content: String,
    tokens: Vec<String>,
    token_count: usize,
}

// Kind tags. Encoding matches exhaustively, so a new upstream variant fails to
// compile here until it has a tag; decoding an unknown tag is a miss.

/// Parse-shaped chunk kinds only: external source kinds are never stored.
fn chunk_kind_tag(kind: &ChunkKind) -> Option<u8> {
    Some(match kind {
        ChunkKind::Function => 0,
        ChunkKind::Struct => 1,
        ChunkKind::Impl => 2,
        ChunkKind::Module => 3,
        ChunkKind::Class => 4,
        ChunkKind::Method => 5,
        ChunkKind::Other => 6,
        ChunkKind::Issue
        | ChunkKind::PullRequest
        | ChunkKind::WikiPage
        | ChunkKind::DbSchema
        | ChunkKind::ApiEndpoint
        | ChunkKind::Ticket
        | ChunkKind::ExternalOther => return None,
    })
}

fn chunk_kind_from_tag(tag: u8) -> Option<ChunkKind> {
    Some(match tag {
        0 => ChunkKind::Function,
        1 => ChunkKind::Struct,
        2 => ChunkKind::Impl,
        3 => ChunkKind::Module,
        4 => ChunkKind::Class,
        5 => ChunkKind::Method,
        6 => ChunkKind::Other,
        _ => return None,
    })
}

fn import_kind_tag(kind: &ImportKind) -> u8 {
    match kind {
        ImportKind::Named => 0,
        ImportKind::Default => 1,
        ImportKind::Star => 2,
        ImportKind::SideEffect => 3,
        ImportKind::Dynamic => 4,
        ImportKind::Reexport => 5,
    }
}

fn import_kind_from_tag(tag: u8) -> Option<ImportKind> {
    Some(match tag {
        0 => ImportKind::Named,
        1 => ImportKind::Default,
        2 => ImportKind::Star,
        3 => ImportKind::SideEffect,
        4 => ImportKind::Dynamic,
        5 => ImportKind::Reexport,
        _ => return None,
    })
}

fn type_kind_tag(kind: &TypeDefKind) -> u8 {
    match kind {
        TypeDefKind::Class => 0,
        TypeDefKind::Interface => 1,
        TypeDefKind::TypeAlias => 2,
        TypeDefKind::Enum => 3,
        TypeDefKind::Struct => 4,
        TypeDefKind::Trait => 5,
        TypeDefKind::Protocol => 6,
        TypeDefKind::Record => 7,
        TypeDefKind::Annotation => 8,
        TypeDefKind::Union => 9,
    }
}

fn type_kind_from_tag(tag: u8) -> Option<TypeDefKind> {
    Some(match tag {
        0 => TypeDefKind::Class,
        1 => TypeDefKind::Interface,
        2 => TypeDefKind::TypeAlias,
        3 => TypeDefKind::Enum,
        4 => TypeDefKind::Struct,
        5 => TypeDefKind::Trait,
        6 => TypeDefKind::Protocol,
        7 => TypeDefKind::Record,
        8 => TypeDefKind::Annotation,
        9 => TypeDefKind::Union,
        _ => return None,
    })
}

impl StoredChunk {
    /// Only parse-shaped chunks are stored: a chunk with an origin or an
    /// external kind did not come from the parser.
    fn from_chunk(c: &CodeChunk) -> Option<Self> {
        if c.input_origin.is_some() {
            return None;
        }
        let kind = chunk_kind_tag(&c.kind)?;
        Some(Self {
            symbol_name: c.symbol_name.clone(),
            kind,
            start_line: c.start_line,
            end_line: c.end_line,
            content: c.content.clone(),
            tokens: c.tokens.clone(),
            token_count: c.token_count,
        })
    }

    fn into_chunk(self, rel: &str) -> Option<CodeChunk> {
        Some(CodeChunk {
            file_path: rel.to_string(),
            symbol_name: self.symbol_name,
            kind: chunk_kind_from_tag(self.kind)?,
            start_line: self.start_line,
            end_line: self.end_line,
            content: self.content,
            tokens: self.tokens,
            token_count: self.token_count,
            input_origin: None,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct StoredAnalysis {
    imports: Vec<(String, Vec<String>, u8, usize, bool)>,
    calls: Vec<StoredCall>,
    types: Vec<StoredType>,
    exports: Vec<String>,
    type_uses: Vec<(String, usize)>,
    ext_methods: Vec<(String, usize, usize)>,
}

#[derive(Serialize, Deserialize)]
struct StoredCall {
    callee: String,
    line: usize,
    col: usize,
    receiver: Option<String>,
    is_method: bool,
    callee_pos: Option<(usize, usize)>,
}

#[derive(Serialize, Deserialize)]
struct StoredType {
    name: String,
    kind: u8,
    line: usize,
    end_line: usize,
    is_exported: bool,
    generics: Vec<String>,
    namespace: Option<String>,
}

impl StoredAnalysis {
    fn from_analysis(a: &DeepAnalysis) -> Self {
        Self {
            imports: a
                .imports
                .iter()
                .map(|i| {
                    (
                        i.source.clone(),
                        i.names.clone(),
                        import_kind_tag(&i.kind),
                        i.line,
                        i.is_type_only,
                    )
                })
                .collect(),
            calls: a
                .calls
                .iter()
                .map(|c| StoredCall {
                    callee: c.callee.clone(),
                    line: c.line,
                    col: c.col,
                    receiver: c.receiver.clone(),
                    is_method: c.is_method,
                    callee_pos: c.callee_pos,
                })
                .collect(),
            types: a
                .types
                .iter()
                .map(|t| StoredType {
                    name: t.name.clone(),
                    kind: type_kind_tag(&t.kind),
                    line: t.line,
                    end_line: t.end_line,
                    is_exported: t.is_exported,
                    generics: t.generics.clone(),
                    namespace: t.namespace.clone(),
                })
                .collect(),
            exports: a.exports.clone(),
            type_uses: a
                .type_uses
                .iter()
                .map(|u| (u.name.clone(), u.line))
                .collect(),
            ext_methods: a
                .ext_methods
                .iter()
                .map(|m| (m.name.clone(), m.line, m.end_line))
                .collect(),
        }
    }

    fn into_analysis(self) -> Option<DeepAnalysis> {
        let imports = self
            .imports
            .into_iter()
            .map(|(source, names, kind, line, is_type_only)| {
                Some(ImportInfo {
                    source,
                    names,
                    kind: import_kind_from_tag(kind)?,
                    line,
                    is_type_only,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let types = self
            .types
            .into_iter()
            .map(|t| {
                Some(TypeDef {
                    name: t.name,
                    kind: type_kind_from_tag(t.kind)?,
                    line: t.line,
                    end_line: t.end_line,
                    is_exported: t.is_exported,
                    generics: t.generics,
                    namespace: t.namespace,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(DeepAnalysis {
            imports,
            calls: self
                .calls
                .into_iter()
                .map(|c| CallSite {
                    callee: c.callee,
                    line: c.line,
                    col: c.col,
                    receiver: c.receiver,
                    is_method: c.is_method,
                    callee_pos: c.callee_pos,
                })
                .collect(),
            types,
            exports: self.exports,
            type_uses: self
                .type_uses
                .into_iter()
                .map(|(name, line)| TypeUse { name, line })
                .collect(),
            ext_methods: self
                .ext_methods
                .into_iter()
                .map(|(name, line, end_line)| ExtMethodDef {
                    name,
                    line,
                    end_line,
                })
                .collect(),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct StoredSignatures {
    tree_sitter: bool,
    sigs: Vec<StoredSignature>,
}

#[derive(Serialize, Deserialize)]
struct StoredSignature {
    kind: String,
    name: String,
    params: String,
    return_type: String,
    is_async: bool,
    is_exported: bool,
    indent: usize,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

/// Signature kinds are `&'static str`. A decoded kind is interned once; the
/// set is small and bounded, and a kind outside the bound is a miss.
fn intern_kind(kind: &str) -> Option<&'static str> {
    const MAX_KINDS: usize = 64;
    const MAX_KIND_LEN: usize = 32;
    static KINDS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    if kind.is_empty() || kind.len() > MAX_KIND_LEN || !kind.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    let mut kinds = KINDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(known) = kinds.iter().find(|k| **k == kind) {
        return Some(known);
    }
    if kinds.len() >= MAX_KINDS {
        return None;
    }
    let leaked: &'static str = Box::leak(kind.to_owned().into_boxed_str());
    kinds.push(leaked);
    Some(leaked)
}

impl StoredSignatures {
    fn from_signatures(sigs: &[Signature], tree_sitter: bool) -> Self {
        Self {
            tree_sitter,
            sigs: sigs
                .iter()
                .map(|s| StoredSignature {
                    kind: s.kind.to_string(),
                    name: s.name.clone(),
                    params: s.params.clone(),
                    return_type: s.return_type.clone(),
                    is_async: s.is_async,
                    is_exported: s.is_exported,
                    indent: s.indent,
                    start_line: s.start_line,
                    end_line: s.end_line,
                })
                .collect(),
        }
    }

    fn into_signatures(self) -> Option<(Vec<Signature>, bool)> {
        let sigs = self
            .sigs
            .into_iter()
            .map(|s| {
                Some(Signature {
                    kind: intern_kind(&s.kind)?,
                    name: s.name,
                    params: s.params,
                    return_type: s.return_type,
                    is_async: s.is_async,
                    is_exported: s.is_exported,
                    indent: s.indent,
                    start_line: s.start_line,
                    end_line: s.end_line,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some((sigs, self.tree_sitter))
    }
}

#[cfg(test)]
#[path = "derivation_cache_tests.rs"]
mod tests;
