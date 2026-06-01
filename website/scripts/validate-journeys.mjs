#!/usr/bin/env node

/**
 * Journey / route-coverage guard.
 *
 * The 14 journeys (journeys.ts) and the 4 persona tracks (tracks.ts) are the
 * backbone of the information architecture. Every journey and track must:
 *   - point at a page that actually resolves (no 404s), and
 *   - belong to one of the four declared tracks (journeys only).
 *
 * Reachability = a page file under src/pages, or a redirect declared in
 * astro.config.mjs. This is also a general broken-internal-route checker for the
 * curated nav, so the self-select landing can never link into the void.
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

console.log('Validating journey & track route coverage...\n');

const libDir = path.join(ROOT, 'src', 'lib');
const journeysSrc = fs.readFileSync(path.join(libDir, 'journeys.ts'), 'utf-8');
const tracksSrc = fs.readFileSync(path.join(libDir, 'tracks.ts'), 'utf-8');
const astroConfig = fs.readFileSync(path.join(ROOT, 'astro.config.mjs'), 'utf-8');
const pagesDir = path.join(ROOT, 'src', 'pages');

const extractHrefs = (src) => [...src.matchAll(/href:\s*'([^']+)'/g)].map((m) => m[1]);
const extractTrackIds = (src) =>
  [...src.matchAll(/trackId:\s*'([^']+)'/g)].map((m) => m[1]);
const declaredTrackIds = new Set([...tracksSrc.matchAll(/id:\s*'([^']+)'/g)].map((m) => m[1]));

const resolves = (href) => {
  const clean = href.replace(/\/+$/, '');
  const rel = clean.replace(/^\//, '');
  const fileCandidates = [
    path.join(pagesDir, `${rel}.astro`),
    path.join(pagesDir, rel, 'index.astro'),
  ];
  if (fileCandidates.some((p) => fs.existsSync(p))) return true;
  // Redirect declared in astro.config (with or without trailing slash).
  return new RegExp(`['"]${clean}/?['"]\\s*:`).test(astroConfig);
};

// 1) Exactly 14 journeys, all mapped to a declared track.
const journeyHrefs = extractHrefs(journeysSrc);
const journeyNums = [...journeysSrc.matchAll(/num:\s*(\d+)/g)].map((m) => Number(m[1]));
if (journeyNums.length === 14) ok('14 journeys defined');
else fail(`expected 14 journeys, found ${journeyNums.length}`);

for (const id of extractTrackIds(journeysSrc)) {
  if (!declaredTrackIds.has(id)) fail(`journey references unknown trackId "${id}"`);
}

// 2) Every journey route resolves.
let jBad = 0;
for (const href of journeyHrefs) {
  if (!resolves(href)) {
    fail(`journey route does not resolve: ${href}`);
    jBad++;
  }
}
if (jBad === 0) ok(`all ${journeyHrefs.length} journey routes resolve`);

// 3) Every track route resolves.
const trackHrefs = extractHrefs(tracksSrc);
let tBad = 0;
for (const href of trackHrefs) {
  if (!resolves(href)) {
    fail(`track route does not resolve: ${href}`);
    tBad++;
  }
}
if (tBad === 0) ok(`all ${trackHrefs.length} track routes resolve`);

console.log(`\n${errors === 0 ? 'All journey/route checks passed' : `${errors} check(s) failed`}`);
process.exit(errors > 0 ? 1 : 0);
