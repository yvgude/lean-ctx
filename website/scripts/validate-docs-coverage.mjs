#!/usr/bin/env node

/**
 * CI validation: ensures every tool in mcp-tools.json has a corresponding
 * documentation page generated in src/pages/docs/tools/.
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');

const manifestPath = path.join(ROOT, 'generated', 'mcp-tools.json');
const pagesDir = path.join(ROOT, 'src', 'pages', 'docs', 'tools');

let errors = 0;

console.log('Validating docs coverage...\n');

const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf-8'));
const tools = manifest.tools.granular;

const existingPages = fs.readdirSync(pagesDir)
  .filter(f => f.endsWith('.astro'))
  .map(f => f.replace('.astro', ''));

const categoryPages = ['core', 'intelligence', 'session', 'memory', 'workflow', 'analysis'];

for (const tool of tools) {
  const slug = tool.name.replace(/_/g, '-');
  if (existingPages.includes(slug)) {
    console.log(`  OK: ${tool.name} → /docs/tools/${slug}`);
  } else {
    console.error(`  MISSING: ${tool.name} has no page at /docs/tools/${slug}`);
    errors++;
  }
}

console.log(`\nCategory pages:`);
for (const cat of categoryPages) {
  if (existingPages.includes(cat)) {
    console.log(`  OK: /docs/tools/${cat}`);
  } else {
    console.error(`  MISSING: /docs/tools/${cat}`);
    errors++;
  }
}

console.log(`\n${tools.length} tools checked, ${categoryPages.length} categories checked`);
console.log(`${errors === 0 ? 'All tools have documentation pages' : `${errors} page(s) missing`}`);
process.exit(errors > 0 ? 1 : 0);
