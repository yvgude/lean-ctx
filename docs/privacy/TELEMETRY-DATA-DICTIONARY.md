# Telemetry Data Dictionary

Status: beschreibt den implementierten Vertrag `telemetry_v2`
(`SCHEMA_VERSION`, `deny_unknown_fields`). Serverseitige Aufbewahrung ist hier
**nicht** festgelegt; sie bedarf der Rechtsprüfung (§28).

Architekturkontext: [../architecture/telemetry.md](../architecture/telemetry.md).

Dieses Dokument definiert je Feld: Quelle, Zweck, Aufbewahrung, Kontobindung,
Optionalität, Aggregation und Löschverhalten.

## Lesehilfe

- **Aufbewahrung lokal** — die Konfiguration und das append-only Ledger auf dem
  Gerät des Nutzers; entfernbar mit `lean-ctx telemetry purge-local`.
- **Aufbewahrung remote** — durch Backend-Politik bestimmt und in diesem
  Repository nicht festgelegt. Wo unten „Backend-Politik" steht, ist die Frist
  offen und darf nicht behauptet werden.
- **Kontobindung** — ob das Feld mit einem Konto oder einer Organisation
  verknüpfbar ist.
- **Löschung** — `lean-ctx telemetry delete-remote` fordert die Löschung der
  Installation an; `reset-id` bricht die Verkettung für künftige Sendungen.

## 1. Batch

| Feld | Quelle | Zweck | Aufbewahrung | Kontobindung | Optional | Aggregation | Löschung |
|---|---|---|---|---|---|---|---|
| `schema_version` | Konstante | Vertragsversion, fail-closed | remote: Backend-Politik | nein | nein | keine | mit Installation |
| `deletion_token_hash` | `sha256` eines lokal erzeugten Geheimnisses (64 Hex) | erlaubt Löschung ohne Klaridentität | remote: Backend-Politik | nein | nein | keine | Schlüssel für Löschung |
| `events[]` | Client | Nutzlast | siehe unten | siehe unten | nein | täglich gebündelt | mit Installation |

Grenzen: höchstens `MAX_BATCH_EVENTS` Ereignisse; leerer Batch ist ungültig.

## 2. Envelope

| Feld | Quelle | Zweck | Aufbewahrung | Kontobindung | Optional | Aggregation | Löschung |
|---|---|---|---|---|---|---|---|
| `schema_version` | Konstante | Versionsprüfung | Backend-Politik | nein | nein | keine | mit Installation |
| `timestamp_bucket` | Client, Format `YYYY-MM-DD` | Tagesgenauigkeit statt Zeitpunkt | Backend-Politik | nein | nein | Tagesbucket | mit Installation |
| `installation_id` | zufällige UUID v4, lokal erzeugt | Installationen unterscheiden | lokal in Konfiguration; remote Backend-Politik | nein | nein | keine | `reset-id` bricht Verkettung; `delete-remote` löscht |
| `account_id` | `hmac-sha256:<64 Hex>` | Konto-Kohorten | Backend-Politik | **ja** | ja | keine | mit Installation/Konto |
| `organization_id` | `hmac-sha256:<64 Hex>` | Organisations-Kohorten | Backend-Politik | **ja** | ja | keine | mit Installation/Organisation |
| `app_version` | Build | Versionsverteilung | Backend-Politik | nein | nein | keine | mit Installation |
| `event` | Client | typisierte Nutzlast | siehe unten | siehe unten | nein | siehe unten | mit Installation |

`account_id` und `organization_id` setzt LeanCTX | OSS nie: Seine Sendungen
tragen keine Konto- oder Organisationsbindung. Die Felder gehören zum
gemeinsamen Vertrag mit dem gehosteten Dienst.

Rohe Bezeichner sind als pseudonyme ID unzulässig: Nur das Präfix
`hmac-sha256:` mit 64 Kleinbuchstaben-Hexzeichen wird akzeptiert, sowohl vom
Konstruktor als auch beim Deserialisieren. Fehlermeldungen geben die abgewiesene
ID nicht wieder. Die übrigen semantischen Grenzen werden weiterhin über
`TelemetryEnvelopeV2::validate()` beziehungsweise `TelemetryBatchV2::validate()`
vor dem Versand geprüft; gültige V2-Wire-Bytes bleiben unverändert.

## 3. Ereignisfamilien

Vierundzwanzig typisierte Varianten. Keine trägt Freitext.

Das Ereignisobjekt erlaubt ausschließlich `name` und `metrics`; zusätzliche
Felder werden bereits beim Deserialisieren abgewiesen, auch innerhalb eines
Envelopes. Sie werden nicht stillschweigend entfernt. Die gültigen V2-Wire-Bytes
bleiben unverändert; die Korrektur erweitert das erlaubte Schema nicht.

Der Vertrag ist mit dem gehosteten Dienst geteilt, der denselben Typ liest.
Die Spalte „LeanCTX | OSS" sagt, ob die Engine das Ereignis sendet:

- **ja** — wird gebildet, sobald es etwas zu zählen gibt;
- **nie (Zähler ohne Aufrufer)** — der Zähler existiert in
  `core::telemetry_aggregate`, wird aber von keinem Code-Pfad der Engine
  erhöht; das Ereignis bleibt deshalb aus;
- **nie (gehostete Ebene)** — gehört zu Testphase, Abonnement, Team und
  bezahlter Orchestrierung des gehosteten Dienstes; die Engine bildet es nie.

| Ereignis | Metrik-Typ | LeanCTX \| OSS |
|---|---|---|
| `heartbeat` | `HeartbeatMetrics` | ja |
| `setup_profile` | `SetupProfileMetrics` | ja |
| `setup_completed`, `integration_detected` | `OccurrenceMetrics` | ja (nach `setup`) |
| `session_aggregate` | `SessionMetrics` | ja |
| `tool_usage_aggregate` | `ToolUsageMetrics` | ja |
| `tool_call_aggregate` | `ToolCallMetrics` | ja |
| `autopilot_aggregate`, `autopilot_fallback_aggregate` | `DecisionMetrics` | nie (Zähler ohne Aufrufer) |
| `sync_aggregate` | `SyncMetrics` | nie (Zähler ohne Aufrufer) |
| `checkout_started` | `OccurrenceMetrics` | nie (Zähler ohne Aufrufer) |
| `trial_started`, `upgrade_viewed`, `subscription_activated`, `subscription_cancelled`, `team_created`, `team_member_invited` | `OccurrenceMetrics` | nie (gehostete Ebene) |
| `trial_ended`, `team_context_promoted` | `OutcomeMetrics` | nie (gehostete Ebene) |
| `error_category_aggregate` | `ErrorMetrics` | ja |
| `version_upgrade` | `VersionUpgradeMetrics` | ja (beim Wechsel der Hauptversion) |
| `orchestration_aggregate` | `OrchestrationMetrics` | nie (gehostete Ebene) |
| `usage_history` | `UsageHistoryMetrics` (ab 3.11.1) | ja |
| `feature_aggregate` | `FeatureMetrics` (ab 3.11.1) | ja |

## 4. Metrikfelder

Für alle Felder dieses Abschnitts gilt einheitlich: Quelle ist der lokale
Client, Aufbewahrung remote nach Backend-Politik, Kontobindung nur mittelbar
über `account_id`/`organization_id` im Envelope, Aggregation je Tagesbucket,
Löschung mit der Installation. Abweichungen sind vermerkt.

### `HeartbeatMetrics`

| Feld | Werte | Zweck | Optional |
|---|---|---|---|
| `distribution_channel` | `cargo`, `homebrew`, `npm`, `docker`, `source`, `aur`, `pypi`, `binary`, `unknown` | Kanalverteilung. Ab 3.11.1 aus dem Speicherort der laufenden Datei abgeleitet (z. B. `node_modules` → `npm`); der Pfad selbst verlässt den Rechner nie | nein |
| `client_family` | `claude`, `codex`, `cursor`, `gemini`, `windsurf`, `zed`, `vscode_copilot`, `kiro`, `antigravity`, `codebuddy`, `codewhale`; ab 3.11.1 zusätzlich `cline`, `roo_code`, `kilo_code`, `continue_dev`, `opencode`, `goose`, `amp`, `augment`, `jetbrains`, `warp`, `trae`, `qwen_code`, `crush`, `claude_desktop`, `chatgpt`, `lm_studio`, `copilot_cli`, `visual_studio`, `neovim`, `emacs`, `factory` und `none`; ab 3.11.5 zusätzlich `pi` (Pi Coding Agent über seine MCP-Erweiterungen wie `pi-lean-ctx` oder `pi-mcp-adapter`) und `oh_my_pi`; sonst `other` | Client-Kohorte: aus dem MCP-Handshake dieses Prozesses, sonst dem zuletzt gespeicherten Handshake (höchstens 7 Tage alt), sonst aus den Markern `CLAUDECODE`, `CODEX_HOME`, `CURSOR_TRACE_ID`, `GEMINI_CLI` (unter Linux auch aus einem Elternprozess, wenn der Client die Umgebung seines MCP-Servers filtert; nur ob sie gesetzt sind, nie ihr Wert). `none` = kein MCP-Client gesehen (nur CLI und Shell-Hooks), `other` = ein MCP-Client, den LeanCTX nicht kennt; sein Name wird nie gesendet | nein |
| `operating_system` | `macos`, `linux`, `windows`, `other` | Plattformverteilung | nein |
| `architecture` | `x86_64`, `aarch64`, `other` | Build-Priorisierung | nein |
| `install_age` | `lt_1h`, `lt_1d`, `lt_7d`, `lt_30d`, `gte_30d` | Alter der lokalen Installationskennung als Bereich; trennt Menschen von kurzlebigen Agent-Sandboxes (ab 3.11.1) | ja |
| `active_days` | `d1`, `d2_3`, `d4_7`, `d8_14`, `d15_plus` | Anzahl UTC-Tage mit erfolgreicher Sendung in den letzten 30 Tagen, aus dem lokalen Sende-Ledger, als Bereich (ab 3.11.1) | ja |
| `runtime_environment` | `local`, `container`, `codespaces`, `gitpod`, `replit`, `cloud_agent`, `ci`, `unknown` | Laufumgebung aus dokumentierten Markern (`CODESPACES`, `GITPOD_WORKSPACE_ID`, `REPL_ID`, `CLAUDE_CODE_REMOTE`, Container-Indikatoren, CI nur nach `LEAN_CTX_TELEMETRY_IN_CI=1`); keine Werte der Variablen (ab 3.11.1) | ja |

Alle sieben sind geschlossene Aufzählungen — keine freien Zeichenketten, keine
Pfade, keine Zeitstempel. Die drei optionalen Felder fehlen bei älteren Clients.

### `SetupProfileMetrics`

Einmal je Tagesbatch, Momentaufnahme der Einrichtung.

| Feld | Werte | Zweck | Optional |
|---|---|---|---|
| `integration_mode` | `default`, `mcp`, `hybrid`, `replace` | welcher Integrationsmodus genutzt wird (`default` = nie gesetzt) | nein |
| `embeddings` | `unsupported`, `disabled`, `not_installed`, `installed` | Verbreitung der semantischen Suche | nein |

Nicht enthalten: Modellname, Pfade, Konfigurationswerte, Projektanzahl.

### `ToolCallMetrics`

| Feld | Werte | Zweck | Optional |
|---|---|---|---|
| `tools[].tool` | Name eines **eingebauten** lean-ctx-Tools (`[a-z][a-z0-9_]*`, ≤ 64 Byte) | welche Tools tatsächlich genutzt werden | nein |
| `tools[].calls`, `tools[].failures` | Zähler seit dem letzten bestätigten Batch | Nutzung und Fehlerquote je Tool | nein |
| `tools[].latency_milliseconds_total` | Summe der Laufzeit dieser Aufrufe in ms | mittlere Latenz je Tool | ja (ab 3.11.1) |
| `tools[].failure_kinds` | Fehler je geschlossener Klasse: `invalid_input`, `not_found`, `permission`, `policy_blocked`, `timeout`, `conflict`, `too_large`, `unavailable`, `other` | warum Tools fehlschlagen; die Klasse wird lokal aus der Meldung abgeleitet, die Meldung selbst wird nie gesendet | ja (ab 3.11.1) |
| `tools[].failure_messages[]` | höchstens 5 `{template, count}` je Tool und Tag; `template` ist die erste Zeile der Fehlermeldung, auf dem Gerät bereinigt: Text in Anführungszeichen → `‹str›`, Pfade und Dateinamen → `‹path›`, Zahlen und alles mit Ziffern → `‹n›`, Bezeichner → `‹sym›`, URLs → `‹url›`, E-Mail-Adressen → `‹email›`, Nicht-ASCII-Wörter → `‹text›`; höchstens 160 Zeichen, keine Ziffern, geschlossener Zeichensatz. Bleibt kein sicherer Wortlaut übrig, wird kein Muster gesendet. | welche Fehler Nutzer treffen, damit wir sie beheben können | ja (ab 3.11.1) |

### `FeatureMetrics` (Event `feature_aggregate`, ab 3.11.1)

Tageszähler der genutzten LeanCTX-Befehle und Hintergrundfunktionen.

| Feld | Werte | Zweck | Optional |
|---|---|---|---|
| `features[].feature` | Code aus der festen Registry in `core/telemetry_features.rs`: `cli.<befehl>` oder `cli.<befehl>.<verb>` (z. B. `cli.pack.export`, `cli.graph.build`), `index.graph`, `index.bm25`, `index.semantic`; je Segment `[a-z][a-z0-9_]{0,23}`, höchstens drei Segmente | welche Funktionen tatsächlich genutzt werden. Befehl und Verb werden nur gezählt, wenn sie wörtlich in der Registry stehen; Argumente, Pfade, Abfragen oder andere Eingaben werden nie Teil eines Codes. Befehle im Hot-Path (Shell-Hooks, `-c`, `read`, `grep`, Statuszeile …) werden nicht gezählt | nein |
| `features[].count` | Nutzungen am Tag (≥ 1) | Nutzung | nein |
| `features[].failures` | davon fehlgeschlagen (≤ `count`; bei Index-Builds) | Zuverlässigkeit | ja |

Höchstens 96 Codes je Tag, aufsteigend sortiert.

MCP-Sessions (ein MCP-Serverprozess = eine Agenten-Session) zählen über
dieselben Codes, nur als Tagessummen, ohne Session-Kennung:

| Code | `count` | `failures` |
|---|---|---|
| `mcp.session` | Sessions nach dem Client-Handshake | – |
| `mcp.session.empty` | beendet ohne einen einzigen Tool-Aufruf | – |
| `mcp.session.clean` (ab 3.11.5) | beendet, kein Tool-Aufruf schlug fehl | – |
| `mcp.session.recovered` (ab 3.11.5) | Aufrufe schlugen fehl, aber der letzte Aufruf jedes Tools gelang | – |
| `mcp.session.unresolved` (ab 3.11.5) | der letzte Aufruf mindestens eines Tools schlug fehl | davon mit einem solchen Fehler auf LeanCTX-Seite (`timeout`, `unavailable`, `too_large`, `other`) statt beim Aufrufer |
| `mcp.session.retry` (ab 3.11.5) | Aufrufe direkt nach einem fehlgeschlagenen Aufruf desselben Tools | davon erneut fehlgeschlagen |

Jede beendete Session erhält genau einen der Ausgänge `empty`, `clean`,
`recovered`, `unresolved`. Ausgewertet werden nur die prozessinternen Zähler
aus `core::telemetry`; Argumente, Pfade oder Fehlertexte fliessen nicht ein.
Zweck: wie oft Agenten-Sessions mit LeanCTX technisch ohne Fehler durchlaufen
und ob LeanCTX oder der Aufrufer einen Abbruch verursacht. Ob die
Programmieraufgabe selbst gelang, lässt sich daraus nicht ableiten.

### `UsageHistoryMetrics` (Event `usage_history`, ab 3.11.1)

Die eigene Tagesbilanz der Installation, wie `lean-ctx gain` sie zeigt (`stats.json`).

| Feld | Werte | Zweck | Optional |
|---|---|---|---|
| `days[].date` | lokaler Kalendertag `YYYY-MM-DD`, höchstens die letzten 90 Tage | Verlauf auch vor dem ersten Heartbeat | nein |
| `days[].commands` | komprimierte Operationen des Tages (MCP-Aufrufe und Shell-Hook-Befehle) | Nutzung ausserhalb von MCP sichtbar machen | nein |
| `days[].original_tokens`, `days[].delivered_tokens` | Tokens vor und nach der Kompression | Einsparung pro Tag | nein |
| `lifetime.commands`, `lifetime.original_tokens`, `lifetime.delivered_tokens` | Gesamtsummen seit Installation | Lifetime-Wert | nein |
| `lifetime.first_use_month` | `YYYY-MM` | Nutzungsdauer, nur Monatsgenauigkeit | ja |

Keine Befehlsnamen, Pfade oder Inhalte. `ErrorCategory` kennt ab 3.11.1 zusätzlich
`command`: ein über ein Shell-Tool ausgeführter Befehl endete mit Exit-Code ≠ 0
(Fehler des Befehls, nicht von LeanCTX).

Namen stammen ausschließlich aus der statischen Tool-Registry des Binaries;
Tools fremder MCP-Server, Argumente und Ergebnisse werden nie gezählt. Liste
strikt sortiert und eindeutig, höchstens `MAX_TOOL_ENTRIES` (128) Einträge
(bei Überlauf die meistgenutzten), `failures ≤ calls`, `calls ≥ 1`.

### Zählerstrukturen

| Struktur | Felder | Zweck |
|---|---|---|
| `OccurrenceMetrics` | `count` | Häufigkeit eines Funnel- oder Setup-Ereignisses |
| `OutcomeMetrics` | `accepted`, `rejected`, `unknown` | Ergebnisverteilung |
| `SessionMetrics` | `sessions`, `duration_seconds` (Histogramm) | Nutzungsintensität |
| `ToolUsageMetrics` | `calls`, `failures`, `latency_milliseconds` (Histogramm), optional `tokens.original` / `tokens.delivered` (ab 3.11.1) | Zuverlässigkeit, Latenz und Token-Einsparung (Tagessummen, keine Inhalte) |
| `DecisionMetrics` | `admitted`, `denied`, `fallback` | Autopilot-Entscheidungskategorien |
| `SyncMetrics` | `attempts`, `successes`, `failures` | Sync-Zuverlässigkeit |
| `ErrorMetrics` | `category`, `count` | Fehlerklassen ohne Fehlertext |
| `VersionUpgradeMetrics` | `from_major`, `to_major` | Upgrade-Pfade |

`category` ist eine geschlossene Aufzählung: `authentication`, `authorization`,
`configuration`, `network`, `provider`, `timeout`, `validation`, `internal`, `command` (ab 3.11.1).
**Rohe Fehlermeldungen erscheinen nirgends.**

Konsistenz wird erzwungen: `failures ≤ calls`; `successes + failures ≤ attempts`;
`to_major ≥ from_major`. Jeder Zähler ist durch `MAX_COUNT` begrenzt.

### `OrchestrationMetrics`

| Feld | Zweck |
|---|---|
| `admitted_tasks`, `execution_plans` | Umfang bezahlter Orchestrierung |
| `retries`, `fallbacks`, `cancellations` | Robustheit |
| `lease_conflicts` | Konkurrenz um Mutationsrechte |
| `receipts` | Nachweisdichte |
| `outcomes` | eingebettete `OutcomeMetrics` |
| `node_count`, `fan_out`, `depth`, `parallel_nodes` | Histogramme der Graphform |

Nicht enthalten und ausdrücklich verboten: Aufgabentext, Prompts,
Kind-Agent-Ausgaben, Pfade, Dateinamen, Evidenz- und Kontextinhalte, exakte
Repository-Identität, Policy-Text.

### `Histogram`

`upper_bounds[]` und `counts[]`, geordnet und in Länge gekoppelt, begrenzt durch
`MAX_HISTOGRAM_BUCKETS`. Histogramme tragen Verteilungen, keine Einzelwerte —
ein einzelner Vorgang ist daraus nicht rekonstruierbar.

## 5. Lokales Ledger

Append-only auf dem Gerät, nie an das Backend gesendet.

| Feld | Zweck | Aufbewahrung | Löschung |
|---|---|---|---|
| `timestamp` | Nachvollzug der Sendung | lokal | `purge-local` |
| `installation_id`, `version`, `os`, `arch` | Zuordnung der Sendung | lokal | `purge-local` |
| `schema_version` | Vertragsversion | lokal | `purge-local` |
| `event_names[]` | welche Ereignisse gingen raus | lokal | `purge-local` |
| `payload_hash` | Integritätsnachweis **statt Inhalt** | lokal | `purge-local` |
| `endpoint`, `status` | Ziel und Ergebnis | lokal | `purge-local` |

Das Ledger enthält bewusst keine Nutzlasten, nur deren Hash.

## 6. Rechtliche Prüfliste (offen)

Vor einer öffentlichen Datenschutzaussage ist durch Rechtsberatung zu klären:

- Einstufung der Installations-ID und der HMAC-Pseudonyme nach Schweizer DSG
  und DSGVO;
- Rechtsgrundlage für Standard-an-Telemetrie je Rechtsordnung;
- konkrete serverseitige Aufbewahrungsfristen je Tabelle;
- Erfüllung von Auskunfts- und Löschbegehren über `deletion_token_hash`, ohne
  eine Klaridentität einzuführen;
- Behandlung von IP-Adressen an der Netzwerkkante — sie dürfen nicht als
  Produktanalyse-Identifikator persistiert werden. Umgesetzt: Der Server
  speichert nur einen geschlüsselten /24- bzw. /48-Netz-Hash und das
  öffentliche autonome System, nie die Adresse (siehe
  `docs/architecture/telemetry.md`);
- Auftragsverarbeitung und Drittlandtransfer;
- Aufbewahrung in Team- und Enterprise-Kontexten mit Organisationsbindung.

Bis diese Punkte geklärt sind, wird keine Frist und keine Rechtsgrundlage als
Tatsache behauptet.
