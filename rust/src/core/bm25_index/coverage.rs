// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx index why <file>`: explain whether a file is in the search corpus
//! and, if not, which rule dropped it.
//!
//! An incomplete index is indistinguishable from an empty result — `ctx_compose`
//! just says "no match" — so every stage that can drop a file is replayed here
//! with the indexer's own rules ([`CorpusRules`], the same walk, the same
//! size/binary/minified checks) in the order the indexer applies them. The
//! first stage that rejects the file is the answer.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use super::{
    BM25Index, CORPUS_MAX_DEPTH, CorpusRules, IndexedFileState, build::MAX_FILE_SIZE_BYTES,
    list_code_files, looks_minified,
};
use crate::core::ingestion::IngestKind;

/// Why a file is not part of the corpus. Variants are ordered like the
/// indexer's stages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub(crate) enum Exclusion {
    NotFound,
    NotAFile,
    OutsideRoot,
    /// A directory on the path is pruned by the walk (dependency/vendor
    /// directory, agent worktree copy) or the file is a cloud placeholder.
    PrunedDirectory {
        path: String,
        kind: &'static str,
    },
    /// Ignored by `.gitignore`, `.ignore`, `.git/info/exclude` or the global
    /// gitignore.
    Gitignored,
    TooDeep {
        depth: usize,
        limit: usize,
    },
    /// Symlinked directories are not followed by the corpus walk.
    NotWalked,
    Binary,
    Lockfile,
    IgnorePattern {
        pattern: String,
    },
    IndexFilter {
        summary: String,
    },
    FileCap {
        cap: usize,
    },
    TooLarge {
        size_bytes: u64,
        limit_bytes: u64,
    },
    BinaryContent,
    NoExtractableText,
    Unreadable {
        error: String,
    },
    Minified,
    /// The context gateway does not admit the file into derived stores
    /// (restricted or withheld, G5); it is deliberately absent.
    WithheldByGateway,
}

impl Exclusion {
    /// One line: what dropped the file.
    #[must_use]
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::NotFound => "file does not exist".into(),
            Self::NotAFile => "not a regular file".into(),
            Self::OutsideRoot => "outside the project root".into(),
            Self::PrunedDirectory { path, kind } => format!("inside {path}/ — {kind}"),
            Self::Gitignored => {
                "ignored by .gitignore / .ignore / .git/info/exclude / global gitignore".into()
            }
            Self::TooDeep { depth, limit } => {
                format!("nested {depth} levels deep; the corpus walk stops at {limit}")
            }
            Self::NotWalked => {
                "not reachable by the corpus walk (symlinked directories are not followed)".into()
            }
            Self::Binary => "binary file type".into(),
            Self::Lockfile => "dependency lockfile (generated content)".into(),
            Self::IgnorePattern { pattern } => format!("matches ignore pattern `{pattern}`"),
            Self::IndexFilter { summary } => format!("excluded by the [index] filter ({summary})"),
            Self::FileCap { cap } => {
                format!("file cap reached: the corpus is limited to {cap} files")
            }
            Self::TooLarge {
                size_bytes,
                limit_bytes,
            } => format!(
                "{} exceeds the per-file limit of {}",
                human_bytes(*size_bytes),
                human_bytes(*limit_bytes)
            ),
            Self::BinaryContent => "content is binary (NUL bytes in the first 8 KiB)".into(),
            Self::NoExtractableText => "document yielded no extractable text".into(),
            Self::Unreadable { error } => format!("cannot be read: {error}"),
            Self::Minified => "looks minified/bundled (very long lines)".into(),
            Self::WithheldByGateway => {
                "withheld by the context gateway (restricted or not admissible); \
                 never stored in an index"
                    .into()
            }
        }
    }

    /// How to get the file indexed, when the user can change it.
    #[must_use]
    pub(crate) fn remedy(&self) -> Option<String> {
        let remedy = match self {
            Self::OutsideRoot => "pass --root <project> to diagnose another project",
            Self::Gitignored => {
                "lean-ctx index build --no-gitignore, or [index] respect_gitignore = false"
            }
            Self::IgnorePattern { .. } => {
                "built-in patterns are fixed; for extra_ignore_patterns edit the config"
            }
            Self::IndexFilter { .. } => {
                "adjust [index] include/exclude or the --include/--exclude flags"
            }
            Self::FileCap { .. } => "set bm25_max_files in config (0 = unlimited)",
            Self::PrunedDirectory { .. }
            | Self::NotWalked
            | Self::NotFound
            | Self::NotAFile
            | Self::TooDeep { .. }
            | Self::Binary
            | Self::Lockfile
            | Self::TooLarge { .. }
            | Self::BinaryContent
            | Self::NoExtractableText
            | Self::Unreadable { .. }
            | Self::Minified => return None,
            Self::WithheldByGateway => {
                "lean-ctx inspect explains the decision; ctx_read shows what may be read"
            }
        };
        Some(remedy.to_string())
    }
}

/// What the persisted BM25 index holds for an eligible file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum IndexState {
    /// No BM25 index has been built (or it could not be loaded).
    NoIndex,
    /// Eligible but absent from the index — built before the file existed, or
    /// by an older version that could not read its encoding.
    Missing,
    /// Indexed; `stale` when the file changed since.
    Indexed { chunks: usize, stale: bool },
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct FileCoverage {
    /// Path relative to the project root (as the index keys it), or the input
    /// when it could not be resolved.
    pub path: String,
    pub kind: Option<&'static str>,
    pub encoding: Option<&'static str>,
    pub size_bytes: Option<u64>,
    pub exclusion: Option<Exclusion>,
    /// Only set when the file is eligible.
    pub index: Option<IndexState>,
}

impl FileCoverage {
    fn excluded(path: String, exclusion: Exclusion) -> Self {
        Self {
            path,
            kind: None,
            encoding: None,
            size_bytes: None,
            exclusion: Some(exclusion),
            index: None,
        }
    }
}

/// Resolve `input` (absolute, relative to `root`, or relative to the cwd —
/// in that order, since index paths are root-relative) and explain its corpus
/// membership.
#[must_use]
pub(crate) fn explain(root: &Path, input: &str) -> FileCoverage {
    let candidate = Path::new(input);
    let abs = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else if root.join(candidate).exists() {
        root.join(candidate)
    } else {
        std::env::current_dir().map_or_else(|_| root.join(candidate), |cwd| cwd.join(candidate))
    };
    let Ok(abs) = abs.canonicalize() else {
        return FileCoverage::excluded(input.to_string(), Exclusion::NotFound);
    };
    let Some(rel) = root
        .canonicalize()
        .ok()
        .and_then(|r| abs.strip_prefix(r).ok().map(Path::to_path_buf))
    else {
        return FileCoverage::excluded(input.to_string(), Exclusion::OutsideRoot);
    };
    let rel = rel.to_string_lossy().to_string();
    if !abs.is_file() {
        return FileCoverage::excluded(rel, Exclusion::NotAFile);
    }
    explain_rel(root, &rel)
}

/// Explain an existing file given as `rel` (platform separators) under `root`.
fn explain_rel(root: &Path, rel: &str) -> FileCoverage {
    let rules = CorpusRules::load();
    // Walk paths are `root` + components, uncanonicalized — the same spelling
    // the corpus walk produces.
    let target = root.join(rel);
    let mut out = FileCoverage::excluded(rel.to_string(), Exclusion::NotFound);
    out.exclusion = None;
    out.size_bytes = target.metadata().ok().map(|m| m.len());
    if let Ok(bytes) = read_prefix(&target) {
        out.encoding = Some(if crate::core::text_decode::looks_binary(&bytes) {
            "binary"
        } else {
            crate::core::text_decode::detect_encoding(&bytes).label()
        });
    }

    if let Some(exclusion) = walk_exclusion(&rules, root, &target, rel) {
        out.exclusion = Some(exclusion);
        return out;
    }

    let kind = crate::core::ingestion::classify_path(&target);
    out.kind = Some(kind.as_str());
    let exclusion = match kind {
        IngestKind::Binary => Some(Exclusion::Binary),
        IngestKind::Generated => Some(Exclusion::Lockfile),
        _ => None,
    }
    .or_else(|| {
        rules
            .ignore_pattern_for(rel)
            .map(|pattern| Exclusion::IgnorePattern {
                pattern: pattern.to_string(),
            })
    })
    .or_else(|| {
        rules.filter_excludes(rel).then(|| Exclusion::IndexFilter {
            summary: rules.filter.summary().unwrap_or_default(),
        })
    })
    .or_else(|| content_exclusion(&target))
    .or_else(|| {
        // Every per-file rule admits it; only the corpus-wide cap is left.
        let corpus = list_code_files(root);
        corpus.binary_search(&rel.to_string()).is_err().then_some(
            if corpus.len() >= rules.max_files {
                Exclusion::FileCap {
                    cap: rules.max_files,
                }
            } else {
                Exclusion::NotWalked
            },
        )
    });
    if exclusion.is_some() {
        out.exclusion = exclusion;
        return out;
    }

    out.index = Some(index_state(root, rel, &target));
    out
}

/// Replays the corpus walk restricted to the target's ancestor chain, so the
/// cost is proportional to the path depth, not to the repository. A second,
/// unfiltered walk tells gitignore apart from pruning and depth.
fn walk_exclusion(rules: &CorpusRules, root: &Path, target: &Path, rel: &str) -> Option<Exclusion> {
    let on_path = {
        let target = target.to_path_buf();
        move |entry: &ignore::DirEntry| target.starts_with(entry.path())
    };

    let corpus_walk = {
        let on_path = on_path.clone();
        rules
            .walker(root)
            .filter_entry(move |e| on_path(e) && crate::core::walk_filter::keep_entry(e))
            .build()
    };
    if reaches(corpus_walk, target) {
        return None;
    }

    let pruned: Arc<Mutex<Option<(PathBuf, &'static str)>>> = Arc::new(Mutex::new(None));
    let unfiltered_walk = {
        let pruned = Arc::clone(&pruned);
        ignore::WalkBuilder::new(root)
            .standard_filters(false)
            .filter_entry(move |e| {
                if !on_path(e) {
                    return false;
                }
                let kind = prune_kind(e);
                if let Some(kind) = kind
                    && let Ok(mut slot) = pruned.lock()
                {
                    slot.get_or_insert_with(|| (e.path().to_path_buf(), kind));
                }
                kind.is_none()
            })
            .build()
    };
    let reachable = reaches(unfiltered_walk, target);
    let pruned = pruned.lock().ok().and_then(|mut slot| slot.take());
    if let Some((dir, kind)) = pruned {
        let shown = dir
            .strip_prefix(root)
            .unwrap_or(&dir)
            .to_string_lossy()
            .to_string();
        return Some(Exclusion::PrunedDirectory { path: shown, kind });
    }
    if !reachable {
        return Some(Exclusion::NotWalked);
    }
    let depth = Path::new(rel).components().count();
    if depth > CORPUS_MAX_DEPTH {
        return Some(Exclusion::TooDeep {
            depth,
            limit: CORPUS_MAX_DEPTH,
        });
    }
    Some(Exclusion::Gitignored)
}

fn reaches(walk: ignore::Walk, target: &Path) -> bool {
    walk.flatten().any(|e| e.path() == target)
}

/// Mirrors [`crate::core::walk_filter::keep_entry`], naming the rule.
fn prune_kind(entry: &ignore::DirEntry) -> Option<&'static str> {
    if crate::core::walk_filter::is_vendor_dir(entry) {
        Some("dependency/vendor directory, never indexed")
    } else if crate::core::walk_filter::is_agent_worktree_dir(entry) {
        Some("agent worktree copy, never indexed")
    } else if !crate::core::cloud_files::keep_entry(entry) {
        Some("cloud placeholder that is not downloaded")
    } else {
        None
    }
}

/// The build's per-file content checks, in `prepare_file` order.
fn content_exclusion(target: &Path) -> Option<Exclusion> {
    let size_bytes = target.metadata().map(|m| m.len()).unwrap_or(0);
    if size_bytes > MAX_FILE_SIZE_BYTES {
        return Some(Exclusion::TooLarge {
            size_bytes,
            limit_bytes: MAX_FILE_SIZE_BYTES,
        });
    }
    let content = if crate::core::extractors::is_binary_document(target) {
        match std::fs::read(target) {
            Ok(bytes) => {
                let text = crate::core::extractors::extract(target, &bytes).text;
                if text.is_empty() {
                    return Some(Exclusion::NoExtractableText);
                }
                text
            }
            Err(e) => {
                return Some(Exclusion::Unreadable {
                    error: e.to_string(),
                });
            }
        }
    } else {
        match crate::core::text_decode::read_text(target) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                return Some(Exclusion::BinaryContent);
            }
            Err(e) => {
                return Some(Exclusion::Unreadable {
                    error: e.to_string(),
                });
            }
        }
    };
    if looks_minified(&content) {
        return Some(Exclusion::Minified);
    }
    // Same store admission the build applies (G5): a file the gateway keeps
    // out of every index must be explained as such, not reported as missing.
    crate::core::context_admission::stores::StoreAdmission::current()
        .admit(&content, target)
        .is_none()
        .then_some(Exclusion::WithheldByGateway)
}

fn index_state(root: &Path, rel: &str, target: &Path) -> IndexState {
    let Some(index) = BM25Index::load(root) else {
        return IndexState::NoIndex;
    };
    let Some(stored) = index.files.get(rel) else {
        return IndexState::Missing;
    };
    IndexState::Indexed {
        chunks: index.chunks.iter().filter(|c| c.file_path == rel).count(),
        stale: IndexedFileState::from_path(target).as_ref() != Some(stored),
    }
}

/// Human-readable verdict (CLI `index why` and `ctx_index action=why`): the
/// verdict first, then the remedy. Deterministic for a given file state.
#[must_use]
pub(crate) fn render(cov: &FileCoverage) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(out, "  File:      {}", cov.path);
    if let Some(kind) = cov.kind {
        let _ = writeln!(out, "  Kind:      {kind}");
    }
    if let Some(encoding) = cov.encoding {
        let _ = writeln!(out, "  Encoding:  {encoding}");
    }
    if let Some(exclusion) = &cov.exclusion {
        let _ = writeln!(out, "  Corpus:    not indexed — {}", exclusion.describe());
        if let Some(remedy) = exclusion.remedy() {
            let _ = writeln!(out, "  Fix:       {remedy}");
        }
        return out;
    }
    let _ = writeln!(
        out,
        "  Corpus:    eligible (BM25 and the semantic index chunk this corpus)"
    );
    let bm25 = match &cov.index {
        Some(IndexState::Indexed {
            chunks,
            stale: false,
        }) => {
            format!("indexed, {chunks} chunks, up to date")
        }
        Some(IndexState::Indexed {
            chunks,
            stale: true,
        }) => format!(
            "indexed, {chunks} chunks, changed since the last build — run: lean-ctx index build"
        ),
        Some(IndexState::Missing) => "missing from the current index (built before this file, \
             or by an older version that could not read its encoding) — run: lean-ctx index build"
            .to_string(),
        Some(IndexState::NoIndex) | None => {
            "no index built yet — run: lean-ctx index build".to_string()
        }
    };
    let _ = writeln!(out, "  BM25:      {bm25}");
    out
}

fn read_prefix(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::with_capacity(8192);
    std::fs::File::open(path)?
        .take(64 * 1024)
        .read_to_end(&mut buf)?;
    Ok(buf)
}

fn human_bytes(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{:.0} KiB", (bytes as f64 / 1024.0).ceil())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A git project plus an isolated data dir, so the user's config (extra
    /// ignore patterns, file cap, `[index]` filter) and indexes stay out.
    struct Project {
        dir: tempfile::TempDir,
        _data: tempfile::TempDir,
        _env: crate::core::data_dir::TestEnvGuard,
    }

    impl Project {
        fn path(&self) -> &Path {
            self.dir.path()
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        }
    }

    fn project() -> Project {
        let env = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().expect("data dir");
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".git")).expect("git dir");
        Project {
            dir,
            _data: data,
            _env: env,
        }
    }

    fn write(root: &Path, rel: &str, bytes: &[u8]) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, bytes).expect("write");
    }

    fn reason(root: &Path, rel: &str) -> Option<Exclusion> {
        explain(root, rel).exclusion
    }

    #[test]
    fn eligible_windows_1252_file_is_admitted_and_named() {
        let dir = project();
        write(
            dir.path(),
            "src/Legacy.cs",
            b"// Gr\xF6\xDFe\r\nclass Legacy {}\r\n",
        );
        let cov = explain(dir.path(), "src/Legacy.cs");
        assert_eq!(cov.exclusion, None, "{cov:?}");
        assert_eq!(cov.kind, Some("code"));
        assert_eq!(cov.encoding, Some("windows-1252 (legacy ANSI)"));
        assert!(matches!(
            cov.index,
            Some(IndexState::NoIndex | IndexState::Missing)
        ));
    }

    #[test]
    fn missing_and_outside_root_are_reported() {
        let dir = project();
        assert_eq!(reason(dir.path(), "nope.rs"), Some(Exclusion::NotFound));
        let other = tempfile::tempdir().expect("tempdir");
        write(other.path(), "x.rs", b"fn x() {}");
        let outside = other.path().join("x.rs");
        assert_eq!(
            reason(dir.path(), outside.to_str().expect("utf8 path")),
            Some(Exclusion::OutsideRoot)
        );
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        assert_eq!(reason(dir.path(), "src"), Some(Exclusion::NotAFile));
    }

    #[test]
    fn gitignored_file_is_reported() {
        let dir = project();
        write(dir.path(), ".gitignore", b"generated/\n");
        write(dir.path(), "generated/api.rs", b"fn api() {}");
        assert_eq!(
            reason(dir.path(), "generated/api.rs"),
            Some(Exclusion::Gitignored)
        );
    }

    #[test]
    fn vendor_directory_is_named() {
        let dir = project();
        write(
            dir.path(),
            "node_modules/lib/index.js",
            b"module.exports = 1;",
        );
        assert_eq!(
            reason(dir.path(), "node_modules/lib/index.js"),
            Some(Exclusion::PrunedDirectory {
                path: "node_modules".into(),
                kind: "dependency/vendor directory, never indexed",
            })
        );
    }

    #[test]
    fn default_ignore_pattern_is_named() {
        let dir = project();
        write(dir.path(), "app.min.js", b"var a=1;");
        assert_eq!(
            reason(dir.path(), "app.min.js"),
            Some(Exclusion::IgnorePattern {
                pattern: "*.min.js".into()
            })
        );
    }

    #[test]
    fn binary_types_lockfiles_and_binary_content_are_distinguished() {
        let dir = project();
        write(dir.path(), "logo.png", b"\x89PNG\r\n\x1a\n");
        write(dir.path(), "Cargo.lock", b"# lock\n");
        write(dir.path(), "data.cs", b"class A {}\0\0\0");
        assert_eq!(reason(dir.path(), "logo.png"), Some(Exclusion::Binary));
        assert_eq!(reason(dir.path(), "Cargo.lock"), Some(Exclusion::Lockfile));
        assert_eq!(
            reason(dir.path(), "data.cs"),
            Some(Exclusion::BinaryContent)
        );
    }

    #[test]
    fn utf16_file_is_eligible() {
        let dir = project();
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "class Wide {}\r\n".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        write(dir.path(), "Wide.cs", &bytes);
        let cov = explain(dir.path(), "Wide.cs");
        assert_eq!(cov.exclusion, None, "{cov:?}");
        assert_eq!(cov.encoding, Some("utf-16le (BOM)"));
    }

    #[test]
    fn oversized_and_minified_files_are_reported() {
        let dir = project();
        let big = vec![b'a'; usize::try_from(MAX_FILE_SIZE_BYTES).expect("fits") + 1];
        write(dir.path(), "huge.txt", &big);
        assert!(matches!(
            reason(dir.path(), "huge.txt"),
            Some(Exclusion::TooLarge { .. })
        ));
        let minified = format!("var x={};\n", "1+".repeat(40_000));
        write(dir.path(), "bundle.js", minified.as_bytes());
        assert_eq!(reason(dir.path(), "bundle.js"), Some(Exclusion::Minified));
    }

    /// G5: a file the gateway keeps out of every index is explained as such —
    /// not reported as indexable and then silently missing.
    #[test]
    fn a_file_withheld_by_the_gateway_is_explained() {
        let dir = project();
        write(
            dir.path(),
            "plans.md",
            b"*** TOP SECRET ***\nlaunch window\n",
        );
        write(dir.path(), "notes.md", b"plain release notes\n");
        assert_eq!(
            reason(dir.path(), "plans.md"),
            Some(Exclusion::WithheldByGateway)
        );
        assert_eq!(reason(dir.path(), "notes.md"), None);
        assert!(Exclusion::WithheldByGateway.remedy().is_some());
    }

    #[test]
    fn built_index_reports_chunks_and_staleness() {
        let dir = project();
        write(
            dir.path(),
            "src/lib.rs",
            b"pub fn alpha() {}\n\npub fn beta() {}\n",
        );
        let index = BM25Index::build_from_directory(dir.path());
        index.save(dir.path()).expect("save index");
        match explain(dir.path(), "src/lib.rs").index {
            Some(IndexState::Indexed { chunks, stale }) => {
                assert!(chunks > 0);
                assert!(!stale);
            }
            other => panic!("expected indexed, got {other:?}"),
        }
        write(dir.path(), "src/new.rs", b"pub fn gamma() {}\n");
        assert_eq!(
            explain(dir.path(), "src/new.rs").index,
            Some(IndexState::Missing)
        );
    }

    /// Regression for the Windows report: ANSI and UTF-16 files used to be
    /// dropped by `read_to_string` and were unsearchable.
    #[test]
    fn legacy_encoded_files_are_indexed_and_searchable() {
        let dir = project();
        write(
            dir.path(),
            "src/Kuendigung.cs",
            b"class Kuendigung\r\n{\r\n    // K\xFCndigungsfrist berechnen\r\n    int Tage() { return 30; }\r\n}\r\n",
        );
        let mut utf16 = vec![0xFF, 0xFE];
        for unit in "class Storno\r\n{\r\n    // Stornogeb\u{fc}hr\r\n    int Betrag() { return 5; }\r\n}\r\n".encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        write(dir.path(), "src/Storno.cs", &utf16);

        let index = BM25Index::build_from_directory(dir.path());
        index.save(dir.path()).expect("save index");

        for (file, query) in [
            ("src/Kuendigung.cs", "Kündigungsfrist"),
            ("src/Storno.cs", "Stornogebühr"),
        ] {
            assert!(
                matches!(
                    explain(dir.path(), file).index,
                    Some(IndexState::Indexed { chunks, .. }) if chunks > 0
                ),
                "{file} must be indexed"
            );
            let hits = index.search(query, 5);
            assert!(
                hits.iter().any(|h| h.file_path.replace('\\', "/") == file),
                "{query} must find {file}, got {hits:?}"
            );
        }
    }

    #[test]
    fn render_leads_with_the_verdict_and_remedy() {
        let p = project();
        write(p.path(), ".gitignore", b"out/\n");
        write(p.path(), "out/gen.rs", b"fn g() {}");
        let text = render(&explain(p.path(), "out/gen.rs"));
        assert!(
            text.contains("Corpus:    not indexed — ignored by .gitignore"),
            "{text}"
        );
        assert!(
            text.contains("Fix:       lean-ctx index build --no-gitignore"),
            "{text}"
        );

        write(p.path(), "src/ok.rs", b"fn ok() {}");
        let text = render(&explain(p.path(), "src/ok.rs"));
        assert!(text.contains("Corpus:    eligible"), "{text}");
        assert!(text.contains("BM25:      no index built yet"), "{text}");
    }

    #[test]
    fn every_exclusion_describes_itself() {
        let all = [
            Exclusion::NotFound,
            Exclusion::Gitignored,
            Exclusion::FileCap { cap: 5000 },
            Exclusion::TooLarge {
                size_bytes: 3 * 1024 * 1024,
                limit_bytes: MAX_FILE_SIZE_BYTES,
            },
        ];
        for e in all {
            assert!(!e.describe().is_empty());
        }
        assert_eq!(
            Exclusion::TooLarge {
                size_bytes: 3 * 1024 * 1024,
                limit_bytes: MAX_FILE_SIZE_BYTES
            }
            .describe(),
            "3.0 MiB exceeds the per-file limit of 2.0 MiB"
        );
        assert!(Exclusion::FileCap { cap: 1 }.remedy().is_some());
        assert!(Exclusion::Binary.remedy().is_none());
    }
}
