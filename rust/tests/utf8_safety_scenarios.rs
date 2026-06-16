use std::collections::HashMap;

fn make_cyrillic_string(target_bytes: usize) -> String {
    let base = "план реализации — спецификация документа в ";
    let mut s = String::new();
    while s.len() < target_bytes + 200 {
        s.push_str(base);
    }
    s
}

fn make_mixed_multibyte(target_bytes: usize) -> String {
    let base = "日本語テスト → résumé → план → 中文 → emoji🎉 end ";
    let mut s = String::new();
    while s.len() < target_bytes + 200 {
        s.push_str(base);
    }
    s
}

fn make_emoji_heavy(target_bytes: usize) -> String {
    let base = "🎉🚀💡🔥✨🎯🧪📦🔧⚡";
    let mut s = String::new();
    while s.len() < target_bytes + 200 {
        s.push_str(base);
    }
    s
}

#[test]
fn hash_fast_cyrillic_no_panic() {
    let s = make_cyrillic_string(20_000);
    assert!(
        s.len() > 16 * 1024,
        "string must exceed hash_fast threshold"
    );
    let hash = lean_ctx::server::helpers::hash_fast(&s);
    assert!(!hash.is_empty());
}

#[test]
fn hash_fast_emoji_no_panic() {
    let s = make_emoji_heavy(20_000);
    assert!(s.len() > 16 * 1024);
    let hash = lean_ctx::server::helpers::hash_fast(&s);
    assert!(!hash.is_empty());
}

#[test]
fn hash_fast_mixed_multibyte_no_panic() {
    let s = make_mixed_multibyte(20_000);
    assert!(s.len() > 16 * 1024);
    let hash = lean_ctx::server::helpers::hash_fast(&s);
    assert!(!hash.is_empty());
}

#[test]
fn hash_fast_boundary_at_4096() {
    let mut s = String::new();
    while s.len() < 4094 {
        s.push('a');
    }
    s.push('в');
    assert_eq!(s.as_bytes()[4094], 0xd0);
    assert_eq!(s.as_bytes()[4095], 0xb2);
    while s.len() < 20_000 {
        s.push('x');
    }
    let hash = lean_ctx::server::helpers::hash_fast(&s);
    assert!(!hash.is_empty());
}

#[test]
fn hash_fast_boundary_at_4095() {
    let mut s = String::new();
    while s.len() < 4095 {
        s.push('a');
    }
    s.push('в');
    while s.len() < 20_000 {
        s.push('x');
    }
    let hash = lean_ctx::server::helpers::hash_fast(&s);
    assert!(!hash.is_empty());
}

#[test]
fn hash_fast_consistent() {
    let s = make_cyrillic_string(20_000);
    let h1 = lean_ctx::server::helpers::hash_fast(&s);
    let h2 = lean_ctx::server::helpers::hash_fast(&s);
    assert_eq!(h1, h2, "hash_fast must be deterministic");
}

#[test]
fn hash_fast_small_string_no_truncation() {
    let s = "маленькая строка";
    assert!(s.len() <= 16 * 1024);
    let hash = lean_ctx::server::helpers::hash_fast(s);
    assert!(!hash.is_empty());
}

#[test]
fn truncation_patterns_cyrillic_no_panic() {
    let cyrillic_line = "ошибка: не удалось выполнить команду — проверьте настройки сервера и повторите попытку позже, пожалуйста обратитесь к документации для получения дополнительной информации";
    assert!(cyrillic_line.len() > 80);

    let trunc_80 = &cyrillic_line[..cyrillic_line.floor_char_boundary(80)];
    assert!(trunc_80.len() <= 80);
    assert!(cyrillic_line.is_char_boundary(trunc_80.len()));

    let trunc_50 = &cyrillic_line[..cyrillic_line.floor_char_boundary(50)];
    assert!(trunc_50.len() <= 50);

    let trunc_47 = &cyrillic_line[..cyrillic_line.floor_char_boundary(47)];
    assert!(trunc_47.len() <= 47);
}

#[test]
fn truncation_patterns_cjk_no_panic() {
    let cjk_line = "日本語のテストデータです。このテキストは非常に長いため、切り捨てが必要になります。正しく処理されるかテストします。";
    assert!(cjk_line.len() > 50);

    let trunc = &cjk_line[..cjk_line.floor_char_boundary(50)];
    assert!(trunc.len() <= 50);
    assert!(cjk_line.is_char_boundary(trunc.len()));
}

#[test]
fn truncation_patterns_emoji_boundary() {
    let emoji_line = "test🎉🚀💡result";
    for boundary in [4, 5, 6, 7, 8] {
        let trunc = &emoji_line[..emoji_line.floor_char_boundary(boundary)];
        assert!(trunc.len() <= boundary);
        assert!(emoji_line.is_char_boundary(trunc.len()));
    }
}

#[test]
fn ceil_char_boundary_suffix_slicing() {
    let s = make_cyrillic_string(10_000);
    for offset in [4000, 4095, 4096, 4097, 5000] {
        let start = s.ceil_char_boundary(s.len().saturating_sub(offset));
        let suffix = &s[start..];
        assert!(!suffix.is_empty());
        assert!(s.is_char_boundary(start));
    }
}

#[test]
fn floor_char_boundary_stress_test() {
    let generators: Vec<fn(usize) -> String> =
        vec![make_cyrillic_string, make_mixed_multibyte, make_emoji_heavy];
    let boundaries = [32, 47, 50, 57, 77, 80, 117, 200, 4096, 50000];

    for r#gen in &generators {
        let s = r#gen(60_000);
        for &b in &boundaries {
            if b < s.len() {
                let end = s.floor_char_boundary(b);
                assert!(end <= b);
                assert!(s.is_char_boundary(end));
                let _slice = &s[..end];
            }
        }
    }
}

#[test]
fn redev1l_exact_scenario() {
    let mut s = String::new();
    s.push_str("8.8-plan.md 781L\n");
    s.push_str("# EYE-343 §8.8 Discovery-producer + BMS-camera-claim — план реализации\n");
    s.push_str("\n> Спецификация: [`docs/korobka-arch.md` §8.8]");
    while s.len() < 20_000 {
        s.push_str("\nДополнительная строка с кириллицей для тестирования буфера.");
    }
    let hash = lean_ctx::server::helpers::hash_fast(&s);
    assert!(
        !hash.is_empty(),
        "must not panic on ReDev1L's exact file pattern"
    );
}

#[test]
fn all_truncation_offsets_used_in_codebase() {
    let offsets: HashMap<usize, &str> = HashMap::from([
        (4096, "server/helpers.rs hash_fast prefix"),
        (200, "gotcha_tracker/detect.rs + patterns/curl.rs"),
        (80, "patterns/just.rs + tool_defs/mod.rs"),
        (50, "patterns/cargo.rs"),
        (47, "patterns/test.rs"),
        (57, "codebook.rs"),
        (77, "terse/mcp_compress.rs + ctx_edit.rs"),
        (117, "ctx_preload.rs"),
        (32, "stats/format.rs"),
        (50000, "dashboard/routes/context.rs"),
    ]);

    let test_string = make_mixed_multibyte(60_000);

    for (&offset, location) in &offsets {
        let end = test_string.floor_char_boundary(offset);
        assert!(
            end <= offset,
            "floor_char_boundary({offset}) returned {end} for {location}"
        );
        assert!(
            test_string.is_char_boundary(end),
            "non-char-boundary at {end} for {location}"
        );
        let _slice = &test_string[..end];
    }
}
