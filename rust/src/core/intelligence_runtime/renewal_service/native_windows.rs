// SPDX-License-Identifier: Apache-2.0
//! Per-user Windows scheduling around the existing verified private commands.
use super::{
    manager_windows as manager,
    specification::{Specification, safe_text, windows_task_arguments},
};
use crate::core::{
    config::Config,
    intelligence_runtime::{InstallError, Result, project_sync, provisioning},
    windows_private::{Directory, Privacy, current_user_sid_string},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

const LIMIT: usize = 16 * 1024;

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Record {
    specification: Specification,
    sid: String,
    host_sha256: String,
    start_boundary: String,
}

// File locks close before the pinned directory chain.
struct Store {
    lock: Option<File>,
    directory: Directory,
    _config: Directory,
    _home: Directory,
    home: PathBuf,
    config: PathBuf,
    sync: Option<PathBuf>,
    sid: String,
    label: String,
    name: String,
}

fn sid() -> Result<String> {
    super::principal_windows::require_user_token()?;
    let sid = String::from_utf16(&current_user_sid_string()?)
        .map_err(|_| InstallError::RenewalService)?;
    if matches!(sid.as_str(), "S-1-5-18" | "S-1-5-19" | "S-1-5-20") {
        return Err(InstallError::RenewalService);
    }
    Ok(sid)
}

fn leaf(path: &Path) -> Result<&str> {
    path.file_name()
        .and_then(|part| part.to_str())
        .ok_or(InstallError::RenewalService)
}

fn private_read(path: &Path) -> Result<Vec<u8>> {
    let parent = path.parent().ok_or(InstallError::RenewalService)?;
    let directory = Directory::open(parent, Privacy::Private)?;
    Ok(directory.read_file(leaf(path)?, LIMIT)?)
}

fn selected_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or(InstallError::RenewalService)
}

fn data_directory() -> Result<PathBuf> {
    let path = crate::core::paths::data_dir_read_only().map_err(|_| InstallError::Configuration)?;
    let directory = Directory::open(&path, Privacy::Writable)?;
    Ok(directory.path().canonicalize()?)
}

impl Store {
    fn open(sync: Option<&Path>) -> Result<Self> {
        let config =
            crate::core::paths::config_dir_read_only().map_err(|_| InstallError::Configuration)?;
        Self::at(&selected_home()?, &config, sync)
    }

    fn at(home: &Path, config: &Path, sync: Option<&Path>) -> Result<Self> {
        let sid = sid()?;
        let home_guard = Directory::open(home, Privacy::Writable)?;
        let config_guard = Directory::open(config, Privacy::Writable)?;
        let home = home_guard.path().canonicalize()?;
        let config = config_guard.path().canonicalize()?;
        let mut hash = Sha256::new();
        hash.update(sid.as_bytes());
        hash.update([0]);
        hash.update(safe_text(&config)?.as_bytes());
        if let Some(path) = sync {
            safe_text(path)?;
            if path.components().any(|part| {
                matches!(
                    part,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            }) {
                return Err(InstallError::RenewalService);
            }
            hash.update([0]);
            hash.update(safe_text(path)?.as_bytes());
        }
        let kind = if sync.is_some() { "sync" } else { "renewal" };
        let label = format!("com.leanctx.{kind}.{}", &hex::encode(hash.finalize())[..32]);
        let created = Directory::create_chain(&config.join("runtime-services"))?;
        let directory = Directory::open(created.path(), Privacy::Private)?;
        let lock = directory.open_lock(&format!("{label}.lock"))?;
        lock.try_lock().map_err(|_| InstallError::Busy)?;
        Ok(Self {
            lock: Some(lock),
            directory,
            _config: config_guard,
            _home: home_guard,
            home,
            config,
            sync: sync.map(Path::to_path_buf),
            sid,
            name: format!("{label}.json"),
            label,
        })
    }

    fn path(&self) -> PathBuf {
        self.directory.path().join(&self.name)
    }

    fn record(&self) -> Result<Option<Record>> {
        let bytes = match self.directory.read_file(&self.name, LIMIT) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let record: Record =
            serde_json::from_slice(&bytes).map_err(|_| InstallError::RenewalService)?;
        record
            .specification
            .validate(&self.home, &self.config, self.sync.as_deref())?;
        if record.sid != self.sid
            || !super::super::is_digest(&record.host_sha256, 64)
            || record.start_boundary.len() > 64
            || chrono::DateTime::parse_from_rfc3339(&record.start_boundary).is_err()
        {
            return Err(InstallError::RenewalService);
        }
        Ok(Some(record))
    }

    fn task(&self, record: &Record) -> Result<manager::Task> {
        Ok(manager::Task {
            label: self.label.clone(),
            sid: self.sid.clone(),
            host: safe_text(&record.specification.host)?.to_owned(),
            arguments: windows_task_arguments(&self.path())?,
            working_directory: safe_text(&self.config)?.to_owned(),
            start_boundary: record.start_boundary.clone(),
        })
    }

    fn cycle_lock(&self) -> Result<File> {
        let file = self
            .directory
            .open_lock(&format!("{}.cycle.lock", self.label))?;
        file.try_lock().map_err(|_| InstallError::Busy)?;
        Ok(file)
    }

    fn release(&mut self) {
        self.lock.take();
    }

    fn save_cycle(&self, result: &Result<serde_json::Value>) -> Result<()> {
        if self.sync.is_none() {
            return Ok(());
        }
        // Validate an existing private receipt before replacing it; a malformed
        // or redirected side file must not silently become a successful write.
        self.last_cycle()?;
        let status = result
            .as_ref()
            .ok()
            .and_then(|value| value["status"].as_str())
            .filter(|status| {
                matches!(
                    *status,
                    "empty" | "unchanged" | "conflict" | "acknowledged" | "uploaded" | "downloaded"
                )
            })
            .unwrap_or("failed");
        let reason = match result {
            Err(InstallError::SyncCycle(reason)) => project_sync::public_reason(reason),
            Err(_) => Some("host_admission_failed"),
            Ok(_) => None,
        };
        let value =
            serde_json::json!({"status":status,"conflict":status == "conflict","reason":reason});
        self.directory.atomic_write(
            &format!("{}.cycle.json", self.label),
            &serde_json::to_vec(&value).map_err(|_| InstallError::RenewalService)?,
            true,
        )?;
        Ok(())
    }

    fn last_cycle(&self) -> Result<serde_json::Value> {
        let bytes = match self
            .directory
            .read_file(&format!("{}.cycle.json", self.label), LIMIT)
        {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(serde_json::Value::Null);
            }
            Err(error) => return Err(error.into()),
        };
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| InstallError::RenewalService)?;
        let status = value["status"]
            .as_str()
            .filter(|status| {
                matches!(
                    *status,
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
            .and_then(project_sync::public_reason);
        Ok(serde_json::json!({"status":status,"conflict":status == "conflict","reason":reason}))
    }
}

/// Holds both the executable and its directory so the admitted path cannot be replaced.
struct Executable {
    _file: File,
    _directory: Directory,
    path: PathBuf,
    digest: String,
}
fn executable(path: &Path) -> Result<Executable> {
    let directory = Directory::open(
        path.parent().ok_or(InstallError::RenewalService)?,
        Privacy::Writable,
    )?;
    let file = directory.open_controlled_file(leaf(path)?)?;
    let mut source = (&file).take(512 * 1024 * 1024 + 1);
    let mut digest = Sha256::new();
    let mut bytes = vec![0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let count = source.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > 512 * 1024 * 1024 {
            return Err(InstallError::Size);
        }
        digest.update(&bytes[..count]);
    }
    let path = path.canonicalize()?;
    safe_text(&path)?;
    Ok(Executable {
        _file: file,
        _directory: directory,
        path,
        digest: hex::encode(digest.finalize()),
    })
}

fn execute_cycle(sync: Option<&Path>, staging: bool) -> Result<serde_json::Value> {
    if let Some(path) = sync {
        private_read(path)?;
        project_sync::cycle_for_mode(path, staging)
    } else {
        provisioning::renew_cancellable_for_mode(staging, || false)
    }
}

pub(super) fn run(
    operation: &str,
    sync: Option<&Path>,
    staging: bool,
) -> Result<serde_json::Value> {
    let operation = operation.replacen("sync-service-", "renewal-service-", 1);
    if operation == "renewal-service-tick" {
        return tick(sync, staging);
    }
    let store = Store::open(sync)?;
    let mut record = store.record()?;
    if record.is_none() && manager::exists(&store.label)? {
        return Err(InstallError::RenewalService);
    }
    if record.is_none()
        && matches!(
            operation.as_str(),
            "renewal-service-status" | "renewal-service-remove"
        )
    {
        return Ok(
            serde_json::json!({"status":"not_installed","registered":false,"owned":false,"release_approved":false}),
        );
    }
    if operation == "renewal-service-install" {
        if let Some(record) = &record {
            manager::status(&store.task(record)?)?;
        }
        let observed = Config::try_load_global()
            .map_err(|_| InstallError::Configuration)?
            .intelligence_runtime;
        if observed.staging != staging || !super::super::bootstrap::permits_config(&observed) {
            return Err(InstallError::RenewalService);
        }
        private_read(Path::new(&observed.license_configuration))?;
        let host = executable(&std::env::current_exe()?)?;
        let data = if sync.is_some() || !staging {
            Some(data_directory()?)
        } else {
            None
        };
        let spec = Specification {
            schema_version: 1,
            host: host.path.clone(),
            home: store.home.clone(),
            configuration_directory: store.config.clone(),
            sync_configuration: store.sync.clone(),
            sync_data_directory: data,
            staging,
        };
        if record
            .as_ref()
            .is_some_and(|record| record.specification != spec || record.host_sha256 != host.digest)
        {
            return Err(InstallError::RenewalService);
        }
        // Do not overlap an active scheduled or manual cycle during re-install.
        let _cycle = store.cycle_lock()?;
        execute_cycle(sync, staging)?;
        if Config::try_load_global()
            .map_err(|_| InstallError::Configuration)?
            .intelligence_runtime
            != observed
        {
            return Err(InstallError::RenewalService);
        }
        if record.is_none() {
            let selected = Record {
                specification: spec,
                sid: store.sid.clone(),
                host_sha256: host.digest.clone(),
                start_boundary: (chrono::Utc::now() + chrono::Duration::seconds(2))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            };
            store.directory.atomic_write(
                &store.name,
                &serde_json::to_vec(&selected).map_err(|_| InstallError::RenewalService)?,
                false,
            )?;
            record = Some(selected);
        }
        manager::install(&store.task(record.as_ref().ok_or(InstallError::RenewalService)?)?)?;
    }
    let record = record.ok_or(InstallError::RenewalService)?;
    let task = store.task(&record)?;
    if operation == "renewal-service-remove" {
        manager::remove(&task, sync.is_some())?;
        // A manually invoked tick also owns this lock; retain the record if it
        // is still running even after the scheduler itself has drained.
        let _cycle = store.cycle_lock()?;
        store.directory.remove_file(&store.name)?;
        return Ok(
            serde_json::json!({"status":"removed","registered":false,"license_data_preserved":true,"release_approved":false}),
        );
    }
    if !matches!(
        operation.as_str(),
        "renewal-service-status" | "renewal-service-install"
    ) {
        return Err(InstallError::Usage);
    }
    let state = manager::status(&task)?;
    let image_matches =
        executable(&record.specification.host).is_ok_and(|host| host.digest == record.host_sha256);
    Ok(
        serde_json::json!({"status":if !image_matches {"requires_reinstall"} else if state["registered"] != true {"incomplete"} else if state["enabled"] != true {"disabled"} else {"installed"},
        "owned":true,"manager":state,"interval_seconds":60,"renewal_authority":"verified_saved_runtime",
        "selected_project_sync":sync.is_some(),"last_recorded_cycle":store.last_cycle()?,"release_approved":false}),
    )
}

fn tick(sync: Option<&Path>, staging: bool) -> Result<serde_json::Value> {
    let mut store = Store::open(sync)?;
    let record = store.record()?.ok_or(InstallError::RenewalService)?;
    let host = executable(&std::env::current_exe()?)?;
    if host.path != record.specification.host || host.digest != record.host_sha256 {
        return Err(InstallError::RenewalService);
    }
    if record.specification.staging != staging
        || record.specification.sync_data_directory
            != if sync.is_some() || !staging {
                Some(data_directory()?)
            } else {
                None
            }
    {
        return Err(InstallError::RenewalService);
    }
    let _cycle = store.cycle_lock()?;
    let current = Config::try_load_global()
        .map_err(|_| InstallError::Configuration)?
        .intelligence_runtime;
    private_read(Path::new(&current.license_configuration))?;
    store.release();
    execute_cycle(sync, staging)
}

/// Called directly by Task Scheduler, before the CLI consults global config.
pub(super) fn run_saved(binding: &Path) -> Result<serde_json::Value> {
    super::principal_windows::require_user_token()?;
    let original: Record = serde_json::from_slice(&private_read(binding)?)
        .map_err(|_| InstallError::RenewalService)?;
    let spec = &original.specification;
    let mut store = Store::at(
        &spec.home,
        &spec.configuration_directory,
        spec.sync_configuration.as_deref(),
    )?;
    if store.path() != binding || store.record()?.as_ref() != Some(&original) {
        return Err(InstallError::RenewalService);
    }
    let host = executable(&spec.host)?;
    if host.path != std::env::current_exe()?.canonicalize()? || host.digest != original.host_sha256
    {
        return Err(InstallError::RenewalService);
    }
    let state = manager::status(&store.task(&original)?)?;
    if state["registered"] != true || state["enabled"] != true {
        return Err(InstallError::RenewalService);
    }
    let mut command = Command::new(&host.path);
    command
        .env_clear()
        .env("HOME", &spec.home)
        .env("USERPROFILE", &spec.home)
        .env("LEAN_CTX_CONFIG_DIR", &spec.configuration_directory)
        .env("DO_NOT_TRACK", "1")
        .env("__LEAN_CTX_NO_DAEMON", "1")
        .current_dir(&spec.configuration_directory)
        .args([
            "engine",
            "runtime",
            if spec.sync_configuration.is_some() {
                "sync-service-tick"
            } else {
                "renewal-service-tick"
            },
        ])
        .arg("--accept-proprietary")
        .stdin(Stdio::null());
    // OS libraries/keyring use these standard locations. They do not select a
    // license, package, policy or context store.
    for name in [
        "SystemRoot",
        "WINDIR",
        "TEMP",
        "TMP",
        "LOCALAPPDATA",
        "APPDATA",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    if spec.staging {
        command.arg("--staging");
    }
    if let Some(data) = &spec.sync_data_directory {
        command.env("LEAN_CTX_DATA_DIR", data);
    }
    if let Some(sync) = &spec.sync_configuration {
        command.arg("--sync-configuration").arg(sync);
    }
    // Removal may hold the owner lock while waiting for this wrapper/child.
    // Keep the directory and executable guards alive, but release that lock.
    store.release();
    let result = crate::core::process_capture::run_with_output_limits(
        &mut command,
        Some(Duration::from_secs(if spec.sync_configuration.is_some() {
            300
        } else {
            80
        })),
        4096,
        4096,
    )
    .map_err(|_| InstallError::RenewalService)
    .and_then(|captured| {
        if captured.timed_out
            || captured.cancelled
            || !captured.output.status.success()
            || !captured.output.stderr.is_empty()
        {
            return Err(InstallError::RenewalService);
        }
        serde_json::from_slice(&captured.output.stdout).map_err(|_| InstallError::RenewalService)
    });
    if let Ok(current) = Store::at(
        &spec.home,
        &spec.configuration_directory,
        spec.sync_configuration.as_deref(),
    ) && current.record()?.as_ref() == Some(&original)
    {
        current.save_cycle(&result)?;
    }
    result
}
