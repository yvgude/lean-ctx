import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { leanCtxMiddleware, withLeanCtx } from "../src/vercel-ai";
import { LeanCtxError } from "../src/errors";

let server: Server;
let baseUrl: string;

beforeAll(async () => {
  server = createServer((req, res) => {
    if (req.url !== "/v1/compress" || req.method !== "POST") {
      res.statusCode = 404;
      res.end();
      return;
    }
    let raw = "";
    req.on("data", (chunk) => (raw += chunk));
    req.on("end", () => {
      const body = JSON.parse(raw) as { messages: Array<Record<string, unknown>> };
      // Echo the prompt back with every string content truncated to 8 chars so
      // the test can assert the middleware actually rewired params.prompt.
      const out = body.messages.map((message) => {
        const rewritten = { ...message };
        if (typeof rewritten.content === "string") rewritten.content = rewritten.content.slice(0, 8);
        return rewritten;
      });
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ messages: out, stats: {} }));
    });
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address() as AddressInfo;
  baseUrl = `http://127.0.0.1:${port}`;
});

afterAll(() => {
  server.close();
});

describe("leanCtxMiddleware", () => {
  it("compresses the prompt in transformParams", async () => {
    const middleware = leanCtxMiddleware({ baseUrl, model: "gpt-4o" });
    const params = {
      prompt: [{ role: "user", content: "this is a very long prompt body" }],
      temperature: 0.2,
    };
    const out = await middleware.transformParams({ type: "generate", params });
    expect(out.prompt).toEqual([{ role: "user", content: "this is " }]);
    // Unrelated params are preserved untouched.
    expect(out.temperature).toBe(0.2);
  });

  it("passes non-array prompts through unchanged", async () => {
    const middleware = leanCtxMiddleware({ baseUrl });
    const params = { prompt: undefined, raw: "x" };
    const out = await middleware.transformParams({ type: "generate", params });
    expect(out).toBe(params);
  });

  it("falls back to the original prompt when the proxy is unreachable", async () => {
    const middleware = leanCtxMiddleware({ baseUrl: "http://127.0.0.1:1", timeoutMs: 200 });
    const params = { prompt: [{ role: "user", content: "x".repeat(40) }] };
    const out = await middleware.transformParams({ type: "generate", params });
    // Resilience: a compaction hiccup must never break the generation.
    expect(out.prompt).toEqual(params.prompt);
  });
});

describe("withLeanCtx", () => {
  it("throws a clear error when the 'ai' package is absent", () => {
    // `ai` is an optional peer dependency and not installed in this test env.
    expect(() => withLeanCtx({ id: "model" }, { baseUrl })).toThrow(LeanCtxError);
  });
});
