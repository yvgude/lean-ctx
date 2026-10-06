# Context Autopilot

Status: Kernprimitive implementiert (`core::context_kernel::autopilot`), als
Produktstufe nicht abgenommen. Kein Release-, Team- oder Pro-Verfügbarkeits-
nachweis.

## Aufgabe und Produktstufen

Der Context Autopilot entscheidet, **welcher Kontext** für eine Aufgabe
beschafft, reduziert, vorgeladen und aufbewahrt wird. Er ist nicht der
Execution Autopilot (Wahl von Agent, Modell und Ausführungspfad — siehe
[execution-autopilot.md](execution-autopilot.md)) und nicht der Decision Spine,
der beide einbettet.

- Community erhält einen statischen, deterministischen Planer. Er lernt nichts
  Personalisiertes.
- Pro erhält den adaptiven Planer, der aus tatsächlichen Ergebnissen lernt.
- Team und Enterprise ergänzen Governance und geteilte Policy, keine
  konkurrierende Planerarchitektur.

Die Trennung ist im Code als `PlannerTier` materialisiert; beide Planer teilen
denselben `ContextPlan`-Vertrag.

## Kanonische Objekte

```text
// Der Kernel-Plan ist der eine kanonische semantische Kontextplan.
type ContextPlan = ContextPlanV1;   // aus core::context_kernel::types

AutopilotDecision {
  decision_id, tier, context_plan
  task_class, context_need, strategy
  read_policy, retrieval_policy, memory_policy
  routing_policy, tool_surface, checkpoint_policy
  confidence_milli, reasons, preloads
  economics, shadow?, policy_observations
}

AdaptiveLearningState {
  accepted, rejected, partial
  preload_hits, preload_misses
  learned_mode?, modes { mode -> ModeLearningState }
  processed_receipts        // Replay-Schutz
}

ModeLearningState { accepted, rejected, partial }

PlannerTier { Community | AdaptivePro }
```

Der semantische Kontextplan gehört dem Context Kernel. Der öffentliche
`ContextPlanProjectionV1` ist seine Wire-Projektion, kein weiterer Planer.
Die Policy-Felder daneben sind Entscheidungsbegründungen, nicht ein zweiter Plan.
`processed_receipts` ist der Grund, weshalb dasselbe Receipt nicht zweimal
trainieren kann.

`decision_id` deckt die materiellen Steuergrössen ab. Ändert sich der Plan nach
der Entscheidung, ist die Bindung ungültig — der Test
`decision_id_covers_material_controls` hält das fest.

## Harte Invarianten

- **Expliziter Nutzerwille schlägt Lernen.** Nur eine Sicherheits-Policy steht
  darüber; nichts anderes.
- **Konfidenz-Rückfall.** Unterhalb von 650 von 1000
  (`DEFAULT_CONFIDENCE_THRESHOLD_MILLI`) fällt der adaptive Planer auf das
  deterministische Community-Verhalten zurück statt aggressiv zu improvisieren.
- **Monotone Kosten/Wert-Schranke.** Eine Transformation wird nur gewählt, wenn
  ihr Nettowert strikt positiv ist (`break_even`, `AutopilotEconomics`).
  Annotations- und Tool-Schema-Overhead zählen mit.
- **Community ist byte-deterministisch** und ignoriert jeden Lernzustand.
- **Lernen nur aus validierter Evidenz.** Ein Ergebnis trainiert genau einmal,
  ist modus-attribuiert, und ein Receipt ohne gültige Entscheidungsbindung wird
  abgewiesen. Unbekannte Outcomes trainieren nicht.
- **Beschränkter Lernzustand.** Höchstens 10 000 Beobachtungen und 32 Modi,
  live wie beim Import. Der Replay-Speicher schlägt bei Kapazität fehl statt zu
  überschreiben.
- **Preloads sind begrenzt und abbrechbar.** Höchstens 4 Items, 4 096 Token
  gesamt und 2 048 Token je Item; die Grenzen gelten hart, und wiederholt
  nutzlose Vorhersage schaltet die Prädiktion ab.
- **Remote-Preload erfordert Einwilligung.**
- **Shadow-Metriken sind rein diagnostisch** und dürfen ausführbare Policy nie
  umgehen.

## Eingaben und Ausgaben

Eingaben sind vorhandene Primitive: Aufgabe und strukturierte Intention,
Projektzustand, Kontextdruck, Context Ledger und IR, jüngste Reads und
Dedup-Zustand, Knowledge, Gotchas, Graph, Suchindex, ModePredictor, adaptive
Schwellen, Korrektur- und Bounce-Signale, Provider/Modell, Token-Preise,
Prompt-Cache-Ökonomie, Tool-Schemata sowie historische und akzeptierte
Outcomes.

Entscheiden darf der Autopilot unter anderem: statische oder adaptive
Lesestrategie, konkreter Read-Modus, Nutzen von semantischer Suche und Graph,
Grösse der exponierten Tool-Oberfläche, Proxy-only-Pfad, Angemessenheit des
aktuellen Modells, Kontext-Recall, Wissenskonsolidierung, Checkpoint-Erzeugung
und die Verwerfung einer früheren Strategie aufgrund von Korrekturevidenz.

## Sicherheitsmodell

Verlustbehaftet wirkende Reduktion braucht einen deterministischen
Wiederherstellungspfad, solange die Quelle existiert; dafür dienen die
bestehenden Archiv- und CCR-Mechanismen. Für wesentliche adaptive
Entscheidungen wird eine kontrafaktische Baseline als `ShadowComparison`
mitgeführt, um beantworten zu können, ob Pro geholfen oder geschadet hat.

`ContextKernel::plan_with_policy` prüft die übergebene Policy vor
Content-Deduplizierung und Auswahl. Abgewiesene Kandidaten verbrauchen kein
Auswahlbudget und können keinen erlaubten Kandidaten gleichen Inhalts verdrängen.
Mehrdeutige Kandidaten-IDs werden gemeinsam abgewiesen; je ID bleibt genau eine
diagnostische Exklusion. Der Kernel berechnet Auswahl, Provider-Statistik und
Budget gemeinsam; bereits verbrauchte Tokens und das Policy-Limit bleiben erhalten.
Autopilot übernimmt diesen Plan und projiziert nur die Policy-Beobachtungen.

Neue Plan-IDs verwenden gerahmtes JSON-Material
`leanctx.context-plan-identity/v2` und den vollständigen BLAKE3-Digest. Aufgabe,
Query, Projekt, angefragtes und verwendetes Budget, Policy und endgültige Auswahl
sind gebunden; Trennzeichen in Eingaben können keine Feldgrenzen verschieben.
Bestehende IDs bleiben opake Referenzen und werden beim Lesen nicht umgeschrieben.
Die öffentliche V1-Projektion erhält dadurch keine zusätzlichen Wire-Felder.
Receipt-IDs behalten unabhängig davon ihr bisheriges Hashformat, damit ein
erneut ausgewerteter historischer Plan denselben Lookup-/Replay-Schlüssel ergibt.

Das kanonische Budget zählt die tatsächlich ausgewählten Views; der ältere
`PolicyFilter::apply` bleibt ein kompatibler Filter über rohe Prefix-Schätzungen,
nicht die ausführbare Planungsinstanz. Liegt eine Grenze unter bereits erfasstem
Verbrauch, bleibt dieser Verbrauch sichtbar und die zusätzliche Allokation ist
null. `policy_observations` enthält auch Integritätsablehnungen des Kernels,
etwa nicht eindeutige IDs oder ungültige Scores, nicht nur konfigurierte Regeln.

Beide Runtime-Adapter laden dieselbe globale `kernel-policy.toml` aus dem
konfigurierten lean-ctx-Konfigurationsverzeichnis vor der Kandidatenabfrage.
Nur eine fehlende optionale Datei verwendet die Standard-Policy. Ungültiges
TOML, unbekannte Felder, Lesefehler und nicht reguläre Dateien unterbinden die
Kontextanreicherung; der Task-Adapter liefert dafür einen typisierten Fehler.
Der kompatible `PolicyFilter::from_config` sperrt bei Ladefehlern alle Kandidaten;
neue Aufrufer verwenden `try_from_config`, um den Fehler auswerten zu können.
Symlinks müssen vor der v4-Migration durch eine geprüfte reguläre Policy-Datei
ersetzt werden; weder Quell- noch Zieldatei wird automatisch verändert.
Die Policy wird nicht stillschweigend in eine zweite Projekt-/Org-Hierarchie
umgedeutet. Deren bestehende Governance-Regeln bleiben gesondert zuständig.

Bei gesetztem `retention_days` prüft derselbe Kernel vor Deduplizierung und
Auswahl das Alter aus `Freshness.created_at` gegen `TaskEnvelopeV1.created_at`
des zugelassenen Tasks. Provenienz bleibt Herkunftsmetadatum, keine zweite Uhr.
Die Aufbewahrungsgrenze und ein gesetztes Kandidaten-TTL sind strikt: Genau
an der Grenze ist ein Objekt abgelaufen, bei null Tagen bleibt keines erhalten.
Fehlende, ungültige oder zukünftige Kandidatenzeiten werden ohne Echo des
Zeitstempels ausgeschlossen; ungültige Taskzeiten verhindern die Planung.
Legacy-Aufrufer ohne Referenzzeit dürfen bei aktiver Retention keine Kandidaten
freigeben. Der explizite Prüfzeitpunkt bindet die Planidentität nur bei
konfigurierter Retention; ohne Retention bleibt das bisherige Identitätsmaterial
unverändert. Die Prüfung filtert Kontext, sie löscht keine gespeicherten Daten.
Der Ledger-Adapter projiziert gespeicherte Unix-Sekunden in RFC3339; das
persistierte Ledger-Format bleibt unverändert. Nicht darstellbare Zeiten bleiben
ungültig und werden bei aktiver Retention nicht durch einen aktuellen Wert ersetzt.

Der semantische Kernelplan bewahrt zusätzlich je Objekt eine interne, versionierte
Origin-Abbildung: Die registrierte `CandidateProvider::provider_id` wird bei der
Enumeration gebunden, während `source` als Legacy-Alias sowie `content_ref`,
Sensitivität und Provenienz des Objekts unverändert erhalten bleiben. Mehrere
Origins unter einer Objekt-ID bleiben als Ablehnungsbeleg erhalten und fließen
in die Planidentität ein; die öffentliche V1-Projektion erhält dadurch keine
neuen Felder.

Die Kernelplan-Konstruktion liefert jetzt ein `Result`: Auch die öffentliche
`ContextKernel::plan`-Methode sowie `plan_with_policy_at` geben bei einer
inkonsistenten Compiler-Korrelation einen typisierten `KernelPlanError` zurück.
Ein vom Compiler nicht angebotener Kandidat oder fehlende Provider-Statistik
wird nicht als leerer Erfolgsplan verborgen; der zugelassene Task-Pfad verwirft
die Planung und setzt den Handoff auf `Suppressed`. Der Legacy-Adapter bildet
denselben Fehler kompatibel auf `None` ab und startet keinen zweiten Planer.

Auch die Legacy-Funktionen `degrade_plan` und `fallback_plan` liefern jetzt ein
`Result`. Degradierung delegiert an den Kernel, erhält bereits verbrauchtes
Budget und alle Origins und berechnet Auswahlstatistiken erneut. Verfügbarkeit
bezieht sich auf registrierte Provider-IDs, nicht auf den Source-Alias;
fehlende/mehrdeutige Origins oder inkonsistente Abrechnung führen zum Fehler.
Gesundheitsübersicht und Auswahl nutzen denselben Status je Provider-ID:
widersprüchliche Duplikate und leere IDs gelten als nicht verfügbar.
Eine unveränderte Auswahl behält ihre ID. Eine echte Einschränkung verwendet
`leanctx.context-plan-derivation/v1`: ursprüngliche Plan-ID und finaler Zustand
sind gemeinsam gebunden, ohne fehlende Request-/Policy-Daten zu erfinden.
Die normale V2-Planidentität und öffentliche V1-Projektion bleiben unverändert.
`fallback_plan` erstellt über den Kernel eine neue, explizit ungescopte Anfrage
ohne Provider; sie ist kein Ersatz für einen fehlgeschlagenen vorhandenen Plan.

`Unknown` ist auch im Legacy-Feedback kein Qualitätswert: Kernel-Receipts
enthalten dafür weder Qualitätssignal noch Feedback-Zurechnung. Collector und
Lernadapter ändern keine Gewichte und initialisieren dafür keinen Speicher.
Historische JSONL-Zeilen mit `unknown` bleiben bytegetreu erhalten, werden beim
Wiederherstellen der Gewichte aber nicht mehr als Beobachtung gewertet.
Dies authentifiziert keine Legacy-Receipts: Produktlernen benötigt weiterhin
die validierte Ausführungs- und Outcome-Kette; Tool-Erfolg genügt dafür nicht.

## Vorhandener Engine-Stand

- `AutopilotController` mit `CommunityContextPlanner` und
  `AdaptiveContextPlanner`, Konfidenzschwelle konfigurierbar.
- `AdaptiveLearningState` mit Beobachtung, Modus-Attribution, Preload-Trefferrate,
  `reset`, `export_json`, `import_json` — inspizierbar, exportierbar, löschbar.
- Break-Even-Prüfung, Shadow-Vergleich, Preload-Planung mit Abbruch.
- Umfangreiche Unit-Tests decken Determinismus, Override-Präzedenz,
  Konfidenz-Rückfall, Lern-Attribution, Replay-Schutz, Kardinalitätsgrenzen und
  Preload-Deckel ab.

## Offene Abnahme-Gates

- Die im Prompt geforderte CLI-Oberfläche (`autopilot status|explain|history|
  reset|export`) ist nicht als vollständiger Befehlssatz belegt.
- Personalisierter, geräteübergreifender Lernzustand, verschlüsselte Kontinuität
  und gehosteter persönlicher Index sind nicht abgenommen.
- Predictive Preload ist implementiert, aber nicht als Produktnutzen gemessen.
- Die Kopplung an Entitlements (Pro schaltet den adaptiven Planer frei) ist nicht
  Ende-zu-Ende nachgewiesen — siehe [entitlements.md](entitlements.md).
- Aktive Inferenz und Provider-Bandit sind angebunden, aber nicht als bewährter
  Produktivpfad belegt.

## Mindestabnahme

Der Master-E2E muss zeigen: identischer Zustand ergibt bei Community
byte-identische Pläne; ein expliziter Nutzerwunsch überstimmt jede gelernte
Präferenz; niedrige Konfidenz fällt nachweislich auf Community zurück; eine
Transformation mit nicht-positivem Nettowert wird abgelehnt; ein akzeptiertes
Outcome trainiert genau einmal und überlebt einen Zustands-Restore; ein
Receipt ohne gültige Bindung wird abgewiesen; und der Nutzer kann den
Lernzustand einsehen, exportieren und zurücksetzen. Erst dann ist der Context
Autopilot für die jeweilige Produktstufe abgenommen.
### Semantic Rust construction compatibility

The core semantic `ContextPlanV1` now has kernel-owned origin metadata; external
Rust callers constructing legacy plans use `ContextPlanV1::empty` and assign
the existing public fields instead of struct literals. This is a deliberate
v4 Rust construction migration, not a claim of unchanged source compatibility.
Legacy JSON without origins still decodes, and the separately versioned public
protocol V1 projection does not gain these internal fields.
