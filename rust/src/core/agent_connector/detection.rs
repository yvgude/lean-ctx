use super::traits::AgentInfo;
use std::path::{Path, PathBuf};
use std::process::Command;

const PROBE_TIMEOUT_MS: u64 = 2_000;

pub(crate) fn detect_agents() -> Vec<AgentInfo> {
    vec![detect_codex(), detect_claude(), detect_cursor()]
}

fn detect_codex() -> AgentInfo {
    let (path, version, available) = probe_binary("codex");
    AgentInfo {
        name: "codex".into(),
        version,
        path,
        available,
        capabilities: if available {
            vec![
                "non-interactive".into(),
                "json-output".into(),
                "approve-mode".into(),
            ]
        } else {
            vec![]
        },
    }
}

fn detect_claude() -> AgentInfo {
    let (path, version, available) = probe_binary("claude");
    AgentInfo {
        name: "claude-code".into(),
        version,
        path,
        available,
        capabilities: if available {
            vec!["non-interactive".into(), "json-output".into(), "mcp".into()]
        } else {
            vec![]
        },
    }
}

fn detect_cursor() -> AgentInfo {
    let (path, version, available) = probe_binary("cursor");
    AgentInfo {
        name: "cursor".into(),
        version,
        path,
        available,
        capabilities: if available {
            vec!["acp".into(), "cloud-agents".into()]
        } else {
            vec![]
        },
    }
}

fn probe_binary(name: &str) -> (PathBuf, Option<String>, bool) {
    match which_binary(name) {
        Some(p) => {
            let v = probe_version(&p);
            let available = v.is_some();
            (p, v, available)
        }
        None => (PathBuf::from(name), None, false),
    }
}

fn which_binary(name: &str) -> Option<PathBuf> {
    super::timeout::run_with_timeout(Command::new("which").arg(name), PROBE_TIMEOUT_MS)
        .ok()
        .filter(|o| !o.timed_out && o.output.status.success())
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.output.stdout).trim().to_string();
            if s.is_empty() {
                None
            } else {
                Some(PathBuf::from(s))
            }
        })
}

pub(crate) fn probe_version(path: &Path) -> Option<String> {
    probe_version_with_timeout(path, PROBE_TIMEOUT_MS)
}

pub(crate) fn probe_version_with_timeout(path: &Path, timeout_ms: u64) -> Option<String> {
    if timeout_ms == 0 {
        return None;
    }
    super::timeout::run_with_timeout(
        Command::new(path).arg("--version"),
        timeout_ms.min(PROBE_TIMEOUT_MS),
    )
    .ok()
    .filter(|o| !o.timed_out && o.output.status.success())
    .map(|o| {
        String::from_utf8_lossy(&o.output.stdout)
            .trim()
            .lines()
            .next()
            .unwrap_or("")
            .to_string()
    })
    .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_three_agents() {
        let agents = detect_agents();
        assert_eq!(agents.len(), 3);
        assert_eq!(agents[0].name, "codex");
        assert_eq!(agents[1].name, "claude-code");
        assert_eq!(agents[2].name, "cursor");
    }
}
