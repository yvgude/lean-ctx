// SPDX-License-Identifier: Apache-2.0
//! The editor targets behind the jail's IDE config allow dirs, cached (#2195).
//!
//! With `allow_ide_config_dirs`, every jailed path check rebuilt
//! [`build_targets`](crate::core::editor_registry::build_targets), which probes
//! each supported editor on `PATH` (`where.exe` on Windows, about 80 process
//! spawns a minute). The target list is now built once per key and reused.
//!
//! Invalidation: the key is the home directory, the config generation
//! ([`Config::generation`](crate::core::config::Config::generation), which
//! advances whenever a changed global or project config is loaded) and the
//! environment the registry reads (`CLAUDE_CONFIG_DIR`, `APPDATA`, `PATH`,
//! `PI_CODING_AGENT_DIR`, `DEEPSEEK_MCP_CONFIG`, `CODEX_HOME`, …; a test keeps
//! the list in step with the registry's sources).
//! An editor installed or removed later changes no config and no environment,
//! so an entry also expires after [`TARGETS_TTL`].
//!
//! Only the target list is cached. The directory checks and canonicalization
//! that turn it into allow dirs (`collect_ide_allow_dirs`) still run on every
//! check, so the jail decides exactly as it did uncached.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::core::editor_registry::EditorTarget;

/// How long one target list is reused at most, for changes nothing signals.
pub(super) const TARGETS_TTL: Duration = Duration::from_mins(1);

/// Environment variables `build_targets` reads for its paths and probes.
/// `every_variable_the_registry_reads_is_in_the_key` scans the sources
/// `build_targets` reaches (`tests::REGISTRY_SOURCES`),
/// so a variable added there without being added here fails the test.
const EDITOR_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "XDG_CONFIG_HOME",
    "CLAUDE_CONFIG_DIR",
    "CODEBUDDY_CONFIG_DIR",
    "CLINE_DATA_DIR",
    "CLINE_MCP_SETTINGS_PATH",
    "PI_CODING_AGENT_DIR",
    "DEEPSEEK_MCP_CONFIG",
    "CODEX_HOME",
    "CODEX_PROFILE",
    "LEAN_CTX_CODEX_PROFILE",
];

/// What a cached target list was built for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TargetsKey {
    home: PathBuf,
    config_generation: u64,
    env: u64,
}

impl TargetsKey {
    /// The key for `home` under the current config and environment.
    pub(super) fn current(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            config_generation: crate::core::config::Config::generation(),
            env: editor_env_fingerprint(),
        }
    }
}

fn editor_env_fingerprint() -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for name in EDITOR_ENV {
        std::env::var_os(name).hash(&mut hasher);
    }
    hasher.finish()
}

pub(super) struct TargetsEntry {
    key: TargetsKey,
    built_at: Instant,
    targets: Arc<Vec<EditorTarget>>,
}

pub(super) type TargetsSlot = Mutex<Option<TargetsEntry>>;

/// The process-wide target list for the jail.
static IDE_TARGETS: TargetsSlot = Mutex::new(None);

/// The editor targets for `home`, built at most once per key and TTL.
pub(super) fn editor_targets(home: &Path) -> Arc<Vec<EditorTarget>> {
    targets_in(
        &IDE_TARGETS,
        TargetsKey::current(home),
        Instant::now(),
        crate::core::editor_registry::build_targets,
    )
}

/// [`editor_targets`] with the slot, key, clock and builder explicit.
pub(super) fn targets_in(
    slot: &TargetsSlot,
    key: TargetsKey,
    now: Instant,
    build: impl FnOnce(&Path) -> Vec<EditorTarget>,
) -> Arc<Vec<EditorTarget>> {
    if let Ok(guard) = slot.lock()
        && let Some(entry) = guard.as_ref()
        && entry.key == key
        && now.saturating_duration_since(entry.built_at) < TARGETS_TTL
    {
        return Arc::clone(&entry.targets);
    }
    // Built without the lock held: probing the editors spawns processes.
    // A poisoned lock only costs the cache, never the check.
    let targets = Arc::new(build(&key.home));
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(TargetsEntry {
            key,
            built_at: now,
            targets: Arc::clone(&targets),
        });
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// The sources whose code `build_targets` runs: the registry itself and
    /// the helpers it calls for paths (home, Codex dir, OpenCode dir).
    pub(super) const REGISTRY_SOURCES: &[(&str, &str)] = &[
        (
            "editor_registry/detect.rs",
            include_str!("editor_registry/detect.rs"),
        ),
        (
            "editor_registry/paths.rs",
            include_str!("editor_registry/paths.rs"),
        ),
        ("opencode_config.rs", include_str!("opencode_config.rs")),
        (
            "lean-ctx-fs/src/home.rs",
            include_str!("../../crates/lean-ctx-fs/src/home.rs"),
        ),
    ];

    /// The variable names a source reads: `env::var("X")`, `env::var_os("X")`
    /// and `const …_ENV: &str = "X"` (read through `env::var(key)`), outside
    /// its `mod tests`.
    fn env_names(source: &str) -> Vec<String> {
        let code = source.split("\nmod tests {").next().unwrap_or(source);
        let mut names = Vec::new();
        for marker in ["env::var(\"", "env::var_os(\"", "_ENV: &str = \""] {
            for (at, _) in code.match_indices(marker) {
                let rest = &code[at + marker.len()..];
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
                    .collect();
                if !name.is_empty() && rest[name.len()..].starts_with('"') {
                    names.push(name);
                }
            }
        }
        names
    }

    /// #2195 r1: `PI_CODING_AGENT_DIR` and `DEEPSEEK_MCP_CONFIG` were read by
    /// the registry but missing from the key, so a changed value kept the old
    /// directory allowed until the TTL ran out.
    #[test]
    fn every_variable_the_registry_reads_is_in_the_key() {
        let mut seen = Vec::new();
        for (file, source) in REGISTRY_SOURCES {
            for name in env_names(source) {
                assert!(
                    EDITOR_ENV.contains(&name.as_str()),
                    "{file} reads {name}, which the cache key does not cover"
                );
                seen.push(name);
            }
        }
        for name in [
            "PI_CODING_AGENT_DIR",
            "DEEPSEEK_MCP_CONFIG",
            "CODEX_HOME",
            "HOME",
        ] {
            assert!(seen.iter().any(|s| s == name), "the scan finds {name}");
        }
    }

    #[test]
    fn a_changed_registry_variable_changes_the_key() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let home = Path::new("/home/u");
        for name in ["PI_CODING_AGENT_DIR", "DEEPSEEK_MCP_CONFIG"] {
            let saved = std::env::var_os(name);
            crate::test_env::set_var(name, "/tmp/one");
            let before = TargetsKey::current(home);
            crate::test_env::set_var(name, "/tmp/two");
            let after = TargetsKey::current(home);
            match saved {
                Some(v) => crate::test_env::set_var(name, v),
                None => crate::test_env::remove_var(name),
            }
            assert_ne!(before, after, "{name} is part of the key");
        }
    }

    fn key(home: &str, config_generation: u64) -> TargetsKey {
        TargetsKey {
            home: PathBuf::from(home),
            config_generation,
            env: 7,
        }
    }

    fn counting(builds: &Cell<u32>) -> impl Fn(&Path) -> Vec<EditorTarget> + '_ {
        |home| {
            builds.set(builds.get() + 1);
            crate::core::editor_registry::build_targets(home)
        }
    }

    #[test]
    fn repeated_checks_build_the_targets_once() {
        let slot = TargetsSlot::new(None);
        let builds = Cell::new(0);
        let start = Instant::now();
        for i in 0..80 {
            targets_in(
                &slot,
                key("/home/u", 1),
                start + Duration::from_millis(i * 500),
                counting(&builds),
            );
        }
        assert_eq!(builds.get(), 1, "80 checks within the TTL: one build");
    }

    #[test]
    fn a_config_change_home_change_or_expiry_builds_again() {
        let slot = TargetsSlot::new(None);
        let builds = Cell::new(0);
        let start = Instant::now();
        targets_in(&slot, key("/home/u", 1), start, counting(&builds));
        targets_in(&slot, key("/home/u", 2), start, counting(&builds));
        assert_eq!(builds.get(), 2, "a new config generation rebuilds");
        targets_in(&slot, key("/home/u", 2), start, counting(&builds));
        assert_eq!(builds.get(), 2, "the new entry is reused");
        targets_in(&slot, key("/home/v", 2), start, counting(&builds));
        assert_eq!(builds.get(), 3, "another home rebuilds");
        targets_in(
            &slot,
            key("/home/v", 2),
            start + TARGETS_TTL,
            counting(&builds),
        );
        assert_eq!(builds.get(), 4, "an expired entry rebuilds");
    }

    #[test]
    fn a_changed_config_file_advances_the_key() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let path = crate::core::config::Config::path().expect("isolated config path");
        std::fs::create_dir_all(path.parent().expect("config dir")).expect("config dir");
        std::fs::write(&path, "allow_ide_config_dirs = true\n").expect("write config");
        // The generation is process-wide: a parallel test that loads another
        // config advances it too. An unchanged config must keep the key in at
        // least one undisturbed window; a regression changes it every time.
        let mut before = None;
        for _ in 0..50 {
            let _ = crate::core::config::Config::load();
            let first = TargetsKey::current(Path::new("/home/u"));
            let _ = crate::core::config::Config::load();
            if TargetsKey::current(Path::new("/home/u")) == first {
                before = Some(first);
                break;
            }
        }
        let before = before.expect("an unchanged config keeps the key");
        std::fs::write(&path, "allow_ide_config_dirs = false\n").expect("write config");
        let _ = crate::core::config::Config::load();
        assert_ne!(
            TargetsKey::current(Path::new("/home/u")),
            before,
            "a changed config advances the key"
        );
    }

    #[test]
    fn cached_allow_dirs_and_jail_decisions_match_the_uncached_ones() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let home = tempfile::tempdir().expect("home");
        let home = crate::core::pathjail::canonicalize_or_self(home.path());
        for dir in [".cursor", ".codex", ".claude", ".gemini", "projects/app"] {
            std::fs::create_dir_all(home.join(dir)).expect("editor dir");
        }
        let uncached = || {
            let mut out = Vec::new();
            super::super::collect_ide_allow_dirs(
                &home,
                &crate::core::editor_registry::build_targets(&home),
                &mut out,
            );
            out
        };
        let expected = uncached();
        assert!(!expected.is_empty(), "the sandbox home has editor dirs");
        for _ in 0..3 {
            assert_eq!(super::super::home_allow_dirs(&home, true), expected);
        }
        assert!(super::super::home_allow_dirs(&home, false).is_empty());

        // A directory created after the list was cached is decided as before.
        std::fs::create_dir_all(home.join(".windsurf")).expect("late editor dir");
        let expected = uncached();
        let cached = super::super::home_allow_dirs(&home, true);
        assert_eq!(cached, expected);
        for candidate in [
            home.join(".cursor/mcp.json"),
            home.join(".windsurf/settings.json"),
            home.join("projects/app/src/main.rs"),
            home.join(".ssh/id_ed25519"),
        ] {
            let allowed = |dirs: &[PathBuf]| dirs.iter().any(|d| candidate.starts_with(d));
            assert_eq!(
                allowed(&cached),
                allowed(&expected),
                "{} is decided as uncached",
                candidate.display()
            );
        }
    }
}
