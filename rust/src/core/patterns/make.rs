use std::collections::HashMap;

macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn compiler_invocation_re() -> &'static regex::Regex {
    static_regex!(
        r"(?i)^(/[^\s]+/)?(gcc|g\+\+|c\+\+|cc|clang\+\+|clang|ld\.bfd|ld\.gold|ld|ar|rustc|javac|zig|nvcc|emcc|icc|icpc)\s"
    )
}

fn is_compiler_echo_line(line: &str) -> bool {
    compiler_invocation_re().is_match(line.trim_start())
}

fn is_warning_line(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    l.contains("warning:") || l.contains(" warning ") || l.contains(": warning ")
}

fn is_error_line(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    l.contains("error:")
        || l.contains("error ")
        || l.contains("*** ")
        || l.contains("fatal error")
        || l.contains("undefined reference")
        || l.contains("undefined symbol")
        || l.contains("make: ***")
        || l.contains("gmake: ***")
        || l.contains("ninja: error")
}

fn is_make_meta_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("make[") || t.starts_with("gmake[") || t.starts_with("make:")
}

pub fn compress(command: &str, output: &str) -> Option<String> {
    let cl = command.trim();
    let cl = cl.to_ascii_lowercase();
    if cl != "make" && !cl.starts_with("make ") {
        return None;
    }
    Some(compress_make_output(output))
}

fn compress_make_output(output: &str) -> String {
    let mut kept_non_warning = Vec::new();
    let mut warning_counts: HashMap<String, u32> = HashMap::new();
    let mut last_significant: Option<String> = None;

    for line in output.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        if is_compiler_echo_line(trimmed) {
            continue;
        }

        last_significant = Some(trimmed.to_string());

        if is_warning_line(trimmed) {
            let key = trimmed.trim().to_string();
            *warning_counts.entry(key).or_insert(0) += 1;
            continue;
        }

        let tl = trimmed.to_ascii_lowercase();
        if tl.contains("nothing to be done")
            || tl.contains("is up to date.")
            || is_error_line(trimmed)
            || is_make_meta_line(trimmed)
        {
            kept_non_warning.push(trimmed.to_string());
            continue;
        }

        if trimmed.starts_with('@') {
            continue;
        }

        if trimmed.contains("Entering directory") || trimmed.contains("Leaving directory") {
            kept_non_warning.push(trimmed.to_string());
        }
    }

    if let Some(ref last) = last_significant {
        if !kept_non_warning.iter().any(|k| k == last) {
            kept_non_warning.push(format!("result: {last}"));
        }
    }

    let mut sections: Vec<String> = Vec::new();

    if !warning_counts.is_empty() {
        let mut wlines: Vec<String> = warning_counts
            .into_iter()
            .map(|(text, n)| {
                if n > 1 {
                    format!("{text} (x{n})")
                } else {
                    text
                }
            })
            .collect();
        wlines.sort();
        sections.push(format!("warnings:\n{}", wlines.join("\n")));
    }

    if !kept_non_warning.is_empty() {
        sections.push(kept_non_warning.join("\n"));
    }

    if sections.is_empty() {
        "make (no warnings/errors/meta)".to_string()
    } else {
        sections.join("\n\n")
    }
}
