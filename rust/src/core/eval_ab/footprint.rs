//! Footprint ablation eval (#959) — proving lean-ctx's OWN injected context earns
//! its tokens.
//!
//! lean-ctx injects three things into every session: the rules block, the MCP tool
//! schemas and the wakeup briefing. The Nisi/WorkOS "Case" talk shows that added
//! context routinely makes agents *worse* — so each element must be *proven*
//! net-positive, not assumed.
//!
//! The key reuse insight: **each element's ablation is itself an A/B** — arm A is
//! the full injected prefix, arm B is the full prefix MINUS that one element. So
//! the whole eval_ab machinery applies unchanged: the pinned [`ModelRunner`] (+
//! strict replay recording), the deterministic [`score_task`] scorers, and the
//! bootstrap-CI [`Verdict`]/[`AbReport`]. A footprint run is therefore as
//! reproducible and auditable as the context A/B (#235), and the prune
//! recommendation falls straight out of the per-element verdict + token cost.

use anyhow::Result;
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};

use crate::core::agent_identity::{hex_decode, hex_encode, verify_signature};
use crate::core::context_quality::EvidenceTier;
use crate::core::tokens::count_tokens;

use super::model::{ModelFingerprint, ModelRequest, ModelRunner};
use super::report::{AbReport, PairRecord, ReportConfig, Verdict};
use super::scorers::score_task;
use super::suite::EvalSuite;
use super::{artifact, sha256_hex};

/// Report schema discriminator + version.
const KIND: &str = "lean-ctx.footprint-report";
const SCHEMA_VERSION: u32 = 1;

/// Shared framing for every arm — only the injected PREFIX differs between A and B,
/// exactly mirroring how lean-ctx rides the host instruction file + MCP surface.
const FOOTPRINT_SYSTEM: &str = "You are an AI coding agent. Use the lean-ctx context provided above \
(rules, available tools and session memory) when deciding what to do next. Answer concisely and correctly.";

/// One element of lean-ctx's injected per-turn footprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectedElement {
    /// The tool-mapping rules block written into the host instruction file.
    Rules,
    /// The advertised MCP tool descriptions + input schemas.
    ToolSchemas,
    /// The wakeup briefing (facts / last task / decisions) injected at session start.
    Wakeup,
}

impl InjectedElement {
    /// Every element, in the stable order used for assembly + reporting.
    pub const ALL: [InjectedElement; 3] = [Self::Rules, Self::ToolSchemas, Self::Wakeup];

    /// Stable label used in reports + the determinism digest.
    pub fn label(self) -> &'static str {
        match self {
            Self::Rules => "rules",
            Self::ToolSchemas => "tool_schemas",
            Self::Wakeup => "wakeup",
        }
    }

    /// Section header prefixed to the element's text inside the assembled prefix.
    fn header(self) -> &'static str {
        match self {
            Self::Rules => "## lean-ctx rules",
            Self::ToolSchemas => "## available tools",
            Self::Wakeup => "## session memory",
        }
    }
}

/// The three injected texts, rendered once and reused across every arm. Serializable
/// so a released build's footprint can be exported and compared against a candidate.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Footprint {
    /// Rules block text (`rules_inject::canonical_rules_block`).
    pub rules: String,
    /// Serialized advertised tool descriptions + schemas.
    pub tool_schemas: String,
    /// Wakeup briefing text.
    pub wakeup: String,
}

impl Footprint {
    /// The live footprint this install actually injects (CLI path).
    ///
    /// Rules + tool schemas are deterministic for a given config; the wakeup
    /// briefing reflects the on-disk session/knowledge store, so a live footprint
    /// run pins its evidence through the recording (a drifted wakeup misses a
    /// replay key and hard-errors, exactly like the context A/B).
    #[must_use]
    pub fn live(project_root: &str) -> Self {
        let tools = crate::server::tool_visibility::advertised_tool_defs_default();
        Self {
            rules: crate::rules_inject::canonical_rules_block(),
            tool_schemas: serialize_tools(&tools),
            wakeup: crate::tools::ctx_overview::build_wakeup_briefing(project_root, None),
        }
    }

    /// The raw text of one element.
    fn element_text(&self, e: InjectedElement) -> &str {
        match e {
            InjectedElement::Rules => &self.rules,
            InjectedElement::ToolSchemas => &self.tool_schemas,
            InjectedElement::Wakeup => &self.wakeup,
        }
    }

    /// Tokens contributed by one element's text in isolation.
    #[must_use]
    pub fn element_tokens(&self, e: InjectedElement) -> usize {
        count_tokens(self.element_text(e))
    }
}

/// Serializes advertised tools to a stable string — exactly the two fields a client
/// re-sends every turn (description + input schema), matching `context_overhead`.
fn serialize_tools(tools: &[rmcp::model::Tool]) -> String {
    let mut out = String::new();
    for t in tools {
        let desc = t.description.as_deref().unwrap_or("");
        let schema = serde_json::to_string(&t.input_schema).unwrap_or_default();
        out.push_str(&format!("- {}: {desc}\n  {schema}\n", t.name));
    }
    out
}

/// The assembled injected prefix for one arm (full, or full minus one element).
#[derive(Debug, Clone)]
struct AssembledPrefix {
    text: String,
    tokens: usize,
    digest: String,
}

/// Assembles the injected prefix, optionally dropping one element.
fn assemble_prefix(fp: &Footprint, dropped: Option<InjectedElement>) -> AssembledPrefix {
    let mut sections = Vec::new();
    for e in InjectedElement::ALL {
        if Some(e) == dropped {
            continue;
        }
        let text = fp.element_text(e);
        if !text.trim().is_empty() {
            sections.push(format!("{}\n{text}", e.header()));
        }
    }
    let text = sections.join("\n\n");
    let tokens = count_tokens(&text);
    let digest = sha256_hex(text.as_bytes());
    AssembledPrefix {
        text,
        tokens,
        digest,
    }
}

/// Builds the chat request for one arm: the injected prefix rides the system turn.
fn build_footprint_request(prefix: &str, prompt: &str) -> ModelRequest {
    let system = if prefix.is_empty() {
        FOOTPRINT_SYSTEM.to_string()
    } else {
        format!("{prefix}\n\n{FOOTPRINT_SYSTEM}")
    };
    ModelRequest {
        system,
        user: prompt.to_string(),
    }
}

/// Configuration for a footprint run.
#[derive(Debug, Clone, Copy)]
pub struct FootprintConfig {
    /// Statistics + non-inferiority gate config (shared with the context A/B).
    pub report: ReportConfig,
    /// Minimum marginal tokens before an unhelpful element is flagged for pruning.
    pub token_floor: usize,
}

impl Default for FootprintConfig {
    fn default() -> Self {
        Self {
            report: ReportConfig::default(),
            token_floor: 50,
        }
    }
}

/// The per-element conclusion: cost, quality delta, verdict and prune recommendation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementVerdict {
    pub element: InjectedElement,
    /// Marginal tokens this element adds to the full prefix.
    pub token_cost: usize,
    /// Pass rate with the element present (the full arm).
    pub pass_rate_with: f64,
    /// Pass rate with the element removed.
    pub pass_rate_without: f64,
    /// `with − without` — the element's contribution to quality.
    pub pass_rate_delta: f64,
    /// Bootstrap-CI verdict of *adding* the element (`Improved` keeps it).
    pub verdict: Verdict,
    /// Element costs tokens (≥ floor) without earning a quality improvement.
    pub prune_recommended: bool,
    /// The full paired A/B report (full vs. minus-element) for auditing.
    pub report: AbReport,
}

/// The full footprint ablation report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FootprintReport {
    pub schema_version: u32,
    pub kind: String,
    pub suite: String,
    pub full_prefix_tokens: usize,
    pub full_prefix_digest: String,
    pub rules_tokens: usize,
    pub tool_schema_tokens: usize,
    pub wakeup_tokens: usize,
    pub model: ModelFingerprint,
    pub elements: Vec<ElementVerdict>,
    /// Machine-independent digest over every element's evidence.
    pub determinism_digest: String,
    /// Ed25519 public key (hex). `None` until signed.
    pub signer_public_key: Option<String>,
    /// Ed25519 signature over `determinism_digest` (hex). `None` until signed.
    pub signature: Option<String>,
}

impl FootprintReport {
    /// No injected element is actively *harmful* (removing it improves quality
    /// beyond the margin).
    #[must_use]
    pub fn no_harmful_element(&self) -> bool {
        self.elements
            .iter()
            .all(|e| e.verdict != Verdict::Regressed)
    }

    /// The gate: no harmful element, and — unless this is a mechanism check — every
    /// element's verdict is powered.
    #[must_use]
    pub fn gate_passes(&self, mechanism: bool) -> bool {
        self.elements
            .iter()
            .all(|e| e.verdict.passes_gate(mechanism))
    }

    /// Recomputes the evidence digest from the per-element reports.
    fn recompute_digest(&self) -> String {
        let parts: Vec<String> = self
            .elements
            .iter()
            .map(|e| artifact::determinism_digest(&e.report))
            .collect();
        sha256_hex(parts.join("|").as_bytes())
    }

    /// Signs with the persistent machine identity (`agent_identity` keystore).
    pub fn sign(&mut self, agent_id: &str) -> Result<(), String> {
        let key = crate::core::agent_identity::get_or_create_keypair(agent_id)?;
        self.sign_with_key(&key);
        Ok(())
    }

    /// Signs `determinism_digest` (which commits to all evidence) with an explicit key.
    pub fn sign_with_key(&mut self, key: &SigningKey) {
        let sig = key.sign(self.determinism_digest.as_bytes());
        self.signer_public_key = Some(hex_encode(&key.verifying_key().to_bytes()));
        self.signature = Some(hex_encode(&sig.to_bytes()));
    }

    /// Verifies the signature *and* that the embedded digest still matches the evidence.
    #[must_use]
    pub fn verify(&self) -> bool {
        if self.recompute_digest() != self.determinism_digest {
            return false;
        }
        let (Some(sig), Some(pk)) = (&self.signature, &self.signer_public_key) else {
            return false;
        };
        let (Ok(sig_bytes), Ok(pk_bytes)) = (hex_decode(sig), hex_decode(pk)) else {
            return false;
        };
        verify_signature(&pk_bytes, self.determinism_digest.as_bytes(), &sig_bytes)
    }

    /// Pretty JSON for machine consumption.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Compact, deterministic side-by-side summary for the terminal.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("Footprint ablation — suite {}\n", self.suite));
        out.push_str(&format!(
            "Full injected prefix: {} tok (rules {} + tools {} + wakeup {})\n",
            self.full_prefix_tokens, self.rules_tokens, self.tool_schema_tokens, self.wakeup_tokens
        ));
        out.push_str(&format!(
            "Model: {} ({})\n\n",
            self.model.params.model, self.model.provider
        ));
        out.push_str(&format!(
            "{:<13} {:>6} {:>8} {:>8} {:>7}  {:<14} {}\n",
            "Element", "Cost", "Pass+", "Pass-", "dPass", "Verdict", "Action"
        ));
        for e in &self.elements {
            let action = if e.prune_recommended {
                if e.verdict == Verdict::Regressed {
                    format!("PRUNE (harmful, -{} tok)", e.token_cost)
                } else {
                    format!("PRUNE (-{} tok)", e.token_cost)
                }
            } else {
                "keep".to_string()
            };
            out.push_str(&format!(
                "{:<13} {:>6} {:>7.0}% {:>7.0}% {:>+6.0}%  {:<14} {}\n",
                e.element.label(),
                e.token_cost,
                e.pass_rate_with * 100.0,
                e.pass_rate_without * 100.0,
                e.pass_rate_delta * 100.0,
                e.verdict.label(),
                action,
            ));
        }
        out.push_str(&format!(
            "\nVerdict: {}\n",
            if self.no_harmful_element() {
                "OK (no harmful element)"
            } else {
                "HARMFUL ELEMENT PRESENT"
            }
        ));
        out.push_str(&format!(
            "Determinism digest: {}\n",
            self.determinism_digest
        ));
        out
    }
}

/// Compares two complete footprints on the same tasks: `baseline` (for example the
/// footprint exported from the released build) against `candidate` (a trimmed tool
/// surface or shorter instructions). Same model, same tasks, same system framing —
/// the footprint text is the only variable. A reduction is only worth shipping when
/// this report is `NonInferior` or `Improved`; the token delta sits in every record.
pub fn run_footprint_compare(
    suite: &EvalSuite,
    suite_name: &str,
    baseline: &Footprint,
    candidate: &Footprint,
    runner: &dyn ModelRunner,
    cfg: ReportConfig,
) -> Result<AbReport> {
    let base = assemble_prefix(baseline, None);
    let cand = assemble_prefix(candidate, None);
    let mut records = Vec::with_capacity(suite.tasks.len());
    for task in &suite.tasks {
        let workspace = task.workspace_path(&suite.dir);
        let base_resp = runner.run(&build_footprint_request(&base.text, &task.prompt))?;
        let base_score = score_task(task, &base_resp.text, &workspace)?;
        // Identical prefixes would issue an identically keyed request: reuse it.
        let (cand_score, cand_digest) = if cand.digest == base.digest {
            (base_score.clone(), base_resp.digest())
        } else {
            let resp = runner.run(&build_footprint_request(&cand.text, &task.prompt))?;
            (score_task(task, &resp.text, &workspace)?, resp.digest())
        };
        records.push(PairRecord {
            task_id: task.id.clone(),
            domain: task.domain.label().to_string(),
            baseline_value: base_score.value,
            lean_ctx_value: cand_score.value,
            baseline_passed: base_score.passed,
            lean_ctx_passed: cand_score.passed,
            baseline_tokens: base.tokens,
            lean_ctx_tokens: cand.tokens,
            baseline_context_digest: base.digest.clone(),
            lean_ctx_context_digest: cand.digest.clone(),
            baseline_answer_digest: base_resp.digest(),
            lean_ctx_answer_digest: cand_digest,
        });
    }
    Ok(AbReport::build(
        format!("{suite_name}::footprint-compare"),
        cand.tokens,
        runner.fingerprint().clone(),
        records,
        cfg,
    ))
}

/// Runs the footprint ablation: the full arm once, then each element's minus arm,
/// pairing them into a per-element [`AbReport`] and collapsing to a prune verdict.
///
/// The model is the only non-deterministic input; with a [`super::model::RecordedRunner`]
/// the whole run is byte-identical everywhere.
pub fn run_footprint_ab(
    suite: &EvalSuite,
    suite_name: &str,
    footprint: &Footprint,
    runner: &dyn ModelRunner,
    cfg: &FootprintConfig,
) -> Result<FootprintReport> {
    let full = assemble_prefix(footprint, None);

    // The full arm is identical for every element, so run it exactly once.
    let mut full_runs: Vec<(f64, bool, String)> = Vec::with_capacity(suite.tasks.len());
    for task in &suite.tasks {
        let workspace = task.workspace_path(&suite.dir);
        let resp = runner.run(&build_footprint_request(&full.text, &task.prompt))?;
        let score = score_task(task, &resp.text, &workspace)?;
        full_runs.push((score.value, score.passed, resp.digest()));
    }

    let mut elements = Vec::with_capacity(InjectedElement::ALL.len());
    for element in InjectedElement::ALL {
        let minus = assemble_prefix(footprint, Some(element));
        let token_cost = full.tokens.saturating_sub(minus.tokens);
        // An absent element leaves the prefix byte-identical → reuse the full arm
        // instead of issuing a redundant (and identically-keyed) model call.
        let element_present = minus.digest != full.digest;

        let mut records = Vec::with_capacity(suite.tasks.len());
        for (i, task) in suite.tasks.iter().enumerate() {
            let (without_value, without_passed, without_digest) = if element_present {
                let workspace = task.workspace_path(&suite.dir);
                let resp = runner.run(&build_footprint_request(&minus.text, &task.prompt))?;
                let score = score_task(task, &resp.text, &workspace)?;
                (score.value, score.passed, resp.digest())
            } else {
                (full_runs[i].0, full_runs[i].1, full_runs[i].2.clone())
            };

            records.push(PairRecord {
                task_id: task.id.clone(),
                domain: task.domain.label().to_string(),
                baseline_value: without_value,
                lean_ctx_value: full_runs[i].0,
                baseline_passed: without_passed,
                lean_ctx_passed: full_runs[i].1,
                baseline_tokens: minus.tokens,
                lean_ctx_tokens: full.tokens,
                baseline_context_digest: minus.digest.clone(),
                lean_ctx_context_digest: full.digest.clone(),
                baseline_answer_digest: without_digest,
                lean_ctx_answer_digest: full_runs[i].2.clone(),
            });
        }

        let report = AbReport::build(
            format!("{suite_name}::{}", element.label()),
            full.tokens,
            runner.fingerprint().clone(),
            records,
            cfg.report,
        );
        let pass_rate_with = report.stats.lean_ctx_pass_rate;
        let pass_rate_without = report.stats.baseline_pass_rate;
        // Pruning removes context, so it needs powered evidence from a real model: an
        // underpowered or fixture-only run keeps the element even when it looked harmful
        // (uncertainty moves toward more context). The gate still fails on any observed
        // regression.
        let prune_recommended = report.power.is_some_and(|p| p.powered)
            && report
                .evidence_tier
                .is_some_and(EvidenceTier::supports_task_quality_claim)
            && matches!(report.verdict, Verdict::NonInferior | Verdict::Regressed)
            && token_cost >= cfg.token_floor;

        elements.push(ElementVerdict {
            element,
            token_cost,
            pass_rate_with,
            pass_rate_without,
            pass_rate_delta: pass_rate_with - pass_rate_without,
            verdict: report.verdict,
            prune_recommended,
            report,
        });
    }

    let determinism_digest = {
        let parts: Vec<String> = elements
            .iter()
            .map(|e| artifact::determinism_digest(&e.report))
            .collect();
        sha256_hex(parts.join("|").as_bytes())
    };

    Ok(FootprintReport {
        schema_version: SCHEMA_VERSION,
        kind: KIND.to_string(),
        suite: suite_name.to_string(),
        full_prefix_tokens: full.tokens,
        full_prefix_digest: full.digest,
        rules_tokens: footprint.element_tokens(InjectedElement::Rules),
        tool_schema_tokens: footprint.element_tokens(InjectedElement::ToolSchemas),
        wakeup_tokens: footprint.element_tokens(InjectedElement::Wakeup),
        model: runner.fingerprint().clone(),
        elements,
        determinism_digest,
        signer_public_key: None,
        signature: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::eval_ab::model::{
        ModelFingerprint, ModelParams, ModelResponse, PROVIDER_RECORDED, RecordedRunner, Recording,
    };
    use std::path::PathBuf;

    fn fixed_footprint() -> Footprint {
        Footprint {
            rules: "RULE: prefer ctx_search over grep for code search. RULE: read a file before \
                editing it. RULE: never fabricate values. RULE: keep outputs deterministic."
                .repeat(2),
            tool_schemas: "- ctx_search: semantic and lexical code search across the repository. \
                - ctx_read: read source files with compression. - ctx_shell: run shell commands \
                with compressed output. - ctx_symbol: find an exact symbol definition."
                .repeat(2),
            wakeup: "FACTS: the project indexes code with BM25 and a property graph. \
                LAST_TASK: wire the footprint ablation harness. DECISIONS: reuse eval_ab."
                .to_string(),
        }
    }

    fn fixture_fingerprint() -> ModelFingerprint {
        ModelFingerprint {
            provider: PROVIDER_RECORDED.into(),
            endpoint: "test".into(),
            params: ModelParams {
                model: "fixture".into(),
                ..ModelParams::default()
            },
        }
    }

    fn test_key() -> SigningKey {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        SigningKey::from_bytes(&seed)
    }

    #[test]
    fn dropping_an_element_reduces_tokens_and_is_deterministic() {
        let fp = fixed_footprint();
        for e in InjectedElement::ALL {
            assert!(fp.element_tokens(e) > 0, "{} must carry tokens", e.label());
        }
        let full = assemble_prefix(&fp, None);
        let again = assemble_prefix(&fp, None);
        assert_eq!(full.digest, again.digest, "assembly must be deterministic");
        for e in InjectedElement::ALL {
            let minus = assemble_prefix(&fp, Some(e));
            assert!(
                minus.tokens < full.tokens,
                "dropping {} must reduce prefix tokens",
                e.label()
            );
            assert_ne!(minus.digest, full.digest);
        }
    }

    /// Builds an `n`-task QA suite + a recording where the tool schemas are the only
    /// element that changes an answer, so (when powered) tool_schemas must be IMPROVED
    /// (kept) and rules/wakeup must be prune candidates (cost tokens, no quality gain).
    fn pipeline_setup(n: usize) -> (EvalSuite, Footprint, RecordedRunner) {
        pipeline_setup_with(n, fixture_fingerprint())
    }

    /// Same recording, labelled as a capture of a real model (replayed → tier C).
    fn real_model_fingerprint() -> ModelFingerprint {
        ModelFingerprint {
            provider: crate::core::eval_ab::model::PROVIDER_OPENAI.into(),
            endpoint: "http://localhost:11434/v1".into(),
            params: ModelParams {
                model: "local-model".into(),
                ..ModelParams::default()
            },
        }
    }

    fn pipeline_setup_with(
        n: usize,
        fingerprint: ModelFingerprint,
    ) -> (EvalSuite, Footprint, RecordedRunner) {
        let raw: Vec<String> = (0..n)
            .map(|i| {
                let (q, a) = if i % 2 == 0 {
                    ("finds a symbol", "ctx_symbol")
                } else {
                    ("searches code", "ctx_search")
                };
                format!(
                    "{{\"id\":\"t{i}\",\"domain\":\"qa\",\"prompt\":\"Which tool {q} (case {i})?\",\"workspace\":\"ws\",\"answers\":[\"{a}\"]}}"
                )
            })
            .collect();
        let suite = EvalSuite::parse(&raw.join("\n"), PathBuf::from(".")).unwrap();
        let fp = fixed_footprint();
        let full = assemble_prefix(&fp, None);

        let mut rec = Recording::new(fingerprint);
        for task in &suite.tasks {
            let gold = task.answers[0].clone();
            let full_req = build_footprint_request(&full.text, &task.prompt);
            rec.entries
                .insert(full_req.key(), ModelResponse::new(gold.clone()));
            for e in InjectedElement::ALL {
                let minus = assemble_prefix(&fp, Some(e));
                if minus.digest == full.digest {
                    continue;
                }
                let req = build_footprint_request(&minus.text, &task.prompt);
                let answer = if e == InjectedElement::ToolSchemas {
                    "a vague wrong guess".to_string()
                } else {
                    gold.clone()
                };
                rec.entries.insert(req.key(), ModelResponse::new(answer));
            }
        }
        (suite, fp, RecordedRunner::new(rec))
    }

    #[test]
    fn underpowered_footprint_run_never_recommends_pruning() {
        let (suite, fp, runner) = pipeline_setup(2);
        let report = run_footprint_ab(&suite, "fixture", &fp, &runner, &FootprintConfig::default())
            .expect("recording must cover every replay key");
        for e in &report.elements {
            assert_eq!(e.verdict, Verdict::Underpowered, "{}", e.element.label());
            assert!(
                !e.prune_recommended,
                "{} pruned on two tasks of evidence",
                e.element.label()
            );
        }
    }

    #[test]
    fn powered_fixture_answers_never_recommend_pruning() {
        let (suite, fp, runner) = pipeline_setup(super::super::report::MIN_POWERED_PAIRS);
        let report = run_footprint_ab(&suite, "fixture", &fp, &runner, &FootprintConfig::default())
            .expect("recording must cover every replay key");
        for e in &report.elements {
            assert!(
                !e.prune_recommended,
                "{} pruned on tier-A fixture evidence",
                e.element.label()
            );
        }
    }

    #[test]
    fn footprint_run_flags_unhelpful_elements_for_pruning() {
        let (suite, fp, runner) = pipeline_setup_with(
            super::super::report::MIN_POWERED_PAIRS,
            real_model_fingerprint(),
        );
        let report = run_footprint_ab(&suite, "fixture", &fp, &runner, &FootprintConfig::default())
            .expect("recording must cover every replay key");

        assert_eq!(report.elements.len(), 3);

        let tools = report
            .elements
            .iter()
            .find(|e| e.element == InjectedElement::ToolSchemas)
            .unwrap();
        assert_eq!(
            tools.verdict,
            Verdict::Improved,
            "tool schemas decide answers"
        );
        assert!(!tools.prune_recommended, "an improving element is kept");

        let rules = report
            .elements
            .iter()
            .find(|e| e.element == InjectedElement::Rules)
            .unwrap();
        assert!(
            rules.prune_recommended,
            "rules cost tokens but never changed an answer → prune"
        );
        assert!(
            report.gate_passes(false),
            "no element is actively harmful here"
        );
    }

    /// A candidate footprint that drops what the answers depend on must show up as a
    /// regression against the baseline, with its token saving in every record.
    #[test]
    fn footprint_compare_flags_a_smaller_candidate_that_loses_answers() {
        let (suite, fp, runner) = pipeline_setup(super::super::report::MIN_POWERED_PAIRS);
        let candidate = Footprint {
            tool_schemas: String::new(),
            ..fp.clone()
        };
        let report = run_footprint_compare(
            &suite,
            "fixture",
            &fp,
            &candidate,
            &runner,
            ReportConfig::default(),
        )
        .expect("recording covers both prefixes");
        assert_eq!(report.verdict, Verdict::Regressed, "{:?}", report.stats);
        assert!(report.records[0].lean_ctx_tokens < report.records[0].baseline_tokens);

        let same = run_footprint_compare(
            &suite,
            "fixture",
            &fp,
            &fp,
            &runner,
            ReportConfig::default(),
        )
        .expect("identical footprints reuse one request per task");
        assert_eq!(same.stats.losses, 0);
        assert_eq!(same.stats.wins, 0);
    }

    #[test]
    fn footprint_report_is_deterministic_and_signable() {
        let (suite, fp, runner) = pipeline_setup(2);
        let cfg = FootprintConfig::default();
        let report = run_footprint_ab(&suite, "fixture", &fp, &runner, &cfg).unwrap();
        let report2 = run_footprint_ab(&suite, "fixture", &fp, &runner, &cfg).unwrap();
        assert_eq!(report.determinism_digest, report2.determinism_digest);

        let mut signed = report;
        signed.sign_with_key(&test_key());
        assert!(signed.verify(), "fresh signature must verify");

        signed.elements[0].report.records[0].lean_ctx_value = 0.123;
        assert!(
            !signed.verify(),
            "tampered evidence must break verification"
        );
    }

    #[test]
    fn live_footprint_carries_rules_and_tool_schemas() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let fp = Footprint::live(".");
        assert!(!fp.rules.is_empty(), "default config injects a rules block");
        assert!(!fp.tool_schemas.is_empty(), "tools are always advertised");
    }

    /// Guards the committed footprint suite (`rust/eval/footprint-suite.ndjson`)
    /// in-process so suite drift fails in `cargo test` / `dev-install`, mirroring
    /// the accuracy-suite guard. Model-free: structure + footprint-sensitivity only.
    #[test]
    fn committed_footprint_suite_loads_and_is_sensitive() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/footprint-suite.ndjson");
        let suite = EvalSuite::load(&path).expect("committed footprint suite must load + validate");
        assert!(
            suite.tasks.len() >= 4,
            "need enough tasks for a meaningful ablation, got {}",
            suite.tasks.len()
        );
        assert!(
            suite.tasks.iter().any(|t| t.id.starts_with("route-")),
            "need a tool-routing task (sensitive to the tool-schema element)"
        );
        assert!(
            suite.tasks.iter().any(|t| t.id.starts_with("control-")),
            "need a footprint-insensitive control task"
        );
        for t in &suite.tasks {
            assert_eq!(t.domain, super::super::suite::Domain::Qa);
            assert!(!t.answers.is_empty(), "task {} needs a gold answer", t.id);
        }
    }
}
