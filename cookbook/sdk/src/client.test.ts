import { describe, expect, it } from "vitest";

import { LeanCtxClient } from "./client.js";

describe("LeanCtxClient", () => {
  it("normalizes trailing slash", () => {
    const c = new LeanCtxClient({ baseUrl: "http://127.0.0.1:8080/" });
    expect(c.baseUrl).toBe("http://127.0.0.1:8080");
  });

  it("posts tool calls to /v1/tools/call", async () => {
    const calls: Array<{ url: string; init?: RequestInit }> = [];
    const fetchImpl: typeof fetch = async (url, init) => {
      calls.push({ url: String(url), init });
      return new Response(
        JSON.stringify({ result: { content: [{ type: "text", text: "ok" }] } }),
        { status: 200, headers: { "content-type": "application/json" } }
      );
    };

    const c = new LeanCtxClient({
      baseUrl: "http://127.0.0.1:8080",
      fetchImpl,
    });
    const r = await c.callToolResult("ctx_read", { path: "README.md" });

    expect(calls).toHaveLength(1);
    expect(calls[0]?.url).toBe("http://127.0.0.1:8080/v1/tools/call");
    expect(calls[0]?.init?.method).toBe("POST");
    expect(r).toEqual({ content: [{ type: "text", text: "ok" }] });
  });
});
