// SPDX-License-Identifier: Apache-2.0
import { expect, mock, test } from "claude-code/testing";

const SHELL = "mcp__lean-ctx__ctx_shell";
const SHARED_HOOK_CONTEXT =
  "lean-ctx active: ALWAYS use ctx_* MCP tools instead of native equivalents.\n" +
  "Exclusive tools: ctx_compose, ctx_callgraph, ctx_knowledge, ctx_session.";

function mcpResult(text: string) {
  return { value: { content: [{ type: "text", text }], isError: false } };
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

// Code Mode: the hyphenated server name is only reachable with bracket syntax;
// measured two failed guesses per session without this section.
test("the REPL call form for lean-ctx is in the prompt only while REPL is offered", async ($, on) => {
  on("session.start", () => ({ cwd: "/work" }));
  on("tool.list", () => ({
    value: [{ name: "mcp__lean-ctx__ctx_read", description: "read", mcp: true }],
  }));
  on("prompt.compose", () => ({ sections: [{ id: "engine", text: "base", scope: "shared" }] }));
  await $.session.start({ surface: "terminal", isInteractive: false, cwd: "/work" });
  const section = (tools: string[]) =>
    $.prompt
      .compose({ model: "m", promptModel: "m", surfaces: ["terminal"], tools, outputStyle: null, traits: [] })
      .then((r) => r.sections.find((s) => s.id === "lean-ctx:code-mode"));

  const withRepl = await section(["REPL", "mcp__lean-ctx__ctx_read"]);
  expect(withRepl?.scope).toBe("session");
  expect(withRepl?.text).toContain('tools["mcp__lean-ctx__ctx_read"]');
  expect(await section(["Read", "mcp__lean-ctx__ctx_read"])).toBeUndefined();
});

// #2057: the cockpit sidebar can be turned off and on, and the choice holds
// for new sessions through the mod's `cockpit_panel` option.
function cockpitEngine(on: Parameters<Parameters<typeof test>[1]>[1]) {
  const opened: string[] = [];
  const closed: string[] = [];
  const written: { key: string; value: unknown }[] = [];
  mock.clock(on);
  on("session.start", () => ({ cwd: "/work" }));
  on("command.register", ($, event) => ({ value: { command: event.name } }));
  on("ui.status", () => ({ value: undefined }));
  on("ui.open", ($, event) => {
    opened.push(event.id);
    return { value: { isPlaced: true as const } };
  });
  on("ui.close", ($, event) => {
    closed.push(event.id);
    return { value: undefined };
  });
  const toggle = (option: string, label: string) => ({
    key: `lean-ctx.${option}`,
    label,
    kind: "toggle" as const,
    value: true,
    provider: { kind: "plugin" as const, name: "lean-ctx" },
    isLocked: false,
  });
  on("config.list", () => ({
    value: [
      toggle("cockpit_panel", "Open the LeanCTX cockpit sidebar"),
      toggle("cockpit_pulse_line", "Show the LeanCTX pulse line"),
      toggle("cockpit_turn_cards", "Show LeanCTX turn and milestone cards"),
    ],
  }));
  on("config.set", ($, event) => {
    written.push({ key: event.key, value: event.value });
    return { value: event.value };
  });
  return { opened, closed, written };
}

const runCockpit = ($: Parameters<Parameters<typeof test>[1]>[0], args: string) =>
  $.command.run({
    command: "cockpit",
    args,
    origin: { kind: "composer" },
    presentation: { isFullscreen: true, columns: 160 },
  });

test("/cockpit off closes the sidebar and keeps it closed; /cockpit on brings it back", async ($, on) => {
  const engine = cockpitEngine(on);
  await $.session.start({ surface: "terminal", isInteractive: true, cwd: "/work" });
  expect(engine.opened).toEqual(["leanctx-cockpit"]);

  const off = await runCockpit($, "off");
  expect(engine.closed).toEqual(["leanctx-cockpit"]);
  expect(engine.written).toEqual([{ key: "lean-ctx.cockpit_panel", value: false }]);
  expect(off.text).toContain("off");
  expect(off.text).toContain("/cockpit on");

  const on_ = await runCockpit($, "ON ");
  expect(engine.written.at(-1)).toEqual({ key: "lean-ctx.cockpit_panel", value: true });
  expect(engine.opened).toEqual(["leanctx-cockpit", "leanctx-cockpit"]);
  expect(on_.text).toContain("opens in new sessions again");

  const usage = await runCockpit($, "toggle");
  expect(usage.text).toContain("Usage: /cockpit");
  expect(engine.written.length).toBe(2);
});

test("/cockpit pulse|cards on|off keep the lines above the prompt off or on without touching the sidebar", async ($, on) => {
  const engine = cockpitEngine(on);
  await $.session.start({ surface: "terminal", isInteractive: true, cwd: "/work" });

  const pulseOff = await runCockpit($, "pulse off");
  expect(pulseOff.text).toContain("pulse line off");
  expect(pulseOff.text).toContain("/cockpit pulse on");
  const cardsOff = await runCockpit($, " Cards  OFF ");
  expect(cardsOff.text).toContain("turn and milestone cards off");
  const cardsOn = await runCockpit($, "cards on");
  expect(cardsOn.text).toContain("cards on, now and in new sessions");
  expect(engine.written).toEqual([
    { key: "lean-ctx.cockpit_pulse_line", value: false },
    { key: "lean-ctx.cockpit_turn_cards", value: false },
    { key: "lean-ctx.cockpit_turn_cards", value: true },
  ]);
  expect(engine.closed).toEqual([]);
  expect(engine.opened).toEqual(["leanctx-cockpit"]);

  for (const bad of ["pulse", "cards maybe", "pulse off now"]) {
    expect((await runCockpit($, bad)).text).toContain("Usage: /cockpit");
  }
  expect(engine.written.length).toBe(3);
});

test("with cockpit_panel off a session starts without the sidebar; /cockpit still opens it once", { options: { cockpit_panel: false } }, async ($, on) => {
  const engine = cockpitEngine(on);
  await $.session.start({ surface: "terminal", isInteractive: true, cwd: "/work" });
  expect(engine.opened).toEqual([]);

  const once = await runCockpit($, "");
  expect(engine.opened).toEqual(["leanctx-cockpit"]);
  expect(once.text).toBe("LeanCTX cockpit opened.");
  expect(engine.written).toEqual([]);
});

test("the lean-ctx skill is prefixed with this session's live facts", async ($, on) => {
  on("skill.prompt", () => ({ text: "STATIC SKILL BODY" }));
  const out = await $.skill.prompt({ skill: "lean-ctx", text: "STATIC SKILL BODY" });
  expect(out.text.startsWith("## Live in this session (lean-ctx Claude Code mod)")).toBe(true);
  // Job and Bash-output facts come from the lean-ctx-jobs plugin, when enabled.
  expect(out.text).not.toContain("Never `sleep` or poll");
  expect(out.text).toContain(
    "ctx_callgraph, ctx_compose, ctx_glob, ctx_patch, ctx_read, ctx_search, ctx_session, ctx_shell, ctx_tree",
  );
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
  // Routed by the rules: deferring it sent models back to native Edit.
  const patch = await $.tool.describe({
    tool: "mcp__lean-ctx__ctx_patch",
    description: "patch description",
    isDeferred: true,
    provider: { plugin: "mcp:lean-ctx", tier: "user" },
  });
  const expand = await $.tool.describe({
    tool: "mcp__lean-ctx__ctx_expand",
    description: "expand description",
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
  expect(patch).toEqual({ description: "patch description", isDeferred: false });
  expect(expand).toEqual({ description: "expand description", isDeferred: true });
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
  on("tool.describe", ($, event) => ({ description: event.description }));
  on("command.run", () => ({ text: "pre-existing command" }));

  await $.tool.describe({
    tool: SHELL,
    description: "shell",
    provider: { plugin: "mcp:lean-ctx", tier: "user" },
  });
  const answer = await $.command.run({
    command: "leanctx",
    args: "",
    origin: { kind: "sdk" },
    presentation: { isFullscreen: false, columns: 80 },
  });

  expect(answer.text).toBe("pre-existing command");
});

// The core mod hooks no `tool.call` (Code Mode refuses any plugin that does),
// so lean-ctx calls are counted from each model request's tool uses.
test("/leanctx reports per-session request, token and tool counts", async ($, on) => {
  on("session.start", () => ({ cwd: "/work" }));
  on("tool.list", () => ({ value: [{ name: SHELL, description: "shell", mcp: true }] }));
  on("command.register", ($, event) => ({ value: { command: event.name } }));
  const stepTools = [[{ name: "ToolSearch", input: {} }], [{ name: SHELL, input: {} }, { name: "Bash", input: {} }]];
  on("turn.step", async function* ($, event) {
    yield { kind: "text", index: 0, text: "done" };
    return {
      turnId: event.turnId,
      index: event.index,
      answer: "done",
      toolUses: stepTools[event.index] ?? [],
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

  for (const index of [0, 1]) {
    const stream = $.turn.step({ turnId: "turn-1", index, model: "claude-test", messageCount: 1 });
    let chunk = await stream.next();
    while (chunk.done !== true) chunk = await stream.next();
  }

  const answer = await $.command.run({
    command: "leanctx",
    args: "",
    origin: { kind: "sdk" },
    presentation: { isFullscreen: false, columns: 80 },
  });
  expect(answer.text).toBe(
    `Requests 2 · input 24 · output 6 · cache read 8 · cache creation 4 · ToolSearch-only 1 · lean-ctx calls 1 · hook attachments dropped 1 (−${SHARED_HOOK_CONTEXT.length} chars) · compactions 0`,
  );
});
