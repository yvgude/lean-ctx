// SPDX-License-Identifier: Apache-2.0
//! Bounded execution of an authenticated staging package, never a mutable path.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::{InstallError, Result, VerifiedPackage, install};

pub(super) fn check(root: &Path, selected: &str, key: &[u8; 32]) -> Result<serde_json::Value> {
    let package = install::verified_active(root, selected, key)?;
    let (temporary, mut command) = prepare(&package, root)?;
    command.arg("--describe").stdin(Stdio::null());
    let captured = crate::core::process_capture::run_with_output_limits(
        &mut command,
        Some(Duration::from_secs(2)),
        65_536,
        4_096,
    )
    .map_err(|_| InstallError::Health)?;
    if captured.timed_out
        || captured.cancelled
        || !captured.output.status.success()
        || !captured.output.stderr.is_empty()
    {
        return Err(InstallError::Health);
    }
    let actual: serde_json::Value =
        serde_json::from_slice(&captured.output.stdout).map_err(|_| InstallError::Health)?;
    if actual != package.description {
        return Err(InstallError::Health);
    }
    temporary.close()?;
    Ok(serde_json::json!({
        "status": "healthy", "receipt": package.receipt, "description": actual,
        "runtime_started": true, "service_running": false, "user_state_modified": false,
        "release_approved": false
    }))
}

/// Keep the short socket workspace and authenticated executable alive together.
pub(super) struct Prepared {
    workspace: install::Temporary,
    #[cfg(target_os = "linux")]
    executable: tempfile::TempDir,
}

impl Prepared {
    #[cfg(unix)]
    pub(super) fn path(&self) -> &Path {
        self.workspace.path()
    }

    pub(super) fn close(self) -> std::io::Result<()> {
        let workspace = self.workspace.close();
        #[cfg(target_os = "linux")]
        {
            // Attempt both cleanups even when the first one fails.
            let executable = self.executable.close();
            workspace.and(executable)
        }
        #[cfg(not(target_os = "linux"))]
        workspace
    }
}

pub(super) fn prepare(package: &VerifiedPackage, root: &Path) -> Result<(Prepared, Command)> {
    // This host currently admits only the delivered routing contract. A new
    // capability/version needs explicit host support, not discovery by execution.
    routing_version(package)?;
    #[cfg(not(windows))]
    let temporary = {
        let mut builder = tempfile::Builder::new();
        builder.prefix("leanctx-runtime-health-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        // Short canonical paths are required by Unix socket address length limits.
        #[cfg(unix)]
        {
            builder.tempdir_in("/tmp")?
        }
        #[cfg(not(unix))]
        {
            builder.tempdir()?
        }
    };
    #[cfg(windows)]
    let temporary = {
        let parent = crate::core::windows_private::Directory::create(
            &std::env::temp_dir().join("leanctx-runtime-private"),
        )?;
        install::temporary_directory(parent.path(), "health-")?
    };
    let directory = temporary.path().canonicalize()?;
    #[cfg(target_os = "linux")]
    let executable_directory = install::execution_directory(root)?;
    #[cfg(target_os = "linux")]
    let executable = executable_directory.path().join(super::EXECUTABLE_NAME);
    #[cfg(not(target_os = "linux"))]
    let _ = root;
    #[cfg(not(target_os = "linux"))]
    let executable = directory.join(super::EXECUTABLE_NAME);
    // Execute bytes authenticated above, not the separately mutable installed
    // inode. This is private process hygiene, not a hostile same-UID sandbox.
    install::write_new(&executable, &package.binary, true)?;
    let mut command = Command::new(&executable);
    command
        .current_dir(&directory)
        .env_clear()
        .env("HOME", &directory)
        .env("TMPDIR", &directory);
    // libdbus discovers the logged-in user's Secret Service through this
    // standard location. Never inherit a caller-selected remote D-Bus address.
    #[cfg(target_os = "linux")]
    {
        // SAFETY: geteuid only reads this process identity and has no arguments.
        let uid = unsafe { libc::geteuid() };
        command.env("XDG_RUNTIME_DIR", format!("/run/user/{uid}"));
    }
    #[cfg(windows)]
    command.env("TEMP", &directory).env("TMP", &directory);
    Ok((
        Prepared {
            workspace: temporary,
            #[cfg(target_os = "linux")]
            executable: executable_directory,
        },
        command,
    ))
}

/// Exact supported descriptions only; unknown capability sets never execute.
pub(super) fn routing_version(package: &VerifiedPackage) -> Result<&'static str> {
    for version in ["1.0.0", "2.0.0"] {
        if package.description["capabilities"]
            == serde_json::json!([{"id": "pro.runtime.adaptive_routing", "version": version}])
        {
            return Ok(version);
        }
    }
    if package.description["capabilities"]
        == serde_json::json!([
            {"id": "pro.runtime.adaptive_routing", "version": "2.0.0"},
            {"id": "pro.runtime.adaptive_context", "version": "1.0.0"}
        ])
    {
        return Ok("2.0.0");
    }
    if package.description["capabilities"]
        == serde_json::json!([
            {"id": "pro.runtime.adaptive_routing", "version": "2.0.0"},
            {"id": "pro.runtime.adaptive_context", "version": "1.0.0"},
            {"id": "pro.runtime.memory_curation", "version": "1.0.0"}
        ])
    {
        return Ok("2.0.0");
    }
    if package.description["capabilities"]
        == serde_json::json!([
            {"id": "pro.runtime.adaptive_routing", "version": "2.0.0"},
            {"id": "pro.runtime.adaptive_context", "version": "1.0.0"},
            {"id": "pro.runtime.memory_curation", "version": "1.0.0"},
            {"id": "pro.runtime.personal_protection", "version": "1.0.0"}
        ])
    {
        return Ok("2.0.0");
    }
    if package.description["capabilities"]
        == serde_json::json!([
            {"id": "pro.runtime.adaptive_routing", "version": "2.0.0"},
            {"id": "pro.runtime.adaptive_context", "version": "1.0.0"},
            {"id": "pro.runtime.memory_curation", "version": "1.0.0"},
            {"id": "pro.runtime.personal_protection", "version": "1.0.0"},
            {"id": "pro.runtime.code_security", "version": "1.0.0"}
        ])
    {
        return Ok("2.0.0");
    }
    if package.description["capabilities"]
        == serde_json::json!([
            {"id": "pro.runtime.adaptive_routing", "version": "2.0.0"},
            {"id": "pro.runtime.adaptive_context", "version": "1.0.0"},
            {"id": "pro.runtime.memory_curation", "version": "1.0.0"},
            {"id": "pro.runtime.personal_protection", "version": "1.0.0"},
            {"id": "pro.runtime.code_security", "version": "1.0.0"},
            {"id": "pro.runtime.semantic_detectors", "version": "1.0.0"}
        ])
    {
        return Ok("2.0.0");
    }
    Err(InstallError::Manifest)
}

pub(super) fn supports_semantic_detectors(package: &VerifiedPackage) -> bool {
    routing_version(package).is_ok()
        && package.description["capabilities"]
            .as_array()
            .is_some_and(|capabilities| {
                capabilities.iter().any(|capability| capability
                == &serde_json::json!({"id": "pro.runtime.semantic_detectors", "version": "1.0.0"}))
            })
}

pub(super) fn supports_code_security(package: &VerifiedPackage) -> bool {
    routing_version(package).is_ok()
        && package.description["capabilities"]
            .as_array()
            .is_some_and(|capabilities| {
                capabilities.iter().any(|capability| capability ==
                &serde_json::json!({"id": "pro.runtime.code_security", "version": "1.0.0"}))
            })
}

pub(super) fn supports_protection(package: &VerifiedPackage) -> bool {
    routing_version(package).is_ok()
        && package.description["capabilities"]
            .as_array()
            .is_some_and(|capabilities| {
                capabilities.iter().any(|capability| capability ==
                &serde_json::json!({"id": "pro.runtime.personal_protection", "version": "1.0.0"}))
            })
}

pub(super) fn supports_memory(package: &VerifiedPackage) -> bool {
    routing_version(package).is_ok()
        && package.description["capabilities"]
            .as_array()
            .is_some_and(|capabilities| {
                capabilities.iter().any(|capability| capability
                == &serde_json::json!({"id": "pro.runtime.memory_curation", "version": "1.0.0"}))
            })
}

/// The context-policy capability version the package advertises: `1.0.0`
/// learns only, `1.1.0` also promotes and monitors.
pub(super) fn context_policy_version(package: &VerifiedPackage) -> Option<&'static str> {
    if routing_version(package).is_err() {
        return None;
    }
    let capabilities = package.description["capabilities"].as_array()?;
    ["1.1.0", "1.0.0"].into_iter().find(|version| {
        capabilities.iter().any(|capability| {
            capability
                == &serde_json::json!({"id": "pro.runtime.context_policy", "version": version})
        })
    })
}

pub(super) fn supports_context(package: &VerifiedPackage) -> bool {
    routing_version(package).is_ok()
        && package.description["capabilities"]
            .as_array()
            .is_some_and(|capabilities| {
                capabilities.iter().any(|capability| capability
                == &serde_json::json!({"id": "pro.runtime.adaptive_context", "version": "1.0.0"}))
            })
}
