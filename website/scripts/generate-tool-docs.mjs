#!/usr/bin/env node

/**
 * Generates individual Astro route files for each MCP tool.
 * Reads tool names from generated/mcp-tools.json and creates:
 *   - src/pages/docs/tools/{tool-slug}.astro (root route)
 *   - src/pages/[locale]/docs/tools/{tool-slug}.astro (i18n route)
 *
 * Each route imports DocsToolDetailPage.astro with the tool name as prop.
 * Run: node scripts/generate-tool-docs.mjs
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');

const manifestPath = path.join(ROOT, 'generated', 'mcp-tools.json');
const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf-8'));

const tools = manifest.tools.granular;

function toolNameToSlug(name) {
  return name.replace(/_/g, '-');
}

function generateRouteContent(toolName) {
  return `---
import DocsToolDetailPage from '../../../../page-templates/DocsToolDetailPage.astro';
import { getLocaleStaticPaths } from '../../../../i18n/routing.ts';
export const getStaticPaths = getLocaleStaticPaths;
---
<DocsToolDetailPage toolName="${toolName}" />
`;
}

function generateRootRouteContent(toolName) {
  return `---
import DocsToolDetailPage from '../../../page-templates/DocsToolDetailPage.astro';
---
<DocsToolDetailPage toolName="${toolName}" />
`;
}

const rootDir = path.join(ROOT, 'src', 'pages', 'docs', 'tools');
const localeDir = path.join(ROOT, 'src', 'pages', '[locale]', 'docs', 'tools');

fs.mkdirSync(rootDir, { recursive: true });
fs.mkdirSync(localeDir, { recursive: true });

let created = 0;
let skipped = 0;

for (const tool of tools) {
  const slug = toolNameToSlug(tool.name);

  const rootFile = path.join(rootDir, `${slug}.astro`);
  const localeFile = path.join(localeDir, `${slug}.astro`);

  fs.writeFileSync(rootFile, generateRootRouteContent(tool.name));
  fs.writeFileSync(localeFile, generateRouteContent(tool.name));
  created++;
}

console.log(`Generated ${created} tool pages (${created} root + ${created} locale = ${created * 2} files total)`);
console.log(`Skipped: ${skipped} (category overlap)`);
console.log(`\nTool slugs:`);
for (const tool of tools) {
  const slug = toolNameToSlug(tool.name);
  console.log(`  /docs/tools/${slug}`);
}
