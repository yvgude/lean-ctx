//! Evidence / citation metadata attached to every fetched document.
//!
//! Research agents must be able to attribute claims to a source. Every
//! [`crate::core::web`] read carries a [`Citation`] (URL, title, site, fetch
//! timestamp) that is rendered as a compact footer so downstream reasoning — and
//! the human reading the answer — can trace each fact back to where it came
//! from.

use super::url_guard;

/// Source attribution for a fetched document or quote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    pub url: String,
    pub title: Option<String>,
    pub site: Option<String>,
    /// RFC 3339 UTC timestamp of when the content was retrieved.
    pub fetched_at: String,
}

impl Citation {
    #[must_use]
    pub fn new(url: &str, title: Option<String>) -> Self {
        Self {
            url: url.to_string(),
            title,
            site: url_guard::validate(url).ok().map(|u| u.host),
            fetched_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Render a compact, machine-and-human readable citation footer.
    #[must_use]
    pub fn footer(&self) -> String {
        let headline = match &self.title {
            Some(t) if !t.is_empty() => format!("{t} — {}", self.url),
            _ => self.url.clone(),
        };
        let site = self.site.as_deref().unwrap_or("unknown");
        format!(
            "\n\n---\nSource: {headline}\nSite: {site} · Retrieved: {}",
            self.fetched_at
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_site_from_url() {
        let c = Citation::new("https://en.wikipedia.org/wiki/Rust", Some("Rust".into()));
        assert_eq!(c.site.as_deref(), Some("en.wikipedia.org"));
    }

    #[test]
    fn footer_contains_source_and_retrieval() {
        let c = Citation::new("https://x.com/a", Some("Title".into()));
        let footer = c.footer();
        assert!(footer.contains("Source: Title — https://x.com/a"));
        assert!(footer.contains("Site: x.com"));
        assert!(footer.contains("Retrieved:"));
    }

    #[test]
    fn footer_without_title_uses_url() {
        let c = Citation::new("https://x.com/a", None);
        assert!(c.footer().contains("Source: https://x.com/a"));
    }
}
