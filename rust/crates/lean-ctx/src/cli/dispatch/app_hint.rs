// SPDX-License-Identifier: Apache-2.0
//! The one hint about LeanCTX Code, the free desktop app.
//!
//! Printed after a plain human `lean-ctx doctor`, in place of that run's
//! Cockpit footer, at most once per LeanCTX minor version: the version it
//! was last shown for is kept in `<data dir>/app/hint-shown`. Never when the
//! app already manages this machine, never for machine output, a redirect,
//! CI or `LEAN_CTX_NO_COCKPIT`, and never with `LEAN_CTX_NO_APP_HINT` set.
//! The text is a constant: no version, date, counter, path or token.

use std::path::{Path, PathBuf};

use super::cockpit_hint::{self, Surroundings};
use lean_ctx::core::app_presence;

/// The hint, word for word.
pub(super) const APP_HINT: &str = "LeanCTX Code is the free desktop app for LeanCTX: leanctx.com \u{2014} hide with LEAN_CTX_NO_APP_HINT=1";

/// Everything the decision depends on, read once per run.
#[derive(Debug, Clone, Copy)]
pub(super) struct HintState<'a> {
    pub(super) surroundings: Surroundings,
    /// `LEAN_CTX_NO_APP_HINT` is set.
    pub(super) suppressed: bool,
    pub(super) app_detected: bool,
    /// The minor version the hint was last shown for.
    pub(super) last_shown: Option<&'a str>,
    /// This build's minor version, `major.minor`.
    pub(super) minor: &'a str,
}

/// Whether the hint is due after `lean-ctx doctor <args>`. Pure.
pub(super) fn due(args: &[String], state: HintState<'_>) -> bool {
    args.is_empty()
        && state.surroundings.person()
        && !state.suppressed
        && !state.app_detected
        && state.last_shown != Some(state.minor)
}

/// `major.minor` of a version string.
pub(super) fn minor_of(version: &str) -> String {
    version.split('.').take(2).collect::<Vec<_>>().join(".")
}

fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join("app").join("hint-shown")
}

fn read_last_shown(data_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(state_path(data_dir)).ok()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty() && line.len() <= 16).then(|| line.to_string())
}

/// Records that the hint was shown for `minor`; the hint is printed only
/// when this succeeds, so a read-only data dir never repeats it. Written
/// atomically and owner-only, the rule the app follows for its marker.
fn record_shown(data_dir: &Path, minor: &str) -> bool {
    let path = state_path(data_dir);
    let Some(dir) = path.parent() else {
        return false;
    };
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    if builder.create(dir).is_err() {
        return false;
    }
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // `recursive` keeps the mode of a directory that already exists:
        // tighten a real one we own, best effort; never follow a symlink.
        // SAFETY: geteuid has no arguments or preconditions and only reads identity.
        let euid = unsafe { libc::geteuid() };
        if let Ok(meta) = std::fs::symlink_metadata(dir)
            && meta.is_dir()
            && meta.uid() == euid
            && meta.mode() & 0o077 != 0
        {
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        Some(std::fs::Permissions::from_mode(0o600))
    };
    #[cfg(not(unix))]
    let permissions = None::<std::fs::Permissions>;
    lean_ctx::core::atomic_fs::try_atomic_write(
        &path,
        format!("{minor}\n").as_bytes(),
        permissions.as_ref(),
    )
    .is_ok()
}

/// The line to print after `lean-ctx doctor <args>` for `data_dir`, if the
/// hint is due; records it as shown.
pub(super) fn take_hint(
    args: &[String],
    data_dir: &Path,
    surroundings: Surroundings,
    suppressed: bool,
    minor: &str,
) -> Option<&'static str> {
    let app_detected = app_presence::detect_in(data_dir).is_some();
    if app_detected {
        lean_ctx::core::telemetry_features::record_app_detected();
    }
    let last_shown = read_last_shown(data_dir);
    let state = HintState {
        surroundings,
        suppressed,
        app_detected,
        last_shown: last_shown.as_deref(),
        minor,
    };
    if !due(args, state) || !record_shown(data_dir, minor) {
        return None;
    }
    lean_ctx::core::telemetry_features::record_app_hint_shown();
    Some(APP_HINT)
}

/// After `lean-ctx doctor`: the app hint when due, otherwise the Cockpit
/// footer — never both.
pub(super) fn after_doctor(args: &[String]) {
    let surroundings = Surroundings::now();
    let suppressed = lean_ctx::core::host_env::var("LEAN_CTX_NO_APP_HINT").is_some();
    let minor = minor_of(env!("CARGO_PKG_VERSION"));
    let hint = lean_ctx::core::data_dir::lean_ctx_data_dir()
        .ok()
        .and_then(|dir| take_hint(args, &dir, surroundings, suppressed, &minor));
    match hint {
        Some(line) => eprintln!("\n\x1b[2m{line}\x1b[0m"),
        None => cockpit_hint::print_footer("diagnosis", args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERSON: Surroundings = Surroundings {
        terminal: true,
        ci: false,
        opted_out: false,
    };

    fn state(last_shown: Option<&str>) -> HintState<'_> {
        HintState {
            surroundings: PERSON,
            suppressed: false,
            app_detected: false,
            last_shown,
            minor: "3.12",
        }
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_person_running_plain_doctor_gets_the_hint_once_per_minor_version() {
        assert!(due(&[], state(None)));
        assert!(due(&[], state(Some("3.11"))));
        assert!(!due(&[], state(Some("3.12"))));
        assert_eq!(minor_of("3.12.3"), "3.12");
        assert_eq!(minor_of("3.12.3-rc.1"), "3.12");
    }

    #[test]
    fn machine_output_pipes_ci_opt_outs_and_a_managed_machine_get_no_hint() {
        for flags in [
            &["--json"][..],
            &["--fix"],
            &["gate"],
            &["--format", "json"],
        ] {
            assert!(!due(&args(flags), state(None)), "{flags:?}");
        }
        let quiet = [
            HintState {
                surroundings: Surroundings {
                    terminal: false,
                    ..PERSON
                },
                ..state(None)
            },
            HintState {
                surroundings: Surroundings { ci: true, ..PERSON },
                ..state(None)
            },
            HintState {
                surroundings: Surroundings {
                    opted_out: true,
                    ..PERSON
                },
                ..state(None)
            },
            HintState {
                suppressed: true,
                ..state(None)
            },
            HintState {
                app_detected: true,
                ..state(None)
            },
        ];
        for case in quiet {
            assert!(!due(&[], case), "{case:?}");
        }
    }

    #[test]
    fn the_hint_is_constant_and_names_how_to_hide_it() {
        assert!(APP_HINT.contains("LeanCTX Code"));
        assert!(APP_HINT.contains("LEAN_CTX_NO_APP_HINT"));
        assert!(!APP_HINT.contains("Cloud"));
        assert!(!APP_HINT.contains(env!("CARGO_PKG_VERSION")));
        assert!(!APP_HINT.contains("http") && !APP_HINT.contains("token"));
    }

    #[test]
    fn take_hint_shows_once_records_the_version_and_stays_quiet_when_managed() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            take_hint(&[], dir.path(), PERSON, false, "3.12"),
            Some(APP_HINT)
        );
        assert_eq!(read_last_shown(dir.path()).as_deref(), Some("3.12"));
        assert_eq!(take_hint(&[], dir.path(), PERSON, false, "3.12"), None);
        assert_eq!(
            take_hint(&[], dir.path(), PERSON, false, "3.13"),
            Some(APP_HINT)
        );

        let managed = tempfile::tempdir().unwrap();
        let marker = app_presence::marker_path_in(managed.path());
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(
            &marker,
            r#"{"schema":"leanctx.app-presence/v1","app":"leanctx-code"}"#,
        )
        .unwrap();
        assert_eq!(take_hint(&[], managed.path(), PERSON, false, "3.12"), None);
        assert_eq!(read_last_shown(managed.path()), None);

        let quiet = tempfile::tempdir().unwrap();
        assert_eq!(
            take_hint(&args(&["--json"]), quiet.path(), PERSON, false, "3.12"),
            None
        );
        assert_eq!(take_hint(&[], quiet.path(), PERSON, true, "3.12"), None);
        assert_eq!(read_last_shown(quiet.path()), None);
    }

    #[test]
    fn the_funnel_codes_are_valid_wire_codes() {
        use lean_ctx::core::telemetry_features::{APP_CODE_DETECTED, APP_CODE_HINT_SHOWN};
        assert_eq!(APP_CODE_DETECTED, "app.code.detected");
        assert_eq!(APP_CODE_HINT_SHOWN, "app.code_hint.shown");
        for code in [APP_CODE_DETECTED, APP_CODE_HINT_SHOWN] {
            assert!(
                lean_ctx::core::telemetry_v2::valid_feature_code(code),
                "{code}"
            );
        }
    }

    #[test]
    fn a_marker_is_detected_only_with_the_v1_schema_and_a_safe_link() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(app_presence::detect_in(dir.path()), None);
        let marker = app_presence::marker_path_in(dir.path());
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        for wrong in [
            "not json",
            r#"{"schema":"leanctx.app-presence/v2","app":"leanctx-code"}"#,
            r#"{"schema":"leanctx.app-presence/v1","app":"other"}"#,
        ] {
            std::fs::write(&marker, wrong).unwrap();
            assert_eq!(app_presence::detect_in(dir.path()), None, "{wrong}");
        }
        std::fs::write(
            &marker,
            r#"{"schema":"leanctx.app-presence/v1","app":"leanctx-code","open_url":"leanctx://cockpit"}"#,
        )
        .unwrap();
        let presence = app_presence::detect_in(dir.path()).expect("managed");
        assert_eq!(presence.open_url.as_deref(), Some("leanctx://cockpit"));
        for unsafe_url in [
            "https://evil.invalid/",
            "javascript:alert(1)",
            "leanctx://x\"><script>",
            // `&` would start an HTML character reference inside `href`.
            "leanctx://cockpit?a=1&lt/",
            "leanctx://cockpit?a=1&b=2",
        ] {
            assert!(!app_presence::valid_open_url(unsafe_url), "{unsafe_url}");
        }
        assert!(app_presence::valid_open_url("leanctx://cockpit?view=a"));
        std::fs::write(
            &marker,
            r#"{"schema":"leanctx.app-presence/v1","app":"leanctx-code","open_url":"javascript:alert(1)"}"#,
        )
        .unwrap();
        assert_eq!(
            app_presence::detect_in(dir.path()),
            Some(app_presence::AppPresence { open_url: None })
        );
    }

    #[test]
    fn an_oversized_or_linked_marker_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let marker = app_presence::marker_path_in(dir.path());
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        let valid = r#"{"schema":"leanctx.app-presence/v1","app":"leanctx-code"}"#;
        let padded = format!("{valid}{}", " ".repeat(4096));
        std::fs::write(&marker, padded).unwrap();
        assert_eq!(app_presence::detect_in(dir.path()), None);

        #[cfg(unix)]
        {
            let elsewhere = dir.path().join("elsewhere.json");
            std::fs::write(&elsewhere, valid).unwrap();
            std::fs::remove_file(&marker).unwrap();
            std::os::unix::fs::symlink(&elsewhere, &marker).unwrap();
            assert_eq!(app_presence::detect_in(dir.path()), None);
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_shown_state_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        assert!(record_shown(dir.path(), "3.12"));
        let mode = std::fs::metadata(state_path(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(read_last_shown(dir.path()).as_deref(), Some("3.12"));

        let existing = tempfile::tempdir().unwrap();
        let app_dir = existing.path().join("app");
        std::fs::create_dir(&app_dir).unwrap();
        std::fs::set_permissions(&app_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(record_shown(existing.path(), "3.12"));
        let dir_mode = std::fs::metadata(&app_dir).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o777, 0o700);
    }

    #[test]
    fn the_api_answer_is_deterministic_and_carries_no_path() {
        let off = app_presence::status_json(None);
        let value: serde_json::Value = serde_json::from_str(&off).unwrap();
        assert_eq!(value["managed"], false);
        assert_eq!(value["schema"], "leanctx.app-presence/v1");
        assert_eq!(value["app"], "LeanCTX Code");
        assert!(value["open_url"].is_null());
        let presence = app_presence::AppPresence {
            open_url: Some("leanctx://cockpit".into()),
        };
        let on = app_presence::status_json(Some(&presence));
        assert_eq!(on, app_presence::status_json(Some(&presence)));
        let value: serde_json::Value = serde_json::from_str(&on).unwrap();
        assert_eq!(value["managed"], true);
        assert_eq!(value["open_url"], "leanctx://cockpit");
        assert!(!on.contains("/home") && !on.contains("/Users") && !on.contains("\\\\"));
    }
}
