// SPDX-License-Identifier: Apache-2.0
//! Personal Pro rule preparation; published safety rules remain ordinary policy data.
#[cfg(windows)]
use crate::core::windows_private::{Directory, Privacy};
use crate::core::{intelligence_runtime::personal_protection, policy};
use serde_json::json;
use sha2::{Digest, Sha256};
#[cfg(not(windows))]
use std::io::Write;
use std::{fs, io::Read, path::Path};

const PREFIX: &str = "personal_";
const ERROR: &str = "personal policy operation refused";

pub(super) fn run(args: &[String]) {
    match execute(args) {
        Ok(value) => println!("{value}"),
        Err(message) => {
            eprintln!("policy personal: {message}");
            std::process::exit(1);
        }
    }
}

fn execute(args: &[String]) -> Result<serde_json::Value, &'static str> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(HELP);
    };
    if matches!(command, "-h" | "--help") {
        return Ok(json!({"usage":HELP}));
    }
    let root = std::env::current_dir().map_err(|_| ERROR)?;
    #[cfg(windows)]
    let _root_guard = Directory::open(&root, Privacy::Writable).map_err(|_| ERROR)?;
    #[cfg(not(windows))]
    verify_directory(&root, false)?;
    let directory = root.join(".lean-ctx");
    #[cfg(windows)]
    let _existing_guard = windows_directory(&directory, false)?;
    #[cfg(not(windows))]
    verify_directory(&directory, false)?;
    let path = directory.join("policy.toml");
    if command == "status" && args.len() == 1 {
        let current = read_current(&path)?;
        return Ok(
            json!({"schema_version":1,"policy_sha256":current.as_deref().map(digest),
            "pro_required_for_changes":true,"published_protection_requires_license":false}),
        );
    }
    if !matches!(command, "preview" | "apply") || args.len() < 2 {
        return Err(HELP);
    }
    let mut expected = None;
    let mut sample = false;
    match &args[2..] {
        [] => {}
        [flag] if command == "preview" && flag == "--stdin" => sample = true,
        [flag, hash]
            if command == "apply"
                && flag == "--expected-sha256"
                && hash.len() == 64
                && hash.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            expected = Some(hash.as_str());
        }
        _ => return Err(HELP),
    }
    let raw = policy::files::read_protected(Path::new(&args[1]), true)
        .map_err(|_| ERROR)?
        .ok_or(ERROR)?;
    let compiled = personal_protection::compile(&raw)
        .map_err(|_| "Pro rule preparation unavailable; existing protection is unchanged")?;
    if command == "preview" {
        return policy::runtime::with_project_source_view(root.to_str().ok_or(ERROR)?, || {
            let current = read_current(&path)?;
            let pack = merged(current.as_deref(), &compiled)?;
            let active = effective(&pack)?;
            let mut result = json!({"schema_version":1,"rule_count":compiled.rule_count,
            "mask_rules":compiled.redaction.len(),"block_rules":compiled.blocked_patterns.len(),
            "base_policy_sha256":current.as_deref().map(digest),"published":false});
            if sample {
                let mut text = String::new();
                std::io::stdin()
                    .take(policy::files::MAX_BYTES + 1)
                    .read_to_string(&mut text)
                    .map_err(|_| ERROR)?;
                if text.len() as u64 > policy::files::MAX_BYTES {
                    return Err(ERROR);
                }
                let mut outcome = policy::content::evaluate_text(&text, &active);
                if let Some(current) = policy::runtime::active() {
                    let original = policy::content::evaluate_text(&text, &current);
                    if original.blocked {
                        outcome = original;
                    } else if !outcome.blocked {
                        // A preview cannot release material withheld by the current
                        // policy. Apply original blocking before inspecting rewritten
                        // text, just like the shared data boundary.
                        let retained = policy::content::evaluate_text(&outcome.text, &current);
                        if retained.blocked {
                            outcome = retained;
                        } else {
                            outcome.text = retained.text;
                            outcome.audit.extend(retained.audit);
                        }
                    }
                }
                result["preview"] = json!({"blocked":outcome.blocked,"text":outcome.text,
                "reason":outcome.block_reason,"decisions":outcome.audit});
            }
            Ok(result)
        })
        .map_err(|_| ERROR)?;
    }
    #[cfg(windows)]
    let _publication_guard = windows_directory(&directory, true)?.ok_or(ERROR)?;
    #[cfg(not(windows))]
    verify_directory(&directory, true)?;
    let _lock = lock(&directory)?;
    let current = read_current(&path)?;
    match (&current, expected) {
        (None, None) => {}
        (Some(text), Some(expected)) if digest(text).eq_ignore_ascii_case(expected) => {}
        _ => {
            return Err(
                "policy revision changed or expected revision missing; inspect 'policy personal status'",
            );
        }
    }
    let pack = merged(current.as_deref(), &compiled)?;
    effective(&pack)?;
    let text = toml::to_string_pretty(&pack).map_err(|_| ERROR)?;
    if text.len() as u64 > policy::files::MAX_BYTES {
        return Err(ERROR);
    }
    // Recheck the exact policy before publication while holding the cooperative
    // writer lock; external editors must provide the expected content revision.
    if read_current(&path)? != current {
        return Err(ERROR);
    }
    publish(&path, text.as_bytes(), current.is_none())?;
    Ok(
        json!({"schema_version":1,"published":true,"rule_count":compiled.rule_count,
        "policy_sha256":digest(&text),"protection_survives_license_expiry":true}),
    )
}

const HELP: &str = "policy personal status | preview <rules.json> [--stdin] | apply <rules.json> [--expected-sha256 <current-policy-digest>]";

fn digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

fn merged(
    current: Option<&str>,
    compiled: &personal_protection::Compiled,
) -> Result<policy::PolicyPack, &'static str> {
    let mut pack = match current {
        Some(text) => policy::parse(text).map_err(|_| ERROR)?,
        None => policy::parse("name = \"personal-protection\"\nversion = \"1.0.0\"\ndescription = \"Personal protection rules\"").map_err(|_| ERROR)?,
    };
    // Only recorded ownership permits replacement. A legacy hand-authored
    // personal_* rule must not disappear just because its name shares a prefix.
    for id in std::mem::take(&mut pack.filters.managed_personal_rules) {
        pack.redaction.remove(&id);
        pack.filters.blocked_patterns.remove(&id);
    }
    for id in compiled
        .redaction
        .keys()
        .chain(compiled.blocked_patterns.keys())
    {
        let name = format!("{PREFIX}{id}");
        if pack.redaction.contains_key(&name) || pack.filters.blocked_patterns.contains_key(&name) {
            return Err("personal rule ID conflicts with an existing unmanaged rule");
        }
        pack.filters.managed_personal_rules.insert(name);
    }
    pack.redaction.extend(
        compiled
            .redaction
            .iter()
            .map(|(id, pattern)| (format!("{PREFIX}{id}"), pattern.clone())),
    );
    pack.filters.blocked_patterns.extend(
        compiled
            .blocked_patterns
            .iter()
            .map(|(id, pattern)| (format!("{PREFIX}{id}"), pattern.clone())),
    );
    policy::validate(&pack).map_err(|_| ERROR)?;
    Ok(pack)
}

fn effective(pack: &policy::PolicyPack) -> Result<policy::runtime::ActivePolicy, &'static str> {
    let local = policy::resolve(pack).map_err(|_| ERROR)?;
    let resolved = match policy::org::active_resolved_checked().map_err(|_| ERROR)? {
        Some(org) => policy::floor::merge_floor(&org, Some(&local)),
        None => local,
    };
    let active = policy::runtime::ActivePolicy::from_resolved(resolved);
    if !active.content_valid {
        return Err(ERROR);
    }
    Ok(active)
}

fn read_current(path: &Path) -> Result<Option<String>, &'static str> {
    #[cfg(windows)]
    {
        let Some(directory) = windows_directory(path.parent().ok_or(ERROR)?, false)? else {
            return Ok(None);
        };
        match directory.read_file("policy.toml", policy::files::MAX_BYTES as usize) {
            Ok(bytes) => String::from_utf8(bytes).map(Some).map_err(|_| ERROR),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(ERROR),
        }
    }
    #[cfg(not(windows))]
    policy::files::read_protected(path, false).map_err(|_| ERROR)
}

#[cfg(windows)]
fn windows_directory(path: &Path, create: bool) -> Result<Option<Directory>, &'static str> {
    match Directory::open(path, Privacy::Writable) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if create {
                Directory::create(path).map(Some).map_err(|_| ERROR)
            } else {
                Ok(None)
            }
        }
        Err(_) => Err(ERROR),
    }
}

#[cfg(not(windows))]
fn verify_directory(path: &Path, create: bool) -> Result<(), &'static str> {
    if create {
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let mut builder = builder;
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(ERROR),
        }
    }
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => verify_owner(&meta),
        Err(e) if !create && e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(ERROR),
    }
}

#[cfg(not(windows))]
fn verify_owner(metadata: &fs::Metadata) -> Result<(), &'static str> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no arguments or preconditions and only reads identity.
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            return Err(
                "personal policy storage must be owned by this user and not writable by others",
            );
        }
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Err("personal policy storage validation unavailable")
    }
    #[cfg(unix)]
    Ok(())
}

#[cfg(not(windows))]
fn lock(directory: &Path) -> Result<fs::File, &'static str> {
    use fs2::FileExt;
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options
        .open(directory.join("personal-policy.lock"))
        .map_err(|_| ERROR)?;
    let metadata = file.metadata().map_err(|_| ERROR)?;
    if !metadata.is_file() {
        return Err(ERROR);
    }
    verify_owner(&metadata)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(ERROR);
        }
    }
    file.try_lock_exclusive()
        .map_err(|_| "another personal policy publication is active")?;
    Ok(file)
}

#[cfg(windows)]
fn lock(directory: &Path) -> Result<fs::File, &'static str> {
    use fs2::FileExt;
    let directory = Directory::open(directory, Privacy::Writable).map_err(|_| ERROR)?;
    let file = directory
        .open_lock("personal-policy.lock")
        .map_err(|_| ERROR)?;
    file.try_lock_exclusive()
        .map_err(|_| "another personal policy publication is active")?;
    Ok(file)
}

#[cfg(windows)]
fn publish(path: &Path, bytes: &[u8], new: bool) -> Result<(), &'static str> {
    let directory =
        Directory::open(path.parent().ok_or(ERROR)?, Privacy::Writable).map_err(|_| ERROR)?;
    directory
        .atomic_write("policy.toml", bytes, !new)
        .map_err(|_| ERROR)
}

#[cfg(not(windows))]
fn publish(path: &Path, bytes: &[u8], new: bool) -> Result<(), &'static str> {
    let parent = path.parent().ok_or(ERROR)?;
    verify_directory(parent, false)?;
    let mut stage = tempfile::NamedTempFile::new_in(parent).map_err(|_| ERROR)?;
    stage.write_all(bytes).map_err(|_| ERROR)?;
    stage.as_file().sync_all().map_err(|_| ERROR)?;
    if new {
        stage.persist_noclobber(path).map_err(|_| ERROR)?;
    } else {
        stage.persist(path).map_err(|_| ERROR)?;
    }
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|_| ERROR)?;
    Ok(())
}
