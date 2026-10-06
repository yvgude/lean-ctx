// SPDX-License-Identifier: Apache-2.0
//! Explicit staging entry points; setup uses the same install authority.

use std::collections::BTreeMap;
use std::path::Path;

use crate::core::config::{Config, IntelligenceRuntimeConfig};

use super::{InstallError, Result, install, manifest::MAX_ARCHIVE, read_regular, verify};

pub(crate) fn run(args: &[String]) -> Result<String> {
    // Restore the saved environment in a child before consulting global config.
    #[cfg(windows)]
    if args.first().is_some_and(|arg| arg == "service-run") {
        if args.len() != 3 || args[1] != "--binding" {
            return Err(InstallError::Usage);
        }
        return serde_json::to_string_pretty(&super::renewal_service::run_saved(Path::new(
            &args[2],
        ))?)
        .map_err(|_| InstallError::State);
    }
    let operation = args
        .first()
        .map(String::as_str)
        .ok_or(InstallError::Usage)?;
    if !matches!(
        operation,
        "verify"
            | "install"
            | "download"
            | "sync"
            | "rotate-root"
            | "rollback"
            | "health"
            | "invoke"
            | "activate"
            | "deactivate"
            | "status"
            | "activate-configured"
            | "sync-configured"
            | "rollback-configured"
            | "provision-configured"
            | "renew-configured"
            | "activate-personal"
            | "renewal-service-install"
            | "renewal-service-status"
            | "renewal-service-remove"
            | "renewal-service-tick"
            | "sync-service-install"
            | "sync-service-status"
            | "sync-service-remove"
            | "sync-service-tick"
            | "sync-project-enable"
            | "sync-project-invite"
            | "sync-project-approve"
            | "sync-project-join"
            | "share-identity"
            | "share-invite"
            | "share-publish"
            | "share-continue"
    ) {
        return Err(InstallError::Usage);
    }
    let mut flags = BTreeMap::new();
    let mut arguments = args[1..].iter();
    let mut staging = false;
    let mut accepted = false;
    let mut credentials_stdin = false;
    while let Some(flag) = arguments.next() {
        match flag.as_str() {
            "--staging" if !staging => staging = true,
            "--accept-proprietary" if !accepted => accepted = true,
            "--credentials-stdin" if !credentials_stdin && operation == "activate-personal" => {
                credentials_stdin = true;
            }
            "--archive"
            | "--manifest"
            | "--signature"
            | "--manifest-sha256"
            | "--trust-key-hex"
            | "--root"
            | "--expected-active"
            | "--request"
            | "--archive-url"
            | "--manifest-url"
            | "--signature-url"
            | "--channel-url"
            | "--transition"
            | "--license-config"
            | "--sync-configuration"
            | "--project"
            | "--output"
            | "--code-file"
            | "--invitation"
            | "--recipient"
            | "--context-id"
            | "--revision"
            | "--expires-at"
            | "--expected-head"
            | "--enrollment"
            | "--account-origin"
            | "--issuer-origin"
            | "--license-id"
            | "--ca-certificate"
            | "--channel-signature-url" => {
                let value = arguments.next().ok_or(InstallError::Usage)?;
                if value.starts_with("--") || flags.insert(flag.as_str(), value.as_str()).is_some()
                {
                    return Err(InstallError::Usage);
                }
            }
            _ => return Err(InstallError::Usage),
        }
    }
    let configured_operation = matches!(
        operation,
        "status"
            | "sync-configured"
            | "rollback-configured"
            | "activate-configured"
            | "activate-personal"
            | "renew-configured"
            | "provision-configured"
            | "deactivate"
            | "renewal-service-install"
            | "renewal-service-status"
            | "renewal-service-remove"
            | "renewal-service-tick"
            | "sync-service-install"
            | "sync-service-status"
            | "sync-service-remove"
            | "sync-service-tick"
            | "sync-project-enable"
            | "sync-project-invite"
            | "sync-project-approve"
            | "sync-project-join"
            | "share-identity"
            | "share-invite"
            | "share-publish"
            | "share-continue"
    );
    if !staging && !configured_operation {
        return Err(InstallError::Usage);
    }
    if configured_operation
        && !matches!(
            operation,
            "status"
                | "deactivate"
                | "renewal-service-status"
                | "renewal-service-remove"
                | "sync-service-status"
                | "sync-service-remove"
        )
    {
        let observed = Config::try_load_global()
            .map_err(|_| InstallError::Configuration)?
            .intelligence_runtime;
        let configured = if observed == IntelligenceRuntimeConfig::default() {
            super::bootstrap::config()?
        } else {
            observed
        };
        if configured != IntelligenceRuntimeConfig::default()
            && (configured.staging != staging || !super::bootstrap::permits_config(&configured))
        {
            return Err(InstallError::Usage);
        }
    }
    if staging
        && let Some(root) = flags.get("--root")
        && super::bootstrap::production_key(Path::new(root))?.is_some()
    {
        return Err(InstallError::Usage);
    }
    if !matches!(
        operation,
        "verify"
            | "deactivate"
            | "status"
            | "renewal-service-status"
            | "renewal-service-remove"
            | "sync-service-status"
            | "sync-service-remove"
    ) && !accepted
    {
        return Err(InstallError::Usage);
    }
    let get = |name| flags.get(name).copied().ok_or(InstallError::Usage);
    if operation.starts_with("share-") {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        return serde_json::to_string_pretty(&super::context_sharing::run(operation, &flags)?)
            .map_err(|_| InstallError::State);
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        return Err(InstallError::Usage);
    }
    if operation.starts_with("renewal-service-") {
        if !flags.is_empty() {
            return Err(InstallError::Usage);
        }
        return serde_json::to_string_pretty(&super::renewal_service::run_with_mode(
            operation, None, staging,
        )?)
        .map_err(|_| InstallError::State);
    }
    if operation.starts_with("sync-service-") {
        if flags.len() != 1 {
            return Err(InstallError::Usage);
        }
        return serde_json::to_string_pretty(&super::renewal_service::run_with_mode(
            operation,
            Some(Path::new(get("--sync-configuration")?)),
            staging,
        )?)
        .map_err(|_| InstallError::State);
    }
    if operation.starts_with("sync-project-") {
        let count = if matches!(operation, "sync-project-approve" | "sync-project-join") {
            2
        } else {
            1
        };
        if flags.len() != count {
            return Err(InstallError::Usage);
        }
        #[cfg(any(target_os = "macos", target_os = "linux", windows))]
        {
            let result = match operation {
                "sync-project-enable" => super::project_sync::enable(Path::new(get("--project")?)),
                "sync-project-invite" => super::project_sync::invite(Path::new(get("--output")?)),
                "sync-project-approve" => super::project_sync::approve(
                    Path::new(get("--project")?),
                    Path::new(get("--code-file")?),
                ),
                "sync-project-join" => {
                    super::project_sync::join(Path::new(get("--project")?), get("--invitation")?)
                }
                _ => Err(InstallError::Usage),
            }?;
            return serde_json::to_string_pretty(&result).map_err(|_| InstallError::State);
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
        return Err(InstallError::Usage);
    }
    if operation == "activate-personal" {
        return serde_json::to_string_pretty(&super::activation::activate(
            &flags,
            credentials_stdin,
        )?)
        .map_err(|_| InstallError::State);
    }
    if operation == "renew-configured" {
        if !flags.is_empty() {
            return Err(InstallError::Usage);
        }
        return serde_json::to_string_pretty(&super::provisioning::renew()?)
            .map_err(|_| InstallError::State);
    }
    if operation == "provision-configured" {
        if flags.len() != 2 {
            return Err(InstallError::Usage);
        }
        let result = super::provisioning::provision(
            Path::new(get("--license-config")?),
            Path::new(get("--enrollment")?),
        )?;
        return serde_json::to_string_pretty(&result).map_err(|_| InstallError::State);
    }
    if operation == "rotate-root" {
        if flags.len() != 4 {
            return Err(InstallError::Usage);
        }
        let proof = serde_json::from_slice(&read_regular(Path::new(get("--transition")?), 4096)?)
            .map_err(|_| InstallError::Manifest)?;
        let result = install::rotate_root(
            Path::new(get("--root")?),
            get("--expected-active")?,
            &super::trust_key(get("--trust-key-hex")?)?,
            proof,
        )?;
        return serde_json::to_string_pretty(&result).map_err(|_| InstallError::State);
    }
    if operation == "sync" {
        if flags.len() != 5 {
            return Err(InstallError::Usage);
        }
        let result = super::catalog::sync(
            [get("--channel-url")?, get("--channel-signature-url")?],
            &super::trust_key(get("--trust-key-hex")?)?,
            Path::new(get("--root")?),
            get("--expected-active")?,
        )?;
        return serde_json::to_string_pretty(&result).map_err(|_| InstallError::State);
    }
    if matches!(
        operation,
        "status" | "activate-configured" | "sync-configured" | "rollback-configured"
    ) {
        if !flags.is_empty() {
            return Err(InstallError::Usage);
        }
        let candidate = super::configuration::inspect_setup()?;
        let result = if operation == "status" {
            candidate.report()
        } else if operation == "sync-configured" {
            candidate.install_and_enable()?
        } else if operation == "rollback-configured" {
            candidate.rollback_and_enable()?
        } else {
            candidate.enable()?
        };
        return serde_json::to_string_pretty(&result).map_err(|_| InstallError::State);
    }
    if operation == "deactivate" {
        if !flags.is_empty() {
            return Err(InstallError::Usage);
        }
        let result = super::configuration::deactivate()?;
        return serde_json::to_string_pretty(&result).map_err(|_| InstallError::State);
    }
    let selected = get("--manifest-sha256")?;
    let encoded = get("--trust-key-hex")?;
    let key = super::trust_key(encoded)?;
    let result = if operation == "activate" {
        if flags.len() != 4 || get("--expected-active")? != selected {
            return Err(InstallError::Usage);
        }
        super::configuration::activate(Path::new(get("--root")?), selected, encoded)?
    } else if operation == "invoke" {
        if flags.len() != 5 || get("--expected-active")? != selected {
            return Err(InstallError::Usage);
        }
        #[cfg(not(unix))]
        return Err(InstallError::Exchange);
        #[cfg(unix)]
        {
            // Explicit staging operator input, NOT production policy admission.
            let bytes = read_regular(
                Path::new(get("--request")?),
                lean_ctx_protocol::runtime_exchange::MAX_RUNTIME_EXCHANGE_BYTES,
            )?;
            let request = serde_json::from_slice(&bytes).map_err(|_| InstallError::Exchange)?;
            super::session::invoke(Path::new(get("--root")?), selected, &key, &request)?
        }
    } else if operation == "health" {
        if flags.len() != 4 || get("--expected-active")? != selected {
            return Err(InstallError::Usage);
        }
        super::health::check(Path::new(get("--root")?), selected, &key)?
    } else if operation == "rollback" {
        if flags.len() != 4 {
            return Err(InstallError::Usage);
        }
        install::rollback(
            Path::new(get("--root")?),
            get("--expected-active")?,
            selected,
            &key,
        )?
    } else {
        if flags.len() != if operation == "verify" { 5 } else { 7 } {
            return Err(InstallError::Usage);
        }
        let (archive, manifest, signature) = if operation == "download" {
            get("--root")?;
            get("--expected-active")?;
            let fetched = super::channel::download(
                [
                    get("--archive-url")?,
                    get("--manifest-url")?,
                    get("--signature-url")?,
                ],
                selected,
                &key,
            )?;
            (fetched.archive, fetched.manifest, fetched.signature)
        } else {
            (
                read_regular(Path::new(get("--archive")?), MAX_ARCHIVE)?,
                read_regular(Path::new(get("--manifest")?), 65_536)?,
                read_regular(Path::new(get("--signature")?), 64)?,
            )
        };
        let package = verify(&archive, &manifest, &signature, selected, &key)?;
        if operation == "verify" {
            serde_json::json!({"status": "verified", "receipt": package.receipt,
                "runtime_started": false, "release_approved": false})
        } else {
            install::install(
                Path::new(get("--root")?),
                get("--expected-active")?,
                &package,
                &archive,
                &manifest,
                &signature,
            )?
        }
    };
    serde_json::to_string_pretty(&result).map_err(|_| InstallError::State)
}
