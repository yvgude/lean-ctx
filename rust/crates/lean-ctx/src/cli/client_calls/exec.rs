// SPDX-License-Identifier: Apache-2.0
//! `client exec`: ACP `terminal/*` through the shell allowlist of
//! `ctx_shell`. The command runs as argv (no shell re-parse) in a working
//! directory inside the PathJail; its output (stdout and stderr merged) is
//! streamed raw — never compressed — as `client.exec.output` lines, then the
//! final `client.exec` document passes the release point and is receipted.
//!
//! The client stops a command by sending SIGTERM (or SIGINT/SIGHUP) to this
//! process: the command's process group is terminated and the final
//! document still names the signal.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;

use super::{Caller, Code, Failure, Outcome, audit, document, emit_released, fail};
use crate::cli::local_read::{flag, flags};

const TOOL: &str = "client.exec";
/// Default of `--output-byte-limit` (ACP leaves it to the client).
const DEFAULT_OUTPUT_LIMIT: usize = 1024 * 1024;
const MAX_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
/// A chunk is emitted at a line end, or once this much is pending, so the
/// redaction sees whole lines.
const CHUNK_FLUSH: usize = 64 * 1024;
/// How long output may still arrive after the command exited (a background
/// child holding the pipes open does not keep the call alive).
const DRAIN_GRACE: Duration = Duration::from_millis(500);
/// How long a terminated group gets before it is killed.
const KILL_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ExecResult {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub output: String,
    pub truncated: bool,
}

/// What `exec` was asked to run, after its arguments were checked.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ExecRequest {
    pub argv: Vec<String>,
    /// The text the shell gate checked for `argv` ([`checked_command`]).
    pub checked: String,
    pub cwd: String,
    pub env: Vec<(String, String)>,
    pub output_limit: usize,
}

/// Words a shell (and so the gate) reads as an assignment, a builtin or a
/// keyword rather than as the program: the gate would check the word after
/// it, while `exec` runs a program of that name.
const NOT_A_PROGRAM: &[&str] = &[
    "export", "unset", "readonly", "local", "declare", "typeset", "let", "alias", "builtin",
    "eval", "exec", "source", ".", "trap", "set", "shopt", "hash", "enable", "if", "then", "else",
    "elif", "fi", "for", "while", "until", "do", "done", "case", "esac", "function", "select",
    "coproc", "time", "!", "{", "}", "[[", "]]",
];

/// The text the shell gate checks for `argv`: POSIX-quoted (the gate parses
/// POSIX on every platform). Refused unless the gate reads back exactly the
/// argv that will run — no assignment, builtin or keyword in the program
/// position, and every argument one word as given.
pub(crate) fn checked_command(argv: &[String]) -> Result<String, Failure> {
    let Some(program) = argv.first().filter(|p| !p.is_empty()) else {
        return Err(Failure::invalid("no command after --"));
    };
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program);
    if program.contains('=') || NOT_A_PROGRAM.contains(&base) {
        return Err(Failure::invalid(format!(
            "{program:?} is not a program; to use the shell, run it: -- sh -c '…'"
        )));
    }
    let joined = lean_ctx::shell::join_command_for(argv, "-c");
    if lean_ctx_shell_policy::shell_tokenize(&joined) != argv {
        return Err(Failure::invalid(
            "an argument cannot be checked exactly as it would run (empty or unprintable)",
        ));
    }
    Ok(joined)
}

/// Check the arguments, the cwd jail, the environment and the allowlist.
/// Every refusal is recorded ([`super::audit_failure`]).
pub(crate) fn prepare(
    args: &[String],
    command: &[String],
    root: &str,
    caller: &Caller,
) -> Result<ExecRequest, Failure> {
    let named = lean_ctx::shell::join_command_for(command, "-c");
    prepare_checked(args, command, root, caller)
        .map_err(|f| super::audit_failure(TOOL, caller, &named, f))
}

fn prepare_checked(
    args: &[String],
    command: &[String],
    root: &str,
    caller: &Caller,
) -> Result<ExecRequest, Failure> {
    let wrap = |f: crate::cli::local_read::Failure| Failure::invalid(f.message);
    let joined = checked_command(command)?;
    let refuse = |outcome: Outcome, message: String| {
        audit(TOOL, caller, &joined, outcome);
        Failure::new(Code::Forbidden, message).recorded()
    };

    let cwd_raw = flag(args, "--cwd").map_err(wrap)?.unwrap_or(root);
    let cwd = lean_ctx::core::command_root::jail_cli_read_path(cwd_raw, root)
        .map_err(|message| refuse(Outcome::Jailed, message))?;
    if !Path::new(&cwd).is_dir() {
        return Err(Failure::invalid(format!("--cwd {cwd} is not a directory")));
    }

    let env = environment(&flags(args, "--env").map_err(wrap)?).map_err(|failure| match failure
        .code
    {
        Code::Forbidden => refuse(Outcome::Denied, failure.message),
        _ => failure,
    })?;

    let output_limit = match flag(args, "--output-byte-limit").map_err(wrap)? {
        None => DEFAULT_OUTPUT_LIMIT,
        Some(raw) => raw
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_OUTPUT_LIMIT).contains(n))
            .ok_or_else(|| {
                Failure::invalid(format!("--output-byte-limit is 1-{MAX_OUTPUT_LIMIT}"))
            })?,
    };

    // The ONE shell gate of `ctx_shell` and the CLI (`shell_security`
    // decides enforce/warn/off); a redaction marker is refused in every mode.
    lean_ctx_shell_policy::check_shell_allowlist(&joined)
        .map_err(|error| refuse(Outcome::Denied, error.message().to_owned()))?;

    Ok(ExecRequest {
        argv: command.to_vec(),
        checked: joined.clone(),
        cwd,
        env,
        output_limit,
    })
}

/// The `--env K=V` overrides, under the ONE override policy of `ctx_shell`'s
/// `env` ([`lean_ctx_shell_policy::env_override_refused`]): every
/// refused name is named, nothing runs (`forbidden`).
pub(crate) fn environment(pairs: &[&str]) -> Result<Vec<(String, String)>, Failure> {
    let mut env = Vec::new();
    let mut refused: Vec<&str> = Vec::new();
    for pair in pairs {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(Failure::invalid(format!("--env {pair:?} is not K=V")));
        };
        if key.is_empty() || key.contains('\0') || value.contains('\0') {
            return Err(Failure::invalid(format!(
                "--env {key:?} is not a valid name"
            )));
        }
        if lean_ctx_shell_policy::env_override_refused(key, value) {
            refused.push(key);
            continue;
        }
        env.push((key.to_owned(), value.to_owned()));
    }
    if refused.is_empty() {
        return Ok(env);
    }
    refused.sort_unstable();
    refused.dedup();
    Err(Failure::new(
        Code::Forbidden,
        format!(
            "--env {} refused: these variables redirect which binary, library or startup \
             code runs (the env policy of ctx_shell); nothing was run",
            refused.join(", ")
        ),
    ))
}

/// The output a client keeps: at most `limit` bytes from the end, cut at a
/// char boundary (ACP `terminal/output`). The buffer is trimmed only once
/// it holds twice the limit, so keeping the tail costs amortized O(1) per
/// byte, not a front shift per chunk.
#[derive(Debug, Default)]
pub(crate) struct Tail {
    text: String,
    limit: usize,
    truncated: bool,
}

impl Tail {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }

    pub(crate) fn push(&mut self, chunk: &str) {
        self.text.push_str(chunk);
        if self.text.len() > self.limit.saturating_mul(2) {
            self.trim();
        }
    }

    fn trim(&mut self) {
        if self.text.len() <= self.limit {
            return;
        }
        let mut cut = self.text.len() - self.limit;
        while !self.text.is_char_boundary(cut) {
            cut += 1;
        }
        self.text.drain(..cut);
        self.truncated = true;
    }

    /// The kept tail. A cut tail starts at a line start (unless that would
    /// leave nothing). The tail only ever holds what the stream sent — key
    /// blocks already redacted or withheld — so no cut can expose a key.
    pub(crate) fn finish(mut self) -> (String, bool) {
        self.trim();
        if self.truncated {
            let start = self.text.find('\n').map_or(0, |i| i + 1);
            if start < self.text.len() {
                self.text.drain(..start);
            }
        }
        (self.text, self.truncated)
    }
}

/// Decodes a byte stream as UTF-8 across read boundaries: an incomplete
/// sequence at the end of a read waits for the next one; invalid bytes
/// become U+FFFD.
#[derive(Debug, Default)]
pub(crate) struct Utf8Stream {
    carry: Vec<u8>,
}

impl Utf8Stream {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> String {
        self.carry.extend_from_slice(bytes);
        let valid = match std::str::from_utf8(&self.carry) {
            // An incomplete sequence at the end waits for the next read.
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            _ => self.carry.len(),
        };
        let rest = self.carry.split_off(valid);
        let text = String::from_utf8_lossy(&self.carry).into_owned();
        self.carry = rest;
        text
    }

    pub(crate) fn finish(&mut self) -> String {
        let text = String::from_utf8_lossy(&self.carry).into_owned();
        self.carry.clear();
        text
    }
}

#[cfg(unix)]
pub(crate) mod stop {
    use std::sync::atomic::{AtomicI32, Ordering};

    /// The signal the client stopped the call with, 0 while none.
    static STOPPED: AtomicI32 = AtomicI32::new(0);

    extern "C" fn on_signal(signal: libc::c_int) {
        STOPPED.store(signal, Ordering::SeqCst);
    }

    pub(super) fn install() {
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            // SAFETY: the handler only stores into an atomic, which is
            // async-signal-safe; it is installed before the command starts.
            unsafe {
                libc::signal(signal, on_signal as *const () as libc::sighandler_t);
            }
        }
    }

    /// Act as if the client had sent `signal` (0 clears it).
    #[cfg(test)]
    pub(crate) fn simulate(signal: i32) {
        STOPPED.store(signal, Ordering::SeqCst);
    }

    pub(super) fn requested() -> Option<i32> {
        match STOPPED.load(Ordering::SeqCst) {
            0 => None,
            signal => Some(signal),
        }
    }

    pub(super) fn group(pid: u32, signal: i32) {
        // `group_target` refuses 0 (killpg(0) = our own group), 1, ids above
        // i32::MAX and the group this process belongs to.
        if let Ok(pid) = lean_ctx::ipc::process::group_target(pid) {
            // SAFETY: killpg on the group this process created for the
            // command; an already gone group only returns ESRCH.
            unsafe {
                libc::killpg(pid, signal);
            }
        }
    }

    pub(super) fn name(signal: i32) -> String {
        match signal {
            libc::SIGTERM => "SIGTERM".into(),
            libc::SIGINT => "SIGINT".into(),
            libc::SIGHUP => "SIGHUP".into(),
            libc::SIGKILL => "SIGKILL".into(),
            libc::SIGSEGV => "SIGSEGV".into(),
            libc::SIGABRT => "SIGABRT".into(),
            libc::SIGPIPE => "SIGPIPE".into(),
            other => format!("SIG{other}"),
        }
    }
}

/// One stream of the command, read in its own thread; chunks go to `tx`.
fn pump(mut source: impl Read + Send + 'static, tx: mpsc::Sender<String>) {
    std::thread::spawn(move || {
        let mut decoder = Utf8Stream::default();
        let mut buffer = [0u8; 8192];
        loop {
            match source.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let text = decoder.push(&buffer[..n]);
                    if !text.is_empty() && tx.send(text).is_err() {
                        return;
                    }
                }
            }
        }
        let rest = decoder.finish();
        if !rest.is_empty() {
            let _ = tx.send(rest);
        }
    });
}

/// Print one `client.exec.output` line with `data`, redacted like every
/// text shown to an agent.
fn emit_chunk(data: &str) {
    let data = lean_ctx_server::server::policy_guard::redact_model_text(data);
    let line = document("client.exec.output", &json!({ "data": data }));
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// The command's output on its way out, line by line: lines are streamed
/// as chunks, and the tail for the final document keeps exactly what was
/// streamed (key blocks already redacted or withheld), so no cut of the
/// tail can expose a key.
///
/// Redaction runs per chunk, so a chunk never ends inside a rule that
/// continues on the next line. A line that opens a private-key block (the
/// "Private key block" rule's own markers, normalized as it sees them —
/// `core::redaction::ends_inside_private_key_block`) starts a hold, whatever
/// its lines look like:
///
/// - When a private-key END line comes within [`HOLD_LINES`] lines or
///   [`HOLD_BYTES`], the whole held text goes through the redaction as one
///   chunk (header to END masked). A certificate's END does not close it.
/// - Otherwise — the bound passed, or the output ended — the block is
///   released line by line: the header line streams redacted (a grep hit
///   keeps its `file:line:` prefix), every line with a base64 run of
///   [`KEY_RUN`] characters or more is withheld, everything else is
///   streamed. Until the block's END or [`HOLD_LINES`] released lines in a
///   row, later lines follow the same rule; a new header starts its own
///   hold.
///
/// A line that ends inside the classification field is held with the next
/// one (bounded). Each line is looked at once (linear in the output).
pub(crate) struct Output {
    tail: Tail,
    sink: fn(&str),
    /// The incomplete last line, and how much of it was searched for a
    /// line end already.
    partial: String,
    scanned: usize,
    /// Lines held for an open classification field.
    carry: String,
    state: KeyState,
    /// The last thing streamed was the withheld notice.
    last_withheld: bool,
}

enum KeyState {
    Normal,
    /// A key block is held from its header line on.
    Holding {
        text: String,
        lines: usize,
    },
    /// A key block passed its bound without END: key-like lines withheld.
    Filtering {
        released_in_a_row: usize,
    },
}

/// Lines after a header the hold waits for the block's END.
const HOLD_LINES: usize = 128;
/// Bytes a hold keeps before the block is released line by line.
const HOLD_BYTES: usize = 64 * 1024;
/// A base64 run this long marks a line of key material.
const KEY_RUN: usize = 40;
/// Most text carried for an open classification field.
const CARRY_CAP: usize = 4 * 1024;
/// What the stream says in place of withheld key material.
pub(crate) const WITHHELD_CHUNK: &str =
    "[lean-ctx: output withheld from the stream: private-key material without its end marker]\n";

/// Whether `line` holds a run of [`KEY_RUN`] base64 characters.
fn has_key_run(line: &str) -> bool {
    let mut run = 0;
    for b in line.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=') {
            run += 1;
            if run >= KEY_RUN {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

impl Output {
    pub(crate) fn new(limit: usize, sink: fn(&str)) -> Self {
        Self {
            tail: Tail::new(limit),
            sink,
            partial: String::new(),
            scanned: 0,
            carry: String::new(),
            state: KeyState::Normal,
            last_withheld: false,
        }
    }

    /// Stream `text` (the sink redacts it) and keep `kept` for the tail.
    fn send(&mut self, text: &str, kept: &str) {
        if text.is_empty() {
            return;
        }
        (self.sink)(text);
        self.tail.push(kept);
        self.last_withheld = false;
    }

    fn withhold(&mut self) {
        if !self.last_withheld {
            (self.sink)(WITHHELD_CHUNK);
            self.tail.push(WITHHELD_CHUNK);
            self.last_withheld = true;
        }
    }

    fn flush_carry(&mut self) {
        let carry = std::mem::take(&mut self.carry);
        self.send(&carry, &carry);
    }

    pub(crate) fn push(&mut self, chunk: &str) {
        self.partial.push_str(chunk);
        while let Some(at) = self.partial[self.scanned..].find('\n') {
            let end = self.scanned + at + 1;
            let line: String = self.partial.drain(..end).collect();
            self.scanned = 0;
            self.line(&line);
        }
        self.scanned = self.partial.len();
        // A line without an end that grows past the chunk size is cut at
        // white space (a token is never split), else as a whole.
        if self.partial.len() >= CHUNK_FLUSH {
            let cut = self
                .partial
                .char_indices()
                .rev()
                .find(|(_, c)| c.is_whitespace())
                .map_or(self.partial.len(), |(i, c)| i + c.len_utf8());
            let line: String = self.partial.drain(..cut).collect();
            self.scanned = self.partial.len();
            self.line(&line);
        }
    }

    fn line(&mut self, line: &str) {
        use lean_ctx::core::redaction::{contains_private_key_end, ends_inside_private_key_block};
        match &mut self.state {
            KeyState::Holding { text, lines } => {
                text.push_str(line);
                *lines += 1;
                if contains_private_key_end(line) {
                    let held = std::mem::take(text);
                    self.state = KeyState::Normal;
                    let shown = lean_ctx_server::server::policy_guard::redact_model_text(&held);
                    self.send(&held, &shown);
                    // `…END…-----BEGIN …` on one line opens the next block.
                    if ends_inside_private_key_block(line) {
                        self.state = KeyState::Holding {
                            text: String::new(),
                            lines: 0,
                        };
                    }
                } else if *lines >= HOLD_LINES || text.len() >= HOLD_BYTES {
                    let held = std::mem::take(text);
                    self.release(&held);
                }
                return;
            }
            KeyState::Filtering { .. } => {
                self.filter_line(line);
                return;
            }
            KeyState::Normal => {}
        }
        if ends_inside_private_key_block(line) {
            self.flush_carry();
            self.state = KeyState::Holding {
                text: line.to_owned(),
                lines: 0,
            };
            return;
        }
        self.carry.push_str(line);
        if self.carry.len() < CARRY_CAP
            && lean_ctx::core::rooted_read::unit_ends_inside_a_rule(line)
        {
            return;
        }
        self.flush_carry();
    }

    /// A held block that passed its bound or the end of the output without
    /// END: its header line streams redacted (a grep hit keeps its
    /// `file:line:` prefix), its other lines are filtered. A later header
    /// among them starts a hold of its own.
    fn release(&mut self, held: &str) {
        use lean_ctx::core::redaction::ends_inside_private_key_block;
        self.state = KeyState::Filtering {
            released_in_a_row: 0,
        };
        let mut lines = held.split_inclusive('\n');
        if let Some(first) = lines.next() {
            if ends_inside_private_key_block(first) {
                let shown = lean_ctx_server::server::policy_guard::redact_model_text(first);
                self.send(&shown, &shown);
            } else {
                self.line(first);
            }
        }
        for line in lines {
            self.line(line);
        }
    }

    /// One line after a key block's bound: key-like lines are withheld.
    fn filter_line(&mut self, line: &str) {
        use lean_ctx::core::redaction::{contains_private_key_end, ends_inside_private_key_block};
        if contains_private_key_end(line) {
            self.withhold();
            self.state = KeyState::Normal;
            if ends_inside_private_key_block(line) {
                self.state = KeyState::Holding {
                    text: String::new(),
                    lines: 0,
                };
            }
            return;
        }
        if ends_inside_private_key_block(line) {
            self.state = KeyState::Holding {
                text: line.to_owned(),
                lines: 0,
            };
            return;
        }
        if has_key_run(line) {
            self.withhold();
            if let KeyState::Filtering { released_in_a_row } = &mut self.state {
                *released_in_a_row = 0;
            }
            return;
        }
        self.send(line, line);
        if let KeyState::Filtering { released_in_a_row } = &mut self.state {
            *released_in_a_row += 1;
            if *released_in_a_row >= HOLD_LINES {
                self.state = KeyState::Normal;
            }
        }
    }

    pub(crate) fn finish(mut self) -> (String, bool) {
        let rest = std::mem::take(&mut self.partial);
        if !rest.is_empty() {
            self.line(&rest);
        }
        // The end of the output cut a block off without END; releasing it
        // can open the hold of a later header, which is released in turn.
        while let KeyState::Holding { text, .. } =
            std::mem::replace(&mut self.state, KeyState::Normal)
        {
            self.release(&text);
        }
        self.flush_carry();
        self.tail.finish()
    }
}

fn spawn(request: &ExecRequest) -> std::io::Result<std::process::Child> {
    let mut command = Command::new(&request.argv[0]);
    command
        .args(&request.argv[1..])
        .current_dir(&request.cwd)
        .envs(request.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command.spawn()
}

/// Run the prepared command to its end, streaming its output to `sink`.
pub(crate) fn run(
    request: &ExecRequest,
    caller: &Caller,
    sink: fn(&str),
) -> Result<ExecResult, Failure> {
    #[cfg(unix)]
    stop::install();

    let joined = &request.checked;
    let mut child = spawn(request).map_err(|error| {
        audit(TOOL, caller, joined, Outcome::Failed);
        let code = if error.kind() == std::io::ErrorKind::NotFound {
            Code::NotFound
        } else {
            Code::Internal
        };
        Failure::new(code, format!("{}: {error}", request.argv[0])).recorded()
    })?;

    let (tx, rx) = mpsc::channel::<String>();
    if let Some(stdout) = child.stdout.take() {
        pump(stdout, tx.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        pump(stderr, tx.clone());
    }
    drop(tx);

    let mut output = Output::new(request.output_limit, sink);
    let mut status = None;
    let mut exited_at: Option<Instant> = None;
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut stopping = false;
    let mut killed_at: Option<Instant> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(chunk) => output.push(&chunk),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if status.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        #[cfg(unix)]
        if !stopping && stop::requested().is_some() {
            stopping = true;
            killed_at = Some(Instant::now());
            stop::group(child.id(), libc::SIGTERM);
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(exit)) => {
                    status = Some(exit);
                    exited_at = Some(Instant::now());
                }
                Ok(None) => {}
                Err(error) => return Err(Failure::new(Code::Internal, error.to_string())),
            }
        }
        if killed_at.is_some_and(|t| t.elapsed() >= KILL_GRACE) {
            #[cfg(unix)]
            stop::group(child.id(), libc::SIGKILL);
            #[cfg(not(unix))]
            let _ = child.kill();
            killed_at = None;
        }
        if exited_at.is_some_and(|t| t.elapsed() >= DRAIN_GRACE) {
            // The command exited, but a background child of its group still
            // holds the pipes: end the group rather than leave it running
            // where terminal/release can no longer reach it. The group
            // still has members (they hold the pipes), so its id is not
            // reused yet.
            #[cfg(unix)]
            {
                stop::group(child.id(), libc::SIGTERM);
                std::thread::sleep(Duration::from_millis(200));
                stop::group(child.id(), libc::SIGKILL);
            }
            break;
        }
    }
    let (output, truncated) = output.finish();
    let status = match status {
        Some(status) => status,
        None => child
            .wait()
            .map_err(|error| Failure::new(Code::Internal, error.to_string()))?,
    };

    let exit_code = status.code();
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&status).map(stop::name);
    #[cfg(not(unix))]
    let signal: Option<String> = {
        let _ = stopping;
        None
    };
    let (outcome, text) = match (exit_code, &signal) {
        (Some(0), _) => (Outcome::Done, "exit 0".to_owned()),
        (Some(code), _) => (Outcome::Failed, format!("exit {code}")),
        (None, Some(name)) if stopping => (Outcome::Cancelled, name.clone()),
        (None, signal) => (
            Outcome::Failed,
            signal.clone().unwrap_or_else(|| "no exit code".to_owned()),
        ),
    };
    super::audit_with(TOOL, caller, joined, outcome, Some(text));
    Ok(ExecResult {
        exit_code,
        signal,
        output,
        truncated,
    })
}

/// `lean-ctx client exec --json … [--cwd D] [--env K=V]… [--output-byte-limit N] -- CMD ARG…`.
pub(super) fn cmd_exec(args: &[String], command: &[String], caller: &Caller) -> ! {
    let root = &caller.project_root;
    let request = prepare(args, command, root, caller).unwrap_or_else(|failure| fail(&failure));
    let result = run(&request, caller, emit_chunk)
        .map_err(|f| super::audit_failure(TOOL, caller, &request.checked, f));
    emit_released("ctx_shell", root, "client.exec", || result)
}
