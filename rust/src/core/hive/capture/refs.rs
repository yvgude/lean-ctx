// SPDX-License-Identifier: Apache-2.0
//! Reference grammar of the capture gate (SPEC-ENGINE §3.1.3, rule G10):
//! `file:`, `sym:`, `test:`, `commit:`, `node:` and `https://`, each checked
//! against the checkout bound to the vault. An invalid reference counts as
//! absent; a commit that is on no remote-tracking branch yet is *pending*.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A parsed reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reference {
    File {
        path: String,
        lines: Option<(u32, u32)>,
    },
    Symbol {
        path: String,
        symbol: String,
    },
    Test {
        path: String,
        name: String,
    },
    Commit(String),
    Node(String),
    Url(String),
}

/// What a reference check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefState {
    Valid,
    Invalid,
    /// A commit that exists locally but is on no remote-tracking branch.
    Unpublished,
}

/// Repo-relative POSIX path, no leading `/`, no `.`/`..` segment, ≤ 300 bytes.
fn path_ok(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 300
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(|c| c.is_whitespace() || c.is_control())
        && path
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

impl Reference {
    /// The reference as C0 2.0 writes it (`sym:<path>::<symbol>`).
    pub fn to_contract(&self) -> String {
        match self {
            Self::File { path, lines: None } => format!("file:{path}"),
            Self::File {
                path,
                lines: Some((a, b)),
            } if a == b => format!("file:{path}#L{a}"),
            Self::File {
                path,
                lines: Some((a, b)),
            } => format!("file:{path}#L{a}-{b}"),
            Self::Symbol { path, symbol } => format!("sym:{path}::{symbol}"),
            Self::Test { path, name } => format!("test:{path}::{name}"),
            Self::Commit(sha) => format!("commit:{sha}"),
            Self::Node(id) => format!("node:{id}"),
            Self::Url(url) => url.clone(),
        }
    }
}

fn symbol_ok(symbol: &str) -> bool {
    symbol.len() <= 200
        && symbol.split("::").flat_map(|s| s.split('.')).all(|seg| {
            let mut chars = seg.chars();
            chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

fn test_name_ok(name: &str) -> bool {
    (1..=200).contains(&name.len())
        && name.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.' | '-' | '[' | ']' | ' ')
        })
}

fn url_ok(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host_port = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host_port.rsplit_once(':').map_or(host_port, |(h, _)| h);
    url.len() <= 500
        && !host.is_empty()
        && !host_port.contains('@')
        && !host.starts_with('[')
        && host.parse::<std::net::Ipv4Addr>().is_err()
        && host.contains('.')
        && url.chars().all(|c| c.is_ascii_graphic())
}

/// Parse one reference; `None` when it does not follow the grammar.
pub fn parse(raw: &str) -> Option<Reference> {
    if raw.starts_with("https://") {
        return url_ok(raw).then(|| Reference::Url(raw.to_string()));
    }
    let (kind, rest) = raw.split_once(':')?;
    match kind {
        "file" => {
            let (path, lines) = match rest.split_once("#L") {
                Some((path, range)) => {
                    let (a, b) = range.split_once('-').unwrap_or((range, range));
                    let (a, b): (u32, u32) = (a.parse().ok()?, b.parse().ok()?);
                    if a == 0 || a > b {
                        return None;
                    }
                    (path, Some((a, b)))
                }
                None => (rest, None),
            };
            path_ok(path).then(|| Reference::File {
                path: path.to_string(),
                lines,
            })
        }
        "sym" => {
            // C0 2.0 anchors write `sym:<path>::<symbol>`; SPEC-ENGINE G10
            // wrote `sym:<path>#<symbol>`. Both are read (a path never holds
            // `::`); `Reference::to_contract` writes the contract form.
            let colons = rest.find("::");
            let (path, symbol) = match (rest.find('#'), colons) {
                (Some(h), Some(c)) if h < c => rest.split_once('#')?,
                (Some(_), None) => rest.split_once('#')?,
                _ => rest.split_once("::")?,
            };
            (path_ok(path) && symbol_ok(symbol)).then(|| Reference::Symbol {
                path: path.to_string(),
                symbol: symbol.to_string(),
            })
        }
        "test" => {
            let (path, name) = rest.split_once("::")?;
            (path_ok(path) && test_name_ok(name)).then(|| Reference::Test {
                path: path.to_string(),
                name: name.to_string(),
            })
        }
        "commit" => ((7..=40).contains(&rest.len())
            && rest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .then(|| Reference::Commit(rest.to_string())),
        "node" => {
            let id = rest.strip_prefix("n_")?;
            (id.len() == 26
                && id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b)))
            .then(|| Reference::Node(rest.to_string()))
        }
        _ => None,
    }
}

/// The checkout references are checked against, with per-run caches so a
/// capture run spawns each `git` query once.
// lean-ctx: one `git` process per distinct path/commit; upgrade to a
// `cat-file --batch` session when capture runs exceed a few hundred refs.
pub struct Checkout {
    root: PathBuf,
    blobs: HashMap<String, Option<Vec<u8>>>,
    commits: HashMap<String, RefState>,
    head: Option<String>,
    head_read: bool,
    /// Whether `HEAD` contains a commit (`merge-base --is-ancestor`).
    contained: HashMap<String, bool>,
    /// Node ids known to the place (replica and this batch).
    pub nodes: std::collections::HashSet<String>,
}

impl Checkout {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            blobs: HashMap::new(),
            commits: HashMap::new(),
            head: None,
            head_read: false,
            contained: HashMap::new(),
            nodes: Default::default(),
        }
    }

    /// Whether `HEAD` is `sha` or a descendant of it. `false` for a commit
    /// this clone does not have, a branch that diverged, or anything that
    /// is not a hex commit id.
    pub fn head_contains(&mut self, sha: &str) -> bool {
        if !(7..=64).contains(&sha.len()) || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
        if let Some(known) = self.contained.get(sha) {
            return *known;
        }
        let known = self
            .git(&["merge-base", "--is-ancestor", sha, "HEAD"])
            .is_some();
        self.contained.insert(sha.to_string(), known);
        known
    }

    fn git(&self, args: &[&str]) -> Option<Vec<u8>> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .ok()?;
        out.status.success().then_some(out.stdout)
    }

    /// The file's bytes at `HEAD`, `None` when it is not in `HEAD`'s tree.
    pub fn blob_bytes(&mut self, path: &str) -> Option<&[u8]> {
        if !self.blobs.contains_key(path) {
            let content = self.git(&["cat-file", "-p", &format!("HEAD:{path}")]);
            self.blobs.insert(path.to_string(), content);
        }
        self.blobs.get(path).and_then(Option::as_deref)
    }

    fn blob(&mut self, path: &str) -> Option<String> {
        self.blob_bytes(path)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }

    /// The bytes a `sym:` reference anchors at `HEAD` (the definition of the
    /// symbol's last segment), as the engine's anchor extractor finds it.
    pub fn symbol_span(&mut self, path: &str, symbol: &str) -> Option<Vec<u8>> {
        let symbol = symbol.replace("::", ".");
        let leaf = symbol.rsplit('.').next().unwrap_or(&symbol).to_string();
        let content = self.blob_bytes(path)?;
        crate::core::knowledge::anchors::anchored(content, path, Some(&leaf)).map(<[u8]>::to_vec)
    }

    /// The full sha of `HEAD`.
    pub fn head(&mut self) -> Option<String> {
        if !self.head_read {
            self.head = self
                .git(&["rev-parse", "HEAD"])
                .map(|b| String::from_utf8_lossy(&b).trim().to_string())
                .filter(|s| matches!(s.len(), 40 | 64));
            self.head_read = true;
        }
        self.head.clone()
    }

    fn commit(&mut self, sha: &str) -> RefState {
        if let Some(state) = self.commits.get(sha) {
            return *state;
        }
        let state = if self
            .git(&["cat-file", "-e", &format!("{sha}^{{commit}}")])
            .is_none()
        {
            RefState::Invalid
        } else if self
            .git(&["branch", "-r", "--contains", sha])
            .is_some_and(|out| !out.trim_ascii().is_empty())
        {
            RefState::Valid
        } else {
            RefState::Unpublished
        };
        self.commits.insert(sha.to_string(), state);
        state
    }

    /// Check one reference.
    pub fn check(&mut self, reference: &Reference) -> RefState {
        let ok = |b: bool| {
            if b {
                RefState::Valid
            } else {
                RefState::Invalid
            }
        };
        match reference {
            Reference::File { path, lines } => {
                let Some(content) = self.blob(path) else {
                    return RefState::Invalid;
                };
                ok(lines.is_none_or(|(_, b)| b as usize <= content.lines().count()))
            }
            Reference::Symbol { path, symbol } => {
                let path = path.clone();
                ok(self.symbol_span(&path, symbol).is_some())
            }
            Reference::Test { path, name } => {
                let leaf = name.rsplit("::").next().unwrap_or(name);
                ok(self.blob(path).is_some_and(|c| c.contains(leaf)))
            }
            Reference::Commit(sha) => self.commit(sha),
            Reference::Node(id) => ok(self.nodes.contains(id)),
            Reference::Url(_) => RefState::Valid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar() {
        for good in [
            "file:rust/src/lib.rs",
            "file:rust/src/lib.rs#L3-9",
            "file:README.md#L1",
            "sym:rust/src/core/hive/outbox.rs#Outbox::push",
            "test:rust/src/a.rs::tests::name",
            "commit:4a551fc452",
            "node:n_abcdefghijklmnopqrstuvwxyz",
            "https://docs.rs/blake3/latest/blake3/",
        ] {
            assert!(parse(good).is_some(), "{good}");
        }
        for bad in [
            "file:/etc/passwd",
            "file:../x.rs",
            "file:a/./b.rs",
            "file:a.rs#L0",
            "file:a.rs#L9-3",
            "sym:a.rs#1bad",
            "commit:XYZ1234",
            "commit:abc",
            "node:n_short",
            "http://example.com",
            "https://127.0.0.1/x",
            "https://user@example.com/x",
            "https://localhost/x",
            "ftp:x",
            "plain text",
        ] {
            assert!(parse(bad).is_none(), "{bad}");
        }
    }

    /// C0 2.0 `vectors/anchor.vectors.json` (ref): file and sym references
    /// read as the contract reads them; `#` sym form still read.
    #[test]
    fn contract_anchor_refs() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2/vectors/anchor.vectors.json");
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("vectors")).expect("json");
        for case in v["ref"].as_array().expect("ref cases") {
            let input = case["input"].as_str().expect("input");
            let out = &case["output"];
            match parse(input) {
                None => assert_eq!(out["valid"], false, "{input}"),
                Some(r) => {
                    assert_eq!(out["valid"], true, "{input}");
                    assert_eq!(r.to_contract(), input);
                    match r {
                        Reference::File { path, .. } => assert_eq!(out["path"], path.as_str()),
                        Reference::Symbol { path, symbol } => {
                            assert_eq!(out["path"], path.as_str());
                            assert_eq!(out["symbol"], symbol.as_str());
                        }
                        other => panic!("{input}: {other:?}"),
                    }
                }
            }
        }
        assert_eq!(
            parse("sym:src/lib.rs#Client::send").map(|r| r.to_contract()),
            Some("sym:src/lib.rs::Client::send".to_string())
        );
    }
}
