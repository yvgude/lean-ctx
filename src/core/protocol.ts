const fileRefs = new Map<string, string>();
let refCounter = 0;

export function getFileRef(absPath: string): string {
  const existing = fileRefs.get(absPath);
  if (existing) return existing;

  refCounter++;
  const ref = `F${refCounter}`;
  fileRefs.set(absPath, ref);
  return ref;
}

export function isFirstUse(absPath: string): boolean {
  return !fileRefs.has(absPath);
}

export function formatFileHeader(absPath: string, lineCount: number, status: FileStatus): string {
  const ref = getFileRef(absPath);
  const shortPath = shortenPath(absPath);
  const first = isFirstUse(absPath);

  if (first) {
    fileRefs.set(absPath, ref);
    return `${ref}=${shortPath} [${lineCount}L ${STATUS_SYMBOLS[status]}]`;
  }

  return `${ref} [${lineCount}L ${STATUS_SYMBOLS[status]}]`;
}

export type FileStatus = 'unchanged' | 'changed' | 'new' | 'deleted' | 'cached';

const STATUS_SYMBOLS: Record<FileStatus, string> = {
  unchanged: '∅',
  changed: 'Δ',
  new: '+',
  deleted: '-',
  cached: '⊙',
};

export function formatCacheHit(absPath: string, turnsAgo: number, lineCount: number): string {
  const ref = getFileRef(absPath);
  return `${ref} [cached ${turnsAgo}t ${lineCount}L ∅]`;
}

export function formatCompactSignature(sig: {
  type: string;
  name: string;
  signature: string;
}): string {
  const typeMap: Record<string, string> = {
    function: 'fn',
    class: 'cl',
    interface: 'if',
    type: 'tp',
    enum: 'en',
    variable: 'var',
    export: 'exp',
    import: 'imp',
    component: 'cmp',
    jsdoc: '/**',
  };

  const prefix = typeMap[sig.type] || sig.type;

  if (sig.type === 'import') return '';
  if (sig.type === 'jsdoc') return sig.signature;

  const compact = compactSignatureLine(sig.signature);
  return `${prefix} ${compact}`;
}

function compactSignatureLine(line: string): string {
  let result = line.trim();

  result = result.replace(/export\s+(default\s+)?/g, '');
  result = result.replace(/async\s+/g, '⊛ ');
  result = result.replace(/function\s+/g, '');
  result = result.replace(/:\s*Promise<([^>]+)>/g, ' →⊛ $1');
  result = result.replace(/:\s*([A-Z]\w+(?:\s*\|\s*\w+)*)/g, ' → $1');
  result = result.replace(/:\s*string/g, ':s');
  result = result.replace(/:\s*number/g, ':n');
  result = result.replace(/:\s*boolean/g, ':b');
  result = result.replace(/:\s*void/g, '');
  result = result.replace(/\s*\|\s*null/g, '?');
  result = result.replace(/\s*\|\s*undefined/g, '?');
  result = result.replace(/\s*\{\.{3}\}\s*$/g, '');

  return result;
}

export function shortenPath(absPath: string): string {
  const home = process.env.HOME || '/Users/user';
  let result = absPath;

  if (result.startsWith(home)) {
    result = '~' + result.slice(home.length);
  }

  const projectRoot = process.env.LEAN_CTX_ROOT;
  if (projectRoot && absPath.startsWith(projectRoot)) {
    result = absPath.slice(projectRoot.length + 1);
  }

  return result;
}

export function formatTokenMeta(savedTokens: number, savedPercent: number): string {
  if (savedTokens <= 0) return '';
  return `[−${savedTokens}tok ${savedPercent}%]`;
}

export function resetRefs(): void {
  fileRefs.clear();
  refCounter = 0;
}

export function getAllRefs(): Map<string, string> {
  return new Map(fileRefs);
}
