// SPDX-License-Identifier: Apache-2.0
//! The files a curl or wget call writes, split out of `self_config`.

/// A word that names a path relative to the working directory: not an
/// option, not absolute or home/variable-anchored, not an assignment, and
/// never a URL (`http://127.0.0.1:9/` is not a file in the directory).
pub(super) fn is_relative_operand(word: &str) -> bool {
    !word.starts_with(['-', '/', '~', '$']) && !word.contains('=') && !word.contains("://")
}

/// curl short options that take a value; the first one in a cluster takes
/// the rest of the token, or the next token when nothing follows it.
const CURL_VALUE_SHORTS: &str = "AbcCdDeEFHKmoPQrTtuUwxXyYz";

/// wget short options that take a value, read the same way.
const WGET_VALUE_SHORTS: &str = "aABDeiIlOoPQRtTUwX";

/// The files a curl or wget call writes, relative or not: curl's output
/// options (`-o`, `-D`, `-c`, `--trace`, `--hsts`, …, and
/// `-O`/`--remote-name` into `--output-dir` or the working directory,
/// written `.`), wget's document (`-O`, else `-P` or the working directory),
/// logs (`-o`, `-a`) and the other files it saves (`--save-cookies`, …).
/// The URL and the values of other options are never targets. One list for
/// the shell gate and for ctx_shell's download guard.
pub fn download_targets(base: &str, args: &[String]) -> Vec<String> {
    if base == "wget" {
        return wget_targets(args);
    }
    // `--hsts` and `--alt-svc` read their cache file and write it back
    // (#2212).
    const CURL_OUTPUT_LONGS: &[&str] = &[
        "--dump-header",
        "--cookie-jar",
        "--trace",
        "--trace-ascii",
        "--stderr",
        "--libcurl",
        "--etag-save",
        "--hsts",
        "--alt-svc",
    ];
    let mut targets = Vec::new();
    let mut outputs = Vec::new();
    let mut output_dir: Option<String> = None;
    let mut remote_name = false;
    let mut i = 0;
    while i < args.len() {
        let tok = args[i].as_str();
        let next = args.get(i + 1).cloned();
        let mut takes_next = false;
        if tok == "--" {
            break;
        } else if matches!(tok, "--remote-name" | "--remote-name-all") {
            remote_name = true;
        } else if let Some((flag, value)) = tok.split_once('=').filter(|_| tok.starts_with("--")) {
            if flag == "--output-dir" {
                output_dir = Some(value.to_string());
            } else if flag == "--output" {
                outputs.push(value.to_string());
            } else if CURL_OUTPUT_LONGS.contains(&flag) {
                targets.push(value.to_string());
            }
        } else if tok == "--output-dir" {
            takes_next = true;
            output_dir = next;
        } else if tok == "--output" {
            takes_next = true;
            outputs.extend(next);
        } else if CURL_OUTPUT_LONGS.contains(&tok) {
            takes_next = true;
            targets.extend(next);
        } else if let Some(cluster) = tok.strip_prefix('-').filter(|c| !c.starts_with('-')) {
            for (pos, letter) in cluster.char_indices() {
                if letter == 'O' {
                    remote_name = true;
                    continue;
                }
                if !CURL_VALUE_SHORTS.contains(letter) {
                    continue;
                }
                let rest = &cluster[pos + 1..];
                let value = if rest.is_empty() {
                    takes_next = true;
                    next.clone()
                } else {
                    Some(rest.to_string())
                };
                match letter {
                    'o' => outputs.extend(value),
                    'D' | 'c' => targets.extend(value),
                    _ => {}
                }
                break;
            }
        }
        i += if takes_next { 2 } else { 1 };
    }
    outputs.retain(|t| t != "-");
    // curl writes `-o`/`--output` files inside `--output-dir` too: it joins
    // the two unconditionally (`--output-dir lean-ctx -o config.toml`
    // writes `lean-ctx/config.toml`).
    if let Some(dir) = output_dir.as_deref() {
        let dir = dir.trim_end_matches('/');
        for file in &mut outputs {
            *file = format!("{dir}/{file}");
        }
    }
    targets.extend(outputs);
    if remote_name {
        targets.push(output_dir.unwrap_or_else(|| ".".to_string()));
    }
    targets.retain(|t| t != "-");
    targets
}

/// wget long options that name a file it writes besides the document and
/// the log (#2212). `--warc-file` names the stem of `<file>.warc[.gz]`; the
/// stem is judged, which lands in the same directory.
const WGET_SAVED_FILES: &[&str] = &[
    "--save-cookies",
    "--hsts-file",
    "--warc-file",
    "--rejected-log",
];

/// Every file a wget call writes: the document (`-O`/`--output-document`,
/// else the URL's name in `-P`/`--directory-prefix` or the working
/// directory, written `.`), a log (`-o`/`-a`) and the files of
/// `WGET_SAVED_FILES`. `-O -` and `--spider` write no document.
fn wget_targets(args: &[String]) -> Vec<String> {
    let mut document: Option<String> = None;
    let mut prefix: Option<String> = None;
    let mut logs = Vec::new();
    let mut spider = false;
    let mut i = 0;
    while i < args.len() {
        let tok = args[i].as_str();
        let next = || args.get(i + 1).cloned();
        let mut takes_next = false;
        if tok == "--spider" {
            spider = true;
        } else if let Some((flag, value)) = tok.split_once('=').filter(|_| tok.starts_with("--")) {
            match flag {
                "--output-document" => document = Some(value.to_string()),
                "--directory-prefix" => prefix = Some(value.to_string()),
                "--output-file" | "--append-output" => logs.push(value.to_string()),
                _ if WGET_SAVED_FILES.contains(&flag) => logs.push(value.to_string()),
                _ => {}
            }
        } else if matches!(
            tok,
            "--output-document" | "--directory-prefix" | "--output-file" | "--append-output"
        ) || WGET_SAVED_FILES.contains(&tok)
        {
            takes_next = true;
            let value = next();
            match tok {
                "--output-document" => document = value,
                "--directory-prefix" => prefix = value,
                _ => logs.extend(value),
            }
        } else if let Some(cluster) = tok.strip_prefix('-').filter(|c| !c.starts_with('-')) {
            // The first value-taking letter takes the rest of the token, or
            // the next token when nothing follows it (`-qO-`, `-U foo`).
            if let Some(pos) = cluster.find(|c: char| WGET_VALUE_SHORTS.contains(c)) {
                let rest = &cluster[pos + 1..];
                let value = if rest.is_empty() {
                    takes_next = true;
                    next()
                } else {
                    Some(rest.to_string())
                };
                match &cluster[pos..=pos] {
                    "O" => document = value,
                    "P" => prefix = value,
                    "o" | "a" => logs.extend(value),
                    _ => {}
                }
            }
        }
        i += if takes_next { 2 } else { 1 };
    }
    let mut targets = logs;
    match document.as_deref() {
        Some("-") => {}
        Some(file) => targets.push(file.to_string()),
        None if !spider => targets.push(prefix.unwrap_or_else(|| ".".to_string())),
        None => {}
    }
    targets
}
