// SPDX-License-Identifier: Apache-2.0
//! Explicit staging entry points; setup uses the same install authority.

use std::collections::BTreeMap;
use std::path::Path;

use crate::core::config::{Config, IntelligenceRuntimeConfig};

use super::{
    InstallError, Result, RuntimeKind, install, manifest::MAX_ARCHIVE, read_regular, verify_kind,
};

pub(crate) fn run(args: &[String]) -> Result<String> {
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
    ) {
        return Err(InstallError::Usage);
    }
    let mut flags = BTreeMap::new();
    let mut arguments = args[1..].iter();
    let mut staging = false;
    let mut accepted = false;
    while let Some(flag) = arguments.next() {
        match flag.as_str() {
            "--staging" if !staging => staging = true,
            "--accept-proprietary" if !accepted => accepted = true,
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
            | "--semantic-models"
            | "--kind"
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
    // The companions of a v4 package are read from local files, with the
    // package they belong to, and by no other operation.
    let companions = flags.get("--semantic-models").copied().map(Path::new);
    if companions.is_some() && operation != "install" {
        return Err(InstallError::Usage);
    }
    // `--kind cockpit` selects the Cockpit package kind; it never counts as a
    // package flag. Its stores are named apart, and each kind refuses the other's.
    let kind = match flags.remove("--kind") {
        None | Some("intelligence") => RuntimeKind::Intelligence,
        Some("cockpit") => RuntimeKind::Cockpit,
        Some(_) => return Err(InstallError::Usage),
    };
    if let Some(root) = flags.get("--root") {
        let cockpit_store = Path::new(root)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(RuntimeKind::Cockpit.store_name()));
        if cockpit_store != (kind == RuntimeKind::Cockpit) {
            return Err(InstallError::Usage);
        }
    }
    if kind == RuntimeKind::Cockpit {
        match operation {
            "status" | "sync-configured" | "activate" | "deactivate" => {
                return serde_json::to_string_pretty(&super::cockpit::run_cli(
                    operation, staging, accepted, &flags,
                )?)
                .map_err(|_| InstallError::State);
            }
            "verify" | "install" | "download" | "sync" | "rotate-root" if companions.is_none() => {}
            _ => return Err(InstallError::Usage),
        }
    }
    let configured_operation = matches!(
        operation,
        "status" | "sync-configured" | "rollback-configured" | "activate-configured" | "deactivate"
    );
    if !staging && !configured_operation {
        return Err(InstallError::Usage);
    }
    if configured_operation && !matches!(operation, "status" | "deactivate") {
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
    if !matches!(operation, "verify" | "deactivate" | "status") && !accepted {
        return Err(InstallError::Usage);
    }
    let get = |name| flags.get(name).copied().ok_or(InstallError::Usage);
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
        let result = super::catalog::sync_kind(
            kind,
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
            let answer =
                super::session::invoke(Path::new(get("--root")?), selected, &key, &request)?;
            // G08: the runtime's answer is foreign content entering the host.
            // Where protection is active its narrative is admitted by the
            // Context Gateway — bound: unchanged, or the answer is not
            // written — before it becomes this command's document.
            crate::core::execution_ledger::host::admit_foreign_document_narrative(
                "runtime-invoke:",
                &answer,
                "engine-runtime-invoke",
            )
            .map_err(|_| InstallError::Exchange)?;
            answer
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
        let expected = if operation == "verify" { 5 } else { 7 };
        if flags.len() != expected + usize::from(companions.is_some()) {
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
        let package = verify_kind(kind, &archive, &manifest, &signature, selected, &key)?;
        if operation == "verify" {
            serde_json::json!({"status": "verified", "receipt": package.receipt,
                "runtime_started": false, "release_approved": false})
        } else {
            let root = Path::new(get("--root")?);
            let expected_active = get("--expected-active")?;
            if let Some(companions) = companions {
                install::install_with_companions(
                    root,
                    expected_active,
                    &package,
                    &archive,
                    &manifest,
                    &signature,
                    companions,
                )?
            } else {
                install::install(
                    root,
                    expected_active,
                    &package,
                    &archive,
                    &manifest,
                    &signature,
                )?
            }
        }
    };
    serde_json::to_string_pretty(&result).map_err(|_| InstallError::State)
}
