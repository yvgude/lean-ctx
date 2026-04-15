import fs from 'node:fs';
import { fileURLToPath } from 'node:url';

export type McpManifest = {
  schema_version: number;
  counts: { granular: number; unified: number };
  read_modes: { count: number; modes: string[] };
  tools: {
    granular: Array<{
      name: string;
      description: string;
      schema_md5: string;
      input_schema: unknown;
    }>;
    unified: Array<{
      name: string;
      description: string;
      schema_md5: string;
      input_schema: unknown;
    }>;
  };
};

let cached: McpManifest | null = null;

export function getMcpManifest(): McpManifest {
  if (cached) return cached;
  const manifestPath = fileURLToPath(new URL('../../generated/mcp-tools.json', import.meta.url));
  const raw = fs.readFileSync(manifestPath, 'utf-8');
  cached = JSON.parse(raw) as McpManifest;
  return cached;
}

export function getGranularTools(): McpManifest['tools']['granular'] {
  return getMcpManifest().tools.granular;
}

export function getGranularToolByName(name: string): McpManifest['tools']['granular'][number] | undefined {
  return getMcpManifest().tools.granular.find((t) => t.name === name);
}

export function getMcpI18nReplacements(opts?: { exampleToolCount?: number }): Record<string, string> {
  const m = getMcpManifest();
  const exampleToolCount = Math.max(0, opts?.exampleToolCount ?? 0);
  const more = Math.max(0, m.counts.granular - exampleToolCount);
  return {
    mcpToolCount: String(m.counts.granular),
    unifiedToolCount: String(m.counts.unified),
    readModeCount: String(m.read_modes.count),
    mcpToolMoreCount: String(more),
  };
}

