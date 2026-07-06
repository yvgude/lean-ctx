//! Hosted-registry installs for `lean-ctx pack` (GL #406, GH #727).
//!
//! `pack install <ns>/<name>` / `pack update <ns>/<name>`: version resolution
//! against the registry index, sha256-verified download, the standard local
//! import gates, lockfile pinning — and depth-1 resolution of the declared
//! dependencies so one install command yields a complete, reproducible set.

use super::pack_cmd::{apply_or_report, format_bytes, parse_flag};

/// Install `ns/name[@version]` from the hosted registry: resolve the version,
/// download, verify the artifact hash against the index, then run the normal
/// import path (manifest validation + content integrity + local signature
/// re-verification) and pin the result in `.lean-ctx/ctxpkg.lock`.
pub(crate) fn cmd_pack_install_remote(
    raw_ref: &str,
    registry_flag: Option<&str>,
    project_root: &str,
    refresh: bool,
) {
    use crate::core::context_package::{LocalRegistry, deps, lockfile, remote};

    let Some(remote_ref) = remote::parse_remote_ref(raw_ref) else {
        eprintln!("ERROR: '{raw_ref}' is not a valid ns/name[@version] reference");
        return;
    };
    let base = remote::registry_base(registry_flag);
    let ns = &remote_ref.namespace;
    let name = &remote_ref.name;
    // CTXPKG_TOKEN (ctxp_ or read-only ctxr_) unlocks private packages (#524).
    let token = remote::publish_token(None);

    // Offline-reproducible installs (GH #727): an unpinned re-install that is
    // already locked (or already imported into the store) never touches the
    // network. `pack update` (refresh=true) and explicit `@version` pins skip
    // this fast path.
    if !refresh && remote_ref.version.is_none() {
        let scoped = format!("@{ns}/{name}");
        let candidate =
            deps::locked_version(&scoped, std::path::Path::new(project_root)).or_else(|| {
                LocalRegistry::open()
                    .ok()
                    .and_then(|r| r.list().ok())
                    .and_then(|entries| {
                        entries
                            .iter()
                            .filter(|e| e.name == scoped)
                            .max_by(|a, b| a.installed_at.cmp(&b.installed_at))
                            .map(|e| e.version.clone())
                    })
            });
        if let Some(version) = candidate {
            let in_store = LocalRegistry::open()
                .ok()
                .and_then(|r| r.get(&scoped, Some(&version)).ok().flatten())
                .is_some();
            if in_store {
                println!("Using installed {scoped}@{version} from the local store (offline).");
                println!("  (run `lean-ctx pack update {ns}/{name}` to fetch a newer version)");
                apply_or_report(&scoped, &version, project_root);
                return;
            }
        }
    }

    println!("Resolving @{ns}/{name} via {base} …");
    let versions = match remote::fetch_versions(&base, ns, name, token.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };
    let info = match remote::select_version(&versions, remote_ref.version.as_deref()) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };
    if info.yanked {
        eprintln!(
            "WARNING: @{ns}/{name}@{} is YANKED — installing only because the version \
             was pinned explicitly",
            info.version
        );
    }

    let bytes = match remote::download_verified(&base, ns, name, info, token.as_deref()) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };
    println!(
        "Downloaded @{ns}/{name}@{} ({}, sha256 verified)",
        info.version,
        format_bytes(bytes.len() as u64)
    );

    // Hand the artifact to the standard import path via a temp file so every
    // local gate (extension, size cap, manifest validation, content integrity)
    // applies identically to remote and local installs.
    let tmp = std::env::temp_dir().join(format!("ctxpkg-install-{}.ctxpkg", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, &bytes) {
        eprintln!("ERROR: stage artifact: {e}");
        return;
    }
    let imported = (|| {
        let registry = LocalRegistry::open()?;
        registry.import_from_file(&tmp)
    })();
    std::fs::remove_file(&tmp).ok();

    let manifest = match imported {
        Ok(m) => m,
        Err(e) => {
            eprintln!("ERROR: import failed: {e}");
            return;
        }
    };

    // Registry compromise ≠ client compromise: re-verify the signature locally.
    match crate::core::context_package::verify_signature(&manifest) {
        Ok(true) => println!("Signature: ed25519 verified locally"),
        Ok(false) => {
            eprintln!(
                "WARNING: package is unsigned — the hosted registry should not have accepted it"
            );
        }
        Err(e) => {
            eprintln!("ERROR: signature verification failed: {e}");
            return;
        }
    }

    if let Err(e) = lockfile::upsert(
        std::path::Path::new(project_root),
        lockfile::LockedPackage {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            artifact_sha256: info.artifact_sha256.clone(),
            registry: base.clone(),
        },
    ) {
        eprintln!("WARNING: could not update ctxpkg.lock: {e}");
    } else {
        println!("Pinned in {}", lockfile::LOCKFILE_REL_PATH);
    }

    // Depth-1 dependency resolution (GH #727): declared, non-optional deps
    // install from the same registry and land in the same lockfile.
    if let Err(e) =
        install_declared_dependencies(&manifest, &base, token.as_deref(), project_root, refresh)
    {
        eprintln!("ERROR: dependency install failed: {e}");
        eprintln!(
            "  `{}` itself is installed; fix the dependency and re-run.",
            manifest.name
        );
        return;
    }

    apply_or_report(&manifest.name, &manifest.version, project_root);
}

/// Install every non-optional declared dependency of `manifest` (depth 1,
/// GH #727). Already-locked deps present in the store are skipped offline;
/// everything else resolves SemVer against the registry, downloads through
/// the standard verified import path, and is pinned in the lockfile.
pub(crate) fn install_declared_dependencies(
    manifest: &crate::core::context_package::PackageManifest,
    base: &str,
    token: Option<&str>,
    project_root: &str,
    refresh: bool,
) -> Result<(), String> {
    use crate::core::context_package::{LocalRegistry, deps, lockfile, remote};

    if manifest.dependencies.iter().all(|d| d.optional) {
        return Ok(());
    }
    let registry = LocalRegistry::open()?;
    let root = std::path::Path::new(project_root);

    for dep in manifest.dependencies.iter().filter(|d| !d.optional) {
        if !refresh && let Some(ver) = deps::already_satisfied(root, &registry, dep) {
            println!(
                "Dependency {}@{ver} already satisfied (locked, offline).",
                dep.name
            );
            continue;
        }

        let resolved = deps::resolve_one(&manifest.name, dep, base, token)?;
        let (ns, slug) = (&resolved.namespace, &resolved.slug);
        println!(
            "Installing dependency @{ns}/{slug}@{} (declared: `{} {}`)",
            resolved.version, dep.name, dep.version_req
        );

        let info = remote::VersionInfo {
            version: resolved.version.clone(),
            artifact_sha256: resolved.artifact_sha256.clone(),
            yanked: false,
        };
        let bytes = remote::download_verified(base, ns, slug, &info, token)?;
        let tmp = std::env::temp_dir().join(format!(
            "ctxpkg-dep-{}-{ns}-{slug}.ctxpkg",
            std::process::id()
        ));
        std::fs::write(&tmp, &bytes).map_err(|e| format!("stage dependency artifact: {e}"))?;
        let imported = registry.import_from_file(&tmp);
        std::fs::remove_file(&tmp).ok();
        let dep_manifest = imported.map_err(|e| format!("dependency `{}`: {e}", dep.name))?;

        if let Err(e) = lockfile::upsert(
            root,
            lockfile::LockedPackage {
                name: dep_manifest.name.clone(),
                version: dep_manifest.version.clone(),
                artifact_sha256: resolved.artifact_sha256.clone(),
                registry: base.to_string(),
            },
        ) {
            eprintln!("WARNING: could not update ctxpkg.lock: {e}");
        }
        println!(
            "  ✓ {}@{} installed + pinned",
            dep_manifest.name, dep_manifest.version
        );
    }
    Ok(())
}

/// `pack update <ns>/<name>` — refresh a hosted pack (and its declared
/// dependencies) to the newest matching versions, updating the lockfile.
pub(crate) fn cmd_pack_update(args: &[String], project_root: &str) {
    let target = args
        .iter()
        .skip_while(|a| a.as_str() != "update")
        .skip(1)
        .find(|a| !a.starts_with("--"));
    let Some(raw_ref) = target else {
        eprintln!("Usage: lean-ctx pack update <ns>/<name> [--registry <url>]");
        return;
    };
    if crate::core::context_package::remote::parse_remote_ref(raw_ref).is_none() {
        eprintln!("ERROR: '{raw_ref}' is not a valid ns/name reference");
        return;
    }
    cmd_pack_install_remote(
        raw_ref,
        parse_flag(args, "--registry").as_deref(),
        project_root,
        true,
    );
}
