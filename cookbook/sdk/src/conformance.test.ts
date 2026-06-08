import { describe, expect, it } from "vitest";

import { LeanCtxClient } from "./client.js";
import { runConformance } from "./conformance.js";

/** A stub server that returns valid /v1 contract responses. */
function okFetch(): typeof fetch {
  return async (url) => {
    const u = String(url);
    if (u.endsWith("/health")) {
      return new Response("ok", { status: 200 });
    }
    if (u.endsWith("/v1/capabilities")) {
      return new Response(
        JSON.stringify({
          contract_version: 1,
          server: { name: "lean-ctx", version: "3.7.5" },
          plane: "personal",
          transports: ["rest"],
          presets: ["coding"],
          read_modes: ["full"],
          tools: { total: 1, names: ["ctx_read"] },
          features: {},
          extensions: {},
          contracts: {},
        }),
        { status: 200, headers: { "content-type": "application/json" } }
      );
    }
    if (u.endsWith("/v1/openapi.json")) {
      return new Response(
        JSON.stringify({ openapi: "3.0.3", info: {}, paths: {} }),
        { status: 200, headers: { "content-type": "application/json" } }
      );
    }
    if (u.includes("/v1/tools")) {
      return new Response(
        JSON.stringify({ tools: [], total: 0, offset: 0, limit: 1 }),
        { status: 200, headers: { "content-type": "application/json" } }
      );
    }
    return new Response("not found", { status: 404 });
  };
}

describe("runConformance", () => {
  it("passes against a conformant server", async () => {
    const c = new LeanCtxClient({
      baseUrl: "http://127.0.0.1:9",
      fetchImpl: okFetch(),
    });
    const card = await runConformance(c);
    expect(card.allPassed).toBe(true);
    expect(card.total).toBe(4);
    expect(card.passed).toBe(4);
  });

  it("records a failure when capabilities are malformed", async () => {
    const fetchImpl: typeof fetch = async (url) => {
      const u = String(url);
      if (u.endsWith("/v1/capabilities")) {
        return new Response(JSON.stringify({ wrong: true }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      return okFetch()(url);
    };
    const c = new LeanCtxClient({ baseUrl: "http://127.0.0.1:9", fetchImpl });
    const card = await runConformance(c);
    expect(card.allPassed).toBe(false);
    const caps = card.checks.find((x) => x.name === "capabilities_shape");
    expect(caps?.passed).toBe(false);
  });
});
