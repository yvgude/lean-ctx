pub fn compress_ps(output: &str) -> Option<String> {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() < 2 {
        return None;
    }

    let header = lines[0];
    let procs: Vec<&str> = lines[1..]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .copied()
        .collect();
    let total = procs.len();

    if total <= 10 {
        return None;
    }

    let mut high_cpu: Vec<&str> = Vec::new();
    let mut high_mem: Vec<&str> = Vec::new();

    for &line in &procs {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 4 {
            let cpu: f64 = cols.get(2).and_then(|s| s.parse().ok()).unwrap_or(0.0);
            let mem: f64 = cols.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.0);
            if cpu > 1.0 {
                high_cpu.push(line);
            }
            if mem > 1.0 {
                high_mem.push(line);
            }
        }
    }

    let mut out = format!("ps: {total} processes\n{header}\n");

    if !high_cpu.is_empty() {
        out.push_str(&format!("--- high CPU ({}) ---\n", high_cpu.len()));
        for l in high_cpu.iter().take(15) {
            out.push_str(l);
            out.push('\n');
        }
    }
    if !high_mem.is_empty() {
        out.push_str(&format!("--- high MEM ({}) ---\n", high_mem.len()));
        for l in high_mem.iter().take(15) {
            out.push_str(l);
            out.push('\n');
        }
    }

    Some(out.trim_end().to_string())
}

/// df output is safety-critical (disk usage, root filesystem) and typically
/// small (<30 lines). Verbatim passthrough prevents hiding critical info.
pub fn compress_df(output: &str) -> Option<String> {
    Some(output.to_string())
}

pub fn compress_du(output: &str) -> Option<String> {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();

    if lines.len() <= 10 {
        return None;
    }

    let mut parsed: Vec<(u64, &str)> = lines
        .iter()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(2, |c: char| c.is_whitespace()).collect();
            if parts.len() == 2 {
                let size = parse_size_field(parts[0]);
                Some((size, parts[1].trim()))
            } else {
                None
            }
        })
        .collect();

    parsed.sort_by_key(|b| std::cmp::Reverse(b.0));

    let top: Vec<String> = parsed
        .iter()
        .take(15)
        .map(|(size, path)| format!("{}\t{path}", format_size(*size)))
        .collect();

    Some(format!(
        "du: {} entries (top 15 by size)\n{}",
        lines.len(),
        top.join("\n")
    ))
}

fn parse_size_field(s: &str) -> u64 {
    let s = s.trim();
    if let Ok(v) = s.parse::<u64>() {
        return v;
    }
    let (num_part, suffix) = s.split_at(s.len().saturating_sub(1));
    let base: f64 = num_part.parse().unwrap_or(0.0);
    match suffix.to_uppercase().as_str() {
        "K" => (base * 1024.0) as u64,
        "M" => (base * 1024.0 * 1024.0) as u64,
        "G" => (base * 1024.0 * 1024.0 * 1024.0) as u64,
        _ => s.parse().unwrap_or(0),
    }
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1}G", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1}M", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.0}K", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}")
    }
}

pub fn compress_ping(output: &str) -> Option<String> {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() < 3 {
        return None;
    }

    let mut host = "";
    let mut stats_line = "";
    let mut rtt_line = "";

    for line in &lines {
        if line.starts_with("PING ") || line.starts_with("ping ") {
            host = line;
        }
        if line.contains("packets transmitted") || line.contains("packet loss") {
            stats_line = line;
        }
        if line.contains("rtt ") || line.contains("round-trip") {
            rtt_line = line;
        }
    }

    if stats_line.is_empty() {
        return None;
    }

    let mut out = String::new();
    if !host.is_empty() {
        out.push_str(host);
        out.push('\n');
    }
    out.push_str(stats_line);
    if !rtt_line.is_empty() {
        out.push('\n');
        out.push_str(rtt_line);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_compresses_large_output() {
        let mut lines = vec![
            "USER       PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND".to_string(),
        ];
        for i in 0..50 {
            lines.push(format!(
                "user     {:>5}  0.0  0.1  12345  1234 ?        S    10:00   0:00 process_{i}",
                1000 + i
            ));
        }
        lines.push(
            "root      9999 95.0  8.5 999999 99999 ?        R    10:00   5:00 heavy_proc"
                .to_string(),
        );
        let output = lines.join("\n");
        let result = compress_ps(&output).expect("should compress");
        assert!(result.contains("51 processes"));
        assert!(result.contains("heavy_proc"));
    }

    #[test]
    fn ps_skips_small_output() {
        let output = "USER PID %CPU %MEM\nroot 1 0.0 0.1 init";
        assert!(compress_ps(output).is_none());
    }

    #[test]
    fn df_returns_verbatim() {
        let mut lines =
            vec!["Filesystem     1K-blocks    Used Available Use% Mounted on".to_string()];
        for i in 0..20 {
            let pct = if i < 5 { 90 } else { 10 };
            lines.push(format!(
                "/dev/sda{i}  1000000  500000  500000  {pct}% /mnt/disk{i}"
            ));
        }
        let output = lines.join("\n");
        let result = compress_df(&output).expect("should return Some");
        assert_eq!(result, output, "df must pass through verbatim");
    }

    #[test]
    fn du_compresses_large_listing() {
        let mut lines = Vec::new();
        for i in 0..30 {
            lines.push(format!("{}\t./dir_{i}", (i + 1) * 1024));
        }
        let output = lines.join("\n");
        let result = compress_du(&output).expect("should compress");
        assert!(result.contains("30 entries"));
        assert!(result.contains("top 15"));
    }

    #[test]
    fn ping_extracts_summary() {
        let output = "PING google.com (142.250.80.46): 56 data bytes\n64 bytes from 142.250.80.46: icmp_seq=0 ttl=116 time=12.3 ms\n64 bytes from 142.250.80.46: icmp_seq=1 ttl=116 time=11.8 ms\n64 bytes from 142.250.80.46: icmp_seq=2 ttl=116 time=12.1 ms\n\n--- google.com ping statistics ---\n3 packets transmitted, 3 packets received, 0.0% packet loss\nrtt min/avg/max/stddev = 11.8/12.1/12.3/0.2 ms";
        let result = compress_ping(output).expect("should compress");
        assert!(result.contains("3 packets transmitted"));
        assert!(result.contains("rtt"));
        assert!(!result.contains("icmp_seq=1"));
    }
}
