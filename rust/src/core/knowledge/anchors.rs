// SPDX-License-Identifier: Apache-2.0
//! Code anchors of knowledge facts (Hive plan v3 J2; C0 1.9 section 3,
//! `CodeAnchorV1`).
//!
//! An anchor ties a fact to the code it is about: a repo-relative `path`, an
//! optional `symbol` defined in that file, and the `digest` of exactly those
//! bytes when the fact was written. The engine resolves anchors against the
//! checkout on disk, never against a cached index, so a stale graph cannot
//! report an old body as current:
//!
//! - `current`: the symbol (or file) still digests to the anchored bytes;
//! - `changed`: it is still there with other bytes;
//! - `moved`: the same bytes now live under another path (found through the
//!   project's code graph);
//! - `missing`: neither the symbol nor its bytes are found.
//!
//! A fact with any anchor that is not `current` is *to verify*: it is still
//! recalled, marked, never presented as current knowledge.
//!
//! Digest (normalisation fixed by the contract's `anchor_digest` vectors):
//! the UTF-8 bytes of the definition node (tree-sitter node bytes, without
//! the doc comments or attributes before it) or of the whole file, with a
//! leading BOM dropped, `\r\n` and `\r` as `\n`, and trailing spaces and tabs
//! of every line removed; `sha256:` + lowercase hex.
//!
//! Sealed-safe: anchors are part of the fact. A sealed repo's facts travel
//! only inside the encrypted content, so their paths and symbol names do too;
//! only the state (`to_verify`) is ever plain metadata.

use std::path::{Component, Path};

use unicode_normalization::UnicodeNormalization as _;

/// The contract's anchor (C0 1.9 `CodeAnchorV1`). The engine's symbol names
/// are `name`, or `name#n` for the n-th definition of that name in a file,
/// and slug paths for Markdown sections ([`markdown_sections`]).
pub use lean_ctx_protocol::hive::anchor::{CodeAnchorV1 as CodeAnchor, MAX_ANCHORS};

const MAX_LANG_BYTES: usize = 32;
/// Files above this size are not anchored (or resolved): `missing`.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Where an anchor stands in a checkout now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorState {
    Current,
    Changed,
    Moved { path: String },
    Missing,
}

impl AnchorState {
    /// The contract's reason for a fact to verify (`None` when current).
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Self::Current => None,
            Self::Changed => Some("anchor_changed"),
            Self::Moved { .. } => Some("anchor_moved"),
            Self::Missing => Some("anchor_missing"),
        }
    }
}

/// The contract's limits on a fact's anchors (`validate_anchors`: at most
/// [`MAX_ANCHORS`], each valid, sorted by path and symbol with the whole-file
/// anchor first, no repeats), plus the engine's own: every path in NFC.
pub fn validate_all(anchors: &[CodeAnchor]) -> Result<(), String> {
    lean_ctx_protocol::hive::anchor::validate_anchors(anchors).map_err(|e| e.to_string())?;
    anchors
        .iter()
        .try_for_each(|anchor| check_path(&anchor.path))
}

/// Sort and de-duplicate anchors into their one encoding.
pub fn canonical(mut anchors: Vec<CodeAnchor>) -> Vec<CodeAnchor> {
    anchors.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
    anchors.dedup_by(|a, b| sort_key(a) == sort_key(b));
    anchors
}

fn sort_key(anchor: &CodeAnchor) -> (&str, Option<&str>) {
    (anchor.path.as_str(), anchor.symbol.as_deref())
}

#[cfg(test)]
fn is_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn check_path(path: &str) -> Result<(), String> {
    let malformed = || Err(format!("anchor path {path:?} is malformed"));
    if path.is_empty()
        || path.len() > lean_ctx_protocol::hive::anchor::MAX_ANCHOR_PATH_BYTES
        || path.starts_with('/')
        || path.starts_with("./")
        || path.ends_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path.nfc().collect::<String>() != path
    {
        return malformed();
    }
    if path
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return malformed();
    }
    Ok(())
}

/// A path as an anchor names it: relative to `root`, `/`-separated, NFC.
/// `None` for a path outside `root` or with `..` components.
pub fn anchor_path(root: &Path, path: &str) -> Option<String> {
    let candidate = Path::new(path);
    let relative = if candidate.is_absolute() {
        candidate.strip_prefix(root).ok()?
    } else {
        candidate
    };
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.nfc().collect::<String>()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    let joined = parts.join("/");
    check_path(&joined).ok()?;
    Some(joined)
}

/// `sha256:` + hex of the normalised bytes (C0 1.9 section 3: BOM dropped,
/// CRLF/CR to LF, trailing spaces and tabs removed per line).
pub use lean_ctx_protocol::hive::anchor::anchor_digest as digest;

fn extension(path: &str) -> &str {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
}

/// The file at the anchor path, read inside `root` only: no symlink may lead
/// out of the checkout, and oversized or unreadable files are `None`.
fn read_inside(root: &Path, path: &str) -> Option<Vec<u8>> {
    check_path(path).ok()?;
    let root = root.canonicalize().ok()?;
    let full = root.join(path).canonicalize().ok()?;
    if !full.starts_with(&root) {
        return None;
    }
    let meta = std::fs::metadata(&full).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    std::fs::read(&full).ok()
}

/// The slug of a Markdown heading text: lowercase, letters and digits kept,
/// runs of anything else as one `-`, none at the edges ("Release & Steps"
/// → "release-steps"). Imported facts (K1) key sections by the same slugs.
pub fn heading_slug(text: &str) -> String {
    let mut slug = String::new();
    for c in text.trim().chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

/// The ATX sections of a Markdown file as (slug path, byte range): a
/// section runs from its heading to the next heading of the same or a
/// higher level (or the end). The path joins the slugs of its parent
/// headings with `/` ("release/steps"); headings inside fenced code are not
/// headings. Repeated paths get `#n` from the second on.
pub fn markdown_sections(content: &str) -> Vec<(String, std::ops::Range<usize>)> {
    let mut headings: Vec<(usize, usize, String)> = Vec::new(); // (level, start, slug)
    let mut fence: Option<&str> = None;
    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = Some(&trimmed[..3]);
            continue;
        }
        if line.len() - trimmed.len() > 3 {
            continue; // indented code
        }
        let level = trimmed.bytes().take_while(|b| *b == b'#').count();
        let rest = &trimmed[level..];
        if (1..=6).contains(&level) && (rest.starts_with(' ') || rest.trim().is_empty()) {
            let text = rest.trim().trim_end_matches('#').trim();
            headings.push((level, start, heading_slug(text)));
        }
    }
    let mut sections = Vec::with_capacity(headings.len());
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut seen = std::collections::HashMap::<String, usize>::new();
    for (i, (level, start, slug)) in headings.iter().enumerate() {
        while stack.last().is_some_and(|(l, _)| l >= level) {
            stack.pop();
        }
        stack.push((*level, slug.clone()));
        let end = headings[i + 1..]
            .iter()
            .find(|(l, _, _)| l <= level)
            .map_or(content.len(), |(_, s, _)| *s);
        let path = stack
            .iter()
            .map(|(_, s)| s.as_str())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("/");
        if path.is_empty() {
            continue;
        }
        let n = seen.entry(path.clone()).or_insert(0);
        *n += 1;
        let symbol = if *n == 1 { path } else { format!("{path}#{n}") };
        sections.push((symbol, *start..end));
    }
    sections
}

/// Each definition in `content` as (symbol, byte range): `name`, and
/// `name#n` from the second definition of the same name on; Markdown files
/// by their sections ([`markdown_sections`]).
#[cfg(feature = "tree-sitter")]
fn symbols(content: &str, ext: &str) -> Vec<(String, std::ops::Range<usize>)> {
    if matches!(ext, "md" | "markdown" | "mdx") {
        return markdown_sections(content);
    }
    let Some(spans) = crate::core::signatures_ts::extract_spans_ts(content, ext) else {
        return Vec::new();
    };
    let mut seen = std::collections::HashMap::<String, usize>::new();
    spans
        .into_iter()
        .map(|(sig, range)| {
            let n = seen.entry(sig.name.clone()).or_insert(0);
            *n += 1;
            let symbol = if *n == 1 {
                sig.name
            } else {
                format!("{}#{n}", sig.name)
            };
            (symbol, range)
        })
        .collect()
}

#[cfg(not(feature = "tree-sitter"))]
fn symbols(content: &str, ext: &str) -> Vec<(String, std::ops::Range<usize>)> {
    if matches!(ext, "md" | "markdown" | "mdx") {
        return markdown_sections(content);
    }
    Vec::new()
}

/// The anchored bytes of `symbol` (or of the whole file) in `content`.
pub(crate) fn anchored<'a>(
    content: &'a [u8],
    path: &str,
    symbol: Option<&str>,
) -> Option<&'a [u8]> {
    let Some(symbol) = symbol else {
        return Some(content);
    };
    let text = std::str::from_utf8(content).ok()?;
    symbols(text, extension(path))
        .into_iter()
        .find(|(name, _)| name == symbol)
        .map(|(_, range)| &content[range])
}

/// Anchor `path` (and `symbol` in it) as it is in `root` now.
pub fn capture(root: &Path, path: &str, symbol: Option<&str>) -> Result<CodeAnchor, String> {
    let path =
        anchor_path(root, path).ok_or_else(|| format!("{path}: not a path inside the project"))?;
    let content =
        read_inside(root, &path).ok_or_else(|| format!("{path}: not readable in the project"))?;
    let bytes = anchored(&content, &path, symbol)
        .ok_or_else(|| format!("{path}: no definition named {}", symbol.unwrap_or_default()))?;
    let ext = extension(&path);
    let anchor = CodeAnchor {
        path: path.clone(),
        symbol: symbol.map(str::to_owned),
        digest: digest(bytes),
        lang: (!ext.is_empty() && ext.len() <= MAX_LANG_BYTES)
            .then(|| ext.to_ascii_lowercase())
            .filter(|l| {
                l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            }),
    };
    validate_all(std::slice::from_ref(&anchor))?;
    Ok(anchor)
}

/// Parse `path` or `path::symbol` as a writer names an anchor.
pub fn parse_spec(spec: &str) -> (&str, Option<&str>) {
    match spec.split_once("::") {
        Some((path, symbol)) if !symbol.is_empty() => (path, Some(symbol)),
        _ => (spec, None),
    }
}

/// Where `anchor` stands in `root` now.
pub fn resolve(root: &Path, anchor: &CodeAnchor) -> AnchorState {
    if let Some(content) = read_inside(root, &anchor.path)
        && let Some(bytes) = anchored(&content, &anchor.path, anchor.symbol.as_deref())
    {
        return if digest(bytes) == anchor.digest {
            AnchorState::Current
        } else {
            AnchorState::Changed
        };
    }
    match moved_to(root, anchor) {
        Some(path) => AnchorState::Moved { path },
        None => AnchorState::Missing,
    }
}

/// Another file of the project that holds the anchored bytes: for a symbol,
/// a definition of the same name in the code graph; for a whole file, none
/// (a moved file without a symbol is `missing`).
fn moved_to(root: &Path, anchor: &CodeAnchor) -> Option<String> {
    let symbol = anchor.symbol.as_deref()?;
    let name = symbol.split_once('#').map_or(symbol, |(name, _)| name);
    let index = crate::core::graph_index::ProjectIndex::load(root.to_str()?)?;
    let mut files: Vec<&str> = index
        .symbols
        .values()
        .filter(|entry| entry.name == name && entry.file != anchor.path)
        .map(|entry| entry.file.as_str())
        .collect();
    files.sort_unstable();
    files.dedup();
    files.into_iter().find_map(|file| {
        let content = read_inside(root, file)?;
        let text = std::str::from_utf8(&content).ok()?;
        symbols(text, extension(file))
            .into_iter()
            .any(|(_, range)| digest(&content[range]) == anchor.digest)
            .then(|| file.to_owned())
    })
}

/// The reasons a fact with `anchors` is to verify in `root` (empty: current).
/// Anchors resolved past this many in one pass are skipped (each costs a
/// read and a parse).
pub const MAX_RESOLVED_PER_PASS: usize = 20_000;

/// Resolves anchors that may come from elsewhere (Hive facts other engines
/// wrote) without turning the checkout into an oracle: only files the
/// engine may read for knowledge (source admission), and in a git checkout
/// only tracked ones, are ever read. Any other path is `Missing` without a
/// read, whether it exists or not, so a crafted anchor on `.env` learns
/// neither its content nor its existence. Such an anchor (a moved file's old
/// path is gone, so it is never readable) is only looked for elsewhere through
/// the code graph, among readable files. Reads are memoised for the pass;
/// past [`MAX_RESOLVED_PER_PASS`] an anchor has no state (`None`).
pub struct GuardedResolver<'a> {
    root: &'a Path,
    tracked: Option<std::collections::HashSet<String>>,
    admitted: std::collections::HashMap<String, bool>,
    memo: std::collections::HashMap<(String, Option<String>, String), AnchorState>,
    resolved: usize,
}

impl<'a> GuardedResolver<'a> {
    /// For a report that leaves the machine: tracked and admitted files.
    pub fn for_report(root: &'a Path) -> Self {
        Self {
            root,
            tracked: git_tracked(root),
            admitted: std::collections::HashMap::new(),
            memo: std::collections::HashMap::new(),
            resolved: 0,
        }
    }

    /// For what this engine shows its own agents: admitted files.
    pub fn local(root: &'a Path) -> Self {
        Self {
            tracked: None,
            ..Self::for_report(root)
        }
    }

    /// Whether `path` may be read for an anchor at all.
    pub fn may_read(&mut self, path: &str) -> bool {
        if self
            .tracked
            .as_ref()
            .is_some_and(|tracked| !tracked.contains(path))
        {
            return false;
        }
        let root = self.root;
        *self.admitted.entry(path.to_owned()).or_insert_with(|| {
            crate::core::archive::bindings::sources_at(
                &root.to_string_lossy(),
                &[path],
                "ctx_knowledge",
            )
            .is_some()
        })
    }

    /// Where `anchor` stands now; `None` only past the per-pass cap.
    pub fn resolve(&mut self, anchor: &CodeAnchor) -> Option<AnchorState> {
        if check_path(&anchor.path).is_err() {
            return Some(AnchorState::Missing);
        }
        let readable = self.may_read(&anchor.path);
        let key = (
            anchor.path.clone(),
            anchor.symbol.clone(),
            anchor.digest.clone(),
        );
        if let Some(state) = self.memo.get(&key) {
            return Some(state.clone());
        }
        if self.resolved >= MAX_RESOLVED_PER_PASS {
            return None;
        }
        self.resolved += 1;
        let found = if readable {
            resolve(self.root, anchor)
        } else {
            moved_to(self.root, anchor)
                .map_or(AnchorState::Missing, |path| AnchorState::Moved { path })
        };
        let state = match found {
            // A move found through the index points at another file: that
            // one must be readable too, or the move tells nothing.
            AnchorState::Moved { path } if !self.may_read(&path) => AnchorState::Missing,
            state => state,
        };
        self.memo.insert(key, state.clone());
        Some(state)
    }
}

/// The checkout's tracked files (`git ls-files`), or `None` outside git.
fn git_tracked(root: &Path) -> Option<std::collections::HashSet<String>> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        out.stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .filter_map(|p| std::str::from_utf8(p).ok().map(str::to_owned))
            .collect(),
    )
}

pub fn review(root: &Path, anchors: &[CodeAnchor]) -> Vec<(CodeAnchor, AnchorState)> {
    anchors
        .iter()
        .map(|anchor| (anchor.clone(), resolve(root, anchor)))
        .filter(|(_, state)| *state != AnchorState::Current)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, content).unwrap();
        }
        dir
    }

    #[test]
    fn a_guarded_resolver_never_reads_an_unadmitted_or_untracked_file() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let dir = project(&[("src/a.md", "kept\n"), (".env", "API_KEY=abc\n")]);
        let root = dir.path();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        git(&["add", "src/a.md"]);
        let tracked = capture(root, "src/a.md", None).unwrap();
        // An anchor someone else wrote, with the right digest of the secret.
        let probe = CodeAnchor {
            path: ".env".into(),
            symbol: None,
            digest: digest(b"API_KEY=abc\n"),
            lang: None,
        };
        let wrong = CodeAnchor {
            digest: digest(b"API_KEY=xyz\n"),
            ..probe.clone()
        };
        let mut guard = GuardedResolver::for_report(root);
        assert_eq!(guard.resolve(&tracked), Some(AnchorState::Current));
        // Right or wrong guess, present or not: the same answer, no read.
        assert_eq!(guard.resolve(&probe), Some(AnchorState::Missing));
        assert_eq!(guard.resolve(&wrong), Some(AnchorState::Missing));
        std::fs::remove_file(root.join(".env")).unwrap();
        assert_eq!(guard.resolve(&probe), Some(AnchorState::Missing));
        // A tracked file that changes is still seen.
        std::fs::write(root.join("src/a.md"), "changed\n").unwrap();
        let mut fresh = GuardedResolver::for_report(root);
        assert_eq!(fresh.resolve(&tracked), Some(AnchorState::Changed));
    }

    #[test]
    fn the_digest_normalises_bom_line_ends_and_trailing_blanks() {
        assert_eq!(
            digest(b"\xEF\xBB\xBFfn a() {}  \r\n}\t\r"),
            digest(b"fn a() {}\n}\n")
        );
        assert_ne!(
            digest(b"  fn a()"),
            digest(b"fn a()"),
            "leading blanks count"
        );
        assert!(is_digest(&digest(b"")));
    }

    #[test]
    fn whole_file_anchors_follow_the_bytes() {
        let dir = project(&[("src/notes.md", "retry three times\n")]);
        let anchor = capture(dir.path(), "src/notes.md", None).unwrap();
        assert_eq!(anchor.path, "src/notes.md");
        assert_eq!(anchor.lang.as_deref(), Some("md"));
        assert_eq!(resolve(dir.path(), &anchor), AnchorState::Current);
        std::fs::write(dir.path().join("src/notes.md"), "retry three times  \r\n").unwrap();
        assert_eq!(
            resolve(dir.path(), &anchor),
            AnchorState::Current,
            "normalised"
        );
        std::fs::write(dir.path().join("src/notes.md"), "retry twice\n").unwrap();
        assert_eq!(resolve(dir.path(), &anchor), AnchorState::Changed);
        std::fs::remove_file(dir.path().join("src/notes.md")).unwrap();
        assert_eq!(resolve(dir.path(), &anchor), AnchorState::Missing);
    }

    /// C0 1.9 section 3: a whole-file anchor captured from disk carries the
    /// contract's reference digest for every vector.
    #[test]
    fn file_anchors_reproduce_the_contract_digest_vectors() {
        let vectors: serde_json::Value = serde_json::from_str(include_str!(
            "../../../crates/lean-ctx-protocol/tests/fixtures/hive/anchor_digest.vectors.json"
        ))
        .unwrap();
        let cases = vectors["cases"].as_array().unwrap();
        assert!(cases.len() >= 7);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let input = hex::decode(case["input_hex"].as_str().unwrap()).unwrap();
            let dir = project(&[]);
            std::fs::create_dir_all(dir.path().join("src")).unwrap();
            std::fs::write(dir.path().join("src/case.txt"), &input).unwrap();
            let anchor = capture(dir.path(), "src/case.txt", None).unwrap();
            assert_eq!(anchor.digest, case["digest"].as_str().unwrap(), "{name}");
            assert_eq!(resolve(dir.path(), &anchor), AnchorState::Current, "{name}");
        }
    }

    #[cfg(feature = "tree-sitter")]
    #[test]
    fn symbol_anchors_cover_only_their_definition() {
        let source = "/// Retries a webhook.\nfn retry() -> u8 {\n    3\n}\n\nfn other() {}\n";
        let dir = project(&[("src/hook.rs", source)]);
        let anchor = capture(dir.path(), "src/hook.rs", Some("retry")).unwrap();
        assert_eq!(anchor.digest, digest(b"fn retry() -> u8 {\n    3\n}"));
        // Edits elsewhere, and to the doc comment, leave it current.
        std::fs::write(
            dir.path().join("src/hook.rs"),
            "/// Retries a webhook, with care.\nfn retry() -> u8 {\n    3\n}\n\nfn other() { let _ = 1; }\n",
        )
        .unwrap();
        assert_eq!(resolve(dir.path(), &anchor), AnchorState::Current);
        std::fs::write(
            dir.path().join("src/hook.rs"),
            "fn retry() -> u8 {\n    5\n}\n",
        )
        .unwrap();
        assert_eq!(resolve(dir.path(), &anchor), AnchorState::Changed);
        assert!(capture(dir.path(), "src/hook.rs", Some("absent")).is_err());
    }

    #[test]
    fn paths_stay_inside_the_project_and_follow_the_contract() {
        let dir = project(&[("a.rs", "fn a() {}\n")]);
        assert!(capture(dir.path(), "../a.rs", None).is_err());
        assert!(capture(dir.path(), "/etc/hosts", None).is_err());
        assert_eq!(anchor_path(dir.path(), "./a.rs").as_deref(), Some("a.rs"));
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("secret"), "x").unwrap();
            std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("link"))
                .unwrap();
            assert!(
                capture(dir.path(), "link", None).is_err(),
                "no symlink out of the checkout"
            );
        }
        for bad in ["", "/a", "./a", "a/", "a//b", "a/../b", "a\\b"] {
            assert!(check_path(bad).is_err(), "{bad:?}");
        }
        let nfd = "cafe\u{301}.md";
        assert!(check_path(nfd).is_err(), "NFC only");
        assert!(check_path(&nfd.nfc().collect::<String>()).is_ok());
    }

    #[test]
    fn a_fact_carries_sorted_unique_valid_anchors() {
        let a = |path: &str, symbol: Option<&str>| CodeAnchor {
            path: path.into(),
            symbol: symbol.map(str::to_owned),
            digest: digest(b"x"),
            lang: None,
        };
        let anchors = canonical(vec![
            a("b.rs", None),
            a("a.rs", Some("f")),
            a("a.rs", None),
            a("a.rs", None),
        ]);
        assert_eq!(
            anchors,
            vec![a("a.rs", None), a("a.rs", Some("f")), a("b.rs", None)]
        );
        assert!(validate_all(&anchors).is_ok());
        assert!(validate_all(&[a("b.rs", None), a("a.rs", None)]).is_err());
        assert!(validate_all(&vec![a("a.rs", None); 2]).is_err());
        assert!(
            validate_all(
                &(0..17)
                    .map(|i| a(&format!("f{i:02}.rs"), None))
                    .collect::<Vec<_>>()
            )
            .is_err()
        );
        let mut bad = a("a.rs", Some(" f"));
        assert!(bad.validate().is_err());
        bad.symbol = None;
        bad.lang = Some("Rust".into());
        assert!(bad.validate().is_err());
        assert_eq!(parse_spec("src/a.rs::retry"), ("src/a.rs", Some("retry")));
        assert_eq!(parse_spec("src/a.rs"), ("src/a.rs", None));
    }

    #[test]
    fn markdown_sections_nest_by_level_and_skip_fenced_code() {
        let doc = "# Release\nintro\n## Steps\n1. tag\n```sh\n# not a heading\n```\n## Steps\nagain\n# Notes & More\nend\n";
        let sections = markdown_sections(doc);
        let names: Vec<&str> = sections.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["release", "release/steps", "release/steps#2", "notes-more"]
        );
        let span = |name: &str| {
            let (_, range) = sections.iter().find(|(n, _)| n == name).unwrap();
            &doc[range.clone()]
        };
        assert_eq!(
            span("release/steps"),
            "## Steps\n1. tag\n```sh\n# not a heading\n```\n"
        );
        assert_eq!(span("notes-more"), "# Notes & More\nend\n");
        assert!(span("release").starts_with("# Release\n") && span("release").ends_with("again\n"));
        assert_eq!(heading_slug("  Release & Steps!  "), "release-steps");
        assert_eq!(heading_slug("Grüße 2"), "grüße-2");
    }

    #[test]
    fn a_markdown_section_anchor_ignores_edits_elsewhere() {
        let dir = project(&[(
            "docs/release.md",
            "# Release\n## Steps\n1. tag\n## Notes\nx\n",
        )]);
        let anchor = capture(dir.path(), "docs/release.md", Some("release/steps")).unwrap();
        std::fs::write(
            dir.path().join("docs/release.md"),
            "# Release\n## Steps\n1. tag\n## Notes\ny\n",
        )
        .unwrap();
        assert_eq!(resolve(dir.path(), &anchor), AnchorState::Current);
        std::fs::write(
            dir.path().join("docs/release.md"),
            "# Release\n## Notes\ny\n",
        )
        .unwrap();
        assert_eq!(resolve(dir.path(), &anchor), AnchorState::Missing);
    }
}
