macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn maven_download_re() -> &'static regex::Regex {
    static_regex!(r"(?i)\[INFO\]\s+(Downloading|Downloaded)\s+")
}

fn maven_progress_re() -> &'static regex::Regex {
    static_regex!(r"\[INFO\].*kB\s+\|")
}

fn gradle_download_re() -> &'static regex::Regex {
    static_regex!(r"(?i)^(Downloading|Download)\s+https?://")
}

fn gradle_progress_re() -> &'static regex::Regex {
    static_regex!(r"^[<>=\s]+$|^[0-9]+%\s+EXECUTING")
}

fn tests_run_re() -> &'static regex::Regex {
    static_regex!(r"Tests run:\s*\d+")
}

fn is_maven_noise(line: &str) -> bool {
    let t = line.trim_start();
    if maven_download_re().is_match(t) {
        return true;
    }
    if maven_progress_re().is_match(t) {
        return true;
    }
    if t.contains("Progress (") && t.contains("):") && t.contains('%') {
        return true;
    }
    false
}

fn is_gradle_noise(line: &str) -> bool {
    let t = line.trim();
    if gradle_download_re().is_match(t) {
        return true;
    }
    if gradle_progress_re().is_match(t) {
        return true;
    }
    let tl = t.to_ascii_lowercase();
    if tl.starts_with("consider enabling configuration cache")
        || tl.contains("deprecated gradle features were used")
        || tl.starts_with("you can use '--warning-mode")
    {
        return true;
    }
    false
}

fn is_maven_or_gradle_command(command: &str) -> bool {
    let c = command.trim();
    let cl = c.to_ascii_lowercase();
    cl.starts_with("mvn ")
        || cl.starts_with("./mvnw ")
        || cl.starts_with("mvnw ")
        || cl.starts_with("gradle ")
        || cl.starts_with("./gradlew ")
        || cl.starts_with("gradlew ")
}

fn is_gradle_command(command: &str) -> bool {
    let cl = command.trim().to_ascii_lowercase();
    cl.starts_with("gradle ") || cl.starts_with("./gradlew ") || cl.starts_with("gradlew ")
}

pub fn compress(command: &str, output: &str) -> Option<String> {
    if !is_maven_or_gradle_command(command) {
        return None;
    }
    if is_gradle_command(command) {
        Some(compress_gradle(output))
    } else {
        Some(compress_maven(output))
    }
}

fn compress_maven(output: &str) -> String {
    let mut kept = Vec::new();

    for line in output.lines() {
        let t = line.trim_end();
        if t.trim().is_empty() {
            continue;
        }
        if is_maven_noise(t) {
            continue;
        }

        let tl = t.to_ascii_lowercase();
        if tl.contains("[error]")
            || tl.contains("[fatal]")
            || tl.contains("build failure")
            || tl.contains("build success")
            || tl.contains("failure!")
            || tl.contains("tests run:")
            || tl.contains("failures:")
            || tl.contains("errors:")
            || tl.contains("skipped:")
            || tests_run_re().is_match(t)
        {
            kept.push(t.trim().to_string());
            continue;
        }

        if tl.contains("[warning]") {
            kept.push(t.trim().to_string());
        }
    }

    if kept.is_empty() {
        "mvn (no build/test lines kept)".to_string()
    } else {
        kept.join("\n")
    }
}

fn compress_gradle(output: &str) -> String {
    let mut kept = Vec::new();
    let mut task_lines = Vec::new();

    for line in output.lines() {
        let t = line.trim_end();
        if t.trim().is_empty() {
            continue;
        }
        if is_gradle_noise(t) {
            continue;
        }

        let tl = t.to_ascii_lowercase();
        if tl.starts_with("> task ") {
            if tl.contains("failed")
                || tl.contains("failure")
                || tl.contains("skipped")
                || tl.contains("no-source")
            {
                task_lines.push(t.trim().to_string());
            }
            continue;
        }

        if tl.contains("actionable tasks:") {
            kept.push(t.trim().to_string());
            continue;
        }

        if tl.contains("build successful")
            || tl.contains("build failed")
            || tl.starts_with("failure:")
            || tl.contains("what went wrong:")
            || tl.contains("execution failed for task")
            || tl.contains("error:")
            || tl.contains("exception")
            || tl.contains("tests completed:")
            || (tl.contains("test ") && (tl.contains("failed") || tl.contains("passed")))
            || tl.contains("there were failing tests")
        {
            kept.push(t.trim().to_string());
        }
    }

    if !task_lines.is_empty() {
        kept.push("tasks:\n".to_string() + &task_lines.join("\n"));
    }

    if kept.is_empty() {
        "gradle (no summary kept)".to_string()
    } else {
        kept.join("\n")
    }
}
