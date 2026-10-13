// SPDX-License-Identifier: Apache-2.0
import type { On } from "claude-code";
import { describe, expect, test } from "claude-code/testing";

import { drawPatch, drawRead, drawSearch, resultText } from "../hooks/render";

// Real lean-ctx results, captured from 3.11.4.
const PATCH = [
  "✓ probe.py: 1 replacement, 0 lines (4→4 tok)",
  "preimage: bytes=12, mtime_ms=1791886395342, blake3=1571da48",
  "",
  "evidence (diff, redacted, bounded):",
  "```diff",
  "--- probe.py",
  "+++ probe.py",
  "@@ -1,2 +1,2 @@",
  " a = 1",
  "-b = 2",
  "+b = 3",
  "```",
].join("\n");
const ANCHORED = [
  "hooks.json 3L [anchored: N:hh|line → edit via ctx_patch]",
  "1:a9c1|{",
  '2:9238|  "modules": ["./register.ts"]',
  "3:9f7f|}",
].join("\n");
const LINES = ["mod.rs 1353L lines:228-230", " 228|     // server-start", " 229|     \"cline\",", " 230|     \"roo\","].join("\n");
const SEARCH = [
  "2 matches in 1 files (scanned 1):",
  "[∈ enclosing symbol → ctx_search(action=symbol, handle=\"path#name@Lstart\")]",
  "redirect.rs:42 pub(super) fn compute_redirect() -> String { ∈compute_redirect@L42",
  "redirect.rs:93 \tRedirectKind::Grep => redirect_grep(tool_args),",
].join("\n");

describe("parsing", () => {
  test("ctx_patch: the ✓ line is the summary, the fence a diff with the +++ path", () => {
    const drawing = drawPatch(PATCH);
    expect(drawing?.summary).toBe("✓ probe.py: 1 replacement, 0 lines (4→4 tok)");
    const diff = drawing?.blocks.find((b) => b.kind === "diff");
    expect(diff).toMatchObject({ kind: "diff", path: "probe.py" });
    expect(diff && "source" in diff ? diff.source : "").toContain("@@ -1,2 +1,2 @@\n a = 1\n-b = 2\n+b = 3");
    expect(drawing?.blocks.some((b) => b.kind === "note" && b.text.includes("preimage"))).toBe(true);
    expect(drawPatch("✓ created a.py")).toBeNull();
  });

  test("ctx_read anchored: hashes leave the screen, numbering stays", () => {
    const drawing = drawRead(ANCHORED, { path: "/p/hooks.json", mode: "anchored" });
    expect(drawing?.summary).toStartWith("hooks.json 3L");
    expect(drawing?.blocks).toEqual([
      { kind: "code", source: '{\n  "modules": ["./register.ts"]\n}', path: "/p/hooks.json", startLine: 1 },
    ]);
  });

  test("ctx_read lines: indentation kept, a gap starts a new numbered block", () => {
    expect(drawRead(LINES, { path: "/p/mod.rs" })?.blocks[0]).toMatchObject({ startLine: 228, path: "/p/mod.rs" });
    const gapped = drawRead("a.rs 50L lines:1-1,9-9\n 1| x\n 9| y", undefined);
    expect(gapped?.blocks.map((b) => (b.kind === "code" ? b.startLine : b.kind))).toEqual([1, 9]);
  });

  test("ctx_read: several files, full mode with its trailer, and what stays plain", () => {
    const many = `${ANCHORED}\n---\n${LINES}\n---\nRead 2 files`;
    expect(drawRead(many, { paths: ["/p/hooks.json", "/p/mod.rs"] })?.summary).toBe("Read 2 files");
    const full = drawRead("import os\nx = 1\n[lean-ctx gateway: 1 secret(s) redacted]", { path: "/p/a.py", mode: "full" });
    expect(full?.summary).toBe("a.py 2L");
    expect(full?.blocks[1]).toEqual({ kind: "note", text: "[lean-ctx gateway: 1 secret(s) redacted]" });
    expect(drawRead("wg.py 503L\n deps a,b", { path: "wg.py", mode: "signatures" })).toBeNull();
    expect(drawRead("Cannot read file: /x.md: not found", { path: "/x.md", mode: "full" })).toBeNull();
    expect(drawRead("import os\nx = 1", { path: "a.py", mode: "map" })).toBeNull();
  });

  test("ctx_search: hits keep their location and symbol, hints stay notes", () => {
    const drawing = drawSearch(SEARCH);
    expect(drawing?.summary).toBe("2 matches in 1 files (scanned 1):");
    expect(drawing?.blocks).toEqual([
      { kind: "note", text: SEARCH.split("\n")[1] },
      {
        kind: "hit",
        location: "redirect.rs:42",
        path: "redirect.rs",
        line: "pub(super) fn compute_redirect() -> String {",
        symbol: "compute_redirect@L42",
      },
      {
        kind: "hit",
        location: "redirect.rs:93",
        path: "redirect.rs",
        line: "    RedirectKind::Grep => redirect_grep(tool_args),",
        symbol: undefined,
      },
    ]);
    expect(drawSearch("0 matches in 0 files (scanned 913)")).toBeNull();
  });

  test("resultText reads strings, content blocks and McpToolResult", () => {
    expect(resultText([{ type: "text", text: "a" }, { type: "image" }, { type: "text", text: "b" }])).toBe("a\nb");
    expect(resultText({ content: [{ type: "text", text: "c" }] })).toBe("c");
    expect(resultText(undefined)).toBe("");
  });
});

const row = (tool: string, output: unknown, extra: Record<string, unknown> = {}) => ({
  plugin: "lean-ctx",
  surface: "terminal" as const,
  component: "ToolUse" as const,
  props: {
    tool_use_id: `toolu_${tool}`,
    tool,
    input: {},
    isRunning: false,
    isErrored: false,
    isInterrupted: false,
    output,
    ...extra,
  },
});

// The engine beneath the plugin: draws a tool row as its plain result text.
function engine(on: On) {
  on("ui.render", { component: "ToolUse" }, ($, event) => {
    const { Text } = $.ui.resolve(event);
    return <Text>{`engine:${resultText(event.props.output)}`}</Text>;
  });
}

describe("rendering", () => {
  test("ctx_patch draws through the engine's diff renderer", async ($, on) => {
    engine(on);
    const ui = await $.ui.mount(row("mcp__lean-ctx__ctx_patch", [{ type: "text", text: PATCH }]));
    const diff = await ui.find({ type: "Code" });
    expect(diff?.props).toMatchObject({ format: "diff", path: "probe.py" });
    // The engine row keeps only the summary, not the whole result.
    expect(await ui.find({ type: "Text", text: "engine:✓ probe.py: 1 replacement, 0 lines (4→4 tok)" })).toBeDefined();
    await ui.unmount();
  });

  test("ctx_read draws numbered, highlighted code under either server spelling", async ($, on) => {
    engine(on);
    for (const tool of ["mcp__lean-ctx__ctx_read", "mcp__lean_ctx__ctx_read"]) {
      const ui = await $.ui.mount(
        row(tool, [{ type: "text", text: ANCHORED }], { input: { path: "/p/hooks.json", mode: "anchored" } }),
      );
      expect((await ui.find({ type: "Code" }))?.props).toMatchObject({ path: "/p/hooks.json", startLine: 1 });
      await ui.unmount();
    }
  });

  test("errors, running calls and other tools stay with the engine", async ($, on) => {
    engine(on);
    for (const extra of [{ isErrored: true }, { isRunning: true, output: undefined }]) {
      const ui = await $.ui.mount(row("mcp__lean-ctx__ctx_patch", [{ type: "text", text: PATCH }], extra));
      expect(await ui.find({ type: "Code" })).toBeUndefined();
      await ui.unmount();
    }
    const shell = await $.ui.mount(row("mcp__lean-ctx__ctx_shell", [{ type: "text", text: PATCH }]));
    expect(await shell.find({ type: "Code" })).toBeUndefined();
    await shell.unmount();
  });
});
