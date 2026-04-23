#!/usr/bin/env node

/**
 * CI validation: detects duplicate i18n key trees where both camelCase
 * (docsXxx.yyy) and dot-notation (docs.xxx.yyy) variants exist.
 *
 * Known legacy duplicates (warn only, don't fail):
 *   docsAnalytics, docsCliReference, docsGettingStarted,
 *   docsQuickReference, docsConfiguration
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const enPath = path.join(ROOT, 'src', 'i18n', 'locales', 'en.json');

const KNOWN_LEGACY = new Set([
  'docsAnalytics',
  'docsCliReference',
  'docsGettingStarted',
  'docsQuickReference',
  'docsConfiguration',
]);

console.log('Checking for duplicate i18n key trees...\n');

if (!fs.existsSync(enPath)) {
  console.error('FAIL: en.json not found');
  process.exit(1);
}

const keys = Object.keys(JSON.parse(fs.readFileSync(enPath, 'utf-8')));

// Build prefix sets: dot-notation trees (docs.xxx) and camelCase trees (docsXxx)
const dotPrefixes = new Set();
const camelPrefixes = new Set();

for (const key of keys) {
  const dotMatch = key.match(/^docs\.([a-zA-Z]+)\./);
  if (dotMatch) {
    dotPrefixes.add(dotMatch[1]);
  }

  const camelMatch = key.match(/^docs([A-Z][a-zA-Z]*)\./);
  if (camelMatch) {
    camelPrefixes.add(`docs${camelMatch[1]}`);
  }
}

// Find overlapping trees: camelCase prefix whose lowercase variant exists in dot-notation
let warnings = 0;
let newDuplicates = 0;

for (const camelPrefix of camelPrefixes) {
  const suffix = camelPrefix.replace(/^docs/, '');
  const dotVariant = suffix.charAt(0).toLowerCase() + suffix.slice(1);

  if (!dotPrefixes.has(dotVariant)) continue;

  const camelKeys = keys.filter(k => k.startsWith(`${camelPrefix}.`));
  const dotKeys = keys.filter(k => k.startsWith(`docs.${dotVariant}.`));

  if (KNOWN_LEGACY.has(camelPrefix)) {
    console.warn(`  WARN [legacy]: "${camelPrefix}.*" (${camelKeys.length} keys) duplicates "docs.${dotVariant}.*" (${dotKeys.length} keys)`);
    warnings++;
  } else {
    console.warn(`  WARN [new]: "${camelPrefix}.*" (${camelKeys.length} keys) duplicates "docs.${dotVariant}.*" (${dotKeys.length} keys)`);
    warnings++;
    newDuplicates++;
  }
}

// Report camelCase prefixes without dot-notation counterpart (unused legacy trees)
for (const camelPrefix of camelPrefixes) {
  const suffix = camelPrefix.replace(/^docs/, '');
  const dotVariant = suffix.charAt(0).toLowerCase() + suffix.slice(1);

  if (dotPrefixes.has(dotVariant)) continue;
  if (!KNOWN_LEGACY.has(camelPrefix)) continue;

  const count = keys.filter(k => k.startsWith(`${camelPrefix}.`)).length;
  console.warn(`  WARN [orphan]: "${camelPrefix}.*" (${count} keys) — camelCase tree with no dot-notation counterpart`);
  warnings++;
}

console.log(`\n${warnings} duplicate/orphan key tree(s) found`);

if (newDuplicates > 0) {
  console.warn(`${newDuplicates} NEW duplicate(s) detected — consider migrating to docs.xxx notation`);
}

// Legacy duplicates are expected — never fail the build for them
console.log('(legacy duplicates are expected and do not fail the build)');
