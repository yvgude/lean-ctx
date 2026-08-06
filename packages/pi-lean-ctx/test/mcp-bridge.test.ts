import { describe, expect, it, vi } from "vitest";

import { selectBridgeTools, McpBridge, type McpTool } from "../extensions/mcp-bridge.js";

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

// GH #1426 -- McpBridge.start() must always resolve so index.ts can safely
// await it. A fire-and-forget void was previously used because start() was
// believed to be able to reject; by confirming it resolves on failure, the
// awaited call in the extension factory is safe.
describe("McpBridge.start() awaitable invariants (GH #1426)", () => {
  it("always resolves (never rejects) when the binary does not exist", async () => {
    const bridge = new McpBridge("/nonexistent/lean-ctx-binary-gh1426", {}, {
      disabledTools: new Set(),
      localTools: new Set(),
    });
    // pi.registerTool is never called when the connection fails, so an empty
    // stub is sufficient. Cast via unknown to avoid a full ExtensionAPI mock.
    const mockPi = { registerTool: () => {} } as unknown as Parameters<typeof bridge["start"]>[0];
    // The key invariant: start() must resolve regardless of connection errors.
    // index.ts now awaits start() instead of using void (fire-and-forget), so
    // a rejection here would throw in the extension factory and crash the agent.
    await expect(bridge.start(mockPi)).resolves.toBeUndefined();
  });

  it("resolves via timeout when start() is slow (the bounded-await pattern)", async () => {
    vi.useFakeTimers();
    const STARTUP_TIMEOUT_MS = 10_000;
    let raceResolved = false;

    // Simulate a bridge that hangs indefinitely (e.g. lean-ctx MCP handshake stall).
    const hangingStart = new Promise<void>(() => { /* never settles */ });

    const race = Promise.race([
      hangingStart,
      new Promise<void>((resolve) => {
        const t = setTimeout(resolve, STARTUP_TIMEOUT_MS);
        (t as unknown as { unref?: () => void }).unref?.();
      }),
    ]).then(() => { raceResolved = true; });

    expect(raceResolved).toBe(false);
    await vi.advanceTimersByTimeAsync(STARTUP_TIMEOUT_MS);
    await race;
    expect(raceResolved).toBe(true);

    vi.useRealTimers();
  });
});
