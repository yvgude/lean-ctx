// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx pack [<path>] --limit 128k` — one budgeted XML bundle for chat
//! products (#1885). The engine lives in `core::context_bundle`; this module
//! only parses flags and routes the output.

use std::path::{Path, PathBuf};

use crate::core::context_bundle::{
    self, BundleOptions, DEFAULT_KNOWLEDGE_CATEGORIES, Unit, parse_limit,
};

/// Flags that take a value (`--flag value` or `--flag=value`).
const VALUE_FLAGS: &[&str] = &[
    "--limit",
    "--unit",
    "--intent",
    "--emit",
    "--output",
    "-o",
    "--include",
    "--ignore",
    "--knowledge-limit",
    "--root",
    "--project-root",
];
/// Flags without a value (`--with-knowledge` optionally takes `=cats`).
const SWITCHES: &[&str] = &[
    "--copy",
    "--stats",
    "--with-knowledge",
    "--with-auto",
    "--no-security-check",
    "--force",
];
/// Flags that only exist for bundles; any of them selects bundle mode.
const BUNDLE_ONLY: &[&str] = &[
    "--limit",
    "--unit",
    "--intent",
    "--emit",
    "-o",
    "--include",
    "--ignore",
    "--knowledge-limit",
    "--copy",
    "--stats",
    "--with-knowledge",
    "--with-auto",
    "--no-security-check",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Emit {
    Xml,
    Plain,
    Both,
}

#[derive(Debug)]
struct Args {
    path: Option<String>,
    root: Option<String>,
    limit: usize,
    unit: Unit,
    intent: Option<String>,
    emit: Emit,
    output: Option<String>,
    copy: bool,
    stats: bool,
    include: Vec<String>,
    exclude: Vec<String>,
    knowledge: Option<Vec<String>>,
    knowledge_limit: usize,
    knowledge_auto: bool,
    security_check: bool,
}

fn flag_name(arg: &str) -> &str {
    arg.split_once('=').map_or(arg, |(name, _)| name)
}

/// First positional argument, skipping flag values.
fn first_positional(args: &[String]) -> Option<&str> {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg.starts_with('-') {
            if !arg.contains('=') && VALUE_FLAGS.contains(&arg.as_str()) {
                it.next();
            }
            continue;
        }
        return Some(arg);
    }
    None
}

/// Bundle mode: a bundle-only flag is present, or the first positional
/// argument is an existing path rather than a pack subcommand.
pub(super) fn wants_bundle(args: &[String], subcommands: &[&str]) -> bool {
    if args.iter().any(|a| BUNDLE_ONLY.contains(&flag_name(a))) {
        return true;
    }
    first_positional(args).is_some_and(|p| !subcommands.contains(&p) && Path::new(p).exists())
}

fn split_list(value: &str) -> impl Iterator<Item = String> + '_ {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut out = Args {
        path: None,
        root: None,
        limit: context_bundle::DEFAULT_LIMIT,
        unit: Unit::Chars,
        intent: None,
        emit: Emit::Xml,
        output: None,
        copy: false,
        stats: false,
        include: Vec::new(),
        exclude: Vec::new(),
        knowledge: None,
        knowledge_limit: context_bundle::DEFAULT_KNOWLEDGE_LIMIT,
        knowledge_auto: false,
        security_check: true,
    };
    let mut force = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if !arg.starts_with('-') {
            if out.path.is_some() {
                return Err(format!("unexpected argument '{arg}' (one path at most)"));
            }
            out.path = Some(arg.clone());
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (arg.as_str(), None),
        };
        if SWITCHES.contains(&name) {
            match name {
                "--copy" => out.copy = true,
                "--stats" => out.stats = true,
                "--force" => force = true,
                "--with-auto" => out.knowledge_auto = true,
                "--no-security-check" => out.security_check = false,
                _ => {
                    let cats = inline.as_deref().map_or_else(
                        || {
                            DEFAULT_KNOWLEDGE_CATEGORIES
                                .iter()
                                .map(|c| (*c).to_string())
                                .collect()
                        },
                        |v| split_list(v).collect::<Vec<_>>(),
                    );
                    if cats.is_empty() {
                        return Err("--with-knowledge= needs at least one category".into());
                    }
                    out.knowledge = Some(cats);
                }
            }
            if inline.is_some() && name != "--with-knowledge" {
                return Err(format!("{name} takes no value"));
            }
            continue;
        }
        if !VALUE_FLAGS.contains(&name) {
            return Err(format!("unknown flag '{name}'"));
        }
        let value = match inline {
            Some(v) => v,
            None => it
                .next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))?,
        };
        match name {
            "--limit" => out.limit = parse_limit(&value)?,
            "--unit" => out.unit = Unit::parse(&value)?,
            "--intent" => out.intent = Some(value),
            "--emit" => {
                out.emit = match value.as_str() {
                    "xml" => Emit::Xml,
                    "plain" => Emit::Plain,
                    "both" => Emit::Both,
                    other => {
                        return Err(format!("--emit must be xml, plain or both (got '{other}')"));
                    }
                }
            }
            "--output" | "-o" => out.output = (value != "-").then_some(value),
            "--include" => out.include.extend(split_list(&value)),
            "--ignore" => out.exclude.extend(split_list(&value)),
            "--knowledge-limit" => {
                out.knowledge_limit = value
                    .parse()
                    .map_err(|_| format!("--knowledge-limit must be a number (got '{value}')"))?;
            }
            _ => out.root = Some(value),
        }
    }
    if !out.security_check && !force {
        return Err(
            "--no-security-check can put secrets into the bundle; add --force to confirm".into(),
        );
    }
    if out.knowledge_auto && out.knowledge.is_none() {
        return Err("--with-auto only applies together with --with-knowledge".into());
    }
    if out.emit == Emit::Plain && (out.output.is_some() || out.copy) {
        return Err(
            "--emit plain prints only the report; drop --output/--copy or use --emit both".into(),
        );
    }
    Ok(out)
}

/// Project root: `--root` wins, then the git root of the bundled path, then
/// the git root of the working directory.
fn resolve_root(args: &Args, raw: &[String], path: Option<&Path>) -> PathBuf {
    if args.root.is_some() {
        return PathBuf::from(crate::cli::common::detect_project_root(raw));
    }
    if let Some(path) = path {
        let dir = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        return PathBuf::from(crate::cli::common::promote_to_git_root(
            &dir.to_string_lossy(),
        ));
    }
    PathBuf::from(crate::cli::common::detect_project_root(raw))
}

pub(super) fn cmd_pack_bundle(raw: &[String]) {
    let args = match parse(raw) {
        Ok(args) => args,
        Err(err) => {
            eprintln!("lean-ctx pack: {err}");
            eprintln!("Run 'lean-ctx pack --help' for usage.");
            std::process::exit(2);
        }
    };
    let path = args.path.as_ref().map(|p| {
        let p = PathBuf::from(p);
        if p.is_absolute() {
            p
        } else {
            std::env::current_dir().unwrap_or_default().join(p)
        }
    });
    if let Some(p) = &path
        && !p.exists()
    {
        eprintln!("lean-ctx pack: path not found: {}", p.display());
        std::process::exit(2);
    }

    let mut opts = BundleOptions::new(resolve_root(&args, raw, path.as_deref()));
    opts.scope = path;
    opts.limit = args.limit;
    opts.unit = args.unit;
    opts.intent.clone_from(&args.intent);
    opts.include.clone_from(&args.include);
    opts.exclude.clone_from(&args.exclude);
    opts.knowledge.clone_from(&args.knowledge);
    opts.knowledge_limit = args.knowledge_limit;
    opts.knowledge_auto = args.knowledge_auto;
    opts.security_check = args.security_check;

    let bundle = match context_bundle::build(&opts) {
        Ok(bundle) => bundle,
        Err(err) => {
            eprintln!("lean-ctx pack: {err}");
            std::process::exit(1);
        }
    };

    let report = bundle.report();
    match args.emit {
        Emit::Plain => print!("{report}"),
        Emit::Xml | Emit::Both => {
            if args.emit == Emit::Both {
                eprint!("{report}");
            }
            deliver(&args, &bundle.xml, &report);
        }
    }
    if args.stats {
        eprintln!(
            "files={} chars={} tokens={}",
            bundle.count("full") + bundle.count("signatures"),
            bundle.xml.chars().count(),
            crate::core::tokens::count_tokens(&bundle.xml)
        );
    }
    if !bundle.fits() {
        eprintln!(
            "lean-ctx pack: bundle is {} {} but the limit is {} — raise --limit or narrow the path",
            bundle.size, bundle.unit, bundle.limit
        );
        std::process::exit(1);
    }
}

/// Writes the XML to the file, the clipboard, or stdout. Destinations other
/// than stdout get a one-line confirmation on stderr.
fn deliver(args: &Args, xml: &str, report: &str) {
    let headline = report.lines().next().unwrap_or_default();
    if let Some(output) = &args.output {
        if let Err(err) = std::fs::write(output, xml) {
            eprintln!("lean-ctx pack: cannot write {output}: {err}");
            std::process::exit(1);
        }
        eprintln!("{headline} → {output}");
    }
    if args.copy {
        if crate::core::share::copy_to_clipboard(xml) {
            eprintln!("{headline} → clipboard");
        } else {
            eprintln!("lean-ctx pack: no clipboard tool found; printing instead");
            if args.output.is_none() {
                print!("{xml}");
            }
        }
    }
    if args.output.is_none() && !args.copy {
        print!("{xml}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUBS: &[&str] = &["pr", "create", "list", "export"];

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn bundle_mode_needs_a_bundle_flag_or_an_existing_path() {
        assert!(wants_bundle(&v(&["--limit", "128k"]), SUBS));
        assert!(wants_bundle(&v(&["--limit=128k"]), SUBS));
        assert!(wants_bundle(&v(&["-o", "out.xml"]), SUBS));
        assert!(wants_bundle(&v(&["."]), SUBS));
        assert!(!wants_bundle(&v(&[]), SUBS));
        assert!(!wants_bundle(&v(&["pr", "--base", "main"]), SUBS));
        assert!(!wants_bundle(&v(&["export", "x", "--output=y"]), SUBS));
        assert!(!wants_bundle(&v(&["no-such-path-1885"]), SUBS));
        // A flag value is never mistaken for the path.
        assert!(!wants_bundle(&v(&["--root", ".", "list"]), SUBS));
    }

    #[test]
    fn parses_both_flag_forms_and_lists() {
        let a = parse(&v(&[
            "src",
            "--limit=32k",
            "--unit",
            "tokens",
            "--include",
            "*.rs, *.toml",
            "--ignore=target/**",
            "--with-knowledge=decision,gotcha",
            "--emit",
            "both",
        ]))
        .unwrap();
        assert_eq!(a.path.as_deref(), Some("src"));
        assert_eq!(a.limit, 32_000);
        assert_eq!(a.unit, Unit::Tokens);
        assert_eq!(a.include, ["*.rs", "*.toml"]);
        assert_eq!(a.exclude, ["target/**"]);
        assert_eq!(a.knowledge.unwrap(), ["decision", "gotcha"]);
        assert_eq!(a.emit, Emit::Both);
        assert!(a.security_check);

        let d = parse(&v(&["--with-knowledge"])).unwrap();
        assert_eq!(d.knowledge.unwrap(), DEFAULT_KNOWLEDGE_CATEGORIES);
        assert_eq!(d.limit, context_bundle::DEFAULT_LIMIT);
    }

    #[test]
    fn rejects_unsafe_or_malformed_input() {
        let err = |a: &[&str]| parse(&v(a)).unwrap_err();
        assert!(err(&["--no-security-check"]).contains("--force"));
        assert!(parse(&v(&["--no-security-check", "--force"])).is_ok());
        assert!(err(&["--limit"]).contains("needs a value"));
        assert!(err(&["--limit", "0"]).contains("limit"));
        assert!(err(&["--emit", "json"]).contains("xml, plain or both"));
        assert!(err(&["--bogus"]).contains("unknown flag"));
        assert!(err(&["--copy=yes"]).contains("takes no value"));
        assert!(err(&["a", "b"]).contains("one path at most"));
        assert!(err(&["--emit", "plain", "-o", "x"]).contains("--emit both"));
        assert!(err(&["--with-auto"]).contains("--with-knowledge"));
        assert!(parse(&v(&["-o", "-"])).unwrap().output.is_none());
    }
}
