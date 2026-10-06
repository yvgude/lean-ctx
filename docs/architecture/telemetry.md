# Telemetry

Status: Ereignismodell v2, Konfigurationsmodell, lokales Ledger und CLI
implementiert (`core::telemetry_v2`, `core::telemetry_ledger`,
`cli::telemetry_cmd`). Backend-Vertrag und Aufbewahrungspolitik sind nicht in
diesem Repository abgenommen.

Felddefinitionen stehen in
[../privacy/TELEMETRY-DATA-DICTIONARY.md](../privacy/TELEMETRY-DATA-DICTIONARY.md).

## Haltung

Telemetrie ist ein Produktanalysesystem, kein Hintereingang. Sie ist in v4
standardmässig aktiv, jederzeit abschaltbar und vollständig lokal einsehbar.

Zutreffende Formulierung:

> LeanCTX sendet standardmässig begrenzte pseudonyme Produkttelemetrie. Prompts,
> Quellcode, Dateiinhalte, Dateinamen, Shell-Kommandos und Geheimnisse werden
> nicht gesendet. Telemetrie lässt sich jederzeit abschalten.

Nicht behauptet wird „niemals personenbezogene Daten": Eine zufällige, dauerhafte
Installations-ID kann in einschlägigen Rechtsordnungen als pseudonymes
personenbezogenes Datum gelten.

## Standard-an

`TelemetryConfig` trägt `enabled` (Standard `true`), `preference` und
`last_heartbeat`. Offenlegung statt Sendegatter, an drei Stellen mit derselben
Liste aus `core::telemetry_consent::DISCLOSURE`:

- **Setup** fragt „Keep anonymous telemetry on? [Y/n]“ und listet die
  gesendeten Kategorien. Beide Antworten werden persistiert; ein „n“ schreibt
  `enabled = false` und `preference = "explicitly_disabled"` und überlebt jedes
  spätere Upgrade.
- **`telemetry on`** zeigt die Liste nach dem Einschalten.
- **Einmaliger Hinweis** beim ersten interaktiven Befehl (stdin und stderr sind
  Terminals; nie für MCP, Hooks, Server-Modus, Pipes; nie auf stdout), solange
  die Telemetrie tatsächlich senden würde. Die gesehene Version steht in
  `$STATE/telemetry_notice_version`; `NOTICE_VERSION` wird erhöht, wenn die
  Liste eine Kategorie gewinnt.

Vor 3.11.0 wurde ein abgelehntes Setup nicht gespeichert; solche Installationen
sind nach dem Upgrade an und sehen den Hinweis.

**CI sammelt und sendet nie.** `CI` (ausser `false`/`0`) und die üblichen
Anbieter-Marker (`GITHUB_ACTIONS`, `GITLAB_CI`, `BUILDKITE`, `CIRCLECI`,
`JENKINS_URL`, `TF_BUILD`, …) sperren Sammeln und Senden; jeder Lauf hätte sonst
ein frisches Home und zählte als neue Installation. `LEAN_CTX_TELEMETRY_IN_CI=1`
nimmt eine Maschine, die einen solchen Marker aus anderen Gründen setzt, wieder
auf. `telemetry status` nennt den Grund.

Entscheidend ist das Präferenzmodell:

```rust
enum TelemetryPreference { DefaultOn, ExplicitlyEnabled, ExplicitlyDisabled }
```

Es unterscheidet „standardmässig an, weil nie gewählt" von „bewusst gewählt".
**Ein früheres `telemetry off` wird bei der Migration nie überschrieben.**

Abschaltwege, die respektiert werden müssen: `DO_NOT_TRACK=1`,
`LEAN_CTX_TELEMETRY=off`, `telemetry.enabled = false` sowie eine zentrale
Organisations- oder Netzwerkpolitik. **Keine Telemetrie darf eine strengere
Richtlinie umgehen.** Eine unlesbare globale Konfiguration gilt als Opt-out
(fail-closed): Es wird weder gesammelt noch gesendet. Telemetrie blockiert den
Prozess nie.

## Harte Verbote

Niemals gesendet werden: Prompt-Text, Modellantworten, Quellcode, Dateiinhalte,
Dateinamen, vollständige Pfade, Shell-Kommandos, Kommandoausgaben, Geheimnisse,
API-Schlüssel, Umgebungswerte, Repository- und Git-Remote-URLs, vom Nutzer
eingegebener Aufgabentext, Issue-Text, beliebige Fehlertexte und Stacktraces mit
Pfaden oder Inhalten.

Für Orchestrierung zusätzlich verboten: Aufgabentext, Kind-Agent-Ausgaben,
Evidenzinhalte, Kontextinhalte, exakte Repository-Identität und beliebiger
Policy-Text.

## Typisierter Vertrag

Es gibt keinen `serde_json::Value`-Durchgriff aus Laufzeit-Interna. Jedes
Ereignis ist eine typisierte Variante mit typisierten, begrenzten Metriken; die
Strukturen tragen `deny_unknown_fields`.

```text
TelemetryBatchV2 { schema_version, deletion_token_hash, events[] }
  └── TelemetryEnvelopeV2 { schema_version, timestamp_bucket,
                            installation_id, account_id?, organization_id?,
                            app_version, event }
```

Validierung ist fail-closed: falsche Schemaversion, unparsbarer Tagesbucket,
nicht-UUID-Installations-ID, unplausible App-Version, ungültige pseudonyme ID,
überschrittene Zähler-Obergrenzen, widersprüchliche Zählerpaare (etwa mehr
Fehlschläge als Aufrufe) und rückwärts laufende Versionssprünge werden
abgewiesen.

Aggregate werden bevorzugt täglich gebündelt statt hochfrequent je Tool-Aufruf
gesendet.

## Lokale Transparenz

```text
lean-ctx telemetry status
lean-ctx telemetry show | pending     # exakte, aktuell sendefähige Nutzlast
lean-ctx telemetry history | log
lean-ctx telemetry on | off
lean-ctx telemetry reset-id
lean-ctx telemetry purge-local
lean-ctx telemetry delete-remote
```

`status` trennt die **gespeicherte Präferenz** (`Preference`) von der
**tatsächlichen Sendefähigkeit** (`Sending`) und nennt bei Inaktivität deren
Grund — `DO_NOT_TRACK` / `LEAN_CTX_TELEMETRY`, das persistierte Opt-out oder
eine unlesbare Konfiguration. Die Entscheidung stammt dabei ausschliesslich aus
`TelemetryConfig::send_eligible`, also derselben Instanz, die auch den
Sendepfad freigibt.

`show` zeigt die **exakte typisierte Nutzlast**, nicht eine Beschreibung davon.

Das lokale Ledger ist append-only und hält je erfolgreicher Übertragung fest:
Zeitstempel, Installations-ID, Version, OS, Architektur, Schemaversion,
Ereignisnamen, Nutzlast-Hash, Endpunkt und Status. **Private Nutzlasten stehen
nicht im Ledger** — nur deren Hash.

## Client-Engineering

Telemetrie darf LeanCTX nicht verschlechtern: nicht blockierend, begrenzte
Warteschlange, begrenzter Plattenverbrauch, exponentielles Backoff, periodische
Bündelung, Timeout, Ratenbegrenzung, kein Retry-Sturm, keine Panik, keine
Startverzögerung, kein blockierender Tool-Aufruf, kein Scheitern von
Offline-Abläufen. Netzwerkfehler degradieren still mit Debug-Diagnose. Es wird
kein Prozess je Ereignis gestartet.

Der v2-Batch-Sender liest die globale Konfiguration und beide Abschaltvariablen
direkt vor dem HTTP-Aufruf erneut. Ungültige Konfiguration und Opt-out
verhindern diesen Aufruf; ein bereits laufender Request wird dadurch nicht
rückwirkend abgebrochen. Hintergrundsendungen nutzen ein kurzes, die Sendung
beim Beenden ein noch kürzeres Transportlimit.

### Untertägiger, kumulativer Versand

Zähler werden **je UTC-Tag** geführt (`days`: Tagessumme + zuletzt bestätigte
Summe). Jede Sendung trägt die **vollständige laufende Tagessumme**, nicht ein
Delta. Der Server ersetzt je Installation und Tag (`ON CONFLICT … DO UPDATE`),
wiederholte Sendungen desselben Tages sind daher idempotent und kein
Doppelzählen möglich.

- Ausgelöst wird periodisch aus dem MCP-Server (alle zehn Tool-Aufrufe
  geprüft) und beim Beenden des Servers. Auch Nutzer mit nur einem aktiven Tag
  liefern so ihre Tool-Zahlen.
- Zulassung unter der Aggregate-Sperre: höchstens acht Versuche je Tag (unter
  dem Server-Limit von zehn); periodische Sendungen lassen einen Versuch für
  das Beenden frei. Nach einer bestätigten Sendung wächst der Abstand ab 15
  Minuten exponentiell (max. 2 h), beim Beenden gilt ein flacher Abstand von
  5 Minuten. Fehlversuche werden ab 60 s exponentiell zurückgestellt. Ein
  unveränderter, bereits bestätigter Tag wird ohne Versuch abgelehnt.
- Ein Batch enthält zuerst den heutigen Tag, danach unbestätigte abgeschlossene
  Tage (älteste zuerst) unter ihrem **eigenen** Tagesbucket. Höchstens sieben
  abgeschlossene Tage werden lokal zurückgehalten.

Unbestätigte Batches behalten ihre exakten Bytes auch über Tageswechsel; die
Zustellung ist deshalb **at least once**, nicht exactly once — dank der
Ersetzungssemantik ohne Doppelzählung. Aggregate-, One-shot- und Ledger-Sperren verwenden einen
750-ms-Akquisitionsetat. Bei Überlast bleibt eine Bestätigung wiederholbar;
Lösch-/Rotationsbefehle müssen einen Sperrfehler melden. Das ist keine Frist
für Dateisystem-I/O und keine Transaktion über sämtliche lokalen Dateien.
Ein nicht lesbares oder belegtes Ledger gilt nicht als leere Versionshistorie;
die Versionsbeobachtung kann nach Freigabe wiederholt werden. `purge-local`
übernimmt zuerst die Aggregate-/One-shot-Sperren, bevor es das Ledger löscht.
Scheitert der Ledger-Eintrag nach erfolgreichem Versand, bleibt der Batch
unbestätigt und kann erneut übertragen werden, bis die lokale Historie wieder
geschrieben werden kann; daraus folgt keine Exactly-once-Garantie.

## Vorhandener Engine-Stand

- 20 typisierte Ereignisvarianten von `heartbeat` bis
  `orchestration_aggregate`.
- Begrenzungen als Konstanten: `MAX_COUNT`, `MAX_HISTOGRAM_BUCKETS`,
  `MAX_BATCH_EVENTS`.
- `PseudonymousId` akzeptiert ausschliesslich `hmac-sha256:<64 Hex>`; rohe
  Bezeichner werden abgewiesen.
- `deletion_token_hash` als 64-stelliger Kleinbuchstaben-Hex-Digest im Batch.
- Ledger und CLI wie oben, inklusive `purge-local` und `delete-remote`.
- Tests decken Roundtrip, Batch-Grenzen, Ablehnung unbekannter Felder,
  Identitäts- und Versionsprüfung, Kreuzfeld-Konsistenz und Histogrammform ab.

## Offene Abnahme-Gates

- Der Backend-Vertrag (`POST /api/telemetry/v2/batch`,
  `DELETE /api/telemetry/v2/installations/{id}`) liegt ausserhalb dieses
  Repositories und ist hier nicht abgenommen.
- **Serverseitige Aufbewahrungsfristen sind hier nicht festgelegt** und dürfen
  ohne Rechtsprüfung nicht behauptet werden.
- Die Prüfliste für Schweizer DSG- und DSGVO-Anwendbarkeit ist noch zu
  erstellen (§28).
- Ob `delete-remote` eine bestätigte serverseitige Löschung erreicht, ist nicht
  belegt.

## Mindestabnahme

Tool-Zahlen erreichen den Server noch am selben Tag unter dem richtigen
Tagesbucket; ein früheres explizites Opt-out überlebt die Migration; `DO_NOT_TRACK=1` unterdrückt jede
Übertragung; `telemetry show` zeigt exakt die sendefähige Nutzlast; ein
Ereignis mit unbekanntem Feld wird abgewiesen; kein verbotenes Feld erscheint
in irgendeiner Nutzlast; das Ledger enthält Hashes statt Inhalten; und ein
Netzwerkausfall verzögert keinen Tool-Aufruf.
