// SPDX-License-Identifier: Apache-2.0

//! Links inside page bodies: `[Title](/slug#frag)` to another page and
//! `[Title](emspace:tasks/<id>)`-style references to other objects.
//! Code fences and inline code spans are never touched.

use crate::sections::Fence;

/// What a link points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target<'a> {
    /// A page of the same space, by slug, with an optional `#fragment`.
    Page {
        slug: &'a str,
        fragment: Option<&'a str>,
    },
    /// A Hive node by id (`hive:n_…`), with an optional `#heading-slug`.
    Node {
        id: &'a str,
        fragment: Option<&'a str>,
    },
    /// An object addressed `<scheme>:<kind>/<id>`, e.g. `emspace:tasks/01a1…`.
    Object {
        scheme: &'a str,
        kind: &'a str,
        id: &'a str,
    },
    /// Anything else: a URL, a relative path, an anchor.
    Other(&'a str),
}

/// Classifies a link target.
pub fn classify(target: &str) -> Target<'_> {
    if let Some(path) = target.strip_prefix('/') {
        if path.starts_with('/') || path.is_empty() {
            return Target::Other(target);
        }
        let (slug, fragment) = match path.split_once('#') {
            Some((s, f)) => (s, Some(f)),
            None => (path, None),
        };
        return Target::Page {
            slug: slug.trim_end_matches('/'),
            fragment,
        };
    }
    if let Some(node) = target.strip_prefix("hive:") {
        let (id, fragment) = match node.split_once('#') {
            Some((i, f)) => (i, Some(f)),
            None => (node, None),
        };
        if id.starts_with("n_")
            && id.len() > 2
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Target::Node { id, fragment };
        }
    }
    if let Some((scheme, rest)) = target.split_once(':') {
        let plain = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        };
        if let Some((kind, id)) = rest.split_once('/') {
            if plain(scheme) && plain(kind) && plain(id) && !rest.starts_with("//") {
                return Target::Object { scheme, kind, id };
            }
        }
    }
    Target::Other(target)
}

/// What to do with one link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Point it at this target instead.
    Target(String),
    /// Drop the link and keep its text.
    TextOnly,
}

/// Every link target in `body`, in order.
pub fn targets(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    scan(body, |target| {
        out.push(target);
        None
    });
    out
}

/// `body` with each link target replaced by `f(target)` where it returns one.
pub fn rewrite(body: &str, mut f: impl FnMut(&str) -> Option<String>) -> String {
    scan(body, |t| f(t).map(Edit::Target))
}

/// C0 2.2 write rule: `/slug[#frag]` naming a live page becomes
/// `hive:<id>[#frag]`. Returns the body and the targets no page has, which
/// stay as written. Other targets are untouched.
pub fn to_storage(body: &str, id_of: impl Fn(&str) -> Option<String>) -> (String, Vec<String>) {
    let mut dangling = Vec::new();
    let out = scan(body, |t| {
        let Target::Page { slug, fragment } = classify(t) else {
            return None;
        };
        let id = id_of(slug);
        if id.is_none() {
            dangling.push(t.to_owned());
        }
        id.map(|id| Edit::Target(with_fragment(&format!("hive:{id}"), fragment)))
    });
    (out, dangling)
}

/// C0 2.2 export rule: `hive:<id>[#frag]` becomes the target page's
/// `<slug>.md` relative to `from_slug`'s; a link to no live page keeps its
/// text only.
pub fn to_export(body: &str, from_slug: &str, slug_of: impl Fn(&str) -> Option<String>) -> String {
    scan(body, |t| match classify(t) {
        Target::Node { id, fragment } => Some(match slug_of(id) {
            Some(slug) => Edit::Target(with_fragment(&relative_md(from_slug, &slug), fragment)),
            None => Edit::TextOnly,
        }),
        _ => None,
    })
}

/// `hive:<id>[#frag]` back to `/slug[#frag]`, for agents reading a page.
pub fn to_slugs(body: &str, slug_of: impl Fn(&str) -> Option<String>) -> String {
    scan(body, |t| match classify(t) {
        Target::Node { id, fragment } => {
            slug_of(id).map(|slug| Edit::Target(with_fragment(&format!("/{slug}"), fragment)))
        }
        _ => None,
    })
}

fn with_fragment(base: &str, fragment: Option<&str>) -> String {
    match fragment {
        Some(f) => format!("{base}#{f}"),
        None => base.to_owned(),
    }
}

/// The path of `to`'s `<slug>.md` relative to the directory of `from`'s.
pub fn relative_md(from: &str, to: &str) -> String {
    let from_dirs: Vec<&str> = from.split('/').collect();
    let from_dirs = &from_dirs[..from_dirs.len().saturating_sub(1)];
    let to_parts: Vec<&str> = to.split('/').collect();
    let common = from_dirs
        .iter()
        .zip(&to_parts)
        .take_while(|(a, b)| a == b)
        .count();
    let mut out: Vec<&str> = vec![".."; from_dirs.len() - common];
    out.extend(&to_parts[common..]);
    format!("{}.md", out.join("/"))
}

/// The heading slugs of a page's markdown in reading order, `# ` title
/// included: GitHub style, a repeat gets `-1`, `-2`, … (C0 2.2).
pub fn heading_slugs(markdown: &str) -> Vec<String> {
    headings(markdown).into_iter().map(|h| h.slug).collect()
}

/// One ATX heading of a page's markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    /// Byte offset of the heading line.
    pub start: usize,
    /// 1 for `#`, 2 for `##`, ….
    pub level: usize,
    pub text: String,
    /// Its slug, `-1`, `-2`, … for repeats (C0 2.2).
    pub slug: String,
}

/// The headings outside code fences, in reading order.
pub fn headings(markdown: &str) -> Vec<Heading> {
    let mut fence = Fence::default();
    let mut seen = std::collections::BTreeMap::<String, usize>::new();
    let mut out = Vec::new();
    let mut pos = 0;
    for line in markdown.split_inclusive('\n') {
        let start = pos;
        pos += line.len();
        if fence.feed(line) {
            continue;
        }
        let hashes = line.bytes().take_while(|b| *b == b'#').count();
        if !(1..=6).contains(&hashes) || line.as_bytes().get(hashes) != Some(&b' ') {
            continue;
        }
        let text = line[hashes + 1..].trim().to_owned();
        let base = heading_slug(&text);
        let n = seen.entry(base.clone()).or_insert(0);
        let slug = if *n == 0 { base } else { format!("{base}-{n}") };
        *n += 1;
        out.push(Heading {
            start,
            level: hashes,
            text,
            slug,
        });
    }
    out
}

/// The stretch from the heading with slug `want` to the next heading of the
/// same or a higher level.
pub fn section_of<'a>(markdown: &'a str, want: &str) -> Option<&'a str> {
    let hs = headings(markdown);
    let i = hs.iter().position(|h| h.slug == want)?;
    let end = hs[i + 1..]
        .iter()
        .find(|h| h.level <= hs[i].level)
        .map_or(markdown.len(), |h| h.start);
    Some(&markdown[hs[i].start..end])
}

/// One heading's slug: lowercase, each space to `-`, nothing outside `[a-z0-9-]`.
pub fn heading_slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            'a'..='z' | '0'..='9' | '-' => Some(c),
            _ => None,
        })
        .collect()
}

/// Walks the links outside code, letting `f` edit them, and returns the
/// rewritten body.
fn scan<'a>(body: &'a str, mut f: impl FnMut(&'a str) -> Option<Edit>) -> String {
    let mut out = String::with_capacity(body.len());
    let mut fence = Fence::default();
    for line in body.split_inclusive('\n') {
        if fence.feed(line) {
            out.push_str(line);
        } else {
            scan_line(line, &mut out, &mut f);
        }
    }
    out
}

/// The `)` that closes a link target starting at `start`, parentheses
/// balanced; `None` when a space or the line end comes first.
fn target_end(b: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (j, c) in b[start..].iter().enumerate() {
        match c {
            b'(' => depth += 1,
            b')' if depth == 0 => return Some(start + j),
            b')' => depth -= 1,
            b' ' | b'\n' => return None,
            _ => {}
        }
    }
    None
}

fn scan_line<'a>(line: &'a str, out: &mut String, f: &mut impl FnMut(&'a str) -> Option<Edit>) {
    let b = line.as_bytes();
    let mut i = 0;
    let mut copied = 0;
    let mut opens: Vec<usize> = Vec::new();
    while i < b.len() {
        if b[i] == b'[' {
            opens.push(i);
        }
        if b[i] == b'`' {
            // Skip an inline code span: the same run of backticks closes it.
            let run = b[i..].iter().take_while(|c| **c == b'`').count();
            let ticks = &line[i..i + run];
            match line[i + run..].find(ticks) {
                Some(end) => i += run + end + run,
                None => i += run,
            }
            continue;
        }
        if b[i] == b']' && b.get(i + 1) == Some(&b'(') {
            let start = i + 2;
            let end = target_end(b, start);
            let open = opens.pop();
            if let Some(end) = end {
                match f(&line[start..end]) {
                    Some(Edit::Target(new)) => {
                        out.push_str(&line[copied..start]);
                        out.push_str(&new);
                        copied = end;
                    }
                    // An image (`![..](..)`) keeps its markup.
                    Some(Edit::TextOnly) => match open {
                        Some(o) if o >= copied && (o == 0 || b[o - 1] != b'!') => {
                            out.push_str(&line[copied..o]);
                            out.push_str(&line[o + 1..i]);
                            copied = end + 1;
                        }
                        _ => {}
                    },
                    None => {}
                }
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&line[copied..]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_pages_objects_and_others() {
        assert_eq!(
            classify("/design/schema#tables"),
            Target::Page {
                slug: "design/schema",
                fragment: Some("tables")
            }
        );
        assert_eq!(
            classify("/index"),
            Target::Page {
                slug: "index",
                fragment: None
            }
        );
        assert_eq!(
            classify("emspace:tasks/01a1"),
            Target::Object {
                scheme: "emspace",
                kind: "tasks",
                id: "01a1"
            }
        );
        assert_eq!(
            classify("hive:n_01abc#retry-budget"),
            Target::Node {
                id: "n_01abc",
                fragment: Some("retry-budget")
            }
        );
        assert_eq!(classify("hive:x"), Target::Other("hive:x"));
        assert_eq!(classify("https://x.y/z"), Target::Other("https://x.y/z"));
        assert_eq!(classify("//cdn/x"), Target::Other("//cdn/x"));
    }

    #[test]
    fn finds_targets_outside_code_only() {
        let body = "see [A](/a) and `[B](/b)`\n```\n[C](/c)\n```\n[D](/d#x) [E](emspace:tasks/9)\n";
        assert_eq!(targets(body), ["/a", "/d#x", "emspace:tasks/9"]);
    }

    #[test]
    fn rewrites_selected_targets_losslessly() {
        let body = "[A](/a) [U](https://u) [T](emspace:tasks/9)\n";
        let out = rewrite(body, |t| match classify(t) {
            Target::Object { id, .. } => Some(format!("hive:task/{id}")),
            _ => None,
        });
        assert_eq!(out, "[A](/a) [U](https://u) [T](hive:task/9)\n");
        assert_eq!(rewrite(body, |_| None), body);
    }

    // C0 2.2 vectors (hive-v2 vectors/doc_links + heading_slug).
    fn pages() -> Vec<(&'static str, &'static str)> {
        vec![
            ("n_aaaaaaaaaaaaaaaaaaaaaadpga", "knowledge/wiki"),
            ("n_aaaaaaaaaaaaaaaaaaaaaadpgb", "knowledge/knowledge-items"),
            (
                "n_aaaaaaaaaaaaaaaaaaaaaadpgc",
                "architecture/place-event-store",
            ),
        ]
    }

    #[test]
    fn write_vectors() {
        let id_of = |s: &str| {
            pages()
                .into_iter()
                .find(|p| p.1 == s)
                .map(|p| p.0.to_owned())
        };
        let cases = [
            (
                "[types](/knowledge/knowledge-items#types-and-bodies)",
                "[types](hive:n_aaaaaaaaaaaaaaaaaaaaaadpgb#types-and-bodies)",
                vec![],
            ),
            (
                "[log](/architecture/place-event-store)",
                "[log](hive:n_aaaaaaaaaaaaaaaaaaaaaadpgc)",
                vec![],
            ),
            (
                "[x](/no/such-page)",
                "[x](/no/such-page)",
                vec!["/no/such-page".to_owned()],
            ),
            (
                "[web](https://leanctx.com/)",
                "[web](https://leanctx.com/)",
                vec![],
            ),
        ];
        for (input, out, dangling) in cases {
            assert_eq!(
                to_storage(input, id_of),
                (out.to_owned(), dangling),
                "{input}"
            );
        }
    }

    #[test]
    fn export_vectors() {
        let slug_of = |id: &str| {
            pages()
                .into_iter()
                .find(|p| p.0 == id)
                .map(|p| p.1.to_owned())
        };
        let cases = [
            (
                "[types](hive:n_aaaaaaaaaaaaaaaaaaaaaadpgb#types-and-bodies)",
                "[types](knowledge-items.md#types-and-bodies)",
            ),
            (
                "[log](hive:n_aaaaaaaaaaaaaaaaaaaaaadpgc)",
                "[log](../architecture/place-event-store.md)",
            ),
            ("[gone](hive:n_aaaaaaaaaaaaaaaaaaaaaadpgd)", "gone"),
        ];
        for (input, out) in cases {
            assert_eq!(to_export(input, "knowledge/wiki", slug_of), out, "{input}");
        }
        assert_eq!(
            to_slugs(
                "[t](hive:n_aaaaaaaaaaaaaaaaaaaaaadpgb#x) ![i](hive:n_zz)",
                slug_of
            ),
            "[t](/knowledge/knowledge-items#x) ![i](hive:n_zz)"
        );
        assert_eq!(
            to_export("![i](hive:n_zz) and [g](hive:n_zz).", "a", slug_of),
            "![i](hive:n_zz) and g."
        );
    }

    #[test]
    fn heading_slug_vectors() {
        let md = "# Wiki\n## What It Does\n## C0 2.2: Docs!\n## `base_event` (CAS)\n## Rules And Edge Cases\n\
                  ## Two  Spaces\n## Über Pages\n```\n# not a heading\n```\n## Rules And Edge Cases\n## Rules And Edge Cases\n";
        assert_eq!(
            heading_slugs(md),
            [
                "wiki",
                "what-it-does",
                "c0-22-docs",
                "baseevent-cas",
                "rules-and-edge-cases",
                "two--spaces",
                "ber-pages",
                "rules-and-edge-cases-1",
                "rules-and-edge-cases-2",
            ]
        );
    }

    #[test]
    fn balanced_parentheses_stay_in_the_target() {
        assert_eq!(
            targets("[w](https://e.org/a_(b))\n"),
            ["https://e.org/a_(b)"]
        );
    }
}
