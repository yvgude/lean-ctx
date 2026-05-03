pub fn compress(cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ok".to_string());
    }

    if cmd.contains("generate") {
        return Some(compress_generate(trimmed));
    }
    if cmd.contains("migrate") {
        return Some(compress_migrate(trimmed));
    }
    if cmd.contains("db push") || cmd.contains("db pull") {
        return Some(compress_db_sync(trimmed));
    }
    if cmd.contains("studio") {
        return Some("Prisma Studio started".to_string());
    }
    if cmd.contains("format") {
        return Some(compress_format(trimmed));
    }
    if cmd.contains("validate") {
        return Some(compress_validate(trimmed));
    }

    Some(compact_lines(trimmed, 10))
}

fn compress_generate(output: &str) -> String {
    let mut generated = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        let plain = strip_ansi(trimmed);
        if plain.contains("Generated") || plain.contains("generated") {
            generated.push(plain);
        }
    }
    if generated.is_empty() {
        return strip_noise(output);
    }
    generated.join("\n")
}

fn compress_migrate(output: &str) -> String {
    let mut results = Vec::new();
    let mut migration_name = String::new();

    for line in output.lines() {
        let plain = strip_ansi(line.trim());
        if plain.contains("migration") && plain.contains("created") {
            migration_name.clone_from(&plain);
        }
        if plain.contains("applied")
            || plain.contains("Already in sync")
            || plain.contains("Database is up to date")
        {
            results.push(plain);
        }
    }

    if results.is_empty() && migration_name.is_empty() {
        return strip_noise(output);
    }

    let mut parts = Vec::new();
    if !migration_name.is_empty() {
        parts.push(migration_name);
    }
    parts.extend(results);
    parts.join("\n")
}

fn compress_db_sync(output: &str) -> String {
    let lines: Vec<String> = output
        .lines()
        .map(|l| strip_ansi(l.trim()))
        .filter(|l| !l.is_empty() && !l.contains("warn") && !l.starts_with("Prisma schema"))
        .collect();

    if lines.is_empty() {
        return "ok (synced)".to_string();
    }
    lines.join("\n")
}

fn compress_format(output: &str) -> String {
    if output.contains("already formatted") || output.contains("unchanged") {
        return "ok (already formatted)".to_string();
    }
    strip_noise(output)
}

fn compress_validate(output: &str) -> String {
    let plain = strip_ansi(output.trim());
    if plain.contains("valid") && !plain.contains("invalid") {
        return "ok (schema valid)".to_string();
    }
    compact_lines(&plain, 10)
}

fn strip_noise(output: &str) -> String {
    output
        .lines()
        .map(|l| strip_ansi(l.trim()))
        .filter(|l| !l.is_empty() && !l.contains("████") && !l.contains("▀") && !l.contains("━"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_ansi(s: &str) -> String {
    crate::core::compressor::strip_ansi(s)
}

fn compact_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= max {
        return lines.join("\n");
    }
    format!(
        "{}\n... ({} more lines)",
        lines[..max].join("\n"),
        lines.len() - max
    )
}
