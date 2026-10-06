# Cross-Agent Context Delivery

Status: V4-Zielvertrag mit vorhandenen lokalen Teilprimitiven. Dieses Dokument
ist weder Release- noch Pro-Verfügbarkeitsnachweis.

## Zweck und Produktgrenze

Cross-Agent Context Delivery verhindert, dass ein Agent denselben unveränderten
Kontext erneut materialisiert, den ein anderer berechtigter Agent bereits
erhalten hat. Das Ergebnis ist eine überprüfbare Referenz auf die ursprüngliche
Auslieferung, nicht bloss ein anonymer Cache-Treffer. Die Funktion gehört zum
Pro-Wertversprechen; Team erweitert den Scope auf autorisierte gemeinsame
Workspaces, ohne die Isolationsregeln abzuschwächen.

Nicht Teil dieses Vertrags sind Agent-Bus-Nachrichten, Task-Delegation,
Checkpoint-Synchronisierung oder ein allgemeiner Content-Store.

## Kanonischer Vertrag

Eine produktionsfähige Auslieferung benötigt mindestens:

```text
CrossAgentDeliveryV1 {
  delivery_id
  schema_version
  scope { account_id, workspace_id, project_id }
  request_key { kind, canonical_input_digest, policy_revision }
  content { algorithm, digest, byte_len, media_type }
  source_validator
  original { agent_id, conversation_id, host, delivered_at }
  classification
  expansion_handle
  expires_at
}
```

`delivery_id` identifiziert das Ereignis. Der Content-Digest identifiziert die
unveränderten Bytes. Diese Identitäten dürfen nicht austauschbar verwendet
werden. Account, Workspace und Projekt sind Bestandteil des Lookup-Scopes und
werden vor jedem Treffer autorisiert; Pfade allein sind keine Mandantengrenze.

## Aufnahme

1. Adapter normalisiert alle semantisch relevanten Eingaben deterministisch.
2. Policy klassifiziert Ergebnis und erlaubten Wiederverwendungs-Scope.
3. Validator bindet die Auslieferung an überprüfbaren Quellzustand.
4. Vollständiger BLAKE3-Content-Digest, Byte- und Tokenzahl werden berechnet.
5. Original-Agent, Conversation, Host und Scope werden unveränderlich erfasst.
6. Materialisierung wird unter begrenzter TTL und Kapazität gespeichert.

Geheimnisse, Credentials, private Schlüssel und als nicht teilbar klassifizierte
Inhalte dürfen nicht aufgenommen werden. Fehlende Klassifikation oder fehlende
Scope-Autorität bedeutet fail-closed: normal materialisieren, keine Referenz.

## Lookup und unverändert-Nachweis

Ein Treffer ist nur zulässig, wenn alle Bedingungen gelten:

- Schema und Operationstyp werden unterstützt.
- Account-, Workspace- und Projekt-Scope stimmen exakt und sind autorisiert.
- Kanonischer Request-Key und aktive Policy-Revision stimmen überein.
- Validator bestätigt unveränderten Quellzustand.
- Eintrag ist nicht abgelaufen oder invalidiert.
- Klassifikation erlaubt Übergabe an den anfragenden Agenten.
- Content-Handle ist vorhanden und sein Digest wird bei Expansion verifiziert.

Ein `mtime` ist nur ein schneller Kandidatenfilter, kein ausreichender
Integritätsbeweis. Bei Änderung, unklarer Frische, fehlender Autorität oder
Digest-Abweichung gibt es keinen Stub; der Adapter materialisiert neu und kann
danach eine neue Auslieferung erfassen.

## Referenz und deterministische Expansion

Die Kurzantwort nennt stabil den logischen Inhalt, die ursprüngliche
Auslieferung und die vermiedenen Tokens. Sie enthält einen opaken,
scope-gebundenen `expansion_handle`, niemals einen frei auflösbaren Dateipfad.
Die Expansion:

1. prüft erneut Account, Workspace, Projekt, Klassifikation und Ablauf;
2. lädt exakt den referenzierten Content-Handle;
3. verifiziert Algorithmus, Digest und Bytezahl;
4. wendet dieselbe kanonische Darstellung und Policy-Revision an;
5. liefert entweder deterministische Bytes oder einen typisierten Fehler.

Ein Stub ohne funktionierenden, autorisierten Expansionspfad zählt nicht als
erfolgreiche Cross-Agent-Auslieferung.

## Attribution und Wertmessung

Receipts unterscheiden `original_agent_id` vom tatsächlich bedienenden
`serving_agent_id`. Bei direkter Registry-Bedienung können beide identisch sein;
bei Proxy oder Team-Dienst sind sie verschieden. Gemessen werden mindestens
Referenzen, Expansionen, Validierungsfehler, Invalidierungen und vermiedene
Tokens. Einsparung ist `max(original_tokens - reference_tokens, 0)` und wird
nur nach erfolgreich bedienter Referenz gezählt. Pro-Reporting aggregiert diese
Werte ohne Content, Pfade oder Agentenkennungen offenzulegen.

## Vorhandene Implementierung

Die Engine besitzt zwei Generationen von Teilprimitiven:

- `BuiltinDeliveryRegistry` speichert pro Prozess/Daemon einen 96-Bit-
  BLAKE3-Präfix plus Pfad, Original-Agent/Conversation, `mtime`, TTL,
  Relay-Inhalt und Tokenzähler. Kapazitäts- und TTL-Eviction sind getestet.
  Der live V1-Pfad gibt bei einem Treffer bis zu 8 KiB `relay_content` direkt
  aus; er verifiziert dabei weder einen vollständigen Content-Digest noch eine
  autoritative Scope-Berechtigung.
- `DeliveryEntryV2` führt versionierte kanonische Request-Keys, vollständige
  BLAKE3-Content-Handles, Operationstypen, Validatoren, Producer-Felder und
  L1/L2/L3-Statistiken ein. Das Disk-Tier persistiert derzeit nur Metadaten;
  Blob-Speicherung und Expansion sind nicht implementiert.
- File-, Shell-, Search-, Directory- und Compose-Builder binden ihre jeweils
  relevanten Inputs deterministisch an den Request-Key.
- Nur V2 unterdrückt derzeit Wiederverwendung über verschiedene Conversations.
  Der live, standardmäßig aktive V1-Registry-Pfad wählt dagegen gerade Einträge
  anderer Agenten/Conversations. Seine Daemon-Endpunkte übernehmen Requester-ID
  und Conversation-ID ungeprüft aus dem Request; bei leerem Pfad entfällt sogar
  der Pfadanteil des Lookups und es wird nur der 96-Bit-Präfix verglichen.

## Offene Abnahme-Gates

Der bestehende Stand ist kein vollständiges V4-/Pro-Gate:

- `DeliveryEntryV2` enthält keine autoritative Account-, Workspace- oder
  Project-ID und keine Privacy-Klassifikation.
- Ein Pfad im Key verhindert keine Cross-Project- oder Cross-Account-Leaks.
- V1 akzeptiert selbstbehauptete Requester-Identität, erlaubt einen leeren Pfad
  und liefert Relay-Bytes ohne Serve-Time-Prüfung des vollständigen Digests.
- Der V2-Stub ist Text, aber kein opaker, erneut autorisierter Expansion-Handle;
  persistierter Blob-Content und Expansion sind nicht implementiert.
- `mtime`-Validatoren beweisen keine Bytegleichheit nach grober Zeitauflösung;
  immutable Validatoren benötigen eine dokumentierte Vertrauensgrundlage.
- Original- und Serving-Agent werden im älteren Event nicht unabhängig
  bestimmt.
- Die abgeleitete Producer-ID ist nicht durchgängig eine stabile Agent Identity:
  `CLAUDECODE` ist typischerweise nur ein Presence-Flag, der lokale Fallback
  prozessgebunden.
- V2 zählt den vollen Original-Tokenwert bereits im Coordinator als gespart,
  bevor `entry_allows_stub` die Auslieferung erlaubt; Referenzkosten werden
  nicht abgezogen. Das widerspricht der oben definierten Wertmessung.
- Für den aktiven `delivery-v2`-Diskpfad ist weder Startup-Trimming noch das
  konfigurierte Byte-Limit wirksam belegt; die vorhandene Größenprüfung
  vergleicht zudem Token mit Byte-Limit.
- Eine explizite Invalidierungs-API fehlt. V1 ignoriert beim Check den
  übergebenen `mtime`; Key-Drift/TTL ersetzen keine Content-Invalidierung.
- Es gibt kein Pro-Entitlement-Gate: Delivery ist über Konfiguration
  standardmäßig aktiv. Pro-Value-Reporting ist nicht implementiert; vorhandene
  Stats werden lediglich geloggt.
- Klassifikations-, Invalidierungs-, Restart-, Manipulations- und echte
  Zwei-Agenten-E2E-Tests fehlen als integrierter Release-Nachweis.
- Authentisierte Daemon-Endpunkte, vollständige Digest-Verifikation und eine
  begründete Kollisionsgrenze für alte 96-Bit-Präfixe fehlen.

## Mindesttests

- gleicher Digest + gleicher autorisierter Scope → Referenz und Expansion;
- geänderter Inhalt bei gleichem Pfad/`mtime` → Miss und Neumaterialisierung;
- anderer Project-, Workspace- oder Account-Scope → fail-closed Miss;
- verweigerte Klassifikation → keine Aufnahme und keine Auslieferung;
- manipulierter Handle/Digest → typisierter Fehler, keine Einsparungsbuchung;
- TTL, Capacity und explizite Invalidierung → keine veraltete Expansion;
- Daemon-Restart und konkurrierende Agenten → deterministisches Ergebnis;
- Pro deaktiviert/abgelaufen → keine bezahlte Capability;
- Telemetrie/Reporting → korrekte Aggregate ohne Content- oder ID-Leak.

Erst wenn diese Gates gemeinsam mit den allgemeinen V4-Hard-Gates bestanden
sind, darf Cross-Agent Context Delivery als Pro-fertig oder umsatzbereit gelten.
