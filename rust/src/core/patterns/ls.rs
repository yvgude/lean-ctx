#[must_use]
pub fn compress(output: &str) -> Option<String> {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() < 5 {
        return None;
    }

    let is_long = lines.iter().any(|l| {
        l.starts_with('-') || l.starts_with('d') || l.starts_with('l') || l.starts_with("total ")
    });

    if is_long {
        compress_long(output)
    } else {
        compress_short(output)
    }
}

fn compress_long(output: &str) -> Option<String> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for line in output.lines() {
        if line.starts_with("total ") || line.trim().is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 9 {
            continue;
        }

        let name = parts[8..].join(" ");

        if name == "." || name == ".." {
            continue;
        }

        if line.starts_with('d') {
            dirs.push(format!("{name}/"));
        } else {
            let size = format_size(parts[4]);
            files.push(format!("{name}  {size}"));
        }
    }

    if dirs.is_empty() && files.is_empty() {
        return None;
    }

    let mut result = String::new();
    for d in &dirs {
        result.push_str(d);
        result.push('\n');
    }
    for f in &files {
        result.push_str(f);
        result.push('\n');
    }

    result.push_str(&format!("\n{} files, {} dirs", files.len(), dirs.len()));

    Some(result)
}

fn compress_short(output: &str) -> Option<String> {
    let items: Vec<&str> = output
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .collect();

    if items.len() < 10 {
        return None;
    }

    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for item in &items {
        if item.ends_with('/') {
            dirs.push(*item);
        } else {
            files.push(*item);
        }
    }

    let mut result = String::new();
    for d in &dirs {
        result.push_str(d);
        result.push('\n');
    }

    let mut line_buf = String::new();
    for f in &files {
        if line_buf.len() + f.len() + 2 > 70 {
            result.push_str(&line_buf);
            result.push('\n');
            line_buf.clear();
        }
        if !line_buf.is_empty() {
            line_buf.push_str("  ");
        }
        line_buf.push_str(f);
    }
    if !line_buf.is_empty() {
        result.push_str(&line_buf);
        result.push('\n');
    }

    result.push_str(&format!("\n{} items", dirs.len() + files.len()));

    Some(result)
}

fn format_size(size_str: &str) -> String {
    let last = size_str.as_bytes().last().copied().unwrap_or(b'0');
    if matches!(last, b'K' | b'M' | b'G' | b'T') {
        return size_str.to_string();
    }
    let bytes: u64 = size_str.parse().unwrap_or(0);
    if bytes >= 1_048_576 {
        format!("{:.1}M", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1}K", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_size_raw_small() {
        assert_eq!(format_size("512"), "512B");
    }

    #[test]
    fn format_size_raw_kb() {
        assert_eq!(format_size("4096"), "4.0K");
    }

    #[test]
    fn format_size_raw_mb() {
        assert_eq!(format_size("1048576"), "1.0M");
    }

    #[test]
    fn format_size_human_k_passthrough() {
        assert_eq!(format_size("4.0K"), "4.0K");
    }

    #[test]
    fn format_size_human_m_passthrough() {
        assert_eq!(format_size("1.2M"), "1.2M");
    }

    #[test]
    fn format_size_human_g_passthrough() {
        assert_eq!(format_size("2.5G"), "2.5G");
    }

    #[test]
    fn format_size_zero_and_empty() {
        assert_eq!(format_size("0"), "0B");
        assert_eq!(format_size(""), "0B");
    }

    #[test]
    fn format_size_integer_k_passthrough() {
        assert_eq!(format_size("15K"), "15K");
    }

    #[test]
    fn compress_long_ls_l_raw_bytes() {
        let output = "total 32\n\
            drwxr-xr-x  5 user staff   160 May 20 10:00 src\n\
            drwxr-xr-x  3 user staff    96 May 20 10:00 tests\n\
            -rw-r--r--  1 user staff  4096 May 20 10:00 Cargo.toml\n\
            -rw-r--r--  1 user staff 12288 May 20 10:00 Cargo.lock\n\
            -rw-r--r--  1 user staff   512 May 20 10:00 README.md\n\
            -rw-r--r--  1 user staff   100 May 20 10:00 .gitignore\n\
            -rw-r--r--  1 user staff    42 May 20 10:00 .env\n";
        let result = compress(output).expect("should compress");
        assert!(result.contains("4.0K"), "4096 should become 4.0K: {result}");
        assert!(
            result.contains("12.0K"),
            "12288 should become 12.0K: {result}"
        );
        assert!(result.contains("512B"), "512 should become 512B: {result}");
        assert!(
            result.contains("src/"),
            "dirs should have trailing /: {result}"
        );
    }

    #[test]
    fn compress_long_ls_lah_human_readable() {
        let output = "total 32K\n\
            drwxr-xr-x  5 user staff  160 May 20 10:00 src\n\
            drwxr-xr-x  3 user staff   96 May 20 10:00 tests\n\
            -rw-r--r--  1 user staff 4.0K May 20 10:00 Cargo.toml\n\
            -rw-r--r--  1 user staff  12K May 20 10:00 Cargo.lock\n\
            -rw-r--r--  1 user staff 1.2M May 20 10:00 big-file.bin\n\
            -rw-r--r--  1 user staff  512 May 20 10:00 README.md\n\
            -rw-r--r--  1 user staff  100 May 20 10:00 .gitignore\n";
        let result = compress(output).expect("should compress");
        assert!(
            result.contains("4.0K"),
            "human 4.0K should pass through: {result}"
        );
        assert!(
            result.contains("12K"),
            "human 12K should pass through: {result}"
        );
        assert!(
            result.contains("1.2M"),
            "human 1.2M should pass through: {result}"
        );
        assert!(
            !result.contains("  0B"),
            "should NOT show 0B for human-readable sizes: {result}"
        );
    }

    #[test]
    fn compress_long_ls_lh_same_as_lah() {
        let output = "total 16K\n\
            drwxr-xr-x  2 user staff   64 May 20 10:00 docs\n\
            -rw-r--r--  1 user staff 2.5G May 20 10:00 database.db\n\
            -rw-r--r--  1 user staff 330K May 20 10:00 image.png\n\
            -rw-r--r--  1 user staff  15T May 20 10:00 huge.tar\n\
            -rw-r--r--  1 user staff   42 May 20 10:00 tiny.txt\n\
            -rw-r--r--  1 user staff    0 May 20 10:00 empty.log\n";
        let result = compress(output).expect("should compress");
        assert!(result.contains("2.5G"), "G suffix: {result}");
        assert!(result.contains("15T"), "T suffix: {result}");
    }

    #[test]
    fn compress_long_mixed_dirs_and_files() {
        let output = "total 8\n\
            drwxr-xr-x  2 user staff  64 May 20 10:00 .git\n\
            drwxr-xr-x  2 user staff  64 May 20 10:00 node_modules\n\
            drwxr-xr-x  2 user staff  64 May 20 10:00 src\n\
            -rw-r--r--  1 user staff 256 May 20 10:00 package.json\n\
            -rw-r--r--  1 user staff 100 May 20 10:00 .env\n";
        let result = compress(output).expect("should compress");
        assert!(result.contains(".git/"));
        assert!(result.contains(".env"));
        assert!(result.contains("3 dirs"));
        assert!(result.contains("2 files"));
    }

    #[test]
    fn compress_long_dotfiles_preserved() {
        let output = "total 4\n\
            -rw-r--r--  1 user staff 100 May 20 10:00 .env\n\
            -rw-r--r--  1 user staff 200 May 20 10:00 .gitignore\n\
            -rw-r--r--  1 user staff 300 May 20 10:00 .dockerignore\n\
            -rw-r--r--  1 user staff 400 May 20 10:00 .eslintrc\n\
            -rw-r--r--  1 user staff 500 May 20 10:00 .prettierrc\n";
        let result = compress(output).expect("should compress");
        assert!(result.contains(".env"), "dotfiles must appear: {result}");
        assert!(result.contains(".gitignore"));
    }
}
