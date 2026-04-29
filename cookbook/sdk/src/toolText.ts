export function toolResultToText(result: unknown): string {
  if (!result || typeof result !== "object") {
    return "";
  }

  const r = result as Record<string, unknown>;
  const content = Array.isArray(r.content) ? r.content : [];

  let out = "";
  for (const c of content) {
    if (!c || typeof c !== "object") continue;
    const cc = c as Record<string, unknown>;

    const direct = cc.text;
    if (typeof direct === "string") {
      out += direct;
      continue;
    }
    if (direct && typeof direct === "object") {
      const nested = (direct as Record<string, unknown>).text;
      if (typeof nested === "string") {
        out += nested;
        continue;
      }
    }

    // Some serializers use { type: "text", value: "..." } or similar shapes.
    if (cc.type === "text" && typeof cc.value === "string") {
      out += cc.value;
    }
  }

  if (out) return out;

  const structured = (r.structuredContent ?? r.structured_content) as unknown;
  if (structured === undefined) return "";
  if (typeof structured === "string") return structured;

  try {
    return JSON.stringify(structured, null, 2);
  } catch {
    return String(structured);
  }
}
