// SPDX-License-Identifier: Apache-2.0
import { afterEach, describe, expect, it, vi } from "vitest";

import { lastReadFallback, readViaBridge } from "../extensions/bridge-read.js";

describe("readViaBridge", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("falls back without writing into Pi's prompt editor (#2256)", async () => {
    const errors = vi.spyOn(console, "error");
    const failing = {
      callTool: async () => {
        throw new Error("MCP transport closed");
      },
    };

    const text = await readViaBridge(failing, { path: "/tmp/x.ts", mode: "full" });

    expect(text).toBeUndefined();
    expect(errors).not.toHaveBeenCalled();
    expect(lastReadFallback()).toBe("ctx_read(full) fell back to the CLI: MCP transport closed");
  });
});
