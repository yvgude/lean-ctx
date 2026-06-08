import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { type TSchema, Type } from "typebox";
import type { McpBridgeRetryState, McpBridgeStatus } from "./types.js";

/** Result shape returned by the MCP client's `callTool`. */
type McpCallResult = Awaited<ReturnType<Client["callTool"]>>;

const CLI_OVERRIDE_TOOLS = new Set([
  "ctx_read",
  "ctx_multi_read",
  "ctx_shell",
  "ctx_search",
  "ctx_tree",
]);

// No additional prefix filter — CLI_OVERRIDE_TOOLS covers exactly the tools we overwrite

const MAX_RECONNECT_ATTEMPTS = 3;
const RECONNECT_DELAY_MS = 2000;
const TOOL_CALL_TIMEOUT_MS = 120000;

type McpTool = {
  name: string;
  description?: string;
  inputSchema?: Record<string, unknown>;
};

/**
 * How the bridge should expose discovered MCP tools, so lean-ctx can coexist
 * with other Pi extensions (AFT, magic-context) instead of crashing on a name
 * collision (issue #359).
 */
export type BridgeToolPolicy = {
  /** Lower-cased tool names the bridge must not register at all. */
  disabledTools: Set<string>;
  /** Optional prefix applied to the Pi-facing tool name (not the MCP call). */
  toolPrefix?: string;
};

const DEFAULT_TOOL_POLICY: BridgeToolPolicy = { disabledTools: new Set() };

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
  private lastError: string | undefined;
  private lastHungTool: string | undefined;
  private lastRetry: McpBridgeRetryState | undefined;

  constructor(
    binary: string,
    extraEnv: Record<string, string> = {},
    policy: BridgeToolPolicy = DEFAULT_TOOL_POLICY,
  ) {
    this.binary = binary;
    this.extraEnv = extraEnv;
    this.policy = policy;
  }

  async start(pi: ExtensionAPI): Promise<void> {
    try {
      await this.connect();
      await this.discoverAndRegisterTools(pi);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      this.lastError = msg;
      console.error(`[lean-ctx MCP bridge] Failed to start: ${msg}`);
    }
  }

  private async connect(): Promise<void> {
    this.transport = new StdioClientTransport({
      command: this.binary,
      args: [],
      // config.json `env` (lowest) < process env < the forced compress flag.
      env: { ...this.extraEnv, ...process.env, LEAN_CTX_COMPRESS: "1" },
      stderr: "pipe",
    });

    this.client = new Client({
      name: "pi-lean-ctx",
      version: "2.0.0",
    });

    this.transport.onclose = () => {
      this.connected = false;
      this.lastError = "MCP transport closed";
      this.scheduleReconnect();
    };

    this.transport.onerror = (err) => {
      this.lastError = err.message;
      console.error(`[lean-ctx MCP bridge] Transport error: ${err.message}`);
    };

    await this.client.connect(this.transport);
    this.connected = true;
    this.reconnectAttempts = 0;
    this.lastError = undefined;
  }

  private scheduleReconnect(): void {
    if (this.reconnectAttempts >= MAX_RECONNECT_ATTEMPTS) {
      this.lastError = `Max reconnect attempts (${MAX_RECONNECT_ATTEMPTS}) reached.`;
      console.error(
        `[lean-ctx MCP bridge] Max reconnect attempts (${MAX_RECONNECT_ATTEMPTS}) reached. MCP tools unavailable.`,
      );
      return;
    }

    this.reconnectAttempts++;
    const delay = RECONNECT_DELAY_MS * this.reconnectAttempts;

    setTimeout(async () => {
      try {
        await this.connect();
        console.error("[lean-ctx MCP bridge] Reconnected successfully");
      } catch (error) {
        this.lastError = error instanceof Error ? error.message : String(error);
        this.scheduleReconnect();
      }
    }, delay);
  }

  private async forceReconnect(): Promise<void> {
    this.connected = false;
    try {
      await this.client?.close();
    } catch {
      // best-effort cleanup
    }
    this.client = null;
    this.transport = null;
    await this.connect();
  }

  private async discoverAndRegisterTools(pi: ExtensionAPI): Promise<void> {
    if (!this.client) return;

    const result = await this.client.listTools();
    const tools = (result.tools ?? []) as McpTool[];

    for (const tool of tools) {
      if (CLI_OVERRIDE_TOOLS.has(tool.name)) continue;
      if (this.policy.disabledTools.has(tool.name.toLowerCase())) {
        this.disabledToolNames.push(tool.name);
        continue;
      }
      this.registerMcpTool(pi, tool);
    }
  }

  private registerMcpTool(pi: ExtensionAPI, tool: McpTool): void {
    const bridge = this;
    const schema = this.jsonSchemaToTypebox(tool.inputSchema);
    // The prefix renames only the Pi-facing tool; the MCP call still targets
    // the real `tool.name` captured in the closure below.
    const exposedName = this.policy.toolPrefix
      ? `${this.policy.toolPrefix}${tool.name}`
      : tool.name;

    try {
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
      // Another extension (e.g. magic-context) already owns this name. Skip it
      // and keep going so the whole agent doesn't crash on load (#359). Set a
      // prefix (LEAN_CTX_PI_TOOL_PREFIX) or disable the tool to resolve cleanly.
      const msg = err instanceof Error ? err.message : String(err);
      this.skippedTools.push(exposedName);
      console.error(
        `[lean-ctx MCP bridge] Skipped tool "${exposedName}" — already registered by another extension? (${msg}). `
          + "Set LEAN_CTX_PI_TOOL_PREFIX or add it to LEAN_CTX_PI_DISABLE_TOOLS to silence this.",
      );
    }
  }

  async callTool(
    name: string,
    args: Record<string, unknown>,
    signal?: AbortSignal,
  ): Promise<{ content: Array<{ type: "text"; text: string }> }> {
    if (!this.client || !this.connected) {
      throw new Error(
        `lean-ctx MCP bridge not connected. Tool "${name}" unavailable.`,
      );
    }

    if (signal?.aborted) {
      throw new Error(`lean-ctx MCP tool "${name}" interrupted by host.`);
    }

    try {
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
    const call = this.client?.callTool({ name, arguments: args });
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
      const desc = (prop.description as string) ?? undefined;
      const jsonType = prop.type as string | undefined;

      let field: TSchema;
      switch (jsonType) {
        case "number":
        case "integer":
          field = Type.Number({ description: desc });
          break;
        case "boolean":
          field = Type.Boolean({ description: desc });
          break;
        case "array":
          field = Type.Array(Type.Unknown(), { description: desc });
          break;
        case "object":
          field = Type.Record(Type.String(), Type.Unknown(), {
            description: desc,
          });
          break;
        default:
          field = Type.String({ description: desc });
          break;
      }

      fields[key] = required.has(key)
        ? field
        : Type.Optional(field);
    }

    return Type.Object(fields);
  }

  /** True when the MCP client is connected and able to serve tool calls. */
  isConnected(): boolean {
    return this.connected && this.client !== null;
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
    };
  }

  async shutdown(): Promise<void> {
    this.reconnectAttempts = MAX_RECONNECT_ATTEMPTS;
    try {
      await this.client?.close();
    } catch {
      // best-effort cleanup
    }
    this.client = null;
    this.transport = null;
    this.connected = false;
  }
}
