// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hive link <workspace>/<vault>[@<server>]`: link a git
//! repository to a vault.
//!
//! Writes `.lean-ctx-hive.json` at the repository root, next to
//! `.lean-ctx-id`. A hosted vault keeps link v1 (`{"schema", "repo": {"ns",
//! "name"}}`), which every engine in the team reads; a self-hosted one needs
//! link v2 (`{"schema": "leanctx.hive.link/v2", "vault": "<address>"}`, C0
//! 2.0 `link.schema.json`). Both are read; v1 is a hosted address.
//! Resolution sends the vault on first sight; Hive binds it when this
//! instance may contribute there. A repository without any identity marker
//! gets a random `.lean-ctx-id` so its identity survives moves.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::HiveError;
use super::vault_address::VaultAddress;
use lean_ctx_protocol::hive::vault::VaultRefV1;

pub const LINK_FILE: &str = ".lean-ctx-hive.json";
/// The link file before the Hive rename. Still read when [`LINK_FILE`] is
/// missing; reading it writes [`LINK_FILE`] with the same bytes and keeps
/// this file (it is committed, older engines in the team read it).
pub const LEGACY_LINK_FILE: &str = ".lean-ctx-hive.json";

/// True when `root` holds a link file under either name.
pub fn has_link_file(root: &Path) -> bool {
    root.join(LINK_FILE).exists() || root.join(LEGACY_LINK_FILE).exists()
}
pub const LINK_SCHEMA: &str = "leanctx.hive.link/v1";
pub const LINK_SCHEMA_V2: &str = "leanctx.hive.link/v2";
const IDENTITY_FILE: &str = ".lean-ctx-id";

/// A link as read: the vault plus, for a self-hosted one, its server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultLink {
    pub schema: String,
    pub vault: VaultRefV1,
    /// Self-hosted server (`host[:port]`); `None` = hosted Hive.
    pub server: Option<String>,
}

impl VaultLink {
    /// The vault address `<ws>/<vault>[@<server>]`.
    pub fn address(&self) -> VaultAddress {
        VaultAddress {
            ws: self.vault.ns.clone(),
            vault: self.vault.name.clone(),
            server: self.server.clone(),
        }
    }

    /// True when this link names the Hive at `origin`.
    pub fn is_for(&self, origin: &str) -> bool {
        self.address().origin() == origin.trim_end_matches('/').to_ascii_lowercase()
    }
}

/// Link v1 on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkV1 {
    schema: String,
    /// On disk as `repo` (pre-VA name): the file is committed, and engines
    /// before the rename (they refuse unknown fields) read it in the same
    /// team.
    #[serde(rename = "repo", alias = "vault")]
    vault: VaultRefV1,
}

/// Link v2 on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkV2 {
    schema: String,
    vault: String,
}

/// What `link` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkOutcome {
    pub file: PathBuf,
    /// A `.lean-ctx-id` had to be created.
    pub created_identity: bool,
}

/// The link of the project at `root`, if any. A linked git worktree without
/// its own link file uses the one of its main checkout: the same repository
/// (one git dir, one remote), so the same vault.
pub fn read_link(root: &Path) -> Result<Option<VaultLink>, HiveError> {
    if let Some(link) = read_link_file(root)? {
        return Ok(Some(link));
    }
    match main_worktree_root(root) {
        Some(main) => read_link_file(&main),
        None => Ok(None),
    }
}

/// The main checkout of the linked git worktree at `root`, read from git's
/// own files without running git: `<root>/.git` is a file naming the
/// worktree's git dir, whose `gitdir` points back at that file and whose
/// `commondir` names the repository's `.git` directory of the main checkout.
/// `None` for a main checkout, a bare repository, or files that do not
/// agree with each other.
pub fn main_worktree_root(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if !std::fs::symlink_metadata(&dot_git).ok()?.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    let gitdir = root.join(gitdir).canonicalize().ok()?;
    let back = std::fs::read_to_string(gitdir.join("gitdir")).ok()?;
    if gitdir.join(back.trim()).canonicalize().ok()? != dot_git.canonicalize().ok()? {
        return None;
    }
    let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let common = gitdir.join(common.trim()).canonicalize().ok()?;
    if common.file_name()? != ".git" {
        return None;
    }
    let main = common.parent()?;
    let main_dot_git = std::fs::symlink_metadata(main.join(".git")).ok()?;
    (main_dot_git.is_dir() && main != root.canonicalize().ok()?).then(|| main.to_path_buf())
}

fn read_link_file(root: &Path) -> Result<Option<VaultLink>, HiveError> {
    if let Some((path, text)) = read_named(root, LINK_FILE)? {
        return parse(&path, &text).map(Some);
    }
    let Some((path, text)) = read_named(root, LEGACY_LINK_FILE)? else {
        return Ok(None);
    };
    let link = parse(&path, &text)?;
    // Migrate on read: the Hive name gets the same bytes; the old file stays.
    // Best effort: a read-only checkout still reads the old name next time.
    let _ = write_file(&root.join(LINK_FILE), text.as_bytes());
    Ok(Some(link))
}

fn read_named(root: &Path, name: &str) -> Result<Option<(PathBuf, String)>, HiveError> {
    let path = root.join(name);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some((path, text))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(HiveError::Local(format!("read {}: {e}", path.display()))),
    }
}

fn parse(path: &Path, text: &str) -> Result<VaultLink, HiveError> {
    parse_link(text).map_err(|why| HiveError::Local(format!("{} {why}", path.display())))
}

/// Link v1 or v2 text → the link, or why it is refused.
fn parse_link(text: &str) -> Result<VaultLink, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("is damaged: {e}"))?;
    match value["schema"].as_str() {
        Some(LINK_SCHEMA) => {
            let link: LinkV1 =
                serde_json::from_value(value).map_err(|e| format!("is damaged: {e}"))?;
            link.vault.validate().map_err(|e| format!(": {e}"))?;
            Ok(VaultLink {
                schema: link.schema,
                vault: link.vault,
                server: None,
            })
        }
        Some(LINK_SCHEMA_V2) => {
            let link: LinkV2 =
                serde_json::from_value(value).map_err(|e| format!("is damaged: {e}"))?;
            let address = VaultAddress::parse(&link.vault).map_err(|e| format!(": {e}"))?;
            let vault = VaultRefV1 {
                ns: address.ws,
                name: address.vault,
            };
            vault.validate().map_err(|e| format!(": {e}"))?;
            Ok(VaultLink {
                schema: link.schema,
                vault,
                server: address.server,
            })
        }
        _ => Err("has an unknown schema".into()),
    }
}

/// The file text of a link: v1 for a hosted vault, v2 for a self-hosted one.
fn link_text(address: &VaultAddress) -> Result<String, HiveError> {
    let value = if address.is_hosted() {
        serde_json::to_value(LinkV1 {
            schema: LINK_SCHEMA.into(),
            vault: VaultRefV1 {
                ns: address.ws.clone(),
                name: address.vault.clone(),
            },
        })
    } else {
        serde_json::to_value(LinkV2 {
            schema: LINK_SCHEMA_V2.into(),
            vault: address.to_string(),
        })
    }
    .map_err(|e| HiveError::Local(e.to_string()))?;
    let mut text =
        serde_json::to_string_pretty(&value).map_err(|e| HiveError::Local(e.to_string()))?;
    text.push('\n');
    Ok(text)
}

/// Link the repository at `root` to `vault` (`<workspace>/<vault>[@<server>]`).
pub fn link(root: &Path, vault: &str) -> Result<LinkOutcome, HiveError> {
    let address = VaultAddress::parse(vault).map_err(|e| HiveError::Config(e.to_string()))?;
    VaultRefV1 {
        ns: address.ws.clone(),
        name: address.vault.clone(),
    }
    .validate()
    .map_err(|e| HiveError::Config(e.to_string()))?;
    let root_str = root.to_string_lossy();
    let created_identity = crate::core::project_hash::project_identity(&root_str).is_none();
    if created_identity {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| HiveError::Local(format!("random: {e}")))?;
        write_file(
            &root.join(IDENTITY_FILE),
            format!("{}\n", hex::encode(bytes)).as_bytes(),
        )?;
    }
    let text = link_text(&address)?;
    let file = root.join(LINK_FILE);
    write_file(&file, text.as_bytes())?;
    // An existing old-name file follows, so older engines in the team agree.
    let legacy = root.join(LEGACY_LINK_FILE);
    if legacy.exists() {
        write_file(&legacy, text.as_bytes())?;
    }
    Ok(LinkOutcome {
        file,
        created_identity,
    })
}

/// Write `path` in a checkout someone else may have prepared: the bytes are
/// staged in a fresh, never pre-existing temp file (create_new, random
/// name) and renamed over `path`, so a planted symlink at the temp or the
/// final name is replaced, never written through (RB8).
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), HiveError> {
    crate::core::atomic_fs::try_atomic_write(path, bytes, None)
        .map_err(|e| HiveError::Local(format!("write {}: {e}", path.display())))
}

/// A main checkout and a linked worktree laid out as `git worktree add`
/// writes them: `<main>/.git/worktrees/wt/{gitdir,commondir}` and
/// `<wt>/.git` (a file).
#[cfg(test)]
pub(crate) fn worktree_layout(base: &Path) -> (PathBuf, PathBuf) {
    let main = base.join("main");
    let wt = base.join("wt");
    let admin = main.join(".git").join("worktrees").join("wt");
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
    std::fs::write(
        admin.join("gitdir"),
        format!("{}\n", wt.join(".git").display()),
    )
    .unwrap();
    std::fs::write(admin.join("commondir"), "../..\n").unwrap();
    (main, wt)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RB8: a cloned repo with planted symlinks at the files `link` writes
    /// (or at their old temp names) never gets a file outside it changed.
    #[cfg(unix)]
    #[test]
    fn link_never_writes_through_a_planted_symlink() {
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("authorized_keys");
        std::fs::write(&victim, "ssh-ed25519 KEEP\n").unwrap();
        let repo = tempfile::tempdir().unwrap();
        for name in [
            LINK_FILE,
            IDENTITY_FILE,
            ".lean-ctx-hive.tmp",
            ".lean-ctx-id.tmp",
        ] {
            std::os::unix::fs::symlink(&victim, repo.path().join(name)).unwrap();
        }
        link(repo.path(), "acme/payments").unwrap();
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "ssh-ed25519 KEEP\n"
        );
        let written = std::fs::symlink_metadata(repo.path().join(LINK_FILE)).unwrap();
        assert!(written.is_file(), "the link file replaced the symlink");
        assert!(read_link(repo.path()).unwrap().is_some());
    }

    #[test]
    fn link_writes_the_repo_and_an_identity_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = link(dir.path(), "acme/payments").unwrap();
        assert!(outcome.created_identity);
        assert!(dir.path().join(IDENTITY_FILE).exists());
        let link_back = read_link(dir.path()).unwrap().unwrap();
        assert_eq!(
            (link_back.vault.ns.as_str(), link_back.vault.name.as_str()),
            ("acme", "payments")
        );

        let again = link(dir.path(), "acme/billing").unwrap();
        assert!(!again.created_identity, "the identity is kept");
        assert_eq!(
            read_link(dir.path()).unwrap().unwrap().vault.name,
            "billing"
        );
    }

    #[test]
    fn projects_with_an_identity_get_no_new_id_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        assert!(!link(dir.path(), "acme/demo").unwrap().created_identity);
        assert!(!dir.path().join(IDENTITY_FILE).exists());
    }

    /// `ProjectLink` of the engine before the VA rename.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct PreVaLink {
        schema: String,
        repo: VaultRefV1,
    }

    #[test]
    fn the_link_file_keeps_repo_so_older_engines_in_the_team_read_it() {
        let old = r#"{"schema":"leanctx.hive.link/v1","repo":{"ns":"acme","name":"payments"}}"#;
        let read = crate::core::hive::vault::assert_pre_va_round_trip::<LinkV1, PreVaLink>(
            old, "repo", "vault",
        );
        assert_eq!(
            (read.vault.ns.as_str(), read.vault.name.as_str()),
            ("acme", "payments")
        );

        let dir = tempfile::tempdir().unwrap();
        link(dir.path(), "acme/payments").unwrap();
        let text = std::fs::read_to_string(dir.path().join(LINK_FILE)).unwrap();
        serde_json::from_str::<PreVaLink>(&text).expect("an engine before the rename reads it");
        let back = read_link(dir.path()).unwrap().unwrap();
        assert_eq!((back.vault, back.server), (read.vault, None));
    }

    #[test]
    fn the_hive_link_file_is_read_and_migrated_to_the_hive_name() {
        let dir = tempfile::tempdir().unwrap();
        let old = "{\"schema\":\"leanctx.hive.link/v1\",\"repo\":{\"ns\":\"acme\",\"name\":\"payments\"}}\n";
        std::fs::write(dir.path().join(LEGACY_LINK_FILE), old).unwrap();
        assert!(has_link_file(dir.path()));
        let read = read_link(dir.path()).unwrap().unwrap();
        assert_eq!(read.vault.name, "payments");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(LINK_FILE)).unwrap(),
            old,
            "the Hive name gets the same bytes"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join(LEGACY_LINK_FILE)).unwrap(),
            old,
            "the old file stays"
        );
        // A new link keeps both names in step.
        link(dir.path(), "acme/billing").unwrap();
        for name in [LINK_FILE, LEGACY_LINK_FILE] {
            let text = std::fs::read_to_string(dir.path().join(name)).unwrap();
            assert!(text.contains("billing"), "{name}: {text}");
        }
    }

    #[test]
    fn a_worktree_without_a_link_uses_the_main_checkouts() {
        let base = tempfile::tempdir().unwrap();
        let (main, wt) = worktree_layout(base.path());
        assert_eq!(
            main_worktree_root(&wt).unwrap(),
            main.canonicalize().unwrap()
        );
        assert!(main_worktree_root(&main).is_none(), "the main checkout");
        assert!(read_link(&wt).unwrap().is_none(), "nothing linked yet");

        link(&main, "acme/payments").unwrap();
        assert_eq!(read_link(&wt).unwrap().unwrap().vault.name, "payments");
        // The worktree's own link wins.
        link(&wt, "acme/billing").unwrap();
        assert_eq!(read_link(&wt).unwrap().unwrap().vault.name, "billing");
        // A damaged link of the main checkout is still refused.
        std::fs::remove_file(wt.join(LINK_FILE)).unwrap();
        std::fs::write(main.join(LINK_FILE), "{").unwrap();
        assert!(read_link(&wt).is_err());
    }

    #[test]
    fn a_dot_git_file_that_git_does_not_back_is_not_followed() {
        let base = tempfile::tempdir().unwrap();
        let (main, wt) = worktree_layout(base.path());
        link(&main, "acme/payments").unwrap();
        // A second checkout claims the same worktree admin dir: its back
        // pointer names the other checkout, so it gets nothing.
        let impostor = base.path().join("impostor");
        std::fs::create_dir_all(&impostor).unwrap();
        std::fs::copy(wt.join(".git"), impostor.join(".git")).unwrap();
        assert!(main_worktree_root(&impostor).is_none());
        assert!(read_link(&impostor).unwrap().is_none());
        // A bare common dir has no main checkout.
        let admin = main.join(".git/worktrees/wt");
        std::fs::write(admin.join("commondir"), "..\n").unwrap();
        assert!(main_worktree_root(&wt).is_none());
    }

    #[test]
    fn malformed_vault_names_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(link(dir.path(), "payments").is_err());
        assert!(link(dir.path(), "acme/../etc").is_err());
        assert!(link(dir.path(), "acme/payments@localhost").is_err());
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/link/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    /// C0 2.0 `link/`: v2 carries the address, v1 is read as hosted, and
    /// every invalid case is refused.
    #[test]
    fn link_files_match_the_contract_fixtures() {
        let hosted = parse_link(&fixture("lean-ctx-hive.hosted.json")).unwrap();
        assert_eq!(hosted.address().to_string(), "acme/payments");
        assert!(hosted.is_for("https://hive.leanctx.com"));
        let own = parse_link(&fixture("lean-ctx-hive.self_hosted.json")).unwrap();
        assert_eq!(own.server.as_deref(), Some("hive.acme.example:8443"));
        assert!(own.is_for("https://hive.acme.example:8443/"));
        assert!(!own.is_for("https://hive.leanctx.com"));
        let v1 = parse_link(&fixture("lean-ctx-hive.v1.json")).unwrap();
        assert_eq!((v1.vault, v1.server), (hosted.vault.clone(), None));
        let dir = format!(
            "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/link/invalid",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut refused = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let case: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap())
                    .unwrap();
            let base = case["base"]
                .as_str()
                .unwrap()
                .strip_prefix("link/")
                .unwrap();
            let mut doc: serde_json::Value = serde_json::from_str(&fixture(base)).unwrap();
            for op in case["patch"].as_array().unwrap() {
                assert_eq!(op["op"], "replace", "{case}");
                *doc.pointer_mut(op["path"].as_str().unwrap()).unwrap() = op["value"].clone();
            }
            assert!(parse_link(&doc.to_string()).is_err(), "{case}");
            refused += 1;
        }
        assert!(refused > 0);
    }

    #[test]
    fn a_self_hosted_vault_is_written_as_link_v2() {
        let dir = tempfile::tempdir().unwrap();
        link(dir.path(), "acme/payments@Hive.Acme.Example:8443").unwrap();
        let text = std::fs::read_to_string(dir.path().join(LINK_FILE)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"schema": LINK_SCHEMA_V2, "vault": "acme/payments@hive.acme.example:8443"})
        );
        link(dir.path(), "acme/payments@hive.leanctx.com").unwrap();
        let back = read_link(dir.path()).unwrap().unwrap();
        assert_eq!((back.schema.as_str(), back.server), (LINK_SCHEMA, None));
    }
}
