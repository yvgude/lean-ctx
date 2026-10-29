use super::types::{
    CompressionConfig, OutputHints, Profile, ProfileAutonomy, ProfileMeta, ReadConfig,
    RoutingConfig,
};
use std::collections::HashMap;

// ── Built-in Profiles ──────────────────────────────────────

fn read_mode(mode: &str) -> ReadConfig {
    ReadConfig {
        default_mode: Some(mode.to_string()),
    }
}

fn crp(mode: Option<&str>) -> CompressionConfig {
    CompressionConfig {
        crp_mode: mode.map(str::to_string),
    }
}

fn base(
    name: &str,
    description: &str,
    read: ReadConfig,
    compression: CompressionConfig,
) -> Profile {
    Profile {
        profile: ProfileMeta {
            name: name.to_string(),
            inherits: None,
            description: description.to_string(),
        },
        read,
        compression,
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        routing: RoutingConfig::default(),
        autonomy: ProfileAutonomy::default(),
        output_hints: OutputHints::default(),
    }
}

pub(super) fn builtin_coder() -> Profile {
    Profile {
        autonomy: ProfileAutonomy {
            checkpoint_interval: Some(10),
            ..ProfileAutonomy::default()
        },
        ..base(
            "coder",
            "Default coding workflow",
            read_mode("auto"),
            crp(None),
        )
    }
}

pub(super) fn builtin_exploration() -> Profile {
    Profile {
        output_hints: OutputHints {
            related_hint: Some(true),
            compressed_hint: Some(true),
            cross_source_hint: Some(true),
            proactive_context: Some(true),
            ..OutputHints::default()
        },
        ..base(
            "exploration",
            "Broad context for understanding codebases",
            read_mode("map"),
            crp(None),
        )
    }
}

fn builtin_bugfix() -> Profile {
    Profile {
        autonomy: ProfileAutonomy {
            checkpoint_interval: Some(10),
            ..ProfileAutonomy::default()
        },
        ..base(
            "bugfix",
            "Focused context for debugging specific issues",
            read_mode("auto"),
            crp(Some("tdd")),
        )
    }
}

fn builtin_hotfix() -> Profile {
    Profile {
        autonomy: ProfileAutonomy {
            checkpoint_interval: Some(5),
            ..ProfileAutonomy::default()
        },
        ..base(
            "hotfix",
            "Minimal context, fast iteration for urgent fixes",
            read_mode("signatures"),
            crp(Some("tdd")),
        )
    }
}

fn builtin_ci_debug() -> Profile {
    base(
        "ci-debug",
        "CI/CD debugging with shell-heavy workflows",
        read_mode("auto"),
        crp(None),
    )
}

fn builtin_review() -> Profile {
    Profile {
        output_hints: OutputHints {
            verify_footer: Some(true),
            related_hint: Some(true),
            compressed_hint: Some(true),
            ..OutputHints::default()
        },
        ..base(
            "review",
            "Code review with broad read-only context",
            read_mode("map"),
            crp(Some("compact")),
        )
    }
}

fn builtin_passthrough() -> Profile {
    base(
        "passthrough",
        "No output modification — always full content, no compression",
        read_mode("full"),
        crp(Some("off")),
    )
}

/// Returns all built-in profile definitions.
pub(crate) fn builtin_profiles() -> HashMap<String, Profile> {
    let mut map = HashMap::new();
    for p in [
        builtin_coder(),
        builtin_exploration(),
        builtin_bugfix(),
        builtin_hotfix(),
        builtin_ci_debug(),
        builtin_review(),
        builtin_passthrough(),
    ] {
        map.insert(p.profile.name.clone(), p);
    }
    map
}

/// Constructs a single built-in profile by name, building only the one
/// requested.
///
/// `active_profile()` resolves to a built-in on most calls (no on-disk
/// override), and it is invoked many times per tool dispatch. Going through
/// [`builtin_profiles`] there materialized all seven profile structs just to
/// drop six — this hot-path shortcut builds exactly one. The match arms must
/// stay in sync with [`builtin_profiles`].
pub(super) fn builtin_profile(name: &str) -> Option<Profile> {
    match name {
        "coder" => Some(builtin_coder()),
        "exploration" => Some(builtin_exploration()),
        "bugfix" => Some(builtin_bugfix()),
        "hotfix" => Some(builtin_hotfix()),
        "ci-debug" => Some(builtin_ci_debug()),
        "review" => Some(builtin_review()),
        "passthrough" => Some(builtin_passthrough()),
        _ => None,
    }
}
