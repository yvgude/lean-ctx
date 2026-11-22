// SPDX-License-Identifier: Apache-2.0
import type {
  EngineInterface,
  McpToolResult,
  Register,
  TurnStepResult,
} from "claude-code";

import { registerCockpit } from "./cockpit";

// This plugin registers no tool-call hook: Claude Code turns Code Mode off for any
// enabled plugin that does. Background-job wakes, sleep answers and native
// Bash shaping live in the separate `lean-ctx-jobs` plugin.
import { registerResultRendering } from "./render";

// Every tool the injected rules route an intent to must stay in front: a
// deferred ctx_patch/ctx_glob/ctx_tree sends the model back to native
// Edit/Glob, which replace mode denies (`alwaysLoad` cannot undo a deferral).
// `claude_mod.rs` tests this list against the rules and the server's core.
const DEFAULT_FRONT_LOADED_TOOLS = [
  "ctx_read",
  "ctx_search",
  "ctx_shell",
  "ctx_glob",
  "ctx_tree",
  "ctx_compose",
  "ctx_patch",
  "ctx_callgraph",
  "ctx_session",
];
// Every tool of the lean-ctx server, not only `ctx_*`: the `shell` alias of
// ctx_shell would otherwise stay front-loaded under `alwaysLoad`.
const LEAN_CTX_TOOL_PATTERN = /^mcp__lean[-_]ctx__[A-Za-z0-9_-]+$/;
const LEAN_CTX_TOOL_NAME = /^mcp__(lean[-_]ctx)__([A-Za-z0-9_-]+)$/;
// Upper bound for the session state injected after a compaction (~500 tokens).
const MAX_RESUME_CHARS = 2_000;
type LeanCtxHookTextEnd = { end: string; continues?: readonly string[] };
type LeanCtxHookTextSignature = { start: string; ends: readonly LeanCtxHookTextEnd[] };
// Exact leading signatures emitted by observe.rs. Keep these stable and update
// the cross-language drift test there whenever an authored hook text changes.
const LEAN_CTX_HOOK_TEXT_SIGNATURES: readonly LeanCtxHookTextSignature[] = [
  {
    start: "lean-ctx tools for this session: ctx_read for Read, ctx_search for Grep, ctx_shell for Shell, ctx_glob for Glob, ctx_tree for ls.",
    ends: [
      { end: "Native Read stays available for editing." },
    ],
  },
  {
    start: "lean-ctx routing: reads/searches via ctx_read/ctx_search/ctx_glob/ctx_tree, commands via ctx_shell;",
    ends: [
      { end: "Bash/cat/sed/grep." },
    ],
  },
  {
    start: "lean-ctx active: ALWAYS use ctx_* MCP tools instead of native equivalents.",
    ends: [
      { end: "Exclusive tools: ctx_compose, ctx_callgraph, ctx_knowledge, ctx_session." },
    ],
  },
  {
    // Rules v13 (A3) and later.
    start: "lean-ctx routing — the host's built-in tools map to:",
    ends: [
      {
        end: "Use native Read for out-of-root; `lean-ctx doctor` shows effective roots.",
        continues: [
          "Other tools: ctx_call(tool=<name>).",
          "Advanced tools not in your profile are available via ctx_call(tool=<name>) gateway.",
          "Prefer stdlib and native platform alternatives before adding code or dependencies.",
          "Solution efficiency ladder:",
          "challenge every requirement, prefer deletion.",
        ],
      },
      {
        end: "Other tools: ctx_call(tool=<name>).",
        continues: [
          "Prefer stdlib and native platform alternatives before adding code or dependencies.",
          "Solution efficiency ladder:",
          "challenge every requirement, prefer deletion.",
        ],
      },
      {
        // Emitted by lean-ctx 3.11.4 and earlier.
        end: "Advanced tools not in your profile are available via ctx_call(tool=<name>) gateway.",
        continues: [
          "Prefer stdlib and native platform alternatives before adding code or dependencies.",
          "Solution efficiency ladder:",
          "challenge every requirement, prefer deletion.",
        ],
      },
      { end: "Prefer stdlib and native platform alternatives before adding code or dependencies." },
      { end: "Preserve validation, security, and error-handling." },
    ],
  },
  {
    // Rules v12 and earlier.
    start: "CRITICAL: ALWAYS use lean-ctx ctx_* tools as mapped below.",
    ends: [
      {
        end: "Use native Read for out-of-root; `lean-ctx doctor` shows effective roots.",
        continues: [
          "Other tools: ctx_call(tool=<name>).",
          "Advanced tools not in your profile are available via ctx_call(tool=<name>) gateway.",
          "Prefer stdlib and native platform alternatives before adding code or dependencies.",
          "Solution efficiency ladder:",
          "challenge every requirement, prefer deletion.",
        ],
      },
      {
        end: "Other tools: ctx_call(tool=<name>).",
        continues: [
          "Prefer stdlib and native platform alternatives before adding code or dependencies.",
          "Solution efficiency ladder:",
          "challenge every requirement, prefer deletion.",
        ],
      },
      {
        // Emitted by lean-ctx 3.11.4 and earlier.
        end: "Advanced tools not in your profile are available via ctx_call(tool=<name>) gateway.",
        continues: [
          "Prefer stdlib and native platform alternatives before adding code or dependencies.",
          "Solution efficiency ladder:",
          "challenge every requirement, prefer deletion.",
        ],
      },
      { end: "Prefer stdlib and native platform alternatives before adding code or dependencies." },
      { end: "Preserve validation, security, and error-handling." },
    ],
  },
  {
    start: "lean-ctx shadow mode: native read/search/shell calls auto-route to ctx_* — no tool-mapping needed.",
    ends: [
      {
        end: "ctx_search(action=semantic) (by meaning).",
        continues: [
          "Prefer stdlib and native platform alternatives before adding code or dependencies.",
          "Solution efficiency ladder:",
          "challenge every requirement, prefer deletion.",
        ],
      },
      {
        end: "ctx_callgraph (callers).",
        continues: [
          "Prefer stdlib and native platform alternatives before adding code or dependencies.",
          "Solution efficiency ladder:",
          "challenge every requirement, prefer deletion.",
        ],
      },
      {
        end: "ctx_knowledge / ctx_session (memory).",
        continues: [
          "Prefer stdlib and native platform alternatives before adding code or dependencies.",
          "Solution efficiency ladder:",
          "challenge every requirement, prefer deletion.",
        ],
      },
      {
        end: "ctx_session (memory).",
        continues: [
          "Prefer stdlib and native platform alternatives before adding code or dependencies.",
          "Solution efficiency ladder:",
          "challenge every requirement, prefer deletion.",
        ],
      },
      { end: "Prefer stdlib and native platform alternatives before adding code or dependencies." },
      { end: "Preserve validation, security, and error-handling." },
    ],
  },
  {
    start: "lean-ctx policy (mechanically enforced):",
    ends: [
      { end: "are overruled by this policy." },
    ],
  },
] as const;

const HOOK_CONTEXT_FRAME = /(?:^|\n)([A-Za-z]+ hook additional context: )$/;

type JsonRecord = Record<string, unknown>;
type Metrics = {
  requests: number;
  input: number;
  output: number;
  cacheRead: number;
  cacheCreation: number;
  toolSearchOnly: number;
  leanCtxCalls: number;
  droppedHookAttachments: number;
  droppedHookChars: number;
  compactions: number;
};

const metrics: Metrics = {
  requests: 0,
  input: 0,
  output: 0,
  cacheRead: 0,
  cacheCreation: 0,
  toolSearchOnly: 0,
  leanCtxCalls: 0,
  droppedHookAttachments: 0,
  droppedHookChars: 0,
  compactions: 0,
};
let leanCtxSeen = false;
// The lean-ctx MCP server name as this session spells it (`lean-ctx`/`lean_ctx`).
let leanServer: string | undefined;
let commandAttempted = false;
let commandRegistered = false;
// Set by a main-loop compaction; the next prompt carries the lean-ctx session state once.
let resumePending = false;

export const register: Register = (on, options) => {
  // The cockpit: sidebar, pulse line, turn and milestone overlays (cockpit.tsx).
  registerCockpit(
    on,
    {},
    {
      panel: asRecord(options).cockpit_panel !== false,
      pulse: asRecord(options).cockpit_pulse_line !== false,
      cards: asRecord(options).cockpit_turn_cards !== false,
    },
  );
  // ctx_patch / ctx_read / ctx_search results drawn as diffs and code
  // (render.tsx). Display only: the model receives every result unchanged.
  if (asRecord(options).color_output !== false) registerResultRendering(on);
  const frontLoaded = getFrontLoadedTools(options);
  const keepHookContext = asRecord(options).keep_hook_context === true;

  on("tool.describe", { tool: LEAN_CTX_TOOL_PATTERN }, async ($, event) => {
    const match = getLeanCtxTool(event.tool);
    if (!match) return { description: event.description, isDeferred: event.isDeferred };

    leanCtxSeen = true;
    leanServer = match.server;
    await ensureMeterCommand($);
    return {
      description: event.description,
      isDeferred: !frontLoaded.has(match.tool),
    };
  });

  // Live skill (concept K5): the shipped SKILL.md is static; this prefixes it
  // with what is true in *this* session, so the guidance never contradicts the
  // mod or the configured tool surface. Byte-stable for a given
  // configuration, so it never churns the prompt cache. The lean-ctx-jobs
  // plugin adds its own section for job wakes and Bash shaping.
  on("skill.prompt", { skill: "lean-ctx" }, async ($, event, next) => {
    const base = await next(event);
    return { text: `${liveSkillHeader(frontLoaded)}\n\n${base.text}` };
  });

  // Code Mode: Claude calls tools from REPL code, and the lean-ctx names carry
  // a hyphen, so `tools.ctx_read` and `tools.mcp__lean_ctx__…` both throw.
  // Measured: two failed calls per session before the model found the form.
  // One session-side section, only while REPL is offered and lean-ctx is
  // connected; byte-stable for a given server name.
  on("prompt.compose", async ($, event, next) => {
    const result = await next(event);
    if (!leanServer || !event.tools.includes("REPL")) return result;
    return {
      sections: [
        ...result.sections,
        { id: "lean-ctx:code-mode", text: codeModeSection(leanServer), scope: "session" },
      ],
    };
  });

  // The MCP instructions and live skill carry the durable guidance. Drop only
  // exact lean-ctx SessionStart/UserPromptSubmit hook blocks; other authors and
  // other hook events pass through unchanged. No agentId check keeps this
  // channel diet active in both the main loop and subagents.
  on("prompt.attachment", async ($, event, next) => {
    if (
      keepHookContext ||
      event.origin.kind !== "hook" ||
      (event.origin.event !== "SessionStart" && event.origin.event !== "UserPromptSubmit")
    ) {
      return next(event);
    }
    const stripped = stripLeanCtxHookText(event.text);
    if (stripped.removedChars === 0) return next(event);
    metrics.droppedHookAttachments += 1;
    metrics.droppedHookChars += stripped.removedChars;
    return { text: stripped.text || null };
  });

  on("session.start", async ($, event, next) => {
    resetSessionState();
    // Detect lean-ctx up front so `/leanctx` exists before the first model
    // request (tool.describe only fires once a request renders the tools).
    // A server still connecting is picked up later by the tool hooks.
    if (!leanCtxSeen) {
      try {
        const match = (await $.tool.list())
          .map((tool) => getLeanCtxTool(tool.name))
          .find((found) => found !== undefined);
        if (match) {
          leanCtxSeen = true;
          leanServer = match.server;
        }
      } catch {
        // Listing is best-effort; the tool hooks still detect lean-ctx.
      }
    }
    if (leanCtxSeen) await ensureMeterCommand($);
    return next(event);
  });

  // K8 compaction coordination: before the main conversation is compacted,
  // lean-ctx persists its session state and the summarizer is told what lean-ctx
  // still depends on; afterwards the next prompt carries that state once.
  // Subagent compactions and every failure pass through untouched.
  on("session.compact", async ($, event, next) => {
    if (!leanServer || event.agentId) return next(event);
    const server = leanServer;
    try {
      await $.mcp.call(server, "ctx_session", { action: "save" });
    } catch {
      // Saving is best-effort; compaction proceeds regardless.
    }
    const instructions = [event.instructions, compactionInstructions()].filter(Boolean).join("\n\n");
    const result = await next({ ...event, instructions });
    if (!result.skip) {
      metrics.compactions += 1;
      resumePending = true;
    }
    return result;
  });

  on("prompt.submit", async ($, event, next) => {
    if (!resumePending || !leanServer) return next(event);
    resumePending = false;
    const state = await sessionState($, leanServer);
    return state ? next({ ...event, context: [...(event.context ?? []), state] }) : next(event);
  });

  on("turn.step", async function* ($, event, next) {
    const result = yield* next(event);
    if (leanCtxSeen) recordRequest(result);
    return result;
  });

  on("command.run", { command: "leanctx" }, async ($, event, next) => {
    return commandRegistered ? { text: formatMetrics() } : next(event);
  });
};

// What the compaction summary must keep for lean-ctx to stay usable: recovery
// handles of compressed output the ongoing work refers to. The lean-ctx-jobs
// plugin adds the ids of jobs that will still wake the model.
function compactionInstructions(): string {
  return "lean-ctx: keep, verbatim, any lean-ctx recovery handles the ongoing work still relies on (ctx_expand ids and `full original at …` paths).";
}

// The lean-ctx session state (task, decisions, findings) for the first prompt
// after a compaction, bounded so it can never crowd out the conversation.
async function sessionState($: EngineInterface, server: string): Promise<string | undefined> {
  try {
    const response = await $.mcp.call(server, "ctx_session", { action: "status" });
    if (response.isError) return undefined;
    const text = mcpText(response).trim();
    if (!text) return undefined;
    const bounded = text.length <= MAX_RESUME_CHARS ? text : `${text.slice(0, MAX_RESUME_CHARS)}…`;
    return `lean-ctx session state, restored after compaction:\n${bounded}`;
  } catch {
    return undefined;
  }
}

function liveSkillHeader(frontLoaded: Set<string>): string {
  const front = [...frontLoaded].sort().join(", ");
  return [
    "## Live in this session (lean-ctx Claude Code mod)",
    `- In your tool list: ${front}. Other lean-ctx tools load with one ToolSearch \`select:\` or run via \`ctx_call\`.`,
  ].join("\n");
}

function codeModeSection(server: string): string {
  return [
    "# lean-ctx in REPL code",
    `Call lean-ctx tools by their full name with bracket syntax: \`await tools["mcp__${server}__ctx_read"]({ path, mode })\`, the name spelled exactly so; \`tools.ctx_read\` is not defined.`,
  ].join("\n");
}

function getFrontLoadedTools(options: unknown): Set<string> {
  const configured = asRecord(options).front_loaded_tools;
  if (!Array.isArray(configured)) return new Set(DEFAULT_FRONT_LOADED_TOOLS);
  return new Set(configured.filter((value): value is string => typeof value === "string"));
}

function getLeanCtxTool(name: string): { server: string; tool: string } | undefined {
  const match = LEAN_CTX_TOOL_NAME.exec(name);
  if (!match?.[1] || !match[2]) return undefined;
  return { server: match[1], tool: match[2] };
}

function mcpText(response: McpToolResult): string {
  return response.content
    .map((block) => (block.type === "text" ? block.text : ""))
    .filter(Boolean)
    .join("\n");
}

type LeanCtxHookTextBlock = { start: number; end: number };

function stripLeanCtxHookText(text: string): { text: string; removedChars: number } {
  let remaining = text;
  let removedChars = 0;
  while (true) {
    const block = LEAN_CTX_HOOK_TEXT_SIGNATURES
      .map((signature) => findLeanCtxHookTextBlock(remaining, signature))
      .find((candidate) => candidate !== undefined);
    if (!block) break;
    const next = removeJoinedTextBlock(remaining, block);
    if (next === remaining) break;
    removedChars += remaining.length - next.length;
    remaining = next;
  }
  return { text: remaining, removedChars };
}

function findLeanCtxHookTextBlock(
  text: string,
  signature: LeanCtxHookTextSignature,
): LeanCtxHookTextBlock | undefined {
  let start = text.indexOf(signature.start);
  while (start !== -1) {
    // Claude Code frames hook context as "<Event> hook additional context: "
    // on the same line; the frame goes with the block it introduces.
    const frame = start === 0 || text[start - 1] === "\n" ? "" : HOOK_CONTEXT_FRAME.exec(text.slice(0, start))?.[1];
    if (frame !== undefined) {
      const end = findLeanCtxHookTextEnd(text, start, signature);
      if (end !== undefined) return { start: start - frame.length, end };
    }
    start = text.indexOf(signature.start, start + 1);
  }
  return undefined;
}

function findLeanCtxHookTextEnd(
  text: string,
  start: number,
  signature: LeanCtxHookTextSignature,
): number | undefined {
  let lineStart = start;
  while (lineStart <= text.length) {
    const end = lineEnd(text, lineStart);
    const line = text.slice(lineStart, end);
    const ending = signature.ends.find((marker) => line.endsWith(marker.end));
    if (ending) {
      const nextLine = nextNonEmptyLineStart(text, end);
      const next = text.slice(nextLine, lineEnd(text, nextLine));
      if (!ending.continues?.some((marker) => next.startsWith(marker))) return end;
      lineStart = nextLine;
      continue;
    }
    if (end === text.length) break;
    lineStart = text.startsWith("\r\n", end) ? end + 2 : end + 1;
  }
}

function nextNonEmptyLineStart(text: string, end: number): number {
  let next = end;
  while (next < text.length) {
    if (text.startsWith("\r\n", next)) next += 2;
    else if (text[next] === "\n") next += 1;
    else break;
    if (text.slice(next, lineEnd(text, next)) !== "") break;
  }
  return next;
}

function lineEnd(text: string, from: number): number {
  const newline = text.indexOf("\n", from);
  if (newline === -1) return text.length;
  return newline > from && text[newline - 1] === "\r" ? newline - 1 : newline;
}

function removeJoinedTextBlock(text: string, block: LeanCtxHookTextBlock): string {
  let before = text.slice(0, block.start);
  let after = text.slice(block.end);
  if (before.endsWith("\r\n")) before = before.slice(0, -2);
  else if (before.endsWith("\n")) before = before.slice(0, -1);
  else if (after.startsWith("\r\n")) after = after.slice(2);
  else if (after.startsWith("\n")) after = after.slice(1);
  return `${before}${after}`;
}

function recordRequest(result: TurnStepResult): void {
  metrics.requests += 1;
  if (result.usage) {
    metrics.input += nonNegative(result.usage.input_tokens);
    metrics.output += nonNegative(result.usage.output_tokens);
    metrics.cacheRead += nonNegative(result.usage.cache_read_input_tokens);
    metrics.cacheCreation += nonNegative(result.usage.cache_creation_input_tokens);
  }
  if (
    result.toolUses.length > 0 &&
    result.toolUses.every((use) => /(?:^|[_-])tool[_-]?search$/i.test(use.name))
  ) {
    metrics.toolSearchOnly += 1;
  }
  metrics.leanCtxCalls += result.toolUses.filter((use) => getLeanCtxTool(use.name) !== undefined).length;
}

function formatMetrics(): string {
  return [
    `Requests ${metrics.requests}`,
    `input ${metrics.input}`,
    `output ${metrics.output}`,
    `cache read ${metrics.cacheRead}`,
    `cache creation ${metrics.cacheCreation}`,
    `ToolSearch-only ${metrics.toolSearchOnly}`,
    `lean-ctx calls ${metrics.leanCtxCalls}`,
    `hook attachments dropped ${metrics.droppedHookAttachments} (−${metrics.droppedHookChars} chars)`,
    `compactions ${metrics.compactions}`,
  ].join(" · ");
}

function nonNegative(value: number): number {
  return Number.isFinite(value) && value > 0 ? value : 0;
}

async function ensureMeterCommand($: EngineInterface): Promise<void> {
  if (commandAttempted) return;
  commandAttempted = true;
  try {
    const command = await $.command.register({
      name: "leanctx",
      description: "Show per-session Claude Code request and lean-ctx usage.",
    });
    commandRegistered = command.command === "leanctx";
  } catch {
    // The name may already be taken; keep all other mod behavior fail-open.
  }
}

function resetSessionState(): void {
  metrics.requests = 0;
  metrics.input = 0;
  metrics.output = 0;
  metrics.cacheRead = 0;
  metrics.cacheCreation = 0;
  metrics.toolSearchOnly = 0;
  metrics.leanCtxCalls = 0;
  metrics.droppedHookAttachments = 0;
  metrics.droppedHookChars = 0;
  metrics.compactions = 0;
  resumePending = false;
}

function asRecord(value: unknown): JsonRecord {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as JsonRecord)
    : {};
}
