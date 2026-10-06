use super::*;

// Regression tests for #443: persisting config must never reset customized
// values nor leak project-local overrides into the global file.

fn tmp_config() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    (dir, path)
}

// The canonical persist path keeps every customized value and applies only
// the requested change.
#[test]
fn update_global_at_preserves_customized_and_persists_change() {
    let (_dir, path) = tmp_config();
    std::fs::write(
        &path,
        "max_ram_percent = 30\ncompression_level = \"standard\"\n",
    )
    .unwrap();

    let returned = Config::update_global_at(&path, |c| c.proxy_enabled = Some(true))
        .expect("update_global_at must succeed");
    assert_eq!(returned.proxy_enabled, Some(true));

    let reloaded: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        reloaded.max_ram_percent, 30,
        "customized value must survive"
    );
    assert_eq!(reloaded.compression_level, CompressionLevel::Standard);
    assert_eq!(reloaded.proxy_enabled, Some(true));
}

// load_global never folds in project-local overrides; it reads only the
// global file. update_global builds on this, so persists cannot leak.
#[test]
fn load_global_from_reads_only_the_given_file() {
    let (_dir, path) = tmp_config();
    std::fs::write(&path, "theme = \"global-theme\"\n").unwrap();
    let cfg = Config::load_global_from(&path);
    assert_eq!(cfg.theme, "global-theme");
}

// Root-cause marker: the OLD `load() (with merge_local) -> save()` pattern
// leaks a project-local override into the global file. This proves why
// persist paths must use load_global / update_global instead.
#[test]
fn merged_load_then_save_leaks_local_override_root_cause_marker() {
    let (_dir, path) = tmp_config();
    std::fs::write(&path, "theme = \"global-theme\"\n").unwrap();

    // Simulate `Config::load()`: global file + project-local override merged.
    let mut cfg = Config::load_global_from(&path);
    cfg.merge_local("theme = \"project-local\"\n", true);
    // OLD persist: write the merged struct back to the GLOBAL file.
    cfg.save_to(&path).unwrap();

    let reloaded: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        reloaded.theme, "project-local",
        "OLD load+merge_local+save leaks the project-local value into global (#443)"
    );
}

// Subticket 4 contract: refuse to touch an unparseable config; never clobber.
#[test]
fn update_global_at_refuses_unparseable_and_leaves_file_untouched() {
    let (_dir, path) = tmp_config();
    let corrupt = "max_ram_percent = = =\n";
    std::fs::write(&path, corrupt).unwrap();

    let result = Config::update_global_at(&path, |c| c.proxy_enabled = Some(true));
    assert!(
        result.is_err(),
        "must refuse to modify an unparseable config"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        corrupt,
        "the corrupt file must be left exactly as-is"
    );
}

// LR-TEL-01: the daily cloud background task used to mutate and save the whole
// `Config` it had read before its network work, so a telemetry opt-out
// committed during that window was merged away on save. The persist path now
// re-loads from disk under the config lock and applies only the stamps the run
// produced.
//
// Deterministic by construction: the interleaving is statement order, not
// concurrency — no threads, no sleeps, no environment mutation, temp paths only.
#[test]
fn cloud_background_delta_preserves_a_concurrent_telemetry_opt_out() {
    let (_dir, path) = tmp_config();
    std::fs::write(
        &path,
        "max_ram_percent = 30\n\
         [telemetry]\n\
         enabled = true\n\
         preference = \"explicitly_enabled\"\n\
         notice_shown = true\n",
    )
    .unwrap();

    // What the background task reads before it starts talking to the network.
    let stale = Config::load_global_from(&path);
    assert!(stale.telemetry.enabled, "snapshot starts opted in");

    // `lean-ctx telemetry off` commits while that network work is in flight.
    std::fs::write(
        &path,
        "max_ram_percent = 30\n\
         [telemetry]\n\
         enabled = false\n\
         preference = \"explicitly_disabled\"\n\
         notice_shown = true\n",
    )
    .unwrap();

    // The run persists only the stamps it produced — never the stale snapshot.
    let delta = crate::cloud_sync::CloudBackgroundDelta {
        last_heartbeat: Some("2026-09-21".to_string()),
        last_sync: Some("2026-09-21".to_string()),
        last_index_push: vec![("project-hash".to_string(), "2026-09-21".to_string())],
        ..Default::default()
    };
    // Drives the production persist step itself, not a test-only re-creation
    // of it: `cloud_background_tasks` reaches the same function.
    crate::cloud_sync::persist_background_delta_at(&path, &delta)
        .expect("persisting the delta must succeed");

    let reloaded: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        !reloaded.telemetry.enabled,
        "an opt-out committed during the network window must survive the save"
    );
    assert_eq!(
        reloaded.telemetry.preference,
        TelemetryPreference::ExplicitlyDisabled
    );
    assert_eq!(
        reloaded.telemetry.last_heartbeat.as_deref(),
        Some("2026-09-21"),
        "the heartbeat stamp must still be persisted"
    );
    assert_eq!(reloaded.cloud.last_sync.as_deref(), Some("2026-09-21"));
    assert_eq!(
        reloaded
            .cloud
            .last_index_push
            .get("project-hash")
            .map(String::as_str),
        Some("2026-09-21")
    );
    assert_eq!(
        reloaded.max_ram_percent, 30,
        "an unrelated configuration writer's value must be preserved"
    );
}

// Characterization of long-standing behaviour, NOT a regression for the
// LR-TEL-01 fix: `update_global_at` has always loaded its own base inside the
// call, so a `Config` the caller loaded earlier was never the save base. That
// was true before this change too — the race was never "the closure sees stale
// data", it was the unlocked window *between* that load and the save. The
// ordering proof for the actual fix is
// `a_second_update_cannot_load_until_the_first_closure_commits` below; this
// test only pins the base-selection behaviour so it cannot regress.
#[test]
fn update_global_at_loads_its_own_base_not_the_callers() {
    let (_dir, path) = tmp_config();
    std::fs::write(&path, "[telemetry]\nenabled = true\n").unwrap();

    let stale = Config::load_global_from(&path);
    assert!(stale.telemetry.enabled);

    // The file moves on underneath the holder of `stale`.
    std::fs::write(&path, "[telemetry]\nenabled = false\n").unwrap();

    let mut seen_enabled = None;
    Config::update_global_at(&path, |on_disk| {
        seen_enabled = Some(on_disk.telemetry.enabled);
        on_disk.cloud.last_sync = Some("2026-09-21".to_string());
    })
    .expect("update must succeed");

    assert_eq!(
        seen_enabled,
        Some(false),
        "the closure's base is loaded inside the call, so it reflects the file"
    );
    let reloaded: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(!reloaded.telemetry.enabled);
    assert_eq!(reloaded.cloud.last_sync.as_deref(), Some("2026-09-21"));
}

// The actual LR-TEL-01 proof: two concurrent updates are serialized, and the
// second one's *load* cannot happen until the first one's closure has
// committed. Before the fix, writer B could load while A was still between its
// own load and save, and then overwrite A's value.
//
// Synchronized, not timed: A parks inside its closure until the test releases
// it, so the interleaving is driven by channel sends rather than by sleeps.
// Nothing here asserts on duration or speed. The `recv_timeout` bounds exist
// only so a regression fails the test instead of hanging it forever.
#[test]
fn a_second_update_cannot_load_until_the_first_closure_commits() {
    use std::sync::mpsc;
    use std::time::Duration;

    // Generous ceiling: only ever hit if the lock is broken/deadlocked.
    let guard_against_hang = Duration::from_secs(10);

    let (_dir, path) = tmp_config();
    std::fs::write(&path, "max_ram_percent = 10\n").unwrap();

    let (a_entered_tx, a_entered_rx) = mpsc::channel::<()>();
    let (a_release_tx, a_release_rx) = mpsc::channel::<()>();
    let (b_base_tx, b_base_rx) = mpsc::channel::<u8>();

    std::thread::scope(|scope| {
        // Writer A: mutate, announce that it is inside the critical section,
        // then hold there until released. The save happens after the release.
        let path_a = &path;
        let writer_a = scope.spawn(move || {
            Config::update_global_at(path_a, |cfg| {
                cfg.max_ram_percent = 20;
                a_entered_tx.send(()).unwrap();
                a_release_rx.recv_timeout(guard_against_hang).unwrap();
            })
        });

        a_entered_rx
            .recv_timeout(guard_against_hang)
            .expect("writer A must reach its closure");

        // Deterministic exclusion check while A is parked. The former leaf-only
        // lock would be absent here, independently of how B is scheduled.
        let probe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_file_name("config.toml.v4-migration.lock"))
            .unwrap();
        let exclusion = fs2::FileExt::try_lock_exclusive(&probe);
        if exclusion.is_ok() {
            fs2::FileExt::unlock(&probe).unwrap();
            a_release_tx.send(()).unwrap();
            writer_a.join().unwrap().unwrap();
            panic!("config lock must cover the update closure, not only the write");
        }
        assert!(crate::core::file_lock::is_contended(
            &exclusion.unwrap_err()
        ));

        // Writer B starts while A is parked mid-critical-section.
        let writer_b = scope.spawn(|| {
            Config::update_global_at(&path, |cfg| {
                b_base_tx.send(cfg.max_ram_percent).unwrap();
                cfg.proxy_enabled = Some(true);
            })
        });

        a_release_tx.send(()).unwrap();
        writer_a.join().unwrap().expect("writer A must succeed");
        writer_b.join().unwrap().expect("writer B must succeed");
    });

    // The ordering proof: B's base already contained A's committed value, so
    // B's load provably happened after A's save.
    assert_eq!(
        b_base_rx.recv_timeout(guard_against_hang).unwrap(),
        20,
        "writer B must load a base that already contains writer A's commit"
    );

    // Neither update was lost.
    let reloaded: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(reloaded.max_ram_percent, 20, "writer A's value survived");
    assert_eq!(
        reloaded.proxy_enabled,
        Some(true),
        "writer B's value survived"
    );
}

// A cycle that produced nothing writes nothing: `apply` only ever touches the
// fields the delta actually carries.
#[test]
fn an_empty_cloud_background_delta_changes_no_field() {
    let delta = crate::cloud_sync::CloudBackgroundDelta::default();
    assert!(delta.is_empty(), "a run that produced no stamps is empty");

    let mut config: Config = toml::from_str(
        "[telemetry]\n\
         enabled = false\n\
         preference = \"explicitly_disabled\"\n\
         [cloud]\n\
         last_sync = \"2026-01-01\"\n",
    )
    .unwrap();

    delta.apply(&mut config);

    assert!(!config.telemetry.enabled);
    assert_eq!(
        config.telemetry.preference,
        TelemetryPreference::ExplicitlyDisabled
    );
    assert_eq!(config.cloud.last_sync.as_deref(), Some("2026-01-01"));
    assert!(config.telemetry.last_heartbeat.is_none());
}

#[test]
fn load_global_from_missing_or_empty_yields_defaults() {
    let (_dir, path) = tmp_config();
    // Missing file.
    let cfg = Config::load_global_from(&path);
    assert_eq!(cfg.max_ram_percent, Config::default().max_ram_percent);
    // Empty / whitespace-only file.
    std::fs::write(&path, "   \n").unwrap();
    let cfg2 = Config::load_global_from(&path);
    assert_eq!(cfg2.max_ram_percent, Config::default().max_ram_percent);
}
