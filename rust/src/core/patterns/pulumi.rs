//! Pulumi (`pulumi up`/`preview`/`destroy`) output compression.
//!
//! Pulumi prints a per-resource event tree (one row per resource) followed by
//! an `Outputs:` block, a `Resources:` summary and a `Duration:` line. The tree
//! is noise; the outputs (stack exports — real data), the resource counts, the
//! duration and any diagnostics are signal. We keep the latter and drop the
//! tree.

macro_rules! static_regex {
    ($pattern:expr_2021) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

/// Resource-count summary rows: `+ 5 to create`, `~ 2 updated`, `3 unchanged`.
fn summary_re() -> &'static regex::Regex {
    static_regex!(
        r"^[+~\-]?\s*\d+\s+(to create|to update|to delete|to replace|created|updated|deleted|replaced|unchanged|changed)\b"
    )
}

pub fn compress(command: &str, output: &str) -> Option<String> {
    let c = command.trim();
    let sub = c
        .strip_prefix("pulumi")
        .map_or("", str::trim_start)
        .split_whitespace()
        .next()
        .unwrap_or("");
    match sub {
        "up" | "update" | "preview" | "destroy" | "refresh" => Some(compress_update(output)),
        _ => Some(compress_generic(output)),
    }
}

fn compress_update(output: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut in_outputs = false;

    for raw in output.lines() {
        let t = raw.trim();
        if t.is_empty() {
            in_outputs = false;
            continue;
        }
        let tl = t.to_ascii_lowercase();

        if t == "Outputs:" {
            in_outputs = true;
            kept.push(t.to_string());
            continue;
        }
        if in_outputs {
            // Output kv pairs are indented; the block ends at a blank line or a
            // following section header (handled above / below).
            if t == "Resources:" || tl.starts_with("duration:") {
                in_outputs = false;
            } else {
                kept.push(format!("  {t}"));
                continue;
            }
        }

        if t == "Resources:" || tl.starts_with("duration:") {
            kept.push(t.to_string());
            continue;
        }
        if summary_re().is_match(t) {
            kept.push(t.to_string());
            continue;
        }
        if tl.starts_with("updating (")
            || tl.starts_with("previewing update (")
            || tl.starts_with("destroying (")
        {
            kept.push(t.to_string());
            continue;
        }
        if tl.contains("error:")
            || tl.starts_with("error")
            || tl.starts_with("diagnostics:")
            || tl.contains("failed")
            || tl.contains("panic:")
        {
            kept.push(t.to_string());
        }
    }

    if kept.is_empty() {
        return compress_generic(output);
    }
    kept.join("\n")
}

fn compress_generic(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return "pulumi: ok".to_string();
    }
    let max = 15;
    if lines.len() <= max {
        return lines.join("\n");
    }
    format!(
        "{}\n... (+{} lines)",
        lines[..max].join("\n"),
        lines.len() - max
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const UP: &str = "Updating (dev):\n     Type                 Name        Status\n +   pulumi:pulumi:Stack  proj-dev    created\n +   ├─ aws:s3:Bucket     my-bucket   created\n ~   └─ aws:s3:BucketPolicy pol       updated\n\nOutputs:\n    bucketName: \"my-bucket-abc123\"\n    url       : \"https://my-bucket.s3.amazonaws.com\"\n\nResources:\n    + 5 created\n    ~ 2 updated\n    10 unchanged\n\nDuration: 35s\n";

    #[test]
    fn keeps_outputs_summary_duration_drops_tree() {
        let r = compress("pulumi up", UP).unwrap();
        assert!(
            r.contains("url       : \"https://my-bucket"),
            "keeps outputs: {r}"
        );
        assert!(r.contains("+ 5 created"), "keeps summary: {r}");
        assert!(r.contains("Duration: 35s"), "keeps duration: {r}");
        assert!(
            !r.contains("pulumi:pulumi:Stack"),
            "drops resource tree: {r}"
        );
        assert!(!r.contains("aws:s3:Bucket "), "drops resource tree: {r}");
    }

    #[test]
    fn keeps_errors() {
        let out = "Updating (dev):\n +   aws:s3:Bucket b created\nDiagnostics:\n  aws:s3:Bucket (b):\n    error: creating S3 Bucket: BucketAlreadyExists";
        let r = compress("pulumi up", out).unwrap();
        assert!(r.contains("error: creating S3 Bucket"), "{r}");
    }

    #[test]
    fn shorter_than_input() {
        let r = compress("pulumi up", UP).unwrap();
        assert!(r.len() < UP.len(), "compressed shorter");
    }
}
