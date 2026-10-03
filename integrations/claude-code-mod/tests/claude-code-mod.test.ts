import { expect, mock, test } from "claude-code/testing";

const SHELL = "mcp__lean-ctx__ctx_shell";
const WATCH_CONTEXT =
  "This background job is being watched; you will be woken automatically on completion, so do not poll or sleep.";

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
  expect(wakes).toEqual([]);

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
    "Requests 1 · input 12 · output 3 · cache read 4 · cache creation 2 · ToolSearch-only 1 · lean-ctx calls 1 · sleeps answered 1 · wakes delivered 1",
  );
});
