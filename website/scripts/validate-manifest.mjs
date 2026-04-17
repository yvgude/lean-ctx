import fs from 'node:fs';
import path from 'node:path';

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
const MANIFEST_PATH = path.join(ROOT, 'generated', 'mcp-tools.json');

function readJson(filePath) {
  const raw = fs.readFileSync(filePath, 'utf-8');
  try {
    return JSON.parse(raw);
  } catch (e) {
    throw new Error(`Failed to parse JSON: ${filePath}\n${e?.message ?? e}`);
  }
}

function assert(cond, msg) {
  if (!cond) throw new Error(msg);
}

function main() {
  assert(fs.existsSync(MANIFEST_PATH), `Missing manifest: ${MANIFEST_PATH} (run gen_mcp_manifest)`);

  const m = readJson(MANIFEST_PATH);
  assert(typeof m === 'object' && m, 'manifest: expected object');
  assert(typeof m.schema_version === 'number', 'manifest: schema_version missing');

  assert(m.counts && typeof m.counts.granular === 'number', 'manifest: counts.granular missing');
  assert(m.counts && typeof m.counts.unified === 'number', 'manifest: counts.unified missing');
  assert(m.read_modes && typeof m.read_modes.count === 'number', 'manifest: read_modes.count missing');
  assert(Array.isArray(m.read_modes?.modes), 'manifest: read_modes.modes missing');

  assert(Array.isArray(m.tools?.granular), 'manifest: tools.granular missing');
  assert(Array.isArray(m.tools?.unified), 'manifest: tools.unified missing');
  assert(m.counts.granular === m.tools.granular.length, 'manifest: counts.granular != tools.granular.length');
  assert(m.counts.unified === m.tools.unified.length, 'manifest: counts.unified != tools.unified.length');
  assert(m.read_modes.count === m.read_modes.modes.length, 'manifest: read_modes.count != read_modes.modes.length');

  console.log('manifest ok');
}

main();

