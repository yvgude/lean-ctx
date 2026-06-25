//! Opt-in, self-hostable Wrapped share page.
//!
//! Produces a single standalone HTML file with the Wrapped SVG embedded inline (renders
//! offline, anywhere) plus Open Graph / Twitter card meta so that *when hosted* the link
//! unfurls into a rich preview. Zero network calls, zero telemetry — the user explicitly
//! runs `gain --share` and chooses where to host it (their site / GH Pages / a gist),
//! which is the permalink. Nothing is uploaded by lean-ctx.
//!
//! Social networks (Twitter/X) do not render SVG `og:image`, so the image meta points at
//! a sibling `lean-ctx-wrapped.png` under the supplied `--base-url`; we never fabricate a
//! URL — image meta is emitted only when a base URL is provided.

use crate::core::wrapped::{WrappedReport, format_tokens};

impl WrappedReport {
    /// Renders the self-contained share page. `base_url` (optional) is the location the
    /// user will host the page at; when present, absolute OG/Twitter image meta is emitted.
    #[must_use]
    pub fn to_share_html(&self, base_url: Option<&str>) -> String {
        let title = "lean-ctx Wrapped";
        let period_label = match self.period.as_str() {
            "week" => "this week",
            "month" => "this month",
            _ => "with lean-ctx",
        };
        let desc = format!(
            "I saved {} tokens (~${:.2}) {period_label}. Your AI saw only what mattered.",
            format_tokens(self.tokens_saved),
            self.cost_avoided_usd,
        );
        let svg = self.to_svg();
        let meta = social_meta(title, &desc, base_url);

        format!(
            r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8"/>
  <meta name="viewport" content="width=device-width, initial-scale=1"/>
  <title>{title}</title>
  <meta name="description" content="{desc_attr}"/>
  <style>body{{margin:0;min-height:100vh;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:22px;background:#0b1020;font-family:Inter,system-ui,-apple-system,Segoe UI,Roboto,sans-serif}}.card{{width:min(1200px,94vw)}}.card svg{{width:100%;height:auto;display:block;border-radius:12px}}a.cta{{color:#34d399;text-decoration:none;font-size:18px;font-weight:600}}</style>
{meta}</head>
<body>
  <div class="card">
{svg}
  </div>
  <a class="cta" href="https://leanctx.com">Get lean-ctx — your AI saw only what mattered &rarr;</a>
</body>
</html>
"#,
            desc_attr = escape(&desc),
        )
    }

    /// A ready-to-post one-liner for `gain --copy`. The opt-in permalink `url`
    /// (once published) is appended when present. Honest about the estimate marker.
    #[must_use]
    pub fn share_text(&self, url: Option<&str>) -> String {
        let period_label = match self.period.as_str() {
            "week" => " this week",
            "month" => " this month",
            _ => "",
        };
        let est = if self.pricing_estimated {
            " (est.)"
        } else {
            ""
        };
        let mut s = format!(
            "I saved {} tokens (~${:.2}{est}){period_label} with lean-ctx — my AI saw only what mattered.",
            format_tokens(self.tokens_saved),
            self.cost_avoided_usd,
        );
        if let Some(u) = url {
            s.push(' ');
            s.push_str(u);
        }
        s
    }
}

/// Builds the Open Graph / Twitter meta block. Image meta only when a base URL is given.
fn social_meta(title: &str, desc: &str, base_url: Option<&str>) -> String {
    let mut m = String::new();
    m.push_str(&tag_prop("og:title", title));
    m.push_str(&tag_prop("og:description", desc));
    m.push_str("  <meta property=\"og:type\" content=\"website\"/>\n");
    m.push_str(&tag_name("twitter:title", title));
    m.push_str(&tag_name("twitter:description", desc));

    if let Some(base) = base_url {
        let base = base.trim_end_matches('/');
        let image = format!("{base}/lean-ctx-wrapped.png");
        m.push_str(&tag_prop("og:url", base));
        m.push_str(&tag_prop("og:image", &image));
        m.push_str("  <meta name=\"twitter:card\" content=\"summary_large_image\"/>\n");
        m.push_str(&tag_name("twitter:image", &image));
    } else {
        m.push_str("  <meta name=\"twitter:card\" content=\"summary\"/>\n");
    }
    m
}

fn tag_prop(property: &str, content: &str) -> String {
    format!(
        "  <meta property=\"{property}\" content=\"{}\"/>\n",
        escape(content)
    )
}

fn tag_name(name: &str, content: &str) -> String {
    format!(
        "  <meta name=\"{name}\" content=\"{}\"/>\n",
        escape(content)
    )
}

/// HTML/attribute escaping for data-derived strings.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use crate::core::wrapped::WrappedReport;

    fn sample() -> WrappedReport {
        WrappedReport {
            period: "all".into(),
            tokens_saved: 348_300_000,
            tokens_input: 580_000_000,
            cost_avoided_usd: 870.81,
            total_commands: 17_055,
            sessions_count: 67,
            top_commands: vec![("ctx_search".into(), 100, 60.0)],
            compression_rate_pct: 60.2,
            files_touched: 1_234,
            daily_savings: vec![10, 50, 30, 80, 20, 40, 60],
            bounce_tokens: 0,
            model_key: "claude-3.5-sonnet".into(),
            pricing_estimated: false,
            percentile: Some(99),
        }
    }

    #[test]
    fn page_is_self_contained_and_branded() {
        let html = sample().to_share_html(None);
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<svg"), "SVG must be embedded inline");
        assert!(html.contains("</html>"));
        assert!(
            html.contains("leanctx.com"),
            "viral CTA must link the brand"
        );
        assert!(html.contains("348.3M"), "must show the headline metric");
    }

    #[test]
    fn without_base_url_no_image_meta() {
        let html = sample().to_share_html(None);
        assert!(
            !html.contains("og:image"),
            "must not fabricate an image URL without a base"
        );
        assert!(html.contains("name=\"twitter:card\" content=\"summary\""));
    }

    #[test]
    fn with_base_url_emits_absolute_image_meta() {
        let html = sample().to_share_html(Some("https://me.dev/wrapped/"));
        assert!(
            html.contains("og:image\" content=\"https://me.dev/wrapped/lean-ctx-wrapped.png\"")
        );
        assert!(html.contains("summary_large_image"));
        // Trailing slash on the base must be normalised (no double slash).
        assert!(!html.contains("wrapped//lean-ctx-wrapped.png"));
    }

    #[test]
    fn base_url_is_attribute_escaped() {
        let html = sample().to_share_html(Some("https://me.dev/w?a=1&b=2"));
        assert!(
            html.contains("a=1&amp;b=2"),
            "ampersands in the base URL must be escaped in attributes"
        );
        assert!(
            !html.contains("a=1&b=2\""),
            "a raw unescaped ampersand must not survive into an attribute"
        );
    }

    #[test]
    fn share_text_is_postable_and_honest() {
        let txt = sample().share_text(None);
        assert!(
            txt.contains("348.3M"),
            "headline metric must be in the share line"
        );
        assert!(txt.contains("lean-ctx"), "must name the brand");
        assert!(!txt.contains("http"), "no URL when none is supplied");
    }

    #[test]
    fn share_text_appends_permalink_and_estimate_marker() {
        let mut r = sample();
        r.pricing_estimated = true;
        let txt = r.share_text(Some("https://leanctx.com/w/abc123"));
        assert!(txt.ends_with("https://leanctx.com/w/abc123"));
        assert!(
            txt.contains("(est.)"),
            "estimated pricing must be disclosed"
        );
    }
}
