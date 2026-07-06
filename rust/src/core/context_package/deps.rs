//! Depth-1 dependency resolution at install time (GH #727, Phase 3).
//!
//! A package may declare [`PackageDependency`] entries (SemVer ranges). On
//! `pack install` / `addon add`, the direct dependencies of the root package
//! are resolved against the registry index and installed alongside it — one
//! consent surface listing everything that will land.
//!
//! **Depth-1 is deliberate** (issue non-goal: no transitive graphs): only the
//! root's own dependencies resolve; a dependency's dependencies do not. That
//! keeps resolution O(deps), makes cycles impossible beyond self-reference
//! (which is refused), and keeps the consent prompt honest — nothing installs
//! that was not listed.
//!
//! Determinism: given the same registry index, resolution always picks the
//! **highest non-yanked version matching the range** — and repeated installs
//! short-circuit offline via the lockfile + local store (`already_satisfied`).

use super::manifest::{PackageDependency, PackageManifest};
use super::remote::{self, VersionInfo};

/// One resolved direct dependency, ready to download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDep {
    /// Scoped name as declared (`@ns/name`).
    pub name: String,
    /// Registry namespace (without `@`).
    pub namespace: String,
    /// Bare package name (slug).
    pub slug: String,
    /// The picked version (highest non-yanked match of the range).
    pub version: String,
    /// Artifact hash from the registry index (verified again on download).
    pub artifact_sha256: String,
}

/// Resolve the direct, non-optional dependencies of `manifest` against the
/// registry at `base`. Fails on: unscoped names, self-dependency, invalid
/// ranges, and ranges with no installable match — a partially-resolved
/// install is worse than a refused one.
pub fn resolve_dependencies(
    manifest: &PackageManifest,
    base: &str,
    token: Option<&str>,
) -> Result<Vec<ResolvedDep>, String> {
    let mut resolved = Vec::new();
    for dep in &manifest.dependencies {
        if dep.optional {
            continue;
        }
        resolved.push(resolve_one(&manifest.name, dep, base, token)?);
    }
    Ok(resolved)
}

/// Resolve a single declared dependency against the registry index.
pub fn resolve_one(
    root_name: &str,
    dep: &PackageDependency,
    base: &str,
    token: Option<&str>,
) -> Result<ResolvedDep, String> {
    let Some(remote_ref) = remote::parse_remote_ref(&dep.name) else {
        return Err(format!(
            "dependency `{}` is not a scoped @ns/name reference — unresolvable",
            dep.name
        ));
    };
    if dep.name.trim_start_matches('@') == root_name.trim_start_matches('@') {
        return Err(format!(
            "package depends on itself (`{}`) — refused",
            dep.name
        ));
    }
    let req = parse_version_req(&dep.version_req)
        .map_err(|e| format!("dependency `{}`: {e}", dep.name))?;

    let versions = remote::fetch_versions(base, &remote_ref.namespace, &remote_ref.name, token)
        .map_err(|e| format!("dependency `{}`: {e}", dep.name))?;
    let best = pick_highest_match(&versions, &req).ok_or_else(|| {
        format!(
            "dependency `{}`: no installable version matches `{}` (available: {})",
            dep.name,
            dep.version_req,
            versions
                .iter()
                .map(|v| v.version.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;

    Ok(ResolvedDep {
        name: dep.name.clone(),
        namespace: remote_ref.namespace,
        slug: remote_ref.name,
        version: best.version.clone(),
        artifact_sha256: best.artifact_sha256.clone(),
    })
}

/// Parse a SemVer range. An empty/`*` requirement means "any version".
pub fn parse_version_req(req: &str) -> Result<semver::VersionReq, String> {
    let trimmed = req.trim();
    if trimmed.is_empty() || trimmed == "*" {
        return Ok(semver::VersionReq::STAR);
    }
    semver::VersionReq::parse(trimmed).map_err(|e| format!("invalid version range `{req}`: {e}"))
}

/// Highest non-yanked version matching `req`. Non-SemVer versions in the
/// index are skipped (they can never match a range).
pub fn pick_highest_match<'a>(
    versions: &'a [VersionInfo],
    req: &semver::VersionReq,
) -> Option<&'a VersionInfo> {
    versions
        .iter()
        .filter(|v| !v.yanked)
        .filter_map(|v| Some((semver::Version::parse(&v.version).ok()?, v)))
        .filter(|(parsed, _)| req.matches(parsed))
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, v)| v)
}

/// Version of `name` pinned in the project lockfile, if any.
pub fn locked_version(name: &str, project_root: &std::path::Path) -> Option<String> {
    let lock = super::lockfile::load(project_root).ok()?;
    lock.packages
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.version.clone())
}

/// True when `name@version-satisfying-req` is already pinned in the lockfile
/// **and** present in the local store — the offline-reproducible fast path:
/// a second `pack install` touches no network for satisfied dependencies.
pub fn already_satisfied(
    project_root: &std::path::Path,
    registry: &super::registry::LocalRegistry,
    dep: &PackageDependency,
) -> Option<String> {
    let lock = super::lockfile::load(project_root).ok()?;
    let locked = lock.packages.iter().find(|p| p.name == dep.name)?;
    let req = parse_version_req(&dep.version_req).ok()?;
    let version = semver::Version::parse(&locked.version).ok()?;
    if !req.matches(&version) {
        return None;
    }
    let installed = registry.get(&dep.name, Some(&locked.version)).ok()??;
    Some(installed.version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(version: &str, yanked: bool) -> VersionInfo {
        VersionInfo {
            version: version.into(),
            artifact_sha256: "a".repeat(64),
            yanked,
        }
    }

    #[test]
    fn picks_highest_matching_version() {
        let versions = [v("1.0.0", false), v("1.2.0", false), v("2.0.0", false)];
        let req = parse_version_req("^1.0").unwrap();
        assert_eq!(
            pick_highest_match(&versions, &req).unwrap().version,
            "1.2.0"
        );
    }

    #[test]
    fn yanked_versions_never_match() {
        let versions = [v("1.0.0", false), v("1.3.0", true)];
        let req = parse_version_req("^1.0").unwrap();
        assert_eq!(
            pick_highest_match(&versions, &req).unwrap().version,
            "1.0.0"
        );
    }

    #[test]
    fn no_match_yields_none() {
        let versions = [v("1.0.0", false)];
        let req = parse_version_req("^2.0").unwrap();
        assert!(pick_highest_match(&versions, &req).is_none());
    }

    #[test]
    fn star_and_empty_match_anything() {
        let versions = [v("0.3.7", false)];
        for raw in ["", "*", "  "] {
            let req = parse_version_req(raw).unwrap();
            assert_eq!(
                pick_highest_match(&versions, &req).unwrap().version,
                "0.3.7",
                "req `{raw}`"
            );
        }
    }

    #[test]
    fn non_semver_index_entries_are_skipped() {
        let versions = [v("not-a-version", false), v("1.1.0", false)];
        let req = parse_version_req("^1").unwrap();
        assert_eq!(
            pick_highest_match(&versions, &req).unwrap().version,
            "1.1.0"
        );
    }

    #[test]
    fn invalid_range_is_an_error() {
        assert!(parse_version_req(">>nope<<").is_err());
    }

    #[test]
    fn self_dependency_is_refused() {
        let mut manifest = crate::core::context_package::manifest::PackageManifest {
            dependencies: vec![PackageDependency {
                name: "@acme/root".into(),
                version_req: "^1".into(),
                optional: false,
            }],
            ..minimal("@acme/root")
        };
        // resolve_one is exercised via resolve_dependencies; the self-check
        // fires before any network I/O, so an invalid base URL never matters.
        let err = resolve_dependencies(&manifest, "http://127.0.0.1:1", None).unwrap_err();
        assert!(err.contains("depends on itself"), "got: {err}");

        // Optional dependencies are skipped entirely.
        manifest.dependencies[0].optional = true;
        assert_eq!(
            resolve_dependencies(&manifest, "http://127.0.0.1:1", None).unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn unscoped_dependency_is_refused() {
        let manifest = crate::core::context_package::manifest::PackageManifest {
            dependencies: vec![PackageDependency {
                name: "plain-name".into(),
                version_req: "^1".into(),
                optional: false,
            }],
            ..minimal("@acme/root")
        };
        let err = resolve_dependencies(&manifest, "http://127.0.0.1:1", None).unwrap_err();
        assert!(err.contains("not a scoped"), "got: {err}");
    }

    fn minimal(name: &str) -> crate::core::context_package::manifest::PackageManifest {
        use crate::core::context_package::manifest::*;
        PackageManifest {
            schema_version: crate::core::contracts::CONTEXT_PACKAGE_V2_SCHEMA_VERSION,
            conformance_level: None,
            kind: PackageKind::default(),
            name: name.into(),
            version: "1.0.0".into(),
            description: "d".into(),
            author: None,
            scope: None,
            created_at: chrono::Utc::now(),
            updated_at: None,
            layers: vec![],
            dependencies: vec![],
            tags: vec![],
            visibility: None,
            integrity: PackageIntegrity {
                sha256: "a".repeat(64),
                content_hash: "b".repeat(64),
                byte_size: 1,
            },
            provenance: PackageProvenance {
                tool: "lean-ctx".into(),
                tool_version: "0".into(),
                project_hash: None,
                source_session_id: None,
            },
            compatibility: CompatibilitySpec::default(),
            stats: PackageStats::default(),
            signature: None,
            graph_summary: None,
            marketplace: None,
        }
    }
}
