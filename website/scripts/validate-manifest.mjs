#!/usr/bin/env node

/**
 * CI validation: ensures mcp-tools.json is valid and matches expectations.
 * Checks: JSON parseable, has required fields, tool count > 0, all tools have schemas.
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const manifestPath = path.join(ROOT, 'generated', 'mcp-tools.json');

let errors = 0;

function check(condition, message) {
  if (!condition) {
    console.error(`FAIL: ${message}`);
    errors++;
  } else {
    console.log(`  OK: ${message}`);
  }
}

console.log('Validating mcp-tools.json...\n');

check(fs.existsSync(manifestPath), 'mcp-tools.json exists');

const raw = fs.readFileSync(manifestPath, 'utf-8');
let manifest;
try {
  manifest = JSON.parse(raw);
  check(true, 'JSON is valid');
} catch {
  check(false, 'JSON is valid');
  process.exit(1);
}

check(manifest.schema_version === 1, 'schema_version is 1');
check(manifest.counts?.granular > 0, `granular tool count > 0 (got ${manifest.counts?.granular})`);
check(manifest.counts?.unified > 0, `unified tool count > 0 (got ${manifest.counts?.unified})`);
check(manifest.read_modes?.count >= 10, `read mode count >= 10 (got ${manifest.read_modes?.count})`);

const tools = manifest.tools?.granular ?? [];
check(tools.length === manifest.counts?.granular, `tool array length matches count (${tools.length} vs ${manifest.counts?.granular})`);

for (const tool of tools) {
  // Every tool is namespaced ctx_*, except the unprefixed `shell` alias of ctx_shell.
  check(tool.name === 'shell' || tool.name?.startsWith('ctx_'), `tool "${tool.name}" has ctx_ prefix (or is the shell alias)`);
  check(tool.description?.length > 10, `tool "${tool.name}" has description`);
  check(tool.schema_md5?.length === 64, `tool "${tool.name}" has a 64-char schema hash`);
  check(tool.input_schema?.type === 'object', `tool "${tool.name}" has object schema`);
}

// Reachability instead of file count: the site routes most tools to their category
// page via redirects rather than generating 63 standalone files. Every granular tool
// must resolve to either a dedicated page (src/pages/docs/tools/<slug>.astro) or a
// redirect declared in astro.config.mjs — otherwise its /docs/tools/<slug> URL 404s.
const pagesDir = path.join(ROOT, 'src', 'pages', 'docs', 'tools');
const pageSlugs = fs.existsSync(pagesDir)
  ? new Set(
      fs
        .readdirSync(pagesDir)
        .filter((f) => f.startsWith('ctx-') && f.endsWith('.astro'))
        .map((f) => f.replace(/\.astro$/, ''))
    )
  : new Set();

const astroConfig = fs.readFileSync(path.join(ROOT, 'astro.config.mjs'), 'utf-8');
const redirectSlugs = new Set(
  [...astroConfig.matchAll(/['"]\/docs\/tools\/([a-z0-9][a-z0-9-]*)\/?['"]\s*:/g)].map((m) => m[1])
);

for (const tool of tools) {
  const slug = tool.name.replace(/_/g, '-');
  check(
    pageSlugs.has(slug) || redirectSlugs.has(slug),
    `tool "${tool.name}" reachable via page or redirect (/docs/tools/${slug})`
  );
}

// Cross-check: read modes count is exactly 10
check(manifest.read_modes?.count === 10, `read mode count is exactly 10 (got ${manifest.read_modes?.count})`);

const enrichPath = path.join(ROOT, 'generated', 'tool-enrichments.json');
if (fs.existsSync(enrichPath)) {
  const enrichments = JSON.parse(fs.readFileSync(enrichPath, 'utf-8'));
  const allCategoryTools = Object.values(enrichments.categories).flatMap(c => c.tools);
  const manifestNames = new Set(tools.map(t => t.name));

  for (const name of allCategoryTools) {
    check(manifestNames.has(name), `enrichment tool "${name}" exists in manifest`);
  }

  for (const name of manifestNames) {
    check(allCategoryTools.includes(name), `manifest tool "${name}" is categorized in enrichments`);
  }
}

console.log(`\n${errors === 0 ? 'All checks passed' : `${errors} check(s) failed`}`);
process.exit(errors > 0 ? 1 : 0);
