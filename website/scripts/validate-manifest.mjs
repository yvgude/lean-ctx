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
  check(tool.name?.startsWith('ctx_'), `tool "${tool.name}" has ctx_ prefix`);
  check(tool.description?.length > 10, `tool "${tool.name}" has description`);
  check(tool.schema_md5?.length === 32, `tool "${tool.name}" has valid MD5 hash`);
  check(tool.input_schema?.type === 'object', `tool "${tool.name}" has object schema`);
}

// Cross-check: tool page count matches manifest granular count
const pagesDir = path.join(ROOT, 'src', 'pages', 'docs', 'tools');
if (fs.existsSync(pagesDir)) {
  const toolPages = fs.readdirSync(pagesDir).filter(f => f.startsWith('ctx-') && f.endsWith('.astro'));
  check(
    toolPages.length === manifest.counts?.granular,
    `tool page count matches granular count (${toolPages.length} pages vs ${manifest.counts?.granular} tools)`
  );
} else {
  check(false, 'tool pages directory exists at src/pages/docs/tools/');
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
