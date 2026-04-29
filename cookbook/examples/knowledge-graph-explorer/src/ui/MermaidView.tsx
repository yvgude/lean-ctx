import mermaid from "mermaid";
import type React from "react";
import { useEffect, useId, useMemo, useRef, useState } from "react";

let mermaidInitialized = false;

function ensureMermaidInitialized(): void {
  if (mermaidInitialized) return;
  mermaidInitialized = true;

  const prefersDark =
    window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? false;
  mermaid.initialize({
    startOnLoad: false,
    securityLevel: "strict",
    theme: prefersDark ? "dark" : "default",
  });
}

export function MermaidView(props: { code: string }): React.ReactElement {
  const [svg, setSvg] = useState<string>("");
  const [err, setErr] = useState<string>("");
  const reactId = useId();
  const containerRef = useRef<HTMLDivElement | null>(null);

  const renderId = useMemo(() => {
    const base = reactId.replace(/[^a-zA-Z0-9_-]/g, "");
    return `m_${base}_${Date.now()}`;
  }, [reactId]);

  useEffect(() => {
    const code = props.code.trim();
    if (!code) {
      setSvg("");
      setErr("");
      return;
    }

    ensureMermaidInitialized();
    let cancelled = false;

    mermaid
      .render(renderId, code)
      .then(({ svg }) => {
        if (cancelled) return;
        setSvg(svg);
        setErr("");
      })
      .catch((e) => {
        if (cancelled) return;
        setSvg("");
        setErr(String(e));
      });

    return () => {
      cancelled = true;
    };
  }, [props.code, renderId]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    while (container.firstChild) {
      container.removeChild(container.firstChild);
    }

    if (!svg) return;

    const doc = new DOMParser().parseFromString(svg, "image/svg+xml");
    const el = doc.documentElement;
    container.appendChild(document.importNode(el, true));
  }, [svg]);

  if (err) {
    return <div className="error">Mermaid render error: {err}</div>;
  }

  if (!svg) {
    return <div className="muted">No diagram yet.</div>;
  }

  return <div className="mermaidWrap" ref={containerRef} />;
}
