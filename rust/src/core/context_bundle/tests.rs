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

/// Was `files_with_secrets_are_withheld_unless_the_check_is_off`.
///
/// Rule (G01): the admitted reader decides on every source BEFORE the
/// bundle's own scan sees it, and `security_check` cannot switch that
/// decision off. A built-in secret is therefore masked by the reader — the
/// file is bundled in its masked form — whether the scan is on or off; the
/// old expectation (file withheld with the scan on, raw key in the bundle
/// with the scan off) no longer exists.
///
/// What the scan still does on its own: it withholds a file in which a
/// finding is left in the ADMITTED text. That takes an operator rule which
/// keeps the match out of the gateway's masking — here
/// `[secret_detection].exclude_patterns`, which the scan does not apply.
/// Switching the scan off then bundles exactly what the reader delivered.
#[test]
fn the_reader_masks_a_secret_with_the_scan_on_or_off_and_the_scan_withholds_what_is_left() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Assembled at run time: no key-shaped literal in the source.
    let key = ["AK", "IAIOSFODNN7", "EXAMPLE"].concat();
    write(
        root,
        "src/config.rs",
        &format!("const KEY: &str = \"{key}\";\n"),
    );
    write(root, "src/lib.rs", "pub fn ok() {}\n");
    let unchecked = || {
        let mut opts = options(root, 50_000, "");
        opts.security_check = false;
        opts
    };

    for opts in [options(root, 50_000, ""), unchecked()] {
        let scan = opts.security_check;
        let bundle = build(&opts).unwrap();
        assert!(!bundle.xml.contains(&key), "scan {scan}: {}", bundle.xml);
        assert!(
            bundle
                .xml
                .contains("const KEY: &str = \"[REDACTED:AWS key]\";"),
            "scan {scan}: {}",
            bundle.xml
        );
        assert!(bundle.withheld.is_empty(), "scan {scan}");
    }

    // The operator carves the match out of the gateway's masking.
    let config_dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
    std::fs::create_dir_all(&config_dir).expect("config dir");
    std::fs::write(
        config_dir.join("config.toml"),
        "[secret_detection]\nexclude_patterns = ['EXAMPLE$']\n",
    )
    .expect("config");

    let bundle = build(&options(root, 50_000, "")).unwrap();
    assert!(!bundle.xml.contains(&key), "secret leaked into the bundle");
    assert_eq!(bundle.withheld[0].path, "src/config.rs");
    assert!(
        bundle.withheld[0]
            .reason
            .starts_with("possible secret (aws_key L1")
    );
    assert!(bundle.xml.contains("- src/config.rs: possible secret"));

    let bundle = build(&unchecked()).unwrap();
    assert!(bundle.withheld.is_empty());
    assert!(bundle.xml.contains("<file path=\"src/config.rs\""));
}

/// G01: the bundle read every file with a plain `fs::read`, so a source the
/// project policy blocks was bundled in full and ranked. Every file now comes
/// from the rooted admitted reader: the withheld source has no content and no
/// rank, and all the bundle says about it is its path with the reader's own
/// content-free sentence — the way the read tools name a withheld source.
#[test]
fn a_source_the_admitted_reader_withholds_is_named_but_neither_bundled_nor_ranked() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(
        crate::core::policy::load(
            "name = \"bundle-admission-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
             [filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )
        .unwrap(),
    ));
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "src/open.rs", "pub fn open_marker() {}\n");
    write(
        root,
        "src/plan.rs",
        "CONFIDENTIAL\npub fn hidden_marker() {}\n",
    );

    // The task names the withheld file: a plain read would rank it first.
    let bundle = build(&options(root, 50_000, "plan")).unwrap();

    assert!(bundle.xml.contains("pub fn open_marker() {}"));
    assert!(!bundle.xml.contains("hidden_marker"), "{}", bundle.xml);
    assert!(!bundle.xml.contains("CONFIDENTIAL"), "{}", bundle.xml);
    assert!(!bundle.xml.contains("<file path=\"src/plan.rs\""));
    let ranked: Vec<&str> = bundle.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(ranked, ["src/open.rs"]);
    assert_eq!(
        bundle.withheld,
        [super::Withheld {
            path: "src/plan.rs".to_string(),
            reason: "source content withheld by policy".to_string(),
        }]
    );
    assert!(
        bundle
            .xml
            .contains("- src/plan.rs: source content withheld by policy")
    );
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

/// The bundle's knowledge section carries only facts that are served now:
/// the fact bound to a deleted file is left out, the unbound one stays.
#[test]
fn a_fact_that_is_not_served_is_not_bundled() {
    use crate::core::context_admission::stored::lineage_support::{
        governed, project as bound_project, scoped, two_facts, withdraw_source,
    };
    let _data = governed();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let (_directory, root) = bound_project();
    two_facts(&root, "architecture");
    let section = || {
        scoped(&root, || {
            super::knowledge_facts(&root, &["all".to_owned()], 10, false).join("\n")
        })
    };
    let before = section();
    assert!(before.contains("RS256"), "{before}");
    assert!(before.contains("release captain"), "{before}");
    withdraw_source(&root);
    let after = section();
    assert!(!after.contains("RS256"), "{after}");
    assert!(!after.contains("token-check"), "{after}");
    assert!(after.contains("release captain"), "{after}");
}
