use std::collections::HashSet;
use std::sync::Mutex;

use super::registry::LocalRegistry;

static APPLIED_PACKAGES: Mutex<Option<HashSet<String>>> = Mutex::new(None);

pub(crate) fn auto_load_packages(project_root: &str) -> Vec<String> {
    let Ok(registry) = LocalRegistry::open() else {
        return Vec::new();
    };

    let Ok(packages) = registry.auto_load_packages(project_root) else {
        return Vec::new();
    };

    if packages.is_empty() {
        return Vec::new();
    }

    let mut guard = APPLIED_PACKAGES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let applied = guard.get_or_insert_with(HashSet::new);

    let mut loaded = Vec::new();

    for entry in &packages {
        let key = format!("{}@{}:{project_root}", entry.name, entry.version);
        if applied.contains(&key) {
            continue;
        }

        let Ok(package) = registry.load_admitted(&entry.name, &entry.version) else {
            tracing::warn!(
                "ctxpkg auto-load: failed to load {} v{}",
                entry.name,
                entry.version
            );
            continue;
        };

        match super::loader::load_admitted_package(&package, project_root) {
            Ok(report) => {
                applied.insert(key);
                let label = format!("{} v{}", entry.name, entry.version);
                if !report.warnings.is_empty() {
                    for w in &report.warnings {
                        tracing::warn!("ctxpkg auto-load {label}: {w}");
                    }
                }
                loaded.push(label);
            }
            Err(e) => {
                tracing::warn!(
                    "ctxpkg auto-load: apply failed for {} v{}: {e}",
                    entry.name,
                    entry.version
                );
            }
        }
    }

    loaded
}
