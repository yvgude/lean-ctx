import { type ChildProcess, spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { LeanCtxClient } from "./client.js";
import { LeanCtxHttpError } from "./errors.js";

function repoRootFromHere(): string {
  const here = path.dirname(fileURLToPath(import.meta.url)); // cookbook/sdk/src
  return path.resolve(here, "../../.."); // repo root
}

async function findFreePort(): Promise<number> {
  return await new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.on("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      const addr = srv.address();
      if (!addr || typeof addr === "string") {
        srv.close(() => reject(new Error("failed to bind ephemeral port")));
        return;
      }
      const port = addr.port;
      srv.close((err) => {
        if (err) reject(err);
        else resolve(port);
      });
    });
  });
}

async function sleep(ms: number): Promise<void> {
  await new Promise((r) => setTimeout(r, ms));
}

async function waitForHealthy(
  baseUrl: string,
  timeoutMs: number
): Promise<void> {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    try {
      const res = await fetch(`${baseUrl}/health`, { method: "GET" });
      if (res.ok) return;
    } catch {
      // ignore
    }
    await sleep(50);
  }
  throw new Error(
    `lean-ctx server did not become healthy within ${timeoutMs}ms`
  );
}

function startLeanCtxServer(opts: {
  binPath: string;
  port: number;
  projectRoot: string;
  authToken?: string;
  maxRps?: number;
  rateBurst?: number;
}): { proc: ChildProcess; baseUrl: string; stop: () => Promise<void> } {
  const baseUrl = `http://127.0.0.1:${opts.port}`;
  const args = [
    "serve",
    "--host",
    "127.0.0.1",
    "--port",
    String(opts.port),
    "--project-root",
    opts.projectRoot,
  ];
  if (opts.maxRps !== undefined) args.push("--max-rps", String(opts.maxRps));
  if (opts.rateBurst !== undefined)
    args.push("--rate-burst", String(opts.rateBurst));

  const proc = spawn(opts.binPath, args, {
    stdio: ["ignore", "pipe", "pipe"],
    // The token goes in the environment: `--auth-token` would put it in the
    // process list (deprecated since 3.13.0).
    env: opts.authToken
      ? { ...process.env, LEAN_CTX_HTTP_TOKEN: opts.authToken }
      : { ...process.env },
  });

  const stop = async () => {
    if (proc.exitCode !== null) return;
    proc.kill("SIGINT");
    await Promise.race([
      new Promise<void>((resolve) => proc.once("exit", () => resolve())),
      sleep(3_000).then(() => {
        if (proc.exitCode === null) proc.kill("SIGKILL");
      }),
    ]);
  };

  return { proc, baseUrl, stop };
}

describe("LeanCtxClient E2E (real server)", () => {
  it("calls health/manifest/tools/call and surfaces typed error codes", async () => {
    const repoRoot = repoRootFromHere();
    const binPath =
      process.env.LEAN_CTX_BIN?.trim() ||
      path.join(repoRoot, "rust/target/debug/lean-ctx");

    if (!fs.existsSync(binPath)) {
      console.warn(
        `Skipping E2E: lean-ctx binary not found at ${binPath}. Build with: (cd rust && cargo build --all-features)`
      );
      return;
    }

    const port = await findFreePort();
    const { proc, baseUrl, stop } = startLeanCtxServer({
      binPath,
      port,
      projectRoot: repoRoot,
      authToken: "test-token",
    });

    try {
      await waitForHealthy(baseUrl, 10_000);

      const unauth = new LeanCtxClient({ baseUrl });
      const ok = await unauth.health();
      expect(ok).toContain("ok");

      try {
        await unauth.manifest();
        throw new Error("expected unauthorized manifest request to throw");
      } catch (e) {
        expect(e).toBeInstanceOf(LeanCtxHttpError);
        const err = e as LeanCtxHttpError;
        expect(err.status).toBe(401);
        expect(err.errorCode).toBe("unauthorized");
      }

      const c = new LeanCtxClient({ baseUrl, bearerToken: "test-token" });
      const manifest = await c.manifest();
      expect(manifest).toBeTruthy();

      const tools = await c.listTools({ offset: 0, limit: 10 });
      expect(Array.isArray(tools.tools)).toBe(true);
      expect(typeof tools.total).toBe("number");
      expect(tools.total).toBeGreaterThan(0);

      const text = await c.callToolText("ctx_read", {
        path: "docs/contracts/http-mcp-contract-v1.md",
        mode: "lines:1-10",
      });
      expect(text).toContain("HTTP-MCP Contract v1");
    } finally {
      await stop();
      proc.stdout?.destroy();
      proc.stderr?.destroy();
    }
  }, 60_000);
});
