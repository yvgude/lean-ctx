import { describe, expect, it } from "vitest";

import {
  classifySearchExit,
  innerTimeoutEnv,
  isDangerousEnvKey,
  sanitizeExtraEnv,
} from "../extensions/exec-result.js";

describe("classifySearchExit (#1762, #1499)", () => {
  it("treats exit 1 with empty stderr as a completed search with no matches", () => {
    // The exact shape `lean-ctx find absent.txt <dir>` produces: exit 1,
    // nothing on stdout, nothing on stderr. This used to surface as
    // "lean-ctx failed: find …".
    expect(classifySearchExit({ code: 1, stdout: "", stderr: "" }, "fallback")).toEqual({
      kind: "empty",
    });
    expect(classifySearchExit({ code: 1, stdout: "", stderr: "  \n" }, "fallback")).toEqual({
      kind: "empty",
    });
  });

  it("returns stdout on exit 0", () => {
    expect(
      classifySearchExit({ code: 0, stdout: "/p/present.txt\n", stderr: "" }, "fallback"),
    ).toEqual({ kind: "matches", stdout: "/p/present.txt\n" });
  });

  it("keeps exit 1 with stderr a real failure", () => {
    // #1499: ripgrep missing reports exit 1 *and* an explanation.
    expect(
      classifySearchExit({ code: 1, stdout: "", stderr: "'rg' is not recognized\n" }, "fb"),
    ).toEqual({ kind: "error", message: "'rg' is not recognized" });
  });

  it("reports exit >= 2 with the best available message", () => {
    expect(classifySearchExit({ code: 2, stdout: "", stderr: "bad regex" }, "fb")).toEqual({
      kind: "error",
      message: "bad regex",
    });
    expect(classifySearchExit({ code: 2, stdout: "partial", stderr: "" }, "fb")).toEqual({
      kind: "error",
      message: "partial",
    });
    expect(classifySearchExit({ code: 127, stdout: "", stderr: "" }, "lean-ctx failed: find x")).toEqual({
      kind: "error",
      message: "lean-ctx failed: find x",
    });
  });
});

describe("sanitizeExtraEnv (#1761)", () => {
  it("accepts the variables the CLI's inline-override rejection recommends", () => {
    // GIT_EDITOR is blocked as an *inline* assignment (it can redirect which
    // binary runs when parsed out of a shell string) but allowed as a
    // structured env entry — exactly the recovery the rejection names.
    const { accepted, rejected } = sanitizeExtraEnv({
      GIT_EDITOR: "true",
      GIT_ASKPASS: "/usr/bin/false",
      FOO: "bar",
    });
    expect(accepted).toEqual({ GIT_EDITOR: "true", GIT_ASKPASS: "/usr/bin/false", FOO: "bar" });
    expect(rejected).toEqual([]);
  });

  it("drops the keys the MCP env parameter drops, case-insensitively", () => {
    const { accepted, rejected } = sanitizeExtraEnv({
      PATH: "/tmp",
      path: "/tmp",
      LD_PRELOAD: "x.so",
      LD_AUDIT_PATH: "y",
      LEAN_CTX_COMPRESS: "0",
      lctx_anything: "1",
      NODE_OPTIONS: "--require evil",
      OK: "1",
    });
    expect(accepted).toEqual({ OK: "1" });
    expect(rejected).toEqual([
      "PATH",
      "path",
      "LD_PRELOAD",
      "LD_AUDIT_PATH",
      "LEAN_CTX_COMPRESS",
      "lctx_anything",
      "NODE_OPTIONS",
    ]);
  });

  it("rejects non-string values and non-object input", () => {
    expect(sanitizeExtraEnv({ N: 1, S: "s" })).toEqual({ accepted: { S: "s" }, rejected: ["N"] });
    expect(sanitizeExtraEnv(undefined)).toEqual({ accepted: {}, rejected: [] });
    expect(sanitizeExtraEnv(["A=1"])).toEqual({ accepted: {}, rejected: [] });
    expect(sanitizeExtraEnv("A=1")).toEqual({ accepted: {}, rejected: [] });
  });

  it("classifies keys exactly like the Rust list", () => {
    for (const key of ["GIT_SSH", "GIT_SSH_COMMAND", "GIT_EXEC_PATH", "HOME", "GOROOT"]) {
      expect(isDangerousEnvKey(key), key).toBe(true);
    }
    for (const key of ["GIT_EDITOR", "GIT_EXTERNAL_DIFF", "SSH_ASKPASS", "RUST_LOG", "TERM"]) {
      expect(isDangerousEnvKey(key), key).toBe(false);
    }
  });
});

describe("innerTimeoutEnv (#1833)", () => {
  it("hands the per-call timeout to lean-ctx -c in milliseconds", () => {
    // The issue's repro: timeout=200 must outlast lean-ctx's 120 s default.
    expect(innerTimeoutEnv(200, {})).toEqual({ LEAN_CTX_SHELL_TIMEOUT_MS: "200000" });
    expect(innerTimeoutEnv(0.5, {})).toEqual({ LEAN_CTX_SHELL_TIMEOUT_MS: "500" });
  });

  it("caps at lean-ctx's own one-hour ceiling for a per-call timeout", () => {
    expect(innerTimeoutEnv(86_400, {})).toEqual({ LEAN_CTX_SHELL_TIMEOUT_MS: "3600000" });
  });

  it("adds nothing without a usable timeout", () => {
    for (const value of [undefined, 0, -5, Number.NaN, Number.POSITIVE_INFINITY, "200"]) {
      expect(innerTimeoutEnv(value, {}), String(value)).toEqual({});
    }
  });

  it("leaves an operator pin in place, as lean-ctx does", () => {
    expect(innerTimeoutEnv(200, { LEAN_CTX_SHELL_TIMEOUT_MS: "900000" })).toEqual({});
    // lean-ctx ignores an empty, zero or malformed pin, so the call's value applies.
    for (const pin of ["", "0", "abc", "-1"]) {
      expect(innerTimeoutEnv(200, { LEAN_CTX_SHELL_TIMEOUT_MS: pin }), pin).toEqual({
        LEAN_CTX_SHELL_TIMEOUT_MS: "200000",
      });
    }
  });
});
