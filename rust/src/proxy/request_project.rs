// SPDX-License-Identifier: Apache-2.0
//! The one project the proxy acts for.
//!
//! A proxied request names no project the gateway could trust: nothing in a
//! header, a gateway tag, a body field or a path selects or overrides it.
//! The project is the operator's, resolved in this order:
//!
//! 1. the configured root (`[proxy] project_root`, env
//!    `LEAN_CTX_PROXY_PROJECT_ROOT`), verified at every use: absolute,
//!    existing, canonical, not a broad or unsafe root (the rule the Engine
//!    applies to a project root, `pathutil::is_broad_or_unsafe_root`), and —
//!    under a protected launch (`LEAN_CTX_REQUIRED_POLICY_ROOT`) — equal to
//!    the pinned root. A configured root that fails any of this is not
//!    replaced by something else: the proxy refuses.
//! 2. else the launch-pinned root.
//! 3. else none.
//!
//! The process working directory is never a project here. (The admission
//! still applies a policy pack it finds from the working directory to the
//! content of a request, as it always did — that only ever withholds. It is
//! not a project that sources are admitted for.)
//!
//! [`scope`] is what an admission runs in; [`source_project`] is the project
//! that sources of provider-sealed items and recovery copies are looked up
//! and re-admitted for: the configured root, or the pinned root when a policy
//! pack verifies there.

use std::path::{Path, PathBuf};

use crate::core::config::Config;
use crate::core::policy::runtime as project_policy;

/// The configured root cannot be used. Nothing is admitted or sent.
pub(crate) const ROOT_UNUSABLE: &str = "gateway.project_root_unusable";
/// The configured root is not the root this process was launched for.
pub(crate) const ROOT_MISMATCH: &str = "gateway.project_root_mismatch";

/// Where the proxy's project comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProxyProject {
    /// The operator's configured root, verified now.
    Configured(PathBuf),
    /// The root this process was launched for, without a configured one.
    Pinned(PathBuf),
    None,
}

/// A root that exists, is a directory and is not too broad to be a project.
fn verified(root: &Path) -> Option<PathBuf> {
    if !root.is_absolute() || crate::core::pathutil::is_broad_or_unsafe_root(root) {
        return None;
    }
    let canonical = crate::core::pathutil::canonicalize_secure(root).ok()?;
    (canonical.is_dir() && !crate::core::pathutil::is_broad_or_unsafe_root(&canonical))
        .then_some(canonical)
}

/// The proxy's project under `config`, verified at this moment. `Err` is a
/// content-free reason code: the configuration names a project the proxy
/// cannot act for.
pub(crate) fn resolve(config: &Config) -> Result<ProxyProject, &'static str> {
    resolve_from(
        config.proxy.configured_project_root().as_deref(),
        std::env::var_os(project_policy::REQUIRED_POLICY_ROOT_ENV).as_deref(),
    )
}

/// [`resolve`] for a configured root and a launch pin as written.
fn resolve_from(
    configured: Option<&str>,
    pinned: Option<&std::ffi::OsStr>,
) -> Result<ProxyProject, &'static str> {
    let pinned = match pinned {
        // A pin that cannot be verified is not ignored.
        Some(pin) => Some(verified(Path::new(pin)).ok_or(ROOT_MISMATCH)?),
        None => None,
    };
    let Some(configured) = configured else {
        return Ok(pinned.map_or(ProxyProject::None, ProxyProject::Pinned));
    };
    let configured = verified(Path::new(configured.trim())).ok_or(ROOT_UNUSABLE)?;
    if pinned.is_some_and(|pinned| pinned != configured) {
        return Err(ROOT_MISMATCH);
    }
    Ok(ProxyProject::Configured(configured))
}

impl ProxyProject {
    /// The request project an admission runs in, if any.
    #[cfg(feature = "http-server")]
    pub(crate) fn scope(&self) -> Option<&Path> {
        match self {
            Self::Configured(root) | Self::Pinned(root) => Some(root),
            Self::None => None,
        }
    }

    /// The project sources are admitted for: the configured root; the pinned
    /// root only when a policy pack verifies there; otherwise none.
    pub(crate) fn source_project(&self) -> Option<PathBuf> {
        match self {
            Self::Configured(root) => Some(root.clone()),
            Self::Pinned(root) => project_policy::for_project(root)
                .ok()
                .flatten()
                .map(|_| root.clone()),
            Self::None => None,
        }
    }

    /// [`Self::source_project`] decided by a verification the caller already
    /// holds instead of a further one: `policy_verified` is whether exactly
    /// that verification found a policy for this root. The configured root
    /// needs none; the pinned root is a source project only with one — an
    /// observed organization decision alone does not make it one.
    #[cfg(feature = "http-server")]
    pub(crate) fn source_project_under(&self, policy_verified: bool) -> Option<PathBuf> {
        match self {
            Self::Configured(root) => Some(root.clone()),
            Self::Pinned(root) => policy_verified.then(|| root.clone()),
            Self::None => None,
        }
    }
}

/// Run `operation` in the request scope of `root`.
pub(crate) fn in_project<T>(root: &Path, operation: impl FnOnce() -> T) -> T {
    project_policy::REQUEST_PROJECT
        .sync_scope(std::cell::RefCell::new(Some(root.to_path_buf())), operation)
}

/// Request header in which a host shows its presenter capability to the
/// proxy (`core::archive::presented`, "Presenters"). An internal gateway
/// header: not on `forward::headers::ALLOWED_REQUEST_HEADERS`, so the relay
/// never forwards it. Nothing in a request body, a path or a query is ever
/// read as a capability.
#[cfg(feature = "http-server")]
pub(crate) const PRESENTER_HEADER: &str = "x-leanctx-presenter";

/// Take the presenter capability out of the request headers: every value of
/// [`PRESENTER_HEADER`] is removed here, before the request is handled, and
/// exactly one well-formed value is a capability. A host that sends none (or
/// several, or a malformed one) is served under this process's own
/// capability, i.e. it reuses no presentation made elsewhere.
///
/// What this establishes is that the request comes from a holder of that
/// capability — not who the host is. Every source of a lineage is still
/// re-admitted under the current policy.
#[cfg(feature = "http-server")]
pub(crate) fn take_presenter(
    headers: &mut axum::http::HeaderMap,
) -> Option<crate::core::archive::presented::PresenterCapability> {
    let shown = shown_presenter(headers);
    headers.remove(PRESENTER_HEADER);
    shown
}

/// The presenter capability in `headers`, without removing it: for an entry
/// that only borrows the headers (a WebSocket upgrade) and forwards by an
/// allowlist or by a list that names [`PRESENTER_HEADER`].
#[cfg(feature = "http-server")]
pub(crate) fn shown_presenter(
    headers: &axum::http::HeaderMap,
) -> Option<crate::core::archive::presented::PresenterCapability> {
    let mut values = headers.get_all(PRESENTER_HEADER).iter();
    match (values.next(), values.next()) {
        (Some(only), None) => only
            .to_str()
            .ok()
            .and_then(crate::core::archive::presented::PresenterCapability::parse),
        _ => None,
    }
}

/// Handle one proxied request as the request of a host that showed
/// `presenter`. Request state, like the request project: the lineage checks
/// inside [`in_project`] and `sealed_origin` read it; nothing else does.
#[cfg(feature = "http-server")]
pub(crate) async fn as_presenter<F: std::future::Future>(
    presenter: Option<crate::core::archive::presented::PresenterCapability>,
    request: F,
) -> F::Output {
    crate::core::archive::presented::presenter_scope(presenter, request).await
}

/// The project sources are admitted for under the current configuration, or
/// `None` — also when the configuration names an unusable root.
pub(crate) fn current_source_project() -> Option<PathBuf> {
    resolve(&Config::load_arc())
        .ok()
        .and_then(|project| project.source_project())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_root(root: &str) -> Result<ProxyProject, &'static str> {
        resolve_from(Some(root), None)
    }

    /// Under a protected launch the configured root must be the pinned one:
    /// another root refuses instead of preferring either; without a
    /// configured root the pinned one is the proxy's project.
    #[test]
    fn configured_root_must_equal_the_launch_pinned_root() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let one = tempfile::tempdir().expect("project");
        let other = tempfile::tempdir().expect("other project");
        // The crate's canonical spelling: no Windows verbatim prefix.
        let root = crate::core::pathutil::canonicalize_secure(one.path()).expect("canonical root");
        let pin = one.path().as_os_str();
        let configured =
            |directory: &tempfile::TempDir| resolve_from(directory.path().to_str(), Some(pin));
        assert_eq!(configured(&one), Ok(ProxyProject::Configured(root.clone())));
        assert_eq!(configured(&other), Err(ROOT_MISMATCH));
        assert_eq!(
            resolve_from(None, Some(pin)),
            Ok(ProxyProject::Pinned(root))
        );
        let gone = one.path().join("gone");
        assert_eq!(
            resolve_from(None, Some(gone.as_os_str())),
            Err(ROOT_MISMATCH),
            "a pin that cannot be verified is not ignored"
        );
    }

    /// A configured root is used only as a verified, canonical project
    /// directory; a relative path, a missing directory, a file and a root too
    /// broad to be a project are refused, not replaced.
    #[test]
    fn configured_root_is_verified_and_never_replaced() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let directory = tempfile::tempdir().expect("project");
        let root =
            crate::core::pathutil::canonicalize_secure(directory.path()).expect("canonical root");
        assert_eq!(
            with_root(directory.path().to_str().expect("utf-8")),
            Ok(ProxyProject::Configured(root.clone()))
        );
        assert_eq!(resolve_from(None, None), Ok(ProxyProject::None));
        assert_eq!(ProxyProject::None.source_project(), None);
        assert_eq!(
            ProxyProject::Configured(root.clone()).source_project(),
            Some(root.clone())
        );
        assert_eq!(
            ProxyProject::Pinned(root.clone()).source_project(),
            None,
            "a pinned root without a verified pack is not a source project"
        );

        let file = root.join("file.txt");
        std::fs::write(&file, "x").expect("file");
        let missing = root.join("missing");
        for unusable in [
            "relative/project",
            ".",
            "/",
            missing.to_str().expect("utf-8"),
            file.to_str().expect("utf-8"),
        ] {
            assert_eq!(with_root(unusable), Err(ROOT_UNUSABLE), "{unusable}");
        }
        if let Some(home) = dirs::home_dir().and_then(|home| home.to_str().map(str::to_owned)) {
            assert_eq!(with_root(&home), Err(ROOT_UNUSABLE));
        }
    }
}
