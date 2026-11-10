# Client Calls v1

Status: **experimental** (`"contract": "client-calls/1"`).

An ACP client on this machine, such as LeanCTX | Hive Desktop, makes the
calls an agent asks it for — `fs/read_text_file`, `fs/write_text_file`,
`terminal/*` — through this Instance instead of the plain file system and
shell. Every call passes the same boundaries as `ctx_read`, `ctx_edit` and
`ctx_shell`: the PathJail (project root plus the configured extra and
read-only roots), the secret-path policy, the shell allowlist
(`shell_security`), the context policy (gateway, redaction) and the Decision
Receipt of the CLI tool run. Every call with a valid `--session` and
`--agent`, a refused one included (a bad argument or input too), writes
one signed audit-trail entry attributed to the client's session and agent.

The client spawns `lean-ctx client …` and reads stdout.

## Common rules

- `--json` is required (the only output form). `--session <id>` (the ACP
  session id) and `--agent <id>` (the agent the client runs) are required:
  1–200 bytes, no control characters. They attribute the call in the audit
  trail; they grant nothing.
- `--project-root P`: the project whose jail applies (default: detected from
  the cwd). A broad root (home, `/`) is `no_project`.
- One JSON object per line on stdout, each with `"contract": "client-calls/1"`
  and `"kind"`. Nullable fields are always present.
- Errors: exit 1 (2 for a bad argument) and one line
  ```json
  {"contract":"client-calls/1","kind":"error","error":{"code":"forbidden","message":"human text"}}
  ```
  `code` ∈ `invalid_argument` | `no_project` | `forbidden` (PathJail,
  read-only root, secret path, shell allowlist, reserved env, redaction
  marker) | `withheld` (the context policy did not admit the content) |
  `not_found` | `too_large` | `internal`.
- A document the release point withholds as a whole prints nothing on stdout;
  the content-free reason goes to stderr (exit 1).
- Additive rule: consumers ignore unknown fields; a breaking change bumps to
  `client-calls/2`.
- Audit trail: `tool` = `client.fs_read` | `client.fs_write` | `client.exec`,
  `agent_id` = `--agent`, `action` = `session=<--session> outcome=<done|failed|cancelled|jailed|denied>`,
  `event_type` `tool_call` (done, failed), `path_jail_violation` (jailed) or
  `tool_denied` (denied), `input_hash` over the path or the command, never
  the content.
- Activity stream (`lean-ctx activity list --json`, Local Read v1): one event
  per call in the session `--session` of the project, `source` `acp`, `kind`
  `file_read` | `file_write` | `shell`, `tool` as above, `target` the path or
  the (redacted) command, `outcome` `ok` | `error` | `denied` | `cancelled`;
  an exec adds `text` `exit <code>` or the signal name.

---

## 1. `lean-ctx client fs-read --json --session S --agent A --path ABS [--line N] [--limit N]`

ACP `fs/read_text_file`. `--path` must be absolute; `--line` is 1-based,
`--limit` a number of lines (both positive).

```json
{
  "contract": "client-calls/1",
  "kind": "client.fs_read",
  "path": "/Users/me/code/app/src/main.rs",
  "content": "fn main() {}\n",
  "line": null,
  "limit": null,
  "total_lines": 1
}
```

- `content` is the file's text **verbatim**: never a compressed or
  mode-reduced view. Only the context policy may change it (a redacted
  secret), as for every agent read.
- With `line`/`limit`, `content` is exactly those lines, each with its own
  line ending (`\n` or `\r\n`); past the end it is `""`.
- `path` is the resolved path inside the jail. `total_lines` counts the
  delivered text.
- Bounded: a file larger than the configured read limit
  (`LEAN_CTX_MAX_READ_BYTES`, default 8 MiB) is `too_large`. Bytes that are
  not UTF-8 are `invalid_argument` (a lossy view written back would corrupt
  the file).

## 2. `lean-ctx client fs-write --json --session S --agent A --path ABS`

ACP `fs/write_text_file`. The new content is read from **stdin** (UTF-8, at
most the read limit) and replaces the whole file.

```json
{
  "contract": "client-calls/1",
  "kind": "client.fs_write",
  "path": "/Users/me/code/app/src/main.rs",
  "bytes": 13,
  "created": false
}
```

- Written like `ctx_edit create=true`: atomically (temp file + rename), the
  file's permissions kept, missing parent directories inside the jail
  created, same-file writes serialized.
- A read-only root is `forbidden`. Content holding a redaction marker such
  as `[REDACTED:Database credentials]` is `forbidden`: it is what an agent
  shown a redacted view would write back over the real value. The one
  exception is source code that must hold the marker literal: a Rust file
  (`*.rs`) or a test file (see the security reference, "Redaction markers").

## 3. `lean-ctx client exec --json --session S --agent A [--cwd D] [--env K=V]… [--output-byte-limit N] -- CMD [ARG…]`

ACP `terminal/create` … `terminal/release`. The command is run as argv (no
shell re-parse; to use a shell, run it: `-- bash -lc '…'`), stdin closed,
stdout and stderr merged, no PTY.

- Before anything runs:
  - `--cwd` (default: the project root) must resolve inside the jail and be
    a directory;
  - the program word must be a program: a word with `=`, a shell builtin or
    keyword (`export`, `declare`, `eval`, `exec`, `if`, …) is
    `invalid_argument`, and so is an argv the gate cannot read back exactly
    (e.g. an empty argument) — the gate checks exactly the argv that runs;
  - the command passes the shell allowlist exactly as `ctx_shell`
    (`shell_security` enforce / warn / off; a redaction marker is refused in
    every mode), on its POSIX-quoted argv;
  - every `--env` name passes the env-override policy of `ctx_shell`'s `env`
    (`core::shell_allowlist::env_override_refused`), else the call is
    `forbidden` and names every refused variable. Refused: `PATH`,
    `HOME`/`USER`/`LOGNAME`/`XDG_*_HOME`, `SHELL`, `IFS`, `CDPATH`, `ENV`,
    `BASH_ENV`, `PROMPT_COMMAND`, `PS4`, `SHELLOPTS`, `ZDOTDIR`, `BASH_FUNC_*`,
    `LD_*`, `DYLD_*`, `GIT_CONFIG*`, `GIT_SSH`, `GIT_SSH_COMMAND`,
    `GIT_ASKPASS`, `SSH_ASKPASS`, `GIT_EXTERNAL_DIFF`, `GIT_EXEC_PATH`,
    `GIT_PROXY_COMMAND`, `GIT_TEMPLATE_DIR`, `EDITOR`/`VISUAL`/`PAGER`/
    `GIT_EDITOR`/`GIT_PAGER` (except `cat`, `less`, `more`, `true`, `:`,
    empty), `NODE_OPTIONS`, `NODE_PATH`, `NPM_CONFIG_*`, `PERL5OPT`,
    `PERL5LIB`, `RUBYOPT`, `RUBYLIB`, `GEM_*`, `PYTHONPATH`, `PYTHONSTARTUP`,
    `PYTHONHOME`, `CLASSPATH`, `JAVA_HOME`, `JAVA_TOOL_OPTIONS`,
    `_JAVA_OPTIONS`, `JDK_JAVA_OPTIONS`, `CARGO_HOME`, `RUSTUP_HOME`,
    `RUSTC_WRAPPER`, `RUSTC_WORKSPACE_WRAPPER`, `GOPATH`, `GOROOT`,
    `LCTX_*`, `LEAN_CTX_*` (names compare case-insensitively).

  A refusal is the single error line.
- While it runs, one line per chunk of output (whole lines; a line without
  an end is cut at white space after 64 KiB), raw — never compressed — and
  redacted like any agent-visible text. Text that ends inside a rule
  reaching across lines (a private-key block, the classification field) is
  held until the rule closes, so the redaction sees it whole; held text that
  never closes (1 MiB, or the end of the output) is replaced in the stream
  by `[lean-ctx: output withheld from the stream: …]`:
  ```json
  {"contract":"client-calls/1","kind":"client.exec.output","data":"Compiling app v0.1.0\n"}
  ```
- At the end, one final document (released and receipted):
  ```json
  {
    "contract": "client-calls/1",
    "kind": "client.exec",
    "exit_code": 0,
    "signal": null,
    "output": "Compiling app v0.1.0\nFinished\n",
    "truncated": false
  }
  ```
  `output` is the end of the output within `--output-byte-limit` (default
  1 MiB, max 16 MiB), `truncated` whether anything was cut (ACP
  `terminal/output`). A cut tail starts at a line start (when it holds a
  line end) and never inside a private-key block whose header was cut off.
  `exit_code` is `null` when the
  command ended by a signal, then `signal` names it (`"SIGTERM"`, …).
- Exit status of `lean-ctx client exec` is 0 whenever the command ran; its
  own exit code is in the document.
- Stopping (ACP `terminal/kill`, `terminal/release`): send SIGTERM (or
  SIGINT/SIGHUP) to the `lean-ctx client exec` process. It terminates the
  command's process group, kills it after 2 s if still alive, and still
  prints the final document. When the command exits but a background
  child of its process group still holds the pipes, the group is
  terminated 0.5 s later (SIGTERM, then SIGKILL) — nothing of the command
  outlives the call (a child that left the group, e.g. by `setsid`, is not
  reached).
- Unknown command → `not_found`.

### ACP mapping

| ACP | Client Calls |
| --- | --- |
| `fs/read_text_file {path, line, limit}` | `client fs-read --path --line --limit` → `content` |
| `fs/write_text_file {path, content}` | `client fs-write --path`, content on stdin |
| `terminal/create {command, args, env, cwd, outputByteLimit}` | spawn `client exec --cwd --env… --output-byte-limit -- command args…`, keep the process |
| `terminal/output` | the client's buffer of `client.exec.output` chunks (or the final `output`) |
| `terminal/wait_for_exit` | read until the `client.exec` line → `exit_code`, `signal` |
| `terminal/kill` | SIGTERM to the process; the final line follows |
| `terminal/release` | kill if still running, then drop |
