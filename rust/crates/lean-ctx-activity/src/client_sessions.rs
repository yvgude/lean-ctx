// SPDX-License-Identifier: Apache-2.0
//! The local "connected without a ctx call" signal: per MCP client, the last
//! [`SESSIONS_PER_CLIENT`] server sessions and how many tool calls each
//! received, in one private file (`<data dir>/client_sessions.json`, mode 0600).
//!
//! The MCP server writes it at the handshake, on the first tool call and at
//! exit; `lean-ctx doctor` and `lean-ctx status` read it and show one line per
//! client whose last sessions all ran at least [`MIN_SESSION_SECS`] and ended
//! without a single call. The line states that observation and a conditional
//! fix, never a diagnosis: the count cannot tell why the agent made no call.
//! Nothing leaves the machine: this is no telemetry, and nothing of it
//! reaches a tool output.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The file under the data directory.
pub const CLIENT_SESSIONS_FILE: &str = "client_sessions.json";
/// Sessions kept per client; a streak needs this many empty ones in a row.
pub const SESSIONS_PER_CLIENT: usize = 5;
/// A session that ends sooner without a call is not kept at all: health
/// checks (`claude mcp list`), settings pages, reconnects and config reloads
/// start the server, complete the handshake and exit within seconds.
pub const MIN_SESSION_SECS: u64 = 60;
/// Clients kept (the most recently started).
const MAX_CLIENTS: usize = 32;
/// A larger file is not ours to parse; it is replaced on the next write.
const MAX_FILE_BYTES: u64 = 64 * 1024;
/// How long a write waits for another server's write before it is dropped.
const LOCK_TIMEOUT: Duration = Duration::from_millis(200);

/// One MCP server session of a client: its process and start second identify
/// it, `calls` counts received tool calls (at least 1 after the first call),
/// `ended` is the exit second once the server wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientSession {
    pub pid: u32,
    pub started: u64,
    pub calls: u64,
    #[serde(default)]
    pub ended: Option<u64>,
}

impl ClientSession {
    /// Ended after at least [`MIN_SESSION_SECS`] without a call. A session
    /// still running, or killed before it wrote its end, is no evidence
    /// either way and breaks a streak.
    fn is_empty(&self) -> bool {
        self.calls == 0
            && self
                .ended
                .is_some_and(|end| end.saturating_sub(self.started) >= MIN_SESSION_SECS)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Ledger {
    clients: BTreeMap<String, Vec<ClientSession>>,
}

/// What `lean-ctx init --agent` can do for a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupTarget {
    /// Display name.
    pub name: &'static str,
    /// The `lean-ctx init --agent` value.
    pub agent: &'static str,
    /// False for MCP-only hosts where `init --agent` writes no hook (the
    /// empty arm of `install_agent_hook_with_mode`) and no rules file (no
    /// `rules_inject` target): naming the command there would promise a fix
    /// it cannot make. Oh My Pi has no hook either, but its rules target
    /// (`omp_agents_path`) makes `init --agent omp` a real fix.
    pub writes_setup: bool,
}

/// How LeanCTX steers a client's agent, as the caller's config sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Steering {
    /// The user declined rule steering (`rules_injection = off` or
    /// `setup.auto_inject_rules = false`): no line, since every fix we could
    /// name would steer.
    pub declined: bool,
    /// The agent's shell hooks route commands through LeanCTX, which an MCP
    /// call count cannot see.
    pub shell_hooks: bool,
}

/// A client whose last [`SESSIONS_PER_CLIENT`] sessions all were empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptyStreak {
    /// The client id of the handshake (`pi`, `claude-code`, …).
    pub client: String,
    pub sessions: usize,
}

impl EmptyStreak {
    /// The client's `init --agent` target (`Some` for every recorded client).
    pub fn target(&self) -> Option<SetupTarget> {
        setup_target(&self.client)
    }

    /// The one line shown for this client: what was observed and a fix for
    /// the case that the agent should use LeanCTX. `None` when the user
    /// declined steering.
    pub fn line(&self, steering: Steering) -> Option<String> {
        let target = self.target()?;
        if steering.declined {
            return None;
        }
        let name = target.name;
        let seen = format!(
            "{name}: connected {} times without a ctx call",
            self.sessions
        );
        Some(if !target.writes_setup {
            format!(
                "{seen}; if your agent should use LeanCTX, ask it to use the ctx tools (lean-ctx writes no rules for {name})"
            )
        } else if steering.shell_hooks {
            format!(
                "{seen} (shell commands through LeanCTX are not counted); if your agent should use the ctx tools, run lean-ctx init --agent {}",
                target.agent
            )
        } else {
            format!(
                "{seen}; if your agent should use LeanCTX, run lean-ctx init --agent {}",
                target.agent
            )
        })
    }
}

/// The `init --agent` target of a handshake client id
/// (`core::client_capabilities`). `None` for clients without one: they are
/// not recorded, since there is no concrete fix to name.
pub fn setup_target(client: &str) -> Option<SetupTarget> {
    let (name, agent, writes_setup) = match client {
        "claude-code" => ("Claude Code", "claude", true),
        "codex" => ("Codex", "codex", true),
        "cursor" => ("Cursor", "cursor", true),
        "gemini-cli" => ("Gemini CLI", "gemini", true),
        "antigravity" => ("Antigravity", "antigravity", true),
        "windsurf" => ("Windsurf", "windsurf", true),
        "zed" => ("Zed", "zed", true),
        "vscode-copilot" => ("VS Code Copilot", "copilot", true),
        "vscode" => ("VS Code", "vscode", true),
        "kiro" => ("Kiro", "kiro", true),
        "codebuddy" => ("CodeBuddy", "codebuddy", true),
        "codewhale" => ("CodeWhale", "codewhale", false),
        "muse" => ("Muse Code", "muse", false),
        "cline" => ("Cline", "cline", true),
        "roo-code" => ("Roo Code", "roo", true),
        "continue" => ("Continue", "continue", true),
        "opencode" => ("OpenCode", "opencode", true),
        "goose" => ("Goose", "goose", false),
        "amp" => ("Amp", "amp", true),
        "augment" => ("Augment", "augment", true),
        "jetbrains" => ("JetBrains", "jetbrains", true),
        "trae" => ("Trae", "trae", true),
        "qwen-code" => ("Qwen Code", "qwen", true),
        "crush" => ("Crush", "crush", true),
        "neovim" => ("Neovim", "neovim", true),
        "emacs" => ("Emacs", "emacs", true),
        "pi" => ("Pi", "pi", true),
        "oh-my-pi" => ("Oh My Pi", "omp", true),
        _ => return None,
    };
    Some(SetupTarget {
        name,
        agent,
        writes_setup,
    })
}

/// What a session reports.
#[derive(Debug, Clone, Copy)]
pub enum Mark {
    /// The handshake completed.
    Started,
    /// The first tool call was received.
    Called,
    /// The server exits at second `at` after `calls` received tool calls.
    Ended { calls: u64, at: u64 },
}

/// The ledger file under one data directory.
#[derive(Debug, Clone)]
pub struct ClientSessions {
    path: PathBuf,
}

impl ClientSessions {
    pub fn in_data_dir(dir: &Path) -> Self {
        Self {
            path: dir.join(CLIENT_SESSIONS_FILE),
        }
    }

    /// Records `mark` for the session `(pid, started)` of `client`. Every mark
    /// creates the session when it is missing, so their order does not matter.
    /// A session that ends sooner than [`MIN_SESSION_SECS`] without a call is
    /// removed, so probes never push real sessions out. Clients without a
    /// setup target are not recorded.
    pub fn record(&self, client: &str, pid: u32, started: u64, mark: Mark) -> Result<(), String> {
        if setup_target(client).is_none() {
            return Ok(());
        }
        let lock_path = self.path.with_extension("json.lock");
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let lock = options.open(&lock_path).map_err(|e| e.to_string())?;
        lean_ctx_fs::file_lock::acquire_exclusive_timeout(&lock, LOCK_TIMEOUT)?;

        let mut ledger = self.load();
        let sessions = ledger.clients.entry(client.to_string()).or_default();
        let index = if let Some(i) = sessions
            .iter()
            .position(|s| s.pid == pid && s.started == started)
        {
            i
        } else {
            sessions.push(ClientSession {
                pid,
                started,
                calls: 0,
                ended: None,
            });
            sessions.len() - 1
        };
        let session = &mut sessions[index];
        match mark {
            Mark::Started => {}
            Mark::Called => session.calls = session.calls.max(1),
            Mark::Ended { calls, at } => {
                session.calls = session.calls.max(calls);
                session.ended = Some(at);
                if session.calls == 0 && at.saturating_sub(started) < MIN_SESSION_SECS {
                    sessions.remove(index);
                }
            }
        }
        sessions.sort_by_key(|s| s.started);
        let excess = sessions.len().saturating_sub(SESSIONS_PER_CLIENT);
        sessions.drain(..excess);
        ledger.clients.retain(|_, sessions| !sessions.is_empty());
        while ledger.clients.len() > MAX_CLIENTS {
            let oldest = ledger
                .clients
                .iter()
                .min_by_key(|(_, s)| s.last().map_or(0, |s| s.started))
                .map(|(name, _)| name.clone());
            match oldest {
                Some(name) => ledger.clients.remove(&name),
                None => break,
            };
        }

        // Mode 0600 on Unix; elsewhere the data directory's ACL protects it.
        #[cfg(unix)]
        let permissions =
            Some(<std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600));
        #[cfg(not(unix))]
        let permissions: Option<std::fs::Permissions> = None;
        let bytes = serde_json::to_vec(&ledger).map_err(|e| e.to_string())?;
        lean_ctx_fs::atomic_fs::try_atomic_write(&self.path, &bytes, permissions.as_ref())
            .map_err(|e| e.to_string())
    }

    /// Clients whose last [`SESSIONS_PER_CLIENT`] sessions all ran at least
    /// [`MIN_SESSION_SECS`] and ended without a call.
    pub fn empty_streaks(&self) -> Vec<EmptyStreak> {
        self.load()
            .clients
            .into_iter()
            .filter(|(client, sessions)| {
                setup_target(client).is_some()
                    && sessions.len() >= SESSIONS_PER_CLIENT
                    && sessions.iter().all(ClientSession::is_empty)
            })
            .map(|(client, sessions)| EmptyStreak {
                client,
                sessions: sessions.len(),
            })
            .collect()
    }

    fn load(&self) -> Ledger {
        let Ok(file) = std::fs::File::open(&self.path) else {
            return Ledger::default();
        };
        let mut text = String::new();
        if file
            .take(MAX_FILE_BYTES + 1)
            .read_to_string(&mut text)
            .is_err()
            || text.len() as u64 > MAX_FILE_BYTES
        {
            return Ledger::default();
        }
        serde_json::from_str(&text).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Steering = Steering {
        declined: false,
        shell_hooks: false,
    };

    fn ledger() -> (tempfile::TempDir, ClientSessions) {
        let dir = tempfile::tempdir().unwrap();
        let store = ClientSessions::in_data_dir(dir.path());
        (dir, store)
    }

    /// One session of `secs` seconds without a call, starting at `started`.
    fn session(store: &ClientSessions, client: &str, started: u64, secs: u64) {
        store
            .record(client, 100, started, Mark::Started)
            .and_then(|()| {
                store.record(
                    client,
                    100,
                    started,
                    Mark::Ended {
                        calls: 0,
                        at: started + secs,
                    },
                )
            })
            .unwrap();
    }

    fn empty_session(store: &ClientSessions, client: &str, started: u64) {
        session(store, client, started * 1000, MIN_SESSION_SECS);
    }

    #[cfg(unix)]
    fn mode(dir: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(dir.join(CLIENT_SESSIONS_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    fn pi_streak(store: &ClientSessions) -> EmptyStreak {
        for started in 1..=SESSIONS_PER_CLIENT as u64 {
            empty_session(store, "pi", started);
        }
        let mut streaks = store.empty_streaks();
        assert_eq!(streaks.len(), 1);
        streaks.remove(0)
    }

    #[test]
    fn five_empty_sessions_give_one_observation_with_a_conditional_fix() {
        let (_dir, store) = ledger();
        assert_eq!(
            pi_streak(&store).line(PLAIN).as_deref(),
            Some(
                "Pi: connected 5 times without a ctx call; if your agent should use LeanCTX, run lean-ctx init --agent pi"
            )
        );
    }

    #[test]
    fn short_sessions_never_count_and_never_push_real_sessions_out() {
        let (_dir, store) = ledger();
        for started in 1..SESSIONS_PER_CLIENT as u64 {
            empty_session(&store, "codex", started);
        }
        // Health checks and reconnects: many, each ending within seconds.
        for probe in 0..20 {
            session(&store, "codex", 50_000 + probe, 2);
        }
        assert!(store.empty_streaks().is_empty(), "probes are no sessions");
        empty_session(&store, "codex", 99);
        assert_eq!(store.empty_streaks().len(), 1, "the four real ones stayed");
    }

    #[test]
    fn no_line_while_a_session_is_used_running_or_without_an_end() {
        let (_dir, store) = ledger();
        for started in 1..SESSIONS_PER_CLIENT as u64 {
            empty_session(&store, "cursor", started);
        }
        // Running, or killed before it wrote its end: no evidence.
        store.record("cursor", 9, 60_000, Mark::Started).unwrap();
        assert!(store.empty_streaks().is_empty());
        // One used session among the last five breaks it too.
        store.record("cursor", 9, 60_000, Mark::Called).unwrap();
        store
            .record(
                "cursor",
                9,
                60_000,
                Mark::Ended {
                    calls: 3,
                    at: 60_001,
                },
            )
            .unwrap();
        assert!(store.empty_streaks().is_empty());
    }

    #[test]
    fn no_line_when_the_user_declined_rule_steering() {
        let (_dir, store) = ledger();
        let declined = Steering {
            declined: true,
            shell_hooks: false,
        };
        assert_eq!(pi_streak(&store).line(declined), None);
    }

    #[test]
    fn shell_hook_clients_get_a_line_that_says_shell_use_is_not_counted() {
        let (_dir, store) = ledger();
        let hooks = Steering {
            declined: false,
            shell_hooks: true,
        };
        assert_eq!(
            pi_streak(&store).line(hooks).as_deref(),
            Some(
                "Pi: connected 5 times without a ctx call (shell commands through LeanCTX are not counted); if your agent should use the ctx tools, run lean-ctx init --agent pi"
            )
        );
    }

    #[test]
    fn hosts_without_a_setup_surface_get_no_init_command() {
        let (_dir, store) = ledger();
        for started in 1..=SESSIONS_PER_CLIENT as u64 {
            empty_session(&store, "goose", started);
        }
        let line = store.empty_streaks()[0].line(PLAIN).unwrap();
        assert_eq!(
            line,
            "Goose: connected 5 times without a ctx call; if your agent should use LeanCTX, ask it to use the ctx tools (lean-ctx writes no rules for Goose)"
        );
    }

    #[test]
    fn oh_my_pi_gets_its_init_command_since_init_writes_its_rules() {
        assert!(setup_target("oh-my-pi").is_some_and(|t| t.writes_setup && t.agent == "omp"));
    }

    #[test]
    fn the_file_is_bounded_private_and_skips_clients_without_a_fix() {
        let (dir, store) = ledger();
        for started in 1..=12 {
            empty_session(&store, "claude-code", started);
        }
        empty_session(&store, "unknown", 1);
        let ledger = store.load();
        assert_eq!(ledger.clients.len(), 1);
        let kept = &ledger.clients["claude-code"];
        assert_eq!(kept.len(), SESSIONS_PER_CLIENT);
        assert_eq!(kept[0].started, 8000, "the newest sessions are kept");
        #[cfg(unix)]
        assert_eq!(mode(dir.path()), 0o600);
        #[cfg(not(unix))]
        let _ = dir;
    }

    #[test]
    fn a_corrupt_or_oversized_file_reads_empty_and_is_replaced() {
        let (dir, store) = ledger();
        let path = dir.path().join(CLIENT_SESSIONS_FILE);
        // A valid streak, padded past the read cap: not parsed.
        for started in 1..=SESSIONS_PER_CLIENT as u64 {
            empty_session(&store, "pi", started);
        }
        let mut padded = std::fs::read(&path).unwrap();
        padded.resize(MAX_FILE_BYTES as usize + 1, b' ');
        std::fs::write(&path, &padded).unwrap();
        assert!(store.empty_streaks().is_empty());
        // Garbage: not parsed either; the next write replaces it.
        std::fs::write(&path, b"{\"clients\": [tru").unwrap();
        assert!(store.empty_streaks().is_empty());
        store.record("pi", 1, 1, Mark::Started).unwrap();
        assert_eq!(store.load().clients["pi"].len(), 1);
        #[cfg(unix)]
        assert_eq!(mode(dir.path()), 0o600);
    }

    #[test]
    fn concurrent_servers_keep_both_sessions() {
        let (_dir, store) = ledger();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for pid in [1, 2] {
                let (store, barrier) = (&store, &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    // A write that times out on the lock is dropped by design
                    // (fail-open); a server would lose that one mark. Retried
                    // here, so a slow runner cannot fail the property under
                    // test: no write that succeeds loses another's session.
                    let mut attempts = 0;
                    while let Err(error) = store.record("codex", pid, 7, Mark::Started) {
                        attempts += 1;
                        assert!(attempts < 50, "lock never free: {error}");
                    }
                });
            }
        });
        let pids: Vec<u32> = store.load().clients["codex"]
            .iter()
            .map(|s| s.pid)
            .collect();
        assert_eq!(pids.len(), 2, "{pids:?}");
    }
}
