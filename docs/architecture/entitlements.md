# Entitlements

Status: Signierte Entitlement-Hülle, Capability-Registry und Plan-Modell
implementiert (`core::billing`, `cloud_client::entitlement_cache`,
`lean_ctx_protocol::EntitlementEnvelopeV1`). Abrechnungs- und
Trial-Ende-zu-Ende nicht abgenommen.

## Aufgabe

Entitlements beantworten genau eine Frage: **Darf dieser Lauf diese Capability
benutzen?** Sie sind kein Plan-Namensvergleich und keine Feature-Flag-Sammlung.

Kanonische Pläne: `community`, `pro`, `team`, `enterprise`. Legacy-Aliasse
existieren nur für die Migration. `supporter` ist reine Anerkennungs-Metadata
und gewährt niemals eine Capability — das steht so am Feld.

## Capability statt Planname

Verboten ist der über den Code verstreute Planvergleich:

```rust
if plan == Plan::Pro || plan == Plan::Team { … }   // nein
```

Richtig ist die Capability-Abfrage:

```rust
entitlement_allows(plan, "pro.work_graph.local")   // registry-gestützt
```

Die Registry ist die einzige Wahrheitsquelle. **Unbekannte Capabilities werden
immer verweigert** — fail closed, nicht fail open.

## Kanonische Objekte

```text
EntitlementEnvelopeV1 {
  schema_version, entitlement_id, kind
  account_id?, plan, seats, capabilities
  issued_at, not_before, expires_at, grace_until
  allowed_deployments, deployment_id?
  org_id?, workspace_id?
  signer { algorithm, key_id, public_key_digest }
  signature
}

Entitlements {
  plan, seats, hosted_index_mb, managed_connectors
  private_registry, sso_oidc, sso_scim
  audit_retention_days, revenue_share, supporter, cloud_sync
}
```

Die Hülle ist kryptografisch signiert. Zur Prüfung genügt der Laufzeit ein
öffentlicher Verifikationsschlüssel; **ein Netzwerkaufruf je Nutzung ist nicht
erforderlich**.

## Stufenzuordnung

| | Community | Pro | Team | Enterprise |
|---|---|---|---|---|
| Sitze | 1 | 1 | unbegrenzt | unbegrenzt |
| Hosted Index (MB) | 0 | 1 000 | 20 000 | unbegrenzt |
| Managed Connectors | 0 | 0 | 10 | unbegrenzt |
| Private Registry | nein | nein | ja | ja |
| SSO OIDC | nein | nein | ja | ja |
| SSO SCIM | nein | nein | nein | ja |
| Audit-Aufbewahrung (Tage) | 0 | 0 | 365 | 3 650 |
| Revenue Share | nein | nein | ja | ja |
| Cloud Sync | nein | ja | ja | ja |

Diese Tabelle gibt `Plan::entitlements()` wieder. Sie ist eine **Obergrenze**,
kein Ersatz für die signierte Menge.

## Harte Invarianten

- **Stufen-Obergrenzen erweitern niemals eine signierte Teilmenge** und
  ersetzen keine endliche Sitzzahl. Der Kommentar am Code sagt das ausdrücklich.
- **Community-Capabilities sind immer erlaubt**, auch ohne gültige Hülle. Alles
  darüber verlangt eine gültige, aktuelle, gebundene Hülle.
- **Ohne aktuellen Snapshot fällt alles auf Community zurück** (`is_current`).
- **Eine gehaltene Instanz kann ihr signiertes Zeitfenster nicht verlängern.**
  Es findet dabei kein Netzwerkzugriff statt.
- Deployment-Beschränkungen, Organisations- und Workspace-Bindung werden
  mitgeprüft, nicht nur Plan und Ablauf.

## Ablauf und Grace

Ein Netzwerkausfall darf den Entwicklerfluss nicht brechen; dafür existieren
zwischengespeicherte Hülle und Grace-Fenster (`expires_at` → `grace_until`).

Nach endgültigem Ablauf gilt:

- Nutzerdaten werden **nie** beschädigt oder gelöscht.
- Adaptives Pro-Verhalten fällt auf deterministisches Community-Verhalten
  zurück — siehe [autopilot.md](autopilot.md).
- Automatische Cloud-Schreibvorgänge enden sauber.
- Manuelle Exporte bleiben verfügbar.
- Geteilte Team-Operationen werden gemäss Produktpolitik lesend oder
  eingeschränkt.
- Der Nutzer sieht einen klaren, nicht-destruktiven Zustand.

## Vorhandener Engine-Stand

- `EntitlementEnvelopeV1` mit Ed25519-Signatur, kanonischer Serialisierung und
  Schlüssel-Digest-Bindung.
- `entitlement_cache` mit `allows`, Snapshot-Guard, Grace-Berechnung und
  Rückfall auf Community.
- Registry-gestützte Capability-Auflösung mit Mindestplan je Eintrag.
- Beispiel eines echten Gates: `ctx_work_graph` verlangt
  `PRO_LOCAL_WORK_GRAPH`, erlaubt aber `cancel` auch nach Ablauf, damit eigene
  laufende Arbeit stoppbar bleibt.
- Tests belegen, dass gefälschte Legacy-Plan-Dateien keine Autorisierung
  erzeugen.

## Offene Abnahme-Gates

- Der Pro-Trial (Aktivierung, serverseitige Konfiguration, Trial-Ende-Bericht)
  ist nicht abgenommen; Kennzahlen dürfen nur mit echter Evidenz behauptet
  werden.
- Der serverseitige Preiskatalog mit Laufzeit-Anzeigefallback ist nicht belegt.
- Offline-Enterprise-Entitlements sind modelliert, aber nicht als Betriebspfad
  abgenommen.
- Sitzzählung und Workspace-Bindung sind nicht mehrbenutzerseitig verifiziert.
- Die Kopplung Entitlement → adaptiver Planer ist nicht Ende-zu-Ende belegt.

## Mindestabnahme

Der Master-E2E muss zeigen: eine gültig signierte Pro-Hülle schaltet genau die
signierten Capabilities frei; eine unbekannte Capability wird verweigert; eine
manipulierte Signatur wird abgewiesen; nach `expires_at` trägt das
Grace-Fenster, nach `grace_until` fällt der Lauf auf Community zurück, ohne
Daten anzutasten; eine Stufen-Obergrenze erweitert die signierte Menge nicht;
und der Nutzer sieht in jedem dieser Zustände eine klare, nicht-destruktive
Meldung.
