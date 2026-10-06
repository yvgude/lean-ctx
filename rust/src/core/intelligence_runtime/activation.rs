// SPDX-License-Identifier: Apache-2.0
//! Interactive account handoff to the verified private implementation.
use std::collections::BTreeMap;
#[cfg(any(unix, windows))]
use std::path::Path;

use super::{InstallError, Result};

#[cfg(not(any(unix, windows)))]
pub(super) fn activate(_flags: &BTreeMap<&str, &str>, _stdin: bool) -> Result<serde_json::Value> {
    Err(InstallError::Configuration)
}

#[cfg(any(unix, windows))]
pub(super) fn activate(
    flags: &BTreeMap<&str, &str>,
    credentials_stdin: bool,
) -> Result<serde_json::Value> {
    use crate::core::config::Config;
    use std::io::{IsTerminal, Read, Write};
    use zeroize::Zeroizing;

    if flags.keys().any(|name| {
        !matches!(
            *name,
            "--account-origin" | "--issuer-origin" | "--license-id" | "--ca-certificate"
        )
    }) {
        return Err(InstallError::Usage);
    }
    let account = flags.get("--account-origin").ok_or(InstallError::Usage)?;
    let issuer = flags.get("--issuer-origin").ok_or(InstallError::Usage)?;
    let observed = Config::try_load_global()
        .map_err(|_| InstallError::Configuration)?
        .intelligence_runtime;
    if !(observed.enabled
        && observed.accept_proprietary
        && super::bootstrap::permits_config(&observed)
        && observed.license_configuration.is_empty())
    {
        return Err(InstallError::Configuration);
    }
    let root = Path::new(&observed.root);
    let _configuration_guard = super::install::configuration_guard(root)?;
    require_current_configuration(&observed)?;
    let package = super::install::verified_active(
        root,
        &observed.manifest_sha256,
        &super::trust_key(&observed.trust_key_hex)?,
    )?;
    if package.description["entitlement_required"] != true {
        return Err(InstallError::Policy);
    }
    let directory = root.join("personal-license");
    if std::fs::symlink_metadata(&directory).is_ok() {
        return Err(InstallError::Activation);
    }
    let configuration = directory.join("runtime.json");
    let configuration_text = configuration.to_str().ok_or(InstallError::Configuration)?;
    let (temporary, mut command) = super::health::prepare(&package, root)?;
    command
        .arg("--activate-personal")
        .arg("--account-origin")
        .arg(account)
        .arg("--issuer-origin")
        .arg(issuer)
        .arg("--directory")
        .arg(&directory);
    for name in ["--license-id", "--ca-certificate"] {
        if let Some(value) = flags.get(name) {
            command.arg(name).arg(value);
        }
    }
    let credentials = if credentials_stdin {
        let mut input = Zeroizing::new(Vec::new());
        std::io::stdin()
            .take(4097)
            .read_to_end(&mut input)
            .map_err(|_| InstallError::Activation)?;
        input
    } else {
        if !std::io::stdin().is_terminal() {
            return Err(InstallError::Usage);
        }
        eprint!("Account email: ");
        std::io::stderr()
            .flush()
            .map_err(|_| InstallError::Activation)?;
        let mut email = String::new();
        std::io::stdin()
            .read_line(&mut email)
            .map_err(|_| InstallError::Activation)?;
        let password = Zeroizing::new(
            rpassword::prompt_password("Account password: ")
                .map_err(|_| InstallError::Activation)?,
        );
        #[derive(serde::Serialize)]
        struct Input<'a> {
            email: &'a str,
            password: &'a str,
        }
        Zeroizing::new(
            serde_json::to_vec(&Input {
                email: email.trim(),
                password: &password,
            })
            .map_err(|_| InstallError::Activation)?,
        )
    };
    if credentials.is_empty() || credentials.len() > 4096 {
        return Err(InstallError::Usage);
    }
    // Interactive entry may outlive consent or a configuration change. Do not
    // hand credentials to the previously selected child after such a change.
    require_current_configuration(&observed)?;
    let captured = run_activation(command, credentials)?;
    if captured.timed_out
        || captured.cancelled
        || !captured.output.status.success()
        || !captured.output.stderr.is_empty()
    {
        return Err(InstallError::Activation);
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Receipt {
        status: String,
        logout_confirmed: bool,
        service_running: bool,
        release_approved: bool,
    }
    let receipt: Receipt =
        serde_json::from_slice(&captured.output.stdout).map_err(|_| InstallError::Activation)?;
    if receipt.status != "personal_activated" || receipt.service_running || receipt.release_approved
    {
        return Err(InstallError::Activation);
    }
    temporary
        .close()
        .map_err(|_| InstallError::ProvisionedConfiguration)?;
    #[cfg(windows)]
    let _license_authority = {
        use crate::core::windows_private::{Directory, Privacy};
        let authority = Directory::open(&directory, Privacy::Private)
            .map_err(|_| InstallError::ProvisionedConfiguration)?;
        drop(Zeroizing::new(
            authority
                .read_file("runtime.json", 16 * 1024)
                .map_err(|_| InstallError::ProvisionedConfiguration)?,
        ));
        authority
    };
    #[cfg(not(windows))]
    drop(Zeroizing::new(
        super::read_regular(&configuration, 16 * 1024)
            .map_err(|_| InstallError::ProvisionedConfiguration)?,
    ));
    Config::try_update_global(|config| {
        if config.intelligence_runtime != observed {
            return Err(crate::core::error::LeanCtxError::Config(
                "runtime activation configuration changed".into(),
            ));
        }
        configuration_text.clone_into(&mut config.intelligence_runtime.license_configuration);
        Ok(())
    })
    .map_err(|_| InstallError::ProvisionedConfiguration)?;
    Ok(
        serde_json::json!({"status":"personal_activated","configuration_saved":true,
        "logout_confirmed":receipt.logout_confirmed,"service_running":false,"release_approved":false}),
    )
}

#[cfg(any(unix, windows))]
fn require_current_configuration(
    observed: &crate::core::config::IntelligenceRuntimeConfig,
) -> Result<()> {
    let current = crate::core::config::Config::try_load_global()
        .map_err(|_| InstallError::Configuration)?
        .intelligence_runtime;
    if current != *observed
        || !current.enabled
        || !current.accept_proprietary
        || !super::bootstrap::permits_config(&current)
        || !current.license_configuration.is_empty()
    {
        return Err(InstallError::Configuration);
    }
    Ok(())
}

#[cfg(unix)]
fn run_activation(
    mut command: std::process::Command,
    credentials: zeroize::Zeroizing<Vec<u8>>,
) -> Result<crate::core::process_capture::CapturedOutput> {
    use std::{
        io::Write,
        net::Shutdown,
        os::{fd::OwnedFd, unix::net::UnixStream},
        process::Stdio,
        time::Duration,
    };
    // Preserve the existing bounded Unix socket handoff and EOF behavior.
    let (mut sender, child_input) = UnixStream::pair()?;
    sender.set_write_timeout(Some(Duration::from_secs(1)))?;
    sender
        .write_all(&credentials)
        .map_err(|_| InstallError::Activation)?;
    drop(credentials);
    sender.shutdown(Shutdown::Write)?;
    command.stdin(Stdio::from(OwnedFd::from(child_input)));
    crate::core::process_capture::run_with_output_limits(
        &mut command,
        Some(Duration::from_mins(2)),
        1024,
        4096,
    )
    .map_err(|_| InstallError::Activation)
}

#[cfg(windows)]
fn run_activation(
    command: std::process::Command,
    credentials: zeroize::Zeroizing<Vec<u8>>,
) -> Result<crate::core::process_capture::CapturedOutput> {
    run_with_pipe(command, credentials, std::time::Duration::from_mins(2))
}

#[cfg(any(windows, test))]
fn run_with_pipe(
    mut command: std::process::Command,
    credentials: zeroize::Zeroizing<Vec<u8>>,
    timeout: std::time::Duration,
) -> Result<crate::core::process_capture::CapturedOutput> {
    use std::io::Write;
    let (reader, mut writer) = std::io::pipe()?;
    command.stdin(reader);
    std::thread::scope(|scope| {
        let input = std::thread::Builder::new()
            .name("leanctx-activation-input".into())
            .spawn_scoped(scope, move || writer.write_all(&credentials))
            .map_err(|_| InstallError::Activation)?;
        let captured = crate::core::process_capture::run_with_output_limits(
            &mut command,
            Some(timeout),
            1024,
            4096,
        );
        // Capture kills the Windows Job Object on timeout. Drop Command's last
        // reader copy even after spawn failure before joining a blocked writer.
        drop(command);
        let written = input.join().map_err(|_| InstallError::Activation)?;
        let captured = captured.map_err(|_| InstallError::Activation)?;
        written.map_err(|_| InstallError::Activation)?;
        Ok(captured)
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    use zeroize::Zeroizing;

    #[test]
    fn activation_pipe_delivers_complete_bounded_input_and_eof() {
        let mut child = Command::new("sh");
        child.args(["-c", "wc -c"]);
        let result = run_with_pipe(
            child,
            Zeroizing::new(vec![b'x'; 4096]),
            Duration::from_secs(2),
        )
        .expect("capture");
        assert!(result.output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&result.output.stdout).trim(),
            "4096"
        );
        assert!(result.output.stderr.is_empty());
    }

    #[test]
    fn activation_pipe_releases_writer_after_failed_spawn_and_timeout() {
        // Deliberately exceed OS pipe capacity to exercise cancellation, beyond
        // the real credential limit already enforced by activate().
        let start = Instant::now();
        let absent = Command::new("/nonexistent/leanctx-activation-child");
        assert!(
            run_with_pipe(
                absent,
                Zeroizing::new(vec![0; 1_048_576]),
                Duration::from_millis(150)
            )
            .is_err()
        );
        let mut child = Command::new("sh");
        child.args(["-c", "sleep 30"]);
        assert!(
            run_with_pipe(
                child,
                Zeroizing::new(vec![0; 1_048_576]),
                Duration::from_millis(150)
            )
            .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
