# OCLA Capability Fabric

Status: kanonischer V4-Zielvertrag mit vorhandenen Teilprimitiven. Kein
Release- oder kommerzieller Verfügbarkeitsnachweis.

## Rolle im V4-System

OCLA beschreibt, entdeckt und filtert ausführbare Fähigkeiten. Es ist das
Interoperabilitätsfundament zwischen Task/Execution Plan, Connectoren,
Modellen, Providern, Validatoren und Addons. OCLA trifft nicht selbst die
gesamte Produktentscheidung: Decision Spine plant, Policy autorisiert,
Execution Autopilot wählt und führt aus, Receipts/Outcomes bewerten das Ergebnis.

Der interne `ocla_bus` transportiert semantische Runtime-Ereignisse. Er ist
nicht der lokale oder Team Agent Bus und darf nicht zu einer zweiten
Orchestrierungsquelle werden.

## OCP-Beziehung

OCP bleibt das untergeordnete offene Protokoll und dessen Upstream die
Schema-Quelle, soweit verfügbar. OCLA nutzt diese stabilen Datentypen für
Capability- und Execution-Interop. Vendoring pinnt eine bekannte Version;
Schemaänderungen werden über Upstream-RFC/Versionierung übernommen oder als
bewusste, kompatible Projektion dokumentiert. LeanCTX darf OCP-Semantik nicht
still forken.

## Manifestvertrag

`CapabilityManifestV1` ist die kanonische technische Beschreibung:

```text
CapabilityManifestV1 {
  schema_version, capability_id, provider, kind, version
  surfaces, support_matrix
  local, remote, reversibility, determinism, data_movement
  supported_classifications, measurement_support
  input_schema_ref, output_schema_ref, conformance_version
}
```

Die Umstellung von `OclaService::manifest` auf `OclaResult<CapabilityManifestV1>`
ist ein expliziter Rust-API-3-Bruch: Aufrufer migrieren durch `?`, während der
Wire-Vertrag `ocla/v1` und die `CapabilityManifestV1`-Felder unverändert bleiben.
Service-Implementierungen müssen ein explizites Manifest liefern; der Default
meldet `Unavailable` statt eine Platzhalter-Identität zu erzeugen. Fehlende oder
ungültige Manifeste lassen Discovery und Katalog typisiert fehlschlagen, ohne
Teilresultat oder Rückfall auf ein unvalidiertes Rohmanifest. Gültige Manifeste
werden weiterhin kanonisch normalisiert und serialisiert.
Die eingebetteten `CapabilityAdapter::manifest`-APIs bleiben in diesem
Teilschritt unverändert; ARCH03 ist damit noch nicht abgeschlossen.

V4 muss mindestens Context Source, Read/Compression Strategy,
Search/Retrieval, Validator, Model, Provider, Agent Runtime, Scheduler,
Shell-Output Optimization, Addon und Remote Capability ausdrücken. Erweiterung
erfolgt additiv oder über eine neue Schema-/Conformance-Version; unbekannte
kritische Semantik darf nicht still ignoriert werden.

Eine registrierte Capability besitzt zusätzlich einen operativen Status mit
Health, letzter belegter Conformance, Grenzen und Scope. `available` aus einer
statischen Registrierung genügt nicht als Laufzeitgesundheit.

## Registrierung und Katalog

Interne Built-ins, AgentConnectoren, Modelle/Provider, Validatoren und externe
Addons liefern dasselbe Manifestformat. Registrierung validiert eindeutige
ID/Version, Provider, Schemas, Oberflächen, Lokalität, Datenbewegung,
Klassifikationen und Conformance. Konflikte oder unvollständige sicherheits-
relevante Felder werden fail-closed abgelehnt.

Die erforderlichen lokalen Adapter werden vor Freigabe der Registry als ein
Satz registriert. Scheitert eine Registrierung, wird der ganze Satz verworfen;
Katalog, Discovery und Health-Prüfung liefern einen typisierten Fehler statt
eines Teilkatalogs. Nachregistrieren kann diese fehlgeschlagene Initialisierung
nicht freischalten. Registrierungsfehler werden ohne Panic weitergegeben;
die vorhandene Manifestvalidierung bleibt die einzige Prüfautorität.
Die getrennten Helfer zum Laden eingebetteter Manifeste verwenden weiterhin
`expect`; ihre fallible Ablösung ist noch offen. Eine vollständig panic-freie
Initialisierung ist damit noch nicht nachgewiesen.

Der öffentliche `TechnicalCatalogue` enthält ausschließlich veröffentlichbare
technische Fakten. Kundenpreise, beobachtete Qualität, Kapazität,
Zuverlässigkeit und gelernte Gewichte gehören nicht hinein. Health ist
kurzlebiger Zustand und darf das versionierte Manifest nicht mutieren.

## Candidate Selection

Auswahl nach Namen allein ist verboten. Die Pipeline lautet:

1. TaskEnvelope und ExecutionPlan validieren.
2. technisch kompatible Manifestversionen und Oberflächen ermitteln;
3. Entitlement, Tenant-/Workspace-Scope und harte Policy anwenden;
4. Taskklasse, Komplexität, Risiko, Klassifikation, Lokalität,
   Reversibilität und Determinismus filtern;
5. Health, Limits und verfügbare Budgetkapazität prüfen;
6. zulässige Kandidaten nach Scheduler-Vertrag bewerten;
7. gewählten und sicheren Fallback als versionierte Decision referenzieren.

Fehlende Metadaten für eine aktive harte Einschränkung bedeuten Ausschluss.
Fallback darf Provider-, Region-, Klassifikations-, Kosten- oder
Entitlement-Policy niemals umgehen.

## Produktgrenze

- Free erhält den deterministischen, eingefrorenen Referenzscheduler aus
  öffentlichen technischen Fakten, ohne verborgene kundenspezifische Gewichte.
- Die optionale private Free Intelligence Runtime darf autorisierte persönliche
  und gemeinsame Outcome-Daten adaptiv verwenden; Privacy und Mindest-Evidenz
  bleiben erforderlich. Lokale Intelligenz ist keine bezahlte Freischaltung.
- Cloud/Team Scale verkauft verwaltete Ressourcen und Betrieb, nicht die
  grundlegende lokale Zusammenarbeit oder Intelligenz.
- Enterprise ergänzt zentrale Governance, Regionen, Provider-Allowlisten und
  Auditierbarkeit.

Die offene Capability-Schnittstelle bleibt ohne private Runtime nutzbar;
private Free-Algorithmen und bezahlte Services/Governance sind getrennte
Produktgrenzen, keine heimlich inkompatiblen Manifeste.

## Receipts, Messung und Datenschutz

Jede Auswahl referenziert Kandidatenmenge, Ausschlussgründe, Policy- und
Entitlement-Entscheid, Scheduler-Version und Fallback. Ausführung erzeugt ein
Receipt mit Capability-/Provider-/Model-Version, Grenzen, Token-/Kosten-/
Latenzwerten und Outcome-Referenz. Payload-Bytes und Secrets gehören weder in
Manifest noch Decision-Receipt.

Adaptive Beobachtungen bleiben nach Taskklasse und Rewardart getrennt;
inkomparable Qualität, Kosten oder Latenz werden nicht ohne Evidenz zu einem
einzigen Score vermischt. Telemetrie respektiert Opt-out, Klassifikation,
Retention und Account-/Workspace-Löschung.

## Historischer Ausgangsstand vor der Stage-2-Konsolidierung

Die folgenden Bestands- und Lückenlisten dokumentieren den ursprünglichen
Auditstand, nicht den aktuellen Code. Inzwischen verwendet der kanonische
Katalog explizite Built-in-Manifeste und die reale AdapterRegistry;
`discover_compatible` normalisiert und prüft Manifestkonflikte vor der Policy.
Das Protokoll besitzt 20 Capability-Kinds und zusätzliche semantische
Validierungen. Aktuelle Regressionen stehen in `core/ocla/registry.rs`,
`core/ocla/adapters/registry.rs` und `ocla_manifest_conformance`.
Eine vollständige V4-Nutzerabnahme folgt daraus nicht.

- Das Protokoll definiert `CapabilityManifestV1` mit acht technischen Kinds,
  Surface-Schemas, Lokal/Remote, Reversibilität, Determinismus,
  Datenbewegung, Klassifikationen und Messunterstützung.
- `OclaRegistry` exponiert 14 Built-in-Services in deterministischer Reihenfolge:
  ObservationHook, UsageSink, MetricsExporter, SavingsLedger,
  IntentClassifier, OutcomeTracker, CompressionProvider, ResponseOptimizer,
  EfficiencyAnalyzer, ConfigTuner, ExperimentRunner,
  ConnectorScheduler, AgentGateway und DeliveryRegistry.
- Mit Ausnahme des CompressionProvider verwenden die Services derzeit den
  Default-Manifestpfad: `version: 0.0.0`, `supported: false`, generischer
  `context_source` und eine Placeholder-ID. Service-`available` und technisch
  ausführbares Capability-Manifest widersprechen sich damit für 13 Built-ins.
  (Der ModelRouter wurde mit dem automatischen Modell-Routing in v4 entfernt.)
- `TechnicalCatalogue` trennt veröffentlichbare technische Fakten von privaten
  kommerziellen/operativen Schedulerdaten.
- `ReferenceScheduler` generiert höchstens 100 Kandidaten deterministisch,
  validiert Manifeste, filtert über `PolicyConstraints` und erstellt
  deterministische Plan-/Decision-Referenzen.
- Harte Policy-Prüfungen können Provider, Region, Klassifikation, Kosten,
  Qualität, Latenz, Lokalität und Reversibilität fail-closed verlangen.

## Historische Lückenliste des ursprünglichen Audits

- Die interne `OclaCapabilityKind`-Taxonomie der 15 Built-ins und die
  öffentliche `CapabilityKind`-Taxonomie sind nicht zu einem vollständigen
  V4-Klassenmodell normalisiert; mehrere verlangte Klassen sind nur `Tool` oder
  `Other` abbildbar.
- `CapabilityManifestV1::validate` prüft derzeit im Kern nur Schema-Version und
  dass mindestens local oder remote gesetzt ist. ID-Felder, Version, Provider,
  Surface-Matrix, Schema-Refs, Klassifikationen und Conformance benötigen
  strengere semantische Validierung.
- Die globale Built-in-Registry ist statisch und markiert viele Fähigkeiten als
  verfügbar; echtes Health-, Degradation- und Conformance-Enforcement ist nicht
  als durchgängige Admission belegt.
- Vierzehn Built-ins besitzen nur explizit nicht unterstützte Placeholder-
  Manifeste. Sie sind daher noch nicht als auswählbare V4-Capabilities
  registriert, auch wenn ihr interner Service-Status `available` meldet.
- Der Referenzscheduler prüft bei Candidate-Generierung nicht, ob die jeweilige
  Surface-Matrix tatsächlich `supported: true` meldet. Dadurch können die 14
  Placeholder-Manifeste als Kandidaten entstehen und ausgewählt werden.
- `discover_compatible` filtert nur Provider. Die vollständige Manifest-/
  Policy-/Entitlement-/Scope-Prüfung ist nicht an jeden Dispatch gekoppelt.
- Der Referenzscheduler erzeugt Plans mit Nullbudgets, ohne Policy-Decision-Ref
  und einem manuellen Fallback. Sichere Ausführbarkeit und ein policy-konformer
  Fallback sind nicht Ende-zu-Ende belegt.
- Der Fallback wird erst nach dem Policy-Filter erzeugt und nicht selbst durch
  `PolicyConstraints::permits` geprüft. Wenn alle Kandidaten ausgeschlossen
  werden, kann der aktuelle `leanctx/manual`-Fallback somit harte Provider-,
  Region-, Klassifikations-, Kosten-, Lokalitäts- oder Reversibilitätsregeln
  umgehen. Das ist ein fail-closed Blocker vor produktivem Dispatch.
- Region, Klassifikation, Lokalität und Reversibilität werden teilweise aus
  frei formatierten Provider-/Reference-Strings abgeleitet statt aus einem
  autoritativen typisierten Candidate-Vertrag.
- AgentConnector-, Model-/Provider-, Validator-, Addon- und Remote-Capability-
  Registrierung ist nicht als vollständiger technischer Katalog abgenommen.
- OCP-Upstream-Pin, Driftprüfung und kanonische Beziehungs-ADR fehlen als
  abgeschlossener Release-Nachweis.
- Community/Pro/Team/Enterprise-Entitlements sowie adaptive Lern- und
  Privacy-Gates sind nicht als unveränderter Gesamtpfad bewiesen.
- Conformance-, Manipulations-, Health-/Recovery-, Fallback- und echte
  Multi-Provider-/Connector-E2E-Tests fehlen als integrierte V4-Abnahme.

## Mindestabnahme

Die Phase ist erst fertig, wenn jede verlangte Capability-Klasse registriert,
versioniert, health- und conformance-geprüft ist; harte Policies und
Entitlements vor jedem Dispatch greifen; Community deterministisch bleibt;
Free-Intelligenz nur autorisierte Outcome-Daten lernt; Fallbacks dieselben Grenzen
einhalten; und Receipts die Auswahl sowie reale Ausführung reproduzierbar
belegen. Alle 15 Built-ins müssen einzeln auf echten Hauptpfaden statt nur per
Smoke-Aufruf geprüft sein.
