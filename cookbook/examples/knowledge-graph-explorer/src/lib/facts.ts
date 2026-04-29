export interface KnowledgeFactRow {
  category: string;
  key: string;
  value: string;
  qualityPct: number;
  rawLine: string;
}

export function parseRecallFacts(text: string): KnowledgeFactRow[] {
  const out: KnowledgeFactRow[] = [];

  for (const line of text.split("\n")) {
    const m = line.match(
      /^\s*\[([^/]+)\/([^\]]+)\]:\s*(.*)\s+\(quality:\s*([0-9]+)%/u
    );
    if (!m) continue;

    const [, category, key, value, qualityPctStr] = m;
    const qualityPct = Number(qualityPctStr);

    if (!category || !key) continue;
    if (!Number.isFinite(qualityPct)) continue;

    out.push({
      category,
      key,
      value: value ?? "",
      qualityPct,
      rawLine: line,
    });
  }

  return out;
}
