import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { portForUid, resolveBaseUrl, resolvePort, resolveToken } from "../src/discovery";

const ENV_KEYS = [
  "LEAN_CTX_PROXY_URL",
  "LEAN_CTX_PROXY_PORT",
  "LEAN_CTX_PROXY_TOKEN",
  "LEAN_CTX_DATA_DIR",
  "XDG_DATA_HOME",
  "XDG_CONFIG_HOME",
  "HOME",
];

let saved: Record<string, string | undefined>;
let tmp: string;

beforeEach(() => {
  saved = {};
  for (const key of ENV_KEYS) {
    saved[key] = process.env[key];
    delete process.env[key];
  }
  tmp = mkdtempSync(join(tmpdir(), "leanctx-"));
  process.env.HOME = tmp; // isolate ~/.lean-ctx and XDG defaults
});

afterEach(() => {
  for (const key of ENV_KEYS) {
    if (saved[key] === undefined) delete process.env[key];
    else process.env[key] = saved[key];
  }
  rmSync(tmp, { recursive: true, force: true });
});

describe("discovery", () => {
  it("base url explicit strips trailing slash", () => {
    expect(resolveBaseUrl("http://host:9/")).toBe("http://host:9");
  });

  it("base url from env", () => {
    process.env.LEAN_CTX_PROXY_URL = "http://h:1234/";
    expect(resolveBaseUrl()).toBe("http://h:1234");
  });

  it("port env wins", () => {
    process.env.LEAN_CTX_PROXY_PORT = "5005";
    expect(resolvePort()).toBe(5005);
  });

  it("portForUid matches the rust formula", () => {
    expect(portForUid(1000)).toBe(4444);
    expect(portForUid(2999)).toBe(5443);
    expect(portForUid(500)).toBe(4444);
  });

  it("port read from config.toml", () => {
    process.env.LEAN_CTX_DATA_DIR = tmp;
    writeFileSync(join(tmp, "config.toml"), "proxy_port = 4500\n");
    expect(resolvePort()).toBe(4500);
  });

  it("commented proxy_port is ignored", () => {
    process.env.LEAN_CTX_DATA_DIR = tmp;
    writeFileSync(join(tmp, "config.toml"), "# proxy_port = 3128\n");
    expect(resolvePort()).not.toBe(3128);
  });

  it("token env precedence", () => {
    process.env.LEAN_CTX_PROXY_TOKEN = "envtok";
    expect(resolveToken()).toBe("envtok");
  });

  it("token from session_token file", () => {
    process.env.LEAN_CTX_DATA_DIR = tmp;
    writeFileSync(join(tmp, "session_token"), "deadbeef\n");
    expect(resolveToken()).toBe("deadbeef");
  });

  it("session_token file is withheld from a non-loopback proxy url", () => {
    process.env.LEAN_CTX_DATA_DIR = tmp;
    writeFileSync(join(tmp, "session_token"), "deadbeef\n");
    // A remote base url must never receive the local on-disk credential.
    expect(resolveToken(undefined, "https://remote.example:443")).toBeUndefined();
    // A loopback base url still uses the file token.
    expect(resolveToken(undefined, "http://127.0.0.1:4444")).toBe("deadbeef");
  });

  it("token absent returns undefined", () => {
    expect(resolveToken()).toBeUndefined();
  });
});
