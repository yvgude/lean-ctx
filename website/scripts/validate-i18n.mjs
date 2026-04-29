#!/usr/bin/env node

/**
 * CI validation: enforces locale completeness and key parity.
 * - Every locale declared in src/i18n/config.ts must have a JSON file
 * - No undeclared locale files are allowed
 * - Every locale must have exactly the same keys as en.json
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const localesDir = path.join(ROOT, 'src', 'i18n', 'locales');
const configPath = path.join(ROOT, 'src', 'i18n', 'config.ts');

let errors = 0;

function fail(message) {
  console.error(`  FAIL: ${message}`);
  errors++;
}

function ok(message) {
  console.log(`  OK: ${message}`);
}

function parseConfiguredLocales() {
  if (!fs.existsSync(configPath)) {
    fail(`config file not found: ${configPath}`);
    return [];
  }
  const src = fs.readFileSync(configPath, 'utf-8');
  const match = src.match(/export const locales\s*=\s*\[([\s\S]*?)\]\s*as const/);
  if (!match) {
    fail('could not parse locales array in src/i18n/config.ts');
    return [];
  }
  const locales = [...match[1].matchAll(/'([^']+)'/g)].map((m) => m[1]);
  if (locales.length === 0) {
    fail('no locales configured in src/i18n/config.ts');
    return [];
  }
  return locales;
}

console.log('Validating i18n locale files...\n');

if (!fs.existsSync(localesDir)) {
  fail(`locales dir not found: ${localesDir}`);
  process.exit(1);
}

const configuredLocales = parseConfiguredLocales();
const configuredSet = new Set(configuredLocales);

if (!configuredSet.has('en')) {
  fail("default locale 'en' missing in src/i18n/config.ts");
}

const localeFiles = fs.readdirSync(localesDir).filter((f) => f.endsWith('.json'));
const fileLocales = localeFiles.map((f) => f.replace('.json', ''));
const fileSet = new Set(fileLocales);

for (const locale of configuredLocales) {
  if (!fileSet.has(locale)) {
    fail(`missing locale file: ${locale}.json (declared in config.ts)`);
  }
}

for (const locale of fileLocales) {
  if (!configuredSet.has(locale)) {
    fail(`undeclared locale file: ${locale}.json (not present in config.ts locales)`);
  }
}

const enPath = path.join(localesDir, 'en.json');
if (!fs.existsSync(enPath)) {
  fail('en.json not found');
  process.exit(1);
}

const enKeys = Object.keys(JSON.parse(fs.readFileSync(enPath, 'utf-8')));
ok(`English locale baseline: ${enKeys.length} keys`);

for (const locale of configuredLocales) {
  const localePath = path.join(localesDir, `${locale}.json`);
  if (!fs.existsSync(localePath)) continue;
  if (locale === 'en') continue;

  const localeKeys = new Set(Object.keys(JSON.parse(fs.readFileSync(localePath, 'utf-8'))));
  const missing = enKeys.filter(k => !localeKeys.has(k));
  const extra = [...localeKeys].filter(k => !enKeys.includes(k));

  if (missing.length > 0) {
    fail(`[${locale}] missing ${missing.length} key(s) vs en.json`);
  }
  if (extra.length > 0) {
    fail(`[${locale}] has ${extra.length} extra key(s) vs en.json`);
  }
  if (missing.length === 0 && extra.length === 0) {
    ok(`[${locale}] ${localeKeys.size} keys — complete`);
  } else {
    // Keep one-line visibility of key counts even on failure.
    console.log(`  INFO: [${locale}] ${localeKeys.size} keys (${missing.length} missing, ${extra.length} extra)`);
  }
}

console.log(`\nConfigured locales: ${configuredLocales.length}`);
console.log(`Locale files: ${fileLocales.length}`);
console.log(errors === 0 ? 'All locale checks passed' : `${errors} locale check(s) failed`);
process.exit(errors > 0 ? 1 : 0);
