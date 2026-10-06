# Protected hook admission

When a project or organisation policy is present, the command-gating hook CLI
checks security admission before compression settings, environment capture or
legacy fallback handling. This applies to `rewrite`, `redirect`, `deny`,
`copilot`, `codex-pretooluse`, `vibe-pre-tool` and `rewrite-inline`.

Native reads, search, shell, edits and unrecognised tool routes are denied by
these handlers because their returned content cannot be reliably filtered there.
The denial is independent of `LEAN_CTX_DISABLED`, shadow mode, replace mode,
daemon availability, binary extensions, automatic-memory paths and native
read-before-edit compatibility exceptions. A parent project's policy also
requires inspection when the hook starts in a subdirectory. Invalid configured
policies remain protected. With no configured policy the existing Community
handler retains its behaviour and receives the untouched input stream.

The admitted route is a configured LeanCTX MCP server: qualified names
`mcp__lean-ctx__ctx_*` / `mcp__lean_ctx__ctx_*`, or structural `tool_info` with
server `lean-ctx` / `lean_ctx` and a `ctx_*` name. Bare `ctx_*`, native `shell`,
foreign servers and conflicting or duplicate identities are insufficient.
Admission only selects the MCP path; the MCP runtime must still enforce its
own current project/organisation, tool, path, shell and content policy.

Policy resolution and input handling share a two-second decision budget.
Input is limited to 256 KiB. Invalid input, timeout and worker failure produce
a static, content-free denial instead of passing the original call through.
Codex receives its `hookSpecificOutput` decision, Vibe its top-level decision,
and the other JSON adapters the existing multi-host permission fields. The
generic `deny` adapter retains exit 2 plus a static stderr reason for hosts that
use the exit code to block. The other JSON adapters return their decision on
exit 0. The inline shell adapter emits `false` and exits 2.

These decisions do not make an unqualified host integration protected. A managed
deployment must verify that its installed host invokes a supported hook for
**every** relevant tool, honours its denial schema and deadline, and cannot
silently skip a missing hook executable. Existing narrow tool matchers are not
a blanket security boundary. Host configuration, the MCP server identity and
the policy location must be controlled by the deployment. This is not an OS
sandbox against an administrator who can replace those components. An adapter
that ignores hook decisions must remain unavailable for protected workflows.

Run the isolated actual-process check against a locally built binary:

```sh
python3 scripts/tests/check_protected_hooks.py --binary /absolute/path/to/lean-ctx --output /tmp/protected-hooks-report.json
```

This checks CLI decisions, timeouts and content-free diagnostics, without
installing hooks, changing the running daemon, using model providers or claiming
that an external agent host honoured the decision. External host qualification
and complete end-to-end v4 acceptance remain separate.
