// SPDX-License-Identifier: Apache-2.0
//! Bounded execution of an authenticated staging package, never a mutable path.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::install::SemanticState;
use super::{InstallError, Result, VerifiedPackage, install};

const DESCRIBE_SECONDS: u64 = 2;
/// A start that is given a model set reads and hashes every file of it
/// before it declares semantic detectors, so that description check alone
/// gets this bound. It runs at activation and on request, never per
/// invocation.
const SEMANTIC_DESCRIBE_SECONDS: u64 = 60;

pub(super) fn check(root: &Path, selected: &str, key: &[u8; 32]) -> Result<serde_json::Value> {
    let package = install::verified_active(root, selected, key)?;
    let state = install::semantic_state(&package);
    let (temporary, mut command) = prepare(&package, root)?;
    let mut seconds = DESCRIBE_SECONDS;
    let mut versions = Vec::new();
    // A runtime declares semantic detectors only with its model set present.
    // With the signed set installed the start is given it and must report
    // exactly the packaged description. Without one it must report exactly
    // the rest: a v4 package whose companions are not installed, and an
    // earlier format whose description names the capability, which no
    // backend ever served. A description that does not name it is compared
    // as it always was.
    let expected = match &state {
        SemanticState::Offered { .. } => {
            let set = name_semantic_models(&package, &mut command)
                .ok()
                .flatten()
                .ok_or(InstallError::Health)?;
            versions = set.versions;
            seconds = SEMANTIC_DESCRIBE_SECONDS;
            package.description.clone()
        }
        SemanticState::SetNotPresent | SemanticState::NotOffered => {
            super::manifest::without_semantic(&package.description)
        }
        SemanticState::Unverified => return Err(InstallError::Health),
    };
    command.arg("--describe").stdin(Stdio::null());
    let captured = crate::core::process_capture::run_with_output_limits(
        &mut command,
        Some(Duration::from_secs(seconds)),
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
    if actual != expected {
        return Err(InstallError::Health);
    }
    temporary.close()?;
    let mut report = serde_json::json!({
        "status": "healthy", "receipt": package.receipt, "description": actual,
        "runtime_started": true, "service_running": false, "user_state_modified": false,
        "release_approved": false
    });
    for (key, value) in semantic_report(&package, &state, versions) {
        report[key] = value;
    }
    // This process started the runtime to describe itself and holds no worker.
    let live = crate::core::config::Config::load_global().intelligence_runtime;
    for (key, value) in semantic_worker_report(&live) {
        report[key] = value;
    }
    Ok(report)
}

/// One line for a reader of `runtime status` or `runtime health`.
const WORKER_SCOPE_NOTE: &str = "worker state is per process; a server process holds its own";

/// The semantic worker of THIS process for `config` (`cold`, `warming`,
/// `warm`, `failed`) and, when warm, the contract versions it named. Reading
/// it starts nothing. A worker is never shared between processes: a
/// command-line call reports `cold` while a running server holds a warm one,
/// and a text is examined only by a process that lives long enough to have a
/// warm worker.
pub(super) fn semantic_worker_report(
    config: &crate::core::config::IntelligenceRuntimeConfig,
) -> [(&'static str, serde_json::Value); 4] {
    let (worker, versions) = super::semantic_detectors::worker_state(config);
    [
        ("semantic_worker", worker.into()),
        ("semantic_worker_versions", versions.into()),
        ("semantic_worker_scope", "this_process".into()),
        ("semantic_worker_note", WORKER_SCOPE_NOTE.into()),
    ]
}

/// The install-side semantic fields that `runtime health` and `runtime
/// status` both print, so the two cannot disagree. A model set is asked
/// through a worker that belongs to one process; the worker state is the
/// caller's to add, with its scope.
pub(super) fn semantic_report(
    package: &VerifiedPackage,
    state: &SemanticState,
    versions: Vec<String>,
) -> [(&'static str, serde_json::Value); 4] {
    [
        ("semantic_detectors", state.as_str().into()),
        (
            "semantic_descriptor_sha256_prefix",
            state.descriptor_prefix().into(),
        ),
        ("semantic_versions", versions.into()),
        (
            "semantic_categories",
            super::manifest::semantic_categories(package).into(),
        ),
    ]
}

/// Keep the short socket workspace and authenticated executable alive together.
pub(super) struct Prepared {
    workspace: install::Temporary,
    #[cfg(target_os = "linux")]
    executable: tempfile::TempDir,
}

impl Prepared {
    // The Unix peer socket lives in the workspace; Windows uses a named pipe.
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

/// Name the installed model set to a runtime start: its directory, and the
/// descriptor digest the signed manifest states. No model byte is copied or
/// hashed here. The runtime compares every file with that descriptor, and
/// the descriptor with this digest, each time it loads the set. A package of
/// an earlier format has no set to name and starts as before. A v4 package
/// whose set is not installed, or not admitted, cannot serve semantic
/// detectors.
pub(super) fn name_semantic_models(
    package: &VerifiedPackage,
    command: &mut Command,
) -> Result<Option<install::SemanticSet>> {
    if package.semantic.is_none() {
        return Ok(None);
    }
    let set = install::semantic_set(package).ok_or(InstallError::InvocationAdmission)?;
    command
        .env("LEANCTX_INTELLIGENCE_SEMANTIC_MODELS", &set.directory)
        .env(
            "LEANCTX_INTELLIGENCE_SEMANTIC_DESCRIPTOR_SHA256",
            &set.descriptor_sha256,
        );
    Ok(Some(set))
}

pub(super) fn prepare(package: &VerifiedPackage, root: &Path) -> Result<(Prepared, Command)> {
    // New capability sets need explicit host support before executing any bytes.
    // Package compatibility is independent of retired model-routing behavior.
    validate_capabilities(package)?;
    prepare_binary(&package.binary, root, super::EXECUTABLE_NAME)
}

/// A private copy of authenticated `binary`, named `name`, and a command for
/// it with a cleared environment and a private working directory. The caller
/// has admitted the package's capabilities for what it is about to start.
pub(super) fn prepare_binary(
    binary: &[u8],
    root: &Path,
    name: &str,
) -> Result<(Prepared, Command)> {
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
    let executable = executable_directory.path().join(name);
    #[cfg(not(target_os = "linux"))]
    let _ = root;
    #[cfg(not(target_os = "linux"))]
    let executable = directory.join(name);
    // Execute bytes authenticated above, not the separately mutable installed
    // inode. This is private process hygiene, not a hostile same-UID sandbox.
    install::write_new(&executable, binary, true)?;
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
pub(super) fn validate_capabilities(package: &VerifiedPackage) -> Result<()> {
    if legacy_routing_version(package).is_ok()
        || complete_capability_set(package)
        || package.description["capabilities"]
            == serde_json::json!([
                {"id": "pro.runtime.adaptive_context", "version": "1.0.0"},
                {"id": "pro.runtime.memory_curation", "version": "1.0.0"},
                {"id": "pro.runtime.personal_protection", "version": "1.0.0"},
                {"id": "pro.runtime.code_security", "version": "1.0.0"},
                {"id": "pro.runtime.context_policy", "version": "1.0.0"},
                {"id": "pro.runtime.context_policy", "version": "1.1.0"}
            ])
    {
        return Ok(());
    }
    Err(InstallError::Manifest)
}

/// Whether the package declares one of the complete, authenticated sets the
/// optional capabilities are admitted in: the six context capabilities,
/// optionally followed by the hosted index of 3.11-era runtimes, then the
/// optional tail below. No routing capability is part of any set.
pub(super) fn complete_capability_set(package: &VerifiedPackage) -> bool {
    let base = [
        serde_json::json!({"id": "pro.runtime.adaptive_context", "version": "1.0.0"}),
        serde_json::json!({"id": "pro.runtime.memory_curation", "version": "1.0.0"}),
        serde_json::json!({"id": "pro.runtime.personal_protection", "version": "1.0.0"}),
        serde_json::json!({"id": "pro.runtime.code_security", "version": "1.0.0"}),
        serde_json::json!({"id": "pro.runtime.context_policy", "version": "1.0.0"}),
        serde_json::json!({"id": "pro.runtime.context_policy", "version": "1.1.0"}),
    ];
    let hosted_index = serde_json::json!({"id": "pro.runtime.hosted_index", "version": "2.0.0"});
    let Some(declared) = package.description["capabilities"].as_array() else {
        return false;
    };
    if declared.len() < base.len() || declared[..base.len()] != base[..] {
        return false;
    }
    let mut rest = &declared[base.len()..];
    if rest.first() == Some(&hosted_index) {
        rest = &rest[1..];
    }
    // After the base, in this order and each optional: the organization
    // policy verifier, the team artifact reader, the team artifact author,
    // compliance frameworks, the dashboard workflows, then semantic detectors.
    for optional in [
        org_policy_verifier_capability(),
        team_artifact_reader_capability(),
        team_artifact_author_capability(),
        compliance_frameworks_capability(),
        dashboard_pro_capability(),
    ] {
        if rest.first() == Some(&optional) {
            rest = &rest[1..];
        }
    }
    if rest.is_empty() {
        return true;
    }
    // Semantic detectors: 1.1.0 (answers name the examined categories) from
    // any model set, preceded by 1.0.0 only from a set that examines all
    // three; 1.0.0 alone is the earlier form of the latter.
    let semantic = |version: &str| serde_json::json!({"id": "pro.runtime.semantic_detectors", "version": version});
    [
        vec![semantic("1.0.0")],
        vec![semantic("1.1.0")],
        vec![semantic("1.0.0"), semantic("1.1.0")],
    ]
    .into_iter()
    .any(|tail| rest == &tail[..])
}

/// The one description entry of the Enterprise compliance frameworks.
fn compliance_frameworks_capability() -> serde_json::Value {
    serde_json::json!({
        "id": super::compliance_frameworks::CAPABILITY_ID,
        "version": super::compliance_frameworks::CAPABILITY_VERSION
    })
}

/// Whether this authenticated package declares the compliance frameworks
/// inside one of the complete sets admitted above.
pub(super) fn supports_compliance_frameworks(package: &VerifiedPackage) -> bool {
    complete_capability_set(package)
        && package.description["capabilities"]
            .as_array()
            .is_some_and(|declared| declared.contains(&compliance_frameworks_capability()))
}

/// The one description entry of the Cockpit's Pro dashboard workflows.
fn dashboard_pro_capability() -> serde_json::Value {
    serde_json::json!({
        "id": super::dashboard_pro::CAPABILITY_ID,
        "version": super::dashboard_pro::CAPABILITY_VERSION
    })
}

/// The description entries of the retired team artifact reader and author.
/// A package may still declare them; this engine no longer invokes them.
fn team_artifact_reader_capability() -> serde_json::Value {
    serde_json::json!({"id": "team.runtime.artifact_reader", "version": "1.0.0"})
}

fn team_artifact_author_capability() -> serde_json::Value {
    serde_json::json!({"id": "team.runtime.artifact_author", "version": "1.0.0"})
}

/// The one description entry of the organization policy verifier.
fn org_policy_verifier_capability() -> serde_json::Value {
    serde_json::json!({"id": "enterprise.runtime.org_policy_verifier", "version": "1.0.0"})
}

/// Whether this authenticated package declares the organization policy
/// verifier — and does so inside one of the complete sets admitted above. A
/// package that merely lists the id somewhere is not such a package.
pub(super) fn supports_org_policy_verifier(package: &VerifiedPackage) -> bool {
    complete_capability_set(package)
        && package.description["capabilities"]
            .as_array()
            .is_some_and(|declared| declared.contains(&org_policy_verifier_capability()))
}

/// The semantic detector contract to speak with this package: 1.1.0 when it
/// is declared, else 1.0.0, else none. It is offered only by a v4 package
/// whose signed model set is installed. An earlier format whose description
/// names the capability offers nothing: no backend ever served it.
pub(super) fn semantic_detectors_version(package: &VerifiedPackage) -> Option<&'static str> {
    if validate_capabilities(package).is_err()
        || !package.semantic.as_ref().is_some_and(|semantic| {
            matches!(
                semantic.installation,
                super::manifest::SemanticInstallation::Installed { .. }
            )
        })
    {
        return None;
    }
    let capabilities = package.description["capabilities"].as_array()?;
    ["1.1.0", "1.0.0"].into_iter().find(|version| {
        capabilities.iter().any(|capability| {
            capability
                == &serde_json::json!({"id": "pro.runtime.semantic_detectors", "version": version})
        })
    })
}

// Previously signed packages remain installable for their context capabilities.
// Recognizing their description does not permit a routing invocation. The last
// of them also declares semantic detectors and keeps that capability; a current
// runtime declares it without any routing entry (`complete_capability_set`).
fn legacy_routing_version(package: &VerifiedPackage) -> Result<&'static str> {
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
    semantic_detectors_version(package).is_some()
}

pub(super) fn supports_code_security(package: &VerifiedPackage) -> bool {
    validate_capabilities(package).is_ok()
        && package.description["capabilities"]
            .as_array()
            .is_some_and(|capabilities| {
                capabilities.iter().any(|capability| capability ==
                &serde_json::json!({"id": "pro.runtime.code_security", "version": "1.0.0"}))
            })
}

pub(super) fn supports_protection(package: &VerifiedPackage) -> bool {
    validate_capabilities(package).is_ok()
        && package.description["capabilities"]
            .as_array()
            .is_some_and(|capabilities| {
                capabilities.iter().any(|capability| capability ==
                &serde_json::json!({"id": "pro.runtime.personal_protection", "version": "1.0.0"}))
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn described(capabilities: &serde_json::Value) -> VerifiedPackage {
        VerifiedPackage {
            receipt: super::super::manifest::PackageReceipt {
                manifest_sha256: "a".repeat(64),
                artifact_sha256: "a".repeat(64),
                binary_sha256: "a".repeat(64),
                commit: "a".repeat(40),
                version: "4.1.0".to_owned(),
                license: "LicenseRef-Proprietary".to_owned(),
                rollback_sha256: None,
                staging_only: false,
            },
            binary: Vec::new(),
            description: serde_json::json!({ "capabilities": capabilities }),
            semantic: None,
        }
    }

    /// The exact current set that declares semantic detectors, with no
    /// routing capability in it, is an accepted description, and no other
    /// arrangement is. Declaring is not offering: the capability is offered
    /// only with an installed model set, and it is never an exchange
    /// invocation.
    #[test]
    fn semantic_detectors_are_admitted_without_a_routing_capability() {
        let semantic =
            serde_json::json!({"id": "pro.runtime.semantic_detectors", "version": "1.0.0"});
        let current = serde_json::json!([
            {"id": "pro.runtime.adaptive_context", "version": "1.0.0"},
            {"id": "pro.runtime.memory_curation", "version": "1.0.0"},
            {"id": "pro.runtime.personal_protection", "version": "1.0.0"},
            {"id": "pro.runtime.code_security", "version": "1.0.0"},
            {"id": "pro.runtime.context_policy", "version": "1.0.0"},
            {"id": "pro.runtime.context_policy", "version": "1.1.0"},
            {"id": "pro.runtime.hosted_index", "version": "2.0.0"}
        ]);
        let mut declaring = current.as_array().expect("array").clone();
        declaring.push(semantic.clone());
        let mut misplaced = declaring.clone();
        misplaced.swap(0, 7);

        let mut served = described(&serde_json::Value::Array(declaring));
        assert!(validate_capabilities(&served).is_ok());
        // Was `true`: an earlier format has no model set, so no backend ever
        // served what its description names (lead decision: not offered).
        assert!(!supports_semantic_detectors(&served));
        assert_eq!(install::semantic_state(&served).as_str(), "not_offered");
        assert!(complete_capability_set(&served));
        // The same description with its signed model set installed offers it.
        let listed = |name: &str| super::super::manifest::SemanticCompanion {
            name: name.to_owned(),
            sha256: "a".repeat(64),
            bytes: 1,
        };
        served.semantic = Some(super::super::manifest::SemanticPackage {
            companions: super::super::manifest::SemanticCompanions {
                descriptor: listed("semantic-models.json"),
                files: Vec::new(),
            },
            installation: super::super::manifest::SemanticInstallation::Installed {
                directory: std::path::PathBuf::new(),
                coverage: [true; 3],
            },
        });
        assert_eq!(semantic_detectors_version(&served), Some("1.0.0"));
        assert!(!supports_semantic_detectors(&described(&current)));
        for unknown in [
            serde_json::json!([semantic]),
            serde_json::Value::Array(misplaced),
        ] {
            let package = described(&unknown);
            assert!(validate_capabilities(&package).is_err());
            assert!(!supports_semantic_detectors(&package));
        }

        #[cfg(any(unix, windows))]
        {
            let invocation: lean_ctx_protocol::EngineInvocationV1 =
                serde_json::from_value(serde_json::json!({
                    "schema_version": 1, "invocation_id": "semantic-admission",
                    "engine": {"engine_id": "leanctx-intelligence", "engine_version": "4.1.0"},
                    "operation": {
                        "capability_id": "pro.runtime.semantic_detectors",
                        "capability_version": "1.0.0"
                    },
                    "input_ref": "projection:test",
                    "input_digest": format!("sha256:{}", "a".repeat(64)),
                    "source_refs": ["projection:test"],
                    "policy_admission": {"policy_ref": "policy:test", "decision": "admitted"}
                }))
                .expect("invocation");
            let admit = super::super::session::admit_invocation;
            // Was `is_ok()`: a model set is asked through the warm worker
            // only, so no exchange invocation is admitted for it, with or
            // without an installed set.
            assert!(matches!(
                admit(&served, &invocation),
                Err(InstallError::InvocationAdmission)
            ));
            assert!(matches!(
                admit(&described(&current), &invocation),
                Err(InstallError::InvocationAdmission)
            ));
        }
    }
}
