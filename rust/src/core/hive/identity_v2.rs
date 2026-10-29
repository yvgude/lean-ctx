// SPDX-License-Identifier: Apache-2.0
//! Project identity v2 (CHECKLIST 2.1.2, design
//! `ee/delivery/hive-v5-project-mapping.md`): the same repository has the
//! same identity on every device, built from the committed `.lean-ctx-id`,
//! the normalized origin remote and the root commit. Never the local path.
//!
//! Local only. Hive sees each candidate as an org-salted digest
//! ([`super::vault::identity_digest`]), like the v1 identity today. The
//! identity is a signal for finding the vault, never an authorization:
//! Hive checks membership and vault rights.
//!
//! [`super::mapping`] sends them to `POST /v1/vaults/match` when the Hive
//! offers it; resolve keeps the v1 identity.

use std::path::Path;
use std::process::Command;

use sha2::{Digest as _, Sha256};

use super::vault::{normalize_git_url, project_identity};
use lean_ctx_protocol::hive::vault::stable_identity;

/// Domain of the v2 canonical text (v1: `REPO_IDENTITY_DOMAIN`).
pub const REPO_IDENTITY_V2_DOMAIN: &str = "leanctx.project-identity.v2\n";
const IDENTITY_FILE: &str = ".lean-ctx-id";
/// The longest `.lean-ctx-id` that counts (as `project_hash` reads it).
const MAX_ID_CHARS: usize = 256;

/// The parts of a v2 identity, each optional.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityParts {
    /// `.lean-ctx-id` at the checkout root, trimmed.
    pub id: Option<String>,
    /// The origin remote, normalized (no scheme, credentials, `.git`).
    pub remote: Option<String>,
    /// The smallest root commit of `HEAD`; `None` in a shallow clone.
    pub root: Option<String>,
}

impl IdentityParts {
    /// `leanctx.project-identity.v2\n` + `id:…\n` `remote:…\n` `root:…\n`
    /// in this order, absent parts left out. `None` when every part is
    /// absent or one holds a line break (the text would be ambiguous).
    pub fn canonical_text(&self) -> Option<String> {
        let parts = [
            ("id", &self.id),
            ("remote", &self.remote),
            ("root", &self.root),
        ];
        if parts.iter().all(|(_, v)| v.is_none()) {
            return None;
        }
        let mut text = String::from(REPO_IDENTITY_V2_DOMAIN);
        for (key, value) in parts {
            if let Some(value) = value {
                if value.is_empty() || value.contains(['\n', '\r']) {
                    return None;
                }
                text.push_str(key);
                text.push(':');
                text.push_str(value);
                text.push('\n');
            }
        }
        Some(text)
    }

    /// `sha256_hex(canonical_text)`.
    pub fn stable(&self) -> Option<String> {
        self.canonical_text()
            .map(|text| hex::encode(Sha256::digest(text.as_bytes())))
    }
}

/// How strongly a candidate names the repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    /// Every part this checkout has. A hit maps automatically.
    Exact,
    /// The v1 identity of engines before v5. A hit maps automatically,
    /// because that is how those engines bound the vault.
    Legacy,
    /// The root commit alone, finds a moved remote. Forks share it, so a
    /// hit is only a suggestion the person confirms.
    History,
}

impl CandidateKind {
    /// The `kind` of a match candidate on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Legacy => "legacy",
            Self::History => "history",
        }
    }
}

/// One stable identity to look up, before the org salt is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub kind: CandidateKind,
    pub stable: String,
}

/// The candidates for the checkout at `root`, strongest first, without
/// duplicates. Empty when the checkout has no identity at all.
pub fn candidates(root: &Path) -> Vec<Candidate> {
    candidates_from(&read_parts(root), project_identity(root).as_deref())
}

/// [`candidates`] from parts already read; `legacy` is the v1 identity.
pub fn candidates_from(parts: &IdentityParts, legacy: Option<&str>) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut push = |kind, stable: Option<String>| {
        if let Some(stable) = stable
            && !out.iter().any(|c| c.stable == stable)
        {
            out.push(Candidate { kind, stable });
        }
    };
    push(CandidateKind::Exact, parts.stable());
    push(CandidateKind::Legacy, legacy.map(stable_identity));
    if parts.remote.is_some() && parts.root.is_some() {
        let history = IdentityParts {
            root: parts.root.clone(),
            ..IdentityParts::default()
        };
        push(CandidateKind::History, history.stable());
    }
    out
}

/// Whether the v1 identity is only a manifest name (`cargo:demo`,
/// `npm:…`, …): no git remote and no identity file. Unrelated projects
/// share such names across a workspace.
pub fn weak_legacy(legacy: &str) -> bool {
    !(legacy.starts_with("git:") || legacy.starts_with("explicit:"))
}

/// The candidates of the checkout at `root` that go to `POST
/// /v1/vaults/match`: [`candidates`], without the legacy candidate when it
/// is a [`weak_legacy`] manifest name and an exact candidate exists. A
/// legacy hit maps automatically, so a crate name another project shares
/// must not outrank this checkout's own identity; with no exact candidate
/// the weak one is all there is and Hive only suggests from it.
pub fn match_candidates(root: &Path) -> Vec<Candidate> {
    let legacy = project_identity(root);
    sendable(
        candidates_from(&read_parts(root), legacy.as_deref()),
        legacy.as_deref(),
    )
}

/// [`match_candidates`] from candidates already computed.
pub fn sendable(mut candidates: Vec<Candidate>, legacy: Option<&str>) -> Vec<Candidate> {
    if legacy.is_some_and(weak_legacy) && candidates.iter().any(|c| c.kind == CandidateKind::Exact)
    {
        candidates.retain(|c| c.kind != CandidateKind::Legacy);
    }
    candidates
}

/// Read the parts of the checkout at `root`. A linked worktree reads the
/// remote and the commits of its repository (`git -C` follows `.git`).
pub fn read_parts(root: &Path) -> IdentityParts {
    IdentityParts {
        id: read_id_file(root),
        remote: origin_remote(root),
        root: root_commit(root),
    }
}

/// The origin remote of the checkout at `root`, normalized (no scheme,
/// credentials or `.git`). Stays on this machine.
pub fn origin_remote(root: &Path) -> Option<String> {
    git(root, &["config", "--get", "remote.origin.url"]).and_then(|url| remote_part(&url))
}

/// The `remote` part of a remote URL; `None` when nothing is left.
pub fn remote_part(url: &str) -> Option<String> {
    Some(normalize_git_url(url)).filter(|url| !url.is_empty())
}

/// The `root` part: the smallest of `commits` that are 40 or 64 hex
/// characters, lowercased; `None` for a shallow clone.
pub fn root_part<'a>(commits: impl IntoIterator<Item = &'a str>, shallow: bool) -> Option<String> {
    if shallow {
        return None;
    }
    commits
        .into_iter()
        .map(str::trim)
        .filter(|sha| matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .min()
}

fn read_id_file(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join(IDENTITY_FILE)).ok()?;
    let id = text.trim();
    (!id.is_empty() && id.chars().count() <= MAX_ID_CHARS).then(|| id.to_string())
}

/// The smallest root commit of `HEAD`; `None` for a shallow clone, whose
/// oldest commits depend on the fetch depth, and without git or commits.
fn root_commit(root: &Path) -> Option<String> {
    if git(root, &["rev-parse", "--is-shallow-repository"]).as_deref() != Some("false") {
        return None;
    }
    let roots = git(root, &["rev-list", "--max-parents=0", "HEAD"])?;
    root_part(roots.lines(), false)
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// C0 `vectors/identity_v2.vectors.json` (the engine's byte-equal copy).
#[cfg(test)]
pub(crate) fn contract_vectors() -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2/vectors/identity_v2.vectors.json");
    serde_json::from_slice(&std::fs::read(path).expect("identity_v2 vectors")).expect("json")
}

/// The parts a vector's `input` names.
#[cfg(test)]
pub(crate) fn vector_parts(input: &serde_json::Value) -> IdentityParts {
    IdentityParts {
        id: input["id"].as_str().map(str::to_string),
        remote: input["remote_url"].as_str().and_then(remote_part),
        root: root_part(
            input["root_commits"]
                .as_array()
                .expect("root_commits")
                .iter()
                .filter_map(serde_json::Value::as_str),
            input["shallow"].as_bool().expect("shallow"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Engines of every version and the server agree on the candidates:
    /// each case of the C0 vectors, part by part.
    #[test]
    fn candidates_match_the_contract_vectors() {
        use base64::Engine as _;
        let vectors = contract_vectors();
        let salt = base64::engine::general_purpose::STANDARD
            .decode(vectors["salt"].as_str().unwrap())
            .unwrap();
        let cases = vectors["cases"].as_array().unwrap();
        assert!(cases.len() >= 9);
        for case in cases {
            let (input, output) = (&case["input"], &case["output"]);
            let name = case["name"].as_str().unwrap();
            let parts = vector_parts(input);
            assert_eq!(parts.remote.as_deref(), output["remote"].as_str(), "{name}");
            assert_eq!(parts.root.as_deref(), output["root"].as_str(), "{name}");
            assert_eq!(
                parts.canonical_text().as_deref(),
                output["canonical_text"].as_str(),
                "{name}"
            );
            assert_eq!(
                parts.stable().as_deref(),
                output["stable_v2"].as_str(),
                "{name}"
            );
            let got: Vec<serde_json::Value> =
                candidates_from(&parts, input["legacy_identity"].as_str())
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "kind": c.kind.as_str(),
                            "stable": c.stable,
                            "digest": super::super::vault::identity_digest(&salt, &c.stable),
                        })
                    })
                    .collect();
            assert_eq!(
                serde_json::Value::Array(got),
                output["candidates"],
                "{name}"
            );
        }
    }

    /// The match request leaves a weak legacy candidate (a manifest name)
    /// out next to an exact one; strong legacy identities (git remote,
    /// identity file) and a weak one standing alone are still sent.
    #[test]
    fn a_weak_legacy_name_is_not_sent_next_to_an_exact_candidate() {
        let vectors = contract_vectors();
        let mut checked = 0;
        for case in vectors["cases"].as_array().unwrap() {
            let input = &case["input"];
            let legacy = input["legacy_identity"].as_str();
            let all = candidates_from(&vector_parts(input), legacy);
            let has_exact = all.iter().any(|c| c.kind == CandidateKind::Exact);
            let sent: Vec<&str> = sendable(all.clone(), legacy)
                .iter()
                .map(|c| c.kind.as_str())
                .collect();
            let expected: Vec<&str> = all
                .iter()
                .map(|c| c.kind.as_str())
                .filter(|k| !(*k == "legacy" && has_exact && legacy.is_some_and(weak_legacy)))
                .collect();
            assert_eq!(sent, expected, "{}", case["name"]);
            if legacy == Some("cargo:demo") {
                checked += 1;
                assert_eq!(
                    sent,
                    if has_exact {
                        vec!["exact"]
                    } else {
                        vec!["legacy"]
                    },
                    "{}",
                    case["name"]
                );
            }
        }
        assert_eq!(checked, 2, "both manifest-name vector cases are covered");
        assert!(!weak_legacy("git:github.com/acme/payments"));
        assert!(!weak_legacy("explicit:3f6c1e9a0b7d4c2e"));
        assert!(weak_legacy("npm:demo"));

        // A real checkout: a crate in a local repository without origin.
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
        git_ok(&dir, &["init", "-q", "-b", "main"]);
        git_ok(&dir, &["add", "."]);
        git_ok(&dir, &["commit", "-q", "-m", "init"]);
        let kinds: Vec<CandidateKind> = match_candidates(&dir).iter().map(|c| c.kind).collect();
        assert_eq!(kinds, [CandidateKind::Exact]);
        assert!(
            candidates(&dir)
                .iter()
                .any(|c| c.kind == CandidateKind::Legacy),
            "the contract candidates keep it"
        );
    }

    fn git_ok(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A repository whose first commit holds `name`, so two of them never
    /// share a root commit.
    fn repo_with_remote(base: &Path, name: &str, url: &str) -> std::path::PathBuf {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        git_ok(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("README"), format!("{name}\n")).unwrap();
        git_ok(&dir, &["add", "."]);
        git_ok(&dir, &["commit", "-q", "-m", "init"]);
        git_ok(&dir, &["remote", "add", "origin", url]);
        dir
    }

    #[test]
    fn the_canonical_text_is_fixed_and_rejects_ambiguous_parts() {
        let parts = IdentityParts {
            id: None,
            remote: Some("github.com/acme/payments".into()),
            root: Some("a".repeat(40)),
        };
        assert_eq!(
            parts.canonical_text().unwrap(),
            format!(
                "leanctx.project-identity.v2\nremote:github.com/acme/payments\nroot:{}\n",
                "a".repeat(40)
            )
        );
        assert_eq!(parts.stable().unwrap().len(), 64);
        assert!(IdentityParts::default().canonical_text().is_none());
        let broken = IdentityParts {
            id: Some("x\nroot:evil".into()),
            ..IdentityParts::default()
        };
        assert!(
            broken.canonical_text().is_none(),
            "a line break would forge a part"
        );
        let v1 = stable_identity("git:github.com/acme/payments");
        let remote_only = IdentityParts {
            remote: Some("github.com/acme/payments".into()),
            ..IdentityParts::default()
        };
        assert_ne!(remote_only.stable().unwrap(), v1, "v2 is domain separated");
    }

    #[test]
    fn clones_with_different_remote_spellings_share_the_exact_identity() {
        let base = tempfile::tempdir().unwrap();
        let a = repo_with_remote(base.path(), "a", "git@github.com:acme/payments.git");
        let b = base.path().join("b");
        git_ok(
            base.path(),
            &["clone", "-q", a.to_str().unwrap(), b.to_str().unwrap()],
        );
        git_ok(
            &b,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/acme/payments",
            ],
        );
        let (pa, pb) = (read_parts(&a), read_parts(&b));
        assert_eq!(pa.remote.as_deref(), Some("github.com/acme/payments"));
        assert!(pa.root.is_some());
        assert_eq!(pa, pb);
        let (ca, cb) = (candidates(&a), candidates(&b));
        assert_eq!(ca, cb);
        assert_eq!(ca[0].kind, CandidateKind::Exact);
        assert!(!ca[0].stable.contains("acme"), "no remote text, a hash");

        // Same remote, other history (a recreated repo): another identity,
        // only the legacy v1 candidate agrees.
        let c = repo_with_remote(base.path(), "c", "git@github.com:acme/payments.git");
        let cc = candidates(&c);
        assert_ne!(cc[0].stable, ca[0].stable);
        let legacy = |cs: &[Candidate]| {
            cs.iter()
                .find(|c| c.kind == CandidateKind::Legacy)
                .map(|c| c.stable.clone())
        };
        assert_eq!(legacy(&cc), legacy(&ca));
    }

    #[test]
    fn a_moved_remote_keeps_the_history_candidate_and_a_worktree_reads_its_repo() {
        let base = tempfile::tempdir().unwrap();
        let a = repo_with_remote(base.path(), "a", "git@github.com:acme/payments.git");
        let before = candidates(&a);
        let wt = base.path().join("wt");
        git_ok(&a, &["worktree", "add", "-q", wt.to_str().unwrap()]);
        assert_eq!(read_parts(&wt), read_parts(&a), "a linked worktree");

        git_ok(
            &a,
            &[
                "remote",
                "set-url",
                "origin",
                "git@gitlab.example.com:acme/payments.git",
            ],
        );
        let after = candidates(&a);
        let history = |cs: &[Candidate]| {
            cs.iter()
                .find(|c| c.kind == CandidateKind::History)
                .map(|c| c.stable.clone())
        };
        assert_ne!(after[0].stable, before[0].stable);
        assert!(history(&after).is_some());
        assert_eq!(history(&after), history(&before));
    }

    #[test]
    fn a_shallow_clone_has_no_root_and_a_plain_folder_only_its_v1_identity() {
        let base = tempfile::tempdir().unwrap();
        let a = repo_with_remote(base.path(), "a", "git@github.com:acme/payments.git");
        std::fs::write(a.join("README"), "two\n").unwrap();
        git_ok(&a, &["commit", "-q", "-am", "two"]);
        let shallow = base.path().join("shallow");
        git_ok(
            base.path(),
            &[
                "clone",
                "-q",
                "--depth",
                "1",
                &format!("file://{}", a.display()),
                shallow.to_str().unwrap(),
            ],
        );
        assert!(read_parts(&shallow).root.is_none());

        let plain = base.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
        let cs = candidates(&plain);
        assert_eq!(cs.len(), 1);
        assert_eq!(
            cs[0].kind,
            CandidateKind::Legacy,
            "weak: never auto-created"
        );

        std::fs::write(plain.join(IDENTITY_FILE), "3f6c1e9a0b7d4c2e\n").unwrap();
        let with_id = candidates(&plain);
        assert_eq!(with_id[0].kind, CandidateKind::Exact);
        assert_eq!(read_parts(&plain).id.as_deref(), Some("3f6c1e9a0b7d4c2e"));
        assert!(candidates(&base.path().join("missing")).is_empty());
    }
}
