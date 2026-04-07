use regex::Regex;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct FilterFile {
    #[serde(default)]
    rules: Vec<RawFilterRule>,
}

#[derive(Debug, Deserialize)]
struct RawFilterRule {
    command: Option<String>,
    pattern: Option<String>,
    replace: Option<String>,
    #[serde(default)]
    keep_lines: Vec<String>,
}

#[derive(Debug)]
struct CompiledRule {
    command_re: Option<Regex>,
    pattern_re: Option<Regex>,
    replace: Option<String>,
    keep_lines: Vec<String>,
}

pub struct FilterEngine {
    rules: Vec<CompiledRule>,
}

impl FilterEngine {
    pub fn load() -> Option<Self> {
        let dir = dirs::home_dir()?.join(".lean-ctx").join("filters");
        if !dir.exists() {
            return None;
        }
        let entries = std::fs::read_dir(&dir).ok()?;
        let mut rules = Vec::new();

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                match toml::from_str::<FilterFile>(&content) {
                    Ok(file) => {
                        for raw in file.rules {
                            if let Some(compiled) = compile_rule(raw, &path) {
                                rules.push(compiled);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("lean-ctx: filter parse error in {}: {e}", path.display());
                    }
                }
            }
        }

        if rules.is_empty() {
            None
        } else {
            Some(FilterEngine { rules })
        }
    }

    pub fn apply(&self, command: &str, output: &str) -> Option<String> {
        let cmd_lower = command.to_ascii_lowercase();

        for rule in &self.rules {
            if let Some(ref cmd_re) = rule.command_re {
                if !cmd_re.is_match(&cmd_lower) {
                    continue;
                }
            }

            if !rule.keep_lines.is_empty() {
                let filtered: Vec<&str> = output
                    .lines()
                    .filter(|line| {
                        rule.keep_lines
                            .iter()
                            .any(|pattern| line.contains(pattern.as_str()))
                    })
                    .collect();
                if !filtered.is_empty() {
                    return Some(filtered.join("\n"));
                }
            }

            if let (Some(ref pat_re), Some(ref replacement)) = (&rule.pattern_re, &rule.replace) {
                let result = pat_re.replace_all(output, replacement.as_str());
                if result != output {
                    return Some(result.to_string());
                }
            }
        }

        None
    }

    pub fn list_rules(&self) -> Vec<String> {
        self.rules
            .iter()
            .map(|r| {
                let cmd = r
                    .command_re
                    .as_ref()
                    .map(|re| re.as_str().to_string())
                    .unwrap_or_else(|| "*".to_string());
                if !r.keep_lines.is_empty() {
                    format!("  {cmd} -> keep lines: {:?}", r.keep_lines)
                } else if let Some(ref pat) = r.pattern_re {
                    let repl = r.replace.as_deref().unwrap_or("...");
                    format!("  {cmd} -> /{pat}/ => {repl}")
                } else {
                    format!("  {cmd} -> (no action)")
                }
            })
            .collect()
    }
}

fn compile_rule(raw: RawFilterRule, path: &Path) -> Option<CompiledRule> {
    let command_re = raw.command.as_ref().and_then(|s| {
        Regex::new(s)
            .map_err(|e| {
                eprintln!("lean-ctx: invalid command regex in {}: {e}", path.display());
            })
            .ok()
    });

    let pattern_re = raw.pattern.as_ref().and_then(|s| {
        Regex::new(s)
            .map_err(|e| {
                eprintln!("lean-ctx: invalid pattern regex in {}: {e}", path.display());
            })
            .ok()
    });

    Some(CompiledRule {
        command_re,
        pattern_re,
        replace: raw.replace,
        keep_lines: raw.keep_lines,
    })
}

pub fn validate_filter_file(path: &str) -> Result<usize, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("Cannot read {path}: {e}"))?;
    let file: FilterFile =
        toml::from_str(&content).map_err(|e| format!("TOML parse error: {e}"))?;

    let mut valid = 0;
    for (i, rule) in file.rules.iter().enumerate() {
        if let Some(ref cmd) = rule.command {
            Regex::new(cmd).map_err(|e| format!("Rule {}: invalid command regex: {e}", i + 1))?;
        }
        if let Some(ref pat) = rule.pattern {
            Regex::new(pat).map_err(|e| format!("Rule {}: invalid pattern regex: {e}", i + 1))?;
        }
        valid += 1;
    }
    Ok(valid)
}

pub fn create_example_filter() -> Result<String, String> {
    let dir = dirs::home_dir()
        .ok_or("Cannot determine home directory")?
        .join(".lean-ctx")
        .join("filters");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let path = dir.join("example.toml");
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }

    let content = r#"# lean-ctx custom filter example
# Place .toml files in ~/.lean-ctx/filters/ to define custom compression rules.
# User filters are applied BEFORE builtin patterns.

# Rule 1: Replace verbose upload logs with a summary
# [[rules]]
# command = "myapp deploy"
# pattern = "Uploading .+ to s3://.+"
# replace = "[uploaded to S3]"

# Rule 2: Keep only important lines from terraform plan
# [[rules]]
# command = "terraform plan"
# keep_lines = ["Plan:", "Changes:", "Error:", "No changes"]
"#;

    std::fs::write(&path, content).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_engine_returns_none() {
        let engine = FilterEngine { rules: Vec::new() };
        assert!(engine.apply("git status", "on branch main").is_none());
    }

    #[test]
    fn keep_lines_filter() {
        let engine = FilterEngine {
            rules: vec![CompiledRule {
                command_re: Some(Regex::new("terraform").unwrap()),
                pattern_re: None,
                replace: None,
                keep_lines: vec!["Plan:".to_string(), "Error:".to_string()],
            }],
        };
        let output = "Loading...\nInitializing...\nPlan: 3 to add\nDone.";
        let result = engine.apply("terraform plan", output);
        assert_eq!(result, Some("Plan: 3 to add".to_string()));
    }

    #[test]
    fn regex_replace_filter() {
        let engine = FilterEngine {
            rules: vec![CompiledRule {
                command_re: Some(Regex::new("deploy").unwrap()),
                pattern_re: Some(Regex::new(r"Uploading \S+ to s3://\S+").unwrap()),
                replace: Some("[uploaded to S3]".to_string()),
                keep_lines: Vec::new(),
            }],
        };
        let output = "Starting deploy\nUploading app.zip to s3://bucket/key\nDone";
        let result = engine.apply("myapp deploy prod", output);
        assert!(result.unwrap().contains("[uploaded to S3]"));
    }

    #[test]
    fn command_mismatch_skips() {
        let engine = FilterEngine {
            rules: vec![CompiledRule {
                command_re: Some(Regex::new("terraform").unwrap()),
                pattern_re: None,
                replace: None,
                keep_lines: vec!["Plan:".to_string()],
            }],
        };
        assert!(engine.apply("git status", "Plan: something").is_none());
    }
}
