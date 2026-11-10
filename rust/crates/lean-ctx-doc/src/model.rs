// SPDX-License-Identifier: Apache-2.0

//! Pages, sections and notes.

use serde::{Deserialize, Serialize};

use crate::anchor::Anchor;

/// Where a page stands in its curation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageStatus {
    /// New material, not yet vouched for.
    Draft,
    /// Settled: what the next reader should trust.
    Canonical,
    /// History: kept, behind nothing.
    Retired,
}

impl PageStatus {
    /// Parses the status names emspace and the wire use.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(Self::Draft),
            "canonical" => Some(Self::Canonical),
            "retired" => Some(Self::Retired),
            _ => None,
        }
    }
}

/// C0 2.2 slug: `seg(/seg){0,7}`, seg `[a-z0-9]+(-[a-z0-9]+)*`, ≤ 128 bytes.
pub fn valid_slug(slug: &str) -> bool {
    let segs: Vec<&str> = slug.split('/').collect();
    slug.len() <= 128
        && segs.len() <= 8
        && segs.iter().all(|s| {
            !s.is_empty()
                && !s.starts_with('-')
                && !s.ends_with('-')
                && !s.contains("--")
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// One stretch of a page body.
///
/// `text` is raw markdown and includes the section's own heading line when it
/// has one; concatenating the sections in `order` reproduces the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    pub order: u32,
    pub text: String,
}

impl Section {
    /// The heading this section opens with (`## Foo` → `Foo`), if any.
    pub fn heading(&self) -> Option<&str> {
        let first = self.text.lines().next()?;
        let hashes = first.bytes().take_while(|b| *b == b'#').count();
        if (2..=6).contains(&hashes) && first.as_bytes().get(hashes) == Some(&b' ') {
            Some(first[hashes + 1..].trim())
        } else {
            None
        }
    }
}

/// A dated addendum to a page. A note that `covers` anchors accounts for
/// their change in words, so the page reads noted instead of stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covers: Vec<String>,
}

/// A documentation page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub slug: String,
    pub title: String,
    /// Free classification: `reference`, `design`, `project`, ….
    pub kind: String,
    pub status: PageStatus,
    pub sections: Vec<Section>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<Anchor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
}

impl Page {
    /// The body as one markdown document, title line first.
    pub fn body(&self) -> String {
        crate::sections::join(&self.title, &self.sections)
    }
}
