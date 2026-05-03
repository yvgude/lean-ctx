#!/usr/bin/env node
/**
 * Update marketing/documentation counts across website sources.
 *
 * - "90+ compression patterns" -> "95+ compression patterns"
 * - tool manifest descriptions: "compressed output, 90+ patterns" -> "..., 95+ patterns"
 * - "51 Pattern Modules" -> "55 Pattern Modules"
 *
 * This script is intentionally conservative: it updates only known phrases.
 */
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');

const TEXT_EXTS = new Set(['.astro', '.md', '.json', '.txt', '.html', '.webmanifest']);

function walk(dir, out = []) {
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  for (const e of entries) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, out);
    else out.push(p);
  }
  return out;
}

function applyReplacements(s) {
  const rules = [
    // Core phrasing used across docs
    ['90+ compression patterns', '95+ compression patterns'],
    ['90+ CLI compression patterns', '95+ CLI compression patterns'],
    ['90+ CLI command patterns', '95+ CLI command patterns'],
    ['90+ command patterns', '95+ command patterns'],
    ['SHELL HOOK PATTERNS (90+):', 'SHELL HOOK PATTERNS (95+):'],
    ['Pattern-compressed (90+)', 'Pattern-compressed (95+)'],

    // Tool manifest descriptions
    ['compressed output, 90+ patterns', 'compressed output, 95+ patterns'],
    ['compressed output, 90+ pattern', 'compressed output, 95+ pattern'],

    // Shell patterns concept page
    ['51 Pattern Modules', '55 Pattern Modules'],
    ['51 pattern modules', '55 pattern modules'],
    ['<strong>51 pattern modules</strong>', '<strong>55 pattern modules</strong>'],
    ['<strong>51 pattern modules</strong>', '<strong>55 pattern modules</strong>'],
    ['<strong>51 pattern modules</strong> covering', '<strong>55 pattern modules</strong> covering'],
    ['<strong>51 pattern modules</strong> covering 90+ developer', '<strong>55 pattern modules</strong> covering 90+ developer'],
  ];

  let out = s;
  for (const [from, to] of rules) out = out.split(from).join(to);
  return out;
}

function updateFile(filePath) {
  const ext = path.extname(filePath);
  if (!TEXT_EXTS.has(ext)) return { changed: false };
  const before = fs.readFileSync(filePath, 'utf8');
  const after = applyReplacements(before);
  if (after === before) return { changed: false };
  fs.writeFileSync(filePath, after, 'utf8');
  return { changed: true };
}

const targets = [
  path.join(ROOT, 'src'),
  path.join(ROOT, 'generated', 'mcp-tools.json'),
  path.join(ROOT, 'public'),
  path.join(ROOT, 'dist'),
];

let changedFiles = 0;
let scannedFiles = 0;

for (const t of targets) {
  const stat = fs.existsSync(t) ? fs.statSync(t) : null;
  if (!stat) continue;
  const files = stat.isDirectory() ? walk(t) : [t];
  for (const f of files) {
    scannedFiles++;
    const res = updateFile(f);
    if (res.changed) changedFiles++;
  }
}

console.log(`Scanned ${scannedFiles} files, updated ${changedFiles}.`);
