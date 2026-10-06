// SPDX-License-Identifier: Apache-2.0
// Loopback HTTP server of the semantic bridge. No `vscode` import.

import { timingSafeEqual } from "crypto";
import * as http from "http";
import { AddressInfo } from "net";
import { Navigator, handle } from "./protocol";

/** A request body is a few hundred bytes; anything far larger is refused. */
const MAX_BODY_BYTES = 64 * 1024;

export interface BridgeServer {
  port: number;
  close(): Promise<void>;
}

function tokenMatches(expected: Buffer, got: string | string[] | undefined): boolean {
  if (typeof got !== "string") return false;
  const actual = Buffer.from(got);
  return actual.length === expected.length && timingSafeEqual(actual, expected);
}

function send(res: http.ServerResponse, status: number, body: unknown): void {
  const text = JSON.stringify(body);
  res.writeHead(status, {
    "Content-Type": "application/json",
    "Content-Length": Buffer.byteLength(text),
    Connection: "close",
  });
  res.end(text);
}

/**
 * Serves `root` on `127.0.0.1` (ephemeral port). Every request must carry
 * `X-LeanCtx-Token: <token>`; anything else gets 401 before it is read.
 */
export function startBridge(opts: {
  root: string;
  token: string;
  navigator: Navigator;
  info: { editor: string; version: string };
}): Promise<BridgeServer> {
  const expected = Buffer.from(opts.token);
  const server = http.createServer((req, res) => {
    if (!tokenMatches(expected, req.headers["x-leanctx-token"])) {
      send(res, 401, { error: { code: "UNAUTHORIZED", message: "missing or wrong token" } });
      req.resume();
      return;
    }
    const chunks: Buffer[] = [];
    let size = 0;
    let refused = false;
    req.on("data", (chunk: Buffer) => {
      size += chunk.length;
      if (size > MAX_BODY_BYTES) {
        if (!refused) send(res, 413, { error: { code: "TOO_LARGE", message: "request body too large" } });
        refused = true;
        return;
      }
      chunks.push(chunk);
    });
    req.on("end", () => {
      if (refused) return;
      let body: Record<string, unknown> = {};
      if (size > 0) {
        try {
          const parsed: unknown = JSON.parse(Buffer.concat(chunks).toString("utf8"));
          if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
            body = parsed as Record<string, unknown>;
          }
        } catch {
          send(res, 400, { error: { code: "BAD_REQUEST", message: "body is not JSON" } });
          return;
        }
      }
      const route = (req.url ?? "/").split("?")[0];
      handle(opts.navigator, opts.root, opts.info, req.method ?? "GET", route, body).then(
        (reply) => send(res, reply.status, reply.body),
        (err: unknown) =>
          send(res, 200, { error: { code: "INTERNAL", message: err instanceof Error ? err.message : String(err) } }),
      );
    });
  });
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", reject);
      resolve({
        port: (server.address() as AddressInfo).port,
        close: () =>
          new Promise<void>((done) => {
            server.close(() => done());
            server.closeAllConnections();
          }),
      });
    });
  });
}
