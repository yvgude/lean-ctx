//! Tests for the Wrapped share-card backend (payload validation, leaderboard,
//! ranking, permalink rendering, rate limiting).

#[allow(clippy::wildcard_imports)]
use super::*;

fn valid() -> PublishPayload {
    PublishPayload {
        period: "week".into(),
        tokens_saved: 480_600_000,
        cost_avoided_usd: 1441.79,
        pricing_estimated: true,
        compression_rate_pct: 91.2,
        total_commands: 1234,
        sessions_count: 56,
        files_touched: 789,
        top_commands: vec![TopCommand {
            name: "ctx_search".into(),
            pct: 60.0,
        }],
        model_key: Some("claude-opus".into()),
        display_name: Some("yvesg".into()),
        leaderboard_opt_in: false,
    }
}

#[test]
fn accepts_a_well_formed_payload() {
    assert!(valid().validate().is_ok());
}

fn raw_card(id: &str, user: Option<&str>, tokens: i64, name: &str, rate: f64) -> RawLeaderCard {
    let payload = serde_json::json!({
        "period": "all",
        "tokens_saved": tokens,
        "cost_avoided_usd": tokens as f64 / 1000.0,
        "pricing_estimated": false,
        "compression_rate_pct": rate,
        "display_name": name,
    })
    .to_string();
    RawLeaderCard {
        id: id.to_string(),
        payload_json: payload,
        user_id: user.map(str::to_string),
        link_group: None,
    }
}

fn raw_card_linked(
    id: &str,
    user: Option<&str>,
    tokens: i64,
    name: &str,
    rate: f64,
    group: &str,
) -> RawLeaderCard {
    RawLeaderCard {
        link_group: Some(group.to_string()),
        ..raw_card(id, user, tokens, name, rate)
    }
}

#[test]
fn machines_claimed_to_one_account_stack() {
    // Two machines, same account → one stacked entry (the #488 fix).
    let raw = vec![
        raw_card("cardA", Some("user-1"), 1_000, "Stephen", 80.0),
        raw_card("cardB", Some("user-1"), 3_000, "Stephen", 90.0),
    ];
    let out = aggregate_by_account(raw, "https://leanctx.com");
    assert_eq!(
        out.len(),
        1,
        "two machines on one account collapse to one row"
    );
    assert_eq!(out[0].tokens_saved, 4_000, "points stack across machines");
    assert!(
        out[0].url.ends_with("/w/cardB"),
        "the highest-saving machine represents the account"
    );
    // Token-weighted rate: (1000*80 + 3000*90) / 4000 = 87.5
    assert!((out[0].compression_rate_pct - 87.5).abs() < 1e-9);
}

#[test]
fn distinct_accounts_and_unclaimed_cards_stay_separate() {
    let raw = vec![
        raw_card("a", Some("user-1"), 1_000, "A", 80.0),
        raw_card("b", Some("user-2"), 2_000, "B", 80.0),
        raw_card("c", None, 1_500, "C", 80.0), // unclaimed, stays individual
    ];
    let out = aggregate_by_account(raw, "https://x");
    assert_eq!(out.len(), 3);
    // Ordered by stacked tokens, descending.
    assert_eq!(out[0].id, "b");
    assert_eq!(out[1].id, "c");
    assert_eq!(out[2].id, "a");
}

#[test]
fn aggregation_order_is_deterministic_on_ties() {
    let raw = vec![
        raw_card("zzz", Some("u1"), 1_000, "Z", 50.0),
        raw_card("aaa", Some("u2"), 1_000, "A", 50.0),
    ];
    let out = aggregate_by_account(raw, "https://x");
    assert_eq!(
        out[0].id, "aaa",
        "equal totals are tie-broken by id, stably"
    );
    assert_eq!(out[1].id, "zzz");
}

#[test]
fn single_machine_is_unchanged_by_aggregation() {
    let raw = vec![raw_card("solo", None, 1_234, "Solo", 73.0)];
    let out = aggregate_by_account(raw, "https://leanctx.com");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].tokens_saved, 1_234);
    assert_eq!(out[0].display_name.as_deref(), Some("Solo"));
    assert!((out[0].compression_rate_pct - 73.0).abs() < 1e-9);
}

#[test]
fn link_group_stacks_machines_without_account() {
    // The #736 fix: two machines paired via `gain --link` — no user_id at all.
    let raw = vec![
        raw_card_linked("m1", None, 32_900_000, "Stephen.S", 80.0, "g1"),
        raw_card_linked("m2", None, 2_100_000, "Stephen.S", 70.0, "g1"),
        raw_card("other", None, 5_000_000, "Other", 60.0),
    ];
    let out = aggregate_by_account(raw, "https://x");
    assert_eq!(out.len(), 2, "linked machines collapse, others stay");
    assert_eq!(out[0].tokens_saved, 35_000_000);
    assert!(
        out[0].url.ends_with("/w/m1"),
        "highest-saving machine represents the group"
    );
    assert_eq!(out[1].id, "other");
}

#[test]
fn link_group_and_account_claim_merge_transitively() {
    // A+B share a link_group; B+C share a user_id → all three are one entry.
    let raw = vec![
        raw_card_linked("a", None, 1_000, "N", 50.0, "g"),
        raw_card_linked("b", Some("u1"), 2_000, "N", 50.0, "g"),
        raw_card("c", Some("u1"), 4_000, "N", 50.0),
    ];
    let out = aggregate_by_account(raw, "https://x");
    assert_eq!(out.len(), 1, "link_group and user_id chains merge");
    assert_eq!(out[0].tokens_saved, 7_000);
    assert!(out[0].url.ends_with("/w/c"));
}

#[test]
fn distinct_link_groups_stay_separate() {
    let raw = vec![
        raw_card_linked("a", None, 1_000, "A", 50.0, "g1"),
        raw_card_linked("b", None, 2_000, "B", 50.0, "g2"),
    ];
    let out = aggregate_by_account(raw, "https://x");
    assert_eq!(out.len(), 2, "different groups never merge");
}

#[test]
fn link_code_format_is_canonical_and_unambiguous() {
    for _ in 0..64 {
        let code = generate_link_code();
        assert_eq!(code.len(), 9, "XXXX-XXXX");
        assert_eq!(&code[4..5], "-");
        assert!(
            code.chars()
                .filter(|c| *c != '-')
                .all(|c| "ABCDEFGHJKMNPQRSTUVWXYZ23456789".contains(c)),
            "no ambiguous characters (0/O, 1/I/L): {code}"
        );
    }
    // Normalization: user may paste with or without the dash, any case.
    assert_eq!(format_link_code("KQ3F8ZTN"), "KQ3F-8ZTN");
    assert_eq!(format_link_code("KQ3F-8ZTN"), "KQ3F-8ZTN");
}

#[test]
fn signed_envelope_roundtrips_and_rejects_tampering() {
    use crate::core::agent_identity::hex_encode;
    use ed25519_dalek::{Signer, SigningKey};

    let key = SigningKey::from_bytes(&[7u8; 32]);
    let payload_json = serde_json::to_string(&valid()).unwrap();
    let pubkey_hex = hex_encode(&key.verifying_key().to_bytes());
    let sig_hex = hex_encode(&key.sign(payload_json.as_bytes()).to_bytes());

    // A valid signature parses the payload and yields a stable, fixed-length publisher id.
    let env = SignedEnvelope {
        payload_json: payload_json.clone(),
        public_key: Some(pubkey_hex.clone()),
        signature: Some(sig_hex.clone()),
    };
    let (parsed, publisher_id) = verify_signed_envelope(&env).expect("valid signature");
    assert_eq!(parsed.period, "week");
    assert_eq!(publisher_id.len(), PUBLISHER_ID_HEX_LEN);

    // The same key always maps to the same publisher id — this is the upsert key.
    let again = SignedEnvelope {
        payload_json: payload_json.clone(),
        public_key: Some(pubkey_hex.clone()),
        signature: Some(sig_hex.clone()),
    };
    assert_eq!(verify_signed_envelope(&again).unwrap().1, publisher_id);

    // Tampering with the payload after signing is rejected (signature no longer matches).
    let tampered = SignedEnvelope {
        payload_json: payload_json.replacen("480600000", "999999999", 1),
        public_key: Some(pubkey_hex.clone()),
        signature: Some(sig_hex),
    };
    assert!(verify_signed_envelope(&tampered).is_err());

    // A missing signature cannot slip through the signed path into an unauthenticated upsert.
    let unsigned = SignedEnvelope {
        payload_json,
        public_key: Some(pubkey_hex),
        signature: None,
    };
    assert!(verify_signed_envelope(&unsigned).is_err());
}

#[test]
fn rejects_unknown_fields() {
    let json = r#"{"period":"week","tokens_saved":1,"cost_avoided_usd":0.1,
        "pricing_estimated":false,"compression_rate_pct":50,"total_commands":1,
        "sessions_count":1,"files_touched":1,"repo_path":"/secret/path"}"#;
    assert!(serde_json::from_str::<PublishPayload>(json).is_err());
}

#[test]
fn rejects_bad_period_and_ranges() {
    let mut p = valid();
    p.period = "year".into();
    assert!(p.validate().is_err());

    let mut p = valid();
    p.compression_rate_pct = 150.0;
    assert!(p.validate().is_err());

    let mut p = valid();
    p.tokens_saved = -1;
    assert!(p.validate().is_err());

    let mut p = valid();
    p.cost_avoided_usd = f64::NAN;
    assert!(p.validate().is_err());
}

#[test]
fn rejects_oversized_and_markup_text() {
    let mut p = valid();
    p.display_name = Some("a".repeat(MAX_LABEL_LEN + 1));
    assert!(p.validate().is_err());

    let mut p = valid();
    p.display_name = Some("<script>".into());
    assert!(p.validate().is_err());

    let mut p = valid();
    p.top_commands = (0..=MAX_TOP_COMMANDS)
        .map(|_| TopCommand {
            name: "git".into(),
            pct: 1.0,
        })
        .collect();
    assert!(p.validate().is_err());
}

#[test]
fn png_rasterizes_to_a_valid_image() {
    let svg = valid().to_report().to_svg();
    let png = svg_to_png(&svg).expect("rasterize");
    assert!(
        png.len() > 5000,
        "expected a non-trivial PNG, got {} bytes",
        png.len()
    );
    assert_eq!(&png[1..4], b"PNG", "must have a PNG signature");
    // Written for manual/visual inspection of text rendering during development.
    let _ = std::fs::write("/tmp/lc_card_test.png", &png);
}

#[test]
fn permalink_html_carries_per_card_og_meta() {
    let p = valid();
    let html = render_permalink_html(
        "abc123",
        &p,
        "https://leanctx.com",
        "https://api.leanctx.com",
    );
    assert!(html.contains(
        r#"property="og:image" content="https://api.leanctx.com/api/wrapped/abc123/card.png""#
    ));
    assert!(html.contains(r#"property="og:url" content="https://leanctx.com/w/abc123""#));
    assert!(html.contains("twitter:card"));
    assert!(
        html.contains("yvesg's lean-ctx Wrapped"),
        "display_name personalizes the title"
    );
    assert!(html.contains("<svg"), "card is embedded inline");
}

#[test]
fn leaderboard_html_uses_site_theme_shell() {
    let rows = vec![
        LeaderRow {
            rank: 1,
            id: "a".into(),
            url: "https://leanctx.com/w/a".into(),
            display_name: Some("yvesg".into()),
            tokens_saved: 486_000_000,
            cost_avoided_usd: 1458.0,
            compression_rate_pct: 67.7,
            period: "all".into(),
            pricing_estimated: true,
            flagged: false,
        },
        LeaderRow {
            rank: 2,
            id: "b".into(),
            url: "https://leanctx.com/w/b".into(),
            display_name: None,
            tokens_saved: 12_800_000,
            cost_avoided_usd: 32.0,
            compression_rate_pct: 60.2,
            period: "month".into(),
            pricing_estimated: false,
            flagged: false,
        },
        LeaderRow {
            rank: 3,
            id: "c".into(),
            url: "https://leanctx.com/w/c".into(),
            display_name: Some("roland".into()),
            tokens_saved: 4_200_000,
            cost_avoided_usd: 11.0,
            compression_rate_pct: 55.0,
            period: "week".into(),
            pricing_estimated: false,
            flagged: false,
        },
    ];
    let board = paginate(rows, &LeaderboardQuery::default());
    let html = render_leaderboard_html(&board, None, "https://leanctx.com");
    // Brand shell + design tokens mirrored from the marketing site.
    assert!(
        html.contains("--accent:#34d399"),
        "carries the site accent token"
    );
    assert!(html.contains("Space Grotesk"), "loads the display font");
    assert!(html.contains("lc-logo-ctx"), "renders the LeanCTX wordmark");
    assert!(
        html.contains(r#"class="lc-row lc-rank-1""#),
        "top row is highlighted"
    );
    assert!(html.contains("lc-footer"), "carries the branded footer");
    assert!(html.contains("yvesg"), "shows opted-in display names");
    assert!(
        html.contains("Self-reported savings"),
        "hero label is honest about provenance"
    );
    assert!(
        !html.contains("Verified savings"),
        "must not imply server-side verification"
    );
    // Written for manual/visual comparison with leanctx.com during development.
    let _ = std::fs::write("/tmp/lc_leaderboard.html", &html);
}

#[test]
fn stats_implausible_thresholds() {
    // Real high-volume usage at organic rates is never flagged.
    assert!(!stats_implausible(5_000_000_000, 71.0));
    assert!(!stats_implausible(9_900_000_000, 90.0));
    // A near-100% rate over a tiny sample is an ordinary small-sample artefact.
    assert!(!stats_implausible(10_000, 100.0));
    // High rate AND high volume together is implausible (the observed #1 anomaly).
    assert!(stats_implausible(9_900_000_000, 100.0));
    // Both thresholds are inclusive at the boundary.
    assert!(stats_implausible(
        IMPLAUSIBLE_MIN_TOKENS,
        IMPLAUSIBLE_RATE_PCT
    ));
    assert!(!stats_implausible(IMPLAUSIBLE_MIN_TOKENS - 1, 100.0));
    assert!(!stats_implausible(
        IMPLAUSIBLE_MIN_TOKENS,
        IMPLAUSIBLE_RATE_PCT - 0.1
    ));
}

#[test]
fn rank_and_demote_flagged_sinks_flagged_and_reranks() {
    let row = |id: &str, tokens: i64, flagged: bool| LeaderRow {
        rank: 0,
        id: id.into(),
        url: format!("https://leanctx.com/w/{id}"),
        display_name: None,
        tokens_saved: tokens,
        cost_avoided_usd: 0.0,
        compression_rate_pct: 0.0,
        period: "all".into(),
        pricing_estimated: false,
        flagged,
    };
    // Incoming order is `tokens_saved DESC` (as the SQL returns it); the flagged top card must
    // sink below the plausible ones while the plausible relative order is preserved.
    let mut rows = vec![
        row("fake", 9_900_000_000, true),
        row("real1", 5_000_000_000, false),
        row("real2", 600_000_000, false),
    ];
    rank_and_demote_flagged(&mut rows);
    assert_eq!(
        rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec!["real1", "real2", "fake"]
    );
    assert_eq!(
        rows.iter().map(|r| r.rank).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

/// N ranked rows (rank i+1, tokens descending, names `user{i}`) for pagination tests.
fn ranked_rows(n: usize) -> Vec<LeaderRow> {
    (0..n)
        .map(|i| LeaderRow {
            rank: i + 1,
            id: format!("id{i}"),
            url: format!("https://leanctx.com/w/id{i}"),
            display_name: Some(format!("user{i}")),
            tokens_saved: ((n - i) as i64) * 1_000,
            cost_avoided_usd: ((n - i) as f64) * 0.5,
            compression_rate_pct: 60.0,
            period: "all".into(),
            pricing_estimated: false,
            flagged: false,
        })
        .collect()
}

#[test]
fn paginate_slices_pages_and_preserves_global_ranks() {
    let q = |page: i64, per: i64| LeaderboardQuery {
        page: Some(page),
        per_page: Some(per),
        q: None,
    };
    // 120 entries, 50 per page → 3 pages (50 / 50 / 20).
    let p1 = paginate(ranked_rows(120), &q(1, 50));
    assert_eq!(p1.total_entries, 120);
    assert_eq!(p1.total_pages, 3);
    assert_eq!(p1.page, 1);
    assert_eq!(p1.entries.len(), 50);
    assert_eq!(p1.entries.first().unwrap().rank, 1);

    // Page 2 keeps GLOBAL ranks: its first row is rank 51, not 1.
    let p2 = paginate(ranked_rows(120), &q(2, 50));
    assert_eq!(p2.entries.len(), 50);
    assert_eq!(p2.entries.first().unwrap().rank, 51);

    let p3 = paginate(ranked_rows(120), &q(3, 50));
    assert_eq!(p3.entries.len(), 20);
    assert_eq!(p3.entries.last().unwrap().rank, 120);
}

#[test]
fn paginate_clamps_out_of_range_inputs() {
    // per_page above the max is clamped; a page past the end lands on the last page.
    let over = paginate(
        ranked_rows(30),
        &LeaderboardQuery {
            page: Some(999),
            per_page: Some(10_000),
            q: None,
        },
    );
    assert_eq!(over.per_page, LEADERBOARD_PER_PAGE_MAX);
    assert_eq!(over.total_pages, 1); // 30 entries at per_page 100 → 1 page
    assert_eq!(over.page, 1);
    // page 0 / negative clamps up to 1.
    let under = paginate(
        ranked_rows(30),
        &LeaderboardQuery {
            page: Some(0),
            per_page: Some(10),
            q: None,
        },
    );
    assert_eq!(under.page, 1);
    assert_eq!(under.per_page, 10);
}

#[test]
fn paginate_total_tokens_is_uncapped_and_page_independent() {
    let expected: i64 = ranked_rows(120).iter().map(|r| r.tokens_saved).sum();
    let expected_usd: f64 = ranked_rows(120).iter().map(|r| r.cost_avoided_usd).sum();
    // The community totals sum ALL entries, not just the returned page…
    let p1 = paginate(ranked_rows(120), &LeaderboardQuery::default());
    assert_eq!(p1.total_tokens_saved, expected);
    assert!((p1.total_cost_avoided_usd - expected_usd).abs() < 1e-6);
    // …and are identical on a later page even though the entries differ.
    let p2 = paginate(
        ranked_rows(120),
        &LeaderboardQuery {
            page: Some(2),
            per_page: Some(50),
            q: None,
        },
    );
    assert_eq!(p2.total_tokens_saved, expected);
    assert!((p2.total_cost_avoided_usd - expected_usd).abs() < 1e-6);
}

#[test]
fn paginate_q_filters_by_name_but_keeps_global_rank_and_total() {
    let total: i64 = ranked_rows(30).iter().map(|r| r.tokens_saved).sum();
    // "user1" is a substring of user1 and user10..user19 → 11 rows.
    let res = paginate(
        ranked_rows(30),
        &LeaderboardQuery {
            page: Some(1),
            per_page: Some(50),
            q: Some("user1".into()),
        },
    );
    assert_eq!(res.total_entries, 11);
    assert!(
        res.entries
            .iter()
            .all(|e| e.display_name.as_deref().unwrap().contains("user1"))
    );
    // user1 has global rank 2 (rank 1 is user0); filtering must not renumber.
    assert!(
        res.entries
            .iter()
            .any(|e| e.rank == 2 && e.display_name.as_deref() == Some("user1"))
    );
    // total_tokens_saved still spans the full board, ignoring the filter.
    assert_eq!(res.total_tokens_saved, total);
}

#[test]
fn paginate_empty_board_is_one_page() {
    let res = paginate(Vec::new(), &LeaderboardQuery::default());
    assert_eq!(res.total_entries, 0);
    assert_eq!(res.total_pages, 1);
    assert_eq!(res.page, 1);
    assert!(res.entries.is_empty());
    assert_eq!(res.total_tokens_saved, 0);
}

#[test]
fn render_paginated_board_shows_search_and_pagination() {
    let board = paginate(
        ranked_rows(120),
        &LeaderboardQuery {
            page: Some(2),
            per_page: Some(50),
            q: None,
        },
    );
    let html = render_leaderboard_html(&board, None, "https://leanctx.com");
    assert!(
        html.contains(r#"class="lc-search""#),
        "search box is always present"
    );
    assert!(
        html.contains(r#"class="lc-pagination""#),
        "a multi-page board shows pagination"
    );
    assert!(html.contains("?page=1"), "prev links to page 1");
    assert!(html.contains("?page=3"), "next links to page 3");
    assert!(html.contains("Page 2 / 3"), "shows the current position");
}

#[test]
fn flagged_card_renders_unverified_badge_and_no_gold() {
    let rows = vec![LeaderRow {
        rank: 1,
        id: "x".into(),
        url: "https://leanctx.com/w/x".into(),
        display_name: Some("suspicious".into()),
        tokens_saved: 9_900_000_000,
        cost_avoided_usd: 24_763.0,
        compression_rate_pct: 100.0,
        period: "all".into(),
        pricing_estimated: true,
        flagged: true,
    }];
    let board = paginate(rows, &LeaderboardQuery::default());
    let html = render_leaderboard_html(&board, None, "https://leanctx.com");
    assert!(
        html.contains("lc-flagged"),
        "flagged row carries the muted style"
    );
    assert!(
        html.contains(">unverified<"),
        "flagged row shows the unverified badge"
    );
    assert!(
        !html.contains("lc-row lc-rank-1"),
        "a flagged card never gets the top-rank highlight"
    );
}

#[test]
fn html_escape_neutralizes_markup() {
    assert_eq!(
        html_escape(r#"<b>&"x"</b>"#),
        "&lt;b&gt;&amp;&quot;x&quot;&lt;/b&gt;"
    );
}

#[test]
fn ip_hash_is_salted_and_omitted_without_headers() {
    let mut h = HeaderMap::new();
    // Salts are derived at runtime (not string literals) so this stays a
    // behavioral test and carries no hard-coded cryptographic value.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let salt_a = format!("salt-{nonce}-a");
    let salt_b = format!("salt-{nonce}-b");
    assert!(client_ip_hash(&h, &salt_a).is_none());

    h.insert("x-forwarded-for", "203.0.113.7, 10.0.0.1".parse().unwrap());
    let a = client_ip_hash(&h, &salt_a).unwrap();
    let b = client_ip_hash(&h, &salt_b).unwrap();
    assert_ne!(a, b, "different salts must yield different hashes");
    assert!(!a.contains("203.0.113.7"), "raw IP must never appear");
}
