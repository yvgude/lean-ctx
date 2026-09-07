import { describe, expect, it, vi } from "vitest";

import {
  McpBridge,
  type McpCallResult,
  selectBridgeTools,
  type McpTool,
} from "../extensions/mcp-bridge.js";

const tool = (name: string): McpTool => ({ name });

// The exact set index.ts owns locally (CLI-first replacements). In production
// this set is derived from the actual `registerTool` calls and handed to the
// bridge, so it can never drift; here it is the reference inventory the bridge
// must defer to.
const LOCAL_TOOLS = new Set([
  "ctx_read",
  "ctx_shell",
  "ctx_ls",
  "ctx_find",
  "ctx_grep",
  "lean_ctx",
]);

describe("selectBridgeTools", () => {
  it("exposes ctx_search/ctx_tree/ctx_multi_read — the tools #409 dropped", () => {
    const mcpTools = [
      "ctx_read",
      "ctx_shell",
      "ctx_search",
      "ctx_tree",
      "ctx_multi_read",
      "ctx_overview",
    ].map(tool);

    const { toRegister } = selectBridgeTools(mcpTools, LOCAL_TOOLS, new Set());
    const names = toRegister.map((t) => t.name);

    expect(names).toContain("ctx_search");
    expect(names).toContain("ctx_tree");
    expect(names).toContain("ctx_multi_read");
    expect(names).toContain("ctx_overview");
    // The two that DO have a local replacement must stay suppressed.
    expect(names).not.toContain("ctx_read");
    expect(names).not.toContain("ctx_shell");
  });

  it("skips a tool if and only if it has a local replacement (the #409 invariant)", () => {
    const mcpTools = [
      "ctx_read",
      "ctx_shell",
      "ctx_search",
      "ctx_tree",
      "ctx_multi_read",
      "ctx_overview",
      "ctx_session",
    ].map(tool);

    const { toRegister } = selectBridgeTools(mcpTools, LOCAL_TOOLS, new Set());
    const registered = new Set(toRegister.map((t) => t.name));

    // Suppression is allowed ONLY when a local replacement exists. This is the
    // exact property that broke in #409 and must hold forever.
    for (const t of mcpTools) {
      const skipped = !registered.has(t.name);
      expect(skipped).toBe(LOCAL_TOOLS.has(t.name));
    }
  });

  it("routes disabledTools to disabled and never registers them (#359)", () => {
    const mcpTools = [tool("ctx_search"), tool("ctx_expand")];
    const { toRegister, disabled } = selectBridgeTools(
      mcpTools,
      new Set(),
      new Set(["ctx_expand"]),
    );

    expect(disabled).toEqual(["ctx_expand"]);
    expect(toRegister.map((t) => t.name)).toEqual(["ctx_search"]);
  });

  it("matches disabledTools case-insensitively", () => {
    const { toRegister, disabled } = selectBridgeTools(
      [tool("Ctx_Expand")],
      new Set(),
      new Set(["ctx_expand"]),
    );

    expect(disabled).toEqual(["Ctx_Expand"]);
    expect(toRegister).toHaveLength(0);
  });

  it("registers everything when nothing is local or disabled", () => {
    const mcpTools = ["ctx_search", "ctx_tree", "ctx_multi_read"].map(tool);
    const { toRegister, disabled } = selectBridgeTools(
      mcpTools,
      new Set(),
      new Set(),
    );

    expect(toRegister).toHaveLength(3);
    expect(disabled).toHaveLength(0);
  });
});

import { propToTypebox } from "../extensions/mcp-bridge.js";
import { Type, IsUnion, IsLiteral, IsArray, IsObject, IsString, IsNumber, IsBoolean, IsOptional } from "typebox";

describe("propToTypebox", () => {
  it("converts string enum to Type.Union of Literals", () => {
    const schema = {
      type: "string",
      enum: ["set_line", "replace_lines", "insert_after", "delete"],
      description: "The operation to perform",
    };
    const result = propToTypebox(schema);
    expect(IsUnion(result)).toBe(true);
    // @ts-expect-error — anyOf is Union's internal shape
    const variants = result.anyOf ?? [];
    expect(variants).toHaveLength(4);
    expect(variants.map((v: { const: string }) => v.const)).toEqual([
      "set_line", "replace_lines", "insert_after", "delete",
    ]);
    // @ts-expect-error — TypeBox schema internals
    expect(result.description).toBe("The operation to perform");
  });

  it("converts single-value enum to Literal", () => {
    const result = propToTypebox({ type: "string", enum: ["only"] });
    expect(IsLiteral(result)).toBe(true);
  });

  it("converts array with items.type=object recursively", () => {
    const schema = {
      type: "array",
      items: {
        type: "object",
        properties: {
          oldText: { type: "string", description: "Text to find" },
          newText: { type: "string", description: "Replacement" },
        },
        required: ["oldText", "newText"],
      },
      description: "List of edits",
    };
    const result = propToTypebox(schema);
    expect(IsArray(result)).toBe(true);
    // The items schema should be an Object, not Unknown
    // @ts-expect-error — access items property on Array schema
    const itemsSchema = result.items;
    expect(IsObject(itemsSchema)).toBe(true);
    expect(itemsSchema.properties.oldText).toBeDefined();
    expect(itemsSchema.properties.newText).toBeDefined();
  });

  it("converts nested object with properties recursively", () => {
    const schema = {
      type: "object",
      properties: {
        name: { type: "string", description: "Name" },
        count: { type: "integer", description: "Count" },
        nested: {
          type: "object",
          properties: {
            deep: { type: "boolean" },
          },
        },
      },
      required: ["name"],
    };
    const result = propToTypebox(schema);
    expect(IsObject(result)).toBe(true);
    // name should be required (not Optional), count should be Optional
    // @ts-expect-error — TypeBox schema internals
    expect(IsString(result.properties.name)).toBe(true);
    // @ts-expect-error — TypeBox schema internals
    expect(IsOptional(result.properties.count)).toBe(true);
    // nested object should be converted, not Record<string, unknown>
    // @ts-expect-error — TypeBox schema internals
    const nestedProp = result.properties.nested;
    // TypeBox Optional wraps the schema — the inner type still carries
    // its `properties` on the same object.
    expect(nestedProp).toBeDefined();
    expect(nestedProp.properties?.deep).toBeDefined();
  });

  it("falls back to Type.Record for object without properties", () => {
    const result = propToTypebox({
      type: "object",
      description: "Freeform data",
    });
    // @ts-expect-error — TypeBox schema internals
    expect(result.description).toBe("Freeform data");
  });

  it("converts plain types correctly", () => {
    expect(IsNumber(propToTypebox({ type: "number" }))).toBe(true);
    expect(IsNumber(propToTypebox({ type: "integer" }))).toBe(true);
    expect(IsBoolean(propToTypebox({ type: "boolean" }))).toBe(true);
    expect(IsString(propToTypebox({ type: "string" }))).toBe(true);
  });

  it("handles array without items (fallback to Unknown)", () => {
    const result = propToTypebox({ type: "array" });
    expect(IsArray(result)).toBe(true);
  });
});

function fakePi(registrations: unknown[]) {
  return {
    registerTool(definition: unknown) {
      registrations.push(definition);
    },
  } as never;
}

const bridgePolicy = {
  disabledTools: new Set<string>(),
  localTools: new Set<string>(),
};

const okResult = (): McpCallResult =>
  ({ content: [{ type: "text", text: "ok" }] } as McpCallResult);

/**
 * The private state the reconnect/transport-lifecycle tests have to drive.
 * `transport.onclose` firing is what arms the reconnect timer in production;
 * the hook seams never build a real transport, so the tests reach for it here.
 */
type BridgeInternals = {
  connected: boolean;
  client: { close(): Promise<void> } | null;
  transport: { onclose?: () => void; onerror?: (error: Error) => void } | null;
  scheduleReconnect(): void;
};

describe("McpBridge startup modes", () => {
  it("registers cached tools without starting MCP until the first call", async () => {
    let connections = 0;
    const registrations: unknown[] = [];
    const bridge = new McpBridge("test", {}, bridgePolicy, {
      hooks: {
        connect: async () => {
          connections++;
        },
        callTool: async () => ({
          content: [{ type: "text", text: "ok" }],
        } as McpCallResult),
      },
    });

    bridge.registerCachedTools(fakePi(registrations), [tool("ctx_search")]);
    expect(registrations).toHaveLength(1);
    expect(connections).toBe(0);

    await bridge.callTool("ctx_search", {});
    expect(connections).toBe(1);
  });

  it("preserves Pi's direct tool result contract on a lazy call", async () => {
    const registrations: unknown[] = [];
    const bridge = new McpBridge("test", {}, bridgePolicy, {
      hooks: {
        connect: async () => undefined,
        callTool: async () => ({
          content: [{ type: "text", text: "ok" }],
        } as McpCallResult),
      },
    });
    bridge.registerCachedTools(fakePi(registrations), [tool("ctx_search")]);

    const definition = registrations[0] as {
      execute: (
        toolCallId: string,
        params: Record<string, unknown>,
        signal?: AbortSignal,
      ) => Promise<unknown>;
    };
    const result = await definition.execute("call-1", {}, new AbortController().signal);

    expect(result).toEqual({
      content: [{ type: "text", text: "ok" }],
      details: undefined,
    });
  });

  it("coalesces concurrent first calls behind one connection", async () => {
    let connections = 0;
    let calls = 0;
    let release!: () => void;
    const connectionReady = new Promise<void>((resolve) => {
      release = resolve;
    });
    const bridge = new McpBridge("test", {}, bridgePolicy, {
      hooks: {
        connect: async () => {
          connections++;
          await connectionReady;
        },
        callTool: async () => {
          calls++;
          return { content: [{ type: "text", text: "ok" }] } as McpCallResult;
        },
      },
    });
    bridge.registerCachedTools(fakePi([]), [tool("ctx_search")]);

    const first = bridge.callTool("ctx_search", {});
    const second = bridge.callTool("ctx_search", {});
    await Promise.resolve();
    expect(connections).toBe(1);
    release();
    await Promise.all([first, second]);
    expect(calls).toBe(2);
  });

  it("keeps cache-miss startup eager and registers discovered tools", async () => {
    let connections = 0;
    let discoveries = 0;
    const registrations: unknown[] = [];
    const bridge = new McpBridge("test", {}, bridgePolicy, {
      hooks: {
        connect: async () => {
          connections++;
        },
        listTools: async () => {
          discoveries++;
          return [tool("ctx_search")];
        },
        callTool: async () => ({
          content: [{ type: "text", text: "ok" }],
        } as McpCallResult),
      },
    });

    await bridge.start(fakePi(registrations));
    expect(connections).toBe(1);
    expect(discoveries).toBe(1);
    expect(registrations).toHaveLength(1);
  });

  it("keeps the reconnect timer from racing a lazy connect (#1446 F1)", async () => {
    vi.useFakeTimers();
    try {
      let connections = 0;
      const bridge = new McpBridge("test", {}, bridgePolicy, {
        hooks: {
          connect: async () => {
            connections++;
          },
          callTool: async () => okResult(),
        },
      });
      bridge.registerCachedTools(fakePi([]), [tool("ctx_search")]);
      const internals = bridge as unknown as BridgeInternals;

      // The server died: `transport.onclose` marks the bridge down and arms the
      // reconnect timer.
      internals.connected = false;
      internals.scheduleReconnect();

      // A lazy call lands before the timer fires and reconnects first.
      await bridge.callTool("ctx_search", {});
      expect(connections).toBe(1);

      // The timer must observe the live connection and stand down. Before the
      // fix it called connect() again, spawning a second lean-ctx child and
      // dropping the first one — an orphan for the rest of the session.
      await vi.advanceTimersByTimeAsync(10_000);
      expect(connections).toBe(1);
      expect(bridge.getStatus().reconnectAttempts).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("closes the previous transport before connecting again (#1446 F1)", async () => {
    let closed = 0;
    const bridge = new McpBridge("test", {}, bridgePolicy, {
      hooks: {
        connect: async () => undefined,
        callTool: async () => okResult(),
      },
    });
    bridge.registerCachedTools(fakePi([]), [tool("ctx_search")]);

    const internals = bridge as unknown as BridgeInternals;
    const staleTransport = {
      onclose: (): void => {
        throw new Error("a deliberate close must not schedule a reconnect");
      },
      onerror: (): void => undefined,
      close: async (): Promise<void> => undefined,
    };
    internals.transport = staleTransport;
    internals.client = {
      close: async (): Promise<void> => {
        closed++;
      },
    };

    await bridge.callTool("ctx_search", {});

    expect(closed).toBe(1);
    expect(internals.client).toBeNull();
    expect(internals.transport).toBeNull();
    expect(staleTransport.onclose).toBeUndefined();
  });

  it("bounds a lazy connect and negative-caches the failure (#1446 F2)", async () => {
    let starts = 0;
    const bridge = new McpBridge("test", {}, bridgePolicy, {
      connectTimeoutMs: 20,
      connectFailureCooldownMs: 10_000,
      hooks: {
        // A binary that spawns but never completes `initialize`.
        connect: () => {
          starts++;
          return new Promise<void>(() => undefined);
        },
        callTool: async () => okResult(),
      },
    });
    bridge.registerCachedTools(fakePi([]), [tool("ctx_search")]);

    await expect(bridge.callTool("ctx_search", {}))
      .rejects.toThrow(/failed to connect within/);
    expect(starts).toBe(1);

    // Every later read short-circuits on the negative cache instead of paying
    // the bound again; only this branch produces the "cooldown" wording.
    await expect(bridge.callTool("ctx_search", {}))
      .rejects.toThrow(/retrying after a short cooldown/);
    expect(starts).toBe(1);
  });

  it("retries once the negative-cache window expires (#1446 F2)", async () => {
    let attempts = 0;
    const bridge = new McpBridge("test", {}, bridgePolicy, {
      connectTimeoutMs: 1000,
      connectFailureCooldownMs: 5,
      hooks: {
        connect: async () => {
          attempts++;
          if (attempts === 1) throw new Error("binary unavailable");
        },
        callTool: async () => okResult(),
      },
    });
    bridge.registerCachedTools(fakePi([]), [tool("ctx_search")]);

    await expect(bridge.callTool("ctx_search", {}))
      .rejects.toThrow("binary unavailable");

    await new Promise((resolve) => setTimeout(resolve, 25));

    expect(await bridge.callTool("ctx_search", {})).toEqual(okResult());
    expect(attempts).toBe(2);
  });

  it("aborts a pending lazy connect when the host cancels (#1446 F2)", async () => {
    const controller = new AbortController();
    const bridge = new McpBridge("test", {}, bridgePolicy, {
      connectTimeoutMs: 60_000,
      hooks: {
        connect: () => new Promise<void>(() => undefined),
        callTool: async () => okResult(),
      },
    });
    bridge.registerCachedTools(fakePi([]), [tool("ctx_search")]);

    const pending = bridge.callTool("ctx_search", {}, controller.signal);
    controller.abort();

    await expect(pending).rejects.toThrow(/interrupted by host/);
    // A host abort is the caller's decision, not a bridge failure: it must not
    // poison the next attempt with a cooldown.
    expect(bridge.getStatus().lastError).toBeUndefined();
  });

  it("contains a schema-conversion failure instead of crashing (#1446 F6)", () => {
    const errors = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      const registrations: unknown[] = [];
      const bridge = new McpBridge("test", {}, bridgePolicy, {
        hooks: { connect: async () => undefined, callTool: async () => okResult() },
      });
      const hostile = {
        name: "ctx_hostile",
        inputSchema: {
          type: "object",
          properties: {
            broken: {
              get type(): string {
                throw new Error("unconvertible cached schema");
              },
            },
          },
        },
      } as unknown as McpTool;

      expect(() => bridge.registerCachedTools(
        fakePi(registrations),
        [hostile, tool("ctx_search")],
      )).not.toThrow();

      expect((registrations as Array<{ name: string }>).map((r) => r.name))
        .toEqual(["ctx_search"]);
      expect(bridge.getStatus().skippedTools).toEqual(["ctx_hostile"]);
    } finally {
      errors.mockRestore();
    }
  });

  it("reports eager startup failure so the host can fall back to CLI tools", async () => {
    const bridge = new McpBridge("test", {}, bridgePolicy, {
      hooks: {
        connect: async () => {
          throw new Error("binary unavailable");
        },
      },
    });

    const started = await bridge.start(fakePi([]));

    expect(started).toBe(false);
    expect(bridge.isConnected()).toBe(false);
    expect(bridge.getStatus()).toMatchObject({
      connected: false,
      startupMode: "eager",
      lastError: "binary unavailable",
    });
  });
});
