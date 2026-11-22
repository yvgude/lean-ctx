// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx cloud decisions push`: share the repo's decision records with
//! the team in LeanCTX Cloud (team knowledge W2, owner order 18.11.2026).
//!
//! The engine sends the whole `.leanctx/decisions/*.toml` directory of a
//! project linked to a Cloud vault as `PUT /v1/vaults/{vault_id}/decisions`
//! (Cloud API §16, schema `leanctx.cloud.decisions-sync/v1`) with its
//! instance token. The Cloud admits it with `knowledge.draft`, applies the
//! team policy and writes `decision.synced` to the team's audit log.
//!
//! The sync replaces the vault's records with the set sent, so a push is
//! refused when the directory is missing or empty (unless `--allow-empty`)
//! and when any record is kept back: either would remove Cloud copies.
//!
//! Only decision records leave the machine, never code: regular `.toml`
//! files directly in that directory, inside the project (a symlinked file or
//! directory is refused), at most [`MAX_FILES`] of at most 64 KiB
//! ([`lean_ctx::core::decisions::MAX_DECISION_BYTES`]) each. A record in which the secret scanner finds a credential is kept
//! back and reported. Personal accounts keep their own memory: an instance
//! enrolled in a personal account sends nothing.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

/// Where a project keeps its decision records (the decision layer contract).
pub(crate) use lean_ctx::core::decisions::DECISIONS_DIR;
/// The shared record reader: no symlink followed, bounded by
/// [`lean_ctx::core::decisions::MAX_DECISION_BYTES`] (the Cloud's limit too).
pub(crate) use lean_ctx::core::decisions::read_record;

/// The Cloud's schema of a decisions sync.
pub(crate) const SYNC_SCHEMA: &str = "leanctx.cloud.decisions-sync/v1";
/// Files one sync carries at most (the Cloud's limit).
pub(crate) const MAX_FILES: usize = 500;
/// Largest Cloud answer read.
const MAX_ANSWER_BYTES: usize = 1024 * 1024;

const USAGE: &str =
    "Usage: lean-ctx cloud decisions push [--project-root P] [--dry-run] [--allow-empty] [--json]
  Share the project's decision records (.leanctx/decisions/*.toml) with your
  team in LeanCTX Cloud. Only those records are sent, never code. Requires a
  team account and a project linked to a Cloud vault.
  A push replaces the vault's records with this set: it is refused when the
  directory is missing or empty (unless --allow-empty) and when any record is
  kept back. --dry-run runs the same account and vault checks, sends nothing.
  The revision sent is HEAD, or none when the records have uncommitted changes.";

/// One record to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyncFile {
    pub path: String,
    pub content: String,
}

/// A record kept back, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Held {
    pub path: String,
    pub reason: &'static str,
}

/// What a push would send and what it keeps back.
#[derive(Debug, Default)]
pub(crate) struct Collected {
    pub files: Vec<SyncFile>,
    pub held: Vec<Held>,
    /// The project has no `.leanctx/decisions` directory.
    pub dir_missing: bool,
}

/// `lean-ctx cloud decisions …`; `args` follow `decisions`.
pub(crate) fn run(args: &[String]) {
    if args.first().map(String::as_str) != Some("push") {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    let code = push(args.get(1..).unwrap_or_default());
    if code != 0 {
        std::process::exit(code);
    }
}

/// The parsed flags of `push`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Options {
    pub project_root: Option<String>,
    pub dry_run: bool,
    pub json: bool,
    pub allow_empty: bool,
    pub help: bool,
}

/// Parse the flags of `push`; a flag without its value or an unknown
/// argument is an error, never ignored.
pub(crate) fn parse_args(args: &[String]) -> Result<Options, String> {
    const NEEDS_VALUE: &str = "--project-root needs a value";
    let mut o = Options::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" | "--help" => o.help = true,
            "--json" => o.json = true,
            "--dry-run" => o.dry_run = true,
            "--allow-empty" => o.allow_empty = true,
            "--project-root" => {
                let v = args
                    .get(i + 1)
                    .filter(|v| !v.is_empty() && !v.starts_with("--"))
                    .ok_or(NEEDS_VALUE)?;
                o.project_root = Some(v.clone());
                i += 1;
            }
            _ => match a.strip_prefix("--project-root=") {
                Some("") => return Err(NEEDS_VALUE.to_string()),
                Some(v) => o.project_root = Some(v.to_string()),
                None => return Err(format!("unknown argument {a}\n{USAGE}")),
            },
        }
        i += 1;
    }
    Ok(o)
}

/// The canonical project root: `--project-root` (relative to `cwd`), else
/// the root the other cloud commands use for `cwd`. Collection and the vault
/// lookup both take this one path.
pub(crate) fn resolve_root(explicit: Option<&str>, cwd: &Path) -> Result<PathBuf, String> {
    let raw = match explicit {
        Some(p) => cwd.join(p),
        None => PathBuf::from(lean_ctx::core::protocol::detect_project_root_or_cwd(
            &cwd.to_string_lossy(),
        )),
    };
    std::fs::canonicalize(&raw).map_err(|e| format!("project root {}: {e}", raw.display()))
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// The commit the sent records match: `HEAD`, or none when the records have
/// uncommitted changes (the working tree is what is sent).
fn revision(root: &Path) -> Option<String> {
    if git(root, &["status", "--porcelain", "--", DECISIONS_DIR]).is_some() {
        return None;
    }
    git(root, &["rev-parse", "HEAD"])
}

/// Read the records under `root` that may leave the machine, through the
/// decision layer's shared collector ([`lean_ctx::core::decisions::list_records`]):
/// the local loader refuses exactly the same entries.
pub(crate) fn collect(root: &Path) -> Result<Collected, String> {
    let root =
        std::fs::canonicalize(root).map_err(|e| format!("project root {}: {e}", root.display()))?;
    let Some(entries) = lean_ctx::core::decisions::list_records(&root)? else {
        return Ok(Collected {
            dir_missing: true,
            ..Collected::default()
        });
    };
    let mut out = Collected::default();
    for entry in entries {
        let rel = entry.rel;
        let held = |reason| Held {
            path: rel.clone(),
            reason,
        };
        if !entry.regular {
            out.held.push(held("not a regular file"));
            continue;
        }
        let content = match read_record(&entry.path) {
            Ok(c) => c,
            Err(reason) => {
                out.held.push(held(reason));
                continue;
            }
        };
        if !lean_ctx::core::secret_detection::detect_secrets(&content).is_empty() {
            out.held.push(held("contains a secret"));
            continue;
        }
        if out.files.len() == MAX_FILES {
            out.held.push(held("more than 500 records"));
            continue;
        }
        out.files.push(SyncFile { path: rel, content });
    }
    Ok(out)
}

/// Why this set must not be pushed. The sync replaces the vault's records
/// with the set sent and the Cloud contract has no keep list, so a missing
/// or empty directory would remove every team record and a kept-back record
/// would lose its Cloud copy.
pub(crate) fn refuse_push(c: &Collected, root: &Path, allow_empty: bool) -> Option<String> {
    if !c.held.is_empty() {
        let names: Vec<String> = c
            .held
            .iter()
            .map(|h| format!("{} ({})", h.path, h.reason))
            .collect();
        return Some(format!(
            "{} record(s) kept back: {}. A push replaces the vault's records, so their \
             Cloud copies would be removed; fix or move them and push again",
            c.held.len(),
            names.join(", ")
        ));
    }
    if c.files.is_empty() && !allow_empty {
        let what = if c.dir_missing {
            format!("no {DECISIONS_DIR} directory in {}", root.display())
        } else {
            format!("{DECISIONS_DIR} in {} holds no records", root.display())
        };
        return Some(format!(
            "{what}: a push would remove every decision record of the linked vault in Cloud. \
             Check --project-root, or pass --allow-empty to clear them on purpose"
        ));
    }
    None
}

/// The body of `PUT /v1/vaults/{id}/decisions`.
pub(crate) fn sync_body(revision: Option<&str>, files: &[SyncFile]) -> Value {
    json!({
        "schema": SYNC_SCHEMA,
        "revision": revision,
        "files": files
            .iter()
            .map(|f| json!({ "path": f.path, "content": f.content }))
            .collect::<Vec<_>>(),
    })
}

/// Why an instance may not share decisions: personal accounts keep their
/// own memory (an older enrollment that named no kind is left to the Cloud).
pub(crate) fn refuse_kind(org_kind: Option<&str>) -> Option<&'static str> {
    (org_kind == Some("personal")).then_some(
        "decisions are shared with a team: this device is connected to a personal account, \
         which keeps its own memory",
    )
}

fn push(args: &[String]) -> i32 {
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(e) => return fail(args.iter().any(|a| a == "--json"), &e),
    };
    if opts.help {
        println!("{USAGE}");
        return 0;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = match resolve_root(opts.project_root.as_deref(), &cwd) {
        Ok(r) => r,
        Err(e) => return fail(opts.json, &e),
    };
    execute(&opts, &root, preflight, send)
}

/// Collect, refuse an unsafe set, run the account and vault checks (also on
/// `--dry-run`), then send unless it is a dry run.
pub(crate) fn execute<T>(
    opts: &Options,
    root: &Path,
    preflight: impl FnOnce(&Path) -> Result<T, String>,
    send: impl FnOnce(T, &Value) -> Result<Value, String>,
) -> i32 {
    let collected = match collect(root) {
        Ok(c) => c,
        Err(e) => return fail(opts.json, &e),
    };
    if let Some(why) = refuse_push(&collected, root, opts.allow_empty) {
        return fail(opts.json, &why);
    }
    let target = match preflight(root) {
        Ok(t) => t,
        Err(e) => return fail(opts.json, &e),
    };
    if opts.dry_run {
        return report(opts.json, &collected, None, true);
    }
    let body = sync_body(revision(root).as_deref(), &collected.files);
    match send(target, &body) {
        Ok(ack) => report(opts.json, &collected, Some(&ack), false),
        Err(e) => fail(opts.json, &e),
    }
}

/// A checked destination: a team instance and the vault the root links to.
struct Target {
    store: lean_ctx_cloud::CloudStore,
    vault_id: String,
}

fn cloud_text(e: &lean_ctx_cloud::CloudError) -> String {
    e.to_string()
}

fn preflight(root: &Path) -> Result<Target, String> {
    let store = lean_ctx_cloud::CloudStore::open().map_err(|e| cloud_text(&e))?;
    let instance = store
        .instance()
        .map_err(|e| cloud_text(&e))?
        .ok_or("this device is not connected to LeanCTX Cloud (lean-ctx cloud connect)")?;
    if let Some(why) = refuse_kind(instance.org_kind.as_deref()) {
        return Err(why.to_string());
    }
    let record = lean_ctx_cloud::resolution::vault_for_root(store.data_dir(), root)
        .map_err(|e| cloud_text(&e))?
        .ok_or("this project is not linked to a LeanCTX Cloud vault")?;
    Ok(Target {
        vault_id: record.vault_id,
        store,
    })
}

fn send(target: Target, body: &Value) -> Result<Value, String> {
    let instance = target
        .store
        .instance()
        .map_err(|e| cloud_text(&e))?
        .ok_or("this device is not connected to LeanCTX Cloud (lean-ctx cloud connect)")?;
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    let trust = lean_ctx_cloud::root_cert::trust_root(&target.store, &config, &instance.url)
        .map_err(|e| cloud_text(&e))?;
    let session =
        lean_ctx_cloud::auth::Session::load(target.store, trust).map_err(|e| cloud_text(&e))?;
    let path = format!("/vaults/{}/decisions", target.vault_id);
    session
        .with_client(|client| {
            client
                .send("PUT", &path, Some(body), &[], MAX_ANSWER_BYTES)
                .map(|r| r.body)
        })
        .map_err(|e| cloud_text(&e))
}

fn report(json_out: bool, c: &Collected, ack: Option<&Value>, dry_run: bool) -> i32 {
    let held: Vec<Value> = c
        .held
        .iter()
        .map(|h| json!({ "path": h.path, "reason": h.reason }))
        .collect();
    if json_out {
        let paths: Vec<&str> = c.files.iter().map(|f| f.path.as_str()).collect();
        println!(
            "{}",
            json!({ "dry_run": dry_run, "sent": paths, "held": held, "cloud": ack })
        );
        return 0;
    }
    if dry_run {
        println!(
            "Would share {} decision record(s) with the team (account and vault checked, nothing sent).",
            c.files.len()
        );
    } else {
        println!("Shared {} decision record(s) with the team.", c.files.len());
    }
    if let Some(a) = ack {
        let n = |k: &str| a.get(k).and_then(Value::as_u64).unwrap_or(0);
        println!(
            "Cloud: {} added, {} updated, {} unchanged, {} removed.",
            n("added"),
            n("updated"),
            n("unchanged"),
            n("removed")
        );
        if let Some(refused) = a.get("refused").and_then(Value::as_array) {
            for r in refused {
                println!(
                    "  refused {}: {}",
                    r.get("path").and_then(Value::as_str).unwrap_or("?"),
                    r.get("message").and_then(Value::as_str).unwrap_or("")
                );
            }
        }
    }
    for h in &c.held {
        println!("  kept back {}: {}", h.path, h.reason);
    }
    0
}

fn fail(json_out: bool, message: &str) -> i32 {
    if json_out {
        println!("{}", json!({ "error": message }));
    } else {
        eprintln!("lean-ctx cloud decisions push: {message}");
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(DECISIONS_DIR)).unwrap();
        dir
    }

    fn write(root: &Path, name: &str, text: &str) {
        std::fs::write(root.join(DECISIONS_DIR).join(name), text).unwrap();
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    /// Run `execute` with stub checks; returns (exit code, preflight ran,
    /// body sent).
    fn run_with(opts: &Options, root: &Path, preflight_ok: bool) -> (i32, bool, Option<Value>) {
        let checked = Cell::new(false);
        let sent = std::cell::RefCell::new(None);
        let code = execute(
            opts,
            root,
            |_| {
                checked.set(true);
                if preflight_ok {
                    Ok(())
                } else {
                    Err("this project is not linked to a LeanCTX Cloud vault".to_string())
                }
            },
            |(), body| {
                *sent.borrow_mut() = Some(body.clone());
                Ok(json!({ "added": 0 }))
            },
        );
        (code, checked.get(), sent.into_inner())
    }

    #[test]
    fn collects_only_toml_records_in_order() {
        let p = project();
        write(p.path(), "dec-b.toml", "id = \"dec-b\"\n");
        write(p.path(), "dec-a.toml", "id = \"dec-a\"\n");
        write(p.path(), "notes.md", "not a record");
        std::fs::write(p.path().join("main.rs"), "fn main() {}").unwrap();
        let c = collect(p.path()).unwrap();
        let paths: Vec<&str> = c.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                ".leanctx/decisions/dec-a.toml",
                ".leanctx/decisions/dec-b.toml"
            ]
        );
        assert!(c.held.is_empty());
    }

    #[test]
    fn missing_directory_never_sends_an_empty_set() {
        let dir = tempfile::tempdir().unwrap();
        let c = collect(dir.path()).unwrap();
        assert!(c.dir_missing && c.files.is_empty() && c.held.is_empty());
        let why = refuse_push(&c, dir.path(), false).unwrap();
        assert!(why.contains("no .leanctx/decisions directory"), "{why}");
        assert!(why.contains("--allow-empty"), "{why}");

        let (code, checked, sent) = run_with(&Options::default(), dir.path(), true);
        assert_eq!(code, 1);
        assert!(!checked);
        assert!(sent.is_none(), "no body with files: [] may be produced");
    }

    #[test]
    fn empty_directory_is_refused_unless_allowed() {
        let p = project();
        let c = collect(p.path()).unwrap();
        assert!(!c.dir_missing);
        assert!(
            refuse_push(&c, p.path(), false)
                .unwrap()
                .contains("holds no records")
        );
        assert_eq!(run_with(&Options::default(), p.path(), true).2, None);

        let allow = Options {
            allow_empty: true,
            ..Options::default()
        };
        let (code, _, sent) = run_with(&allow, p.path(), true);
        assert_eq!(code, 0);
        assert_eq!(sent.unwrap()["files"], json!([]));
    }

    #[test]
    fn large_files_and_secrets_are_kept_back() {
        let p = project();
        write(p.path(), "dec-big.toml", &"x".repeat(64 * 1024 + 1));
        write(
            p.path(),
            "dec-key.toml",
            concat!("statement = \"use AK", "IAIOSFODNN7EXAMPLE\"\n"),
        );
        let c = collect(p.path()).unwrap();
        assert!(c.files.is_empty());
        let reasons: Vec<&str> = c.held.iter().map(|h| h.reason).collect();
        assert_eq!(reasons, ["larger than 64 KiB", "contains a secret"]);
    }

    #[test]
    fn held_records_fail_the_push_and_are_named() {
        let p = project();
        write(p.path(), "dec-a.toml", "id = \"dec-a\"\n");
        write(p.path(), "dec-big.toml", &"x".repeat(64 * 1024 + 1));
        let c = collect(p.path()).unwrap();
        let why = refuse_push(&c, p.path(), true).unwrap();
        assert!(
            why.contains(".leanctx/decisions/dec-big.toml (larger than 64 KiB)"),
            "{why}"
        );
        for dry_run in [false, true] {
            let opts = Options {
                dry_run,
                allow_empty: true,
                ..Options::default()
            };
            let (code, _, sent) = run_with(&opts, p.path(), true);
            assert_eq!(code, 1);
            assert!(sent.is_none());
        }
    }

    #[test]
    fn dry_run_runs_the_account_and_vault_checks() {
        let p = project();
        write(p.path(), "dec-a.toml", "id = \"dec-a\"\n");
        let dry = Options {
            dry_run: true,
            ..Options::default()
        };
        let (code, checked, sent) = run_with(&dry, p.path(), false);
        assert_eq!(code, 1);
        assert!(checked);
        assert!(sent.is_none());

        let (code, checked, sent) = run_with(&dry, p.path(), true);
        assert_eq!(code, 0);
        assert!(checked);
        assert!(sent.is_none());

        let (code, _, sent) = run_with(&Options::default(), p.path(), true);
        assert_eq!(code, 0);
        assert_eq!(
            sent.unwrap()["files"][0]["path"],
            ".leanctx/decisions/dec-a.toml"
        );
    }

    #[test]
    fn read_record_is_bounded() {
        let p = project();
        write(p.path(), "ok.toml", "id = \"ok\"\n");
        write(p.path(), "big.toml", &"x".repeat(64 * 1024 + 1));
        std::fs::write(p.path().join(DECISIONS_DIR).join("bin.toml"), [0xff, 0xfe]).unwrap();
        let dir = p.path().join(DECISIONS_DIR);
        assert_eq!(read_record(&dir.join("ok.toml")).unwrap(), "id = \"ok\"\n");
        assert_eq!(
            read_record(&dir.join("big.toml")),
            Err("larger than 64 KiB")
        );
        assert_eq!(read_record(&dir.join("bin.toml")), Err("not UTF-8 text"));
    }

    #[cfg(unix)]
    #[test]
    fn read_record_never_follows_a_symlink() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.toml"), "id = \"x\"").unwrap();
        let p = project();
        let link = p.path().join(DECISIONS_DIR).join("dec-link.toml");
        std::os::unix::fs::symlink(outside.path().join("secret.toml"), &link).unwrap();
        assert_eq!(read_record(&link), Err("not a regular file"));
        let dir = p.path().join(DECISIONS_DIR);
        assert_eq!(read_record(&dir), Err("not a regular file"));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_never_carry_files_from_outside() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.toml"), "id = \"x\"").unwrap();
        let p = project();
        std::os::unix::fs::symlink(
            outside.path().join("secret.toml"),
            p.path().join(DECISIONS_DIR).join("dec-link.toml"),
        )
        .unwrap();
        let c = collect(p.path()).unwrap();
        assert!(c.files.is_empty());
        assert_eq!(c.held[0].reason, "not a regular file");

        let q = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(q.path().join(".leanctx")).unwrap();
        std::os::unix::fs::symlink(outside.path(), q.path().join(DECISIONS_DIR)).unwrap();
        assert!(collect(q.path()).is_err());
    }

    #[test]
    fn explicit_root_is_canonicalised() {
        let p = project();
        let canonical = std::fs::canonicalize(p.path()).unwrap();
        std::fs::create_dir_all(p.path().join("sub")).unwrap();
        let from_sub = resolve_root(Some(".."), &p.path().join("sub")).unwrap();
        assert_eq!(from_sub, canonical);
        assert_eq!(resolve_root(Some("."), p.path()).unwrap(), canonical);
        assert!(resolve_root(Some("no-such-dir"), p.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_root_resolves_to_its_target() {
        let p = project();
        let links = tempfile::tempdir().unwrap();
        let link = links.path().join("proj");
        std::os::unix::fs::symlink(p.path(), &link).unwrap();
        assert_eq!(
            resolve_root(Some(link.to_str().unwrap()), links.path()).unwrap(),
            std::fs::canonicalize(p.path()).unwrap()
        );
    }

    #[test]
    fn flags_are_strict() {
        let o = parse_args(&args(&[
            "--dry-run",
            "--json",
            "--allow-empty",
            "--project-root",
            "repo",
        ]))
        .unwrap();
        assert!(o.dry_run && o.json && o.allow_empty && !o.help);
        assert_eq!(o.project_root.as_deref(), Some("repo"));
        assert_eq!(
            parse_args(&args(&["--project-root=repo"]))
                .unwrap()
                .project_root
                .as_deref(),
            Some("repo")
        );
        for bad in [
            &["--project-root"][..],
            &["--project-root", "--json"][..],
            &["--project-root="][..],
        ] {
            assert_eq!(
                parse_args(&args(bad)).unwrap_err(),
                "--project-root needs a value"
            );
        }
        let err = parse_args(&args(&["--dryrun"])).unwrap_err();
        assert!(err.starts_with("unknown argument --dryrun"), "{err}");
        assert!(parse_args(&args(&["extra"])).is_err());
    }

    #[test]
    fn body_matches_the_cloud_sync_schema() {
        let files = [SyncFile {
            path: ".leanctx/decisions/dec-a.toml".into(),
            content: "id = \"dec-a\"\n".into(),
        }];
        let body = sync_body(Some("abc123"), &files);
        assert_eq!(body["schema"], "leanctx.cloud.decisions-sync/v1");
        assert_eq!(body["revision"], "abc123");
        assert_eq!(body["files"][0]["path"], ".leanctx/decisions/dec-a.toml");
        assert_eq!(sync_body(None, &[])["revision"], Value::Null);
    }

    #[test]
    fn personal_accounts_keep_their_own_memory() {
        assert!(refuse_kind(Some("personal")).is_some());
        assert!(refuse_kind(Some("team")).is_none());
        assert!(refuse_kind(None).is_none());
    }
}
