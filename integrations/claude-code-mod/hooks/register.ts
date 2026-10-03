import type {
  EngineInterface,
  McpToolResult,
  Register,
  Timer,
  ToolCallResult,
  TurnStepResult,
} from "claude-code";

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
};
let watcher: Timer | undefined;
let watcherTickInProgress = false;
let leanCtxSeen = false;
let commandAttempted = false;
let commandRegistered = false;

export const register: Register = (on, options) => {
  const frontLoaded = getFrontLoadedTools(options);

  on("tool.describe", { tool: LEAN_CTX_TOOL_PATTERN }, async ($, event) => {
    const match = getLeanCtxTool(event.tool);
    if (!match) return { description: event.description, isDeferred: event.isDeferred };

    leanCtxSeen = true;
    await ensureMeterCommand($);
    return {
      description: event.description,
      isDeferred: !frontLoaded.has(match.tool),
    };
  });

  on("session.start", async ($, event, next) => {
    resetSessionState();
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
      return next(event);
    }

    leanCtxSeen = true;
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

  on("turn.step", async function* ($, event, next) {
    const result = yield* next(event);
    if (leanCtxSeen) recordRequest(result);
    return result;
  });

  on("command.run", { command: "leanctx" }, async ($, event, next) => {
    return commandRegistered ? { text: formatMetrics() } : next(event);
  });
};

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
    for (const check of checks) {
      if (watchedJobs.get(check.key) !== check.job) continue;
      if (!check.status || check.status.state === "running") {
        check.job.misses = 0;
        continue;
      }
      if (check.status.state === "unknown") {
        check.job.misses += 1;
        if (check.job.misses >= MAX_STATUS_MISSES) watchedJobs.delete(check.key);
        continue;
      }
      watchedJobs.delete(check.key);
      if (check.response) finished.push({ job: check.job, status: check.status, response: check.response });
    }
    stopWatcherWhenIdle();
    if (finished.length > 0) submitWake($, finished);
  } catch {
    watchedJobs.clear();
    stopWatcherWhenIdle();
    // Restore native status polling when the watcher cannot safely read a job.
  } finally {
    watcherTickInProgress = false;
  }
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

function submitWake($: EngineInterface, finished: FinishedJob[]): void {
  const text = formatWakeSummary(finished);
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
  watchedJobs.clear();
  stopWatcherWhenIdle();
}

function asRecord(value: unknown): JsonRecord {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as JsonRecord)
    : {};
}
