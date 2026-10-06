# Agent Connectors

Status: Adapterschicht implementiert (`core::agent_connector`) mit Claude Code,
Codex und Cursor. Als produktisierte Ausführungsschnittstelle mit
Conformance-Suite nicht abgenommen.

## Aufgabe

Der `AgentConnector` ist die **interne Ausführungs-Adapterschicht**: eine
stabile Schnittstelle, über die der Execution Autopilot
([execution-autopilot.md](execution-autopilot.md)) und der Work Graph
([work-graph.md](work-graph.md)) externe Agent-Laufzeiten begrenzt ausführen.

Weitere Laufzeiten werden ausschliesslich über denselben Vertrag ergänzt — nicht
über Sonderpfade.

## Kanonische Objekte

```text
AgentInfo   { name, version?, path, capabilities[], available }

TaskRequest { id, prompt, working_dir, timeout_ms,
              model?, max_turns?,
              profile_name?, profile_hash?, delivery_profile? }

TaskResult  { task_id, agent, model, success, exit_code,
              stdout, stderr, duration_ms,
              tokens_used?, provider_cost_micros?,
              execution_receipt_ref?, termination? }

TaskTermination { Exited | Signalled | TimedOut }

TokenUsage  { input_tokens, output_tokens,
              cache_read_tokens, cache_write_tokens }

ChildDeliveryProfileV1 { task_id, expires_at, profile }
```

## Vertragsanforderungen

Ein Connector muss offenlegen: stabile Identität und Version, verfügbare
Fähigkeiten soweit ermittelbar, Health, Ausführungsunterstützung, Cancellation,
Timeout, Zugwahl-Obergrenze, Arbeitsverzeichnis, Profil-Propagation,
Umgebungsisolierung, Token-Verbrauch, providerseitig gemeldete Kosten soweit
verfügbar, Receipt- und Evidenzextraktion sowie Fehlerklassifikation.

Der Trait deklariert dafür:

```rust
trait AgentConnector: Send + Sync {
    fn info(&self) -> AgentInfo;
    fn name(&self) -> &'static str;
    fn health_check(&self) -> Result<bool>;              // 2 s Vorgabe
    fn health_check_with_timeout(&self, timeout_ms: u64) -> Result<bool>;
    fn execute(&self, request: &TaskRequest) -> Result<TaskResult>;
    fn supports_model_selection(&self) -> bool { false } // konservativ
    fn supports_turn_limit(&self) -> bool { false }      // konservativ
}
```

Beide Fähigkeitsabfragen sind standardmässig `false`: Ein Connector muss
Unterstützung ausdrücklich erklären, statt sie stillschweigend zu erben.

`validate_request` weist vor dem Prozessstart ab, was die gewählte CLI nicht
erzwingen kann — leere Task-ID, leerer Prompt, nicht-positives Timeout, leerer
Modellname, Zugwahl-Obergrenze `0` sowie jede gesetzte Obergrenze bei einem
Connector, der keine unterstützt.

`tokens_used` unterscheidet Cache-Lesen von Cache-Schreiben; erst dadurch ist
Prompt-Cache-Ökonomie überhaupt bewertbar. `provider_cost_micros` ist laut
Kommentar am Feld ausdrücklich eine providerseitig gemeldete Abrechnung und
**nie** eine tabellenbasierte Schätzung; `execution_receipt_ref` bleibt
abwesend, solange die CLI-Ausgabe nicht beide Beobachtungen trägt.

## Harte Invarianten

- **Strukturierte Quelle schlägt Prosa.** Terminalausgabe wird nicht als
  autoritative Verbrauchsangabe geparst, wenn eine strukturierte Quelle
  existiert.
- **Kind erbt keine Zustellautorität.** `ChildDeliveryProfileV1` ist an Task,
  Projekt und Ablaufzeitpunkt gebunden; ein Kind kann die Autorität des
  Elternprozesses nicht übernehmen. Der Test
  `child_cannot_inherit_parent_delivery_authority` hält das fest.
- **Umgebungsvariablen nur für benannte Profile.**
  `apply_profile_environment` setzt `LEAN_CTX_PROFILE` ausschliesslich bei
  explizit benanntem Profil, nie implizit.
- **Terminierung ist typisiert.** `TaskTermination::from_process` unterscheidet
  regulären Abschluss von Timeout; „irgendwie beendet" ist kein Zustand.
- **Cancellation erreicht den Prozess** und bestätigt dessen terminalen
  Zustand; verwaiste Kindprozesse sind ein Fehler, kein Nebeneffekt.
- Jede Ausführung ist einem Receipt zuordenbar.

## Pro-Verhalten

Pro darf genau einen begrenzten Agent-Pfad automatisch wählen und ausführen:

```text
Aufgabe: fehlschlagenden Rust-Test untersuchen und beheben

Kandidaten:
- Claude Code / Sonnet
- Codex / GPT
- Cursor-Agent

Autopilot:
- wählt Codex
- 40k Kontextbudget
- höchstens 12 Züge
- gewähltes Projektprofil
- führt aus
- sammelt Receipt
- bewertet Tests
- lernt das Ergebnis
```

Die manuelle Festlegung auf Agent oder Modell bleibt jederzeit möglich und
bindend.

## Vorhandener Engine-Stand

- Connectors: `claude.rs`, `codex.rs`, `cursor.rs`.
- Quervorhandene Bausteine: `detection.rs` (Erkennung), `capability.rs`
  (Fähigkeiten), `receipt.rs` (kanonische Receipts, u. a.
  `canonical_receipt_digest` und `verify_bundled_receipt`), `timeout.rs` mit
  `run_with_timeout_cancellable` und einem durable Guard, der das Reaping
  bestätigt.
- Tests decken Request-/Result-Roundtrip, Profil-Umgebung, Delivery-Bindung und
  Ablauf ab.

## Offene Abnahme-Gates

- Eine Connector-**Conformance-Suite**, die jeden Connector gegen denselben
  Vertrag prüft, fehlt.
- Fähigkeits- und Modellermittlung ist nicht für alle Connectors belegt.
- Providerseitige Kostenmeldung ist nicht durchgängig strukturiert verfügbar.
- Fehlerklassifikation ist nicht als vollständige, stabile Taxonomie abgenommen.
- Pairing und entfernte Ausführung sind offen: gefordert sind authentifiziertes
  Pairing, Geräte- und Nutzeridentität, verschlüsselter Kanal, Widerruf,
  Replay-Schutz, begrenzte Job-Anfragen und Cancellation. Eine als Pairing
  getarnte entfernte Shell ist ausdrücklich ausgeschlossen.

## Mindestabnahme

Jeder unterstützte Connector muss dieselbe Suite bestehen: Health melden,
Modellwahl und Zugwahl-Obergrenze korrekt annehmen oder begründet ablehnen,
in einem gesetzten Arbeitsverzeichnis mit isolierter Umgebung ausführen,
Token-Verbrauch strukturiert melden, ein kanonisches Receipt liefern, auf
Cancellation innerhalb der Frist terminieren, den Prozessabgang bestätigen und
einen Timeout typisiert von einem Fehlschlag unterscheiden.
