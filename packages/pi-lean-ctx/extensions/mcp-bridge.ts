import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { Type } from "@sinclair/typebox";
import type { McpBridgeStatus, McpToolInterruption } from "./types.js";

const CLI_OVERRIDE_TOOLS = new Set([
  "ctx_read",
  "ctx_multi_read",
  "ctx_shell",
  "ctx_search",
  "ctx_tree",
]);

const MAX_RECONNECT_ATTEMPTS = 3;
const RECONNECT_DELAY_MS = 2000;

type McpTool = {
  name: string;
  description?: string;
  inputSchema?: Record<string, unknown>;
};

const CLIENT_NAME = "pi-lean-ctx";
const MAX_INTERRUPTION_HISTORY = 12;

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

export class McpBridge {
  private client: Client | null = null;
  private transport: StdioClientTransport | null = null;
  private registeredTools: string[] = [];
  private connected = false;
  private binary: string;
  private reconnectAttempts = 0;
  private lastError: string | undefined;
  private lastToolError: string | undefined;
  private lastCancellation: McpToolInterruption | undefined;
  private recentInterruptions: McpToolInterruption[] = [];

  constructor(binary: string) {
    this.binary = binary;
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
      stderr: "pipe",
    });

    this.client = new Client({
      name: CLIENT_NAME,
      version: "2.0.0",
    });

    this.transport.onclose = () => {
      this.connected = false;
      this.recordInterruption("bridge", "disconnected");
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

  private async discoverAndRegisterTools(pi: ExtensionAPI): Promise<void> {
    if (!this.client) return;

    const result = await this.client.listTools();
    const tools = (result.tools ?? []) as McpTool[];

    for (const tool of tools) {
      if (CLI_OVERRIDE_TOOLS.has(tool.name)) continue;
      this.registerMcpTool(pi, tool);
    }
  }

  private registerMcpTool(pi: ExtensionAPI, tool: McpTool): void {
    const bridge = this;
    const schema = this.jsonSchemaToTypebox(tool.inputSchema);

    pi.registerTool({
      name: tool.name,
      label: tool.name,
      description: tool.description ?? `lean-ctx MCP tool: ${tool.name}`,
      promptSnippet: tool.description ?? tool.name,
      parameters: schema,
      async execute(_toolCallId, params, signal) {
        return bridge.callTool(
          tool.name,
          params as Record<string, unknown>,
          signal,
        );
      },
    });

    this.registeredTools.push(tool.name);
  }

  async callTool(
    name: string,
    args: Record<string, unknown>,
    signal?: AbortSignal,
  ): Promise<{ content: Array<{ type: string; text: string }> }> {
    if (!this.client || !this.connected) {
      this.recordInterruption(name, "disconnected");
      throw new Error(
        `lean-ctx MCP bridge not connected. Tool "${name}" unavailable.`,
      );
    }

    if (signal?.aborted) {
      this.recordInterruption(name, "aborted");
      throw new Error(`lean-ctx MCP tool "${name}" interrupted by host.`);
    }

    const call = this.client.callTool({ name, arguments: args });
    const result = await this.withAbortSignal(call, name, signal);
    this.lastToolError = undefined;

    const content = (
      result.content as Array<{ type: string; text?: string }>
    ).map((block) => ({
      type: "text" as const,
      text: block.text ?? "",
    }));

    return { content };
  }

  private async withAbortSignal<T>(
    promise: Promise<T>,
    toolName: string,
    signal?: AbortSignal,
  ): Promise<T> {
    if (!signal) {
      return this.normalizeToolErrors(promise, toolName);
    }

    let onAbort: (() => void) | undefined;
    const abortPromise = new Promise<never>((_, reject) => {
      onAbort = () => {
        signal.removeEventListener("abort", onAbort);
        this.recordInterruption(toolName, "aborted");
        reject(new Error(`lean-ctx MCP tool "${toolName}" interrupted by host.`));
      };

      signal.addEventListener("abort", onAbort, { once: true });
    });

    try {
      return await this.normalizeToolErrors(
        Promise.race([promise, abortPromise]),
        toolName,
      );
    } finally {
      if (onAbort) {
        signal.removeEventListener("abort", onAbort);
      }
    }
  }

  private async normalizeToolErrors<T>(
    promise: Promise<T>,
    toolName: string,
  ): Promise<T> {
    try {
      return await promise;
    } catch (error) {
      if (isHostToolRejection(error)) {
        this.recordInterruption(toolName, "rejected");
        throw new Error(`lean-ctx MCP tool "${toolName}" interrupted by host.`);
      }

      if (isAbortLikeError(error)) {
        this.recordInterruption(toolName, "aborted");
        throw new Error(`lean-ctx MCP tool "${toolName}" interrupted by host.`);
      }

      this.lastToolError = error instanceof Error ? error.message : String(error);
      throw error;
    }
  }

  private recordInterruption(
    toolName: string,
    reason: McpToolInterruption["reason"],
  ): void {
    const event: McpToolInterruption = {
      clientName: CLIENT_NAME,
      toolName,
      reason,
      timestamp: new Date().toISOString(),
    };

    this.lastCancellation = event;
    this.recentInterruptions = [
      event,
      ...this.recentInterruptions,
    ].slice(0, MAX_INTERRUPTION_HISTORY);
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
    const fields: Record<string, ReturnType<typeof Type.String>> = {};

    for (const [key, prop] of Object.entries(properties)) {
      const desc = (prop.description as string) ?? undefined;
      const jsonType = prop.type as string | undefined;

      let field;
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

  getStatus(): McpBridgeStatus {
    return {
      mode: "embedded",
      connected: this.connected,
      toolCount: this.registeredTools.length,
      toolNames: [...this.registeredTools],
      reconnectAttempts: this.reconnectAttempts,
      lastError: this.lastError,
      lastToolError: this.lastToolError,
      lastCancellation: this.lastCancellation,
      recentInterruptions: [...this.recentInterruptions],
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
