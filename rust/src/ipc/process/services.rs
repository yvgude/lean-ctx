// SPDX-License-Identifier: Apache-2.0
//! Recorded-service selection and stopping (#2176): classify lean-ctx
//! service command lines, gate recorded PIDs by identity and user, list
//! unrecorded services, stop only what this instance owns.

use super::{
    ProcessIdentity, StopSignal, identity, installed_executable_path, is_same_process,
    owned_by_current_user, protected_self_pids, same_process, signal_same_process,
};
#[cfg(not(windows))]
use super::{find_pids_by_name, run_with_timeout};

/// A long-lived lean-ctx background service that the lifecycle commands
/// (`stop`, `restart`, `config apply`, `uninstall`, `dev-install`) own and may
/// stop (#2176).
///
/// Candidates are only PIDs this instance recorded (daemon PID file, proxy
/// `/health`, Cockpit launch record — `daemon::owned_service_pids`), and each
/// must also pass this command-line allowlist and an identity check; never
/// "every `lean-ctx` process": the MCP stdio servers of running AI-tool sessions (bare
/// `lean-ctx`, `lean-ctx mcp`), hook processes and in-flight command runners
/// (`lean-ctx -c …`, #1292) are not services and are never selected. On
/// Windows the former MCP blocklist relied on `wmic`, which current Windows
/// releases no longer ship; every failed lookup made Claude Code's MCP servers
/// "orphans" that `restart` terminated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceKind {
    /// `lean-ctx serve --_foreground-daemon` — the daemon `daemon start`,
    /// the autostart unit and editors start.
    Daemon,
    /// `lean-ctx proxy start …` — the API proxy.
    Proxy,
    /// `lean-ctx serve …` without the daemon marker — an HTTP server the user
    /// started deliberately.
    HttpServer,
    /// `lean-ctx cockpit --foreground …` — the detached Cockpit server.
    Cockpit,
}

/// Which services a lifecycle command stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceSweep {
    /// The services the command starts again afterwards (`restart`,
    /// `config apply`): daemon and proxy.
    Restartable,
    /// Every lean-ctx background service (`stop`, `uninstall`,
    /// `dev-install`).
    All,
    /// One kind only: `daemon stop` (the daemon PID file), `proxy stop`
    /// and the managed-proxy port hand-off (the `/health` PID).
    Only(ServiceKind),
}

impl ServiceSweep {
    #[must_use]
    pub fn includes(self, kind: ServiceKind) -> bool {
        match self {
            Self::All => true,
            Self::Restartable => matches!(kind, ServiceKind::Daemon | ServiceKind::Proxy),
            Self::Only(only) => kind == only,
        }
    }
}

/// A PID a lean-ctx instance recorded for one of its services, with the
/// process identity recorded next to it when the record carries one (the
/// daemon identity file, the Cockpit launch record). A record without an
/// identity (a `/health` answer, a daemon PID file of an earlier release) is
/// gated by command line, executable, user and live identity only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedPid {
    pub pid: u32,
    pub identity: Option<ProcessIdentity>,
}

impl RecordedPid {
    #[must_use]
    pub fn bare(pid: u32) -> Self {
        Self {
            pid,
            identity: None,
        }
    }
}

/// Split a process command line into words. Double quotes group a word with
/// spaces (Windows `"C:\Program Files\…\lean-ctx.exe"`); backslashes are path
/// separators, not escapes.
pub(super) fn split_command_line(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut has_word = false;
    for ch in command.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                has_word = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_word {
                    words.push(std::mem::take(&mut current));
                    has_word = false;
                }
            }
            c => {
                current.push(c);
                has_word = true;
            }
        }
    }
    if has_word {
        words.push(current);
    }
    words
}

/// True when `path`'s file name is the lean-ctx executable (`lean-ctx`, or
/// `lean-ctx.exe` in any case), also as the image an update replaced
/// ([`installed_executable_path`]).
pub(super) fn is_lean_ctx_executable(path: &str) -> bool {
    let path = installed_executable_path(path);
    let name = path.rsplit(['/', '\\']).next().unwrap_or(&path);
    let name = name.to_ascii_lowercase();
    name == "lean-ctx" || name == "lean-ctx.exe"
}

/// The arguments after the lean-ctx executable in `command`.
pub(super) fn service_args(command: &str) -> Vec<String> {
    let words = split_command_line(command);
    words
        .iter()
        .position(|w| is_lean_ctx_executable(w))
        .map(|at| words[at + 1..].to_vec())
        .unwrap_or_default()
}

/// Classify a process command line as a lean-ctx background service, or
/// `None` for everything else — MCP stdio servers, hooks, command runners,
/// one-shot CLI calls and foreign programs. Pure, so the selection rules are
/// covered on every platform.
///
/// The executable is the first word whose file name is `lean-ctx`: `ps`
/// prints Unix paths unquoted, so a directory with a space splits the path
/// into several words of which only the last names the binary.
#[must_use]
pub fn classify_service_cmdline(command: &str) -> Option<ServiceKind> {
    let words = split_command_line(command);
    let exe_at = words.iter().position(|w| is_lean_ctx_executable(w))?;
    let args = &words[exe_at + 1..];
    let has = |flag: &str| args.iter().any(|a| a == flag);
    match args.first()?.as_str() {
        "serve" => {
            // `serve --stop/--status/--daemon` are one-shot CLI calls.
            if ["--stop", "--status", "--daemon", "-d", "--help", "-h"]
                .iter()
                .any(|f| has(f))
            {
                None
            } else if has("--_foreground-daemon") {
                Some(ServiceKind::Daemon)
            } else {
                Some(ServiceKind::HttpServer)
            }
        }
        "proxy" => (args.get(1).map(String::as_str) == Some("start")).then_some(ServiceKind::Proxy),
        "cockpit" | "dashboard" => has("--foreground").then_some(ServiceKind::Cockpit),
        _ => None,
    }
}

/// One process selected for a lifecycle stop, with the identity it had when
/// it was selected so a recycled PID is never signalled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceProcess {
    pub pid: u32,
    pub kind: ServiceKind,
    /// The arguments after the executable, e.g. `proxy start --port=4444`;
    /// `restart` starts a proxy it stopped again with these.
    pub args: Vec<String>,
    pub(super) identity: ProcessIdentity,
}

impl ServiceProcess {
    /// The selected process still runs (same PID, same identity).
    #[must_use]
    pub fn is_running(&self) -> bool {
        is_same_process(self.pid, &self.identity)
    }

    /// Deliver `signal` only while the PID is still this process.
    pub fn signal(&self, signal: StopSignal) -> bool {
        signal_same_process(self.pid, &self.identity, signal)
    }
}

/// One process considered for a lifecycle stop or listing: what the OS says
/// about it now, and the identity its record carried, if any.
#[derive(Clone, Debug)]
pub(super) struct Candidate {
    pub(super) pid: u32,
    pub(super) command: String,
    pub(super) live: Option<ProcessIdentity>,
    pub(super) recorded: Option<ProcessIdentity>,
    pub(super) same_user: bool,
}

/// Pure selection: keep the candidates that are lean-ctx background
/// services `sweep` includes. A candidate is dropped when it is protected
/// (this process, its ancestors, its process group), runs as another user,
/// has an unknown identity (fail closed), has an identity executable that is
/// not lean-ctx (a shell whose arguments merely mention the binary), or no
/// longer is the process its record names (PID reuse).
pub(super) fn select_services(
    candidates: Vec<Candidate>,
    protected: &std::collections::HashSet<u32>,
    sweep: ServiceSweep,
) -> Vec<ServiceProcess> {
    candidates
        .into_iter()
        .filter(|c| !protected.contains(&c.pid) && c.same_user)
        .filter_map(|c| {
            let identity = c.live?;
            if !is_lean_ctx_executable(&identity.executable) {
                return None;
            }
            if let Some(recorded) = &c.recorded
                && !same_process(recorded, &identity)
            {
                return None;
            }
            let kind = classify_service_cmdline(&c.command)?;
            sweep.includes(kind).then(|| ServiceProcess {
                pid: c.pid,
                kind,
                args: service_args(&c.command),
                identity,
            })
        })
        .collect()
}

/// Command lines of `pids`, as `(pid, command line)`, from one `ps` call. A
/// PID whose command line cannot be read is left out, so it is never
/// selected.
#[cfg(unix)]
pub(super) fn command_lines(pids: &[u32]) -> Vec<(u32, String)> {
    if pids.is_empty() {
        return Vec::new();
    }
    let list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut cmd = std::process::Command::new("ps");
    cmd.args(["-o", "pid=", "-o", "command=", "-p", &list]);
    let Some(out) = run_with_timeout(cmd, std::time::Duration::from_secs(5)) else {
        return Vec::new();
    };
    parse_ps_pid_command(&String::from_utf8_lossy(&out.stdout), pids)
}

/// Parse `ps -o pid= -o command=` output, keeping only the asked-for PIDs.
#[cfg(unix)]
pub(super) fn parse_ps_pid_command(stdout: &str, pids: &[u32]) -> Vec<(u32, String)> {
    stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, command) = line.split_once(char::is_whitespace)?;
            let pid = pid.parse::<u32>().ok()?;
            let command = command.trim();
            (pids.contains(&pid) && !command.is_empty()).then(|| (pid, command.to_string()))
        })
        .collect()
}

/// Windows has no `ps`, and `wmic` is gone from Windows Server 2025 and
/// current Windows 11 — the old MCP blocklist failed open on exactly that
/// (#2176). The command line is read in-process with
/// `NtQueryInformationProcess(ProcessCommandLineInformation)`: no external
/// tool, no PowerShell execution policy, and `PROCESS_QUERY_LIMITED_INFORMATION`
/// suffices for the caller's own processes.
#[cfg(windows)]
pub(super) fn command_lines(pids: &[u32]) -> Vec<(u32, String)> {
    pids.iter()
        .filter_map(|&pid| windows_command_line(pid).map(|c| (pid, c)))
        .filter(|(_, c)| !c.trim().is_empty())
        .collect()
}

#[cfg(windows)]
pub(super) fn windows_command_line(pid: u32) -> Option<String> {
    use windows_sys::Wdk::System::Threading::{
        NtQueryInformationProcess, ProcessCommandLineInformation,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, UNICODE_STRING};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    const MAX_BYTES: u32 = 1 << 20;

    // SAFETY: the handle is null-checked and closed on every path below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut needed = 0_u32;
    // SAFETY: a zero-length query only writes the required size to `needed`.
    unsafe {
        NtQueryInformationProcess(
            handle,
            ProcessCommandLineInformation,
            std::ptr::null_mut(),
            0,
            &raw mut needed,
        );
    }
    if needed == 0 || needed > MAX_BYTES {
        // SAFETY: `handle` came from OpenProcess and is still open.
        unsafe { CloseHandle(handle) };
        return None;
    }
    // u64 storage keeps the UNICODE_STRING header aligned.
    let mut buf = vec![0_u64; (needed as usize).div_ceil(8)];
    let buf_bytes = buf.len() * 8;
    let buf_len = u32::try_from(buf_bytes).unwrap_or(MAX_BYTES);
    let mut written = 0_u32;
    // SAFETY: `buf` is writable for `buf_len` bytes, which is passed along.
    let status = unsafe {
        NtQueryInformationProcess(
            handle,
            ProcessCommandLineInformation,
            buf.as_mut_ptr().cast(),
            buf_len,
            &raw mut written,
        )
    };
    // SAFETY: `handle` came from OpenProcess and is still open.
    unsafe { CloseHandle(handle) };
    if status < 0 {
        return None;
    }
    // SAFETY: on success the buffer starts with an initialised UNICODE_STRING.
    let header = unsafe { &*buf.as_ptr().cast::<UNICODE_STRING>() };
    let chars = usize::from(header.Length) / 2;
    let start = header.Buffer as usize;
    let base = buf.as_ptr() as usize;
    if header.Buffer.is_null() || start < base || start + chars * 2 > base + buf_bytes {
        return None;
    }
    // SAFETY: the bounds were checked above; the text lies inside `buf`.
    let text = unsafe { std::slice::from_raw_parts(header.Buffer, chars) };
    Some(String::from_utf16_lossy(text))
}

#[cfg(not(any(unix, windows)))]
pub(super) fn command_lines(_pids: &[u32]) -> Vec<(u32, String)> {
    Vec::new()
}

/// Of the recorded `owned` PIDs (daemon PID file, proxy `/health` on this
/// instance's port, Cockpit launch record), the ones that still are lean-ctx
/// background services `sweep` includes. Command line, executable, user and
/// identity (the recorded one, where the record has it) gate every PID
/// again, so a stale record whose PID now belongs to an MCP server, a command
/// runner or a foreign process is never selected. Never this process or its
/// ancestors, never a process whose identity cannot be read.
#[must_use]
pub fn find_service_processes(owned: &[RecordedPid], sweep: ServiceSweep) -> Vec<ServiceProcess> {
    let mut recorded: std::collections::BTreeMap<u32, Option<ProcessIdentity>> =
        std::collections::BTreeMap::new();
    for record in owned.iter().filter(|r| r.pid > 0) {
        let slot = recorded.entry(record.pid).or_default();
        if slot.is_none() {
            slot.clone_from(&record.identity);
        }
    }
    let pids: Vec<u32> = recorded.keys().copied().collect();
    let candidates = command_lines(&pids)
        .into_iter()
        .map(|(pid, command)| Candidate {
            pid,
            command,
            live: identity(pid),
            recorded: recorded.get(&pid).cloned().flatten(),
            same_user: owned_by_current_user(pid),
        })
        .collect();
    select_services(candidates, &protected_self_pids(), sweep)
}

/// The process a record written by that process itself names (the Cockpit
/// launch record), proven by the identity in the record rather than by a
/// command-line kind: this user, lean-ctx executable, same process as
/// recorded, never this process or its ancestors. `None` otherwise.
#[must_use]
pub fn recorded_process(
    pid: u32,
    recorded: &ProcessIdentity,
    kind: ServiceKind,
) -> Option<ServiceProcess> {
    if pid == 0 || protected_self_pids().contains(&pid) || !owned_by_current_user(pid) {
        return None;
    }
    let live = identity(pid)?;
    (is_lean_ctx_executable(&live.executable) && same_process(recorded, &live)).then(|| {
        ServiceProcess {
            pid,
            kind,
            args: Vec::new(),
            identity: live,
        }
    })
}

/// PIDs of every process of this user whose image is lean-ctx, including an
/// image an update renamed. Listing only — callers never signal these.
pub(super) fn lean_ctx_process_pids() -> Vec<u32> {
    #[cfg(windows)]
    {
        windows_lean_ctx_process_pids()
    }
    #[cfg(not(windows))]
    {
        // Same user only (pgrep -U); `comm` keeps `lean-ctx` after the binary
        // was replaced, and the `-f` pattern catches absolute-path launches.
        find_pids_by_name("lean-ctx")
    }
}

/// Toolhelp snapshot instead of `tasklist`: no external tool, and the image
/// names an update leaves behind (`lean-ctx.old.exe`, parked sidecars) are
/// included. The user check happens in the selection.
#[cfg(windows)]
pub(super) fn windows_lean_ctx_process_pids() -> Vec<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    // SAFETY: plain flags; the snapshot handle is checked and closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
        return Vec::new();
    }
    let protected = protected_self_pids();
    let mut pids = Vec::new();
    // SAFETY: PROCESSENTRY32W is plain data; zeroed is a valid initial state.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).unwrap_or(0);
    // SAFETY: valid snapshot handle and a sized, writable entry.
    let mut more = unsafe { Process32FirstW(snapshot, &raw mut entry) } != 0;
    while more {
        let len = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
        if is_lean_ctx_executable(&name) && !protected.contains(&entry.th32ProcessID) {
            pids.push(entry.th32ProcessID);
        }
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot, &raw mut entry) } != 0;
    }
    // SAFETY: `snapshot` came from CreateToolhelp32Snapshot and is still open.
    unsafe { CloseHandle(snapshot) };
    pids
}

/// lean-ctx background services of this user that no record of this
/// instance names (`recorded`): a foreground `lean-ctx serve`, a proxy on
/// another port, another data dir's daemon, a duplicate from a start race.
///
/// Enumerate and classify only — the lifecycle commands never stop these
/// (owner decision 02.11.: only recorded services are ever signalled); `stop`,
/// `uninstall` and `doctor` list them with the way to stop them. MCP stdio
/// servers, hooks and command runners are not services and are never listed.
#[must_use]
pub fn find_unrecorded_services(recorded: &[u32]) -> Vec<ServiceProcess> {
    let pids: Vec<u32> = lean_ctx_process_pids()
        .into_iter()
        .filter(|pid| !recorded.contains(pid))
        .collect();
    let candidates = command_lines(&pids)
        .into_iter()
        .map(|(pid, command)| Candidate {
            pid,
            command,
            live: identity(pid),
            recorded: None,
            same_user: owned_by_current_user(pid),
        })
        .collect();
    select_services(candidates, &protected_self_pids(), ServiceSweep::All)
}

/// The warning `stop`/`uninstall` print and `doctor` shows for
/// [`find_unrecorded_services`], or `None` when there are none:
/// `(what is running, how to stop it)`. Pure; `windows` picks the hint.
#[must_use]
pub fn unrecorded_services_lines(
    services: &[ServiceProcess],
    windows: bool,
) -> Option<(String, String)> {
    let what = describe_services(
        "Still running, not started by this instance (left alone)",
        services,
    )?;
    let pids: Vec<String> = services.iter().map(|s| s.pid.to_string()).collect();
    let how = if windows {
        format!("Stop them with: taskkill /PID {}", pids.join(" /PID "))
    } else {
        format!("Stop them with: kill {}", pids.join(" "))
    };
    Some((what, how))
}

/// What `restart`/`config apply` does about a proxy it stopped: start it
/// again with the arguments it ran with, unless the proxy autostart service
/// brought it back (`autostart_restarted`). Without this, `restart` left the
/// proxy down on Windows and wherever no autostart is installed (#2176
/// review). `None` when there is nothing to start. Pure.
#[must_use]
pub fn proxy_respawn_args(
    stopped: &[ServiceProcess],
    autostart_restarted: bool,
) -> Option<Vec<String>> {
    if autostart_restarted {
        return None;
    }
    stopped
        .iter()
        .find(|s| s.kind == ServiceKind::Proxy)
        .map(|s| s.args.clone())
        .filter(|args| args.first().map(String::as_str) == Some("proxy"))
}

/// What [`stop_services`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ServiceStopReport {
    /// Services that were running and are gone now.
    pub stopped: Vec<ServiceProcess>,
    /// Services that survived terminate and kill.
    pub survivors: Vec<ServiceProcess>,
}

impl ServiceKind {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Proxy => "proxy",
            Self::HttpServer => "HTTP server",
            Self::Cockpit => "Cockpit",
        }
    }
}

impl ServiceStopReport {
    /// One line naming each service that was stopped, e.g.
    /// `Stopped lean-ctx services: daemon (PID 812), proxy (PID 900).`, or
    /// `None` when nothing was running.
    #[must_use]
    pub fn stopped_line(&self) -> Option<String> {
        describe_services("Stopped lean-ctx services", &self.stopped)
    }

    /// One line naming each service that is still running, or `None`.
    #[must_use]
    pub fn survivors_line(&self) -> Option<String> {
        describe_services("Could not stop", &self.survivors)
    }

    /// PIDs of the services that are still running.
    #[must_use]
    pub fn survivor_pids(&self) -> Vec<u32> {
        self.survivors.iter().map(|s| s.pid).collect()
    }
}

pub(super) fn describe_services(prefix: &str, services: &[ServiceProcess]) -> Option<String> {
    if services.is_empty() {
        return None;
    }
    let list = services
        .iter()
        .map(|s| format!("{} (PID {})", s.kind.label(), s.pid))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("{prefix}: {list}."))
}

/// Stop the recorded lean-ctx background services (`owned`) that `sweep`
/// includes (see [`find_service_processes`] and [`stop_selected`]). Nothing
/// is ever selected by process name (#2176).
pub fn stop_services(owned: &[RecordedPid], sweep: ServiceSweep) -> ServiceStopReport {
    stop_selected(find_service_processes(owned, sweep))
}

/// Stop selected services: graceful termination first, a kill for those
/// still running after a grace period. Every signal goes through
/// [`signal_same_process`], so a PID the OS reused in between is left alone.
pub fn stop_selected(targets: Vec<ServiceProcess>) -> ServiceStopReport {
    if targets.is_empty() {
        return ServiceStopReport::default();
    }
    for target in &targets {
        target.signal(StopSignal::Terminate);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline && targets.iter().any(ServiceProcess::is_running) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let mut killed = false;
    for target in targets.iter().filter(|t| t.is_running()) {
        killed |= target.signal(StopSignal::Kill);
    }
    if killed {
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    let (survivors, stopped) = targets.into_iter().partition(ServiceProcess::is_running);
    ServiceStopReport { stopped, survivors }
}
