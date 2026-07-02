// Builds extensions/vendor/mcp-sdk.js — a self-contained bundle of the MCP
// SDK client (incl. zod) so the published package needs ZERO runtime npm
// dependencies.
//
// Why (GH #670): pi installs every package into one shared npm prefix
// (~/.pi/agent/npm). Each `pi install`/`remove` re-reifies the whole tree and
// physically rewrites unrelated packages; an interrupted extraction (Windows
// AV/file locks) strands files like zod/v3/locales/en.js, and npm never
// repairs a package whose package.json version matches. Bundling removes the
// failure class: there is no zod left on disk to corrupt.
//
// The bundle is generated at publish time (`prepack`) and before tests; it is
// NOT committed. Node built-ins stay external (platform: node).
import { build } from "esbuild";
import { mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const outfile = resolve(here, "..", "extensions", "vendor", "mcp-sdk.cjs");
mkdirSync(dirname(outfile), { recursive: true });

const result = await build({
  entryPoints: [resolve(here, "vendor-entry.mjs")],
  outfile,
  bundle: true,
  platform: "node",
  target: "node18",
  // CJS on purpose: pi loads extensions through jiti, which transpiles every
  // module to CJS — an ESM bundle would need a createRequire banner that
  // collides with the CJS wrapper's own `require` binding. As CJS the bundle
  // requires node built-ins natively under jiti, and native ESM importers
  // (vitest, plain `node`) still get named exports via esbuild's
  // cjs-module-lexer annotation.
  format: "cjs",
  sourcemap: false,
  minify: false,
  legalComments: "inline",
  logLevel: "warning",
  metafile: true,
});

const bytes = Object.values(result.metafile.outputs)[0]?.bytes ?? 0;
console.log(`vendor bundle: ${outfile} (${(bytes / 1024).toFixed(0)} KB)`);
