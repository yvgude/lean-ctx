// SPDX-License-Identifier: Apache-2.0
import { expect, mock, test } from "claude-code/testing";

const SHELL = "mcp__lean-ctx__ctx_shell";
const WATCH_CONTEXT =
  "This background job is being watched; you will be woken automatically on completion, so do not poll or sleep.";
const SHARED_HOOK_CONTEXT =
  "lean-ctx active: ALWAYS use ctx_* MCP tools instead of native equivalents.\n" +
  "Exclusive tools: ctx_compose, ctx_callgraph, ctx_knowledge, ctx_session.";

function toolResult(text: string) {
  return {
    result: { content: [{ type: "text", text }], isError: false },
    text,
  };
}

function mcpResult(text: string) {
  return { value: { content: [{ type: "text", text }], isError: false } };
}

async function flushMicrotasks() {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

function bashResult(stdout: string, extra: Record<string, unknown> = {}) {
  return { result: { stdout, stderr: "warning: kept", interrupted: false, ...extra }, text: stdout };
}

test("hook attachments drop only lean-ctx blocks on the main loop and subagents", async ($, on) => {
  const forwarded: string[] = [];
  on("prompt.attachment", ($, event) => {
    forwarded.push(event.text);
    return { text: event.text };
  });

  const joined = `Other hook before\n${SHARED_HOOK_CONTEXT}\nSecurity reminder: keep secrets out.`;
  const subagent = await $.prompt.attachment({
    type: "hook_context",
    text: joined,
    origin: { kind: "hook", event: "SessionStart" },
    agentId: "sub-1",
  });
  expect(subagent).toEqual({ text: "Other hook before\nSecurity reminder: keep secrets out." });

  const mainLoop = await $.prompt.attachment({
    type: "hook_context",
    text: SHARED_HOOK_CONTEXT,
    origin: { kind: "hook", event: "UserPromptSubmit" },
  });
  expect(mainLoop).toEqual({ text: null });

  // As captured from Claude Code 2.1.287: the engine prefixes the hook's
  // context with "<Event> hook additional context: " on the same line.
  const framed = await $.prompt.attachment({
    type: "hook_additional_context",
    text: `SessionStart hook additional context: ${SHARED_HOOK_CONTEXT}`,
    origin: { kind: "hook", event: "SessionStart" },
  });
  expect(framed).toEqual({ text: null });
  const framedJoined = await $.prompt.attachment({
    type: "hook_additional_context",
    text: `Other hook before\nUserPromptSubmit hook additional context: ${SHARED_HOOK_CONTEXT}`,
    origin: { kind: "hook", event: "UserPromptSubmit" },
  });
  expect(framedJoined).toEqual({ text: "Other hook before" });

  for (const origin of [
    { kind: "engine" as const },
    { kind: "plugin" as const, event: "prompt.submit" },
    { kind: "hook" as const, event: "PostToolUse" },
  ]) {
    const untouched = await $.prompt.attachment({ type: "hook_context", text: SHARED_HOOK_CONTEXT, origin });
    expect(untouched).toEqual({ text: SHARED_HOOK_CONTEXT });
  }

  expect(forwarded).toEqual([
    SHARED_HOOK_CONTEXT,
    SHARED_HOOK_CONTEXT,
    SHARED_HOOK_CONTEXT,
  ]);
});

// Shape, don't redirect: large native Bash stdout is replaced by ctx_shape's
// shorter answer; everything else (stderr, small output, raw intent, errors,
// a failing or non-shrinking shaper) leaves the native result untouched.
test("native Bash stdout is shaped through ctx_shape and fails open", async ($, on) => {
  const big = "Compiling crate v0.1.0\n".repeat(200);
  const shapeCalls: Record<string, unknown>[] = [];
  let shaper: "ok" | "error" | "longer" = "ok";
  on("command.register", ($, event) => ({ value: { command: event.name } }));
  on("tool.describe", ($, event) => ({ description: event.description }));
  on("tool.call", ($, event) => {
    const fields = event as unknown as Record<string, unknown>;
    return fields.command === "small" ? bashResult("ok\n") : bashResult(big);
  });
  on("mcp.call", ($, event) => {
    shapeCalls.push(event.args as Record<string, unknown>);
    if (shaper === "error") return { deny: "shaper down" };
    const text = shaper === "longer" ? big + "x" : "Compiling 200 crates\n[lean-ctx: full original at /tmp/h]";
    return mcpResult(text);
  });

  // The server name is learned from the lean-ctx tool surface.
  await $.tool.describe({
    tool: "mcp__lean-ctx__ctx_read",
    description: "read",
    provider: { plugin: "mcp:lean-ctx", tier: "user" },
  });

  const shaped = await $.tool.call({ tool: "Bash", command: "cargo build" });
  const shapedRecord = shaped.result as Record<string, unknown>;
  expect(String(shapedRecord.stdout)).toContain("Compiling 200 crates");
  expect(shapedRecord.stderr).toBe("warning: kept");
  expect(shapeCalls[0]).toEqual({ tool: "Bash", command: "cargo build", output: big, exit_code: 0 });

  const small = await $.tool.call({ tool: "Bash", command: "small" });
  expect((small.result as Record<string, unknown>).stdout).toBe("ok\n");

  const raw = await $.tool.call({ tool: "Bash", command: "LEAN_CTX_RAW=1 cargo build" });
  expect((raw.result as Record<string, unknown>).stdout).toBe(big);

  shaper = "error";
  const failed = await $.tool.call({ tool: "Bash", command: "cargo build" });
  expect((failed.result as Record<string, unknown>).stdout).toBe(big);

  shaper = "longer";
  const grown = await $.tool.call({ tool: "Bash", command: "cargo build" });
  expect((grown.result as Record<string, unknown>).stdout).toBe(big);
  expect(shapeCalls.length).toBe(3);
});

// K8: a main-loop compaction saves lean-ctx's session, keeps the user's own
// /compact instructions and adds lean-ctx's; the next prompt — and only that
// one — carries the restored session state. Subagent compactions are untouched.
test("compaction keeps lean-ctx state and restores it once", async ($, on) => {
  const mcpCalls: Record<string, unknown>[] = [];
  const instructionsSeen: (string | undefined)[] = [];
  const contexts: (readonly string[] | undefined)[] = [];
  on("command.register", ($, event) => ({ value: { command: event.name } }));
  on("tool.describe", ($, event) => ({ description: event.description }));
  on("mcp.call", ($, event) => {
    const args = event.args as Record<string, unknown>;
    mcpCalls.push(args);
    return mcpResult(args.action === "status" ? "Task: ship the mod\nDecision: wake, don't poll" : "saved");
  });
  on("session.compact", ($, event) => {
    instructionsSeen.push(event.instructions);
    return { messages: [{ role: "assistant", text: "summary", toolUses: [] }] };
  });
  on("prompt.submit", ($, event) => {
    contexts.push(event.context);
    return { text: event.text };
  });

  await $.tool.describe({
    tool: "mcp__lean-ctx__ctx_read",
    description: "read",
    provider: { plugin: "mcp:lean-ctx", tier: "user" },
  });

  const transcript = [{ role: "user" as const, text: "build the mod", toolUses: [] }];
  await $.session.compact({ trigger: "manual", messages: transcript, instructions: "keep the API design" });
  expect(mcpCalls[0]).toEqual({ action: "save" });
  expect(instructionsSeen[0]).toContain("keep the API design");
  expect(instructionsSeen[0]).toContain("lean-ctx: keep, verbatim, any lean-ctx recovery handles");

  await $.prompt.submit({ text: "continue", wait: false, origin: { kind: "composer" } });
  expect(String(contexts[0]?.join("\n"))).toContain("lean-ctx session state, restored after compaction");
  expect(String(contexts[0]?.join("\n"))).toContain("Task: ship the mod");

  await $.prompt.submit({ text: "and again", wait: false, origin: { kind: "composer" } });
  expect(contexts[1] ?? []).toEqual([]);

  await $.session.compact({ trigger: "auto", messages: transcript, agentId: "sub-1" });
  await $.prompt.submit({ text: "after subagent compaction", wait: false, origin: { kind: "composer" } });
  expect(contexts[2] ?? []).toEqual([]);
  expect(mcpCalls.filter((c) => c.action === "save").length).toBe(1);
});

// Live finding: `claude -p "/leanctx"` had no command, because lean-ctx was
// only detected once a model request rendered the tools.
test("/leanctx exists right after session start when lean-ctx is connected", async ($, on) => {
  on("session.start", () => ({ cwd: "/work" }));
  on("tool.list", () => ({
    value: [
      { name: "Bash", description: "shell", mcp: false },
      { name: "mcp__lean-ctx__ctx_read", description: "read", mcp: true },
    ],
  }));
  on("command.register", ($, event) => ({ value: { command: event.name } }));
  await $.session.start({ surface: "terminal", isInteractive: false, cwd: "/work" });
  const answer = await $.command.run({
    command: "leanctx",
    args: "",
    origin: { kind: "sdk" },
    presentation: { isFullscreen: false, columns: 80 },
  });
  expect(answer.text).toContain("Requests 0");
});

test("the lean-ctx skill is prefixed with this session's live facts", async ($, on) => {
  on("skill.prompt", () => ({ text: "STATIC SKILL BODY" }));
  const out = await $.skill.prompt({ skill: "lean-ctx", text: "STATIC SKILL BODY" });
  expect(out.text.startsWith("## Live in this session (lean-ctx Claude Code mod)")).toBe(true);
  expect(out.text).toContain("Never `sleep` or poll");
  expect(out.text).toContain("ctx_callgraph, ctx_compose, ctx_read, ctx_search, ctx_session, ctx_shell");
  expect(out.text.endsWith("STATIC SKILL BODY")).toBe(true);

  const other = await $.skill.prompt({ skill: "commit", text: "COMMIT" });
  expect(other.text).toBe("STATIC SKILL BODY");
});

test("lean-ctx tools keep descriptions and only configured tools are front-loaded", async ($, on) => {
  on("command.register", ($, event) => ({ value: { command: event.name } }));
  on("tool.describe", ($, event) => ({
    description: event.description,
    isDeferred: event.isDeferred,
  }));

  const read = await $.tool.describe({
    tool: "mcp__lean-ctx__ctx_read",
    description: "read description",
    isDeferred: true,
    provider: { plugin: "mcp:lean-ctx", tier: "user" },
  });
  const glob = await $.tool.describe({
    tool: "mcp__lean-ctx__ctx_glob",
    description: "glob description",
    isDeferred: true,
    provider: { plugin: "mcp:lean-ctx", tier: "user" },
  });
  const bash = await $.tool.describe({
    tool: "Bash",
    description: "native Bash description",
    isDeferred: true,
    provider: { plugin: "engine", tier: "core" },
  });
  // Real request capture showed the non-`ctx_` alias staying front-loaded.
  const alias = await $.tool.describe({
    tool: "mcp__lean-ctx__shell",
    description: "shell alias",
    provider: { plugin: "mcp:lean-ctx", tier: "user" },
  });

  expect(read).toEqual({ description: "read description", isDeferred: false });
  expect(glob).toEqual({ description: "glob description", isDeferred: true });
  expect(bash).toEqual({ description: "native Bash description", isDeferred: true });
  expect(alias).toEqual({ description: "shell alias", isDeferred: true });
});

test("the mod leaves /leanctx alone when no lean-ctx MCP tools are present", async ($, on) => {
  on("command.run", () => ({ text: "existing command" }));

  const answer = await $.command.run({
    command: "leanctx",
    args: "",
    origin: { kind: "sdk" },
    presentation: { isFullscreen: false, columns: 80 },
  });

  expect(answer.text).toBe("existing command");
});

test("the mod leaves a pre-existing /leanctx command in control", async ($, on) => {
  on("command.register", () => ({ deny: "already registered" }));
  on("tool.call", () => ({ result: "executed" }));
  on("command.run", () => ({ text: "pre-existing command" }));

  await $.tool.call({ tool: SHELL, command: "echo work" });
  const answer = await $.command.run({
    command: "leanctx",
    args: "",
    origin: { kind: "sdk" },
    presentation: { isFullscreen: false, columns: 80 },
  });

  expect(answer.text).toBe("pre-existing command");
});

test("one watcher coalesces terminal jobs into one bounded wake", async ($, on) => {
  const clock = mock.clock(on);
  const wakes: string[] = [];
  on("command.register", ($, event) => ({ value: { command: event.name } }));
  on("tool.call", ($, event) => {
    const fields = event as unknown as Record<string, unknown>;
    if (fields.tool === SHELL && fields.run_in_background) {
      const id = fields.command === "echo second" ? "shell_b" : "shell_a";
      return toolResult(`[background:${id} started]`);
    }
    return { result: "executed" };
  });
  on("mcp.call", ($, event) => {
    const args = event.args as Record<string, unknown>;
    const id = String(args.job_id);
    if (id === "shell_a") {
      return mcpResult(
        `[full output: /private/tmp/task-a.log — read it directly]\n[background:shell_a completed, exit 0]`,
      );
    }
    const lines = Array.from({ length: 25 }, (_, index) => `line-${String(index + 1).padStart(2, "0")}`);
    return mcpResult(`${lines.join("\n")}\n[background:shell_b failed, exit 7]`);
  });
  on("prompt.submit", ($, event) => {
    wakes.push(event.text);
    return { text: event.text };
  });

  const first = await $.tool.call({ tool: SHELL, command: "echo first", run_in_background: true });
  const second = await $.tool.call({ tool: SHELL, command: "echo second", run_in_background: true });

  expect(first.context).toEqual([WATCH_CONTEXT]);
  expect(second.context).toEqual([WATCH_CONTEXT]);

  await clock.advance(2_000);
  await flushMicrotasks();

  expect(wakes.length).toBe(1);
  expect(wakes[0]).toContain("Job shell_a finished · exit 0");
  expect(wakes[0]).toContain("Full output: /private/tmp/task-a.log");
  expect(wakes[0]).toContain("Job shell_b finished · exit 7");
  expect(wakes[0]).toContain("line-06");
  expect(wakes[0]).toContain("line-25");
  expect(wakes[0]).not.toContain("line-05");
});

// The shapes the real ctx_shell sends (rust/src/server/tool_trait.rs): the
// launch ack is rendered only as a JSON text block, and a finished job's
// status carries structuredContent with isError=true when exit != 0.
test("real ctx_shell shapes: JSON-text ack, structured failed status, transient miss keeps watching", async ($, on) => {
  const clock = mock.clock(on);
  const wakes: string[] = [];
  let polls = 0;
  on("command.register", ($, event) => ({ value: { command: event.name } }));
  on("tool.call", () =>
    toolResult('{"jobId":"shell_real","state":"running","summary":"background job started"}'),
  );
  on("mcp.call", () => {
    polls += 1;
    if (polls === 1) return { deny: "transient MCP failure" };
    if (polls === 2) {
      return { value: { content: [{ type: "text", text: "noise" }], isError: false } };
    }
    return {
      value: {
        content: [{ type: "text", text: "build output" }],
        structuredContent: {
          jobId: "shell_real",
          state: "failed",
          exitCode: 7,
          archiveId: "arch_7",
          summary: "3 tests failed",
        },
        isError: true,
      },
    };
  });
  on("prompt.submit", ($, event) => {
    wakes.push(event.text);
    return { text: event.text };
  });

  const ack = await $.tool.call({ tool: SHELL, command: "cargo test", run_in_background: true });
  expect(ack.context).toEqual([WATCH_CONTEXT]);

  for (let tick = 0; tick < 3; tick += 1) {
    await clock.advance(2_000);
    await flushMicrotasks();
  }

  expect(polls).toBe(3);
  expect(wakes.length).toBe(1);
  expect(wakes[0]).toContain("Job shell_real finished · exit 7");
  expect(wakes[0]).toContain("Full output: ctx_expand id=arch_7");
});

test("sleep waits are answered only while watched and MCP errors fail open", async ($, on) => {
  const clock = mock.clock(on);
  const forwarded: string[] = [];
  const wakes: string[] = [];
  let mcpFails = false;
  on("command.register", ($, event) => ({ value: { command: event.name } }));
  on("tool.call", ($, event) => {
    const fields = event as unknown as Record<string, unknown>;
    if (fields.tool === SHELL && fields.run_in_background) return toolResult("[background:shell_error started]");
    forwarded.push(`${String(fields.tool)}:${String(fields.command ?? "")}`);
    return { result: "executed" };
  });
  on("mcp.call", () => {
    if (mcpFails) return { deny: "temporary MCP failure" };
    return mcpResult("[background:shell_error completed, exit 0]");
  });
  on("prompt.submit", ($, event) => {
    wakes.push(event.text);
    return { text: event.text };
  });

  const before = await $.tool.call({ tool: "Bash", command: "sleep 1" });
  expect(before.result).toBe("executed");
  await $.tool.call({ tool: SHELL, command: "echo work", run_in_background: true });
  const bashSleep = await $.tool.call({ tool: "Bash", command: "sleep 1" });
  const shellSleep = await $.tool.call({ tool: SHELL, command: "sleep 1" });
  const bashCommand = await $.tool.call({ tool: "Bash", command: "echo safe" });

  expect(String(bashSleep.result)).toContain("shell_error");
  expect(String(shellSleep.result)).toContain("shell_error");
  expect(bashCommand.result).toBe("executed");
  expect(forwarded).toEqual(["Bash:sleep 1", "Bash:echo safe"]);

  mcpFails = true;
  // One transient failure keeps the watch; five in a row hand the job back
  // to the model's own polling (fail-open).
  await clock.advance(2_000);
  await flushMicrotasks();
  const sleepDuringHiccup = await $.tool.call({ tool: "Bash", command: "sleep 1" });
  expect(String(sleepDuringHiccup.result)).toContain("shell_error");
  for (let tick = 0; tick < 4; tick += 1) {
    await clock.advance(2_000);
    await flushMicrotasks();
  }
  // The model was told it would be woken: handing the job back is explicit.
  expect(wakes.length).toBe(1);
  expect(wakes[0]).toContain("can no longer watch background job(s) shell_error");

  const sleepAfterError = await $.tool.call({ tool: "Bash", command: "sleep 1" });
  expect(sleepAfterError.result).toBe("executed");
  expect(forwarded).toEqual(["Bash:sleep 1", "Bash:echo safe", "Bash:sleep 1"]);
});

test("/leanctx reports per-session request, token, tool, sleep, and wake counts", async ($, on) => {
  const clock = mock.clock(on);
  on("session.start", () => ({ cwd: "/work" }));
  on("command.register", ($, event) => ({ value: { command: event.name } }));
  on("tool.call", ($, event) => {
    const fields = event as unknown as Record<string, unknown>;
    if (fields.tool === SHELL && fields.run_in_background) return toolResult("[background:shell_meter started]");
    return { result: "executed" };
  });
  on("mcp.call", () => mcpResult("[background:shell_meter completed, exit 0]"));
  on("prompt.submit", ($, event) => ({ text: event.text }));
  on("turn.step", async function* ($, event) {
    yield { kind: "text", index: 0, text: "done" };
    return {
      turnId: event.turnId,
      index: event.index,
      answer: "done",
      toolUses: [{ name: "ToolSearch", input: {} }],
      stopReason: "end_turn",
      usage: {
        input_tokens: 12,
        output_tokens: 3,
        cache_read_input_tokens: 4,
        cache_creation_input_tokens: 2,
        model: "claude-test",
      },
    };
  });

  await $.session.start({ surface: "terminal", isInteractive: true, cwd: "/work" });
  const droppedAttachment = await $.prompt.attachment({
    type: "hook_context",
    text: SHARED_HOOK_CONTEXT,
    origin: { kind: "hook", event: "SessionStart" },
  });
  expect(droppedAttachment).toEqual({ text: null });
  await $.tool.call({ tool: SHELL, command: "echo work", run_in_background: true });
  await $.tool.call({ tool: "Bash", command: "sleep 1" });

  const stream = $.turn.step({ turnId: "turn-1", index: 0, model: "claude-test", messageCount: 1 });
  let chunk = await stream.next();
  while (chunk.done !== true) chunk = await stream.next();

  await clock.advance(2_000);
  await flushMicrotasks();

  const answer = await $.command.run({
    command: "leanctx",
    args: "",
    origin: { kind: "sdk" },
    presentation: { isFullscreen: false, columns: 80 },
  });
  expect(answer.text).toBe(
    `Requests 1 · input 12 · output 3 · cache read 4 · cache creation 2 · ToolSearch-only 1 · lean-ctx calls 1 · sleeps answered 1 · wakes delivered 1 · Bash outputs shaped 0 (−0 chars) · hook attachments dropped 1 (−${SHARED_HOOK_CONTEXT.length} chars) · compactions 0`,
  );
});
