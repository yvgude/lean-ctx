//! Resident line-search index for `ctx_search` (Phase 1 of the efficiency epic).
//!
//! Historically `ctx_search` walked the filesystem, read every file, and ran a
//! regex on every line on *every* call — `O(files × lines)`. That is the
//! 40–200 ms latency floor this module eliminates.
//!
//! This module keeps a RAM-resident trigram index (`trigram → file ids`) so the
//! common case (an identifier / literal query) collapses to: intersect a few
//! posting lists in memory → read & regex-verify only the handful of candidate
//! files. The index never decides matches itself; it only *narrows the file
//! set*, then `ctx_search` verifies candidates with the exact same regex loop,
//! so the returned `file:line` hits are byte-identical to the walk path.
//!
//! Design notes:
//! - The index is built with the *same* walk config and file filters as
//!   `ctx_search` (see [`crate::tools::ctx_search`]) so the searchable universe
//!   is identical — that is what guarantees recall parity.
//! - Only `[A-Za-z0-9_]` trigrams are indexed. Lookups only ever use trigrams
//!   from pure-identifier queries, so this is both sufficient and memory-bounded.
//! - Narrowing is applied *only* for pure `[A-Za-z0-9_]` literal queries (the
//!   dominant agent case). Any query containing a regex metacharacter falls
//!   back to scanning the cached file list (still skips the directory walk).
//! - Freshness uses a short TTL with background rebuild, mirroring
//!   [`crate::core::bm25_cache`]. A real fs watcher is Phase 5.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use glob::Pattern;
use ignore::WalkBuilder;

use crate::tools::ctx_search::{MAX_FILE_SIZE, MAX_WALK_DEPTH, is_binary_ext, is_generated_file};

/// Freshness window before a background rebuild is triggered. Matches the
/// bounded-staleness model already used by the BM25 cache.
const TTL: Duration = Duration::from_secs(15);

/// Upper bound on indexed files; larger trees fall back to the walk path.
const MAX_FILES: usize = 200_000;

/// Posting-entry budget (`file_id` occurrences across all trigrams). Up to this
/// many entries we keep exact inverted posting lists (fastest, sparse lookups).
/// Beyond it we switch to the per-file Bloom tier instead of giving up — see
/// [`Narrowing`]. ~4 bytes each → ~48 MB before the switch.
const MAX_POSTING_ENTRIES: usize = 12_000_000;

/// Hard ceiling on total trigram entries collected during a build. Past this we
/// abandon indexing (walk fallback) to avoid pathological memory use even with
/// the compact Bloom tier.
const MAX_TOTAL_ENTRIES: usize = 48_000_000;

/// Bloom tuning: bits per distinct trigram and number of hash probes. ~12 bits
/// with k=7 keeps the false-positive rate well under 1% — and a false positive
/// only costs one extra regex-verified file read (never a missed match).
const BLOOM_BITS_PER_ITEM: usize = 12;
const BLOOM_K: usize = 7;
/// Per-file Bloom size clamp (in bits): 64 bits min, 1 Mi bits (128 KiB) max.
const BLOOM_MIN_BITS: usize = 64;
const BLOOM_MAX_BITS: usize = 1 << 20;

/// A trigram is indexable only if all three bytes are `[A-Za-z0-9_]`.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn pack(b0: u8, b1: u8, b2: u8) -> u32 {
    (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2)
}

/// How candidate files are narrowed for a literal query. Two tiers, chosen by
/// corpus size, both providing a *superset* of true matches (zero false
/// negatives) which `ctx_search` then regex-verifies:
/// - `Postings`: exact inverted lists `trigram → sorted file ids`. Fast, sparse
///   lookups; used while total entries fit [`MAX_POSTING_ENTRIES`].
/// - `Blooms`: one compact per-file Bloom filter of the file's trigrams. ~3×
///   smaller than postings, so monorepos that would otherwise blow the posting
///   budget still get index-narrowing instead of a full directory walk.
enum Narrowing {
    Postings(HashMap<u32, Vec<u32>>),
    Blooms(Vec<FileBloom>),
}

/// A per-file Bloom filter over the file's word-trigrams. No false negatives:
/// if any probed bit for a trigram is unset, the file provably lacks it.
struct FileBloom {
    /// Bit storage; the filter width `m = bits.len() * 64` is a power of two.
    bits: Vec<u64>,
}

/// 64-bit avalanche mix (splitmix64 finalizer) — spreads a packed trigram into
/// a well-distributed hash for double-probing.
#[inline]
fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

impl FileBloom {
    fn with_capacity(distinct_trigrams: usize) -> Self {
        let target = distinct_trigrams
            .saturating_mul(BLOOM_BITS_PER_ITEM)
            .next_power_of_two()
            .clamp(BLOOM_MIN_BITS, BLOOM_MAX_BITS);
        FileBloom {
            bits: vec![0u64; target / 64],
        }
    }

    #[inline]
    fn m_bits(&self) -> usize {
        self.bits.len() * 64
    }

    /// Double hashing: `p_i = h1 + i·h2 (mod m)` with `m` a power of two.
    #[inline]
    fn probes(&self, trigram: u32) -> impl Iterator<Item = usize> + '_ {
        let m = self.m_bits();
        let mask = m - 1; // m is a power of two
        let h = mix64(u64::from(trigram));
        let h1 = (h & 0xFFFF_FFFF) as usize;
        let h2 = ((h >> 32) as usize) | 1; // odd step → full-period probing
        (0..BLOOM_K).map(move |i| h1.wrapping_add(i.wrapping_mul(h2)) & mask)
    }

    fn insert(&mut self, trigram: u32) {
        for p in self.probes(trigram).collect::<Vec<_>>() {
            self.bits[p / 64] |= 1u64 << (p % 64);
        }
    }

    fn maybe_contains(&self, trigram: u32) -> bool {
        self.probes(trigram)
            .all(|p| self.bits[p / 64] & (1u64 << (p % 64)) != 0)
    }
}

/// RAM-resident trigram index over one project root.
pub struct SearchIndex {
    files: Vec<PathBuf>,
    /// Candidate-narrowing structure (exact postings or compact per-file Bloom).
    narrowing: Narrowing,
    respect_gitignore: bool,
    allow_secret_paths: bool,
    built_at: Instant,
}

impl SearchIndex {
    /// Build the index by walking `root` with the exact same config and filters
    /// as `ctx_search`, so the searchable file universe is identical.
    pub fn build(root: &str, respect_gitignore: bool, allow_secret_paths: bool) -> Option<Self> {
        let root_path = Path::new(root);
        if !root_path.exists() {
            return None;
        }
        // Never auto-index a broad/unsafe root (HOME, filesystem root, a dir with
        // dozens of unrelated subtrees). This mirrors the graph/BM25 guard and
        // stops a background build from walking the whole home directory — which
        // on Windows would hydrate OneDrive placeholders (#363).
        if !crate::core::graph_index::is_safe_scan_root_public(root) {
            return None;
        }

        let walker = WalkBuilder::new(root_path)
            .hidden(true)
            .max_depth(Some(MAX_WALK_DEPTH))
            .git_ignore(respect_gitignore)
            .git_global(respect_gitignore)
            .git_exclude(respect_gitignore)
            .require_git(false)
            .filter_entry(crate::core::walk_filter::keep_entry)
            .build();

        let mut files: Vec<PathBuf> = Vec::new();
        // Per-file sorted, deduped trigrams. Same memory as the posting lists
        // would be, but grouped by file so we can materialise *either* tier
        // afterwards without a second pass over the corpus.
        let mut per_file_trigrams: Vec<Vec<u32>> = Vec::new();
        let mut total_entries: usize = 0;
        let mut scratch: HashSet<u32> = HashSet::new();

        for entry in walker.filter_map(std::result::Result::ok) {
            if entry.file_type().is_none_or(|ft| ft.is_dir()) {
                continue;
            }
            if entry.file_type().is_some_and(|ft| ft.is_symlink()) {
                continue;
            }
            let path = entry.path();
            if is_binary_ext(path) || is_generated_file(path) {
                continue;
            }
            if !allow_secret_paths && crate::core::io_boundary::is_secret_like(path).is_some() {
                continue;
            }
            // Only index regular files within the size budget. A FIFO/socket/
            // device node would block the `read_to_string` below forever (#336),
            // hanging the background build and starving the fast path. `metadata`
            // (stat) never opens the file, so it is safe on special files.
            let state = match std::fs::metadata(path) {
                Ok(meta) if !meta.file_type().is_file() => continue,
                Ok(meta) if meta.len() > MAX_FILE_SIZE => continue,
                Ok(meta) => crate::core::content_cache::FileState::from_metadata(&meta),
                Err(_) => continue,
            };
            // Read the corpus exactly once (issue #148): reuse a fresh cached
            // copy if a prior `ctx_search`/build already read this file, else
            // read it now and publish it so the upcoming `ctx_search` verify
            // pass is an in-memory hit instead of a second disk read. Mirrors
            // ctx_search: a non-UTF-8 file is never searchable, so it is skipped.
            let content: std::sync::Arc<str> = if let Some(cached) =
                state.and_then(|s| crate::core::content_cache::get(path, s))
            {
                cached
            } else {
                let Ok(text) = std::fs::read_to_string(path) else {
                    continue;
                };
                let arc: std::sync::Arc<str> = std::sync::Arc::from(text);
                if let Some(s) = state {
                    crate::core::content_cache::insert(path, s, std::sync::Arc::clone(&arc));
                }
                arc
            };

            if files.len() >= MAX_FILES {
                return None; // too large even for the Bloom tier — use the walk
            }

            scratch.clear();
            let bytes = content.as_bytes();
            if bytes.len() >= 3 {
                for w in bytes.windows(3) {
                    if is_word_byte(w[0]) && is_word_byte(w[1]) && is_word_byte(w[2]) {
                        scratch.insert(pack(w[0], w[1], w[2]));
                    }
                }
            }
            total_entries += scratch.len();
            if total_entries > MAX_TOTAL_ENTRIES {
                return None; // memory guard — fall back to walk
            }
            let mut tris: Vec<u32> = scratch.iter().copied().collect();
            tris.sort_unstable();
            files.push(path.to_path_buf());
            per_file_trigrams.push(tris);
        }

        let narrowing = build_narrowing(&per_file_trigrams, total_entries);

        Some(Self {
            files,
            narrowing,
            respect_gitignore,
            allow_secret_paths,
            built_at: Instant::now(),
        })
    }

    fn is_fresh(&self) -> bool {
        self.built_at.elapsed() < TTL
    }

    fn config_matches(&self, respect_gitignore: bool, allow_secret_paths: bool) -> bool {
        self.respect_gitignore == respect_gitignore && self.allow_secret_paths == allow_secret_paths
    }

    /// Candidate files for `pattern`, filtered by the `include` glob (matched
    /// against each file's path relative to `root`). `None` means "no safe
    /// narrowing possible" — the caller should scan the full file list.
    ///
    /// Narrowing is applied only for pure `[A-Za-z0-9_]` literals of length ≥ 3.
    /// For such a literal every match contains it on a single line, hence the
    /// file contains all of its consecutive trigrams: intersecting their
    /// posting lists yields a *superset* of matching files (zero false
    /// negatives), which the caller then regex-verifies.
    pub fn candidate_paths(
        &self,
        pattern: &str,
        includes: &[Pattern],
        root: &Path,
    ) -> CandidateSet {
        if let Some(ids) = self.literal_candidates(pattern) {
            let paths = ids
                .into_iter()
                .map(|id| self.files[id as usize].clone())
                .filter(|p| glob_matches(p, includes, root))
                .collect();
            CandidateSet::Narrowed(paths)
        } else {
            let paths = self
                .files
                .iter()
                .filter(|p| glob_matches(p, includes, root))
                .cloned()
                .collect();
            CandidateSet::FullList(paths)
        }
    }

    /// Returns candidate file ids for a pure-literal query, or `None` if the
    /// query is not a trigram-narrowable pure `[A-Za-z0-9_]` literal. Both tiers
    /// return a *superset* of true matches (zero false negatives).
    fn literal_candidates(&self, pattern: &str) -> Option<Vec<u32>> {
        let bytes = pattern.as_bytes();
        if bytes.len() < 3 || !bytes.iter().all(|&b| is_word_byte(b)) {
            return None;
        }
        // Distinct trigrams of the literal.
        let mut tris: Vec<u32> = bytes.windows(3).map(|w| pack(w[0], w[1], w[2])).collect();
        tris.sort_unstable();
        tris.dedup();

        match &self.narrowing {
            Narrowing::Postings(trigrams) => Some(Self::postings_intersect(trigrams, &tris)),
            Narrowing::Blooms(blooms) => Some(Self::bloom_scan(blooms, &tris)),
        }
    }

    /// Exact-tier: intersect the posting lists of every required trigram
    /// (smallest first for a cheap intersection).
    fn postings_intersect(trigrams: &HashMap<u32, Vec<u32>>, tris: &[u32]) -> Vec<u32> {
        let mut lists: Vec<&Vec<u32>> = Vec::with_capacity(tris.len());
        for &tri in tris {
            match trigrams.get(&tri) {
                // A required trigram is absent → provably no match anywhere.
                None => return Vec::new(),
                Some(list) => lists.push(list),
            }
        }
        lists.sort_by_key(|l| l.len());

        let mut acc: Vec<u32> = lists[0].clone();
        for list in &lists[1..] {
            acc = intersect_sorted(&acc, list);
            if acc.is_empty() {
                break;
            }
        }
        acc
    }

    /// Bloom-tier: a file is a candidate iff its Bloom filter may contain every
    /// required trigram. No false negatives (an unset probe bit ⇒ the trigram is
    /// provably absent), so the result is still a superset of true matches.
    fn bloom_scan(blooms: &[FileBloom], tris: &[u32]) -> Vec<u32> {
        let mut out = Vec::new();
        for (fid, bloom) in blooms.iter().enumerate() {
            if tris.iter().all(|&t| bloom.maybe_contains(t)) {
                out.push(fid as u32);
            }
        }
        out
    }
}

/// Materialise the appropriate narrowing tier for a freshly walked corpus.
fn build_narrowing(per_file: &[Vec<u32>], total_entries: usize) -> Narrowing {
    if total_entries <= MAX_POSTING_ENTRIES {
        let mut trigrams: HashMap<u32, Vec<u32>> = HashMap::new();
        for (fid, tris) in per_file.iter().enumerate() {
            for &t in tris {
                // file ids are appended in ascending order ⇒ lists stay sorted.
                trigrams.entry(t).or_default().push(fid as u32);
            }
        }
        Narrowing::Postings(trigrams)
    } else {
        let blooms = per_file
            .iter()
            .map(|tris| {
                let mut b = FileBloom::with_capacity(tris.len());
                for &t in tris {
                    b.insert(t);
                }
                b
            })
            .collect();
        Narrowing::Blooms(blooms)
    }
}

/// Result of [`SearchIndex::candidate_paths`].
pub enum CandidateSet {
    /// Trigram-narrowed candidate files (a superset of real matches).
    Narrowed(Vec<PathBuf>),
    /// No safe narrowing — the full cached file list (still skips the walk).
    FullList(Vec<PathBuf>),
}

impl CandidateSet {
    pub fn into_paths(self) -> Vec<PathBuf> {
        match self {
            CandidateSet::Narrowed(p) | CandidateSet::FullList(p) => p,
        }
    }
}

/// True when `path` matches *any* of the `includes` globs (relative to `root`),
/// or when there is no filter (`includes` empty).
fn glob_matches(path: &Path, includes: &[Pattern], root: &Path) -> bool {
    if includes.is_empty() {
        return true;
    }
    let rel = path.strip_prefix(root).unwrap_or(path);
    let rel_str = rel.to_string_lossy();
    includes.iter().any(|p| p.matches(&rel_str))
}

/// Intersection of two ascending, deduped `u32` slices.
fn intersect_sorted(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Resident cache (one index per project root) with background (re)build.
// ---------------------------------------------------------------------------

struct CacheEntry {
    index: Option<Arc<SearchIndex>>,
    building: bool,
}

static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Escape hatch: `LEAN_CTX_DISABLE_SEARCH_INDEX=1` forces the walk path
/// everywhere (debugging / A-B measurement / opt-out).
fn index_disabled() -> bool {
    std::env::var("LEAN_CTX_DISABLE_SEARCH_INDEX")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Returns a fresh resident index for `root` if one is available for the given
/// config, otherwise spawns a background (re)build and returns `None` so the
/// caller uses the walk fallback for this call.
pub fn get_fresh(
    root: &str,
    respect_gitignore: bool,
    allow_secret_paths: bool,
) -> Option<Arc<SearchIndex>> {
    // Privileged "ignore gitignore" scans are rare and bypass the index.
    if !respect_gitignore || index_disabled() {
        return None;
    }

    let mut needs_build = false;
    let result = {
        let mut map = cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = map.entry(root.to_string()).or_insert(CacheEntry {
            index: None,
            building: false,
        });
        match &entry.index {
            Some(idx)
                if idx.config_matches(respect_gitignore, allow_secret_paths) && idx.is_fresh() =>
            {
                Some(Arc::clone(idx))
            }
            Some(idx) if idx.config_matches(respect_gitignore, allow_secret_paths) => {
                // Stale but usable: serve it and refresh in the background.
                needs_build = !entry.building;
                if needs_build {
                    entry.building = true;
                }
                Some(Arc::clone(idx))
            }
            _ => {
                needs_build = !entry.building;
                if needs_build {
                    entry.building = true;
                }
                None
            }
        }
    };

    if needs_build {
        spawn_build(root.to_string(), respect_gitignore, allow_secret_paths);
    }
    result
}

/// Ensure a resident index for `root` is built (or building) in the background.
/// Safe to call repeatedly; deduped via the per-root `building` flag.
pub fn ensure_background(root: &str, respect_gitignore: bool, allow_secret_paths: bool) {
    if !respect_gitignore || index_disabled() {
        return;
    }
    let needs_build = {
        let mut map = cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = map.entry(root.to_string()).or_insert(CacheEntry {
            index: None,
            building: false,
        });
        let fresh = entry.index.as_ref().is_some_and(|idx| {
            idx.config_matches(respect_gitignore, allow_secret_paths) && idx.is_fresh()
        });
        if fresh || entry.building {
            false
        } else {
            entry.building = true;
            true
        }
    };
    if needs_build {
        spawn_build(root.to_string(), respect_gitignore, allow_secret_paths);
    }
}

/// Build the index synchronously and install it in the resident cache.
/// Returns `true` on success. Useful for CLI prewarm and benchmarks that need a
/// guaranteed-warm index. Respects the disable env var.
pub fn warm_blocking(root: &str, respect_gitignore: bool, allow_secret_paths: bool) -> bool {
    if !respect_gitignore || index_disabled() {
        return false;
    }
    let Some(idx) = SearchIndex::build(root, respect_gitignore, allow_secret_paths) else {
        return false;
    };
    let mut map = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.insert(
        root.to_string(),
        CacheEntry {
            index: Some(Arc::new(idx)),
            building: false,
        },
    );
    true
}

/// Per-repo lock name serializing the resident search-index build across
/// processes, mirroring the `graph-idx` / `bm25-idx` locks in
/// [`crate::core::index_orchestrator`]. Distinct `search-` prefix so the three
/// indexers never serialize against one another.
fn search_index_lock_name(root: &str) -> String {
    format!(
        "search-idx-{}",
        &crate::core::index_namespace::namespace_hash(Path::new(root))[..8]
    )
}

/// Outcome of a guarded background build: did this process do the walk, or did
/// it yield to another process already building the same root?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildOutcome {
    Built,
    Deferred,
}

/// Build the resident index under a cross-process herd guard (#460).
///
/// The trigram index is RAM-resident (not shareable on disk), so on lock
/// contention we *defer* the proactive pre-warm instead of running a second
/// simultaneous file walk: a boot wave of N sessions on one repo then triggers
/// ~1 walk at a time, not N. Deferring is safe — `ctx_search` still works via
/// its walk fallback, and the per-process `building` flag is cleared so the next
/// `ensure_background` nudge (every search, post-TTL) retries once the holder
/// releases. The short 200 ms wait keeps the common single-session path
/// latency-free.
fn build_guarded(root: &str, respect_gitignore: bool, allow_secret_paths: bool) -> BuildOutcome {
    let lock = crate::core::startup_guard::try_acquire_lock(
        &search_index_lock_name(root),
        Duration::from_millis(200),
        Duration::from_mins(3),
    );
    if lock.is_none() {
        // Another process owns the build. Clear the in-flight flag so a later
        // nudge retries rather than leaving `building` stuck true forever.
        let mut map = cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = map.get_mut(root) {
            entry.building = false;
        }
        return BuildOutcome::Deferred;
    }

    let built = std::panic::catch_unwind(|| {
        SearchIndex::build(root, respect_gitignore, allow_secret_paths)
    })
    .ok()
    .flatten();

    let mut map = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(entry) = map.get_mut(root) {
        entry.building = false;
        if let Some(idx) = built {
            entry.index = Some(Arc::new(idx));
        }
    }
    // `lock` is held until here so the cross-process guard spans the whole walk.
    drop(lock);
    BuildOutcome::Built
}

fn spawn_build(root: String, respect_gitignore: bool, allow_secret_paths: bool) {
    std::thread::spawn(move || {
        let _ = build_guarded(&root, respect_gitignore, allow_secret_paths);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn handler() {}\nlet x = 1;\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn other() {}\n// nothing here\n").unwrap();
        std::fs::write(dir.path().join("c.txt"), "handler appears in text too\n").unwrap();
        dir
    }

    #[test]
    fn build_refuses_to_index_home_directory() {
        // Auto-indexing HOME would walk the entire home tree and, on Windows,
        // hydrate every OneDrive placeholder (#363). The build must bail out.
        if let Some(home) = dirs::home_dir() {
            assert!(
                SearchIndex::build(&home.to_string_lossy(), true, false).is_none(),
                "search index must never auto-build over the home directory"
            );
        }
    }

    #[test]
    fn narrows_to_files_containing_literal() {
        let dir = corpus();
        let idx = SearchIndex::build(dir.path().to_str().unwrap(), true, false).unwrap();
        let cands = idx.candidate_paths("handler", &[], dir.path());
        let paths = cands.into_paths();
        // a.rs and c.txt contain "handler"; b.rs must be excluded.
        assert!(paths.iter().any(|p| p.ends_with("a.rs")));
        assert!(paths.iter().any(|p| p.ends_with("c.txt")));
        assert!(!paths.iter().any(|p| p.ends_with("b.rs")));
    }

    #[test]
    fn absent_trigram_yields_empty_candidates() {
        let dir = corpus();
        let idx = SearchIndex::build(dir.path().to_str().unwrap(), true, false).unwrap();
        match idx.candidate_paths("zzzqqq", &[], dir.path()) {
            CandidateSet::Narrowed(p) => assert!(p.is_empty()),
            CandidateSet::FullList(_) => panic!("pure literal should narrow"),
        }
    }

    #[test]
    fn ext_filter_restricts_candidates() {
        let dir = corpus();
        let idx = SearchIndex::build(dir.path().to_str().unwrap(), true, false).unwrap();
        let paths = idx
            .candidate_paths(
                "handler",
                &[glob::Pattern::new("*.rs").unwrap()],
                dir.path(),
            )
            .into_paths();
        assert!(paths.iter().all(|p| p.extension().unwrap() == "rs"));
        assert!(paths.iter().any(|p| p.ends_with("a.rs")));
    }

    #[test]
    #[cfg(unix)]
    fn build_skips_named_pipe_without_hanging() {
        use std::sync::mpsc;
        use std::time::Duration;
        // #336: the background index build read every file, so a FIFO in the
        // corpus blocked the build thread forever. It must be skipped while the
        // regular files are still indexed, and the build must return.
        let dir = corpus();
        let fifo = dir.path().join("pipe.fifo");
        let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        assert_eq!(
            // SAFETY: `c` is a live CString providing a valid NUL-terminated
            // path pointer for the duration of the call.
            unsafe { libc::mkfifo(c.as_ptr(), 0o644) },
            0,
            "mkfifo failed"
        );

        let root = dir.path().to_str().unwrap().to_string();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let built = SearchIndex::build(&root, true, false);
            let _ = tx.send(built.map(|idx| {
                idx.candidate_paths("handler", &[], std::path::Path::new(&root))
                    .into_paths()
            }));
        });
        let paths = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("SearchIndex::build hung on a FIFO (#336 regression)")
            .expect("index should build");
        assert!(paths.iter().any(|p| p.ends_with("a.rs")));
        assert!(!paths.iter().any(|p| p.ends_with("pipe.fifo")));
    }

    #[test]
    fn regex_query_falls_back_to_full_list() {
        let dir = corpus();
        let idx = SearchIndex::build(dir.path().to_str().unwrap(), true, false).unwrap();
        match idx.candidate_paths("fn .*\\(\\)", &[], dir.path()) {
            CandidateSet::FullList(p) => assert!(!p.is_empty()),
            CandidateSet::Narrowed(_) => panic!("metachar query must not narrow"),
        }
    }

    #[test]
    fn short_query_falls_back_to_full_list() {
        let dir = corpus();
        let idx = SearchIndex::build(dir.path().to_str().unwrap(), true, false).unwrap();
        assert!(matches!(
            idx.candidate_paths("fn", &[], dir.path()),
            CandidateSet::FullList(_)
        ));
    }

    /// The core correctness claim: trigram narrowing never drops a real match.
    /// For each literal query, the set of `file:line` hits found by scanning only
    /// the narrowed candidates must equal the set found by scanning every file.
    #[test]
    fn narrowing_has_identical_recall_to_full_scan() {
        use regex::Regex;
        use std::collections::BTreeSet;

        let dir = tempfile::tempdir().unwrap();
        // A spread of files; some contain the query tokens, most do not.
        let samples = [
            (
                "auth/login.rs",
                "fn authenticate(user) {}\nlet token = mint();\n",
            ),
            (
                "auth/session.rs",
                "struct Session;\n// authenticate again here\n",
            ),
            ("db/pool.rs", "fn connect() {}\nlet retries = 3;\n"),
            (
                "ui/button.tsx",
                "export const Button = () => authenticate;\n",
            ),
            ("readme.md", "This project uses authenticate flows.\n"),
            ("unrelated.rs", "fn helper() { let v = 1; }\n"),
        ];
        for (rel, content) in samples {
            let p = dir.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        let idx = SearchIndex::build(dir.path().to_str().unwrap(), true, false).unwrap();

        let full_scan = |pat: &str| -> BTreeSet<String> {
            let re = Regex::new(pat).unwrap();
            let mut hits = BTreeSet::new();
            for (rel, content) in samples {
                for (i, line) in content.lines().enumerate() {
                    if re.is_match(line) {
                        hits.insert(format!("{rel}:{}", i + 1));
                    }
                }
            }
            hits
        };

        for query in ["authenticate", "Session", "retries", "token"] {
            let re = Regex::new(query).unwrap();
            let candidates = idx.candidate_paths(query, &[], dir.path()).into_paths();
            let mut narrowed = BTreeSet::new();
            for path in &candidates {
                let content = std::fs::read_to_string(path).unwrap();
                let rel = path.strip_prefix(dir.path()).unwrap().to_string_lossy();
                for (i, line) in content.lines().enumerate() {
                    if re.is_match(line) {
                        narrowed.insert(format!("{}:{}", rel.replace('\\', "/"), i + 1));
                    }
                }
            }
            assert_eq!(
                narrowed,
                full_scan(query),
                "recall mismatch for query {query:?}"
            );
        }
    }

    #[test]
    fn intersect_sorted_basic() {
        assert_eq!(
            intersect_sorted(&[1, 2, 3, 5], &[2, 3, 4, 5]),
            vec![2, 3, 5]
        );
        assert_eq!(intersect_sorted(&[1, 2], &[3, 4]), Vec::<u32>::new());
    }

    // ── Bloom tier ────────────────────────────────────────────────────────

    fn trigrams_of(s: &str) -> Vec<u32> {
        let mut set = HashSet::new();
        let b = s.as_bytes();
        if b.len() >= 3 {
            for w in b.windows(3) {
                if is_word_byte(w[0]) && is_word_byte(w[1]) && is_word_byte(w[2]) {
                    set.insert(pack(w[0], w[1], w[2]));
                }
            }
        }
        let mut v: Vec<u32> = set.into_iter().collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn file_bloom_has_no_false_negatives() {
        let tris = trigrams_of("fn authenticate(user) { let token = mint(); }");
        let mut bloom = FileBloom::with_capacity(tris.len());
        for &t in &tris {
            bloom.insert(t);
        }
        // Every inserted trigram must be reported present (Bloom guarantee).
        assert!(tris.iter().all(|&t| bloom.maybe_contains(t)));
    }

    /// Parity fuzz: the Bloom tier must return a *superset* of the exact posting
    /// tier for every query (zero false negatives). False positives are allowed
    /// (and verified away downstream), so we assert containment, not equality.
    #[test]
    fn bloom_tier_is_superset_of_postings_tier() {
        // Deterministic synthetic corpus (LCG → reproducible).
        let mut seed = 0x1234_5678_9abc_def0u64;
        let mut rng = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as u32
        };
        let mut per_file: Vec<Vec<u32>> = Vec::new();
        for _ in 0..80 {
            let n = 50 + (rng() % 250) as usize;
            let mut s = HashSet::new();
            for _ in 0..n {
                s.insert(rng() & 0x00FF_FFFF);
            }
            let mut v: Vec<u32> = s.into_iter().collect();
            v.sort_unstable();
            per_file.push(v);
        }
        let total: usize = per_file.iter().map(Vec::len).sum();

        let postings = build_narrowing(&per_file, total); // ≤ cap → postings
        let blooms = build_narrowing(&per_file, MAX_POSTING_ENTRIES + 1); // forced bloom
        let (Narrowing::Postings(pt), Narrowing::Blooms(bl)) = (&postings, &blooms) else {
            panic!("unexpected narrowing tiers");
        };

        // Queries drawn from real file trigrams (these MUST be found by both),
        // plus a few that are unlikely to exist anywhere.
        for f in &per_file {
            if f.len() < 3 {
                continue;
            }
            let q = vec![f[0], f[f.len() / 2], f[f.len() - 1]];
            let exact = SearchIndex::postings_intersect(pt, &q);
            let bloom: HashSet<u32> = SearchIndex::bloom_scan(bl, &q).into_iter().collect();
            for id in exact {
                assert!(
                    bloom.contains(&id),
                    "Bloom tier dropped a true match (false negative) for {q:?}"
                );
            }
        }
    }

    /// End-to-end: an index forced onto the Bloom tier must still surface every
    /// file that actually contains the literal (recall parity with a full scan).
    #[test]
    fn bloom_tier_end_to_end_recall() {
        let samples = [
            (
                "auth_login.rs",
                "fn authenticate(user) {}\nlet token = mint();\n",
            ),
            (
                "auth_session.rs",
                "struct Session;\n// authenticate again here\n",
            ),
            ("db_pool.rs", "fn connect() {}\nlet retries = 3;\n"),
            (
                "ui_button.tsx",
                "export const Button = () => authenticate;\n",
            ),
            ("readme.md", "This project uses authenticate flows.\n"),
            ("unrelated.rs", "fn helper() { let v = 1; }\n"),
        ];
        let files: Vec<PathBuf> = samples.iter().map(|(rel, _)| PathBuf::from(rel)).collect();
        let per_file: Vec<Vec<u32>> = samples.iter().map(|(_, c)| trigrams_of(c)).collect();

        let idx = SearchIndex {
            files,
            narrowing: build_narrowing(&per_file, MAX_POSTING_ENTRIES + 1),
            respect_gitignore: true,
            allow_secret_paths: false,
            built_at: Instant::now(),
        };
        assert!(
            matches!(idx.narrowing, Narrowing::Blooms(_)),
            "test must exercise the Bloom tier"
        );

        for query in ["authenticate", "Session", "retries", "token"] {
            let cands: HashSet<String> = idx
                .candidate_paths(query, &[], std::path::Path::new(""))
                .into_paths()
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            for (rel, content) in samples {
                if content.contains(query) {
                    assert!(
                        cands.contains(rel),
                        "Bloom tier dropped real match {rel} for query {query:?}"
                    );
                }
            }
        }
    }

    /// A scoped override of `LEAN_CTX_DATA_DIR`, restored on drop, so the
    /// cross-process lock files land in an isolated temp dir during tests.
    struct DataDirGuard {
        prev: Option<String>,
    }
    impl DataDirGuard {
        fn set(path: &std::path::Path) -> Self {
            let prev = std::env::var("LEAN_CTX_DATA_DIR").ok();
            crate::test_env::set_var("LEAN_CTX_DATA_DIR", path);
            Self { prev }
        }
    }
    impl Drop for DataDirGuard {
        fn drop(&mut self) {
            match self.prev.as_deref() {
                Some(v) => crate::test_env::set_var("LEAN_CTX_DATA_DIR", v),
                None => crate::test_env::remove_var("LEAN_CTX_DATA_DIR"),
            }
        }
    }

    #[test]
    fn search_index_lock_name_is_per_repo_and_distinct() {
        let a = search_index_lock_name("/tmp/repo-a");
        let b = search_index_lock_name("/tmp/repo-b");
        assert!(a.starts_with("search-idx-"), "unexpected lock name: {a}");
        assert_ne!(a, b, "lock name must be per-repo");
        assert_eq!(a, search_index_lock_name("/tmp/repo-a"), "stable per repo");
        // Must not collide with the graph/bm25 locks for the same repo, or the
        // three indexers would needlessly serialize against one another.
        let h = &crate::core::index_namespace::namespace_hash(Path::new("/tmp/repo-a"))[..8];
        assert_ne!(
            a,
            format!("graph-idx-{h}"),
            "must not collide with graph lock"
        );
        assert_ne!(
            a,
            format!("bm25-idx-{h}"),
            "must not collide with bm25 lock"
        );
    }

    #[test]
    fn build_guarded_builds_when_uncontended() {
        let _env = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().unwrap();
        let _guard = DataDirGuard::set(data.path());

        let dir = corpus();
        let root = dir.path().to_string_lossy().to_string();
        // Seed the in-flight flag the way `ensure_background` does before spawn.
        {
            let mut map = cache()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            map.insert(
                root.clone(),
                CacheEntry {
                    index: None,
                    building: true,
                },
            );
        }
        assert_eq!(
            build_guarded(&root, true, false),
            BuildOutcome::Built,
            "an uncontended root must build"
        );
        let map = cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = map.get(&root).expect("entry present");
        assert!(!entry.building, "building flag must clear after build");
        assert!(entry.index.is_some(), "index must be installed after build");
    }

    #[test]
    fn build_guarded_defers_when_another_process_holds_the_lock() {
        let _env = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().unwrap();
        let _guard = DataDirGuard::set(data.path());

        let dir = corpus();
        let root = dir.path().to_string_lossy().to_string();
        // Pre-hold the cross-process lock with *this* (alive) PID and a fresh
        // mtime, so neither the dead-owner nor the staleness reclaim can take it
        // — exactly the "another session is already building" state from #460.
        let lock_path = data
            .path()
            .join(format!(".{}.lock", search_index_lock_name(&root)));
        std::fs::write(&lock_path, format!("{}\n", std::process::id())).unwrap();

        {
            let mut map = cache()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            map.insert(
                root.clone(),
                CacheEntry {
                    index: None,
                    building: true,
                },
            );
        }
        assert_eq!(
            build_guarded(&root, true, false),
            BuildOutcome::Deferred,
            "a contended root must defer the proactive pre-warm"
        );
        let map = cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = map.get(&root).expect("entry present");
        assert!(
            !entry.building,
            "deferred build must clear the in-flight flag so a later nudge retries"
        );
        assert!(
            entry.index.is_none(),
            "deferred build must not run a second walk / install an index"
        );
    }
}
