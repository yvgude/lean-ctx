// SPDX-License-Identifier: Apache-2.0
use std::path::Path;

use super::{BundleOptions, Placement, Unit, build};

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn rust_file(name: &str, lines: usize) -> String {
    let mut out = format!("//! {name} module\n");
    for i in 0..lines {
        out.push_str(&format!(
            "pub fn {name}_step_{i}(value: u32) -> u32 {{\n    value.wrapping_mul({i}) + 1\n}}\n"
        ));
    }
    out
}

fn options(root: &Path, limit: usize, intent: &str) -> BundleOptions {
    let mut opts = BundleOptions::new(root.to_path_buf());
    opts.limit = limit;
    opts.intent = Some(intent.to_string());
    opts
}

/// A demo project plus an isolated data dir for its whole lifetime: `build`
/// scans and persists the graph index there, so without isolation a first and
/// second build could read different indexes written by concurrent tests —
/// and would write into the developer's real data dir.
struct Project {
    root: tempfile::TempDir,
    _data: crate::core::data_dir::IsolatedDataDir,
}

impl Project {
    fn path(&self) -> &Path {
        self.root.path()
    }
}

fn project() -> Project {
    let data = crate::core::data_dir::isolated_data_dir();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for name in ["alpha", "beta", "gamma", "delta", "epsilon", "zeta"] {
        write(root, &format!("src/{name}.rs"), &rust_file(name, 40));
    }
    write(root, "src/billing.rs", &rust_file("invoice", 20));
    write(root, "README.md", "# Demo\n\nA demo project.\n");
    Project {
        root: tmp,
        _data: data,
    }
}

#[test]
fn bundle_never_exceeds_the_limit_and_degrades_by_rank() {
    let tmp = project();
    let bundle = build(&options(tmp.path(), 6_000, "billing invoice totals")).unwrap();

    assert!(bundle.fits(), "{} > {}", bundle.size, bundle.limit);
    assert_eq!(bundle.size, bundle.xml.chars().count());
    assert!(
        bundle
            .xml
            .starts_with("<bundle generator=\"lean-ctx\" limit=\"6000\" unit=\"chars\"")
    );
    assert!(bundle.xml.ends_with("</files>\n</bundle>\n"));
    assert_eq!(bundle.files[0].path, "src/billing.rs");
    assert_eq!(bundle.files[0].placement, Placement::Full);
    assert!(
        bundle.files.iter().any(|f| f.placement != Placement::Full),
        "a 6k limit cannot hold every file in full"
    );
    // The tree still names every file, including the omitted ones.
    for file in &bundle.files {
        let name = file.path.rsplit('/').next().unwrap();
        assert!(bundle.xml.contains(name), "{name} missing from the tree");
    }
}

#[test]
fn generous_limits_include_everything_in_full() {
    let tmp = project();
    let bundle = build(&options(tmp.path(), 1_000_000, "")).unwrap();
    assert!(bundle.fits());
    assert_eq!(bundle.count("full"), bundle.files.len());
    assert!(
        bundle
            .xml
            .contains("<file path=\"README.md\" detail=\"full\">")
    );
}

#[test]
fn token_limits_are_measured_in_tokens() {
    let tmp = project();
    let mut opts = options(tmp.path(), 3_000, "billing");
    opts.unit = Unit::Tokens;
    let bundle = build(&opts).unwrap();
    assert!(bundle.fits());
    assert_eq!(bundle.size, crate::core::tokens::count_tokens(&bundle.xml));
    assert!(bundle.xml.contains("unit=\"tokens\""));
}

#[test]
fn output_is_deterministic() {
    let tmp = project();
    let opts = options(tmp.path(), 8_000, "gamma delta");
    assert_eq!(build(&opts).unwrap().xml, build(&opts).unwrap().xml);
}

#[test]
fn files_with_secrets_are_withheld_unless_the_check_is_off() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let key = concat!("AK", "IAIOSFODNN7EXAMPLE");
    write(
        root,
        "src/config.rs",
        &format!("const KEY: &str = \"{key}\";\n"),
    );
    write(root, "src/lib.rs", "pub fn ok() {}\n");

    let bundle = build(&options(root, 50_000, "")).unwrap();
    assert!(!bundle.xml.contains(key), "secret leaked into the bundle");
    assert_eq!(bundle.withheld[0].path, "src/config.rs");
    assert!(
        bundle.withheld[0]
            .reason
            .starts_with("possible secret (aws_key L1")
    );
    assert!(bundle.xml.contains("- src/config.rs: possible secret"));

    let mut unchecked = options(root, 50_000, "");
    unchecked.security_check = false;
    assert!(build(&unchecked).unwrap().xml.contains(key));
}

#[test]
fn a_limit_below_the_frame_reports_over_limit_but_still_renders() {
    let tmp = project();
    let bundle = build(&options(tmp.path(), 50, "")).unwrap();
    assert!(!bundle.fits());
    assert_eq!(bundle.count("full") + bundle.count("signatures"), 0);
    assert!(bundle.xml.ends_with("</bundle>\n"));
    assert!(bundle.report().contains("OVER LIMIT"));
}

#[test]
fn scope_must_stay_inside_the_root() {
    let tmp = project();
    let outside = tempfile::tempdir().unwrap();
    let mut opts = options(tmp.path(), 10_000, "");
    opts.scope = Some(outside.path().to_path_buf());
    let err = build(&opts).unwrap_err();
    assert!(err.contains("outside the project root"), "{err}");

    opts.scope = Some("src/billing.rs".into());
    let bundle = build(&opts).unwrap();
    let paths: Vec<&str> = bundle.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["src/billing.rs"]);
}

#[test]
fn intent_text_is_escaped_and_classified() {
    let tmp = project();
    let bundle = build(&options(tmp.path(), 20_000, "review <billing> & totals")).unwrap();
    assert_eq!(bundle.intent.to_string(), "review");
    assert!(
        bundle
            .xml
            .contains("<task>review &lt;billing&gt; &amp; totals</task>")
    );
}
