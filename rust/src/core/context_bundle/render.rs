// SPDX-License-Identifier: Apache-2.0
//! Output rendering: the XML bundle and the directory tree.
//!
//! The format follows the Repomix convention chat models already know: a
//! `<summary>`, a `<directory_structure>`, then one `<file>` per path with the
//! body in CDATA so source code needs no escaping.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// How much of a file made it into the bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Detail {
    Full,
    Signatures,
}

impl Detail {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Signatures => "signatures",
        }
    }
}

/// Escape text for an XML attribute or element body.
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Wrap `body` in CDATA, splitting any `]]>` it contains.
fn cdata(body: &str) -> String {
    format!("<![CDATA[{}]]>", body.replace("]]>", "]]]]><![CDATA[>"))
}

/// One `<file>` element, newline-terminated.
pub(crate) fn file_block(path: &str, detail: Detail, body: &str) -> String {
    let newline = if body.ends_with('\n') { "" } else { "\n" };
    format!(
        "<file path=\"{}\" detail=\"{}\">{}</file>\n",
        escape(path),
        detail.as_str(),
        cdata(&format!("\n{body}{newline}"))
    )
}

/// One `<fact>` element, newline-terminated.
pub(crate) fn fact_block(category: &str, key: &str, value: &str) -> String {
    format!(
        "<fact category=\"{}\" key=\"{}\">{}</fact>\n",
        escape(category),
        escape(key),
        escape(value)
    )
}

#[derive(Default)]
struct Dir {
    dirs: BTreeMap<String, Dir>,
    files: Vec<String>,
}

impl Dir {
    fn insert(&mut self, path: &str) {
        let mut node = self;
        let mut parts = path.split('/').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                node.files.push(part.to_string());
            } else {
                node = node.dirs.entry(part.to_string()).or_default();
            }
        }
    }

    fn file_count(&self) -> usize {
        self.files.len() + self.dirs.values().map(Dir::file_count).sum::<usize>()
    }

    fn render(&self, depth: usize, max_depth: Option<usize>, with_files: bool, out: &mut String) {
        let indent = "  ".repeat(depth);
        for (name, dir) in &self.dirs {
            if with_files {
                let _ = writeln!(out, "{indent}{name}/");
            } else {
                let _ = writeln!(out, "{indent}{name}/ ({})", files_label(dir.file_count()));
            }
            if max_depth.is_none_or(|max| depth + 1 < max) {
                dir.render(depth + 1, max_depth, with_files, out);
            }
        }
        if with_files {
            let mut files = self.files.clone();
            files.sort();
            for file in files {
                let _ = writeln!(out, "{indent}{file}");
            }
        }
    }
}

/// Directory tree of `paths`, degraded until it fits `max_size` (measured by
/// `measure`): every file → directories with counts → top-level directories.
pub(crate) fn directory_tree(
    paths: &[&str],
    max_size: usize,
    measure: impl Fn(&str) -> usize,
) -> String {
    let mut root = Dir::default();
    for path in paths {
        root.insert(path);
    }
    let variants: [(Option<usize>, bool); 3] = [(None, true), (None, false), (Some(1), false)];
    let mut last = String::new();
    for (max_depth, with_files) in variants {
        let mut out = String::new();
        root.render(0, max_depth, with_files, &mut out);
        if !with_files && !root.files.is_empty() {
            let _ = writeln!(out, "({} at the root)", files_label(root.files.len()));
        }
        if measure(&out) <= max_size {
            return out;
        }
        last = out;
    }
    if measure(&last) <= max_size {
        last
    } else {
        format!(
            "({}; tree omitted to fit the limit)\n",
            files_label(paths.len())
        )
    }
}

pub(crate) fn files_label(count: usize) -> String {
    if count == 1 {
        "1 file".to_string()
    } else {
        format!("{count} files")
    }
}

#[cfg(test)]
mod tests {
    use super::{Detail, directory_tree, escape, fact_block, file_block};

    #[test]
    fn file_blocks_survive_cdata_terminators_and_escape_paths() {
        let block = file_block("a&b.rs", Detail::Full, "let s = \"]]>\";");
        assert!(block.starts_with("<file path=\"a&amp;b.rs\" detail=\"full\"><![CDATA[\n"));
        assert!(block.contains("]]]]><![CDATA[>"));
        assert!(block.ends_with("\n]]></file>\n"));
    }

    #[test]
    fn facts_are_escaped() {
        assert_eq!(
            fact_block("decision", "k<1>", "a & b"),
            "<fact category=\"decision\" key=\"k&lt;1&gt;\">a &amp; b</fact>\n"
        );
        assert_eq!(escape("\"x\""), "&quot;x&quot;");
    }

    #[test]
    fn tree_lists_files_when_it_fits() {
        let tree = directory_tree(&["src/b.rs", "src/a.rs", "README.md"], 1_000, str::len);
        assert_eq!(tree, "src/\n  a.rs\n  b.rs\nREADME.md\n");
    }

    #[test]
    fn tree_collapses_to_directory_counts_then_top_level() {
        let mut paths: Vec<String> = ('a'..='h').map(|c| format!("src/core/{c}.rs")).collect();
        paths.push("src/cli/c.rs".to_string());
        paths.push("x.md".to_string());
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let dirs = directory_tree(&paths, 80, str::len);
        assert_eq!(
            dirs,
            "src/ (9 files)\n  cli/ (1 file)\n  core/ (8 files)\n(1 file at the root)\n"
        );
        let top = directory_tree(&paths, 40, str::len);
        assert_eq!(top, "src/ (9 files)\n(1 file at the root)\n");
        let none = directory_tree(&paths, 5, str::len);
        assert!(none.contains("tree omitted"), "{none}");
    }
}
