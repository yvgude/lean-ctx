// SPDX-License-Identifier: Apache-2.0
//! Explicit staging downloads feed the existing verifier and atomic installer.

use std::io::Read;
use std::time::Duration;

use super::{InstallError, Result, manifest};

pub(super) struct Download {
    pub archive: Vec<u8>,
    pub manifest: Vec<u8>,
    pub signature: Vec<u8>,
}

pub(super) fn download(urls: [&str; 3], selected: &str, key: &[u8; 32]) -> Result<Download> {
    if !super::is_digest(selected, 64) {
        return Err(InstallError::Manifest);
    }
    validate_urls(&urls)?;
    let manifest = fetch(urls[1], 65_536)?;
    let signature = fetch(urls[2], 64)?;
    manifest::verify_manifest(&manifest, &signature, selected, key)?;
    // An unauthenticated manifest cannot cause an archive download or disk write.
    let archive = fetch(urls[0], manifest::MAX_ARCHIVE)?;
    Ok(Download {
        archive,
        manifest,
        signature,
    })
}

pub(super) fn validate_urls(urls: &[&str]) -> Result<()> {
    // Validate every destination before opening any connection. HTTP is confined
    // to literal loopback for isolated staging; it never establishes trust.
    for value in urls {
        let uri = value
            .parse::<ureq::http::Uri>()
            .map_err(|_| InstallError::Download)?;
        if value.len() > 2048
            || value
                .bytes()
                .any(|byte| byte <= b' ' || byte >= 127 || byte == b'#')
            || uri
                .authority()
                .is_none_or(|authority| authority.as_str().contains('@'))
            || uri.query().is_some()
            || !(uri.scheme_str() == Some("https")
                || uri.scheme_str() == Some("http")
                    && matches!(uri.host(), Some("127.0.0.1" | "[::1]")))
        {
            return Err(InstallError::Download);
        }
    }
    Ok(())
}

pub(super) fn fetch(url: &str, limit: usize) -> Result<Vec<u8>> {
    validate_urls(&[url])?;
    let agent = crate::core::http_client::ureq_agent(
        ureq::config::Config::builder()
            .tls_config(crate::core::http_client::platform_tls_config())
            .proxy(None)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_mins(2)))
            .timeout_resolve(Some(Duration::from_secs(10)))
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(15)))
            .build(),
    );
    let response = agent
        .get(url)
        .header(
            "User-Agent",
            concat!("lean-ctx/", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|_| InstallError::Download)?;
    if response.status() != ureq::http::StatusCode::OK {
        return Err(InstallError::Download);
    }
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| InstallError::Download)?;
    if bytes.len() > limit {
        return Err(InstallError::Size);
    }
    Ok(bytes)
}
