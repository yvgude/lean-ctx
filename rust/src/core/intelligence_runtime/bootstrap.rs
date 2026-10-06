// SPDX-License-Identifier: Apache-2.0
//! Optional public trust policy carried by the trusted host artifact, never fetched.

use std::path::{Component, Path, PathBuf};

use crate::core::config::IntelligenceRuntimeConfig;

use super::{InstallError, Result};

// build.rs rejects a partial or malformed channel, so the all-or-none match
// below only ever sees a complete, validated triple.
const BUNDLED_STAGING_CHANNEL: Option<(&str, &str, &str)> = match (
    option_env!("LEANCTX_STAGING_RUNTIME_CHANNEL_URL"),
    option_env!("LEANCTX_STAGING_RUNTIME_CHANNEL_SIGNATURE_URL"),
    option_env!("LEANCTX_STAGING_RUNTIME_CHANNEL_ROOT_KEY_HEX"),
) {
    (Some(url), Some(signature), Some(key)) => Some((url, signature, key)),
    _ => None,
};
const BUNDLED_PRODUCTION_CHANNEL: Option<(&str, &str, &str)> = match (
    option_env!("LEANCTX_PRODUCTION_RUNTIME_CHANNEL_URL"),
    option_env!("LEANCTX_PRODUCTION_RUNTIME_CHANNEL_SIGNATURE_URL"),
    option_env!("LEANCTX_PRODUCTION_RUNTIME_CHANNEL_ROOT_KEY_HEX"),
) {
    (Some(url), Some(signature), Some(key)) => Some((url, signature, key)),
    _ => None,
};

pub(super) fn production_root() -> Result<PathBuf> {
    let data_dir =
        crate::core::data_dir::resolve_data_dir().map_err(|_| InstallError::Directory)?;
    if !data_dir.is_absolute() {
        return Err(InstallError::Directory);
    }
    let root =
        canonical_absolute_directory(&data_dir.join("intelligence-runtime-production"), true)?;
    let staging = canonical_absolute_directory(&data_dir.join("intelligence-runtime"), false)?;
    if root == staging {
        return Err(InstallError::Directory);
    }
    Ok(root)
}

pub(super) fn production_key(root: &Path) -> Result<Option<[u8; 32]>> {
    let production_root = production_root()?;
    if root != production_root.as_path() {
        return Ok(None);
    }
    let Some((url, signature_url, key)) = BUNDLED_PRODUCTION_CHANNEL else {
        return Err(InstallError::Policy);
    };
    super::channel::validate_urls(&[url, signature_url])?;
    Ok(Some(super::trust_key(key)?))
}

pub(super) fn permits_config(config: &IntelligenceRuntimeConfig) -> bool {
    let Ok(production_root) = production_root() else {
        return false;
    };
    if config.staging {
        return canonical_absolute_directory(Path::new(&config.root), false)
            .is_ok_and(|root| root != production_root);
    }
    let Some((url, signature_url, key)) = BUNDLED_PRODUCTION_CHANNEL else {
        return false;
    };
    let Some(root) = production_root.to_str() else {
        return false;
    };
    config.root == root
        && config.channel_url == url
        && config.channel_signature_url == signature_url
        && config.channel_root_key_hex == key
        && super::channel::validate_urls(&[url, signature_url]).is_ok()
        && super::trust_key(key).is_ok()
}

pub(super) fn config() -> Result<IntelligenceRuntimeConfig> {
    if let Some((url, signature_url, key)) = BUNDLED_PRODUCTION_CHANNEL {
        super::channel::validate_urls(&[url, signature_url])?;
        super::trust_key(key)?;
        let root = production_root()?;
        return Ok(IntelligenceRuntimeConfig {
            staging: false,
            root: root.to_str().ok_or(InstallError::Directory)?.to_owned(),
            channel_url: url.into(),
            channel_signature_url: signature_url.into(),
            channel_root_key_hex: key.into(),
            ..IntelligenceRuntimeConfig::default()
        });
    }
    let Some((url, signature_url, key)) = BUNDLED_STAGING_CHANNEL else {
        return Ok(IntelligenceRuntimeConfig::default());
    };
    super::channel::validate_urls(&[url, signature_url])?;
    super::trust_key(key)?;
    let parent = crate::core::data_dir::resolve_data_dir().map_err(|_| InstallError::Directory)?;
    let root = parent.join("intelligence-runtime");
    if !root.is_absolute() {
        return Err(InstallError::Directory);
    }
    Ok(IntelligenceRuntimeConfig {
        staging: true,
        root: root.to_str().ok_or(InstallError::Directory)?.to_owned(),
        channel_url: url.into(),
        channel_signature_url: signature_url.into(),
        channel_root_key_hex: key.into(),
        ..IntelligenceRuntimeConfig::default()
    })
}

// Resolve existing path components and append missing ones without creating a root.
fn canonical_absolute_directory(path: &Path, reject_leaf_symlink: bool) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| InstallError::Directory)?
            .join(path)
    };
    let mut resolved = PathBuf::new();
    let mut components = absolute.components().peekable();
    while let Some(component) = components.next() {
        match component {
            Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
            Component::RootDir => resolved.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                match std::fs::metadata(&resolved) {
                    Ok(metadata) if !metadata.is_dir() => return Err(InstallError::Directory),
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(InstallError::Directory),
                }
                if !resolved.pop() {
                    return Err(InstallError::Directory);
                }
            }
            Component::Normal(name) => {
                let candidate = resolved.join(name);
                let leaf = components
                    .clone()
                    .all(|part| matches!(part, Component::CurDir));
                match std::fs::symlink_metadata(&candidate) {
                    Ok(metadata) => {
                        if leaf && reject_leaf_symlink && metadata.file_type().is_symlink() {
                            return Err(InstallError::Directory);
                        }
                        resolved = candidate
                            .canonicalize()
                            .map_err(|_| InstallError::Directory)?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        resolved.push(name);
                    }
                    Err(_) => return Err(InstallError::Directory),
                }
            }
        }
    }
    if !resolved.is_absolute() {
        return Err(InstallError::Directory);
    }
    match std::fs::metadata(&resolved) {
        Ok(metadata) if metadata.is_dir() => {
            resolved = resolved
                .canonicalize()
                .map_err(|_| InstallError::Directory)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) | Err(_) => return Err(InstallError::Directory),
    }
    Ok(resolved)
}
