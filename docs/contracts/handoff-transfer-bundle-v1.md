# Handoff Transfer Bundle v1 (HandoffTransferBundleV1)

GitLab: `#2320` (Subtickets: `#2347–#2351`)

Das **Handoff Transfer Bundle** ist ein portables, versioniertes Export/Import‑Format für Handoffs über Tools/Teams/Transportflächen hinweg.
Es ergänzt `HandoffLedgerV1` um **projektgebundene Identität**, **boundedness** und **privacy‑aware Export** (redacted‑by‑default), plus eine **Artefakt‑Übersicht** für Replayability.

## Ziele

- **Deterministisch**: gleicher Zustand ⇒ gleiches Bundle (bis auf `exported_at`).
- **Bounded**: klare Caps (Bytes/Counts) verhindern „unendlich große“ Handoffs.
- **Redacted-by-default**: Export ist standardmäßig privacy-safe; `full` nur für `admin`.
- **Portable**: Bundle ist eine einzelne JSON Datei; Import ist **additiv** (kein Overwrite von lokalen Indizes/Caches).
- **Replayability**: enthält project identity hashes + Artefakt‑Übersicht (Proofs).

## Format (JSON)

Top-level Felder:

- `schema_version`: `1`
- `exported_at`: RFC3339 timestamp
- `privacy`: `"redacted" | "full"`
- `project`:
  - `project_root_hash`: best-effort hash des Project Roots
  - `project_identity_hash`: best-effort hash der Project Identity
- `ledger`: `HandoffLedgerV1` (ggf. redacted)
- `artifacts`:
  - `resolved`: Liste konfigurierte Context‑Artifacts (`.lean-ctx-artifacts.json` etc.)
  - `proof_files`: Liste `.lean-ctx/proofs/*` (basename + md5 + bytes), bounded
  - `warnings`: warnings beim artifact resolve/listing

## Privacy Levels

- `privacy="redacted"` (default):
  - `ledger.project_root` wird entfernt
  - `ledger.session_snapshot` wird entfernt
  - freie Textfelder (Task/Findings/Decisions/Next steps, Knowledge values, curated ref contents) werden über `redaction::redact_text` gejailt
- `privacy="full"`:
  - nur erlaubt für Role `admin`
  - wenn Redaction für `admin` aktiv ist, bleibt Redaction konsistent aktiv

## Import Semantik

- Import validiert `schema_version` (hart).
- Project identity mismatch wird als **WARN** reportet (Import bleibt möglich).
- Import ist **additiv**:
  - optional: workflow anwenden
  - optional: session excerpt anwenden
  - optional: knowledge facts importieren (policy‑aware)
- **Non-goal**: kein Overwrite bestehender lokaler Indizes/Caches.

## Surface

Tool:

- `ctx_handoff action=export format=json|summary write=true|false path=<optional> privacy=redacted|full ledger=<optional>`
  (`ledger`: ein gespeichertes Ledger exportieren, etwa ein zusammengeführtes — es wird vorher wie bei `show` freigegeben)
- `ctx_handoff action=import path=<bundle.json> apply_workflow=true|false apply_session=true|false apply_knowledge=true|false`

## Idempotentes `create` (`request_id`)

`ctx_handoff action=create request_id=<1–128 Bytes>` ist pro Agent und Projekt
idempotent: ein gespeichertes Ledger mit gleichem Schlüssel und gleicher Anfrage
(kuratierte Pfade in Reihenfolge) wird erneut ausgeliefert, statt ein zweites zu
schreiben; derselbe Schlüssel mit anderen Pfaden wird abgelehnt
(`idempotency conflict`). `request_id` und `request_digest` (optional) stehen im
Ledger und sind vom `content_md5` abgedeckt; Ledger ohne Schlüssel bleiben
unverändert. Ohne Projekt ist kein Schlüssel zulässig.

## Zusammengeführte Ledger (`ctx_handoff action=merge`)

`ctx_handoff action=merge ledgers=[<ledger.json>, …]` (2–8 Ledger eines Projekts)
schreibt einen neuen `HandoffLedgerV1`, den ein weiterer Agent mit `pull` übernimmt:

- Jedes Eingabe-Ledger wird wie bei `show`/`pull` als Ganzes neu freigegeben;
  ein zurückgehaltenes Ledger verweigert den Merge (typisiert, ohne Inhalt).
- Das Ergebnis ist eine Teilmenge der Eingaben, Byte für Byte: Duplikate fallen
  zusammen, die Obergrenzen von `create` gelten, nichts wird umgeschrieben.
- Ein Fakten-Schlüssel mit verschiedenen Werten behält alle Werte und wird gemeldet.
- Ein Teil gilt als gebunden, wenn er in einem Eingabe-Ledger gebunden war; die
  Quellen aller Eingaben werden neu gelesen und bilden die Herkunft des Ergebnisses.
- Deterministisch: gleiche Eingabemenge → gleiche Bytes und gleiche Datei, in jeder
  Reihenfolge. `merged_from` (optional, nur bei zusammengeführten Ledgern) nennt die
  Inhalts-Digests der Eingaben und ist vom `content_md5` des Ergebnisses abgedeckt.
  Binaries ohne dieses Feld lesen ein zusammengeführtes Ledger, berechnen seinen
  Digest aber ohne `merged_from` (abweichend); sie erzeugen selbst keine.
- Ein kuratierter Auszug bleibt nur, wenn seine Datei neu gebunden wird; Auszüge
  ohne Quelle fallen weg und werden gezählt.

Im Team: `export ledger=<merged.json> write=true` schreibt ein signiertes Bundle ins
Projekt, `lean-ctx team artifact publish --content-file <bundle.json> …` teilt es über
den Team-Hub (Team-Abo der Organisation, serverseitig geprüft); der Empfänger holt es
mit `lean-ctx team artifact use` und übernimmt es mit `ctx_handoff action=import`. Dafür
gibt es keinen eigenen Speicher: Bundle, Signatur und Team-Artefakt sind die
vorhandenen Bausteine.

## Relevanter Code

- Bundle contract/runtime: `rust/src/core/handoff_transfer_bundle.rs`
- Ledger: `rust/src/core/handoff_ledger.rs`
- Tool surface: `rust/src/server/dispatch/session_tools.rs` + `rust/src/tool_defs/granular.rs` + `rust/src/tools/ctx_handoff.rs`

