#!/usr/bin/env node

/**
 * CI validation: checks i18n key consistency across locale files.
 * Reports missing and extra keys vs the English (default) locale.
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const localesDir = path.join(ROOT, 'src', 'i18n', 'locales');

let warnings = 0;
let errors = 0;

console.log('Validating i18n locale files...\n');

const enPath = path.join(localesDir, 'en.json');
if (!fs.existsSync(enPath)) {
  console.error('FAIL: en.json not found');
  process.exit(1);
}

const enKeys = Object.keys(JSON.parse(fs.readFileSync(enPath, 'utf-8')));
console.log(`  English locale: ${enKeys.length} keys`);

const files = fs.readdirSync(localesDir).filter(f => f.endsWith('.json') && f !== 'en.json');

for (const file of files) {
  const locale = file.replace('.json', '');
  const localePath = path.join(localesDir, file);
  const localeKeys = new Set(Object.keys(JSON.parse(fs.readFileSync(localePath, 'utf-8'))));

  const missing = enKeys.filter(k => !localeKeys.has(k));
  const extra = [...localeKeys].filter(k => !enKeys.includes(k));

  if (missing.length > 0) {
    warnings += missing.length;
    console.log(`  [${locale}] ${localeKeys.size} keys, ${missing.length} missing (fallback to en)`);
  } else if (extra.length > 0) {
    console.log(`  [${locale}] ${localeKeys.size} keys, ${extra.length} extra`);
  } else {
    console.log(`  [${locale}] ${localeKeys.size} keys — complete`);
  }
}

console.log(`\n${files.length + 1} locale files checked, ${warnings} missing keys (all have en fallback)`);
if (errors > 0) {
  console.error(`${errors} error(s) found`);
  process.exit(1);
}
