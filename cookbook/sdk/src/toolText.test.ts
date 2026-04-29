import { describe, expect, it } from "vitest";

import { toolResultToText } from "./toolText.js";

describe("toolResultToText", () => {
  it("extracts direct text content", () => {
    const txt = toolResultToText({
      content: [{ type: "text", text: "hello\n" }],
    });
    expect(txt).toBe("hello\n");
  });

  it("extracts nested text content", () => {
    const txt = toolResultToText({
      content: [{ type: "text", text: { text: "nested\n" } }],
    });
    expect(txt).toBe("nested\n");
  });

  it("falls back to structured content", () => {
    const txt = toolResultToText({
      structuredContent: { ok: true, n: 1 },
    });
    expect(txt).toContain('"ok": true');
  });
});
