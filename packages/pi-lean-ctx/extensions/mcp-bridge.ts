// The MCP SDK (incl. zod) is consumed through a self-contained vendor bundle
// instead of node_modules: pi's shared npm prefix rewrites every installed
// package on each `pi install`/`remove`, and an interrupted rewrite corrupted
// zod beyond repair (GH #670). scripts/build-vendor.mjs generates the bundle
// at prepack; extensions/vendor/mcp-sdk.d.cts carries its types.
import { Client, StdioClientTransport } from "./vendor/mcp-sdk.cjs";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { type TSchema, Type } from "typebox";
import type { McpBridgeRetryState, McpBridgeStatus } from "./types.js";

/** Result shape returned by the MCP client's `callTool`. */
export type McpCallResult = Awaited<ReturnType<Client["callTool"]>>;

const MAX_RECONNECT_ATTEMPTS = 3;
const RECONNECT_DELAY_MS = 2000;
const TOOL_CALL_TIMEOUT_MS = 120000;
/**
 * Upper bound on a single connect attempt. Mirrors index.ts's eager
 * `BRIDGE_STARTUP_TIMEOUT_MS` so the lazy first call is bounded the same way
 * the cache-miss startup already was, instead of inheriting the MCP SDK's 60s
 * request default.
 */
const CONNECT_TIMEOUT_MS = 10000;
/**
 * How long a failed connect short-circuits further attempts. Short and fixed:
 * long enough that a broken binary cannot make every read pay the full bound,
 * short enough that a transient failure still retries within one turn.
 */
const CONNECT_FAILURE_COOLDOWN_MS = 2000;

export type McpTool = {
  name: string;
  description?: string;
  inputSchema?: Record<string, unknown>;
};

export type McpBridgeHooks = {
  /** Test/runtime seam for connection setup; production uses the MCP transport. */
  connect?: () => Promise<void>;
  /** Test/runtime seam for schema discovery. */
  listTools?: () => Promise<McpTool[]>;
  /** Test/runtime seam for direct tool calls. */
  callTool?: (
    name: string,
    args: Record<string, unknown>,
    signal?: AbortSignal,
  ) => Promise<McpCallResult>;
  /** Test/runtime seam for cleanup. */
  close?: () => Promise<void>;
};

export type McpBridgeOptions = {
  onSchemasDiscovered?: (tools: McpTool[]) => void | Promise<void>;
  hooks?: McpBridgeHooks;
  /** Bound on one connect attempt; `0` disables the bound. Tests shorten it. */
  connectTimeoutMs?: number;
  /** Negative-cache window after a failed connect; `0` disables it. */
  connectFailureCooldownMs?: number;
};

/** Coalesce all callers arriving before one async start completes. */
export function createCoalescedStarter(start: () => Promise<void>): () => Promise<void> {
  let pending: Promise<void> | undefined;
  return () => {
    if (!pending) {
      pending = start().finally(() => {
        pending = undefined;
      });
    }
    return pending;
  };
}

/**
 * How the bridge should expose discovered MCP tools, so lean-ctx can coexist
 * with other Pi extensions (AFT, magic-context) instead of crashing on a name
 * collision (issue #359).
 */
export type BridgeToolPolicy = {
  /** Lower-cased tool names the bridge must not register at all. */
  disabledTools: Set<string>;
  /**
   * Tool names already owned by a local CLI-first replacement in `index.ts`
   * (e.g. `ctx_read`, `ctx_shell`). The bridge must not re-register their MCP
   * namesakes. This is the *actual* set of locally registered names, supplied
   * by `index.ts`, so a tool can never be suppressed without a replacement
   * (the root cause of issue #409).
   */
  localTools: Set<string>;
  /** Optional prefix applied to the Pi-facing tool name (not the MCP call). */
  toolPrefix?: string;
};

const DEFAULT_TOOL_POLICY: BridgeToolPolicy = {
  disabledTools: new Set(),
  localTools: new Set(),
};

/**
 * Partition discovered MCP tools into the ones the bridge should register and
 * the ones it must skip. A tool is skipped if and only if it is owned by a
 * local CLI-first replacement (`localTools`); anything in `disabledTools` is
 * handed to another extension (#359). Pure and exported so the #409 invariant —
 * never suppress a tool without a local replacement — is locked by unit tests.
 */
export function selectBridgeTools(
  tools: McpTool[],
  localTools: Set<string>,
  disabledTools: Set<string>,
): { toRegister: McpTool[]; disabled: string[] } {
  const toRegister: McpTool[] = [];
  const disabled: string[] = [];
  for (const tool of tools) {
    if (localTools.has(tool.name)) continue;
    if (disabledTools.has(tool.name.toLowerCase())) {
      disabled.push(tool.name);
      continue;
    }
    toRegister.push(tool);
  }
  return { toRegister, disabled };
}

function isAbortLikeError(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  const msg = error.message.toLowerCase();
  return error.name === "AbortError"
    || msg.includes("aborted")
    || msg.includes("cancelled")
    || msg.includes("canceled");
}

function isHostToolRejection(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  const msg = error.message.toLowerCase();
  return msg.includes("the user doesn't want to proceed with this tool use")
    || msg.includes("tool use was rejected")
    || msg.includes("stop what you are doing and wait for the user to tell you how to proceed");
}

function isRetrySafeTool(name: string): boolean {
  const lower = name.toLowerCase();
  const mutatingHints = [
    "edit", "fill", "cache", "workflow",
    "execute", "session", "knowledge", "response",
  ];
  return !mutatingHints.some((hint) => lower.includes(hint));
}

/**
 * Recursively convert a single JSON Schema property to its TypeBox
 * equivalent. Handles enums, nested objects, and typed array items so
 * the LLM receives a faithful schema instead of `Type.Unknown()`.
 *
 * Standalone (not a class method) so it can recurse without `this` and
 * be exported for unit testing.
 */
export function propToTypebox(prop: Record<string, unknown>): TSchema {
  const desc = (prop.description as string) ?? undefined;
  const jsonType = prop.type as string | undefined;

  // JSON Schema `enum` -> Type.Union(Type.Literal(...)) so the LLM
  // knows valid values (e.g. ctx_patch `op`).
  const enumValues = prop.enum as unknown[] | undefined;
  if (Array.isArray(enumValues) && enumValues.length > 0) {
    const literals = enumValues.map((v) => Type.Literal(String(v)));
    return literals.length === 1
      ? Object.assign(literals[0], desc ? { description: desc } : {})
      : Type.Union(literals, desc ? { description: desc } : {});
  }

  switch (jsonType) {
    case "number":
    case "integer":
      return Type.Number({ description: desc });

    case "boolean":
      return Type.Boolean({ description: desc });

    case "array": {
      const items = prop.items as Record<string, unknown> | undefined;
      const itemSchema = items ? propToTypebox(items) : Type.Unknown();
      return Type.Array(itemSchema, desc ? { description: desc } : {});
    }

    case "object": {
      const nested = prop.properties as
        | Record<string, Record<string, unknown>>
        | undefined;
      if (nested) {
        const nestedRequired = new Set(
          (prop.required as string[] | undefined) ?? [],
        );
        const nestedFields: Record<string, TSchema> = {};
        for (const [k, v] of Object.entries(nested)) {
          const f = propToTypebox(v);
          nestedFields[k] = nestedRequired.has(k) ? f : Type.Optional(f);
        }
        return Type.Object(nestedFields, desc ? { description: desc } : {});
      }
      return Type.Record(Type.String(), Type.Unknown(), {
        description: desc,
      });
    }

    default:
      return Type.String({ description: desc });
  }
}

export class McpBridge {
  private client: Client | null = null;
  private transport: StdioClientTransport | null = null;
  private registeredTools: string[] = [];
  private skippedTools: string[] = [];
  private disabledToolNames: string[] = [];
  private connected = false;
  private binary: string;
  private extraEnv: Record<string, string>;
  private policy: BridgeToolPolicy;
  private reconnectAttempts = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | undefined;
  private shuttingDown = false;
  private lastError: string | undefined;
  private lastHungTool: string | undefined;
  private lastRetry: McpBridgeRetryState | undefined;
  private readonly onSchemasDiscovered?: (tools: McpTool[]) => void | Promise<void>;
  private readonly hooks: McpBridgeHooks;
  private readonly ensureConnectedCoalesced: () => Promise<void>;
  private readonly connectTimeoutMs: number;
  private readonly connectFailureCooldownMs: number;
  private connectBlockedUntil: number | undefined;
  private startupMode: "eager" | "lazy" | undefined;

  constructor(
    binary: string,
    extraEnv: Record<string, string> = {},
    policy: BridgeToolPolicy = DEFAULT_TOOL_POLICY,
    options: McpBridgeOptions = {},
  ) {
    this.binary = binary;
    this.extraEnv = extraEnv;
    this.policy = policy;
    this.onSchemasDiscovered = options.onSchemasDiscovered;
    this.hooks = options.hooks ?? {};
    this.connectTimeoutMs = options.connectTimeoutMs ?? CONNECT_TIMEOUT_MS;
    this.connectFailureCooldownMs =
      options.connectFailureCooldownMs ?? CONNECT_FAILURE_COOLDOWN_MS;
    this.ensureConnectedCoalesced = createCoalescedStarter(() => this.connect());
  }

  async start(pi: ExtensionAPI): Promise<boolean> {
    this.startupMode = "eager";
    try {
      await this.ensureConnected();
      if (this.shuttingDown) return false;
      const tools = await this.discoverAndRegisterTools(pi);
      if (this.shuttingDown) return false;
      await this.onSchemasDiscovered?.(tools);
      return true;
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      this.lastError = msg;
      console.error(`[lean-ctx MCP bridge] Failed to start: ${msg}`);
      return false;
    }
  }

  /** Register validated cached schemas without starting the MCP process. */
  registerCachedTools(pi: ExtensionAPI, tools: McpTool[]): void {
    this.startupMode = "lazy";
    this.registerTools(pi, tools);
  }

  private async ensureConnected(signal?: AbortSignal): Promise<void> {
    if (this.isConnected()) return;
    if (signal?.aborted) {
      throw new Error("lean-ctx MCP bridge connect aborted by host.");
    }

    if (this.connectBlockedUntil !== undefined) {
      if (Date.now() < this.connectBlockedUntil) {
        // Negative cache: without it a binary that spawns but never completes
        // `initialize` makes *every* read pay the full startup bound again.
        // The message deliberately avoids "timed out after" so `callTool`'s
        // timeout-retry path does not treat a cooldown as a hung tool call.
        throw new Error(
          "lean-ctx MCP bridge failed to connect; retrying after a short cooldown.",
        );
      }
      this.connectBlockedUntil = undefined;
    }

    try {
      await this.raceConnect(signal);
      if (!this.isConnected()) {
        throw new Error("lean-ctx MCP bridge failed to connect.");
      }
    } catch (error) {
      // A host abort is the caller's decision, not a bridge failure — it must
      // not poison the next call's connect attempt.
      if (!isAbortLikeError(error)) {
        this.lastError = error instanceof Error ? error.message : String(error);
        this.connectBlockedUntil = Date.now() + this.connectFailureCooldownMs;
      }
      throw error;
    }
  }

  /**
   * Run one coalesced connect attempt, bounded by `connectTimeoutMs` and by the
   * caller's abort signal. Losing the race abandons the shared start promise —
   * it stays single-flight, so a later caller joins the same attempt instead of
   * spawning a second server process.
   */
  private async raceConnect(signal?: AbortSignal): Promise<void> {
    const started = this.ensureConnectedCoalesced();
    // This caller may abandon `started`; keep its rejection handled either way.
    started.catch(() => undefined);

    const waits: Promise<void>[] = [started];

    let timer: ReturnType<typeof setTimeout> | undefined;
    if (this.connectTimeoutMs > 0) {
      waits.push(new Promise<never>((_, reject) => {
        timer = setTimeout(() => {
          reject(new Error(
            `lean-ctx MCP bridge failed to connect within ${Math.round(this.connectTimeoutMs / 1000)}s.`,
          ));
        }, this.connectTimeoutMs);
        (timer as { unref?: () => void }).unref?.();
      }));
    }

    let onAbort: (() => void) | undefined;
    if (signal) {
      waits.push(new Promise<never>((_, reject) => {
        onAbort = () => {
          reject(new Error("lean-ctx MCP bridge connect aborted by host."));
        };
        signal.addEventListener("abort", onAbort, { once: true });
      }));
    }

    try {
      await Promise.race(waits);
    } finally {
      if (timer) clearTimeout(timer);
      if (onAbort && signal) signal.removeEventListener("abort", onAbort);
    }
  }

  /**
   * Drop the live client/transport pair, detaching the transport callbacks
   * first so a deliberate close cannot schedule a reconnect.
   *
   * Every path that replaces the pair must go through here: overwriting
   * `this.transport` without closing the previous one orphans a `lean-ctx`
   * child process for the rest of the session (the stray-server failure class
   * that corrupts dashboard stats).
   */
  private async closeActiveConnection(): Promise<void> {
    const client = this.client;
    const transport = this.transport;
    this.client = null;
    this.transport = null;
    this.connected = false;
    if (!client && !transport) return;
    if (transport) {
      transport.onclose = undefined;
      transport.onerror = undefined;
    }
    try {
      if (client) await client.close();
      else await transport?.close();
    } catch {
      // best-effort cleanup
    }
  }

  private async connect(): Promise<void> {
    if (this.shuttingDown) {
      throw new Error("lean-ctx MCP bridge is shutting down.");
    }

    // Reap any previous pair before establishing a new one.
    await this.closeActiveConnection();

    if (this.hooks.connect || this.hooks.callTool) {
      await this.hooks.connect?.();
      if (this.shuttingDown) {
        throw new Error("lean-ctx MCP bridge is shutting down.");
      }
      this.connected = true;
      this.reconnectAttempts = 0;
      this.lastError = undefined;
      this.connectBlockedUntil = undefined;
      return;
    }

    // Held in locals so the callbacks below always describe *this* attempt,
    // even if another one replaces `this.transport` while `connect()` awaits.
    const transport = new StdioClientTransport({
      command: this.binary,
      args: [],
      // config.json `env` (lowest) < process env < the forced compress flag.
      env: { ...this.extraEnv, ...process.env, LEAN_CTX_COMPRESS: "1" },
      stderr: "pipe",
    });

    const client = new Client({
      name: "pi-lean-ctx",
      version: "2.0.0",
    });

    transport.onclose = () => {
      // A superseded transport closing says nothing about the live one.
      if (this.transport !== transport) return;
      this.connected = false;
      this.lastError = "MCP transport closed";
      if (!this.shuttingDown) this.scheduleReconnect();
    };

    transport.onerror = (err) => {
      if (this.transport !== transport) return;
      this.lastError = err.message;
      console.error(`[lean-ctx MCP bridge] Transport error: ${err.message}`);
    };

    this.transport = transport;
    this.client = client;

    await client.connect(transport);
    if (this.shuttingDown) {
      await client.close().catch(() => undefined);
      throw new Error("lean-ctx MCP bridge is shutting down.");
    }
    this.connected = true;
    this.reconnectAttempts = 0;
    this.lastError = undefined;
    this.connectBlockedUntil = undefined;
  }

  private scheduleReconnect(): void {
    if (this.shuttingDown) return;
    if (this.reconnectTimer) return;
    if (this.reconnectAttempts >= MAX_RECONNECT_ATTEMPTS) {
      this.lastError = `Max reconnect attempts (${MAX_RECONNECT_ATTEMPTS}) reached.`;
      console.error(
        `[lean-ctx MCP bridge] Max reconnect attempts (${MAX_RECONNECT_ATTEMPTS}) reached. MCP tools unavailable.`,
      );
      return;
    }

    this.reconnectAttempts++;
    const delay = RECONNECT_DELAY_MS * this.reconnectAttempts;

    this.reconnectTimer = setTimeout(async () => {
      this.reconnectTimer = undefined;
      if (this.shuttingDown) return;
      // A lazy `ensureConnected()` may have reconnected while this timer was
      // pending. Re-entering `connect()` here used to spawn a second server and
      // silently drop the first one, leaving an orphaned `lean-ctx` child for
      // the rest of the session.
      if (this.isConnected()) {
        this.reconnectAttempts = 0;
        return;
      }
      try {
        // Single-flight and bounded, but deliberately not negative-cached: the
        // reconnect backoff is the retry schedule here.
        await this.raceConnect();
        if (!this.isConnected()) {
          throw new Error("lean-ctx MCP bridge failed to connect.");
        }
        if (!this.shuttingDown) console.error("[lean-ctx MCP bridge] Reconnected successfully");
      } catch (error) {
        this.lastError = error instanceof Error ? error.message : String(error);
        this.scheduleReconnect();
      }
    }, delay);
    (this.reconnectTimer as { unref?: () => void }).unref?.();
  }

  private async forceReconnect(): Promise<void> {
    if (this.shuttingDown) return;
    this.connected = false;
    if (this.hooks.close) {
      try {
        await this.hooks.close();
      } catch {
        // best-effort cleanup
      }
    }
    await this.closeActiveConnection();
    await this.connect();
  }

  private async discoverAndRegisterTools(pi: ExtensionAPI): Promise<McpTool[]> {
    if (this.shuttingDown) return [];
    const tools = await this.listTools();
    if (this.shuttingDown) return [];
    this.registerTools(pi, tools);
    return tools;
  }

  private async listTools(): Promise<McpTool[]> {
    if (this.hooks.listTools) return this.hooks.listTools();
    if (!this.client) return [];
    const result = await this.client.listTools();
    return (result.tools ?? []) as McpTool[];
  }

  private registerTools(pi: ExtensionAPI, tools: McpTool[]): void {
    const { toRegister, disabled } = selectBridgeTools(
      tools,
      this.policy.localTools,
      this.policy.disabledTools,
    );
    this.disabledToolNames.push(...disabled);
    for (const tool of toRegister) {
      this.registerMcpTool(pi, tool);
    }
  }

  private registerMcpTool(pi: ExtensionAPI, tool: McpTool): void {
    const bridge = this;
    // The prefix renames only the Pi-facing tool; the MCP call still targets
    // the real `tool.name` captured in the closure below.
    const exposedName = this.policy.toolPrefix
      ? `${this.policy.toolPrefix}${tool.name}`
      : tool.name;

    try {
      // Inside the try: a cached schema is attacker-shaped input in the sense
      // that it comes off disk, and a throw while converting it must not take
      // the whole extension down with it.
      const schema = this.jsonSchemaToTypebox(tool.inputSchema);
      pi.registerTool({
        name: exposedName,
        label: exposedName,
        description: tool.description ?? `lean-ctx MCP tool: ${tool.name}`,
        promptSnippet: tool.description ?? tool.name,
        parameters: schema,
        async execute(_toolCallId, params, signal, _onUpdate, _ctx) {
          const result = await bridge.callTool(
            tool.name,
            params as Record<string, unknown>,
            signal,
          );
          // Pi's AgentToolResult requires a `details` field; MCP tool output has none.
          return { ...result, details: undefined };
        },
      });
      this.registeredTools.push(exposedName);
    } catch (err) {
      // Usually: another extension (e.g. magic-context) already owns this name.
      // Also covers a schema that fails conversion. Skip it and keep going so
      // the whole agent doesn't crash on load (#359). Set a prefix
      // (LEAN_CTX_PI_TOOL_PREFIX) or disable the tool to resolve cleanly.
      const msg = err instanceof Error ? err.message : String(err);
      this.skippedTools.push(exposedName);
      console.error(
        `[lean-ctx MCP bridge] Skipped tool "${exposedName}" — already registered by another extension, or its schema is unusable? (${msg}). `
          + "Set LEAN_CTX_PI_TOOL_PREFIX or add it to LEAN_CTX_PI_DISABLE_TOOLS to silence this.",
      );
    }
  }

  async callTool(
    name: string,
    args: Record<string, unknown>,
    signal?: AbortSignal,
  ): Promise<{ content: Array<{ type: "text"; text: string }> }> {
    if (signal?.aborted) {
      throw new Error(`lean-ctx MCP tool "${name}" interrupted by host.`);
    }

    try {
      await this.ensureConnected(signal);
      const result = await this.callToolWithTimeout(name, args, signal);
      this.lastError = undefined;
      return this.toTextBlocks(result);
    } catch (error) {
      if (isHostToolRejection(error) || isAbortLikeError(error)) {
        throw new Error(`lean-ctx MCP tool "${name}" interrupted by host.`);
      }

      if (this.isTimeoutError(error) && isRetrySafeTool(name)) {
        this.lastRetry = {
          toolName: name,
          reason: "timeout",
          retried: true,
          timestamp: new Date().toISOString(),
        };
        await this.forceReconnect();
        const retried = await this.callToolWithTimeout(name, args, signal);
        this.lastError = undefined;
        return this.toTextBlocks(retried);
      }

      this.lastError = error instanceof Error ? error.message : String(error);
      throw error;
    }
  }

  private async callToolWithTimeout(
    name: string,
    args: Record<string, unknown>,
    signal?: AbortSignal,
  ): Promise<McpCallResult> {
    if (signal?.aborted) {
      throw new Error(`lean-ctx MCP tool "${name}" interrupted by host.`);
    }

    const call = this.hooks.callTool
      ? this.hooks.callTool(name, args, signal)
      : this.client?.callTool({ name, arguments: args }, undefined, { signal });
    if (!call) {
      throw new Error(`lean-ctx MCP bridge not connected. Tool "${name}" unavailable.`);
    }

    let timer: ReturnType<typeof setTimeout> | undefined;
    const timeout = new Promise<never>((_, reject) => {
      timer = setTimeout(() => {
        this.lastHungTool = name;
        reject(
          new Error(
            `lean-ctx MCP tool "${name}" timed out after ${Math.round(TOOL_CALL_TIMEOUT_MS / 1000)}s.`,
          ),
        );
      }, TOOL_CALL_TIMEOUT_MS);
    });

    const promises: Promise<McpCallResult>[] = [call, timeout];

    if (signal) {
      let onAbort: (() => void) | undefined;
      const abortPromise = new Promise<never>((_, reject) => {
        onAbort = () => {
          reject(new Error(`lean-ctx MCP tool "${name}" interrupted by host.`));
        };
        signal.addEventListener("abort", onAbort, { once: true });
      });
      promises.push(abortPromise);

      try {
        return await Promise.race(promises);
      } finally {
        if (timer) clearTimeout(timer);
        if (onAbort) signal.removeEventListener("abort", onAbort);
      }
    }

    try {
      return await Promise.race(promises);
    } finally {
      if (timer) clearTimeout(timer);
    }
  }

  private isTimeoutError(error: unknown): boolean {
    return error instanceof Error && error.message.includes("timed out after");
  }

  private toTextBlocks(
    result: McpCallResult,
  ): { content: Array<{ type: "text"; text: string }> } {
    const content = (
      result.content as Array<{ type: string; text?: string }>
    ).map((block) => ({
      type: "text" as const,
      text: block.text ?? "",
    }));

    return { content };
  }

  private jsonSchemaToTypebox(
    schema?: Record<string, unknown>,
  ): ReturnType<typeof Type.Object> {
    if (!schema || !schema.properties) {
      return Type.Object({});
    }

    const properties = schema.properties as Record<
      string,
      Record<string, unknown>
    >;
    const required = new Set(
      (schema.required as string[] | undefined) ?? [],
    );
    const fields: Record<string, TSchema> = {};

    for (const [key, prop] of Object.entries(properties)) {
      const field = propToTypebox(prop);
      fields[key] = required.has(key) ? field : Type.Optional(field);
    }

    return Type.Object(fields);
  }



  /** True when the MCP client is connected and able to serve tool calls. */
  isConnected(): boolean {
    return this.connected && (
      this.client !== null
      || this.hooks.callTool !== undefined
    );
  }

  getStatus(): McpBridgeStatus {
    return {
      mode: "embedded",
      connected: this.connected,
      toolCount: this.registeredTools.length,
      toolNames: [...this.registeredTools],
      skippedTools: [...this.skippedTools],
      disabledTools: [...this.disabledToolNames],
      toolPrefix: this.policy.toolPrefix,
      reconnectAttempts: this.reconnectAttempts,
      lastError: this.lastError,
      lastHungTool: this.lastHungTool,
      lastRetry: this.lastRetry,
      startupMode: this.startupMode,
    };
  }

  async shutdown(): Promise<void> {
    this.shuttingDown = true;
    this.reconnectAttempts = MAX_RECONNECT_ATTEMPTS;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = undefined;
    }
    try {
      if (this.hooks.close) await this.hooks.close();
      else await this.client?.close();
    } catch {
      // best-effort cleanup
    }
    this.client = null;
    this.transport = null;
    this.connected = false;
  }
}
