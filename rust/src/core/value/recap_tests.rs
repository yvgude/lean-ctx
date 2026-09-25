use super::*;
use crate::core::value::snapshot::{SCHEMA, write_to};

fn cfg() -> ValueDisplayConfig {
    ValueDisplayConfig {
        mode: ValueDisplayMode::Minimal,
        ..ValueDisplayConfig::default()
    }
}

fn snap(session: &str, saved: u64) -> ValueSnapshot {
    ValueSnapshot {
        schema: SCHEMA,
        session_id: session.into(),
        started_at: Some(Utc::now() - chrono::Duration::hours(1)),
        updated_at: Some(Utc::now()),
        tokens_input: saved * 2,
        tokens_saved: saved,
        ..ValueSnapshot::default()
    }
}

fn secrets(n: u64) -> SecurityCounts {
    SecurityCounts {
        secrets_redacted: n,
        ..SecurityCounts::default()
    }
}

#[test]
fn recap_fires_every_n_turns_and_only_describes_its_window() {
    let cfg = cfg();
    let mut state = TurnState::start(Utc::now(), Some(&snap("s", 1_000_000)));
    for turn in 1..10 {
        let s = snap("s", 1_000_000 + turn * 40_000);
        assert_eq!(advance(&mut state, Some(&s), &cfg, false), None);
    }
    let recap = advance(&mut state, Some(&snap("s", 1_400_000)), &cfg, false).unwrap();
    assert_eq!(recap.turns, 10);
    assert_eq!(recap.saved, 400_000, "the pre-existing 1M is never claimed");

    // The next window starts at the recap, not at the session start.
    for _ in 0..9 {
        assert_eq!(
            advance(&mut state, Some(&snap("s", 1_500_000)), &cfg, false),
            None
        );
    }
    let recap = advance(&mut state, Some(&snap("s", 1_500_000)), &cfg, false).unwrap();
    assert_eq!(recap.saved, 100_000);
}

#[test]
fn a_quiet_window_keeps_growing_until_it_is_worth_mentioning() {
    let cfg = cfg();
    let mut state = TurnState::start(Utc::now(), Some(&snap("s", 0)));
    for _ in 0..12 {
        assert_eq!(
            advance(&mut state, Some(&snap("s", 10_000)), &cfg, false),
            None
        );
    }
    let recap = advance(&mut state, Some(&snap("s", 60_000)), &cfg, false).unwrap();
    assert_eq!(recap.turns, 13, "the recap names the real window length");
    assert_eq!(recap.saved, 60_000);
}

#[test]
fn a_security_event_is_always_worth_a_recap() {
    let cfg = cfg();
    let mut state = TurnState::start(Utc::now(), Some(&snap("s", 0)));
    for _ in 0..9 {
        advance(&mut state, Some(&snap("s", 0)), &cfg, false);
    }
    let mut s = snap("s", 0);
    s.security = secrets(2);
    let recap = advance(&mut state, Some(&s), &cfg, false).unwrap();
    assert_eq!(recap.saved, 0);
    assert_eq!(recap.security.secrets_redacted, 2);
    assert_eq!(
        turn_line(&recap, Style::PLAIN),
        "◆ lean-ctx · last 10 turns: 2 secrets kept out of context"
    );
}

#[test]
fn verbose_mentions_any_saving() {
    let cfg = cfg();
    let mut state = TurnState::start(Utc::now(), Some(&snap("s", 0)));
    for _ in 0..9 {
        advance(&mut state, Some(&snap("s", 1)), &cfg, true);
    }
    assert!(advance(&mut state, Some(&snap("s", 5)), &cfg, true).is_some());
}

#[test]
fn a_new_lean_session_is_rebased_by_when_it_began() {
    let cfg = cfg();
    let created = Utc::now() - chrono::Duration::minutes(30);
    let mut state = TurnState::start(created, None);

    // Began inside this host session: everything it saved belongs to it.
    let mut fresh = snap("new", 80_000);
    fresh.started_at = Some(created + chrono::Duration::minutes(1));
    for _ in 0..9 {
        advance(&mut state, Some(&fresh), &cfg, false);
    }
    assert_eq!(
        advance(&mut state, Some(&fresh), &cfg, false)
            .unwrap()
            .saved,
        80_000
    );

    // Carried over from before: only what it saves from now on counts.
    let mut state = TurnState::start(created, None);
    let old = snap("old", 5_000_000);
    for _ in 0..10 {
        assert_eq!(advance(&mut state, Some(&old), &cfg, false), None);
    }
}

#[test]
fn no_snapshot_counts_the_turn_but_says_nothing() {
    let mut state = TurnState::default();
    assert_eq!(advance(&mut state, None, &cfg(), false), None);
    assert_eq!(state.turns, 1);
}

#[test]
fn turn_state_ids_are_sanitized() {
    let dir = Path::new("/x");
    assert!(turn_state_path(dir, "abc-123_x.y").is_some());
    for bad in ["", "../etc", "a/b", ".hidden", &"a".repeat(129)] {
        assert!(turn_state_path(dir, bad).is_none(), "{bad}");
    }
}

#[test]
fn turn_end_persists_the_count_between_hook_processes() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = cfg();
    let now = Utc::now();
    let base = snap("s", 0);
    on_session_start(
        dir.path(),
        "host",
        "resume",
        Some(&base),
        &cfg,
        now,
        Style::PLAIN,
    );
    for _ in 0..9 {
        assert_eq!(
            on_turn_end(
                dir.path(),
                "host",
                Some(&snap("s", 30_000)),
                &cfg,
                now,
                Style::PLAIN
            ),
            None
        );
    }
    let line = on_turn_end(
        dir.path(),
        "host",
        Some(&snap("s", 312_000)),
        &cfg,
        now,
        Style::PLAIN,
    );
    assert_eq!(
        line.as_deref(),
        Some("◆ lean-ctx · last 10 turns: −312.0K tokens")
    );
    assert_eq!(load_turn_state(dir.path(), "host").unwrap().turns, 10);
}

#[test]
fn session_start_shows_the_weekly_digest_once() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = cfg();
    let now = Utc::now();
    let mut a = snap("a", 1_000_000);
    a.security = secrets(1);
    write_to(dir.path(), &a).unwrap();
    write_to(dir.path(), &snap("b", 400_000)).unwrap();
    let mut old = snap("old", 9_000_000);
    old.updated_at = Some(now - chrono::Duration::days(9));
    write_to(dir.path(), &old).unwrap();

    let line = on_session_start(
        dir.path(),
        "h1",
        "startup",
        Some(&a),
        &cfg,
        now,
        Style::PLAIN,
    );
    assert_eq!(
        line.as_deref(),
        Some("◆ lean-ctx · this week (2 sessions): −1.4M tokens · 1 secret kept out of context")
    );
    // Neither the digest nor the covered session is repeated.
    assert_eq!(
        on_session_start(
            dir.path(),
            "h2",
            "startup",
            Some(&a),
            &cfg,
            now,
            Style::PLAIN
        ),
        None
    );
}

#[test]
fn session_start_recaps_a_new_last_session_once() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = cfg();
    let now = Utc::now();
    let state = RecapState {
        last_session_recapped: String::new(),
        last_digest_at: Some(now),
    };
    write_json(&recap_state_path(dir.path()), &state);

    let mut s = snap("s", 1_400_000);
    s.tokens_input = 2_258_064;
    s.security = secrets(3);
    let line = on_session_start(
        dir.path(),
        "h1",
        "startup",
        Some(&s),
        &cfg,
        now,
        Style::PLAIN,
    );
    assert_eq!(
        line.as_deref(),
        Some(
            "◆ lean-ctx · last session (62% of tool input): −1.4M tokens · 3 secrets kept out of context"
        )
    );
    assert_eq!(
        on_session_start(
            dir.path(),
            "h2",
            "startup",
            Some(&s),
            &cfg,
            now,
            Style::PLAIN
        ),
        None
    );
}

#[test]
fn session_start_is_silent_on_resume_small_sessions_and_stale_ones() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = cfg();
    let now = Utc::now();
    write_json(
        &recap_state_path(dir.path()),
        &RecapState {
            last_session_recapped: String::new(),
            last_digest_at: Some(now),
        },
    );
    let big = snap("s", 1_000_000);
    for source in ["resume", "compact", "clear"] {
        assert_eq!(
            on_session_start(dir.path(), "h", source, Some(&big), &cfg, now, Style::PLAIN),
            None
        );
    }
    let small = snap("small", 1_000);
    assert_eq!(
        on_session_start(
            dir.path(),
            "h",
            "startup",
            Some(&small),
            &cfg,
            now,
            Style::PLAIN
        ),
        None
    );
    let mut stale = snap("stale", 1_000_000);
    stale.updated_at = Some(now - chrono::Duration::days(8));
    assert_eq!(
        on_session_start(
            dir.path(),
            "h",
            "startup",
            Some(&stale),
            &cfg,
            now,
            Style::PLAIN
        ),
        None
    );
}

#[test]
fn session_start_baselines_the_host_session() {
    let dir = tempfile::tempdir().unwrap();
    let s = snap("s", 700_000);
    on_session_start(
        dir.path(),
        "host",
        "startup",
        Some(&s),
        &cfg(),
        Utc::now(),
        Style::PLAIN,
    );
    let state = load_turn_state(dir.path(), "host").unwrap();
    assert_eq!(state.base_saved, 700_000);
    assert_eq!(state.turns, 0);
}

#[test]
fn ascii_style_has_no_glyphs() {
    let recap = TurnRecap {
        turns: 1,
        saved: 60_000,
        security: SecurityCounts::default(),
    };
    let style = Style {
        color: false,
        unicode: false,
    };
    assert_eq!(
        turn_line(&recap, style),
        "* lean-ctx | last turn: -60.0K tokens"
    );
}
