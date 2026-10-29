// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx pack [<path>] --limit 128k` — one budgeted XML bundle for chat
//! products (#1885). The engine lives in `core::context_bundle`; this module
//! only parses flags and routes the output.

use std::path::{Path, PathBuf};

use crate::cli::common::{CliToolRun, Printable};
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
            "--no-security-check skips the bundle's own secret scan; add --force to confirm".into(),
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

    // One CLI tool run (`CliToolRun`): the build reads its sources inside the
    // request scope and the source capture, and the text that leaves — the
    // XML, or the report for `--emit plain` — passes the release point. A
    // bundle is a mixed output (tree, summary, task and knowledge text around
    // the files), so it is never recorded as a delivery of its sources.
    //
    // The destination is decided BEFORE the release: the run prepares its
    // receipt for the channel the text really leaves on (stdout, the file,
    // the clipboard) and for exactly the bytes that channel gets. A second
    // destination gets a run of its own (`deliver`).
    let root_text = opts.root.to_string_lossy().into_owned();
    let destination = Destination::first(&args);
    let run = destination.run(&root_text);
    let (built, released) = run.run_checked(|| match context_bundle::build(&opts) {
        Ok(bundle) => {
            let text = match args.emit {
                Emit::Plain => bundle.report(),
                Emit::Xml | Emit::Both => bundle.xml.clone(),
            };
            let printable = Printable {
                text: destination.text(&text),
                source_only: false,
                failed: !bundle.fits(),
            };
            (Ok(bundle), Some(printable))
        }
        Err(err) => (Err(err), None),
    });
    let bundle = match built {
        Ok(bundle) => bundle,
        Err(err) => {
            eprintln!("lean-ctx pack: {err}");
            std::process::exit(1);
        }
    };
    let (released, refused) = released.unwrap_or_default();
    // A refused release is a notice, not a bundle or a report: it goes to
    // stderr, and nothing is written to stdout, a file or the clipboard.
    if refused {
        crate::cli::common::exit_withheld(&released);
    }

    match args.emit {
        Emit::Plain => crate::cli::common::print_released(&released),
        Emit::Xml | Emit::Both => {
            // The report shown NEXT to the bundle (stderr) names files, gives
            // reasons and quotes the task: it leaves through the release
            // point too, as human-facing output prepared for stderr. Only
            // what is written is released: the whole report for `both`, and
            // otherwise just the confirmation lines `deliver` writes.
            let report = bundle.report();
            if args.emit == Emit::Both {
                eprint_human(&root_text, &report);
            }
            let headline = report.lines().next().unwrap_or_default();
            let again = Again {
                root: &root_text,
                xml: &bundle.xml,
                failed: !bundle.fits(),
            };
            deliver(&args, &opts.root, released, &again, headline);
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

fn cannot_write(output: &str, err: &str) -> ! {
    eprintln!("lean-ctx pack: cannot write {output}: {err}");
    std::process::exit(1)
}

/// Write one text of this command (the allocation report, a confirmation
/// line) to stderr: released and redacted like every CLI tool text, as
/// human-facing output (`CliToolRun::human_facing` — no lineage record),
/// prepared for stderr and written right after by the one observed stderr
/// writer, so its receipt binds exactly the bytes that leave (G08). `text`
/// is taken without its final newline; the writer adds it. When the release
/// point refuses the text, its content-free notice is written in its place.
fn eprint_human(root: &str, text: &str) {
    let run = CliToolRun::new("ctx_pack", root, None)
        .human_facing()
        .to_stderr();
    let ((), released) = run.run(|| {
        let printable = Printable {
            text: text.strip_suffix('\n').unwrap_or(text).to_string(),
            source_only: false,
            failed: false,
        };
        ((), Some(printable))
    });
    crate::cli::common::eprint_released(&released.unwrap_or_default(), None);
}

/// Where a bundle text leaves. A run prepares its receipt for ONE of them.
#[derive(Clone, Copy)]
enum Destination {
    Stdout,
    File,
    Clipboard,
}

impl Destination {
    /// The destination the command writes first — the one the build's own
    /// run is prepared for: `--emit plain` prints its report; otherwise the
    /// file comes before the clipboard, and stdout is used when neither was
    /// asked for. The order is the order `deliver` writes in.
    fn first(args: &Args) -> Self {
        if args.emit == Emit::Plain {
            Self::Stdout
        } else if args.output.is_some() {
            Self::File
        } else if args.copy {
            Self::Clipboard
        } else {
            Self::Stdout
        }
    }

    /// A `ctx_pack` run prepared for this destination.
    fn run(self, root: &str) -> CliToolRun<'_> {
        let run = CliToolRun::new("ctx_pack", root, None);
        match self {
            Self::Stdout => run,
            Self::File => run.to_file(),
            Self::Clipboard => run.to_clipboard(),
        }
    }

    /// `text` as this destination's run takes it. The stdout writer adds the
    /// final newline itself, so stdout takes the text without it; a file and
    /// the clipboard get exactly the released text, so they take it WITH its
    /// one final newline — the document that was always written there.
    fn text(self, text: &str) -> String {
        let body = text.strip_suffix('\n').unwrap_or(text);
        match self {
            Self::Stdout => body.to_string(),
            Self::File | Self::Clipboard => format!("{body}\n"),
        }
    }
}

/// What a further delivery of the same bundle needs: a run has one channel,
/// so every destination after the first releases the bundle again in a run
/// of its own, prepared for that destination.
struct Again<'a> {
    root: &'a str,
    xml: &'a str,
    failed: bool,
}

impl Again<'_> {
    /// The bundle released for `destination`. Nothing is read again: the
    /// text is the one the build produced. A refusal ends the command like
    /// the refusal of the first release does.
    fn released_for(&self, destination: Destination) -> String {
        let run = destination.run(self.root);
        let ((), released) = run.run_checked(|| {
            let printable = Printable {
                text: destination.text(self.xml),
                source_only: false,
                failed: self.failed,
            };
            ((), Some(printable))
        });
        let (released, refused) = released.unwrap_or_default();
        if refused {
            crate::cli::common::exit_withheld(&released);
        }
        released
    }
}

/// Hands the RELEASED bundle to the file, the clipboard, or stdout.
/// `released` is the text of the build's own run, prepared for the first
/// destination written here (`Destination::first`); every further
/// destination — the clipboard after a file, stdout after a failed copy —
/// is prepared by a run of its own right before it is written (`again`), so
/// each hand-over is observed on the channel it was prepared for (G08).
/// Destinations other than stdout get a one-line confirmation on stderr,
/// headed by `headline` (the first line of the allocation report); each
/// confirmation line is released as the line that is written.
///
/// - file: only inside the project root — the export rule
///   (`context_bundle::export::destination`); confirmed after the write
///   succeeded;
/// - clipboard: a human-facing local output of the released text. No rule of
///   its own exists for clipboard writes; it gets what stdout would get.
///   Confirmed after the hand-off succeeded; a failed copy confirms nothing;
/// - stdout: the text of a stdout-prepared run, printed unchanged.
fn deliver(args: &Args, root: &Path, released: String, again: &Again<'_>, headline: &str) {
    let root_text = again.root;
    let mut first = Some(released);
    let mut text_for = |destination: Destination| {
        first
            .take()
            .unwrap_or_else(|| again.released_for(destination))
    };
    if let Some(output) = &args.output {
        let candidate = PathBuf::from(output);
        let candidate = if candidate.is_absolute() {
            candidate
        } else {
            std::env::current_dir().unwrap_or_default().join(candidate)
        };
        let path = match context_bundle::export::destination("ctx_pack", root, &candidate) {
            Ok(path) => path,
            Err(err) => cannot_write(output, &err),
        };
        let document = text_for(Destination::File);
        if let Err(err) = std::fs::write(path, &document) {
            cannot_write(output, &err.to_string());
        }
        crate::cli::common::confirm_written_to_file(document.as_bytes());
        eprint_human(root_text, &format!("{headline} → {output}"));
    }
    if args.copy {
        let document = text_for(Destination::Clipboard);
        if crate::core::share::copy_to_clipboard(&document) {
            crate::cli::common::confirm_copied_to_clipboard(&document);
            eprint_human(root_text, &format!("{headline} → clipboard"));
        } else {
            // The clipboard run's receipt stays unconfirmed: nothing left on
            // that channel. The fallback is a stdout delivery of its own.
            eprintln!("lean-ctx pack: no clipboard tool found; printing instead");
            if args.output.is_none() {
                crate::cli::common::print_released(&text_for(Destination::Stdout));
            }
        }
    }
    if args.output.is_none() && !args.copy {
        crate::cli::common::print_released(&text_for(Destination::Stdout));
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
