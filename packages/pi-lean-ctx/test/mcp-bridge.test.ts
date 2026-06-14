import { describe, expect, it } from "vitest";

import { selectBridgeTools, type McpTool } from "../extensions/mcp-bridge.js";

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
