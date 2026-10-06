// SPDX-License-Identifier: Apache-2.0
//! Immutable packages and one atomic selection record; no user-state migration.

use std::fs::File;
#[cfg(not(windows))]
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::manifest::{MAX_ARCHIVE, PackageReceipt};
#[cfg(not(windows))]
use super::read_regular as read_input;
use super::{InstallError, Result, VerifiedPackage, is_digest, sha256, verify};
#[cfg(windows)]
use crate::core::windows_private::{Directory, Privacy};

const STATE: &str = "selection.json";

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    schema: u32,
    active: Option<PackageReceipt>,
    previous: Option<PackageReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    channel: Option<super::catalog::ChannelReceipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    rotations: Vec<super::rotation::Proof>,
}

struct Store {
    root: PathBuf,
    _lock: File,
    #[cfg(windows)]
    authority: Directory,
    #[cfg(windows)]
    _packages_authority: Directory,
}

#[cfg(not(windows))]
fn lock_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let lock = options.open(path)?;
    if !lock.metadata()?.is_file() {
        return Err(InstallError::Input);
    }
    lock.try_lock_exclusive().map_err(|_| InstallError::Busy)?;
    Ok(lock)
}

#[cfg(windows)]
fn lock_file(path: &Path) -> Result<File> {
    let directory = Directory::open(
        path.parent().ok_or(InstallError::Directory)?,
        Privacy::Private,
    )?;
    let lock = directory.open_lock(file_name(path)?)?;
    lock.try_lock_exclusive().map_err(|_| InstallError::Busy)?;
    Ok(lock)
}

pub(super) struct ConfigurationGuard {
    _lock: File,
    #[cfg(windows)]
    _directory: Directory,
}

/// Serialize consented setup across store commit, health and global configuration.
/// Separate from install.lock so bounded health execution never holds that lock.
pub(super) fn configuration_guard(root: &Path) -> Result<ConfigurationGuard> {
    prepare_root(root)?;
    #[cfg(windows)]
    let directory = Directory::open(root, Privacy::Private)?;
    Ok(ConfigurationGuard {
        _lock: lock_file(&root.join("configuration.lock"))?,
        #[cfg(windows)]
        _directory: directory,
    })
}

impl Store {
    fn open(root: &Path) -> Result<Self> {
        #[cfg(windows)]
        let authority = Directory::open(root, Privacy::Private)?;
        private_directory(root)?;
        let lock = lock_file(&root.join("install.lock"))?;
        let versions = root.join("packages");
        #[cfg(not(windows))]
        if !versions.try_exists()? {
            let builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            let mut builder = builder;
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&versions)?;
        }
        #[cfg(windows)]
        let packages_authority = Directory::create(&versions)?;
        private_directory(&versions)?;
        Ok(Self {
            root: root.to_owned(),
            _lock: lock,
            #[cfg(windows)]
            authority,
            #[cfg(windows)]
            _packages_authority: packages_authority,
        })
    }

    fn load(&self, expected: Option<&str>) -> Result<Selection> {
        let path = self.root.join(STATE);
        let state = match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Selection {
                schema: 1,
                ..Selection::default()
            },
            Err(error) => return Err(error.into()),
            Ok(_) => serde_json::from_slice(&read_regular(&path, 16_384)?)
                .map_err(|_| InstallError::State)?,
        };
        if !matches!(state.schema, 1..=3)
            || (state.schema >= 2) != state.channel.is_some()
            || (state.schema == 3) == state.rotations.is_empty()
            || expected.is_some_and(|expected| {
                state
                    .active
                    .as_ref()
                    .map_or("none", |value| value.manifest_sha256.as_str())
                    != expected
            })
        {
            return Err(InstallError::State);
        }
        let production = super::bootstrap::production_key(&self.root)?.is_some();
        for receipt in [&state.active, &state.previous].into_iter().flatten() {
            if receipt.staging_only == production {
                return Err(InstallError::Policy);
            }
            let directory = self.package_dir(&receipt.manifest_sha256)?;
            if sha256(&read_regular(
                &directory.join(super::EXECUTABLE_NAME),
                MAX_ARCHIVE / 2,
            )?) != receipt.binary_sha256
            {
                return Err(InstallError::State);
            }
        }
        Ok(state)
    }

    fn package_dir(&self, digest: &str) -> Result<PathBuf> {
        if !is_digest(digest, 64) {
            return Err(InstallError::State);
        }
        let directory = self.root.join("packages").join(digest);
        private_directory(&directory)?;
        Ok(directory)
    }

    fn select(&self, selection: &Selection) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(selection).map_err(|_| InstallError::State)?;
        if bytes.len() > 16_384 {
            return Err(InstallError::Size);
        }
        #[cfg(windows)]
        {
            self.authority
                .atomic_write(STATE, &bytes, true)
                .map_err(|_| InstallError::Commit)
        }
        #[cfg(not(windows))]
        // The same durable replace primitive as the public CLI updater, under
        // this store's independent lock. Never replace the running CLI binary.
        crate::core::updater::atomic_write_bytes(&self.root.join(STATE), &bytes)
            .map_err(|_| InstallError::Commit)
    }
}

#[cfg(not(windows))]
fn private_directory(path: &Path) -> Result<()> {
    if !path.is_absolute() || std::fs::canonicalize(path)? != path {
        return Err(InstallError::Directory);
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(InstallError::Directory);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and does not mutate process state.
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(InstallError::Directory);
        }
        Ok(())
    }
    #[cfg(not(unix))]
    Err(InstallError::Directory) // No native private-directory authority on this platform.
}

#[cfg(windows)]
fn private_directory(path: &Path) -> Result<()> {
    Directory::open(path, Privacy::Private)
        .map(|_| ())
        .map_err(|_| InstallError::Directory)
}

#[cfg(windows)]
fn file_name(path: &Path) -> Result<&str> {
    path.file_name()
        .and_then(|name| name.to_str())
        .ok_or(InstallError::Input)
}

fn read_regular(path: &Path, limit: usize) -> Result<Vec<u8>> {
    #[cfg(windows)]
    {
        let directory = Directory::open(
            path.parent().ok_or(InstallError::Directory)?,
            Privacy::Private,
        )?;
        Ok(directory.read_file(file_name(path)?, limit)?)
    }
    #[cfg(not(windows))]
    read_input(path, limit)
}

pub(super) fn write_new(path: &Path, contents: &[u8], executable: bool) -> Result<()> {
    #[cfg(windows)]
    let directory = Directory::open(
        path.parent().ok_or(InstallError::Directory)?,
        Privacy::Private,
    )?;
    #[cfg(windows)]
    let mut file = directory.create_file(file_name(path)?)?;
    #[cfg(not(windows))]
    let mut file = {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(if executable { 0o500 } else { 0o400 });
        }
        options.open(path)?
    };
    #[cfg(not(unix))]
    let _ = executable;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(windows))]
pub(super) type Temporary = tempfile::TempDir;

/// Linux may forbid execution in /tmp. Stage verified code beside its admitted
/// installation; /tmp remains data-only and no mount policy is bypassed.
#[cfg(target_os = "linux")]
pub(super) fn execution_directory(root: &Path) -> Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt;
    private_directory(root)?;
    let temporary = tempfile::Builder::new()
        .prefix(".run-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(root)?;
    private_directory(temporary.path())?;
    Ok(temporary)
}

/// Leaf guard drops before TempDir cleanup; parent stays pinned during cleanup.
#[cfg(windows)]
pub(super) struct Temporary {
    directory: Option<Directory>,
    temporary: tempfile::TempDir,
    _parent: Directory,
}

#[cfg(windows)]
impl Temporary {
    pub(super) fn path(&self) -> &Path {
        self.temporary.path()
    }

    pub(super) fn close(mut self) -> std::io::Result<()> {
        drop(self.directory.take());
        self.temporary.close()
    }

    fn publish(mut self, destination: &Path) -> Result<()> {
        self.directory
            .as_ref()
            .ok_or(InstallError::Directory)?
            .sync()?;
        // Windows cannot rename a directory while its no-share-delete guard is
        // held. The parent remains pinned, and the rename reopens/validates the
        // source through that parent before its handle-relative publication.
        drop(self.directory.take());
        crate::core::windows_private::rename_directory(self.path(), destination)?;
        Ok(())
    }
}

#[cfg(windows)]
pub(super) fn temporary_directory(parent: &Path, prefix: &str) -> Result<Temporary> {
    let parent = Directory::open(parent, Privacy::Private)?;
    // A private inheritable parent ACL protects creation itself. Validate the
    // inherited descriptor before putting any authenticated package bytes here.
    let temporary = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(parent.path())?;
    let directory = Directory::open(temporary.path(), Privacy::Private)?;
    Ok(Temporary {
        directory: Some(directory),
        temporary,
        _parent: parent,
    })
}

/// Snapshot for consented setup; installation still checks it under the store lock.
pub(super) fn active_manifest(root: &Path) -> Result<String> {
    if !root.try_exists()? {
        return Ok("none".into());
    }
    let state = Store::open(root)?.load(None)?;
    Ok(state
        .active
        .map_or_else(|| "none".into(), |value| value.manifest_sha256))
}

pub(super) fn channel_trust(
    root: &Path,
    expected: &str,
    anchor: &[u8; 32],
) -> Result<super::rotation::TrustedRoot> {
    let production = super::bootstrap::production_key(root)?;
    if production.is_some_and(|key| key != *anchor) {
        return Err(InstallError::Policy);
    }
    if !root.try_exists()? {
        if expected != "none" {
            return Err(InstallError::State);
        }
        return super::rotation::resolve_for(anchor, &[], production.is_some());
    }
    private_directory(root)?;
    if !root.join(STATE).try_exists()? {
        if expected != "none" {
            return Err(InstallError::State);
        }
        return super::rotation::resolve_for(anchor, &[], production.is_some());
    }
    let state = Store::open(root)?.load(Some(expected))?;
    let trusted = super::rotation::resolve_for(anchor, &state.rotations, production.is_some())?;
    if state
        .channel
        .as_ref()
        .is_some_and(|channel| !channel.rooted_in(&trusted))
    {
        return Err(InstallError::State);
    }
    Ok(trusted)
}

/// Commit trust forward under the existing selection lock; never rewrite global
/// config, reset a watermark, launch a runtime, or replace selected package bytes.
pub(super) fn rotate_root(
    root: &Path,
    expected: &str,
    anchor: &[u8; 32],
    proof: super::rotation::Proof,
) -> Result<serde_json::Value> {
    let production = super::bootstrap::production_key(root)?;
    if production.is_some_and(|key| key != *anchor) {
        return Err(InstallError::Policy);
    }
    let store = Store::open(root)?;
    let mut state = store.load(Some(expected))?;
    let current = super::rotation::resolve_for(anchor, &state.rotations, production.is_some())?;
    let channel = state.channel.as_ref().ok_or(InstallError::State)?;
    if !channel.rooted_in(&current) {
        return Err(InstallError::State);
    }
    if state.rotations.last() == Some(&proof) {
        return Ok(
            serde_json::json!({"status": "already_rotated", "root_key_sha256": sha256(&current.key),
            "runtime_started": false, "release_approved": false}),
        );
    }
    proof.admit(&current, channel.sequence)?;
    state.rotations.push(proof);
    let next = super::rotation::resolve_for(anchor, &state.rotations, production.is_some())?;
    state.schema = 3;
    store.select(&state)?;
    Ok(
        serde_json::json!({"status": "rotated", "root_key_sha256": sha256(&next.key),
        "minimum_sequence": next.minimum_sequence, "runtime_started": false, "release_approved": false}),
    )
}

#[cfg(not(any(unix, windows)))]
fn prepare_root(_root: &Path) -> Result<()> {
    Err(InstallError::Directory)
}

#[cfg(windows)]
fn prepare_root(root: &Path) -> Result<()> {
    Directory::create(root)
        .map(|_| ())
        .map_err(|_| InstallError::Directory)
}

#[cfg(unix)]
fn prepare_root(root: &Path) -> Result<()> {
    if !root.try_exists()? {
        let parent = root.parent().ok_or(InstallError::Directory)?;
        writable_parent(parent)?;
        let mut builder = std::fs::DirBuilder::new();
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
        builder.create(root)?;
    }
    private_directory(root)
}

#[cfg(unix)]
fn writable_parent(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    if !path.is_absolute() || std::fs::canonicalize(path)? != path {
        return Err(InstallError::Directory);
    }
    let metadata = std::fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no preconditions or side effects.
    let owner = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
        return Err(InstallError::Directory);
    }
    Ok(())
}

/// Consented bundled setup may create missing canonical data directories, but
/// never changes permissions of an existing directory or follows a symlink.
#[cfg(unix)]
pub(super) fn prepare_parent_chain(parent: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut missing = Vec::new();
    let mut existing = parent;
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(existing);
                existing = existing.parent().ok_or(InstallError::Directory)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    writable_parent(existing)?;
    for directory in missing.into_iter().rev() {
        std::fs::DirBuilder::new().mode(0o700).create(directory)?;
        private_directory(directory)?;
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn prepare_parent_chain(parent: &Path) -> Result<()> {
    Directory::create_chain(parent)
        .map(|_| ())
        .map_err(|_| InstallError::Directory)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn prepare_parent_chain(_parent: &Path) -> Result<()> {
    Err(InstallError::Directory)
}

/// Reauthenticate the selected package before returning an execution snapshot.
/// The installation lock is dropped before any child process can be started.
pub(super) fn verified_active(
    root: &Path,
    selected: &str,
    key: &[u8; 32],
) -> Result<VerifiedPackage> {
    let store = Store::open(root)?;
    let state = store.load(Some(selected))?;
    let directory = store.package_dir(selected)?;
    let production = verify_delegation(root, &directory, selected, key, &state)?;
    let package = verify(
        &read_regular(&directory.join("artifact.tar.gz"), MAX_ARCHIVE)?,
        &read_regular(&directory.join("manifest.json"), 65_536)?,
        &read_regular(&directory.join("manifest.sig"), 64)?,
        selected,
        key,
    )?;
    if package.receipt.staging_only == production {
        return Err(InstallError::Policy);
    }
    if state.active.as_ref() != Some(&package.receipt) {
        return Err(InstallError::State);
    }
    Ok(package)
}

fn verify_delegation(
    root: &Path,
    directory: &Path,
    selected: &str,
    key: &[u8; 32],
    state: &Selection,
) -> Result<bool> {
    let Some(delegated) = delegated_key(root, directory, selected, state)? else {
        return Ok(false);
    };
    if delegated != *key {
        return Err(InstallError::Signature);
    }
    Ok(true)
}

fn delegated_key(
    root: &Path,
    directory: &Path,
    selected: &str,
    state: &Selection,
) -> Result<Option<[u8; 32]>> {
    let Some(anchor) = super::bootstrap::production_key(root)? else {
        return Ok(None);
    };
    let trusted = super::rotation::resolve_for(&anchor, &state.rotations, true)?;
    let delegation: super::catalog::Delegation = serde_json::from_slice(&read_regular(
        &directory.join("catalog-delegation.json"),
        524_288,
    )?)
    .map_err(|_| InstallError::Manifest)?;
    let (delegated, _) = delegation.verify(selected, &trusted)?;
    Ok(Some(delegated))
}

pub(super) fn install(
    root: &Path,
    expected_active: &str,
    package: &VerifiedPackage,
    archive: &[u8],
    manifest: &[u8],
    signature: &[u8],
) -> Result<serde_json::Value> {
    install_selected(
        root,
        expected_active,
        package,
        archive,
        manifest,
        signature,
        None,
    )
}

pub(super) fn install_selected(
    root: &Path,
    expected_active: &str,
    package: &VerifiedPackage,
    archive: &[u8],
    manifest: &[u8],
    signature: &[u8],
    channel: Option<&super::catalog::Admission>,
) -> Result<serde_json::Value> {
    let production = super::bootstrap::production_key(root)?;
    if package.receipt.staging_only == production.is_some()
        || production.is_some_and(|key| {
            channel.is_none_or(|channel| channel.anchor != key || channel.delegation.is_none())
        })
        || production.is_none() && channel.is_some_and(|channel| channel.delegation.is_some())
    {
        return Err(InstallError::Policy);
    }
    prepare_root(root)?;
    let store = Store::open(root)?;
    let mut state = store.load(Some(expected_active))?;
    let channel_changed =
        channel.is_some_and(|channel| state.channel.as_ref() != Some(&channel.receipt));
    if let Some(channel) = channel {
        let trusted =
            super::rotation::resolve_for(&channel.anchor, &state.rotations, production.is_some())?;
        channel.receipt.admit(state.channel.as_ref(), &trusted)?;
        if let Some(delegation) = &channel.delegation {
            let (key, receipt) = delegation.verify(&package.receipt.manifest_sha256, &trusted)?;
            if receipt != channel.receipt {
                return Err(InstallError::State);
            }
            super::manifest::verify_manifest(
                manifest,
                signature,
                &package.receipt.manifest_sha256,
                &key,
            )?;
        }
        state.channel = Some(channel.receipt.clone());
        state.schema = if state.rotations.is_empty() { 2 } else { 3 };
    }
    let receipt = &package.receipt;
    let already_installed = state.active.as_ref() == Some(receipt);
    if !already_installed
        && state
            .active
            .as_ref()
            // The signed rollback link identifies the complete prior archive, not
            // only its executable (which excludes package metadata and provenance).
            .is_some_and(|active| {
                receipt.rollback_sha256.as_deref() != Some(active.artifact_sha256.as_str())
            })
    {
        return Err(InstallError::State);
    }
    let destination = root.join("packages").join(&receipt.manifest_sha256);
    if destination.try_exists()? {
        let directory = store.package_dir(&receipt.manifest_sha256)?;
        for (name, expected) in [
            ("artifact.tar.gz", archive),
            ("manifest.json", manifest),
            ("manifest.sig", signature),
            (super::EXECUTABLE_NAME, package.binary.as_slice()),
        ] {
            if read_regular(&directory.join(name), MAX_ARCHIVE)? != expected {
                return Err(InstallError::State);
            }
        }
        if let Some(channel) = channel
            && let Some(delegation) = &channel.delegation
        {
            let trusted = super::rotation::resolve_for(&channel.anchor, &state.rotations, true)?;
            let (key, _) = delegation.verify(&receipt.manifest_sha256, &trusted)?;
            verify_delegation(root, &directory, &receipt.manifest_sha256, &key, &state)?;
        }
    } else {
        #[cfg(not(windows))]
        let staged = {
            let mut builder = tempfile::Builder::new();
            builder.prefix(".install-");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                builder.permissions(std::fs::Permissions::from_mode(0o700));
            }
            builder.tempdir_in(root.join("packages"))?
        };
        #[cfg(windows)]
        let staged = temporary_directory(&root.join("packages"), ".install-")?;
        private_directory(staged.path())?;
        write_new(&staged.path().join("artifact.tar.gz"), archive, false)?;
        write_new(&staged.path().join("manifest.json"), manifest, false)?;
        write_new(&staged.path().join("manifest.sig"), signature, false)?;
        if let Some(delegation) = channel.and_then(|channel| channel.delegation.as_ref()) {
            let bytes = serde_json::to_vec(delegation).map_err(|_| InstallError::State)?;
            if bytes.len() > 524_288 {
                return Err(InstallError::Size);
            }
            write_new(
                &staged.path().join("catalog-delegation.json"),
                &bytes,
                false,
            )?;
        }
        write_new(
            &staged.path().join(super::EXECUTABLE_NAME),
            &package.binary,
            true,
        )?;
        #[cfg(windows)]
        staged.publish(&destination)?;
        #[cfg(not(windows))]
        {
            File::open(staged.path())?.sync_all()?;
            std::fs::rename(staged.path(), &destination)?;
            File::open(root.join("packages"))?.sync_all()?;
        }
    }
    if already_installed {
        if channel_changed {
            store.select(&state)?;
        }
        return Ok(serde_json::json!({"status": "already_installed", "receipt": receipt}));
    }
    state.previous = state.active.take();
    state.active = Some(receipt.clone());
    store.select(&state)?;
    Ok(
        serde_json::json!({"status": "installed", "receipt": receipt,
        "runtime_started": false, "user_state_modified": false}),
    )
}

pub(super) fn rollback(
    root: &Path,
    expected_active: &str,
    selected: &str,
    key: &[u8; 32],
) -> Result<serde_json::Value> {
    let store = Store::open(root)?;
    let mut state = store.load(Some(expected_active))?;
    rollback_selected(&store, &mut state, selected, key)
}

/// Ordinary-user rollback accepts no replacement trust keys or package paths.
/// Both the current link and previous package are reauthenticated under one lock.
pub(super) fn rollback_configured(
    root: &Path,
    expected_active: &str,
) -> Result<(serde_json::Value, String)> {
    let store = Store::open(root)?;
    let mut state = store.load(None)?;
    let active = state.active.as_ref().ok_or(InstallError::State)?;
    let (package, _) = verified_delegated_package(&store, expected_active, &state)?;
    if active.manifest_sha256 != expected_active {
        // Resume a selection committed before health/configuration completed.
        // Never infer authorization from a mutable receipt or accept a different
        // concurrent update: the originally configured signature must name this
        // exact active archive as its predecessor, with no outstanding predecessor.
        let (resumed, key) = verified_delegated_package(&store, &active.manifest_sha256, &state)?;
        if state.previous.is_some()
            || resumed.receipt != *active
            || package.receipt.rollback_sha256.as_deref()
                != Some(resumed.receipt.artifact_sha256.as_str())
        {
            return Err(InstallError::State);
        }
        return Ok((
            serde_json::json!({"status": "already_rolled_back", "receipt": resumed.receipt,
                "runtime_started": false, "user_state_modified": false}),
            hex::encode(key),
        ));
    }
    if package.receipt != *active {
        return Err(InstallError::State);
    }
    let selected = state
        .previous
        .as_ref()
        .ok_or(InstallError::State)?
        .manifest_sha256
        .clone();
    let key = delegated_key(root, &store.package_dir(&selected)?, &selected, &state)?
        .ok_or(InstallError::Policy)?;
    let result = rollback_selected(&store, &mut state, &selected, &key)?;
    Ok((result, hex::encode(key)))
}

fn verified_delegated_package(
    store: &Store,
    selected: &str,
    state: &Selection,
) -> Result<(VerifiedPackage, [u8; 32])> {
    let directory = store.package_dir(selected)?;
    let key =
        delegated_key(&store.root, &directory, selected, state)?.ok_or(InstallError::Policy)?;
    let package = verify(
        &read_regular(&directory.join("artifact.tar.gz"), MAX_ARCHIVE)?,
        &read_regular(&directory.join("manifest.json"), 65_536)?,
        &read_regular(&directory.join("manifest.sig"), 64)?,
        selected,
        &key,
    )?;
    if package.receipt.staging_only {
        return Err(InstallError::Policy);
    }
    Ok((package, key))
}

fn rollback_selected(
    store: &Store,
    state: &mut Selection,
    selected: &str,
    key: &[u8; 32],
) -> Result<serde_json::Value> {
    let previous = state.previous.as_ref().ok_or(InstallError::State)?;
    if previous.manifest_sha256 != selected {
        return Err(InstallError::State);
    }
    let directory = store.package_dir(selected)?;
    let production = verify_delegation(&store.root, &directory, selected, key, state)?;
    let package = verify(
        &read_regular(&directory.join("artifact.tar.gz"), MAX_ARCHIVE)?,
        &read_regular(&directory.join("manifest.json"), 65_536)?,
        &read_regular(&directory.join("manifest.sig"), 64)?,
        selected,
        key,
    )?;
    if package.receipt.staging_only == production {
        return Err(InstallError::Policy);
    }
    if package.receipt != *previous
        || state.active.as_ref().is_none_or(|active| {
            active.rollback_sha256.as_deref() != Some(previous.artifact_sha256.as_str())
        })
    {
        return Err(InstallError::State);
    }
    state.active = state.previous.take();
    store.select(state)?;
    Ok(
        serde_json::json!({"status": "rolled_back", "receipt": package.receipt,
        "runtime_started": false, "user_state_modified": false}),
    )
}
