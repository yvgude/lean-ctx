// SPDX-License-Identifier: Apache-2.0
import { readFileSync } from "node:fs";
import { afterEach, describe, expect, it } from "vitest";

import { LENS_READ_BRIDGE_KEY, notifyLensRead } from "../extensions/lens-read-bridge.js";

const globalSlots = globalThis as Record<symbol, unknown>;

function installBridge(bridge: unknown): void {
  globalSlots[LENS_READ_BRIDGE_KEY] = bridge;
}

function fakeBridge() {
  const entries: Record<string, unknown>[] = [];
  return {
    entries,
    bridge: {
      version: 1,
      recordRead(entry: Record<string, unknown>) {
        entries.push(entry);
      },
    },
  };
}

afterEach(() => {
  delete globalSlots[LENS_READ_BRIDGE_KEY];
});

describe("notifyLensRead (#2200)", () => {
  it("uses the symbol pi-lens registers its bridge under", () => {
    expect(LENS_READ_BRIDGE_KEY).toBe(Symbol.for("pi-lens:read-bridge"));
  });

  it("records a full read from line 1 with no limit", () => {
    const { bridge, entries } = fakeBridge();
    installBridge(bridge);
    notifyLensRead("/repo/src/main.ts");
    expect(entries).toEqual([{ filePath: "/repo/src/main.ts", requestedOffset: 1, consumer: "lean-ctx" }]);
  });

  it("records a line-range read with its offset and limit", () => {
    const { bridge, entries } = fakeBridge();
    installBridge(bridge);
    notifyLensRead("/repo/a.ts", 40, 20);
    notifyLensRead("/repo/b.ts", undefined, 0);
    expect(entries).toEqual([
      { filePath: "/repo/a.ts", requestedOffset: 40, requestedLimit: 20, consumer: "lean-ctx" },
      { filePath: "/repo/b.ts", requestedOffset: 1, requestedLimit: 0, consumer: "lean-ctx" },
    ]);
  });

  it("normalises invalid offsets and limits instead of forwarding them", () => {
    const { bridge, entries } = fakeBridge();
    installBridge(bridge);
    notifyLensRead("/repo/a.ts", 0, -1);
    notifyLensRead("/repo/a.ts", 2.5, 1.5);
    expect(entries).toEqual([
      { filePath: "/repo/a.ts", requestedOffset: 1, consumer: "lean-ctx" },
      { filePath: "/repo/a.ts", requestedOffset: 1, consumer: "lean-ctx" },
    ]);
  });

  it("is a silent no-op when the bridge is absent", () => {
    expect(() => notifyLensRead("/repo/a.ts", 1, 1)).not.toThrow();
  });

  it("ignores a bridge with another version or without recordRead", () => {
    const calls: unknown[] = [];
    installBridge({ version: 2, recordRead: (entry: unknown) => calls.push(entry) });
    notifyLensRead("/repo/a.ts");
    installBridge({ version: 1 });
    expect(() => notifyLensRead("/repo/a.ts")).not.toThrow();
    installBridge("not-an-object");
    expect(() => notifyLensRead("/repo/a.ts")).not.toThrow();
    expect(calls).toEqual([]);
  });

  it("never fails the read when the bridge throws", () => {
    installBridge({
      version: 1,
      recordRead() {
        throw new Error("pi-lens exploded");
      },
    });
    expect(() => notifyLensRead("/repo/a.ts", 3, 4)).not.toThrow();
  });

  it("never fails the read when reading the bridge itself throws", () => {
    Object.defineProperty(globalSlots, LENS_READ_BRIDGE_KEY, {
      configurable: true,
      get() {
        throw new Error("getter exploded");
      },
    });
    expect(() => notifyLensRead("/repo/a.ts")).not.toThrow();
  });
});

describe("ctx_read publishes to the pi-lens read bridge (#2200)", () => {
  // The five non-image ctx_read success paths: line-range via MCP bridge, CLI
  // and local slice fallback; full/mode via MCP bridge and CLI. The image path
  // delegates to the native read tool, which pi-lens already records.
  it("calls notifyLensRead on every non-image success path", () => {
    const source = readFileSync(new URL("../extensions/index.ts", import.meta.url), "utf8");
    const rangeCalls = source.match(/notifyLensRead\(absolutePath, startOffset, params\.limit\);/g) ?? [];
    const fullCalls = source.match(/notifyLensRead\(absolutePath\);/g) ?? [];
    expect(rangeCalls).toHaveLength(3);
    expect(fullCalls).toHaveLength(2);
  });
});
