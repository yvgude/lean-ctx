use super::{
    InstalledStore, Path, RevocationList, addon_self_ref, artifact_install, bootstrap,
    fetch_addon_pack, flag_value, install, install_declared_deps, provision_and_wire,
    refresh_pack_dependencies, registry, resolve_declared_deps,
};

/// `addon publish [manifest] --namespace <ns>` — build the signed
/// `kind=addon` pack from a `lean-ctx-addon.toml` and upload it to the
/// hosted ctxpkg registry (GH #726). `--check` runs every local gate
/// (schema, audit, signing, self-verification) and stops before the network.
pub(super) fn cmd_publish(args: &[String]) -> i32 {
    let manifest_path = args
        .iter()
        .skip(1)
        .find(|a| {
            !a.starts_with('-')
                && Path::new(a.as_str())
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
        })
        .map_or_else(|| "lean-ctx-addon.toml".to_string(), String::clone);

    let Some(namespace) = flag_value(args, "--namespace") else {
        eprintln!(
            "Usage: lean-ctx addon publish [lean-ctx-addon.toml] --namespace <ns> \
             [--check] [--registry <url>] [--token <ctxp_…>]"
        );
        eprintln!();
        eprintln!("The namespace is your ctxpkg.com account handle — the pack publishes");
        eprintln!("as @<ns>/<addon-name>. `--check` validates and signs locally without");
        eprintln!("uploading anything.");
        return 1;
    };

    let plan =
        match crate::core::addons::publish::build_addon_pack(Path::new(&manifest_path), &namespace)
        {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Error: {e}");
                return 1;
            }
        };

    println!(
        "Built @{}/{}@{} (kind=addon, {} bytes)",
        plan.namespace,
        plan.slug,
        plan.version,
        plan.bundle_json.len()
    );
    println!("  Audit verdict: {}", plan.audit.verdict.as_str());
    for f in &plan.audit.findings {
        println!("    {} {} — {}", f.level.as_str(), f.code, f.message);
    }
    if plan.artifact_platforms.is_empty() {
        println!("  Artifacts: none (installs use the runner/[install] path)");
    } else {
        println!("  Artifacts: {}", plan.artifact_platforms.join(", "));
    }
    if plan.has_bootstrap {
        println!("  Bootstrap: [install] fallback for platforms without an artifact");
    }

    if args.iter().any(|a| a == "--check") {
        println!("\n--check: all local gates passed — nothing was uploaded.");
        return 0;
    }

    use crate::core::context_package::remote;
    let base = remote::registry_base(flag_value(args, "--registry").as_deref());
    let Some(token) = remote::publish_token(flag_value(args, "--token").as_deref()) else {
        eprintln!("ERROR: no publish token — pass --token or set CTXPKG_TOKEN");
        eprintln!("Mint one at ctxpkg.com/account (sign in, then Tokens → Mint).");
        return 1;
    };
    if token.starts_with("ctxr_") {
        eprintln!(
            "ERROR: this is a read-only install token (ctxr_) — publishing needs a ctxp_ token"
        );
        return 1;
    }

    println!(
        "\nPublishing @{}/{}@{} to {base} …",
        plan.namespace, plan.slug, plan.version
    );
    match remote::publish(
        &base,
        &token,
        &plan.namespace,
        &plan.slug,
        &plan.version,
        plan.bundle_json.as_bytes(),
    ) {
        Ok(receipt) => {
            println!("Published: {}", receipt.published);
            println!("Artifact SHA-256: {}", receipt.artifact_sha256);
            println!(
                "Install with: lean-ctx addon add {}/{}",
                plan.namespace, plan.slug
            );
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            return 1;
        }
    }
    0
}

/// `addon update <name>` — re-resolve the registry entry and reinstall when it
/// changed (GH #725). Managed binaries install side-by-side into a new version
/// dir; only after the health probe passes is the gateway pointer flipped and
/// the old version pruned — a failed update leaves the working install intact.
pub(super) fn cmd_update(name: &str, args: &[String]) -> i32 {
    let Some(entry) = InstalledStore::load().get(name).cloned() else {
        eprintln!("Addon `{name}` is not installed.");
        return 1;
    };
    if entry.source == "local" {
        eprintln!(
            "`{name}` was installed from a local manifest — update it by re-running \
             `lean-ctx addon add <path-to-lean-ctx-addon.toml>`."
        );
        return 1;
    }
    // Re-resolve from where it came: a hosted ctxpkg pack updates against the
    // registry it was installed from (latest non-yanked version), everything
    // else against the bundled registry snapshot.
    let (manifest, update_source) = if let Some(spec) = entry.source.strip_prefix("ctxpkg:") {
        let unpinned = spec.split('@').take(2).collect::<Vec<_>>().join("@");
        let Some(remote_ref) = crate::core::context_package::remote::parse_remote_ref(&unpinned)
        else {
            eprintln!(
                "`{name}` has a malformed install source `{}`.",
                entry.source
            );
            return 1;
        };
        match fetch_addon_pack(&remote_ref, flag_value(args, "--registry").as_deref()) {
            Ok((m, s)) => (m, s),
            Err(e) => {
                eprintln!("Error: {e}");
                return 1;
            }
        }
    } else {
        let Some(m) = registry::get(name) else {
            eprintln!(
                "`{name}` is no longer in the registry — remove it or reinstall from a path."
            );
            return 1;
        };
        (m, entry.source.clone())
    };

    let force = args.iter().any(|a| a == "--force" || a == "-f");
    let no_verify = args.iter().any(|a| a == "--no-verify");

    // Self-dependency root: the addon's own scoped `@ns/slug` derived from the
    // (hosted) update source, else `None` — never the bare `addon.name` slug
    // (GH #727, Finding A). A `local` source already exited above.
    let root_ref = addon_self_ref(&update_source);

    // Up-to-date check: same version and (for managed binaries) same artifact
    // pin ⇒ nothing to do. `--force` reinstalls anyway.
    let same_version = manifest.addon.version == entry.version;
    let same_artifact = match (
        manifest.artifact_for_current_platform(),
        entry.artifact.as_ref(),
    ) {
        (Some(asset), Some(receipt)) => asset.sha256.eq_ignore_ascii_case(&receipt.sha256),
        (None, None) => true,
        _ => false,
    };
    if same_version && same_artifact && !force {
        println!(
            "`{name}` is up to date (v{}).",
            if entry.version.is_empty() {
                "unversioned".to_string()
            } else {
                entry.version.clone()
            }
        );
        // A skills/context dependency may have bumped even when the addon
        // itself did not (GH #727) — refresh those without re-wiring.
        refresh_pack_dependencies(&manifest.dependencies, root_ref.as_deref(), args);
        return 0;
    }

    let cfg = crate::core::config::Config::load();
    if let Err(e) = install::preflight(&manifest, &cfg.addons, force) {
        eprintln!("Error: {e}");
        return 1;
    }

    println!(
        "Updating `{name}`: v{} → v{}",
        entry.version, manifest.addon.version
    );
    if !super::prompt::confirm("Proceed with the update?", super::prompt::wants_yes(args)) {
        println!("Aborted. Nothing was changed.");
        return 0;
    }

    let preview_deps = resolve_declared_deps(&manifest.dependencies, root_ref.as_deref(), args);
    // The wiring must expand `{pack_dir:}` against the versions the install step
    // actually landed (lockfile honoured), not the preview's highest-match
    // resolution (GH #727, Finding B).
    let installed_deps = if preview_deps.is_empty() {
        Vec::new()
    } else {
        println!("Installing declared dependencies (depth-1) …");
        install_declared_deps(&manifest.dependencies, root_ref.as_deref(), args)
    };

    let new_version = manifest.addon.version.clone();
    match provision_and_wire(
        manifest,
        &update_source,
        force,
        no_verify,
        &cfg,
        &installed_deps,
    ) {
        Ok((outcome, verified)) => {
            // The new version is wired and healthy — now prune superseded
            // managed binaries (side-by-side rollback safety until here).
            artifact_install::prune_other_versions(name, &new_version);
            println!(
                "\n✓ Updated `{}` to v{new_version} (gateway server `{}`).",
                outcome.name, outcome.gateway_server
            );
            if let Some(n) = verified {
                println!("  Verified: {n} tool(s) reachable.");
            }
            println!("  Restart your MCP client to pick up the new version.");
        }
        Err(e) => {
            eprintln!("Error: {e}\n  The previous install remains wired.");
            return 1;
        }
    }
    0
}

pub(super) fn cmd_remove(name: &str, args: &[String]) -> i32 {
    let Some(entry) = InstalledStore::load().get(name).cloned() else {
        eprintln!("Addon `{name}` is not installed.");
        return 1;
    };

    if !super::prompt::confirm(
        &format!("Remove addon `{name}` (unwire its MCP server)?"),
        super::prompt::wants_yes(args),
    ) {
        println!("Aborted.");
        return 0;
    }

    match install::remove(name) {
        Ok(outcome) => {
            println!(
                "✓ Removed `{}` (gateway server `{}`).",
                outcome.name, outcome.gateway_server
            );
            // Uninstall the bootstrapped package (#1105), best-effort — a failed
            // uninstall must never block the unwire that already succeeded.
            if let Some(receipt) = entry.install {
                println!(
                    "Uninstalling `{}` via {}…",
                    receipt.package, receipt.manager
                );
                match bootstrap::uninstall(&receipt) {
                    Ok(()) => println!("  ✓ Uninstalled."),
                    Err(e) => eprintln!(
                        "  Note: could not uninstall `{}` automatically: {e}\n  \
                         Remove it manually if you no longer need it.",
                        receipt.package
                    ),
                }
            }
            // Delete managed binaries (GH #725), best-effort for the same reason.
            if entry.artifact.is_some() && artifact_install::remove_managed_binaries(name) {
                println!("  ✓ Deleted managed binaries.");
            }
            if outcome.last_removed {
                println!(
                    "  No addons remain. The gateway stays enabled — disable it with \
                     `lean-ctx config set gateway.enabled false` if you no longer need it."
                );
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            return 1;
        }
    }
    0
}

/// `addon revoke <name>` — block an addon from running everywhere (install,
/// catalog, every proxy call). Protective, so it does not prompt.
pub(super) fn cmd_revoke(name: &str, args: &[String]) -> i32 {
    let reason = flag_value(args, "--reason").unwrap_or_else(|| "manually revoked".to_string());
    let version = flag_value(args, "--version");

    let mut list = RevocationList::load();
    list.revoke(name, &reason, version.clone());
    match list.save() {
        Ok(()) => {
            let scope =
                version.map_or_else(|| "all versions".to_string(), |v| format!("version {v}"));
            println!("✓ Revoked `{name}` ({scope}): {reason}");
            println!(
                "  It will no longer run via the gateway (its tools disappear from `ctx_tools`)."
            );
            if InstalledStore::load().get(name).is_some() {
                println!("  It is still installed — `lean-ctx addon remove {name}` to unwire it.");
            }
            crate::core::mcp_catalog::catalog::invalidate();
        }
        Err(e) => {
            eprintln!("Error: {e}");
            return 1;
        }
    }
    0
}

/// `addon unrevoke <name>` — lift a revocation (removes protection), so confirm.
pub(super) fn cmd_unrevoke(name: &str, args: &[String]) -> i32 {
    let mut list = RevocationList::load();
    if !list.revocations.contains_key(name) {
        eprintln!("Addon `{name}` is not revoked.");
        return 1;
    }
    if !super::prompt::confirm(
        &format!("Lift the revocation on `{name}` (allow it to run again)?"),
        super::prompt::wants_yes(args),
    ) {
        println!("Aborted.");
        return 0;
    }
    list.unrevoke(name);
    match list.save() {
        Ok(()) => {
            println!("✓ Lifted revocation on `{name}`.");
            crate::core::mcp_catalog::catalog::invalidate();
        }
        Err(e) => {
            eprintln!("Error: {e}");
            return 1;
        }
    }
    0
}

/// `addon revocations` — list the active local revocations.
pub(super) fn cmd_revocations() {
    let list = RevocationList::load();
    if list.revocations.is_empty() {
        println!("No revocations.");
        return;
    }
    println!("Revoked addons:\n");
    for (name, rev) in &list.revocations {
        let scope = rev
            .version
            .as_deref()
            .map(|v| format!(" (version {v})"))
            .unwrap_or_default();
        println!("  ⛔ {name}{scope} — {}", rev.reason);
    }
}

/// `addon verify` — re-check each installed addon's live wiring against the
/// integrity hash pinned at install (P2). Exits non-zero if any addon drifted.
pub(super) fn cmd_verify() -> i32 {
    use crate::core::addons::integrity::{self, IntegrityStatus};
    let findings = integrity::verify_all();
    if findings.is_empty() {
        println!("No addons installed.");
        return 0;
    }
    let mut drift = false;
    println!("Addon integrity:\n");
    for f in &findings {
        let glyph = match f.status {
            IntegrityStatus::Ok => "✓",
            IntegrityStatus::Drift => {
                drift = true;
                "⛔"
            }
            IntegrityStatus::Missing | IntegrityStatus::Unpinned => "•",
        };
        println!("  {glyph} {} — {}", f.name, f.status.label());
    }
    if drift {
        eprintln!(
            "\nOne or more addons no longer match their pinned wiring. Review the \
             `[[gateway.servers]]` entries, then re-install (`addon add`) or remove them."
        );
        return 1;
    }
    0
}
