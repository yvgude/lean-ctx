// SPDX-License-Identifier: Apache-2.0
//! The hook profile of one agent session (`LEAN_CTX_HOOK_PROFILE`). A
//! launching app (Hive Desktop) starts a session with `governor` while the
//! machine keeps the standard hooks: the standard actions then behave as the
//! governor's, so nothing runs twice and nothing competes with it, and the
//! global setup stays untouched.
//!
//! `governor`: `rewrite` and `redirect` (PreToolUse) act as
//! `rewrite-enforce` (security only: the allowlist wrap and the secret-path
//! deny, no compression rewrite, no Read redirect); `read-dedup`
//! (PostToolUse Read) acts as `govern` (rule R1.read). `observe` stays as it
//! is (activity and telemetry; the governor keeps it too). Unset or
//! `standard`: every action is itself.

/// Environment variable naming the session's hook profile.
pub const HOOK_PROFILE_ENV: &str = "LEAN_CTX_HOOK_PROFILE";
/// Profiles this build knows (advertised in `capabilities`).
pub const HOOK_PROFILES: &[&str] = &["standard", "governor"];

/// Whether `profile` (the variable's value) selects the governor profile.
fn is_governor(profile: Option<&str>) -> bool {
    profile.is_some_and(|p| p.trim().eq_ignore_ascii_case("governor"))
}

/// The hook action that runs for `action` under `profile`.
pub fn effective_action_for<'a>(action: &'a str, profile: Option<&str>) -> &'a str {
    if !is_governor(profile) {
        return action;
    }
    match action {
        "rewrite" | "redirect" => "rewrite-enforce",
        "read-dedup" => "govern",
        other => other,
    }
}

/// [`effective_action_for`] under this process's `LEAN_CTX_HOOK_PROFILE`.
pub fn effective_action(action: &str) -> &str {
    effective_action_for(action, std::env::var(HOOK_PROFILE_ENV).ok().as_deref())
}

#[cfg(test)]
mod tests {
    use super::effective_action_for;

    #[test]
    fn the_governor_profile_maps_standard_actions_to_the_governors() {
        let governor = Some(" Governor ");
        assert_eq!(effective_action_for("rewrite", governor), "rewrite-enforce");
        assert_eq!(
            effective_action_for("redirect", governor),
            "rewrite-enforce"
        );
        assert_eq!(effective_action_for("read-dedup", governor), "govern");
        assert_eq!(effective_action_for("observe", governor), "observe");
        for other in [None, Some("standard"), Some("bogus")] {
            assert_eq!(effective_action_for("rewrite", other), "rewrite");
            assert_eq!(effective_action_for("read-dedup", other), "read-dedup");
        }
    }
}
