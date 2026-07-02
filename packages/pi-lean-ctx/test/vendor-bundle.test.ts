import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

// Regression guards for GH #670: the package must stay free of runtime npm
// dependencies (pi's shared-prefix reification corrupted zod beyond repair),
// with the MCP SDK vendored as one self-contained CJS bundle. `pretest`
// builds the bundle, so these run against the exact artifact npm pack ships.

const require = createRequire(import.meta.url);
const pkgRoot = resolve(__dirname, "..");
const bundlePath = resolve(pkgRoot, "extensions", "vendor", "mcp-sdk.cjs");

describe("vendor bundle (zero runtime deps, #670)", () => {
  it("package.json declares NO runtime dependencies", () => {
    const pkg = JSON.parse(readFileSync(resolve(pkgRoot, "package.json"), "utf8"));
    expect(pkg.dependencies).toBeUndefined();
  });

  it("bundle exposes Client + StdioClientTransport via CJS require (jiti path)", () => {
    const bundle = require(bundlePath);
    expect(typeof bundle.Client).toBe("function");
    expect(typeof bundle.StdioClientTransport).toBe("function");
  });

  it("bundle exposes named exports via native ESM import", async () => {
    const bundle = await import(bundlePath);
    expect(typeof bundle.Client).toBe("function");
    expect(typeof bundle.StdioClientTransport).toBe("function");
  });

  it("bundle is self-contained — only node built-ins left as requires", () => {
    const source = readFileSync(bundlePath, "utf8");
    // esbuild leaves `require("<builtin>")` for externals (platform: node).
    // Any bare package specifier here would mean an unbundled runtime dep
    // that resolves from the corruptible shared prefix again.
    const builtins = new Set<string>(require("node:module").builtinModules);
    const externals = [...source.matchAll(/\brequire\("([^"./][^"]*)"\)/g)]
      .map((m) => m[1])
      // ajv embeds require() calls inside *generated-code string templates*
      // (standalone codegen, never executed at runtime); real requires never
      // carry a dist/ subpath of ajv.
      .filter((spec) => !spec.startsWith("ajv/dist/") && !spec.startsWith("ajv-formats/dist/"))
      .filter((spec) => !builtins.has(spec.replace(/^node:/, "")));
    expect(externals).toEqual([]);
  });
});
