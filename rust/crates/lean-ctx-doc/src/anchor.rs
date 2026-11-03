// SPDX-License-Identifier: Apache-2.0

//! Code anchors: what a page derives from, pinned at a content digest.
//!
//! An anchor names a file, a directory or a glob (`file:`, `dir:`, `glob:`
//! refs). In `shape` mode a directory anchor pins only its listing: an entry
//! appearing or disappearing moves it, an edit inside a file does not.
//! Digests are sha256 hex over a canonical manifest (C0 2.2 `doc_anchor`):
//! the bytes of a file; `<relpath>\0<file digest>\n` per file in bytewise
//! path order for a directory or glob; `<relpath>\n` for a shape. The file
//! set is what git tracks, so every checkout computes the same value.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// What an anchor covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorKind {
    /// One file.
    File,
    /// A directory with everything under it.
    Dir,
    /// The files a glob matches (`*`, `?`, `**`).
    Glob,
}

/// What a directory or glob anchor hashes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorMode {
    /// The files and their contents.
    #[default]
    Content,
    /// The listing only.
    Shape,
}

/// Whether the page derives from the target or only consulted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorRole {
    /// A change makes the page stale.
    Derived,
    /// Advisory: the page never goes stale over it.
    Cites,
}

/// How a page stands against one anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorState {
    /// The target matches the pinned digest.
    Fresh,
    /// The target moved and nothing accounts for it.
    Stale,
    /// The target moved and an open note covers the change.
    Noted,
    /// The target is gone from the checkout.
    Missing,
}

/// One anchor of a page. `path` is relative to the repository root and uses
/// `/` separators; an empty path is the root itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Anchor {
    pub kind: AnchorKind,
    #[serde(default, skip_serializing_if = "is_content")]
    pub mode: AnchorMode,
    pub role: AnchorRole,
    pub path: String,
    /// sha256 hex of the manifest at pin time; empty while unpinned.
    pub digest: String,
    /// The commit the pin was taken at, when the checkout is a git one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if takes a reference
fn is_content(m: &AnchorMode) -> bool {
    *m == AnchorMode::Content
}

impl Anchor {
    /// The wire ref: `file:<path>`, `dir:<path>` or `glob:<pattern>`.
    pub fn reference(&self) -> String {
        let scheme = match self.kind {
            AnchorKind::File => "file",
            AnchorKind::Dir => "dir",
            AnchorKind::Glob => "glob",
        };
        format!("{scheme}:{}", self.path)
    }

    /// Does this anchor derive the page from `file` (a repo-relative path)?
    /// Cited and shape anchors never count, as in a coverage report.
    pub fn derives(&self, file: &str) -> bool {
        self.role == AnchorRole::Derived && self.mode == AnchorMode::Content && self.matches(file)
    }

    /// Is `file` under this anchor's target, whatever its role and mode?
    pub fn matches(&self, file: &str) -> bool {
        match self.kind {
            AnchorKind::File => file == self.path,
            AnchorKind::Dir => under(&self.path, file),
            AnchorKind::Glob => glob_match(&self.path, file),
        }
    }

    /// The anchor's state given the digest the checkout yields now (`None`
    /// when the target is gone) and whether an open note covers the move.
    pub fn state(&self, current: Option<&str>, covered: bool) -> AnchorState {
        match current {
            None => AnchorState::Missing,
            Some(d) if d == self.digest => AnchorState::Fresh,
            Some(_) if covered => AnchorState::Noted,
            Some(_) => AnchorState::Stale,
        }
    }
}

/// Is `file` the path `dir` or below it?
fn under(dir: &str, file: &str) -> bool {
    let dir = dir.trim_end_matches('/');
    dir.is_empty() || file == dir || file.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

/// Does a glob with `*`, `?` and `**` (whole segments) match `path`?
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match_segments(&pat, &segs)
}

fn match_segments(pat: &[&str], segs: &[&str]) -> bool {
    match pat.split_first() {
        None => segs.is_empty(),
        Some((&"**", rest)) => (0..=segs.len()).any(|i| match_segments(rest, &segs[i..])),
        Some((p, rest)) => segs.split_first().is_some_and(|(s, tail)| {
            match_segment(p.as_bytes(), s.as_bytes()) && match_segments(rest, tail)
        }),
    }
}

fn match_segment(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| match_segment(rest, &s[i..])),
        Some((b'?', rest)) => !s.is_empty() && match_segment(rest, &s[1..]),
        Some((c, rest)) => s.first() == Some(c) && match_segment(rest, &s[1..]),
    }
}

/// Is this path a glob pattern?
pub fn is_glob(path: &str) -> bool {
    path.contains(['*', '?', '['])
}

/// sha256 hex of one file's bytes.
pub fn file_digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// The digest of a set of files: a `<relpath>\0<file digest>\n` line per
/// `(relative path, file digest)` entry, in bytewise path order.
pub fn tree_digest(entries: &[(String, String)]) -> String {
    let mut sorted: Vec<&(String, String)> = entries.iter().collect();
    sorted.sort();
    let mut h = Sha256::new();
    for (path, digest) in sorted {
        h.update(path.as_bytes());
        h.update(b"\0");
        h.update(digest.as_bytes());
        h.update(b"\n");
    }
    hex(&h.finalize())
}

/// The digest of a directory listing: a `<relpath>\n` line per path, sorted.
pub fn shape_digest(paths: &[String]) -> String {
    let mut sorted: Vec<&String> = paths.iter().collect();
    sorted.sort();
    let mut h = Sha256::new();
    for p in sorted {
        h.update(p.as_bytes());
        h.update(b"\n");
    }
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Which files no page derives from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    pub files: usize,
    pub covered: usize,
    /// The uncovered files, in path order.
    pub uncovered: Vec<String>,
}

/// Coverage of `files` (repo-relative) by the derived anchors of all pages.
pub fn coverage<'a>(files: &[&str], anchors: impl IntoIterator<Item = &'a Anchor>) -> Coverage {
    let anchors: Vec<&Anchor> = anchors
        .into_iter()
        .filter(|a| a.role == AnchorRole::Derived)
        .collect();
    let mut uncovered: Vec<String> = files
        .iter()
        .filter(|f| !anchors.iter().any(|a| a.derives(f)))
        .map(|f| (*f).to_owned())
        .collect();
    uncovered.sort();
    Coverage {
        files: files.len(),
        covered: files.len() - uncovered.len(),
        uncovered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(kind: AnchorKind, role: AnchorRole, path: &str) -> Anchor {
        Anchor {
            kind,
            mode: AnchorMode::Content,
            role,
            path: path.into(),
            digest: "aa".into(),
            commit: None,
        }
    }

    #[test]
    fn globs_match_segments_and_double_stars() {
        assert!(glob_match("rust/src/**/*.rs", "rust/src/a.rs"));
        assert!(glob_match("rust/src/**/*.rs", "rust/src/core/hive/b.rs"));
        assert!(!glob_match("rust/src/**/*.rs", "rust/srcx/a.rs"));
        assert!(!glob_match("rust/src/*.rs", "rust/src/core/a.rs"));
        assert!(glob_match("a/?.md", "a/b.md"));
        assert!(glob_match("**", "anything/at/all"));
    }

    #[test]
    fn path_anchors_cover_their_subtree_only() {
        let a = anchor(AnchorKind::Dir, AnchorRole::Derived, "rust/src/core");
        assert!(a.derives("rust/src/core/x.rs"));
        assert!(!a.derives("rust/src/corex/y.rs"));
        assert!(anchor(AnchorKind::Dir, AnchorRole::Derived, "").derives("x"));
        let f = anchor(AnchorKind::File, AnchorRole::Derived, "rust/src/a.rs");
        assert!(f.derives("rust/src/a.rs") && !f.derives("rust/src/a.rs/b"));
        assert_eq!(a.reference(), "dir:rust/src/core");
    }

    #[test]
    fn coverage_ignores_cites_and_shapes() {
        let anchors = [
            anchor(AnchorKind::File, AnchorRole::Derived, "src/a.rs"),
            anchor(AnchorKind::File, AnchorRole::Cites, "src/b.rs"),
            Anchor {
                mode: AnchorMode::Shape,
                ..anchor(AnchorKind::Dir, AnchorRole::Derived, "src")
            },
            anchor(AnchorKind::Glob, AnchorRole::Derived, "src/g/*.rs"),
        ];
        let c = coverage(
            &["src/a.rs", "src/b.rs", "src/g/c.rs", "src/d.rs"],
            &anchors,
        );
        assert_eq!(c.covered, 2);
        assert_eq!(c.uncovered, ["src/b.rs", "src/d.rs"]);
    }

    #[test]
    fn states_follow_the_digest_and_notes() {
        let a = anchor(AnchorKind::File, AnchorRole::Derived, "x");
        assert_eq!(a.state(Some("aa"), false), AnchorState::Fresh);
        assert_eq!(a.state(Some("bb"), false), AnchorState::Stale);
        assert_eq!(a.state(Some("bb"), true), AnchorState::Noted);
        assert_eq!(a.state(None, false), AnchorState::Missing);
    }

    // C0 2.2 vectors (hive-v2 vectors/doc_anchor_digest).
    #[test]
    fn digest_vectors() {
        let lib = file_digest(b"pub mod x;\n");
        assert_eq!(
            lib,
            "1497ba2ae9c58431b70c98c925ac011cdfc28b8b74ce9e0e5ae54c0261ec5fb9"
        );
        let dir = tree_digest(&[
            ("b.md".to_owned(), file_digest(b"beta\n")),
            ("a.md".to_owned(), file_digest(b"alpha\n")),
        ]);
        assert_eq!(
            dir,
            "1d71a3193a1273437b70644f06adaff7d368b9c6f0598776ba0503574103f93f"
        );
        let glob = tree_digest(&[
            ("src/lib.rs".to_owned(), lib),
            ("src/x/y.rs".to_owned(), file_digest(b"pub fn y() {}\n")),
        ]);
        assert_eq!(
            glob,
            "e5ba8609e0f1153e71e6015814700e24ddf89abe7b5953363ccc53f79aff32f6"
        );
        assert!(
            glob_match("src/**/*.rs", "src/lib.rs") && !glob_match("src/**/*.rs", "src/README.md")
        );
        assert_eq!(
            shape_digest(&["b.md".into(), "a.md".into()]),
            "80470d491b39cfbc95db36d718eddff3c90a184765b75ab9353d3b53408d65d1"
        );
        assert_eq!(
            tree_digest(&[]),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn digests_are_order_independent_and_path_sensitive() {
        let a = vec![
            ("a".to_owned(), "d1".to_owned()),
            ("b".to_owned(), "d2".to_owned()),
        ];
        let b = vec![a[1].clone(), a[0].clone()];
        assert_eq!(tree_digest(&a), tree_digest(&b));
        assert_ne!(
            tree_digest(&a),
            tree_digest(&[("c".to_owned(), "d1".to_owned())])
        );
        assert_ne!(shape_digest(&["a".into()]), shape_digest(&["b".into()]));
        assert_eq!(
            file_digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
