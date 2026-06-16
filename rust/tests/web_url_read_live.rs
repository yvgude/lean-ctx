//! Live smoke tests for the web / research context layer (`core::web`).
//!
//! These hit the real network, so they are `#[ignore]` by default and run only
//! via `cargo test --test web_url_read_live -- --ignored --nocapture`. They are
//! the end-to-end proof that the `ctx_url_read` pipeline works against live
//! sources: fetch → SSRF guard → HTML→Markdown / transcript → distill →
//! citation footer.

use lean_ctx::core::web::{ReadMode, ReadOptions, read_url};

fn opts(url: &str, mode: ReadMode) -> ReadOptions<'_> {
    let mut o = ReadOptions::new(url);
    o.mode = mode;
    o
}

#[test]
#[ignore = "hits the live network"]
fn html_markdown_with_citation() {
    let o = opts("https://example.com/", ReadMode::Markdown);
    let r = read_url(&o).expect("example.com should fetch");
    println!("\n=== MARKDOWN ===\n{}\n", r.content);
    assert!(r.content.contains("Example Domain"), "missing page content");
    assert!(r.content.contains("Source:"), "missing citation footer");
    assert!(r.content.contains("Site: example.com"), "missing site line");
    assert!(r.original_tokens > 0, "no source tokens counted");
}

#[test]
#[ignore = "hits the live network"]
fn html_facts_mode_on_real_article() {
    let o = opts(
        "https://en.wikipedia.org/wiki/Rust_(programming_language)",
        ReadMode::Facts,
    );
    let r = read_url(&o).expect("wikipedia should fetch");
    println!(
        "\n=== FACTS ({} src tokens) ===\n{}\n",
        r.original_tokens,
        r.content.chars().take(900).collect::<String>()
    );
    assert_eq!(r.mode, ReadMode::Facts);
    assert!(r.content.contains("Source:"), "missing citation footer");
}

#[test]
#[ignore = "hits the live network"]
fn html_links_mode() {
    let o = opts("https://www.rust-lang.org/", ReadMode::Links);
    let r = read_url(&o).expect("rust-lang.org should fetch");
    println!(
        "\n=== LINKS ===\n{}\n",
        r.content.chars().take(600).collect::<String>()
    );
    assert_eq!(r.mode, ReadMode::Links);
    assert!(r.content.contains("https://"), "expected absolute links");
}

#[test]
#[ignore = "hits the live network"]
fn pdf_extraction_real() {
    let o = opts(
        "https://www.w3.org/WAI/ER/tests/xhtml/testfiles/resources/pdf/dummy.pdf",
        ReadMode::Markdown,
    );
    let r = read_url(&o).expect("pdf should fetch + extract");
    println!("\n=== PDF ===\n{}\n", r.content);
    assert!(
        r.content.to_lowercase().contains("dummy"),
        "expected extracted PDF text"
    );
    assert!(r.content.contains("Source:"), "missing citation footer");
    assert!(r.original_tokens > 0, "no source tokens");
}

#[test]
#[ignore = "hits the live network"]
fn youtube_transcript_real() {
    let o = opts(
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        ReadMode::Transcript,
    );
    let r = read_url(&o).expect("youtube transcript should fetch");
    println!(
        "\n=== TRANSCRIPT ({} src tokens) ===\n{}\n",
        r.original_tokens,
        r.content.chars().take(600).collect::<String>()
    );
    assert_eq!(r.mode, ReadMode::Transcript);
    assert!(r.content.contains("Source:"), "missing citation footer");
    assert!(r.original_tokens > 0, "empty transcript");
}
