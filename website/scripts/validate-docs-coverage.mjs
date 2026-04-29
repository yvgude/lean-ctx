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
const rootPagesDir = path.join(ROOT, 'src', 'pages', 'docs', 'tools');
const localePagesDir = path.join(ROOT, 'src', 'pages', '[locale]', 'docs', 'tools');

let errors = 0;

console.log('Validating docs coverage...\n');

const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf-8'));
const tools = manifest.tools.granular;

const existingRootPages = fs.readdirSync(rootPagesDir)
  .filter(f => f.endsWith('.astro'))
  .map(f => f.replace('.astro', ''));

const existingLocalePages = fs.readdirSync(localePagesDir)
  .filter(f => f.endsWith('.astro'))
  .map(f => f.replace('.astro', ''));

const categoryPages = ['core', 'intelligence', 'session', 'memory', 'workflow', 'analysis'];

for (const tool of tools) {
  const slug = tool.name.replace(/_/g, '-');
  const hasRootPage = existingRootPages.includes(slug);
  const hasLocalePage = existingLocalePages.includes(slug);

  if (hasRootPage && hasLocalePage) {
    console.log(`  OK: ${tool.name} → /docs/tools/${slug} and /[locale]/docs/tools/${slug}`);
  } else {
    if (!hasRootPage) {
      console.error(`  MISSING: ${tool.name} has no root page at /docs/tools/${slug}`);
      errors++;
    }
    if (!hasLocalePage) {
      console.error(`  MISSING: ${tool.name} has no localized template at /[locale]/docs/tools/${slug}`);
      errors++;
    }
  }
}

console.log(`\nCategory pages:`);
for (const cat of categoryPages) {
  const hasRootPage = existingRootPages.includes(cat);
  const hasLocalePage = existingLocalePages.includes(cat);

  if (hasRootPage && hasLocalePage) {
    console.log(`  OK: /docs/tools/${cat} and /[locale]/docs/tools/${cat}`);
  } else {
    if (!hasRootPage) {
      console.error(`  MISSING: /docs/tools/${cat}`);
      errors++;
    }
    if (!hasLocalePage) {
      console.error(`  MISSING: /[locale]/docs/tools/${cat}`);
      errors++;
    }
  }
}

const extraLocaleOnly = existingLocalePages.filter(name => !existingRootPages.includes(name));
const extraRootOnly = existingRootPages.filter(name => !existingLocalePages.includes(name));
if (extraLocaleOnly.length > 0) {
  console.error(`\nMISMATCH: locale-only pages without root equivalent: ${extraLocaleOnly.join(', ')}`);
  errors++;
}
if (extraRootOnly.length > 0) {
  console.error(`\nMISMATCH: root-only pages without locale equivalent: ${extraRootOnly.join(', ')}`);
  errors++;
}

console.log(`\n${tools.length} tools checked, ${categoryPages.length} categories checked`);
console.log(`${errors === 0 ? 'All tool docs are covered for root + locale routes' : `${errors} coverage check(s) failed`}`);
process.exit(errors > 0 ? 1 : 0);
