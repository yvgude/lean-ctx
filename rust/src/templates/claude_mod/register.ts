// SPDX-License-Identifier: Apache-2.0
import type {
  EngineInterface,
  McpToolResult,
  Register,
  Timer,
  ToolCallResult,
  TurnStepResult,
} from "claude-code";

import { registerCockpit } from "./cockpit";

const DEFAULT_FRONT_LOADED_TOOLS = [
  "ctx_read",
  "ctx_search",
  "ctx_shell",
  "ctx_compose",
  "ctx_callgraph",
  "ctx_session",
];
// Every tool of the lean-ctx server, not only `ctx_*`: the `shell` alias of
// ctx_shell would otherwise stay front-loaded under `alwaysLoad`.
const LEAN_CTX_TOOL_PATTERN = /^mcp__lean[-_]ctx__[A-Za-z0-9_-]+$/;
const LEAN_CTX_TOOL_NAME = /^mcp__(lean[-_]ctx)__([A-Za-z0-9_-]+)$/;
const SHELL_TOOLS = new Set(["ctx_shell", "shell"]);
const WATCH_CONTEXT =
  "This background job is being watched; you will be woken automatically on completion, so do not poll or sleep.";
const WATCH_INTERVAL_MS = 2_000;
// Consecutive unreadable status checks before a job is handed back to the
// model's own polling; one transient MCP hiccup must not drop the watch.
const MAX_STATUS_MISSES = 5;
const MAX_TAIL_LINES = 20;
const MAX_TAIL_LINE_CHARS = 300;
// Native Bash stdout below this is kept byte-for-byte: the shaping gain cannot
// pay for the extra hop, and short output is usually exactly what was asked.
const SHAPE_MIN_CHARS = 2_000;
// Commands that explicitly ask for exact bytes are never shaped.
const RAW_INTENT = /\bLEAN_CTX_(?:RAW|DISABLED)=1\b|\blean-ctx\s+raw\b/;
// Upper bound for the session state injected after a compaction (~500 tokens).
const MAX_RESUME_CHARS = 2_000;
type LeanCtxHookTextEnd = { end: string; continues?: readonly string[] };
type LeanCtxHookTextSignature = { start: string; ends: readonly LeanCtxHookTextEnd[] };
// Exact leading signatures emitted by observe.rs. Keep these stable and update
// the cross-language drift test there whenever an authored hook text changes.
const LEAN_CTX_HOOK_TEXT_SIGNATURES: readonly LeanCtxHookTextSignature[] = [
  {
    start: "lean-ctx active: ALWAYS use ctx_* MCP tools instead of native equivalents.",
    ends: [
      { end: "Exclusive tools: ctx_compose, ctx_callgraph, ctx_knowledge, ctx_session." },
    ],
  },
  {
    start: "CRITICAL: ALWAYS use lean-ctx ctx_* tools as mapped below.",
    ends: [
      {
        end: "Use native Read for out-of-root; `lean-ctx doctor` shows effective roots.",
        continues: [
          "Advanced tools not in your profile are available via ctx_call(tool=<name>) gateway.",
          "Prefer stdlib and native platform alternatives before adding code or dependencies.",
          "Solution efficiency ladder:",
          "challenge every requirement, prefer deletion.",
        ],
      },
      {
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
type WatchJob = { server: string; id: string; contextSent: boolean; misses: number };
type JobStatus = { state: "running" | "terminal" | "unknown"; exitCode?: number; archiveId?: string; summary?: string };
type FinishedJob = { job: WatchJob; status: JobStatus; response: McpToolResult };
type JobCheck = {
  key: string;
  job: WatchJob;
  status?: JobStatus;
  response?: McpToolResult;
};
type Metrics = {
  requests: number;
  input: number;
  output: number;
  cacheRead: number;
  cacheCreation: number;
  toolSearchOnly: number;
  leanCtxCalls: number;
  sleepsAnswered: number;
  wakesDelivered: number;
  shapedCalls: number;
  shapedCharsSaved: number;
  droppedHookAttachments: number;
  droppedHookChars: number;
  compactions: number;
};

const watchedJobs = new Map<string, WatchJob>();
const metrics: Metrics = {
  requests: 0,
  input: 0,
  output: 0,
  cacheRead: 0,
  cacheCreation: 0,
  toolSearchOnly: 0,
  leanCtxCalls: 0,
  sleepsAnswered: 0,
  wakesDelivered: 0,
  shapedCalls: 0,
  shapedCharsSaved: 0,
  droppedHookAttachments: 0,
  droppedHookChars: 0,
  compactions: 0,
};
let watcher: Timer | undefined;
let watcherTickInProgress = false;
let leanCtxSeen = false;
// The lean-ctx MCP server name as this session spells it (`lean-ctx`/`lean_ctx`).
let leanServer: string | undefined;
let commandAttempted = false;
let commandRegistered = false;
// Set by a main-loop compaction; the next prompt carries the lean-ctx session state once.
let resumePending = false;

export const register: Register = (on, options) => {
  // The cockpit: sidebar, pulse line, turn and milestone overlays (cockpit.tsx).
  registerCockpit(on);
  const frontLoaded = getFrontLoadedTools(options);
  const shapeNative = asRecord(options).shape_native_output !== false;
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
  // mod (e.g. "no need to poll") or the configured tool surface. Byte-stable
  // for a given configuration, so it never churns the prompt cache.
  on("skill.prompt", { skill: "lean-ctx" }, async ($, event, next) => {
    const base = await next(event);
    return { text: `${liveSkillHeader(frontLoaded, shapeNative)}\n\n${base.text}` };
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

  on("tool.call", async ($, event, next) => {
    const fields = event as unknown as JsonRecord;
    const tool = typeof fields.tool === "string" ? fields.tool : "";
    const leanCtxTool = getLeanCtxTool(tool);

    if (!leanCtxTool) {
      if (tool === "Bash" && watchedJobs.size > 0 && isSleepWait(readString(fields, "command"))) {
        metrics.sleepsAnswered += 1;
        return { result: sleepAnswer() };
      }
      if (tool === "Bash" && shapeNative) {
        return shapeBash($, readString(fields, "command"), await next(event));
      }
      return next(event);
    }

    leanCtxSeen = true;
    leanServer = leanCtxTool.server;
    metrics.leanCtxCalls += 1;
    await ensureMeterCommand($);

    if (
      SHELL_TOOLS.has(leanCtxTool.tool) &&
      watchedJobs.size > 0 &&
      isSleepWait(readString(fields, "command"))
    ) {
      metrics.sleepsAnswered += 1;
      return { result: sleepAnswer() };
    }

    if (SHELL_TOOLS.has(leanCtxTool.tool) && fields.run_in_background) {
      const result = await next(event);
      const jobId = extractJobId(result);
      if (!jobId) return result;

      const key = watchKey(leanCtxTool.server, jobId);
      const job = watchedJobs.get(key) ?? { server: leanCtxTool.server, id: jobId, contextSent: false, misses: 0 };
      watchedJobs.set(key, job);
      if (!startWatcher($)) {
        watchedJobs.delete(key);
        stopWatcherWhenIdle();
        return result;
      }
      return addWatchContext(result, job);
    }

    if (SHELL_TOOLS.has(leanCtxTool.tool) && fields.background_action === "status") {
      const jobId = readString(fields, "job_id");
      const job = jobId ? watchedJobs.get(watchKey(leanCtxTool.server, jobId)) : undefined;
      const result = await next(event);
      return job ? addWatchContext(result, job) : result;
    }

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

// Shape, don't redirect (concept K2): the native Bash call already ran; its
// stdout goes through lean-ctx's command-aware compressor (`ctx_shape`, which
// also applies lean-ctx's secret redaction and output filters) instead of
// denying the call and forcing a round trip to ctx_shell. Lossy results end
// with a recovery handle. stderr is kept verbatim. Fail-open everywhere.
async function shapeBash(
  $: EngineInterface,
  command: string | undefined,
  result: ToolCallResult,
): Promise<ToolCallResult> {
  if (!leanServer || !command || RAW_INTENT.test(command)) return result;
  if ("deny" in result || result.isError) return result;
  const record = asRecord(result.result);
  const stdout = typeof record.stdout === "string" ? record.stdout : "";
  if (
    stdout.length < SHAPE_MIN_CHARS ||
    record.isImage ||
    record.backgroundTaskId ||
    record.persistedOutputPath
  ) {
    return result;
  }
  try {
    // A non-error result without a special-exit interpretation exited 0, so the
    // engine takes its success path (folds build/test noise); otherwise it gets
    // no exit code and keeps the unknown-outcome guard.
    const exitCode = record.returnCodeInterpretation ? {} : { exit_code: 0 };
    const shaped = await $.mcp.call(leanServer, "ctx_shape", {
      tool: "Bash",
      command,
      output: stdout,
      ...exitCode,
    });
    if (shaped.isError) return result;
    const text = mcpText(shaped);
    if (!text || text.length >= stdout.length) return result;
    metrics.shapedCalls += 1;
    metrics.shapedCharsSaved += stdout.length - text.length;
    return { result: { ...record, stdout: text }, context: result.context } as ToolCallResult;
  } catch {
    return result;
  }
}

// What the compaction summary must keep for lean-ctx to stay usable: ids of
// jobs that will still wake the model, and recovery handles of compressed
// output the ongoing work refers to. Deterministic for a given watch set.
function compactionInstructions(): string {
  const ids = [...watchedJobs.values()].map((job) => job.id).sort();
  const lines = [
    "lean-ctx: keep, verbatim, any lean-ctx recovery handles the ongoing work still relies on (ctx_expand ids and `full original at …` paths).",
  ];
  if (ids.length > 0) {
    lines.push(
      `lean-ctx: background job(s) ${ids.join(", ")} are still running and will report when done — keep their ids and do not plan to poll them.`,
    );
  }
  return lines.join("\n");
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

function liveSkillHeader(frontLoaded: Set<string>, shapeNative: boolean): string {
  const front = [...frontLoaded].sort().join(", ");
  return [
    "## Live in this session (lean-ctx Claude Code mod)",
    "- Background jobs (`ctx_shell(run_in_background=true)` or Bash `run_in_background`) wake you when they finish: start them, then continue other work or end the turn. Never `sleep` or poll their status.",
    `- In your tool list: ${front}. Other lean-ctx tools load with one ToolSearch \`select:\` or run via \`ctx_call\`.`,
    shapeNative
      ? "- Native Bash output is compressed automatically (a recovery handle ends any lossy result); there is no need to route commands through ctx_shell just for compression."
      : "- Native Bash output is passed through unchanged in this session.",
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

function readString(record: JsonRecord, key: string): string | undefined {
  return typeof record[key] === "string" ? (record[key] as string) : undefined;
}

function isSleepWait(command: string | undefined): boolean {
  return !!command &&
    /^\s*sleep\s+\d+(?:\.\d+)?(?:\s*&&\s*(?=[^;\n]*(?:\bstatus\b|\btail\b))[^;\n]*)?\s*$/i.test(command);
}

function sleepAnswer(): string {
  const ids = [...watchedJobs.values()].map((job) => job.id).sort();
  return `No need to wait: background job(s) ${ids.join(", ")} are watched and you will be woken automatically; end the turn or continue other work.`;
}

function watchKey(server: string, id: string): string {
  return `${server}\u0000${id}`;
}

function addWatchContext(result: ToolCallResult, job: WatchJob): ToolCallResult {
  if (job.contextSent || "deny" in result) return result;
  job.contextSent = true;
  const context = [...(result.context ?? [])];
  if (!context.includes(WATCH_CONTEXT)) context.push(WATCH_CONTEXT);
  return { ...result, context };
}

function extractJobId(result: ToolCallResult): string | undefined {
  if ("deny" in result) return undefined;
  return readString(readShellFields(result.result, result.text), "jobId");
}

// ctx_shell reports background state as MCP `structuredContent`
// (`{ jobId, state, exitCode?, archiveId?, summary }`, or `{ jobId, errorCode }`
// for an expired id — rust/src/server/tool_trait.rs). Hosts often render
// only that object as the text block, so a JSON text block is the second
// source; the `[background:…]` text header is the last resort.
function readShellFields(raw: unknown, extraText?: string): JsonRecord {
  const body = asRecord(raw);
  const structured = asRecord(body.structuredContent);
  if (readString(structured, "jobId") || readString(structured, "job_id")) return normalize(structured);

  const texts = [
    ...(Array.isArray(body.content)
      ? body.content.map((block) => asRecord(block).text).filter((t): t is string => typeof t === "string")
      : []),
    ...(extraText ? [extraText] : []),
  ];
  for (const text of texts) {
    const trimmed = text.trim();
    if (!trimmed.startsWith("{")) continue;
    try {
      const parsed = asRecord(JSON.parse(trimmed));
      if (readString(parsed, "jobId") || readString(parsed, "job_id")) return normalize(parsed);
    } catch {
      // Not a JSON block; try the next source.
    }
  }

  const joined = texts.join("\n");
  const header = /\[(?:auto-)?background:([A-Za-z0-9_-]+)\s*(running|started|completed|failed|cancelled|canceled|not found)?(?:,\s*exit\s+(-?\d+))?/i.exec(joined);
  if (!header) return {};
  const word = header[2]?.toLowerCase();
  return {
    jobId: header[1],
    state: word === "started" ? "running" : word === "not found" ? "expired" : word,
    exitCode: header[3] === undefined ? undefined : Number(header[3]),
    text: joined,
  };
}

function normalize(fields: JsonRecord): JsonRecord {
  return {
    jobId: readString(fields, "jobId") ?? readString(fields, "job_id"),
    state: readString(fields, "errorCode") ? "expired" : readString(fields, "state"),
    exitCode: getNumber(fields, ["exitCode", "exit_code"]),
    archiveId: readString(fields, "archiveId") ?? readString(fields, "archive_id"),
    summary: readString(fields, "summary"),
  };
}

function getNumber(record: JsonRecord, keys: string[]): number | undefined {
  for (const key of keys) {
    const value = record[key];
    if (typeof value === "number" && Number.isFinite(value)) return value;
  }
  return undefined;
}

function startWatcher($: EngineInterface): boolean {
  if (watcher) return true;
  try {
    watcher = $.clock.every(WATCH_INTERVAL_MS, () => {
      void pollWatchedJobs($);
    });
    return true;
  } catch {
    watcher = undefined;
    return false;
  }
}

function stopWatcherWhenIdle(): void {
  if (watchedJobs.size > 0 || !watcher) return;
  try {
    watcher.cancel();
  } catch {
    // Cancellation failure must not affect the model's tool flow.
  }
  watcher = undefined;
}

async function pollWatchedJobs($: EngineInterface): Promise<void> {
  if (watcherTickInProgress || watchedJobs.size === 0) return;
  watcherTickInProgress = true;
  const snapshot = [...watchedJobs.entries()];

  try {
    const checks = await Promise.all(
      snapshot.map(async ([key, job]): Promise<JobCheck> => {
        try {
          const response = await $.mcp.call(job.server, "ctx_shell", {
            background_action: "status",
            job_id: job.id,
          });
          // A finished job with a non-zero exit is an MCP error result too;
          // only the parsed state decides, never `isError`.
          return { key, job, response, status: inspectJobStatus(response, job.id) };
        } catch {
          return { key, job, status: { state: "unknown" } };
        }
      }),
    );

    const finished: FinishedJob[] = [];
    const lost: WatchJob[] = [];
    for (const check of checks) {
      if (watchedJobs.get(check.key) !== check.job) continue;
      if (!check.status || check.status.state === "running") {
        check.job.misses = 0;
        continue;
      }
      if (check.status.state === "unknown") {
        check.job.misses += 1;
        if (check.job.misses >= MAX_STATUS_MISSES) {
          watchedJobs.delete(check.key);
          lost.push(check.job);
        }
        continue;
      }
      watchedJobs.delete(check.key);
      if (check.response) finished.push({ job: check.job, status: check.status, response: check.response });
    }
    stopWatcherWhenIdle();
    const text = [
      finished.length > 0 ? formatWakeSummary(finished) : "",
      lost.length > 0 ? formatLostNotice(lost) : "",
    ].filter(Boolean).join("\n\n");
    if (text) submitWake($, text);
  } catch {
    // The watcher can no longer read job state: hand every watched job back
    // to the model explicitly — it was told it would be woken, so a silent
    // drop would leave it waiting forever.
    const lost = [...watchedJobs.values()];
    watchedJobs.clear();
    stopWatcherWhenIdle();
    if (lost.length > 0) submitWake($, formatLostNotice(lost));
  } finally {
    watcherTickInProgress = false;
  }
}

function formatLostNotice(lost: WatchJob[]): string {
  const ids = lost.map((job) => job.id).sort();
  return `lean-ctx can no longer watch background job(s) ${ids.join(", ")}: check each once with ctx_shell(background_action="status", job_id=…) when you need its result.`;
}

function inspectJobStatus(response: McpToolResult, jobId: string): JobStatus {
  const fields = readShellFields(response);
  if (fields.jobId !== undefined && fields.jobId !== jobId) return { state: "unknown" };
  const state = readString(fields, "state")?.toLowerCase();
  if (state === "running") return { state: "running" };
  if (state === "completed" || state === "failed" || state === "cancelled" || state === "canceled" || state === "expired") {
    return {
      state: "terminal",
      exitCode: getNumber(fields, ["exitCode"]),
      archiveId: readString(fields, "archiveId"),
      summary: readString(fields, "summary"),
    };
  }
  return { state: "unknown" };
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

function submitWake($: EngineInterface, text: string): void {
  try {
    void $.prompt
      .submit({ text })
      .then(() => {
        metrics.wakesDelivered += 1;
      })
      .catch(() => {
        // A failed wake leaves native status polling available.
      });
  } catch {
    // A failed wake leaves native status polling available.
  }
}

function formatWakeSummary(finished: FinishedJob[]): string {
  const ordered = [...finished].sort((a, b) =>
    a.job.id < b.job.id ? -1 : a.job.id > b.job.id ? 1 : a.job.server < b.job.server ? -1 : 1,
  );
  const sections = ordered.map(({ job, status, response }) => {
    const handle = recoveryHandle(response, status.archiveId);
    const lines = tailLines(response, job.id);
    const body = handle
      ? `Full output: ${handle}`
      : status.summary || lines.join("\n") || "No output captured.";
    const exit = status.exitCode === undefined ? "unknown" : status.exitCode;
    return `Job ${job.id} finished · exit ${exit}\n${body}`;
  });
  return `Watched background job(s) finished:\n${sections.join("\n\n")}`;
}

function recoveryHandle(response: McpToolResult, archiveId?: string): string | undefined {
  if (archiveId) return `ctx_expand id=${archiveId}`;
  const structured = asRecord(response.structuredContent);
  const direct =
    readString(structured, "recovery_handle") ??
    readString(structured, "recoveryHandle") ??
    readString(structured, "tee_path") ??
    readString(structured, "teePath");
  if (direct) return direct;

  const match = /(?:full output at|full output:)\s+([^\]\n]*?)(?:\s+—|\]|$)/i.exec(mcpText(response));
  return match?.[1]?.trim();
}

function tailLines(response: McpToolResult, jobId: string): string[] {
  const escapedId = jobId.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const statusLine = new RegExp(`^\\[background:${escapedId}(?:\\s|\\])`, "i");
  return mcpText(response)
    .split(/\r?\n/)
    // Status headers and the structured JSON echo are metadata, not output.
    .filter((line) => {
      const trimmed = line.trim();
      return trimmed.length > 0 && !statusLine.test(trimmed) && !trimmed.startsWith("{");
    })
    .slice(-MAX_TAIL_LINES)
    .map((line) => (line.length <= MAX_TAIL_LINE_CHARS ? line : `${line.slice(0, MAX_TAIL_LINE_CHARS)}…`));
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
    `sleeps answered ${metrics.sleepsAnswered}`,
    `wakes delivered ${metrics.wakesDelivered}`,
    `Bash outputs shaped ${metrics.shapedCalls} (−${metrics.shapedCharsSaved} chars)`,
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
  metrics.sleepsAnswered = 0;
  metrics.wakesDelivered = 0;
  metrics.shapedCalls = 0;
  metrics.shapedCharsSaved = 0;
  metrics.droppedHookAttachments = 0;
  metrics.droppedHookChars = 0;
  metrics.compactions = 0;
  resumePending = false;
  watchedJobs.clear();
  stopWatcherWhenIdle();
}

function asRecord(value: unknown): JsonRecord {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as JsonRecord)
    : {};
}
