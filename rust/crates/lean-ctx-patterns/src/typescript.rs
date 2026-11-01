// SPDX-License-Identifier: Apache-2.0

macro_rules! static_regex {
    ($pattern:expr_2021) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

/// `file(line,col): error TSnnnn: msg` (plain) or `file:line:col - error TSnnnn: msg` (`--pretty`).
fn tsc_error_re() -> &'static regex::Regex {
    static_regex!(r"^(?:(\S+)\((\d+),(\d+)\):|(\S+?):(\d+):(\d+) -) error (TS\d+): (.+)$")
}
fn error_count_re() -> &'static regex::Regex {
    static_regex!(r"Found (\d+) errors?")
}

/// Groups diagnostics by file. The full path, `line:col`, error code and the
/// complete message (plus tsc's indented elaboration lines) are kept — the
/// message tail is what names the offending type, so it is never cut (#1894).
pub fn compress(output: &str) -> Option<String> {
    let mut files: Vec<(String, Vec<String>)> = Vec::new();
    let mut total_errors = 0u32;
    let mut diagnostics = 0u32;
    let mut in_diagnostic = false;

    for line in output.lines() {
        if let Some(caps) = tsc_error_re().captures(line) {
            let (file, line_no, col) = match caps.get(1) {
                Some(f) => (f.as_str(), &caps[2], &caps[3]),
                None => (&caps[4], &caps[5], &caps[6]),
            };
            let entry = format!("  {line_no}:{col} {} {}", &caps[7], caps[8].trim());
            match files.iter_mut().find(|(f, _)| f == file) {
                Some((_, list)) => list.push(entry),
                None => files.push((file.to_string(), vec![entry])),
            }
            diagnostics += 1;
            in_diagnostic = true;
            continue;
        }
        if let Some(caps) = error_count_re().captures(line) {
            total_errors = caps[1].parse().unwrap_or(0);
            in_diagnostic = false;
            continue;
        }
        let is_elaboration = line.starts_with("  ") && !line.trim().is_empty();
        if in_diagnostic
            && is_elaboration
            && let Some((_, list)) = files.last_mut()
        {
            list.push(format!("    {}", line.trim()));
        } else {
            in_diagnostic = false;
        }
    }

    if files.is_empty() {
        return None;
    }

    let total = if total_errors > 0 {
        total_errors
    } else {
        diagnostics
    };
    let mut result = vec![format!("{total} errors in {} files:", files.len())];
    for (file, entries) in files {
        result.push(file);
        result.extend(entries);
    }
    Some(result.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::compress;

    #[test]
    fn keeps_full_path_position_and_whole_message() {
        let out = "src/a/index.ts(3,7): error TS2322: Type '{ id: string; }' is not assignable to type 'UserRecordWithPermissions'.\n  Property 'permissions' is missing in type '{ id: string; }'.\nsrc/b/index.ts(10,1): error TS2304: Cannot find name 'foo'.\n\nFound 2 errors in 2 files.\n";
        assert_eq!(
            compress(out).unwrap(),
            "2 errors in 2 files:\nsrc/a/index.ts\n  3:7 TS2322 Type '{ id: string; }' is not assignable to type 'UserRecordWithPermissions'.\n    Property 'permissions' is missing in type '{ id: string; }'.\nsrc/b/index.ts\n  10:1 TS2304 Cannot find name 'foo'."
        );
    }

    #[test]
    fn pretty_format_and_grouping() {
        let out = "src/x.ts:1:2 - error TS1005: ';' expected.\n\n1 const a = 1 const b\n\nsrc/x.ts:4:1 - error TS2304: Cannot find name 'c'.\n\nFound 2 errors in the same file, starting at: src/x.ts:1\n";
        assert_eq!(
            compress(out).unwrap(),
            "2 errors in 1 files:\nsrc/x.ts\n  1:2 TS1005 ';' expected.\n  4:1 TS2304 Cannot find name 'c'."
        );
    }

    #[test]
    fn no_diagnostics_is_not_handled() {
        assert!(compress("Version 5.4.5\n").is_none());
    }
}
