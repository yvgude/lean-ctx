# LeanCTX Cookbook

Praktische, echte Beispiele (ohne Mock-Daten), die gegen einen laufenden `lean-ctx serve` arbeiten.

## Voraussetzungen

- Node.js **22+**
- Ein laufender LeanCTX HTTP Server:

```bash
lean-ctx serve --host 127.0.0.1 --port 8080 --project-root /path/to/project
```

Wenn du `lean-ctx` noch nicht installiert hast:

```bash
cd ../rust
cargo run --release --bin lean-ctx -- serve --host 127.0.0.1 --port 8080 --project-root ..
```

## Setup

```bash
cd cookbook
npm ci
```

## Konfiguration (ENV)

- `LEANCTX_BASE_URL` (default: `http://127.0.0.1:8080`)
- `LEANCTX_BEARER_TOKEN` (optional)
  - Nur nötig, wenn du den Server mit `--auth-token <token>` startest oder non-loopback bindest.

## Beispiele

### Quickstart CLI

```bash
cd cookbook
npm run quickstart
```

### Memory Policy Playground (T1/T3/T4)

Erzeugt echte Facts, gibt Feedback, verknüpft Facts und zeigt das Mermaid-Diagramm.

```bash
cd cookbook
npm run memory-playground
```

### Knowledge Graph Explorer (Web)

Lokale Web-UI, die Facts und Relations (Mermaid) über Tool-Calls anzeigt.

```bash
cd cookbook
npm run graph-explorer
```

Die App nutzt einen Dev-Proxy (`/leanctx`), damit im Browser kein CORS-Setup nötig ist.
Falls dein `lean-ctx serve` nicht auf `http://127.0.0.1:8080` läuft:

```bash
cd cookbook
VITE_LEANCTX_BASE_URL="http://127.0.0.1:8080" npm run graph-explorer
```

## Troubleshooting

- **`ECONNREFUSED`**: Server läuft nicht oder falsches `LEANCTX_BASE_URL`.
- **`401 Unauthorized`**: `LEANCTX_BEARER_TOKEN` setzen (und Server mit `--auth-token` starten).

