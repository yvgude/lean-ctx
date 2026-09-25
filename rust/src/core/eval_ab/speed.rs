//! Speed proof (`lean-ctx prove speed`).
//!
//! Answers one question with a measurement instead of an extrapolation: does the same model
//! answer the same tasks faster when it gets lean-ctx's context instead of a raw dump?
//!
//! * **Live only.** Latency of a replayed recording means nothing, so a recorded runner is
//!   refused.
//! * **Fair order.** One warm-up request (not counted) absorbs cold starts such as a local
//!   model loading. After that the arm that goes first alternates per task and per round, so
//!   neither arm profits from a warm connection or cache.
//! * **Quality next to speed.** Every answer is scored. A faster arm that answers worse is
//!   reported as such, and the headline surfaces (`gain --wrapped`) only use a proof in which
//!   lean-ctx answered at least as many tasks correctly.
//! * **Signed.** The artifact is signed with the machine identity like the A/B quality
//!   report, and `verify` recomputes the summary from the samples.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Result, bail};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

use super::build_request;
use super::conditions::{Condition, DEFAULT_BUDGET_TOKENS, assemble};
use super::model::{ModelFingerprint, ModelRequest, ModelRunner, PROVIDER_RECORDED};
use super::scorers::score_task;
use super::suite::EvalSuite;

const SCHEMA_VERSION: u32 = 1;
pub const KIND: &str = "lean-ctx.speed-proof";
pub const DEFAULT_RUNS: usize = 3;

#[derive(Debug, Clone, Copy)]
pub struct SpeedConfig {
    /// Token budget enforced identically on both arms.
    pub budget_tokens: usize,
    /// Measured rounds per task and arm.
    pub runs: usize,
}

impl Default for SpeedConfig {
    fn default() -> Self {
        Self {
            budget_tokens: DEFAULT_BUDGET_TOKENS,
            runs: DEFAULT_RUNS,
        }
    }
}

/// Measurements for one task: every sample, both context sizes, both pass counts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeedTask {
    pub task_id: String,
    pub baseline_tokens: usize,
    pub lean_ctx_tokens: usize,
    pub baseline_context_digest: String,
    pub lean_ctx_context_digest: String,
    /// Wall-clock microseconds per request, in round order.
    pub baseline_us: Vec<u64>,
    pub lean_ctx_us: Vec<u64>,
    /// Rounds whose answer passed the task's scorer.
    pub baseline_passes: usize,
    pub lean_ctx_passes: usize,
}

/// Derived from [`SpeedTask`]s only; `verify` recomputes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeedSummary {
    pub tasks: usize,
    pub runs: usize,
    /// Median over every measured request of the arm.
    pub baseline_median_us: u64,
    pub lean_ctx_median_us: u64,
    /// Tasks whose lean-ctx median is below the baseline median.
    pub tasks_faster: usize,
    pub baseline_context_tokens: usize,
    pub lean_ctx_context_tokens: usize,
    pub baseline_passes: usize,
    pub lean_ctx_passes: usize,
}

impl SpeedSummary {
    pub fn from_tasks(tasks: &[SpeedTask], runs: usize) -> Self {
        let pooled = |pick: fn(&SpeedTask) -> &Vec<u64>| {
            median(tasks.iter().flat_map(|t| pick(t).iter().copied()).collect())
        };
        Self {
            tasks: tasks.len(),
            runs,
            baseline_median_us: pooled(|t| &t.baseline_us),
            lean_ctx_median_us: pooled(|t| &t.lean_ctx_us),
            tasks_faster: tasks
                .iter()
                .filter(|t| median(t.lean_ctx_us.clone()) < median(t.baseline_us.clone()))
                .count(),
            baseline_context_tokens: tasks.iter().map(|t| t.baseline_tokens).sum(),
            lean_ctx_context_tokens: tasks.iter().map(|t| t.lean_ctx_tokens).sum(),
            baseline_passes: tasks.iter().map(|t| t.baseline_passes).sum(),
            lean_ctx_passes: tasks.iter().map(|t| t.lean_ctx_passes).sum(),
        }
    }

    /// Share of median latency lean-ctx removed; negative when it was slower.
    pub fn latency_reduction(&self) -> f64 {
        if self.baseline_median_us == 0 {
            return 0.0;
        }
        1.0 - self.lean_ctx_median_us as f64 / self.baseline_median_us as f64
    }

    /// lean-ctx answered at least as many rounds correctly as the baseline.
    pub fn quality_held(&self) -> bool {
        self.lean_ctx_passes >= self.baseline_passes
    }

    /// Faster and not worse: the only result a headline surface may quote.
    pub fn is_headline(&self) -> bool {
        self.latency_reduction() > 0.0 && self.quality_held()
    }
}

/// A signed speed measurement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeedProofV1 {
    pub schema_version: u32,
    pub kind: String,
    pub created_at: String,
    pub lean_ctx_version: String,
    pub agent_id: String,
    pub suite: String,
    pub budget_tokens: usize,
    /// Model, parameters and endpoint (never credentials).
    pub model: ModelFingerprint,
    pub tasks: Vec<SpeedTask>,
    pub summary: SpeedSummary,
    pub signer_public_key: Option<String>,
    pub signature: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpeedVerifyResult {
    pub signature_valid: bool,
    /// The embedded summary equals the one recomputed from the samples.
    pub summary_matches: bool,
    pub error: Option<String>,
}

impl SpeedVerifyResult {
    pub fn ok(&self) -> bool {
        self.signature_valid && self.summary_matches
    }
}

impl SpeedProofV1 {
    fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        let mut clone = self.clone();
        clone.signature = None;
        clone.signer_public_key = None;
        serde_json::to_vec(&clone).map_err(|e| format!("serialize for signing: {e}"))
    }

    /// Signs with the persistent machine identity (`agent_identity` keystore).
    pub fn sign(&mut self) -> Result<(), String> {
        let key = crate::core::agent_identity::get_or_create_keypair(&self.agent_id)?;
        self.sign_with_key(&key)
    }

    pub fn sign_with_key(&mut self, key: &SigningKey) -> Result<(), String> {
        self.signature = None;
        self.signer_public_key = None;
        let sig = crate::core::agent_identity::sign_bytes_with(key, &self.canonical_bytes()?);
        self.signer_public_key = Some(crate::core::agent_identity::hex_encode(
            &key.verifying_key().to_bytes(),
        ));
        self.signature = Some(crate::core::agent_identity::hex_encode(&sig));
        Ok(())
    }

    /// Checks the signature and recomputes the summary, offline.
    pub fn verify(&self) -> SpeedVerifyResult {
        let summary_matches =
            SpeedSummary::from_tasks(&self.tasks, self.summary.runs) == self.summary;
        let fail = |msg: &str| SpeedVerifyResult {
            signature_valid: false,
            summary_matches,
            error: Some(msg.to_string()),
        };
        let (Some(sig_hex), Some(pk_hex)) = (&self.signature, &self.signer_public_key) else {
            return fail("proof is not signed");
        };
        let (Ok(sig), Ok(pk)) = (
            crate::core::agent_identity::hex_decode(sig_hex),
            crate::core::agent_identity::hex_decode(pk_hex),
        ) else {
            return fail("malformed signature or public key hex");
        };
        let canonical = match self.canonical_bytes() {
            Ok(c) => c,
            Err(e) => return fail(&e),
        };
        if crate::core::agent_identity::verify_signature(&pk, &canonical, &sig) {
            SpeedVerifyResult {
                signature_valid: true,
                summary_matches,
                error: (!summary_matches).then(|| "summary does not match the samples".to_string()),
            }
        } else {
            fail("signature does not match payload (tampered or wrong key)")
        }
    }
}

/// Runs every task `cfg.runs` times per arm against a live model and returns the unsigned
/// proof. `agent_id` names the signer the caller will sign with.
pub fn run_speed(
    suite: &EvalSuite,
    suite_name: &str,
    runner: &dyn ModelRunner,
    cfg: SpeedConfig,
    agent_id: &str,
) -> Result<SpeedProofV1> {
    if runner.fingerprint().provider == PROVIDER_RECORDED {
        bail!("speed must be measured against a live model, not a recording");
    }
    if cfg.runs == 0 {
        bail!("--runs must be at least 1");
    }
    suite.validate()?;

    let mut prepared = Vec::with_capacity(suite.tasks.len());
    for task in &suite.tasks {
        let workspace = task.resolve_workspace_path(&suite.dir)?;
        let base = assemble(
            Condition::Baseline,
            &workspace,
            task.query(),
            cfg.budget_tokens,
        )?;
        let lean = assemble(
            Condition::LeanCtx,
            &workspace,
            task.query(),
            cfg.budget_tokens,
        )?;
        prepared.push((task, workspace, base, lean));
    }

    // Warm-up: absorbs a cold start (model load, first connection); never counted.
    if let Some((task, _, base, _)) = prepared.first() {
        runner.run(&build_request(&base.text, &task.prompt))?;
    }

    let mut tasks = Vec::with_capacity(prepared.len());
    for (index, (task, workspace, base, lean)) in prepared.iter().enumerate() {
        let requests = [
            build_request(&base.text, &task.prompt),
            build_request(&lean.text, &task.prompt),
        ];
        let mut row = SpeedTask {
            task_id: task.id.clone(),
            baseline_tokens: base.tokens,
            lean_ctx_tokens: lean.tokens,
            baseline_context_digest: base.digest.clone(),
            lean_ctx_context_digest: lean.digest.clone(),
            baseline_us: Vec::with_capacity(cfg.runs),
            lean_ctx_us: Vec::with_capacity(cfg.runs),
            baseline_passes: 0,
            lean_ctx_passes: 0,
        };
        for round in 0..cfg.runs {
            for arm in arm_order(index, round) {
                let (micros, text) = timed(runner, &requests[arm])?;
                let passed = score_task(task, &text, workspace)?.passed;
                if arm == 0 {
                    row.baseline_us.push(micros);
                    row.baseline_passes += usize::from(passed);
                } else {
                    row.lean_ctx_us.push(micros);
                    row.lean_ctx_passes += usize::from(passed);
                }
            }
        }
        tasks.push(row);
    }

    let summary = SpeedSummary::from_tasks(&tasks, cfg.runs);
    Ok(SpeedProofV1 {
        schema_version: SCHEMA_VERSION,
        kind: KIND.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        lean_ctx_version: env!("CARGO_PKG_VERSION").to_string(),
        agent_id: agent_id.to_string(),
        suite: suite_name.to_string(),
        budget_tokens: cfg.budget_tokens,
        model: runner.fingerprint().clone(),
        tasks,
        summary,
        signer_public_key: None,
        signature: None,
    })
}

/// Arm 0 is the baseline, arm 1 lean-ctx; who goes first alternates by task and round.
fn arm_order(task_index: usize, round: usize) -> [usize; 2] {
    if (task_index + round).is_multiple_of(2) {
        [0, 1]
    } else {
        [1, 0]
    }
}

fn timed(runner: &dyn ModelRunner, request: &ModelRequest) -> Result<(u64, String)> {
    let started = Instant::now();
    let response = runner.run(request)?;
    let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    Ok((micros, response.text))
}

fn median(mut values: Vec<u64>) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        values[mid - 1].midpoint(values[mid])
    } else {
        values[mid]
    }
}

fn speed_dir() -> Result<PathBuf, String> {
    Ok(crate::core::data_dir::lean_ctx_data_dir()?
        .join("value")
        .join("speed"))
}

/// Writes the proof to `out` (or a timestamped file in the data dir) and makes it the latest.
pub fn write_proof(proof: &SpeedProofV1, out: Option<&Path>) -> Result<PathBuf, String> {
    let dir = speed_dir()?;
    let path = out.map_or_else(
        || {
            let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
            dir.join(format!("speed-proof-v1_{stamp}.json"))
        },
        Path::to_path_buf,
    );
    let json = serde_json::to_string_pretty(proof).map_err(|e| format!("serialize: {e}"))?;
    for target in [path.as_path(), &dir.join("latest.json")] {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
        std::fs::write(target, &json).map_err(|e| format!("write {}: {e}", target.display()))?;
    }
    Ok(path)
}

/// Loads a proof, rejecting unrelated JSON by `kind`.
pub fn load_proof(path: &Path) -> Result<SpeedProofV1, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let proof: SpeedProofV1 =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    if proof.kind != KIND {
        return Err(format!("not a {KIND} artifact (kind = {:?})", proof.kind));
    }
    Ok(proof)
}

pub fn latest_path() -> Option<PathBuf> {
    speed_dir().ok().map(|d| d.join("latest.json"))
}

/// The latest proof, only if its signature and summary verify.
pub fn latest_verified() -> Option<SpeedProofV1> {
    let proof = load_proof(&latest_path()?).ok()?;
    proof.verify().ok().then_some(proof)
}

/// What a headline surface (`gain --wrapped`) may quote: a verified proof in which lean-ctx
/// was faster and answered at least as many tasks correctly. Anything else quotes nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeedHeadline {
    pub faster_pct: f64,
    pub measured_on: String,
    pub tasks: usize,
    pub runs: usize,
    pub model: String,
}

impl SpeedHeadline {
    pub fn latest() -> Option<Self> {
        Self::from_proof(&latest_verified()?)
    }

    pub fn from_proof(proof: &SpeedProofV1) -> Option<Self> {
        let s = &proof.summary;
        s.is_headline().then(|| Self {
            faster_pct: s.latency_reduction() * 100.0,
            measured_on: proof
                .created_at
                .get(..10)
                .unwrap_or(&proof.created_at)
                .to_string(),
            tasks: s.tasks,
            runs: s.runs,
            model: proof.model.params.model.clone(),
        })
    }

    /// `model answered 24% faster with lean-ctx context`
    pub fn phrase(&self) -> String {
        format!(
            "model answered {:.0}% faster with lean-ctx context",
            self.faster_pct
        )
    }

    /// `measured 2026-09-25 · 12 tasks × 3 runs · gpt-4o-mini`
    pub fn detail(&self) -> String {
        format!(
            "measured {} · {} tasks × {} runs · {}",
            self.measured_on, self.tasks, self.runs, self.model
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::eval_ab::model::{ModelParams, ModelResponse, PROVIDER_OPENAI};
    use std::cell::RefCell;

    /// Answers correctly and logs which arm was asked, identified by context size.
    struct Fake {
        fp: ModelFingerprint,
        calls: RefCell<Vec<usize>>,
    }

    impl ModelRunner for Fake {
        fn fingerprint(&self) -> &ModelFingerprint {
            &self.fp
        }
        fn run(&self, req: &ModelRequest) -> Result<ModelResponse> {
            self.calls.borrow_mut().push(req.user.len());
            Ok(ModelResponse::new("bm25 graph knowledge session"))
        }
    }

    fn fake(provider: &str) -> Fake {
        Fake {
            fp: ModelFingerprint {
                provider: provider.into(),
                endpoint: "test".into(),
                params: ModelParams {
                    model: "fixture".into(),
                    ..ModelParams::default()
                },
            },
            calls: RefCell::new(Vec::new()),
        }
    }

    fn suite(root: &Path) -> EvalSuite {
        let ws = root.join("corpus");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(
            ws.join("a-noise.md"),
            "Unrelated notes about weather, cats, and lunch plans.\n".repeat(1_000),
        )
        .unwrap();
        std::fs::write(
            ws.join("z-answer.md"),
            "Consolidation persists artifacts to bm25, graph, knowledge and session stores.",
        )
        .unwrap();
        let raw = [
            r#"{"id":"t1","domain":"qa","prompt":"Which stores does consolidation persist to?","workspace":"corpus","answers":["bm25 graph knowledge session"]}"#,
            r#"{"id":"t2","domain":"qa","prompt":"Where does consolidation persist artifacts?","workspace":"corpus","answers":["bm25 graph knowledge session"]}"#,
        ]
        .join("\n");
        EvalSuite::parse(&raw, root.to_path_buf()).unwrap()
    }

    fn task(id: &str, base: &[u64], lean: &[u64], passes: (usize, usize)) -> SpeedTask {
        SpeedTask {
            task_id: id.into(),
            baseline_tokens: 4000,
            lean_ctx_tokens: 900,
            baseline_context_digest: "b".into(),
            lean_ctx_context_digest: "l".into(),
            baseline_us: base.to_vec(),
            lean_ctx_us: lean.to_vec(),
            baseline_passes: passes.0,
            lean_ctx_passes: passes.1,
        }
    }

    fn signed(tasks: Vec<SpeedTask>) -> (SpeedProofV1, SigningKey) {
        let summary = SpeedSummary::from_tasks(&tasks, 3);
        let mut proof = SpeedProofV1 {
            schema_version: SCHEMA_VERSION,
            kind: KIND.into(),
            created_at: "2026-09-25T00:00:00Z".into(),
            lean_ctx_version: "test".into(),
            agent_id: "test".into(),
            suite: "s".into(),
            budget_tokens: 4000,
            model: fake(PROVIDER_OPENAI).fp,
            tasks,
            summary,
            signer_public_key: None,
            signature: None,
        };
        let key = SigningKey::from_bytes(&[7u8; 32]);
        proof.sign_with_key(&key).unwrap();
        (proof, key)
    }

    #[test]
    fn a_recording_cannot_prove_speed() {
        let root = tempfile::tempdir().unwrap();
        let err = run_speed(
            &suite(root.path()),
            "s",
            &fake(PROVIDER_RECORDED),
            SpeedConfig::default(),
            "test",
        )
        .unwrap_err();
        assert!(err.to_string().contains("live model"));
    }

    #[test]
    fn warm_up_is_not_counted_and_the_first_arm_alternates() {
        let root = tempfile::tempdir().unwrap();
        let runner = fake(PROVIDER_OPENAI);
        let cfg = SpeedConfig {
            runs: 2,
            ..SpeedConfig::default()
        };
        let proof = run_speed(&suite(root.path()), "s", &runner, cfg, "test").unwrap();

        let calls = runner.calls.borrow();
        // 1 warm-up + 2 tasks × 2 rounds × 2 arms.
        assert_eq!(calls.len(), 1 + 2 * 2 * 2);
        // The baseline dump is the larger request; `true` = baseline asked first.
        let first_arms: Vec<bool> = calls[1..].chunks(2).map(|pair| pair[0] > pair[1]).collect();
        assert_eq!(first_arms, vec![true, false, false, true]);

        for row in &proof.tasks {
            assert_eq!(row.baseline_us.len(), 2);
            assert_eq!(row.lean_ctx_us.len(), 2);
            assert!(row.lean_ctx_tokens < row.baseline_tokens);
            assert_eq!(row.lean_ctx_passes, 2);
        }
        assert_eq!(proof.summary.lean_ctx_passes, 4);
    }

    #[test]
    fn summary_uses_pooled_medians_and_scores_quality() {
        let tasks = vec![
            task("a", &[100, 120, 110], &[60, 70, 65], (3, 3)),
            task("b", &[200, 210, 190], &[220, 230, 210], (3, 3)),
        ];
        let s = SpeedSummary::from_tasks(&tasks, 3);
        assert_eq!(s.baseline_median_us, 155);
        assert_eq!(s.lean_ctx_median_us, 140);
        assert_eq!(s.tasks_faster, 1);
        assert!(s.is_headline());

        let worse = SpeedSummary::from_tasks(&[task("a", &[100], &[50], (1, 0))], 1);
        assert!(worse.latency_reduction() > 0.0);
        assert!(!worse.is_headline(), "faster but worse is not a headline");
    }

    #[test]
    fn verify_catches_an_edited_summary_and_a_forged_sample() {
        let (proof, key) = signed(vec![task("a", &[100, 120, 110], &[60, 70, 65], (3, 3))]);
        assert!(proof.verify().ok());

        let mut edited = proof.clone();
        edited.summary.lean_ctx_median_us = 1;
        assert!(!edited.verify().ok());

        // Forging a sample and re-signing still fails: the summary no longer matches.
        let mut forged = proof.clone();
        forged.tasks[0].lean_ctx_us = vec![1, 1, 1];
        forged.sign_with_key(&key).unwrap();
        let result = forged.verify();
        assert!(result.signature_valid && !result.summary_matches);
    }

    #[test]
    fn write_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let (proof, _) = signed(vec![task("a", &[100], &[60], (1, 1))]);
        let out = dir.path().join("p.json");
        let json = serde_json::to_string_pretty(&proof).unwrap();
        std::fs::write(&out, json).unwrap();
        let loaded = load_proof(&out).unwrap();
        assert_eq!(loaded, proof);
        assert!(loaded.verify().ok());

        std::fs::write(&out, r#"{"kind":"other"}"#).unwrap();
        assert!(load_proof(&out).is_err());
    }

    #[test]
    fn only_a_faster_and_not_worse_proof_becomes_a_headline() {
        let (good, _) = signed(vec![task("a", &[100, 120, 110], &[60, 70, 65], (3, 3))]);
        let h = SpeedHeadline::from_proof(&good).unwrap();
        assert_eq!(
            h.phrase(),
            "model answered 41% faster with lean-ctx context"
        );
        assert_eq!(
            h.detail(),
            "measured 2026-09-25 · 1 tasks × 3 runs · fixture"
        );

        let (worse, _) = signed(vec![task("a", &[100, 120, 110], &[60, 70, 65], (3, 2))]);
        assert!(SpeedHeadline::from_proof(&worse).is_none());
        let (slower, _) = signed(vec![task("a", &[60, 70, 65], &[100, 120, 110], (3, 3))]);
        assert!(SpeedHeadline::from_proof(&slower).is_none());
    }

    #[test]
    fn median_handles_even_odd_and_empty() {
        assert_eq!(median(vec![]), 0);
        assert_eq!(median(vec![3, 1, 2]), 2);
        assert_eq!(median(vec![1, 2, 3, 4]), 2);
        assert_eq!(median(vec![u64::MAX, u64::MAX]), u64::MAX);
    }
}
