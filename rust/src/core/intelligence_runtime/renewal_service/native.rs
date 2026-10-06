// SPDX-License-Identifier: Apache-2.0
use crate::core::intelligence_runtime::{InstallError, Result, provisioning, read_regular};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use super::{
    manager,
    specification::{Specification, safe_text},
};

struct Store {
    home: PathBuf,
    config: PathBuf,
    label: String,
    record: PathBuf,
    sync_configuration: Option<PathBuf>,
    _lock: File,
}

fn directory(path: &Path, private: bool) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir()
        || meta.file_type().is_symlink()
        || path.canonicalize()? != path
        || meta.uid() != manager::uid()
        || meta.mode() & if private { 0o077 } else { 0o022 } != 0
    {
        return Err(InstallError::RenewalService);
    }
    safe_text(path)?;
    Ok(())
}

fn private_file(path: &Path) -> Result<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.uid() != manager::uid()
        || meta.mode() & 0o077 != 0
    {
        return Err(InstallError::RenewalService);
    }
    read_regular(path, 16 * 1024)
}

/// Validate each user-owned ancestor before creating or accessing a unit.
fn unit_directory(home: &Path, parent: &Path, create: bool) -> Result<()> {
    let relative = parent
        .strip_prefix(home)
        .map_err(|_| InstallError::RenewalService)?;
    let mut current = home.to_path_buf();
    for component in relative.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(InstallError::RenewalService);
        }
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(_) => directory(&current, false)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !create {
                    return Ok(());
                }
                std::fs::DirBuilder::new().mode(0o700).create(&current)?;
                directory(&current, false)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or(InstallError::RenewalService)?;
    // Complete private bytes before publishing a no-replace final name. A crash
    // may retain an unselected temporary file, never a truncated service record.
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .map_err(|_| InstallError::RenewalService)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn replace_legacy(path: &Path, observed: &[u8], bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or(InstallError::RenewalService)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    // Recheck after draining the manager; never overwrite a changed owned file.
    if private_file(path)? != observed {
        return Err(InstallError::RenewalService);
    }
    file.persist(path)
        .map_err(|_| InstallError::RenewalService)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn verify_observed_units(units: &[(&PathBuf, Option<Vec<u8>>)]) -> Result<()> {
    for (path, observed) in units {
        match (std::fs::symlink_metadata(path), observed) {
            (Ok(_), Some(bytes)) if private_file(path)? == *bytes => {}
            (Err(error), None) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(InstallError::RenewalService),
        }
    }
    Ok(())
}

impl Store {
    fn open(sync_configuration: Option<&Path>) -> Result<Self> {
        if manager::uid() == 0 {
            return Err(InstallError::RenewalService);
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or(InstallError::RenewalService)?
            .canonicalize()?;
        directory(&home, false)?;
        let config = crate::core::paths::config_dir_read_only()
            .map_err(|_| InstallError::RenewalService)?
            .canonicalize()?;
        // Existing global configuration is non-secret and normally0755.
        // Ownership/control is required; the service record and lock stay0600.
        directory(&config, false)?;
        let mut namespace = "runtime-renewal-service".to_owned();
        let mut digest_input = safe_text(&config)?.as_bytes().to_vec();
        if let Some(path) = sync_configuration {
            safe_text(path)?;
            let parent = path.parent().ok_or(InstallError::RenewalService)?;
            directory(parent, false)?;
            if path.components().any(|part| {
                matches!(
                    part,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            }) {
                return Err(InstallError::RenewalService);
            }
            digest_input.push(0);
            digest_input.extend_from_slice(safe_text(path)?.as_bytes());
            namespace = format!(
                "runtime-sync-service-{}",
                &hex::encode(Sha256::digest(safe_text(path)?.as_bytes()))[..24]
            );
        }
        let mut options = OpenOptions::new();
        let lock = options
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(config.join(format!("{namespace}.lock")))?;
        let meta = lock.metadata()?;
        if !meta.is_file()
            || meta.nlink() != 1
            || meta.uid() != manager::uid()
            || meta.mode() & 0o077 != 0
        {
            return Err(InstallError::RenewalService);
        }
        lock.try_lock_exclusive().map_err(|_| InstallError::Busy)?;
        let digest = hex::encode(Sha256::digest(&digest_input));
        let kind = if sync_configuration.is_some() {
            "sync"
        } else {
            "renewal"
        };
        let label = format!("com.leanctx.{kind}.{}", &digest[..24]);
        Ok(Self {
            record: config.join(format!("{namespace}.json")),
            sync_configuration: sync_configuration.map(Path::to_path_buf),
            home,
            config,
            label,
            _lock: lock,
        })
    }

    fn specification(&self) -> Result<Option<Specification>> {
        match std::fs::symlink_metadata(&self.record) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        let spec: Specification = serde_json::from_slice(&private_file(&self.record)?)
            .map_err(|_| InstallError::RenewalService)?;
        spec.validate(&self.home, &self.config, self.sync_configuration.as_deref())?;
        Ok(Some(spec))
    }

    fn units(&self, spec: &Specification) -> Result<Vec<(PathBuf, Vec<u8>)>> {
        let values = [
            safe_text(&spec.host)?,
            safe_text(&self.home)?,
            safe_text(&self.config)?,
        ];
        let sync = self
            .sync_configuration
            .as_deref()
            .map(safe_text)
            .transpose()?;
        let data = spec
            .sync_data_directory
            .as_deref()
            .map(safe_text)
            .transpose()?;
        Ok(manager::units_with_mode(
            &self.label,
            &values,
            &self.home,
            sync,
            data,
            spec.staging,
        ))
    }

    fn cycle_path(&self) -> PathBuf {
        self.record.with_extension("cycle.json")
    }

    fn last_cycle(&self) -> Result<serde_json::Value> {
        if self.sync_configuration.is_none() || !self.cycle_path().try_exists()? {
            return Ok(serde_json::Value::Null);
        }
        let value: serde_json::Value = serde_json::from_slice(&private_file(&self.cycle_path())?)
            .map_err(|_| InstallError::RenewalService)?;
        let status = value["status"]
            .as_str()
            .filter(|s| {
                matches!(
                    *s,
                    "failed"
                        | "empty"
                        | "unchanged"
                        | "conflict"
                        | "acknowledged"
                        | "uploaded"
                        | "downloaded"
                )
            })
            .ok_or(InstallError::RenewalService)?;
        let reason = value["reason"]
            .as_str()
            .and_then(super::super::project_sync::public_reason);
        Ok(serde_json::json!({"status":status,"conflict":status=="conflict","reason":reason}))
    }

    fn save_cycle(&self, result: &Result<serde_json::Value>) -> Result<()> {
        if self.specification()?.is_none() {
            return Ok(());
        }
        if self.cycle_path().try_exists()? {
            private_file(&self.cycle_path())?;
        }
        let value = result.as_ref().cloned().unwrap_or_else(|error| {
            let reason = match error {
                InstallError::SyncCycle(reason) => *reason,
                _ => "host_admission_failed",
            };
            serde_json::json!({"status":"failed","reason":reason})
        });
        let mut temporary = tempfile::NamedTempFile::new_in(&self.config)?;
        temporary
            .write_all(&serde_json::to_vec(&value).map_err(|_| InstallError::RenewalService)?)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.cycle_path())
            .map_err(|_| InstallError::RenewalService)?;
        File::open(&self.config)?.sync_all()?;
        Ok(())
    }
}

fn sync_data_directory() -> Result<PathBuf> {
    let path = crate::core::paths::data_dir_read_only()
        .map_err(|_| InstallError::Configuration)?
        .canonicalize()?;
    directory(&path, false)?;
    Ok(path)
}

pub(super) fn run(
    operation: &str,
    sync_configuration: Option<&Path>,
    staging: bool,
) -> Result<serde_json::Value> {
    let normalized = operation.replacen("sync-service-", "renewal-service-", 1);
    let operation = normalized.as_str();
    if operation == "renewal-service-tick" {
        return tick(sync_configuration.map(Path::to_path_buf), staging);
    }
    let store = Store::open(sync_configuration)?;
    let mut spec = store.specification()?;
    let status = manager::status(&store.label)?;
    if operation == "renewal-service-status" && spec.is_none() {
        return Ok(
            serde_json::json!({"status":"not_installed","registered":status["registered"],
            "owned":false,"release_approved":false}),
        );
    }
    if operation == "renewal-service-remove" && spec.is_none() {
        if status["registered"] == true {
            return Err(InstallError::RenewalService);
        }
        return Ok(
            serde_json::json!({"status":"not_installed","registered":false,"release_approved":false}),
        );
    }
    if operation == "renewal-service-install" {
        let configured = crate::core::config::Config::try_load_global()
            .map_err(|_| InstallError::Configuration)?
            .intelligence_runtime;
        if configured.staging != staging
            || !super::super::bootstrap::permits_config(&configured)
            || spec
                .as_ref()
                .is_some_and(|saved| saved.staging != configured.staging)
        {
            return Err(InstallError::RenewalService);
        }
        if !configured.staging {
            let data = sync_data_directory()?;
            if spec
                .as_ref()
                .is_some_and(|saved| saved.sync_data_directory.as_ref() != Some(&data))
            {
                return Err(InstallError::RenewalService);
            }
        }
        // Verify the saved package and complete one due check before admission.
        // This is a host operation; the public scheduler never interprets a lease.
        if let Some(path) = sync_configuration {
            private_file(path)?;
            let data = sync_data_directory()?;
            if spec
                .as_ref()
                .is_some_and(|saved| saved.sync_data_directory.as_ref() != Some(&data))
            {
                return Err(InstallError::RenewalService);
            }
            super::super::project_sync::cycle_for_mode(path, staging)?;
        } else {
            provisioning::renew_cancellable_for_mode(staging, || false)?;
        }
        if spec.is_none() {
            if status["registered"] == true {
                return Err(InstallError::RenewalService);
            }
            let host = std::env::current_exe()?.canonicalize()?;
            let meta = std::fs::symlink_metadata(&host)?;
            if !meta.is_file() || meta.mode() & 0o022 != 0 || meta.mode() & 0o111 == 0 {
                return Err(InstallError::RenewalService);
            }
            safe_text(&host)?;
            let selected = Specification {
                schema_version: 1,
                host,
                home: store.home.clone(),
                configuration_directory: store.config.clone(),
                sync_configuration: store.sync_configuration.clone(),
                // Production root admission also depends on the data directory
                // for renewal-only services; retain it in the existing record.
                sync_data_directory: if sync_configuration.is_some() || !configured.staging {
                    Some(sync_data_directory()?)
                } else {
                    None
                },
                staging: configured.staging,
            };
            let bytes =
                serde_json::to_vec_pretty(&selected).map_err(|_| InstallError::RenewalService)?;
            // Durable ownership precedes unit publication; an interrupted install
            // resumes only its matching files, never overwrites another unit.
            write_new(&store.record, &bytes)?;
            spec = Some(selected);
        }
    }
    let spec = spec.ok_or(InstallError::RenewalService)?;
    let units = store.units(&spec)?;
    let mut missing = Vec::new();
    let mut legacy = Vec::new();
    let mut observed_units = Vec::new();
    // Admit every unit before publishing, replacing, disabling or removing any.
    for (path, body) in &units {
        unit_directory(
            &store.home,
            path.parent().ok_or(InstallError::RenewalService)?,
            operation == "renewal-service-install",
        )?;
        match std::fs::symlink_metadata(path) {
            Ok(_) => {
                let observed = private_file(path)?;
                observed_units.push((path, Some(observed.clone())));
                if observed != *body {
                    if manager::legacy_unit(body, &store.config).as_ref() != Some(&observed) {
                        return Err(InstallError::RenewalService);
                    }
                    legacy.push((path, body, observed));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push((path, body));
                observed_units.push((path, None));
            }
            Err(error) => return Err(error.into()),
        }
    }
    if operation == "renewal-service-status" && !missing.is_empty() {
        return Ok(serde_json::json!({"status":"incomplete","owned":true,
            "registered":status["registered"],"migration_required":!legacy.is_empty(),
            "release_approved":false}));
    }
    if operation == "renewal-service-install" {
        if !legacy.is_empty() {
            manager::remove(&store.label, sync_configuration.is_some())?;
            verify_observed_units(&observed_units)?;
            for (path, body, observed) in &legacy {
                replace_legacy(path, observed, body)?;
            }
        }
        for (path, body) in &missing {
            write_new(path, body)?;
        }
        manager::install(&store.label, &units)?;
    } else if operation == "renewal-service-remove" {
        manager::remove(&store.label, sync_configuration.is_some())?;
        verify_observed_units(&observed_units)?;
        for (path, observed) in &observed_units {
            if observed.is_some() {
                std::fs::remove_file(path)?;
            }
        }
        std::fs::remove_file(&store.record)?;
        File::open(&store.config)?.sync_all()?;
        manager::reload()?;
        return Ok(serde_json::json!({"status":"removed","registered":false,
            "license_data_preserved":true,"release_approved":false}));
    }
    let current = manager::status(&store.label)?;
    let last_cycle = store.last_cycle()?;
    Ok(
        serde_json::json!({"status":"installed","owned":true,"manager":current,
        "interval_seconds":60,"renewal_authority":"verified_saved_runtime",
        "selected_project_sync":sync_configuration.is_some(),
        "migration_required":operation != "renewal-service-install" && !legacy.is_empty(),
        "last_recorded_cycle":last_cycle,
        "release_approved":false}),
    )
}

/// Tolerate short install/status contention without deadlocking removal.
fn tick_store(sync_configuration: Option<&Path>) -> Result<Store> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    loop {
        match Store::open(sync_configuration) {
            Err(InstallError::Busy) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            result => return result,
        }
    }
}

/// Keep the host alive on manager shutdown until bounded child-group cleanup
/// completes; the private command runs in a separate process group.
fn tick(sync_configuration: Option<PathBuf>, expected_staging: bool) -> Result<serde_json::Value> {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| InstallError::RenewalService)?;
    runtime.block_on(async {
        let mut terminate=tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|_|InstallError::RenewalService)?;
        let mut interrupt=tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .map_err(|_|InstallError::RenewalService)?;
        let stop=Arc::new(AtomicBool::new(false));
        let cancellation=Arc::clone(&stop);
        let drain = sync_configuration.is_some();
        let mut check=tokio::task::spawn_blocking(move || {
            if let Some(path) = sync_configuration {
                private_file(&path)?;
                let selected = tick_store(Some(&path))?;
                let spec = selected.specification()?.ok_or(InstallError::RenewalService)?;
                if spec.staging != expected_staging {
                    return Err(InstallError::RenewalService);
                }
                if spec.sync_data_directory.as_ref() != Some(&sync_data_directory()?) { return Err(InstallError::RenewalService); }
                drop(selected);
                let result = super::super::project_sync::cycle_for_mode(&path, expected_staging);
                // Brief status/install contention is retryable. Removal owns
                // the lock while draining, so retry only for the fixed bound
                // and never recreate a removed service to publish diagnostics.
                if let Ok(store) = tick_store(Some(&path)) { store.save_cycle(&result)?; }
                result
            } else {
                let specification = { tick_store(None)?.specification()? };
                if specification.is_some_and(|spec| spec.staging != expected_staging
                    || !spec.staging && spec.sync_data_directory.as_ref() != sync_data_directory().ok().as_ref()) {
                    return Err(InstallError::RenewalService);
                }
                provisioning::renew_cancellable_for_mode(expected_staging, || {
                    cancellation.load(Ordering::Relaxed)
                })
            }
        });
        tokio::select! {
            result=&mut check => result.map_err(|_|InstallError::RenewalService)?,
            _=terminate.recv() => {if !drain {stop.store(true,Ordering::Relaxed);}check.await.map_err(|_|InstallError::RenewalService)?},
            _=interrupt.recv() => {if !drain {stop.store(true,Ordering::Relaxed);}check.await.map_err(|_|InstallError::RenewalService)?},
        }
    })
}
