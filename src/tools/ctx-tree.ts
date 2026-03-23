import { readdir, stat } from 'node:fs/promises';
import { join, resolve, relative } from 'node:path';
import { z } from 'zod';
import type { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import type { SessionCache } from '../core/session-cache.js';
import { trackSavings } from '../core/token-counter.js';
import { shortenPath } from '../core/protocol.js';

const DEFAULT_IGNORE = [
  'node_modules', '.git', 'dist', 'build', '.svelte-kit',
  '__pycache__', '.next', '.nuxt', '.output', 'coverage',
  '.turbo', '.cache', '.parcel-cache',
];

interface TreeEntry {
  name: string;
  isDir: boolean;
  size?: number;
  lineCount?: number;
  children?: TreeEntry[];
  fileCount?: number;
}

export function registerCtxTree(server: McpServer, cache: SessionCache): void {
  server.tool(
    'ctx_tree',
    'Compact project structure map. Returns a compressed directory tree with file counts and sizes. Much more token-efficient than running ls or find commands.',
    {
      path: z.string().optional().describe('Root directory path (default: LEAN_CTX_ROOT or cwd)'),
      depth: z.number().optional().describe('Max directory depth (default: 3)'),
      show_sizes: z.boolean().optional().describe('Show file sizes and line counts (default: false)'),
      ignore: z.array(z.string()).optional().describe('Additional patterns to ignore'),
    },
    async ({ path: rootPath, depth = 3, show_sizes = false, ignore = [] }) => {
      const root = resolve(rootPath || process.env.LEAN_CTX_ROOT || process.cwd());
      const ignoreSet = new Set([...DEFAULT_IGNORE, ...ignore]);

      try {
        const tree = await buildTree(root, root, depth, ignoreSet, show_sizes);
        const compactOutput = renderCompact(tree, 0);
        const verboseOutput = renderTree(tree, '', true);
        const savings = trackSavings(verboseOutput, compactOutput);

        return {
          content: [{ type: 'text' as const, text: compactOutput + (savings ? `\n${savings}` : '') }],
        };
      } catch (err) {
        return {
          content: [
            {
              type: 'text' as const,
              text: `Error building tree: ${err instanceof Error ? err.message : String(err)}`,
            },
          ],
          isError: true,
        };
      }
    }
  );
}

async function buildTree(
  fullPath: string,
  rootPath: string,
  maxDepth: number,
  ignoreSet: Set<string>,
  showSizes: boolean,
  currentDepth = 0
): Promise<TreeEntry> {
  const name = currentDepth === 0
    ? relative(join(rootPath, '..'), fullPath) || fullPath
    : fullPath.split('/').pop() || fullPath;

  const info = await stat(fullPath);

  if (!info.isDirectory()) {
    const entry: TreeEntry = { name, isDir: false };
    if (showSizes) {
      entry.size = info.size;
      entry.lineCount = await countLines(fullPath);
    }
    return entry;
  }

  const entry: TreeEntry = { name, isDir: true, children: [] };

  if (currentDepth >= maxDepth) {
    entry.fileCount = await countFilesRecursive(fullPath, ignoreSet);
    return entry;
  }

  const items = await readdir(fullPath, { withFileTypes: true });
  const sorted = items
    .filter((item) => !ignoreSet.has(item.name) && !item.name.startsWith('.'))
    .sort((a, b) => {
      if (a.isDirectory() && !b.isDirectory()) return -1;
      if (!a.isDirectory() && b.isDirectory()) return 1;
      return a.name.localeCompare(b.name);
    });

  for (const item of sorted) {
    const childPath = join(fullPath, item.name);
    const child = await buildTree(childPath, rootPath, maxDepth, ignoreSet, showSizes, currentDepth + 1);
    entry.children!.push(child);
  }

  return entry;
}

function renderCompact(entry: TreeEntry, depth: number): string {
  const indent = '  '.repeat(depth);
  const lines: string[] = [];

  if (depth === 0) {
    const shortName = shortenPath(entry.name) || entry.name;
    lines.push(`${shortName}/`);
  }

  if (entry.isDir && entry.children) {
    for (const child of entry.children) {
      if (child.isDir) {
        const count = child.fileCount !== undefined
          ? ` (${child.fileCount})`
          : child.children?.length === 0
            ? ' (0)'
            : '';
        lines.push(`${indent}${child.name}/${count}`);

        if (child.children && child.children.length > 0) {
          lines.push(renderCompact(child, depth + 1));
        }
      } else {
        const meta: string[] = [];
        if (child.lineCount !== undefined) meta.push(`${child.lineCount}L`);
        if (child.size !== undefined) meta.push(formatSize(child.size));
        const suffix = meta.length > 0 ? ` [${meta.join(' ')}]` : '';
        lines.push(`${indent}${child.name}${suffix}`);
      }
    }
  } else if (entry.isDir && entry.fileCount !== undefined) {
    lines.push(`${indent}(${entry.fileCount} files)`);
  }

  return lines.join('\n');
}

function renderTree(entry: TreeEntry, prefix: string, isRoot: boolean): string {
  const lines: string[] = [];

  if (isRoot) {
    lines.push(`${entry.name}/`);
  }

  if (entry.isDir && entry.children) {
    for (let i = 0; i < entry.children.length; i++) {
      const child = entry.children[i];
      const isLast = i === entry.children.length - 1;
      const connector = isLast ? '└── ' : '├── ';
      const childPrefix = isLast ? '    ' : '│   ';

      if (child.isDir) {
        const suffix = child.fileCount !== undefined
          ? ` (${child.fileCount} files)`
          : child.children?.length === 0
            ? ' (empty)'
            : '';
        lines.push(`${prefix}${connector}${child.name}/${suffix}`);

        if (child.children && child.children.length > 0) {
          lines.push(renderTree(child, prefix + childPrefix, false));
        }
      } else {
        const meta: string[] = [];
        if (child.lineCount !== undefined) meta.push(`${child.lineCount} lines`);
        if (child.size !== undefined) meta.push(formatSize(child.size));
        const suffix = meta.length > 0 ? ` [${meta.join(', ')}]` : '';
        lines.push(`${prefix}${connector}${child.name}${suffix}`);
      }
    }
  } else if (entry.isDir && entry.fileCount !== undefined) {
    lines.push(`${prefix}  (${entry.fileCount} files)`);
  }

  return lines.join('\n');
}

async function countLines(filePath: string): Promise<number> {
  try {
    const { readFile } = await import('node:fs/promises');
    const content = await readFile(filePath, 'utf-8');
    return content.split('\n').length;
  } catch {
    return 0;
  }
}

async function countFilesRecursive(dirPath: string, ignoreSet: Set<string>): Promise<number> {
  let count = 0;
  try {
    const items = await readdir(dirPath, { withFileTypes: true });
    for (const item of items) {
      if (ignoreSet.has(item.name) || item.name.startsWith('.')) continue;
      if (item.isDirectory()) {
        count += await countFilesRecursive(join(dirPath, item.name), ignoreSet);
      } else {
        count++;
      }
    }
  } catch {
    // ignore permission errors
  }
  return count;
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
}
