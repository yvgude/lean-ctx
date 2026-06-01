#!/usr/bin/env node

/**
 * Facts guard.
 *
 * Asserts the canonical product numbers live in src/lib/facts.ts and forbids the
 * specific wrong values that had drifted across the site (license, language count,
 * integration-mode count, shell-pattern count, the retired "Nine Pillars" model).
 *
 * This is intentionally a deny-list of *known-wrong* phrases rather than a fuzzy
 * number checker, so it never produces false positives on legitimate copy.
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

console.log('Validating facts SSOT...\n');

// 1) The canonical static facts must be declared in facts.ts.
const factsPath = path.join(ROOT, 'src', 'lib', 'facts.ts');
if (!fs.existsSync(factsPath)) {
  fail('src/lib/facts.ts not found');
  process.exit(1);
}
const factsSrc = fs.readFileSync(factsPath, 'utf-8');
const assertFact = (re, label) => {
  if (re.test(factsSrc)) ok(`facts.ts declares ${label}`);
  else fail(`facts.ts is missing ${label}`);
};
assertFact(/languageCount:\s*18\b/, 'languageCount = 18');
assertFact(/license:\s*'Apache-2\.0'/, "license = 'Apache-2.0'");
assertFact(/roleCount:\s*5\b/, 'roleCount = 5');
assertFact(/integrationModeCount:\s*3\b/, 'integrationModeCount = 3');
assertFact(/shellPatterns:\s*'95\+'/, "shellPatterns = '95+'");

// 2) Forbidden, known-wrong phrases on rendered surfaces.
const FORBIDDEN = [
  [/MIT \+ Apache/i, '"MIT + Apache" license (canonical: Apache-2.0)'],
  [/\b21 languages\b/i, '"21 languages" (canonical: 18)'],
  [/\b12 languages\b/i, '"12 languages" (canonical: 18)'],
  [/Nine [Pp]illars/, '"Nine Pillars" (retired model)'],
  [/\bTwo integration modes\b/i, '"Two integration modes" (canonical: three)'],
  [/60\+\s*shell[- ]?patterns/i, '"60+ shell patterns" (canonical: 95+)'],
  [/60\+\s*compression patterns/i, '"60+ compression patterns" (canonical: 95+)'],
];

const targets = [['en.json', fs.readFileSync(path.join(ROOT, 'src', 'i18n', 'locales', 'en.json'), 'utf-8')]];
for (const f of ['llms.txt', 'ai.txt', 'llms-full.txt', 'site.webmanifest']) {
  const p = path.join(ROOT, 'public', f);
  if (fs.existsSync(p)) targets.push([`public/${f}`, fs.readFileSync(p, 'utf-8')]);
}

let hits = 0;
for (const [name, txt] of targets) {
  for (const [re, label] of FORBIDDEN) {
    if (re.test(txt)) {
      fail(`${name} contains ${label}`);
      hits++;
    }
  }
}
if (hits === 0) ok(`no forbidden facts across ${targets.length} files`);

console.log(`\n${errors === 0 ? 'All facts checks passed' : `${errors} check(s) failed`}`);
process.exit(errors > 0 ? 1 : 0);
