import type React from "react";
import { useMemo, useState } from "react";

import { LeanCtxHttpError } from "@leanctx/sdk";

import { createLeanCtxClient } from "../lib/client.js";
import { parseRecallFacts, type KnowledgeFactRow } from "../lib/facts.js";
import { MermaidView } from "./MermaidView.js";

const LS_TOKEN = "leanctx.graphExplorer.bearerToken";
const LS_CATEGORY = "leanctx.graphExplorer.category";

function loadLocalStorage(key: string): string {
  try {
    return localStorage.getItem(key) ?? "";
  } catch {
    return "";
  }
}

function saveLocalStorage(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // ignore
  }
}

function formatError(e: unknown): string {
  if (e instanceof LeanCtxHttpError) return e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}

export function App(): React.ReactElement {
  const [bearerToken, setBearerToken] = useState<string>(() =>
    loadLocalStorage(LS_TOKEN)
  );
  const [category, setCategory] = useState<string>(
    () => loadLocalStorage(LS_CATEGORY) || "cookbook"
  );
  const [facts, setFacts] = useState<KnowledgeFactRow[]>([]);
  const [selected, setSelected] = useState<KnowledgeFactRow | null>(null);
  const [diagram, setDiagram] = useState<string>("");
  const [rawRecall, setRawRecall] = useState<string>("");
  const [busy, setBusy] = useState<boolean>(false);
  const [error, setError] = useState<string>("");

  const client = useMemo(
    () => createLeanCtxClient({ bearerToken: bearerToken.trim() || undefined }),
    [bearerToken]
  );

  async function loadFacts(): Promise<void> {
    setBusy(true);
    setError("");
    setDiagram("");

    try {
      saveLocalStorage(LS_TOKEN, bearerToken);
      saveLocalStorage(LS_CATEGORY, category);

      const txt = await client.callToolText("ctx_knowledge", {
        action: "recall",
        category: category.trim(),
      });
      setRawRecall(txt);

      const parsed = parseRecallFacts(txt);
      setFacts(parsed);

      if (selected) {
        const stillThere = parsed.find(
          (f) => f.category === selected.category && f.key === selected.key
        );
        setSelected(stillThere ?? null);
      }
    } catch (e) {
      setError(formatError(e));
      setFacts([]);
      setSelected(null);
      setRawRecall("");
    } finally {
      setBusy(false);
    }
  }

  async function loadDiagramFor(fact: KnowledgeFactRow): Promise<void> {
    setBusy(true);
    setError("");
    setDiagram("");

    try {
      const mermaid = await client.callToolText("ctx_knowledge", {
        action: "relations_diagram",
        category: fact.category,
        key: fact.key,
        query: "all",
      });
      setDiagram(mermaid);
    } catch (e) {
      setError(formatError(e));
      setDiagram("");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="container">
      <h2>LeanCTX Knowledge Graph Explorer</h2>
      <p className="muted">
        Dev proxy: <span className="factKey">/leanctx</span> →{" "}
        <span className="factKey">VITE_LEANCTX_BASE_URL</span> (default{" "}
        <span className="factKey">http://127.0.0.1:8080</span>)
      </p>

      {error ? (
        <div className="card error" style={{ marginBottom: 16 }}>
          {error}
        </div>
      ) : null}

      <div className="row">
        <div className="card">
          <div style={{ display: "grid", gap: 10 }}>
            <label>
              <div className="muted">Bearer token (optional)</div>
              <input
                className="input"
                value={bearerToken}
                onChange={(e) => setBearerToken(e.target.value)}
                placeholder="e.g. your --auth-token"
              />
            </label>

            <label>
              <div className="muted">Category</div>
              <input
                className="input"
                value={category}
                onChange={(e) => setCategory(e.target.value)}
                placeholder="cookbook"
              />
            </label>

            <button
              type="button"
              className="btn"
              disabled={busy || !category.trim()}
              onClick={() => void loadFacts()}
            >
              {busy ? "Loading…" : "Load facts"}
            </button>

            <div className="muted">
              Tipp: Erzeuge zuerst Facts via{" "}
              <span className="factKey">npm run memory-playground</span>{" "}
              (Cookbook Root).
            </div>
          </div>

          <hr style={{ margin: "16px 0" }} />

          <div className="muted" style={{ marginBottom: 8 }}>
            Facts ({facts.length})
          </div>
          <ul className="facts">
            {facts.map((f) => {
              const isSelected =
                selected?.category === f.category && selected?.key === f.key;
              return (
                <li key={`${f.category}/${f.key}`}>
                  <button
                    type="button"
                    className={`fact ${isSelected ? "factSelected" : ""}`}
                    onClick={() => {
                      setSelected(f);
                      void loadDiagramFor(f);
                    }}
                  >
                    <div className="factKey">
                      [{f.category}/{f.key}] — quality {f.qualityPct}%
                    </div>
                    <div className="factValue">{f.value}</div>
                  </button>
                </li>
              );
            })}
          </ul>
        </div>

        <div className="card">
          <div
            style={{
              display: "flex",
              alignItems: "center",
              gap: 10,
              marginBottom: 10,
            }}
          >
            <div className="muted">
              Diagram{" "}
              {selected ? (
                <span className="factKey">
                  [{selected.category}/{selected.key}]
                </span>
              ) : null}
            </div>
            <div style={{ flex: 1 }} />
            <button
              type="button"
              className="btn"
              disabled={busy || !selected}
              onClick={() =>
                selected ? void loadDiagramFor(selected) : undefined
              }
            >
              Refresh
            </button>
          </div>

          <MermaidView code={diagram} />

          {!facts.length && rawRecall ? (
            <>
              <hr style={{ margin: "16px 0" }} />
              <div className="muted">Raw recall output</div>
              <pre style={{ whiteSpace: "pre-wrap" }}>{rawRecall}</pre>
            </>
          ) : null}
        </div>
      </div>
    </div>
  );
}
