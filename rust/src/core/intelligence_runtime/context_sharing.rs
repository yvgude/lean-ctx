// SPDX-License-Identifier: Apache-2.0
//! Explicit user commands for the installed private sharing implementation.
use super::{InstallError, Result, health, install, read_regular, sha256, trust_key};
use crate::core::{config::Config, pathutil};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, process::Stdio, time::Duration};

pub(super) fn run(operation: &str, flags: &BTreeMap<&str, &str>) -> Result<Value> {
    let allowed: &[&str] = match operation {
        "share-identity" => &[],
        "share-invite" => &[
            "--recipient",
            "--output",
            "--context-id",
            "--revision",
            "--expires-at",
        ],
        "share-publish" => &["--project", "--invitation"],
        "share-continue" => &["--project", "--invitation", "--expected-head"],
        _ => return Err(InstallError::Usage),
    };
    if flags.keys().any(|key| !allowed.contains(key)) {
        return Err(InstallError::Usage);
    }
    let get = |name| flags.get(name).copied().ok_or(InstallError::Usage);
    let absolute = |name| {
        let path = Path::new(get(name)?);
        if !path.is_absolute() {
            return Err(InstallError::Usage);
        }
        Ok(path)
    };
    let selection = if operation == "share-invite" {
        let recipient =
            uuid::Uuid::parse_str(get("--recipient")?).map_err(|_| InstallError::Usage)?;
        absolute("--output")?;
        let context = flags.get("--context-id").map_or_else(
            || Ok(uuid::Uuid::new_v4()),
            |value| uuid::Uuid::parse_str(value).map_err(|_| InstallError::Usage),
        )?;
        let revision = flags
            .get("--revision")
            .map_or(Ok(1_i64), |v| v.parse())
            .map_err(|_| InstallError::Usage)?;
        let now = chrono::Utc::now();
        let expires = flags
            .get("--expires-at")
            .map_or_else(
                || Ok(now + chrono::Duration::days(1)),
                |v| chrono::DateTime::parse_from_rfc3339(v).map(|v| v.with_timezone(&chrono::Utc)),
            )
            .map_err(|_| InstallError::Usage)?;
        if revision < 1 || expires <= now || expires > now + chrono::Duration::days(365) {
            return Err(InstallError::Usage);
        }
        Some(
            json!({"schema_version":1,"recipient_account_id":recipient.to_string(),
            "context_id":context.to_string(),"revision":revision,"expires_at":expires}),
        )
    } else {
        None
    };
    let project = if matches!(operation, "share-publish" | "share-continue") {
        absolute("--invitation")?;
        let path = pathutil::canonicalize_secure(absolute("--project")?)
            .map_err(|_| InstallError::Directory)?;
        if !path.is_dir() || pathutil::is_broad_or_unsafe_root(&path) {
            return Err(InstallError::Directory);
        }
        Some(path)
    } else {
        None
    };
    let expected_head = flags.get("--expected-head").copied();
    if expected_head.is_some_and(|value| !super::is_digest(value, 64)) {
        return Err(InstallError::Usage);
    }

    let observed = Config::try_load_global()
        .map_err(|_| InstallError::Configuration)?
        .intelligence_runtime;
    let license = Path::new(&observed.license_configuration);
    if !(observed.enabled
        && observed.accept_proprietary
        && super::bootstrap::permits_config(&observed)
        && license.is_absolute())
    {
        return Err(InstallError::Configuration);
    }
    let original = read_regular(license, 16 * 1024)?;
    let binding: Value =
        serde_json::from_slice(&original).map_err(|_| InstallError::Configuration)?;
    let package = install::verified_active(
        Path::new(&observed.root),
        &observed.manifest_sha256,
        &trust_key(&observed.trust_key_hex)?,
    )?;
    if package.description["entitlement_required"] != true {
        return Err(InstallError::Policy);
    }
    let (temporary, mut command) = health::prepare(&package, Path::new(&observed.root))?;
    let workspace = temporary.path().canonicalize()?;
    command
        .env(
            "HOME",
            std::env::var_os("HOME").ok_or(InstallError::Configuration)?,
        )
        .env(
            "LEAN_CTX_DATA_DIR",
            crate::core::paths::data_dir_read_only().map_err(|_| InstallError::Configuration)?,
        )
        .env(
            "LEAN_CTX_CONFIG_DIR",
            crate::core::paths::config_dir_read_only().map_err(|_| InstallError::Configuration)?,
        )
        .env("DO_NOT_TRACK", "1")
        .env("__LEAN_CTX_NO_DAEMON", "1")
        .env("LEANCTX_INTELLIGENCE_LICENSE_CONFIG", license)
        .arg(format!("--{operation}"))
        .stdin(Stdio::null());
    if let Some(selection) = selection {
        let path = workspace.join("selection.json");
        install::write_new(
            &path,
            &serde_json::to_vec(&selection).map_err(|_| InstallError::Exchange)?,
            false,
        )?;
        command.arg(path).arg(absolute("--output")?);
    }
    if let Some(project) = project {
        let engine = pathutil::canonicalize_secure(&std::env::current_exe()?)
            .map_err(|_| InstallError::Configuration)?;
        let engine_hash = sha256(&read_regular(&engine, 512 * 1024 * 1024)?);
        let host = workspace.join("host.json");
        install::write_new(&host, b"{}", false)?;
        let path = workspace.join("engine.json");
        // The private component supplies its current licensed sharing grant.
        // No caller-selected engine, signing key, or automatic overwrite head.
        let value = json!({"schema_version":1,"engine_path":engine,"engine_sha256":engine_hash,
            "project_root":project,"host_settings_path":host,"expected_head":expected_head});
        install::write_new(
            &path,
            &serde_json::to_vec(&value).map_err(|_| InstallError::Exchange)?,
            false,
        )?;
        command.arg(absolute("--invitation")?).arg(path);
    }
    let captured = crate::core::process_capture::run_with_output_limits(
        &mut command,
        Some(Duration::from_secs(90)),
        8192,
        4096,
    )
    .map_err(|_| InstallError::Exchange)?;
    if captured.timed_out
        || captured.cancelled
        || !captured.output.status.success()
        || !captured.output.stderr.is_empty()
    {
        // Private diagnostics, invitation secrets and source text never become CLI errors.
        return Err(InstallError::Exchange);
    }
    let reply: Value =
        serde_json::from_slice(&captured.output.stdout).map_err(|_| InstallError::Exchange)?;
    if !valid_reply(operation, &reply, &binding["installation_id"]) {
        return Err(InstallError::Exchange);
    }
    temporary.close()?;
    if read_regular(license, 16 * 1024)? != original
        || Config::try_load_global()
            .map_err(|_| InstallError::Configuration)?
            .intelligence_runtime
            != observed
    {
        return Err(InstallError::Configuration);
    }
    Ok(reply)
}

fn uuid(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
}
fn digest(value: &Value) -> bool {
    value.as_str().is_some_and(|s| super::is_digest(s, 64))
}
fn descriptor(value: &Value) -> bool {
    value.as_object().is_some_and(|v| v.len() == 7)
        && [
            "id",
            "sender_account_id",
            "recipient_account_id",
            "context_id",
        ]
        .into_iter()
        .all(|key| uuid(&value[key]))
        && value["key_id"]
            .as_str()
            .is_some_and(|key| super::is_digest(key, 32))
        && value["revision"].as_i64().is_some_and(|v| v > 0)
        && value["expires_at"]
            .as_str()
            .is_some_and(|v| chrono::DateTime::parse_from_rfc3339(v).is_ok())
}
fn valid_reply(operation: &str, value: &Value, device: &Value) -> bool {
    let Some(fields) = value.as_object() else {
        return false;
    };
    match operation {
        "share-identity" => {
            fields.len() == 2
                && uuid(&value["account_id"])
                && device.is_string()
                && value["device_id"] == *device
        }
        "share-invite" => {
            fields.len() == 3
                && value["prepared"] == true
                && value["secret_invitation_file"] == true
                && descriptor(&value["descriptor"])
        }
        "share-publish" => {
            descriptor(&value["descriptor"])
                && fields.len() == 3
                && ((value["published"] == true && digest(&value["payload_sha256"]))
                    || (value["published"]
                        .as_bool()
                        .is_some_and(|published| value["conflict"] == !published)))
        }
        "share-continue" => {
            value["schema_version"] == "leanctx.personal-sync/v1"
                && ((fields.len() == 3 && value["received"] == false && value["conflict"] == true)
                    || (fields.len() == 5
                        && value["received"] == true
                        && value["conflict"] == false
                        && digest(&value["head"])
                        && digest(&value["payload_sha256"])))
        }
        _ => false,
    }
}
