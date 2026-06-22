import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";

import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";

import { ProxyClient, compress } from "../src/proxy";
import { LeanCtxAuthError, LeanCtxConnectionError, LeanCtxError } from "../src/errors";

const TOKEN = "secret-token";

let server: Server;
let baseUrl: string;
let requireToken = false;
let mode: "ok" | "bad" = "ok";
let lastBody: Record<string, unknown> | undefined;

beforeAll(async () => {
  server = createServer((req, res) => {
    if (req.method === "GET" && req.url?.startsWith("/v1/references/")) {
      const referenceId = req.url.slice("/v1/references/".length);
      res.setHeader("content-type", "text/plain");
      if (referenceId === "missing") {
        res.statusCode = 404;
        res.end("Reference expired or not found");
        return;
      }
      res.statusCode = 200;
      res.end(`ORIGINAL[${referenceId}]`);
      return;
    }
    if (req.url !== "/v1/compress" || req.method !== "POST") {
      res.statusCode = 404;
      res.end();
      return;
    }
    if (requireToken && req.headers["authorization"] !== `Bearer ${TOKEN}`) {
      res.statusCode = 401;
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ error: "unauthorized" }));
      return;
    }
    let raw = "";
    req.on("data", (chunk) => (raw += chunk));
    req.on("end", () => {
      const body = JSON.parse(raw) as { messages: Array<Record<string, unknown>>; model?: string };
      lastBody = body;
      res.setHeader("content-type", "application/json");
      if (mode === "bad") {
        res.statusCode = 200;
        res.end(JSON.stringify({ unexpected: true }));
        return;
      }
      let original = 0;
      let compressed = 0;
      const out = body.messages.map((message) => {
        const rewritten = { ...message };
        if (typeof rewritten.content === "string") {
          original += rewritten.content.length;
          rewritten.content = rewritten.content.slice(0, 8);
          compressed += (rewritten.content as string).length;
        }
        return rewritten;
      });
      const saved = original - compressed;
      res.statusCode = 200;
      res.end(
        JSON.stringify({
          messages: out,
          stats: {
            original_tokens: original,
            compressed_tokens: compressed,
            saved_tokens: saved,
            saved_pct: original ? Math.round((saved / original) * 1000) / 10 : 0,
            model: body.model ?? null,
          },
        }),
      );
    });
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address() as AddressInfo;
  baseUrl = `http://127.0.0.1:${port}`;
});

afterAll(() => {
  server.close();
});

afterEach(() => {
  requireToken = false;
  mode = "ok";
  lastBody = undefined;
});

describe("ProxyClient transport", () => {
  it("compress() returns rewritten messages", async () => {
    const out = await compress([{ role: "user", content: "this is a long message body" }], {
      model: "gpt-4o",
      baseUrl,
      token: TOKEN,
    });
    expect(out).toEqual([{ role: "user", content: "this is " }]);
  });

  it("client returns stats and sends the model", async () => {
    const client = new ProxyClient({ baseUrl, token: TOKEN });
    const result = await client.compress([{ role: "user", content: "abcdefghijklmnop" }], "claude-sonnet-4");
    expect(result.stats.saved_tokens).toBe("abcdefghijklmnop".length - 8);
    expect(result.stats.saved_pct).toBeGreaterThan(0);
    expect(lastBody?.model).toBe("claude-sonnet-4");
  });

  it("maps 401 to LeanCtxAuthError", async () => {
    requireToken = true;
    await expect(
      compress([{ role: "user", content: `needs a valid token ${"x".repeat(20)}` }], {
        baseUrl,
        token: "wrong",
      }),
    ).rejects.toBeInstanceOf(LeanCtxAuthError);
  });

  it("maps an unreachable daemon to LeanCtxConnectionError", async () => {
    await expect(
      compress([{ role: "user", content: "x".repeat(50) }], { baseUrl: "http://127.0.0.1:1", token: "t" }),
    ).rejects.toBeInstanceOf(LeanCtxConnectionError);
  });

  it("rejects non-array messages", async () => {
    const client = new ProxyClient({ baseUrl, token: TOKEN });
    // @ts-expect-error intentional misuse
    await expect(client.compress({ role: "user" })).rejects.toBeInstanceOf(TypeError);
  });

  it("raises on a malformed response", async () => {
    mode = "bad";
    await expect(
      compress([{ role: "user", content: "y".repeat(40) }], { baseUrl, token: TOKEN }),
    ).rejects.toBeInstanceOf(LeanCtxError);
  });

  it("resolveReference returns the original content", async () => {
    const client = new ProxyClient({ baseUrl, token: TOKEN });
    expect(await client.resolveReference("abc123")).toBe("ORIGINAL[abc123]");
  });

  it("resolveReference rejects a missing reference", async () => {
    const client = new ProxyClient({ baseUrl, token: TOKEN });
    await expect(client.resolveReference("missing")).rejects.toBeInstanceOf(LeanCtxError);
  });

  it("resolveReference rejects an empty id", async () => {
    const client = new ProxyClient({ baseUrl, token: TOKEN });
    await expect(client.resolveReference("")).rejects.toBeInstanceOf(TypeError);
  });
});
