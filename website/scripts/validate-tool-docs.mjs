#!/usr/bin/env node

/**
 * CI validation: compares tool schemas in mcp-tools.json with documentation
 * entries in tool-enrichments.json. Warns about undocumented tools and
 * parameters that exist in the schema but lack enrichment documentation.
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');

const manifestPath = path.join(ROOT, 'generated', 'mcp-tools.json');
const enrichPath = path.join(ROOT, 'generated', 'tool-enrichments.json');

let errors = 0;
let warnings = 0;

function fail(message) {
  console.error(`  FAIL: ${message}`);
  errors++;
}

function warn(message) {
  console.warn(`  WARN: ${message}`);
  warnings++;
}

function ok(message) {
  console.log(`  OK: ${message}`);
}

console.log('Validating tool documentation coverage...\n');

if (!fs.existsSync(manifestPath)) {
  fail('mcp-tools.json not found');
  process.exit(1);
}

if (!fs.existsSync(enrichPath)) {
  fail('tool-enrichments.json not found');
  process.exit(1);
}

const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf-8'));
const enrichments = JSON.parse(fs.readFileSync(enrichPath, 'utf-8'));

const tools = manifest.tools?.granular ?? [];
const enrichedTools = enrichments.tools ?? {};
const allCategoryTools = new Set(
  Object.values(enrichments.categories ?? {}).flatMap(c => c.tools)
);

console.log(`Manifest: ${tools.length} tools | Enrichments: ${Object.keys(enrichedTools).length} tool entries\n`);

for (const tool of tools) {
  const name = tool.name;
  const enrichment = enrichedTools[name];

  if (!enrichment) {
    warn(`"${name}" has no enrichment entry in tool-enrichments.json`);
    continue;
  }

  if (!allCategoryTools.has(name)) {
    warn(`"${name}" has enrichment data but is not assigned to any category`);
  }

  const schemaParams = Object.keys(tool.input_schema?.properties ?? {});
  const docExamples = enrichment.examples ?? [];
  const hasWhenToUse = enrichment.when_to_use?.length > 0;
  const hasWhenNotToUse = enrichment.when_not_to_use?.length > 0;

  if (!hasWhenToUse) {
    warn(`"${name}" enrichment missing "when_to_use"`);
  }
  if (!hasWhenNotToUse) {
    warn(`"${name}" enrichment missing "when_not_to_use"`);
  }

  if (schemaParams.length > 0 && docExamples.length === 0) {
    warn(`"${name}" has ${schemaParams.length} schema params but no usage examples in enrichment`);
  }

  ok(`"${name}" — ${schemaParams.length} params, ${docExamples.length} examples`);
}

// Check for enrichment entries that don't exist in the manifest
const manifestNames = new Set(tools.map(t => t.name));
for (const name of Object.keys(enrichedTools)) {
  if (!manifestNames.has(name)) {
    warn(`enrichment "${name}" has no matching tool in mcp-tools.json`);
  }
}

console.log(`\n${tools.length} tools checked`);
console.log(`${warnings} warning(s), ${errors} error(s)`);
process.exit(errors > 0 ? 1 : 0);
