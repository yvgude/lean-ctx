#!/usr/bin/env node

/**
 * Positioning guard.
 *
 * Enforces the one canonical top-level story across the site and forbids the
 * retired headlines that used to compete with it ("Context OS", "Context Runtime",
 * "Cognitive Filter", "Intelligence Buffer", "Context Engineering Layer").
 *
 * Single source of truth: src/lib/positioning.ts. This script parses that module
 * as text (no TS runtime needed) so the canonical strings can only ever be edited
 * in one place.
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');

let errors = 0;
const fail = (m) => {
  console.error(`  FAIL: ${m}`);
  errors++;
};
const ok = (m) => console.log(`  OK: ${m}`);

console.log('Validating positioning SSOT...\n');

const posPath = path.join(ROOT, 'src', 'lib', 'positioning.ts');
if (!fs.existsSync(posPath)) {
  fail('src/lib/positioning.ts not found');
  process.exit(1);
}
const posSrc = fs.readFileSync(posPath, 'utf-8');

const extractArray = (name) => {
  const m = posSrc.match(new RegExp(`export const ${name}\\s*=\\s*\\[([\\s\\S]*?)\\]`));
  return m ? [...m[1].matchAll(/['"]([^'"]+)['"]/g)].map((x) => x[1]) : [];
};
const RETIRED = extractArray('RETIRED_HEADLINES');
const ALLOWED = extractArray('ALLOWED_FEATURE_TERMS');
const CONCEPT = (posSrc.match(/export const CONCEPT\s*=\s*['"]([^'"]+)['"]/) || [])[1];
const HEADLINE = (posSrc.match(/export const HEADLINE\s*=\s*`([^`]+)`/) || [])[1];

// 1) Canonical strings present and correct.
if (CONCEPT === 'Cognitive Context Layer') ok('CONCEPT = "Cognitive Context Layer"');
else fail(`CONCEPT must be "Cognitive Context Layer" (got "${CONCEPT}")`);

if (HEADLINE && /Cognitive Context Layer/.test(HEADLINE)) ok('HEADLINE contains the concept');
else fail('HEADLINE must contain "Cognitive Context Layer"');

if (RETIRED.length > 0) ok(`${RETIRED.length} retired headlines defined`);
else fail('RETIRED_HEADLINES must not be empty');

// Strip legitimate sub-feature names (e.g. "Cognitive Efficiency Protocol") before
// scanning, so they are never mistaken for a retired top-level headline.
const stripAllowed = (s) => {
  let out = s;
  for (const a of ALLOWED) out = out.split(a).join('');
  return out;
};

// 2) No retired headline may appear in marketing/nav strings in en.json.
const en = JSON.parse(fs.readFileSync(path.join(ROOT, 'src', 'i18n', 'locales', 'en.json'), 'utf-8'));
const MARKETING = /^(index\.|footer\.|seo\.|contextOs\.|compare\.|nav\.|howItWorks\.|whatIs\.|pillar\.)/;
let scanned = 0;
let hits = 0;
for (const [key, value] of Object.entries(en)) {
  if (typeof value !== 'string' || !MARKETING.test(key)) continue;
  scanned++;
  const clean = stripAllowed(value);
  for (const r of RETIRED) {
    if (clean.includes(r)) {
      fail(`retired headline "${r}" found in en.json key "${key}"`);
      hits++;
    }
  }
}
if (hits === 0) ok(`no retired headlines across ${scanned} marketing strings`);

// 3) Hero eyebrow must surface the canonical concept.
const heroLabel = en['index.heroLabel'];
if (heroLabel && CONCEPT && heroLabel.includes(CONCEPT)) ok('index.heroLabel references the concept');
else fail(`index.heroLabel should reference "${CONCEPT}" (got "${heroLabel}")`);

// 4) Machine-readable files for LLMs must also be on-message.
for (const f of ['llms.txt', 'ai.txt', 'llms-full.txt', 'site.webmanifest']) {
  const p = path.join(ROOT, 'public', f);
  if (!fs.existsSync(p)) continue;
  const txt = stripAllowed(fs.readFileSync(p, 'utf-8'));
  for (const r of RETIRED) {
    if (txt.includes(r)) fail(`retired headline "${r}" found in public/${f}`);
  }
}

console.log(`\n${errors === 0 ? 'All positioning checks passed' : `${errors} check(s) failed`}`);
process.exit(errors > 0 ? 1 : 0);
