import { describe, expect, it } from "vitest";

import { posixSafeEnv, POSIX_ENV_NAME } from "../extensions/env.js";

describe("posixSafeEnv", () => {
  it("drops the Windows names that make a validating host refuse the spawn", () => {
    // #1799: these two exist on every 64-bit Windows install. A host bash tool
    // that validates names rejected the whole spawn, so *every* ctx_shell call
    // failed before the command ran — not just ones touching these variables.
    const safe = posixSafeEnv({
      PATH: "/usr/bin",
      "ProgramFiles(x86)": "C:\\Program Files (x86)",
      "CommonProgramFiles(x86)": "C:\\Program Files (x86)\\Common Files",
    });

    expect(Object.keys(safe)).toEqual(["PATH"]);
  });

  it("keeps ordinary names, including the flags lean-ctx relies on", () => {
    const env = {
      PATH: "/usr/bin",
      HOME: "/home/user",
      _UNDERSCORE_FIRST: "ok",
      LEAN_CTX_COMPRESS: "1",
      LEAN_CTX_SAVINGS_FOOTER: "always",
      MIXED123: "ok",
    };

    expect(posixSafeEnv(env)).toEqual(env);
  });

  it("preserves values verbatim, including empty ones", () => {
    expect(posixSafeEnv({ EMPTY: "", SPACED: "a b  c" })).toEqual({
      EMPTY: "",
      SPACED: "a b  c",
    });
  });

  it("rejects every name shape a POSIX shell cannot use as an identifier", () => {
    for (const name of ["1LEADING_DIGIT", "HAS-DASH", "HAS.DOT", "HAS SPACE", "HAS(PAREN)", ""]) {
      expect(POSIX_ENV_NAME.test(name), `${name} must be rejected`).toBe(false);
    }
  });
});
