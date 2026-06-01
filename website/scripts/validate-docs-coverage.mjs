#!/usr/bin/env node

/**
 * Docs coverage guard (redirect-era).
 *
 * The site no longer generates one standalone page per tool, and there is no
 * [locale] route tree (locales collapsed to en-only). Per-tool *reachability* is
 * enforced by validate-manifest.mjs (page or redirect). This guard checks the
 * complementary half:
 *   - every category hub page exists, and
 *   - every /docs/tools/<slug> redirect points at a hub page that actually exists
 *     (no dangling redirects).
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const toolsPagesDir = path.join(ROOT, 'src', 'pages', 'docs', 'tools');
const astroConfig = fs.readFileSync(path.join(ROOT, 'astro.config.mjs'), 'utf-8');

let errors = 0;
const fail = (m) => {
  console.error(`  FAIL: ${m}`);
  errors++;
};
const ok = (m) => console.log(`  OK: ${m}`);

console.log('Validating docs coverage (category hubs + redirect targets)...\n');

const pageExists = (rel) =>
  fs.existsSync(path.join(ROOT, 'src', 'pages', `${rel}.astro`)) ||
  fs.existsSync(path.join(ROOT, 'src', 'pages', rel, 'index.astro'));

const categoryPages = ['core', 'intelligence', 'session', 'memory', 'workflow', 'analysis'];
for (const cat of categoryPages) {
  if (fs.existsSync(path.join(toolsPagesDir, `${cat}.astro`))) ok(`category hub /docs/tools/${cat}`);
  else fail(`missing category hub /docs/tools/${cat}`);
}

const redirects = [
  ...astroConfig.matchAll(/['"](\/docs\/tools\/[a-z0-9-]+)\/?['"]\s*:\s*['"]([^'"]+)['"]/g),
];
let dangling = 0;
for (const [, from, to] of redirects) {
  const rel = to.replace(/^\//, '').replace(/\/+$/, '');
  if (!pageExists(rel)) {
    fail(`redirect ${from} → ${to} has no target page`);
    dangling++;
  }
}
if (dangling === 0) ok(`all ${redirects.length} tool redirects resolve to a real page`);

console.log(`\n${errors === 0 ? 'All docs-coverage checks passed' : `${errors} check(s) failed`}`);
process.exit(errors > 0 ? 1 : 0);
