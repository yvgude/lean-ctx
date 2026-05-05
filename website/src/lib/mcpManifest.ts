import fs from 'node:fs';
import path from 'node:path';
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
      input_schema: {
        properties?: Record<string, {
          description?: string;
          type?: string;
          enum?: string[];
          default?: unknown;
        }>;
        required?: string[];
        type?: string;
      };
    }>;
    unified: Array<{
      name: string;
      description: string;
      schema_md5: string;
      input_schema: unknown;
    }>;
  };
};

export type ToolExample = {
  title: string;
  call: string;
  description: string;
};

export type ToolEnrichment = {
  when_to_use?: string;
  when_not_to_use?: string;
  related?: string[];
  token_impact?: string;
  cli_equivalent?: string | null;
  output_contract?: string;
  code_paths?: string[];
  examples?: ToolExample[];
};

export type ToolCategory = {
  label: string;
  description: string;
  tools: string[];
};

export type ToolEnrichments = {
  categories: Record<string, ToolCategory>;
  tools: Record<string, ToolEnrichment>;
};

let cached: McpManifest | null = null;
let enrichmentsCache: ToolEnrichments | null = null;

export function getMcpManifest(): McpManifest {
  if (cached) return cached;
  const manifestPath = resolveGeneratedFile('mcp-tools.json');
  const raw = fs.readFileSync(manifestPath, 'utf-8');
  cached = JSON.parse(raw) as McpManifest;
  return cached;
}

function resolveGeneratedFile(filename: string): string {
  const viaImportMeta = fileURLToPath(new URL(`../../generated/${filename}`, import.meta.url));
  const viaCwdParent = path.resolve(process.cwd(), '..', 'generated', filename);
  const viaCwd = path.join(process.cwd(), 'generated', filename);

  // Prefer workspace-level generated assets over build output artifacts in dist/generated.
  const candidates = [viaCwdParent, viaCwd, viaImportMeta];
  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) return candidate;
  }

  throw new Error(`Cannot find generated/${filename} (tried ${candidates.join(', ')})`);
}

export function getToolEnrichments(): ToolEnrichments {
  if (enrichmentsCache) return enrichmentsCache;
  const enrichPath = resolveGeneratedFile('tool-enrichments.json');
  const raw = fs.readFileSync(enrichPath, 'utf-8');
  enrichmentsCache = JSON.parse(raw) as ToolEnrichments;
  return enrichmentsCache;
}

export function getGranularTools(): McpManifest['tools']['granular'] {
  return getMcpManifest().tools.granular;
}

export function getGranularToolByName(name: string): McpManifest['tools']['granular'][number] | undefined {
  return getMcpManifest().tools.granular.find((t) => t.name === name);
}

export function getToolEnrichmentByName(name: string): ToolEnrichment | undefined {
  return getToolEnrichments().tools[name];
}

export function getCategoryForTool(toolName: string): { key: string; category: ToolCategory } | undefined {
  const enrichments = getToolEnrichments();
  for (const [key, cat] of Object.entries(enrichments.categories)) {
    if (cat.tools.includes(toolName)) return { key, category: cat };
  }
  return undefined;
}

export function toolNameToSlug(name: string): string {
  return name.replace(/_/g, '-');
}

export function slugToToolName(slug: string): string {
  return slug.replace(/-/g, '_');
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

