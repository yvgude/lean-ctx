use regex::Regex;
use std::sync::OnceLock;

static PLAN_SUMMARY_RE: OnceLock<Regex> = OnceLock::new();
static APPLY_SUMMARY_RE: OnceLock<Regex> = OnceLock::new();
static INSTALLED_PROVIDER_RE: OnceLock<Regex> = OnceLock::new();
static PROVIDER_VERSION_RE: OnceLock<Regex> = OnceLock::new();

fn plan_summary_re() -> &'static Regex {
    PLAN_SUMMARY_RE.get_or_init(|| {
        Regex::new(r"Plan:\s*(\d+)\s+to add,\s*(\d+)\s+to change,\s*(\d+)\s+to destroy").unwrap()
    })
}

fn apply_summary_re() -> &'static Regex {
    APPLY_SUMMARY_RE.get_or_init(|| {
        Regex::new(
            r"Apply complete!\s*Resources:\s*(\d+)\s+added,\s*(\d+)\s+changed,\s*(\d+)\s+destroyed",
        )
        .unwrap()
    })
}

fn installed_provider_re() -> &'static Regex {
    INSTALLED_PROVIDER_RE
        .get_or_init(|| Regex::new(r"-\s*Installed\s+([^\s]+)\s+v([0-9][^\s]*)").unwrap())
}

fn provider_version_re() -> &'static Regex {
    PROVIDER_VERSION_RE
        .get_or_init(|| Regex::new(r"\*\s*provider\[([^\]]+)\]\s+([0-9][^\s]*)").unwrap())
}

fn is_provider_init_noise(line: &str) -> bool {
    let t = line.trim_start();
    let tl = t.to_ascii_lowercase();
    tl.contains("initializing provider plugins")
        || tl.contains("initializing the backend")
        || tl.contains("finding ")
            && (tl.contains("versions matching") || tl.contains("version of"))
        || tl.starts_with("- finding ")
        || tl.starts_with("- installing ")
        || tl.contains("terraform init") && tl.contains("upgrade")
        || tl.starts_with("╷")
        || tl.starts_with("╵")
        || tl.starts_with("│")
}

pub fn compress(command: &str, output: &str) -> Option<String> {
    let c = command.trim();
    let prefix = if c == "terraform" || c.starts_with("terraform ") {
        "terraform"
    } else if c == "tofu" || c.starts_with("tofu ") {
        "tofu"
    } else {
        return None;
    };
    let sub = c.strip_prefix(prefix).map(str::trim_start).unwrap_or("");
    let sub_cmd = sub.split_whitespace().next().unwrap_or("");

    match sub_cmd {
        "plan" => Some(compress_plan(output)),
        "apply" => Some(compress_apply(output)),
        "init" => Some(compress_init(output)),
        "validate" => Some(compress_validate(output)),
        _ => Some(compress_generic(output)),
    }
}

fn compress_plan(output: &str) -> String {
    let mut kept = Vec::new();

    for line in output.lines() {
        if is_provider_init_noise(line) {
            continue;
        }
        let tl = line.trim_start();
        if tl.starts_with("- Installed ") || tl.starts_with("- Installing ") {
            continue;
        }

        if let Some(caps) = plan_summary_re().captures(line) {
            let add = caps.get(1).map(|m| m.as_str()).unwrap_or("0");
            let chg = caps.get(2).map(|m| m.as_str()).unwrap_or("0");
            let des = caps.get(3).map(|m| m.as_str()).unwrap_or("0");
            kept.push(format!("+ {add} added, ~ {chg} changed, - {des} destroyed"));
            continue;
        }

        let l = line.to_ascii_lowercase();
        if l.contains("no changes.") || l.contains("infrastructure matches the configuration") {
            kept.push("No changes.".to_string());
            continue;
        }

        let is_diag = tl.contains('╷')
            || tl.contains('│')
            || tl.contains('╵')
            || l.contains("error:")
            || (l.contains("error ")
                && (l.contains("terraform") || l.contains("plan") || l.contains("provider")))
            || l.contains("warning:")
            || l.contains("warning ");
        if is_diag {
            kept.push(line.trim().to_string());
        }
    }

    if kept.is_empty() {
        "terraform plan (no summary parsed)".to_string()
    } else {
        kept.join("\n")
    }
}

fn compress_apply(output: &str) -> String {
    let mut results = Vec::new();
    let mut errors = Vec::new();

    for line in output.lines() {
        if is_provider_init_noise(line) {
            continue;
        }
        let tl = line.trim();
        if tl.is_empty() {
            continue;
        }

        if let Some(caps) = apply_summary_re().captures(line) {
            let a = caps.get(1).map(|m| m.as_str()).unwrap_or("0");
            let c = caps.get(2).map(|m| m.as_str()).unwrap_or("0");
            let d = caps.get(3).map(|m| m.as_str()).unwrap_or("0");
            results.push(format!(
                "Apply complete: +{a} added, ~{c} changed, -{d} destroyed"
            ));
            continue;
        }

        let ll = tl.to_ascii_lowercase();
        if ll.contains("error")
            && (ll.contains("apply") || ll.contains("terraform") || tl.contains('╷'))
        {
            errors.push(tl.to_string());
        } else if ll.starts_with("creation complete")
            || ll.starts_with("modification complete")
            || ll.starts_with("destruction complete")
            || ll.starts_with("destroy complete")
        {
            results.push(tl.to_string());
        }
    }

    let mut out = Vec::new();
    if !results.is_empty() {
        out.push(results.join("\n"));
    }
    if !errors.is_empty() {
        out.push(format!("errors:\n{}", errors.join("\n")));
    }
    if out.is_empty() {
        "terraform apply (no summary parsed)".to_string()
    } else {
        out.join("\n\n")
    }
}

fn compress_init(output: &str) -> String {
    let mut providers: Vec<String> = Vec::new();
    let mut success = false;

    for line in output.lines() {
        let tl = line.trim();
        if tl.is_empty() {
            continue;
        }
        let ll = tl.to_ascii_lowercase();
        if ll.contains("terraform has been successfully initialized")
            || ll.contains("initialization complete")
        {
            success = true;
        }
        if let Some(caps) = installed_provider_re().captures(tl) {
            let name = caps.get(1).map(|m| m.as_str()).unwrap_or("?");
            let ver = caps.get(2).map(|m| m.as_str()).unwrap_or("?");
            providers.push(format!("{name} v{ver}"));
            continue;
        }
        if let Some(caps) = provider_version_re().captures(tl) {
            let reg = caps.get(1).map(|m| m.as_str()).unwrap_or("?");
            let ver = caps.get(2).map(|m| m.as_str()).unwrap_or("?");
            providers.push(format!("{reg} {ver}"));
        }
    }

    let status = if success {
        "Terraform initialized"
    } else {
        "terraform init"
    };

    if providers.is_empty() {
        status.to_string()
    } else {
        format!("{status}\n{}", providers.join(", "))
    }
}

fn compress_validate(output: &str) -> String {
    let mut errs = Vec::new();
    for line in output.lines() {
        let tl = line.trim();
        if tl.is_empty() {
            continue;
        }
        let ll = tl.to_ascii_lowercase();
        if ll.contains("success!") && ll.contains("configuration is valid") {
            return "Success".to_string();
        }
        if ll.contains("error") || tl.starts_with('╷') || tl.starts_with('│') {
            errs.push(tl.to_string());
        }
    }
    if errs.is_empty() {
        "Success".to_string()
    } else {
        errs.join("\n")
    }
}

fn compress_generic(output: &str) -> String {
    let mut lines: Vec<String> = output
        .lines()
        .filter(|l| !is_provider_init_noise(l))
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() > 40 {
        let n = lines.len();
        lines = lines.split_off(n - 25);
        format!("... (truncated)\n{}", lines.join("\n"))
    } else {
        lines.join("\n")
    }
}
