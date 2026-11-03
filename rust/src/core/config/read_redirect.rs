//! Read-redirect control — whether the PreToolUse redirect hook rewrites a native
//! Read into a compressed `ctx_read` temp copy.
//!
//! Some hosts (Claude Code / CodeBuddy) enforce a read-before-write guard on their
//! native Write/Edit tools, keyed on the path the harness saw read. The redirect
//! hook swaps that path to a temp `.lctx` copy, so the guard tracks the temp and a
//! later native Write to the real file fails with "File has not been read yet"
//! (GH #637). On those hosts `auto` disables the Read redirect so native Read reads
//! the real file and the guard stays intact; compression there flows through the
//! explicit `ctx_read` MCP tool and the (guard-safe) Grep/Glob redirect instead.

use serde::{Deserialize, Serialize};

use super::Config;

/// Controls the native-Read → `ctx_read` redirect hook.
///
/// - `Auto`: (Default) redirect everywhere except hosts with a native
///   read-before-write guard (Claude Code / CodeBuddy), where the path-swap would
///   break native Write/Edit (#637).
/// - `On`: always redirect (legacy behavior; power users on non-guard hosts).
/// - `Off`: never redirect native Read.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReadRedirect {
    #[default]
    Auto,
    On,
    Off,
}

impl ReadRedirect {
    /// Parse `LEAN_CTX_READ_REDIRECT`. Accepts the canonical `auto|on|off` plus the
    /// usual boolean spellings so an env-first user can flip it like any toggle.
    pub fn from_env() -> Option<Self> {
        std::env::var("LEAN_CTX_READ_REDIRECT").ok().and_then(|v| {
            match v.trim().to_lowercase().as_str() {
                "auto" => Some(Self::Auto),
                "on" | "1" | "true" | "yes" => Some(Self::On),
                "off" | "0" | "false" | "no" => Some(Self::Off),
                _ => None,
            }
        })
    }

    /// Env override (`LEAN_CTX_READ_REDIRECT`) wins over the on-disk config value.
    pub fn effective(config: &Config) -> Self {
        Self::from_env().unwrap_or(config.read_redirect)
    }

    /// Whether the native-Read redirect should rewrite the Read into a temp
    /// `ctx_read` copy for the current process/host. `Auto` disables it on hosts
    /// with a read-before-write guard so native Write/Edit keeps working (#637).
    pub fn read_redirect_enabled(config: &Config) -> bool {
        if config.prefer_native_editor_effective() {
            return false;
        }
        match Self::effective(config) {
            Self::On => true,
            Self::Off => false,
            Self::Auto => !host_has_read_before_write_guard(),
        }
    }
}

/// True on hosts whose native Write/Edit enforce a read-before-write guard keyed on
/// the last-read path (Claude Code and the CodeBuddy fork).
///
/// This runs *inside* the PreToolUse hook subprocess, so it keys on the marker Claude
/// Code actually exports to hooks: `CLAUDE_PROJECT_DIR` (documented, present both
/// interactively and in headless `claude -p`). `CLAUDECODE` lives in the host's own
/// process but is **not** propagated to hook children — verified empirically for
/// #637 — so it cannot be the primary in-hook signal. `CLAUDECODE` / `CODEBUDDY` are
/// kept as extra markers (other entry points, the CodeBuddy fork) at no cost.
///
/// **Cursor exception (GH #720):** Cursor ≥ 3.7 exports `CLAUDE_PROJECT_DIR` to its
/// hook children for Claude-compat — alongside its own `CURSOR_*` markers. Cursor
/// has no read-before-write guard, so treating it as Claude Code silently disabled
/// the Read redirect for every Cursor session (0% read compression). The host's own
/// identity markers win over the compat variable.
///
/// Shared with [`super::read_dedup`]: the PostToolUse re-read dedup targets exactly
/// the hosts where this guard forces the PreToolUse redirect off.
pub(crate) fn host_has_read_before_write_guard() -> bool {
    if hook_host_is_cursor() {
        return false;
    }
    std::env::var_os("CLAUDE_PROJECT_DIR").is_some()
        || std::env::var_os("CLAUDECODE").is_some()
        || std::env::var_os("CODEBUDDY").is_some()
}

/// True when the hook subprocess was spawned by Cursor. Checks the markers Cursor
/// actually exports to hook children (verified 3.7.36: `CURSOR_VERSION`,
/// `CURSOR_PROJECT_DIR`, `CURSOR_TRANSCRIPT_PATH`, `CURSOR_EXTENSION_HOST_ROLE`)
/// plus `CURSOR_AGENT`/`CURSOR_TRACE_ID` for agent shells and older builds.
pub fn hook_host_is_cursor() -> bool {
    [
        "CURSOR_VERSION",
        "CURSOR_PROJECT_DIR",
        "CURSOR_TRANSCRIPT_PATH",
        "CURSOR_EXTENSION_HOST_ROLE",
        "CURSOR_AGENT",
        "CURSOR_TRACE_ID",
    ]
    .iter()
    .any(|var| std::env::var_os(var).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_auto() {
        assert_eq!(ReadRedirect::default(), ReadRedirect::Auto);
    }

    #[test]
    fn serde_roundtrip_kebab() {
        #[derive(Deserialize)]
        struct Wrapper {
            read_redirect: ReadRedirect,
        }
        for (raw, want) in [
            ("auto", ReadRedirect::Auto),
            ("on", ReadRedirect::On),
            ("off", ReadRedirect::Off),
        ] {
            let w: Wrapper = toml::from_str(&format!("read_redirect = \"{raw}\"")).expect("parse");
            assert_eq!(w.read_redirect, want, "{raw}");
        }
    }

    #[test]
    fn from_env_parses_canonical_and_boolean_spellings() {
        let _lock = crate::core::data_dir::test_env_lock();

        crate::test_env::set_var("LEAN_CTX_READ_REDIRECT", "auto");
        assert_eq!(ReadRedirect::from_env(), Some(ReadRedirect::Auto));
        crate::test_env::set_var("LEAN_CTX_READ_REDIRECT", "ON");
        assert_eq!(ReadRedirect::from_env(), Some(ReadRedirect::On));
        crate::test_env::set_var("LEAN_CTX_READ_REDIRECT", " off ");
        assert_eq!(ReadRedirect::from_env(), Some(ReadRedirect::Off));
        crate::test_env::set_var("LEAN_CTX_READ_REDIRECT", "1");
        assert_eq!(ReadRedirect::from_env(), Some(ReadRedirect::On));
        crate::test_env::set_var("LEAN_CTX_READ_REDIRECT", "nonsense");
        assert_eq!(ReadRedirect::from_env(), None);
        crate::test_env::remove_var("LEAN_CTX_READ_REDIRECT");
        assert_eq!(ReadRedirect::from_env(), None);
    }

    #[test]
    fn effective_env_overrides_config() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_READ_REDIRECT");

        let cfg = Config {
            read_redirect: ReadRedirect::On,
            ..Config::default()
        };
        assert_eq!(ReadRedirect::effective(&cfg), ReadRedirect::On);

        crate::test_env::set_var("LEAN_CTX_READ_REDIRECT", "off");
        assert_eq!(ReadRedirect::effective(&cfg), ReadRedirect::Off);
        crate::test_env::remove_var("LEAN_CTX_READ_REDIRECT");
    }

    /// Clear every env var the guard detection reads, so tests are
    /// deterministic no matter which host runs `cargo test` (a Cursor agent
    /// shell exports `CURSOR_*` itself, GH #720).
    fn clear_host_markers() {
        for var in [
            "LEAN_CTX_READ_REDIRECT",
            "CLAUDE_PROJECT_DIR",
            "CLAUDECODE",
            "CODEBUDDY",
            "CURSOR_VERSION",
            "CURSOR_PROJECT_DIR",
            "CURSOR_TRANSCRIPT_PATH",
            "CURSOR_EXTENSION_HOST_ROLE",
            "CURSOR_AGENT",
            "CURSOR_TRACE_ID",
        ] {
            crate::test_env::remove_var(var);
        }
    }

    #[test]
    fn enabled_on_and_off_are_absolute() {
        let _lock = crate::core::data_dir::test_env_lock();
        clear_host_markers();

        // `on` redirects even under a guard host.
        let cfg_on = Config {
            read_redirect: ReadRedirect::On,
            ..Config::default()
        };
        crate::test_env::set_var("CLAUDE_PROJECT_DIR", "/repo");
        assert!(ReadRedirect::read_redirect_enabled(&cfg_on));
        crate::test_env::remove_var("CLAUDE_PROJECT_DIR");

        let cfg_off = Config {
            read_redirect: ReadRedirect::Off,
            ..Config::default()
        };
        assert!(!ReadRedirect::read_redirect_enabled(&cfg_off));
    }

    #[test]
    fn prefer_native_editor_disables_redirect() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_PREFER_NATIVE_EDITOR");

        let config = Config {
            prefer_native_editor: true,
            ..Default::default()
        };
        assert!(!ReadRedirect::read_redirect_enabled(&config));
    }

    #[test]
    fn auto_disables_only_on_guard_hosts() {
        let _lock = crate::core::data_dir::test_env_lock();
        clear_host_markers();

        let cfg = Config::default(); // Auto
        assert!(
            ReadRedirect::read_redirect_enabled(&cfg),
            "auto redirects when no guard host is detected"
        );

        // The marker Claude Code actually exports to hook subprocesses (#637) — the
        // one that makes the fix work in headless `claude -p`.
        crate::test_env::set_var("CLAUDE_PROJECT_DIR", "/repo");
        assert!(
            !ReadRedirect::read_redirect_enabled(&cfg),
            "auto must disable the Read redirect under Claude Code hooks (CLAUDE_PROJECT_DIR)"
        );
        crate::test_env::remove_var("CLAUDE_PROJECT_DIR");

        crate::test_env::set_var("CLAUDECODE", "1");
        assert!(
            !ReadRedirect::read_redirect_enabled(&cfg),
            "auto must also disable under the CLAUDECODE marker"
        );
        crate::test_env::remove_var("CLAUDECODE");

        crate::test_env::set_var("CODEBUDDY", "1");
        assert!(
            !ReadRedirect::read_redirect_enabled(&cfg),
            "auto must disable the Read redirect under CodeBuddy (shared Claude hook contract)"
        );
        crate::test_env::remove_var("CODEBUDDY");
    }

    /// GH #720: Cursor ≥ 3.7 exports `CLAUDE_PROJECT_DIR` to hook children for
    /// Claude-compat. Cursor has no read-before-write guard, so its own markers
    /// must override the compat variable — otherwise every Cursor session runs
    /// with the Read redirect silently disabled (0% read compression).
    #[test]
    fn auto_stays_enabled_when_cursor_exports_claude_project_dir() {
        let _lock = crate::core::data_dir::test_env_lock();
        clear_host_markers();
        let cfg = Config::default(); // Auto

        // Exactly what the Cursor 3.7.36 extension host exports to hooks.
        crate::test_env::set_var("CLAUDE_PROJECT_DIR", "/repo");
        crate::test_env::set_var("CURSOR_PROJECT_DIR", "/repo");
        crate::test_env::set_var("CURSOR_VERSION", "3.7.36");
        assert!(
            ReadRedirect::read_redirect_enabled(&cfg),
            "Cursor markers must win over the Claude-compat variable"
        );
        assert!(!host_has_read_before_write_guard());

        // Agent-shell shape: only CURSOR_AGENT is present.
        clear_host_markers();
        crate::test_env::set_var("CLAUDE_PROJECT_DIR", "/repo");
        crate::test_env::set_var("CURSOR_AGENT", "1");
        assert!(ReadRedirect::read_redirect_enabled(&cfg));

        // Without any Cursor marker the Claude guard still wins (real Claude Code).
        clear_host_markers();
        crate::test_env::set_var("CLAUDE_PROJECT_DIR", "/repo");
        assert!(!ReadRedirect::read_redirect_enabled(&cfg));
        clear_host_markers();
    }
}
