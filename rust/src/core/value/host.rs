// SPDX-License-Identifier: Apache-2.0
//! Which agent process a lean-ctx session serves.
//!
//! Several agents can run lean-ctx in one project at once (two Claude windows,
//! a Codex task). Each starts its own MCP server, and the project snapshot only
//! names whichever wrote last. The server records its parent, the agent host;
//! a status line or hook the same host started finds that pid among its own
//! ancestors and so reads its own session, not a neighbour's.

/// The process that started this one: for an MCP server, the agent host.
/// `None` when orphaned (re-parented to init) or off Unix.
pub fn host_pid() -> Option<u32> {
    #[cfg(unix)]
    {
        // SAFETY: getppid() takes no arguments, cannot fail and has no side effects.
        let ppid = unsafe { libc::getppid() } as u32;
        (ppid > 1).then_some(ppid)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// How far up the tree a caller looks: agent → shell → status-line command
/// is two hops; the slack covers wrappers (`sh -c`, `env`, `npx`).
const MAX_DEPTH: usize = 8;

/// This process's ancestors, nearest first, stopping before init.
pub fn ancestors() -> Vec<u32> {
    let parents = parent_table();
    let mut chain = Vec::new();
    let mut pid = std::process::id();
    while let Some(&ppid) = parents.get(&pid) {
        if ppid <= 1 || chain.contains(&ppid) || chain.len() >= MAX_DEPTH {
            break;
        }
        chain.push(ppid);
        pid = ppid;
    }
    chain
}

/// pid → ppid for the processes the walk may visit.
#[cfg(target_os = "linux")]
fn parent_table() -> std::collections::HashMap<u32, u32> {
    let mut table = std::collections::HashMap::new();
    let mut pid = std::process::id();
    for _ in 0..=MAX_DEPTH {
        let Some(ppid) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| parse_proc_stat_ppid(&stat))
        else {
            break;
        };
        table.insert(pid, ppid);
        if ppid <= 1 {
            break;
        }
        pid = ppid;
    }
    table
}

#[cfg(all(unix, not(target_os = "linux")))]
fn parent_table() -> std::collections::HashMap<u32, u32> {
    std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid="])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| parse_ps_table(&String::from_utf8_lossy(&out.stdout)))
        .unwrap_or_default()
}

#[cfg(not(unix))]
fn parent_table() -> std::collections::HashMap<u32, u32> {
    std::collections::HashMap::new()
}

/// `ps -o pid=,ppid=` rows: two numbers per line.
#[cfg(any(test, all(unix, not(target_os = "linux"))))]
fn parse_ps_table(text: &str) -> std::collections::HashMap<u32, u32> {
    text.lines()
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            Some((cols.next()?.parse().ok()?, cols.next()?.parse().ok()?))
        })
        .collect()
}

/// The ppid in `/proc/<pid>/stat`: the field after the `(comm)` that may
/// itself contain spaces and parentheses, so parse from the last `)`.
#[cfg(any(test, target_os = "linux"))]
fn parse_proc_stat_ppid(stat: &str) -> Option<u32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_rows_parse_and_junk_is_skipped() {
        let table = parse_ps_table("  101   1\n  202 101\nPID PPID\n\n");
        assert_eq!(table.get(&202), Some(&101));
        assert_eq!(table.get(&101), Some(&1));
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn proc_stat_ppid_survives_odd_command_names() {
        assert_eq!(parse_proc_stat_ppid("42 (a) b (c) S 7 42 42"), Some(7));
        assert_eq!(parse_proc_stat_ppid("no parens"), None);
    }

    #[cfg(unix)]
    #[test]
    fn ancestors_start_at_the_parent() {
        let chain = ancestors();
        assert_eq!(chain.first().copied(), host_pid());
    }
}
