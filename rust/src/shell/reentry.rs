//! Re-entrancy markers that stop nested `lean-ctx` invocations from
//! double-compressing — decoupled from the user-facing activation flag.
//!
//! Two distinct signals were historically conflated in `LEAN_CTX_ACTIVE`
//! (GH #533), which silently disabled compression for any agent that
//! *inherited* it:
//!
//! - `LEAN_CTX_ACTIVE`: shell-hook re-entry guard. Stops the *shell* hook from
//!   re-firing inside a command lean-ctx already spawned. It is a plain env var
//!   an agent's top-level process can legitimately inherit.
//! - [`WRAP_MARKER`] (`LEAN_CTX_WRAPPED`): process-level ownership. Set ONLY by
//!   lean-ctx on the children it spawns, so its presence reliably means "a
//!   parent lean-ctx already owns compression of this command tree" and a
//!   nested `lean-ctx -c` must pass through. Because lean-ctx is the only
//!   writer, an agent cannot trigger it by leaking an env var.

use std::process::Command;

/// Env var lean-ctx stamps on every child it spawns to mark the command tree as
/// already owned (compression handled by the parent lean-ctx).
pub(crate) const WRAP_MARKER: &str = "LEAN_CTX_WRAPPED";

/// Env var carrying how many nested `lean-ctx` shell spawns led to this
/// process. Defense-in-depth against runaway self-recursion: a shell hook
/// that `exec`s into `lean-ctx -c`, which spawns a shell to run the real
/// command, is supposed to be a single hop — the shell lean-ctx spawns has its
/// profile/rc sourcing neutralized so the hook cannot re-fire inside it. But a
/// third-party shell integration sharing the same rc/profile file (or a
/// profile-clearing gap specific to a shell/platform) can still make the hook
/// fire again inside that "clean" shell, and the process tree then recurses
/// without bound — observed in the wild as thousands of orphaned shell/
/// lean-ctx processes and multi-GB memory exhaustion within seconds. This
/// counter survives that failure mode: once nesting exceeds
/// [`MAX_EXEC_DEPTH`], lean-ctx refuses to spawn another shell at all,
/// regardless of what is causing the repeated re-entry.
const EXEC_DEPTH_MARKER: &str = "LEAN_CTX_EXEC_DEPTH";

/// Hard ceiling on nested `lean-ctx -c`/`-t` shell spawns. Legitimate nesting
/// (an agent wrapper being unwrapped, a script re-invoking itself once) never
/// comes close to this; it exists purely to bound a runaway loop.
pub(crate) const MAX_EXEC_DEPTH: u32 = 8;

/// How many nested shell-spawning `exec` calls led to this process (0 if none).
#[must_use]
pub(crate) fn exec_depth() -> u32 {
    std::env::var(EXEC_DEPTH_MARKER)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// True once nesting is deep enough that lean-ctx must refuse to spawn yet
/// another shell rather than risk unbounded recursion.
#[must_use]
pub(crate) fn exec_depth_exceeded() -> bool {
    exec_depth() >= MAX_EXEC_DEPTH
}

/// Stamp the incremented nesting depth on a child about to be spawned. Called
/// by every function that spawns a shell to run a command, independently of
/// [`mark_child`]/[`clear_shell_default_markers`] — the depth counter must
/// keep climbing even on the `exec_shell_default` path, which deliberately
/// clears the other markers so the delegate shell's own hooks decide fresh.
pub(crate) fn stamp_exec_depth(cmd: &mut Command) {
    cmd.env(EXEC_DEPTH_MARKER, (exec_depth() + 1).to_string());
}

/// Legacy shell-hook re-entry guard. Still stamped on children so the shell
/// hook (which tests `-z "$LEAN_CTX_ACTIVE"`) does not re-fire inside a wrapped
/// command, but it no longer gates compression on its own.
pub(crate) const ACTIVE_MARKER: &str = "LEAN_CTX_ACTIVE";

/// True when `LEAN_CTX_DISABLED` forces `lean-ctx -c` / `-t` to run the
/// command raw instead of compressing it.
#[must_use]
pub(crate) fn is_disabled() -> bool {
    std::env::var("LEAN_CTX_DISABLED").is_ok()
}

/// True when a parent lean-ctx already owns this command tree. Nested explicit
/// `lean-ctx -c` should defer to the ambient shell defaults, not force raw
/// command bytes, so shell-hook compression can still apply when configured.
#[must_use]
pub(crate) fn is_wrapped() -> bool {
    std::env::var(WRAP_MARKER).is_ok()
}

/// True when `lean-ctx -c` / `-t` must avoid owning compression itself: either
/// a parent lean-ctx already owns this command tree ([`WRAP_MARKER`]) or
/// compression is globally disabled (`LEAN_CTX_DISABLED`).
///
/// Deliberately does **not** consult `LEAN_CTX_ACTIVE`: that flag is only a
/// shell-hook re-entry guard, and an agent's top-level process can inherit it,
/// which previously suppressed compression for every command it ran (#533).
#[must_use]
pub(crate) fn should_pass_through() -> bool {
    is_wrapped() || is_disabled()
}

/// Stamp a child command so nested lean-ctx invocations pass through and the
/// shell hook does not re-fire: sets both the ownership marker
/// ([`WRAP_MARKER`]) and the legacy hook guard (`LEAN_CTX_ACTIVE`).
pub(crate) fn mark_child(cmd: &mut Command) {
    cmd.env(ACTIVE_MARKER, "1").env(WRAP_MARKER, "1");
    stamp_exec_depth(cmd);
}

/// Clear lean-ctx ownership/force markers before delegating to the user's shell
/// defaults. This lets a nested explicit wrapper behave like typing the command
/// in that shell: configured hooks may compress; unconfigured shells run raw.
pub(crate) fn clear_shell_default_markers(cmd: &mut Command) {
    cmd.env_remove(ACTIVE_MARKER)
        .env_remove(WRAP_MARKER)
        .env_remove("LEAN_CTX_COMPRESS");
}

#[cfg(test)]
mod tests {
    use super::{
        ACTIVE_MARKER, EXEC_DEPTH_MARKER, MAX_EXEC_DEPTH, WRAP_MARKER, clear_shell_default_markers,
        exec_depth, exec_depth_exceeded, is_disabled, is_wrapped, mark_child, should_pass_through,
        stamp_exec_depth,
    };

    #[test]
    fn exec_depth_defaults_to_zero() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var(EXEC_DEPTH_MARKER);
        assert_eq!(exec_depth(), 0);
        assert!(!exec_depth_exceeded());
    }

    #[test]
    fn stamp_exec_depth_increments_on_child() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var(EXEC_DEPTH_MARKER, "3");
        let mut cmd = std::process::Command::new("true");
        stamp_exec_depth(&mut cmd);
        crate::test_env::remove_var(EXEC_DEPTH_MARKER);
        let envs: std::collections::HashMap<String, String> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
            .collect();
        assert_eq!(envs.get(EXEC_DEPTH_MARKER).map(String::as_str), Some("4"));
    }

    #[test]
    fn exec_depth_exceeded_at_ceiling() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var(EXEC_DEPTH_MARKER, MAX_EXEC_DEPTH.to_string());
        let exceeded = exec_depth_exceeded();
        crate::test_env::remove_var(EXEC_DEPTH_MARKER);
        assert!(
            exceeded,
            "depth equal to the ceiling must already be treated as exceeded"
        );
    }

    #[test]
    fn mark_child_also_stamps_exec_depth() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var(EXEC_DEPTH_MARKER);
        let mut cmd = std::process::Command::new("true");
        mark_child(&mut cmd);
        let envs: std::collections::HashMap<String, String> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
            .collect();
        assert_eq!(envs.get(EXEC_DEPTH_MARKER).map(String::as_str), Some("1"));
    }

    /// clear_shell_default_markers intentionally resets the ownership/compress
    /// markers so a delegate shell's own hooks decide fresh — but the depth
    /// counter must NOT be one of them, or exactly the runaway loop this guard
    /// exists for would reset its own trip-wire on every lap.
    #[test]
    fn clear_shell_default_markers_does_not_reset_exec_depth() {
        let mut cmd = std::process::Command::new("true");
        cmd.env(EXEC_DEPTH_MARKER, "5");
        clear_shell_default_markers(&mut cmd);
        let envs: std::collections::HashMap<String, String> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
            .collect();
        assert_eq!(
            envs.get(EXEC_DEPTH_MARKER).map(String::as_str),
            Some("5"),
            "exec depth must survive clear_shell_default_markers"
        );
    }

    #[test]
    fn wrap_marker_triggers_passthrough() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_DISABLED");
        crate::test_env::remove_var(ACTIVE_MARKER);
        crate::test_env::set_var(WRAP_MARKER, "1");
        assert!(is_wrapped());
        assert!(should_pass_through());
        crate::test_env::remove_var(WRAP_MARKER);
    }

    #[test]
    fn disabled_triggers_passthrough() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var(WRAP_MARKER);
        crate::test_env::remove_var(ACTIVE_MARKER);
        crate::test_env::set_var("LEAN_CTX_DISABLED", "1");
        assert!(is_disabled());
        assert!(should_pass_through());
        crate::test_env::remove_var("LEAN_CTX_DISABLED");
    }

    /// #533: an inherited/leaked `LEAN_CTX_ACTIVE` must NOT suppress compression.
    #[test]
    fn inherited_active_does_not_trigger_passthrough() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var(WRAP_MARKER);
        crate::test_env::remove_var("LEAN_CTX_DISABLED");
        crate::test_env::set_var(ACTIVE_MARKER, "1");
        assert!(
            !should_pass_through(),
            "inherited LEAN_CTX_ACTIVE must not disable compression (#533)"
        );
        crate::test_env::remove_var(ACTIVE_MARKER);
    }

    #[test]
    fn mark_child_sets_both_markers() {
        let mut cmd = std::process::Command::new("true");
        mark_child(&mut cmd);
        let envs: std::collections::HashMap<String, String> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
            .collect();
        assert_eq!(envs.get(WRAP_MARKER).map(String::as_str), Some("1"));
        assert_eq!(envs.get(ACTIVE_MARKER).map(String::as_str), Some("1"));
    }

    #[test]
    fn clear_shell_default_markers_removes_wrapping_state() {
        let mut cmd = std::process::Command::new("true");
        mark_child(&mut cmd);
        cmd.env("LEAN_CTX_COMPRESS", "1");

        clear_shell_default_markers(&mut cmd);

        let envs: std::collections::HashMap<String, Option<String>> = cmd
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_str()?.to_string(),
                    v.and_then(|v| v.to_str().map(str::to_string)),
                ))
            })
            .collect();

        assert_eq!(envs.get(WRAP_MARKER), Some(&None));
        assert_eq!(envs.get(ACTIVE_MARKER), Some(&None));
        assert_eq!(envs.get("LEAN_CTX_COMPRESS"), Some(&None));
    }
}
