// SPDX-License-Identifier: Apache-2.0
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

function bashResult(stdout: string, extra: Record<string, unknown> = {}) {
  return { result: { stdout, stderr: "warning: kept", interrupted: false, ...extra }, text: stdout };
}

test("the lean-ctx skill is prefixed with this session's job and Bash facts", async ($, on) => {
  on("skill.prompt", () => ({ text: "STATIC SKILL BODY" }));
  const out = await $.skill.prompt({ skill: "lean-ctx", text: "STATIC SKILL BODY" });
  expect(out.text.startsWith("## Live in this session (lean-ctx jobs mod)")).toBe(true);
  expect(out.text).toContain("Never `sleep` or poll");
  expect(out.text).toContain("Native Bash output is compressed automatically");
  expect(out.text.endsWith("STATIC SKILL BODY")).toBe(true);

  const other = await $.skill.prompt({ skill: "commit", text: "COMMIT" });
  expect(other.text).toBe("STATIC SKILL BODY");
});

// The ids of jobs that will still wake the model survive a main-loop
// compaction; without watched jobs the instructions pass through unchanged.
test("compaction keeps the ids of watched jobs", async ($, on) => {
  const instructionsSeen: (string | undefined)[] = [];
  mock.clock(on);
  on("command.register", ($, event) => ({ value: { command: event.name } }));
  on("tool.call", () => toolResult("[background:shell_keep started]"));
  on("session.compact", ($, event) => {
    instructionsSeen.push(event.instructions);
    return { messages: [{ role: "assistant", text: "summary", toolUses: [] }] };
  });
  const transcript = [{ role: "user" as const, text: "build", toolUses: [] }];

  await $.session.compact({ trigger: "manual", messages: transcript, instructions: "keep the API design" });
  expect(instructionsSeen[0]).toBe("keep the API design");

  await $.tool.call({ tool: SHELL, command: "cargo test", run_in_background: true });
  await $.session.compact({ trigger: "manual", messages: transcript, instructions: "keep the API design" });
  expect(instructionsSeen[1]).toContain("keep the API design");
  expect(instructionsSeen[1]).toContain("background job(s) shell_keep are still running");
});

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
    // ctx_shape is unlisted; Claude Code 2.1.296+ only calls listed tools,
    // so the mod reaches it through ctx_call (#2089).
    expect(event.tool).toBe("ctx_call");
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
  expect(shapeCalls[0]).toEqual({
    name: "ctx_shape",
    arguments: { tool: "Bash", command: "cargo build", output: big, exit_code: 0 },
  });

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

// The shapes the real ctx_shell sends (rust/crates/lean-ctx-server/src/server/tool_trait.rs): the
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

// User report: a job whose terminal status the model already read through
// background_action="status" was still announced by the next watcher tick —
// a duplicate wake that cost a whole extra turn. The same holds for cancel.
for (const action of ["status", "cancel"] as const) {
  test(`a terminal ${action} answer the model saw ends the watch without a wake`, async ($, on) => {
    const clock = mock.clock(on);
    const wakes: string[] = [];
    let polls = 0;
    const terminal = {
      jobId: "shell_seen",
      state: action === "cancel" ? "cancelled" : "completed",
      exitCode: 0,
    };
    on("command.register", ($, event) => ({ value: { command: event.name } }));
    on("tool.call", ($, event) => {
      const fields = event as unknown as Record<string, unknown>;
      if (fields.background_action) {
        return {
          result: {
            content: [{ type: "text", text: JSON.stringify(terminal) }],
            structuredContent: terminal,
            isError: false,
          },
          text: JSON.stringify(terminal),
        };
      }
      return toolResult('{"jobId":"shell_seen","state":"running","summary":"background job started"}');
    });
    on("mcp.call", () => {
      polls += 1;
      return { value: { content: [{ type: "text", text: "" }], structuredContent: terminal, isError: false } };
    });
    on("prompt.submit", ($, event) => {
      wakes.push(event.text);
      return { text: event.text };
    });

    const ack = await $.tool.call({ tool: SHELL, command: "true", run_in_background: true });
    expect(ack.context).toEqual([WATCH_CONTEXT]);

    const seen = await $.tool.call({ tool: SHELL, background_action: action, job_id: "shell_seen" });
    expect(seen.context ?? []).not.toContain(WATCH_CONTEXT);

    for (let tick = 0; tick < 3; tick += 1) {
      await clock.advance(2_000);
      await flushMicrotasks();
    }
    expect(polls).toBe(0);
    expect(wakes).toEqual([]);
  });
}

// User report (2026-10-12): during a long turn the watcher saw jobs finish and
// submitted the wake at once; the host held it until the turn ended, by which
// time the model had read each job itself — three stale wakes, one extra turn.
test("a job finished mid-turn wakes only if the model has not read it by turn end", async ($, on) => {
  const clock = mock.clock(on);
  const wakes: string[] = [];
  const done = (jobId: string) => ({ jobId, state: "completed", exitCode: 0 });
  on("command.register", ($, event) => ({ value: { command: event.name } }));
  on("tool.call", ($, event) => {
    const fields = event as unknown as Record<string, unknown>;
    if (fields.background_action) {
      const status = done(String(fields.job_id));
      return {
        result: { content: [{ type: "text", text: JSON.stringify(status) }], structuredContent: status, isError: false },
        text: JSON.stringify(status),
      };
    }
    const id = String(fields.command);
    return toolResult(`{"jobId":"${id}","state":"running","summary":"background job started"}`);
  });
  on("mcp.call", ($, event) => {
    const args = event.args as Record<string, unknown>;
    const status = done(String(args.job_id));
    return { value: { content: [{ type: "text", text: "" }], structuredContent: status, isError: false } };
  });
  on("prompt.submit", ($, event) => {
    wakes.push(event.text);
    return { text: event.text };
  });
  on("turn.start", ($, event) => ({ turnId: event.turnId }));
  on("turn.complete", ($, event) => ({ text: event.answer }));

  await $.turn.start({ text: "build it", turnId: "turn-1" });
  await $.tool.call({ tool: SHELL, command: "shell_read", run_in_background: true });
  await $.tool.call({ tool: SHELL, command: "shell_unread", run_in_background: true });
  await clock.advance(2_000);
  await flushMicrotasks();
  expect(wakes).toEqual([]);

  await $.tool.call({ tool: SHELL, background_action: "status", job_id: "shell_read" });
  await $.turn.complete({
    answer: "done",
    durationMs: 1,
    isAborted: false,
    turnId: "turn-1",
    reason: "answer",
  });
  await flushMicrotasks();
  expect(wakes).toEqual([]);
  await clock.advance(2_000);
  await flushMicrotasks();

  expect(wakes.length).toBe(1);
  expect(wakes[0]).toContain("Job shell_unread finished");
  expect(wakes[0]).not.toContain("shell_read finished");
});

// User report (2026-10-13): jobs finishing one after another while Claude was
// idle each submitted their own wake, a row of "Prompt from the lean-ctx
// plugin" entries and a turn each. A wake still waiting for its turn now holds
// later finishes; the model usually reads them in that turn.
function stagedJobs(on: Parameters<Parameters<typeof test>[1]>[1]) {
  const clock = mock.clock(on);
  const wakes: string[] = [];
  const finished = new Set<string>();
  const status = (jobId: string) =>
    finished.has(jobId) ? { jobId, state: "completed", exitCode: 0 } : { jobId, state: "running" };
  on("command.register", ($, event) => ({ value: { command: event.name } }));
  on("tool.call", ($, event) => {
    const fields = event as unknown as Record<string, unknown>;
    if (fields.background_action) {
      const s = status(String(fields.job_id));
      return {
        result: { content: [{ type: "text", text: JSON.stringify(s) }], structuredContent: s, isError: false },
        text: JSON.stringify(s),
      };
    }
    return toolResult(`{"jobId":"${String(fields.command)}","state":"running","summary":"background job started"}`);
  });
  on("mcp.call", ($, event) => {
    const s = status(String((event.args as Record<string, unknown>).job_id));
    return { value: { content: [{ type: "text", text: "" }], structuredContent: s, isError: false } };
  });
  on("prompt.submit", ($, event) => {
    wakes.push(event.text);
    return { text: event.text };
  });
  on("turn.start", ($, event) => ({ turnId: event.turnId }));
  on("turn.complete", ($, event) => ({ text: event.answer }));
  const tick = async () => {
    await clock.advance(2_000);
    await flushMicrotasks();
  };
  return { wakes, finished, tick };
}

const endTurn = ($: Parameters<Parameters<typeof test>[1]>[0], turnId: string) =>
  $.turn.complete({ answer: "ok", durationMs: 1, isAborted: false, turnId, reason: "answer" });

test("a job finishing before the last wake's turn starts joins no extra wake when the model reads it", async ($, on) => {
  const jobs = stagedJobs(on);
  await $.tool.call({ tool: SHELL, command: "shell_a", run_in_background: true });
  await $.tool.call({ tool: SHELL, command: "shell_b", run_in_background: true });

  jobs.finished.add("shell_a");
  await jobs.tick();
  expect(jobs.wakes.length).toBe(1);
  expect(jobs.wakes[0]).toContain("Job shell_a finished");

  // shell_b finishes while that wake still waits for its turn: held.
  jobs.finished.add("shell_b");
  await jobs.tick();
  expect(jobs.wakes.length).toBe(1);

  // The wake's turn reads shell_b itself; nothing is left to announce.
  await $.turn.start({ text: jobs.wakes[0], turnId: "wake-1" });
  await $.tool.call({ tool: SHELL, background_action: "status", job_id: "shell_b" });
  await endTurn($, "wake-1");
  await jobs.tick();
  await jobs.tick();
  expect(jobs.wakes.length).toBe(1);
});

test("a held job the model did not read wakes once after the turn", async ($, on) => {
  const jobs = stagedJobs(on);
  await $.tool.call({ tool: SHELL, command: "shell_a", run_in_background: true });
  await $.tool.call({ tool: SHELL, command: "shell_b", run_in_background: true });
  jobs.finished.add("shell_a");
  await jobs.tick();
  jobs.finished.add("shell_b");
  await jobs.tick();
  expect(jobs.wakes.length).toBe(1);

  await $.turn.start({ text: jobs.wakes[0], turnId: "wake-1" });
  await endTurn($, "wake-1");
  await jobs.tick();
  expect(jobs.wakes.length).toBe(2);
  expect(jobs.wakes[1]).toContain("Job shell_b finished");
  expect(jobs.wakes[1]).not.toContain("shell_a");
});

test("a wake that never starts its turn holds later jobs for at most a minute", async ($, on) => {
  const jobs = stagedJobs(on);
  await $.tool.call({ tool: SHELL, command: "shell_a", run_in_background: true });
  await $.tool.call({ tool: SHELL, command: "shell_b", run_in_background: true });
  jobs.finished.add("shell_a");
  await jobs.tick();
  jobs.finished.add("shell_b");
  for (let i = 0; i < 29; i += 1) await jobs.tick();
  expect(jobs.wakes.length).toBe(1);
  await jobs.tick();
  await jobs.tick();
  expect(jobs.wakes.length).toBe(2);
  expect(jobs.wakes[1]).toContain("Job shell_b finished");
});

test("with wake_on_job_finish off, background jobs are not watched and the skill says to check them", { options: { wake_on_job_finish: false } }, async ($, on) => {
  const jobs = stagedJobs(on);
  on("skill.prompt", () => ({ text: "STATIC SKILL BODY" }));
  const ack = await $.tool.call({ tool: SHELL, command: "shell_a", run_in_background: true });
  expect(ack.context ?? []).not.toContain(WATCH_CONTEXT);
  jobs.finished.add("shell_a");
  await jobs.tick();
  await jobs.tick();
  expect(jobs.wakes).toEqual([]);

  const skill = await $.skill.prompt({ skill: "lean-ctx", text: "STATIC SKILL BODY" });
  expect(skill.text).toContain('background_action="status"');
  expect(skill.text).not.toContain("Never `sleep` or poll");
});

test("a running status answer keeps the watch: the finish still wakes once", async ($, on) => {
  const clock = mock.clock(on);
  const wakes: string[] = [];
  on("command.register", ($, event) => ({ value: { command: event.name } }));
  on("tool.call", () =>
    toolResult('{"jobId":"shell_live","state":"running","summary":"still running"}'),
  );
  on("mcp.call", () => ({
    value: {
      content: [{ type: "text", text: "" }],
      structuredContent: { jobId: "shell_live", state: "completed", exitCode: 0 },
      isError: false,
    },
  }));
  on("prompt.submit", ($, event) => {
    wakes.push(event.text);
    return { text: event.text };
  });

  await $.tool.call({ tool: SHELL, command: "sleep 30", run_in_background: true });
  await $.tool.call({ tool: SHELL, background_action: "status", job_id: "shell_live" });
  for (let tick = 0; tick < 3; tick += 1) {
    await clock.advance(2_000);
    await flushMicrotasks();
  }
  expect(wakes.length).toBe(1);
  expect(wakes[0]).toContain("Job shell_live finished · exit 0");
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

test("/leanctx-jobs reports per-session sleep, wake and shaping counts", async ($, on) => {
  const clock = mock.clock(on);
  on("session.start", () => ({ cwd: "/work" }));
  on("tool.list", () => ({ value: [{ name: "mcp__lean-ctx__ctx_read", description: "read", mcp: true }] }));
  on("command.register", ($, event) => ({ value: { command: event.name } }));
  on("tool.call", ($, event) => {
    const fields = event as unknown as Record<string, unknown>;
    if (fields.tool === SHELL && fields.run_in_background) return toolResult("[background:shell_meter started]");
    return { result: "executed" };
  });
  on("mcp.call", () => mcpResult("[background:shell_meter completed, exit 0]"));
  on("prompt.submit", ($, event) => ({ text: event.text }));

  await $.session.start({ surface: "terminal", isInteractive: true, cwd: "/work" });
  await $.tool.call({ tool: SHELL, command: "echo work", run_in_background: true });
  await $.tool.call({ tool: "Bash", command: "sleep 1" });
  await clock.advance(2_000);
  await flushMicrotasks();

  const answer = await $.command.run({
    command: "leanctx-jobs",
    args: "",
    origin: { kind: "sdk" },
    presentation: { isFullscreen: false, columns: 80 },
  });
  expect(answer.text).toBe("sleeps answered 1 · wakes delivered 1 · Bash outputs shaped 0 (−0 chars)");
});
