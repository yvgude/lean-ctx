#!/usr/bin/env node
// validate:i18n-usage — every t('...') key referenced in .astro/.ts templates must
// exist in en.json, so a missing translation can never render as a raw key label.
// Template-literal keys with ${...} interpolation are expanded for the known
// FAQ pattern (faq1..faq5) and otherwise reported separately for manual review.
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const srcDir = join(here, '..', 'src');
const en = JSON.parse(readFileSync(join(srcDir, 'i18n', 'locales', 'en.json'), 'utf8'));
const enKeys = new Set(Object.keys(en));

function walk(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    const st = statSync(p);
    if (st.isDirectory()) out.push(...walk(p));
    else if (name.endsWith('.astro') || name.endsWith('.ts') || name.endsWith('.tsx')) out.push(p);
  }
  return out;
}

const staticRe = /\bt\(\s*['"]([^'"]+)['"]/g;
const dynRe = /\bt\(\s*`([^`]+)`/g;

// Resolve template-literal keys whose interpolation iterates a known, fixed set.
// Returns an array of concrete keys, or null if the pattern cannot be resolved.
function expandDynamic(raw) {
  // index.faq${i}Q / index.faq${i}A  → i in 1..5
  const faq = raw.match(/^(.*)\$\{i\}(.*)$/);
  if (faq) return [1, 2, 3, 4, 5].map((i) => `${faq[1]}${i}${faq[2]}`);

  // contextPkg.layer${key.charAt(0).toUpperCase() + key.slice(1)}[Desc]
  const layer = raw.match(/^(.*)\$\{key\.charAt\(0\)\.toUpperCase\(\) \+ key\.slice\(1\)\}(.*)$/);
  if (layer) {
    return ['Knowledge', 'Graph', 'Session', 'Patterns', 'Gotchas'].map(
      (cap) => `${layer[1]}${cap}${layer[2]}`,
    );
  }
  return null;
}

const missing = new Map(); // key -> Set(files)
const dynamic = new Map();  // raw -> Set(files)

for (const file of walk(srcDir)) {
  const text = readFileSync(file, 'utf8');
  const rel = file.slice(srcDir.length + 1);
  let m;
  while ((m = staticRe.exec(text))) {
    const key = m[1];
    if (!enKeys.has(key)) {
      if (!missing.has(key)) missing.set(key, new Set());
      missing.get(key).add(rel);
    }
  }
  while ((m = dynRe.exec(text))) {
    const raw = m[1];
    if (!raw.includes('${')) {
      if (!enKeys.has(raw)) {
        if (!missing.has(raw)) missing.set(raw, new Set());
        missing.get(raw).add(rel);
      }
      continue;
    }
    // Expand known interpolation patterns to their concrete keys.
    const expansions = expandDynamic(raw);
    if (expansions) {
      for (const key of expansions) {
        if (!enKeys.has(key)) {
          if (!missing.has(key)) missing.set(key, new Set());
          missing.get(key).add(rel);
        }
      }
    } else {
      if (!dynamic.has(raw)) dynamic.set(raw, new Set());
      dynamic.get(raw).add(rel);
    }
  }
}

if (missing.size === 0) {
  console.log('OK: all referenced t() keys exist in en.json');
} else {
  console.log(`MISSING ${missing.size} key(s) from en.json:`);
  for (const [key, files] of [...missing].sort()) {
    console.log(`  - ${key}  (${[...files].join(', ')})`);
  }
}
if (dynamic.size) {
  console.log(`\nUNRESOLVED dynamic keys (manual review):`);
  for (const [raw, files] of dynamic) console.log(`  - \`${raw}\`  (${[...files].join(', ')})`);
}
process.exit(missing.size ? 1 : 0);
