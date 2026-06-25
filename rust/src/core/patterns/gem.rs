//! `RubyGems` (`gem`) output compression.
//!
//! `gem install`/`update` interleaves the few lines that matter
//! (`Successfully installed …`, gem count) with documentation/fetch noise
//! (`Fetching`, `Parsing documentation`, `Installing ri/rdoc`).
//!
//! NOTE: `gem list` is classified **Verbatim** by the output policy
//! (`is_package_manager_info`, alongside `npm list`/`pip list`/`cargo tree`) —
//! installed-package inventories are reference data the agent reads in full, so
//! they never reach this compressor. The `name (versions)` count+cap path here
//! therefore serves `gem search` (remote listing noise), not `gem list`.

use crate::core::compressor::strip_ansi;

#[must_use]
pub fn compress(cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("gem: ok".to_string());
    }
    if cmd.contains(" list") || cmd.contains(" search") {
        return Some(compress_list(trimmed));
    }
    Some(compress_install(trimmed))
}

fn compress_list(output: &str) -> String {
    let rows: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("***") && l.contains('('))
        .collect();
    if rows.is_empty() {
        return fallback(output);
    }
    let n = rows.len();
    let cap = 30.min(n);
    let mut s = format!("gem: {n} gem(s)\n{}", rows[..cap].join("\n"));
    if n > cap {
        s.push_str(&format!("\n... +{} more", n - cap));
    }
    s
}

fn compress_install(output: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    for raw in output.lines() {
        let line = strip_ansi(raw);
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let l = t.to_ascii_lowercase();
        if l.starts_with("fetching")
            || l.starts_with("parsing documentation")
            || l.starts_with("installing ri")
            || l.starts_with("installing rdoc")
            || l.starts_with("done installing documentation")
            || l.starts_with("building native extensions")
        {
            continue;
        }
        if l.starts_with("successfully installed")
            || l.starts_with("successfully uninstalled")
            || l.contains("gems installed")
            || l.contains("gem installed")
            || l.contains("error")
            || l.contains("could not")
            || l.contains("conflict")
        {
            kept.push(t.to_string());
        }
    }
    if kept.is_empty() {
        return "gem: ok".to_string();
    }
    kept.join("\n")
}

fn fallback(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let n = lines.len().min(10);
    let mut s = lines[..n].join("\n");
    if lines.len() > n {
        s.push_str(&format!("\n... (+{} lines)", lines.len() - n));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_keeps_success_drops_docs() {
        let out = "Fetching rails-7.1.0.gem\nFetching activesupport-7.1.0.gem\nSuccessfully installed activesupport-7.1.0\nSuccessfully installed rails-7.1.0\nParsing documentation for rails-7.1.0\nInstalling ri documentation for rails-7.1.0\nDone installing documentation for rails after 3 seconds\n2 gems installed";
        let r = compress("gem install rails", out).unwrap();
        assert!(r.contains("Successfully installed rails-7.1.0"), "{r}");
        assert!(r.contains("2 gems installed"), "{r}");
        assert!(!r.contains("Fetching"), "drops fetch noise: {r}");
        assert!(!r.contains("Parsing documentation"), "drops doc noise: {r}");
    }

    #[test]
    fn search_counts_and_caps() {
        // `gem search` (remote) is the reachable list path — `gem list` is
        // intercepted upstream as Verbatim and never lands here.
        let out = "*** REMOTE GEMS ***\n\nbundler (2.5.0)\nrails (7.1.0)\nrake (13.1.0)";
        let r = compress("gem search rails", out).unwrap();
        assert!(r.contains("gem: 3 gem(s)"), "{r}");
        assert!(r.contains("rails (7.1.0)"), "{r}");
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(compress("gem install x", "").unwrap(), "gem: ok");
    }
}
