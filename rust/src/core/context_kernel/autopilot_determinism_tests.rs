// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{BTreeSet, HashMap},
    env,
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::core::{
    context_field::{
        ContextField, ContextItemId, FieldWeights, Provenance, TokenBudget, ViewCosts, ViewKind,
    },
    context_kernel::{
        autopilot::{
            AdaptiveLearningState, AutopilotController, AutopilotDecision, AutopilotEconomics,
            AutopilotInput, PlannerTier, PreloadBudget, UserOverrides,
        },
        enforce::KernelMode,
        orchestrator::ContextKernel,
        policy::ContextPolicy,
        types::{
            CandidateProvider, ContextObjectKind, ContextObjectV1, Freshness, RetrievalContext,
            SensitivityLevel, SideEffectPolicy,
        },
    },
    outcome::contracts::TaskClass,
};

const CHILD_TEST_NAME: &str =
    "core::context_kernel::autopilot::determinism_tests::child_emits_canonical_payload";
const CHILD_TIMEOUT: Duration = Duration::from_secs(10);

const PROVIDERS: [&str; 3] = ["provider.alpha", "provider.beta", "provider.gamma"];
const ITEM_KEYS: [&str; 2] = ["item_a", "item_b"];

#[derive(Clone)]
struct FixtureProvider {
    id: String,
    items: Vec<ContextObjectV1>,
}

impl CandidateProvider for FixtureProvider {
    fn provider_id(&self) -> &str {
        &self.id
    }

    fn candidates(&self, _ctx: &RetrievalContext) -> Vec<ContextObjectV1> {
        self.items.clone()
    }

    fn side_effect_policy(&self) -> SideEffectPolicy {
        SideEffectPolicy::ReadOnly
    }
}

fn fixture_providers() -> Vec<FixtureProvider> {
    PROVIDERS
        .iter()
        .enumerate()
        .map(|(provider_index, provider)| FixtureProvider {
            id: (*provider).to_owned(),
            items: ITEM_KEYS
                .iter()
                .enumerate()
                .map(|(item_index, key)| candidate(provider, provider_index, item_index, key))
                .collect(),
        })
        .collect()
}

fn candidate(
    provider: &str,
    provider_index: usize,
    item_index: usize,
    key: &str,
) -> ContextObjectV1 {
    let mut view_costs = ViewCosts::new();
    view_costs.set(ViewKind::Full, 40);

    ContextObjectV1 {
        id: ContextItemId::from_provider(provider, key),
        kind: ContextObjectKind::Fact,
        source: provider.to_owned(),
        content_ref: format!("memory/{provider}/{key}"),
        title: "fix parser bug".to_owned(),
        content: Some("fix parser bug".to_owned()),
        freshness: Freshness {
            created_at: "stable-fixture".to_owned(),
            ..Freshness::default()
        },
        confidence: 1.0,
        sensitivity: SensitivityLevel::Internal,
        token_estimate: 40,
        view_costs,
        provenance: Provenance::default(),
        semantic_fingerprint: Some(format!("candidate{provider_index}{item_index}")),
        metadata: HashMap::new(),
    }
}

fn kernel(provider_order: &[usize], item_order: &[usize]) -> ContextKernel {
    let providers = fixture_providers();
    let mut kernel = ContextKernel::with_field(
        Vec::<Box<dyn CandidateProvider>>::new(),
        ContextField::with_weights(FieldWeights::default()),
    );

    for &provider_index in provider_order {
        let mut provider = providers[provider_index].clone();
        provider.items = item_order
            .iter()
            .map(|&item_index| provider.items[item_index].clone())
            .collect();
        kernel.register(Box::new(provider));
    }

    kernel
}

fn controller(provider_order: &[usize], item_order: &[usize]) -> AutopilotController {
    AutopilotController::with_kernels(
        kernel(provider_order, item_order),
        kernel(provider_order, item_order),
    )
}

fn fixture_input(entitled_to_adaptive: bool) -> AutopilotInput {
    AutopilotInput {
        retrieval: RetrievalContext {
            query: "fix parser bug".to_owned(),
            task: Some("determinism regression".to_owned()),
            project_root: "/deterministic/autopilot-fixture".to_owned(),
            budget: TokenBudget { total: 80, used: 0 },
            max_candidates: PROVIDERS.len() * ITEM_KEYS.len(),
        },
        evaluation_time: None,
        task_class: TaskClass::BugFix,
        entitled_to_adaptive,
        confidence_milli: 900,
        configured_mode: None,
        default_mode: "map".to_owned(),
        security_forced_mode: None,
        overrides: UserOverrides::default(),
        policy: ContextPolicy::default(),
        kernel_mode: KernelMode::Enforce,
        economics: AutopilotEconomics {
            baseline_cost_micros: 1_000,
            candidate_cost_micros: 100,
            expected_quality_value_micros: 100,
            ..AutopilotEconomics::default()
        },
        learning: AdaptiveLearningState::default(),
        available_providers: PROVIDERS
            .iter()
            .map(|provider| (*provider).to_owned())
            .collect(),
        local_providers: BTreeSet::new(),
        cached_preloads: BTreeSet::new(),
        preload_budget: PreloadBudget::default(),
        context_policy: None,
    }
}

fn decision(
    provider_order: &[usize],
    item_order: &[usize],
    entitled_to_adaptive: bool,
) -> AutopilotDecision {
    controller(provider_order, item_order)
        .plan(&fixture_input(entitled_to_adaptive), None)
        .expect("valid deterministic decision")
}

fn expected_item_ids() -> BTreeSet<String> {
    PROVIDERS
        .iter()
        .flat_map(|provider| {
            ITEM_KEYS
                .iter()
                .map(move |key| ContextItemId::from_provider(provider, key).to_string())
        })
        .collect()
}

fn assert_plan_shape(plan: &AutopilotDecision) {
    assert_eq!(plan.context_plan.provider_stats.len(), PROVIDERS.len());
    assert_eq!(plan.context_plan.selected.len(), 2);
    assert_eq!(plan.context_plan.excluded.len(), 4);
    assert_eq!(plan.context_plan.budget.used_tokens, 80);

    let selected_ids: BTreeSet<_> = plan
        .context_plan
        .selected
        .iter()
        .map(|entry| entry.object_id.clone())
        .collect();
    let excluded_ids: BTreeSet<_> = plan
        .context_plan
        .excluded
        .iter()
        .map(|entry| entry.object_id.clone())
        .collect();

    assert_eq!(selected_ids.len(), plan.context_plan.selected.len());
    assert_eq!(excluded_ids.len(), plan.context_plan.excluded.len());
    assert!(selected_ids.is_disjoint(&excluded_ids));

    let mut all_ids = selected_ids;
    all_ids.extend(excluded_ids);
    assert_eq!(all_ids, expected_item_ids());
}

#[test]
fn provider_registration_and_candidate_enumeration_permutations_are_canonical() {
    let variants: &[(&[usize], &[usize])] = &[
        (&[0, 1, 2], &[0, 1]),
        (&[2, 1, 0], &[1, 0]),
        (&[1, 0, 2], &[1, 0]),
        (&[2, 0, 1], &[0, 1]),
    ];
    let mut community_reference = None;
    let mut adaptive_reference = None;

    for &(provider_order, item_order) in variants {
        let community = decision(provider_order, item_order, false);
        let adaptive = decision(provider_order, item_order, true);
        assert_eq!(community.tier, PlannerTier::Community);
        assert_eq!(adaptive.tier, PlannerTier::AdaptivePro);
        assert_plan_shape(&community);
        assert_plan_shape(&adaptive);

        let community_bytes = community
            .canonical_bytes()
            .expect("Community canonical decision serializes");
        let adaptive_bytes = adaptive
            .canonical_bytes()
            .expect("Adaptive canonical decision serializes");

        if let Some(reference) = &community_reference {
            assert_eq!(
                community_bytes, *reference,
                "Community permutation changed bytes"
            );
        } else {
            community_reference = Some(community_bytes);
        }
        if let Some(reference) = &adaptive_reference {
            assert_eq!(
                adaptive_bytes, *reference,
                "Adaptive permutation changed bytes"
            );
        } else {
            adaptive_reference = Some(adaptive_bytes);
        }
    }
}

#[test]
fn provider_item_ids_are_stable_and_budget_cutoff_is_real() {
    let first = decision(&[0, 1, 2], &[0, 1], false);
    let second = decision(&[2, 0, 1], &[1, 0], false);
    assert_plan_shape(&first);
    assert_plan_shape(&second);

    assert_eq!(
        ContextItemId::from_provider(PROVIDERS[0], ITEM_KEYS[0]),
        ContextItemId::from_provider(PROVIDERS[0], ITEM_KEYS[0])
    );
    assert_ne!(
        ContextItemId::from_provider(PROVIDERS[0], ITEM_KEYS[0]),
        ContextItemId::from_provider(PROVIDERS[0], ITEM_KEYS[1])
    );

    let first_ids: BTreeSet<_> = first
        .context_plan
        .selected
        .iter()
        .map(|entry| entry.object_id.clone())
        .chain(
            first
                .context_plan
                .excluded
                .iter()
                .map(|entry| entry.object_id.clone()),
        )
        .collect();
    let second_ids: BTreeSet<_> = second
        .context_plan
        .selected
        .iter()
        .map(|entry| entry.object_id.clone())
        .chain(
            second
                .context_plan
                .excluded
                .iter()
                .map(|entry| entry.object_id.clone()),
        )
        .collect();
    assert_eq!(first_ids, expected_item_ids());
    assert_eq!(second_ids, expected_item_ids());
    assert_eq!(first.context_plan.budget.remaining_tokens, 0);
    assert_eq!(second.context_plan.budget.remaining_tokens, 0);
}

#[test]
fn semantic_control_changes_change_public_decision_bytes() {
    let controller = controller(&[0, 1, 2], &[0, 1]);
    let baseline_input = fixture_input(false);
    let baseline = controller
        .plan(&baseline_input, None)
        .expect("valid baseline decision");
    assert_plan_shape(&baseline);
    let baseline_bytes = baseline
        .canonical_bytes()
        .expect("baseline canonical decision serializes");

    let mut mode_changed = fixture_input(false);
    mode_changed.overrides.read_mode = Some("full".to_owned());
    let mode_bytes = controller
        .plan(&mode_changed, None)
        .expect("valid read-mode decision")
        .canonical_bytes()
        .expect("read-mode canonical decision serializes");
    assert_ne!(baseline_bytes, mode_bytes, "read mode must be canonical");

    let mut retrieval_changed = fixture_input(false);
    retrieval_changed.retrieval.max_candidates = 1;
    let retrieval_bytes = controller
        .plan(&retrieval_changed, None)
        .expect("valid retrieval-control decision")
        .canonical_bytes()
        .expect("retrieval-control canonical decision serializes");
    assert_ne!(
        baseline_bytes, retrieval_bytes,
        "candidate limit must be canonical"
    );

    let mut budget_changed = fixture_input(false);
    budget_changed.retrieval.budget.total = 40;
    let budget_bytes = controller
        .plan(&budget_changed, None)
        .expect("valid budget-control decision")
        .canonical_bytes()
        .expect("budget-control canonical decision serializes");
    assert_ne!(
        baseline_bytes, budget_bytes,
        "budget must affect decision bytes"
    );
}

struct ChildPayload {
    community: Vec<u8>,
    adaptive: Vec<u8>,
}

fn run_child() -> ChildPayload {
    let executable = env::current_exe().expect("current integration-test executable");
    let mut child = Command::new(executable)
        .arg("--exact")
        .arg(CHILD_TEST_NAME)
        .arg("--nocapture")
        .arg("--test-threads=1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn exact canonical-payload child test");

    // Drain both pipes while the child runs, including on small pipe buffers.
    let mut stdout = child.stdout.take().expect("piped child stdout");
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).expect("read child stdout");
        bytes
    });
    let mut stderr = child.stderr.take().expect("piped child stderr");
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).expect("read child stderr");
        bytes
    });

    let started = Instant::now();
    let timed_out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) if started.elapsed() >= CHILD_TIMEOUT => {
                let _ = child.kill();
                break true;
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("poll canonical-payload child: {error}");
            }
        }
    };
    let status = child.wait().expect("reap canonical-payload child");
    let stdout_bytes = stdout_reader.join().expect("stdout reader");
    let stderr_bytes = stderr_reader.join().expect("stderr reader");
    let stdout = String::from_utf8_lossy(&stdout_bytes);
    let stderr = String::from_utf8_lossy(&stderr_bytes);
    assert!(
        !timed_out,
        "canonical-payload child exceeded timeout: {stderr}"
    );
    assert!(
        status.success(),
        "exact canonical-payload child failed: {stderr}\n{stdout}"
    );

    ChildPayload {
        community: decode_hex(&extract_payload(
            &stdout,
            "AUTOPILOT_DETERMINISM_COMMUNITY=",
        )),
        adaptive: decode_hex(&extract_payload(&stdout, "AUTOPILOT_DETERMINISM_ADAPTIVE=")),
    }
}

fn extract_payload(output: &str, marker: &str) -> String {
    let mut matches = output
        .lines()
        .filter_map(|line| line.split_once(marker).map(|(_, payload)| payload.trim()));
    let payload = matches
        .next()
        .unwrap_or_else(|| panic!("child output missing marker {marker:?}: {output}"));
    assert!(
        matches.next().is_none(),
        "child output repeated marker {marker:?}: {output}"
    );
    assert!(!payload.is_empty(), "child output emitted empty {marker:?}");
    payload.to_owned()
}

fn decode_hex(value: &str) -> Vec<u8> {
    assert!(value.len().is_multiple_of(2), "odd-length child payload");
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| (hex_nibble(pair[0]) << 4) | hex_nibble(pair[1]))
        .collect()
}

fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => panic!("invalid child payload hex byte: {byte}"),
    }
}

#[test]
fn child_emits_canonical_payload() {
    let community = decision(&[0, 1, 2], &[0, 1], false);
    let adaptive = decision(&[0, 1, 2], &[0, 1], true);
    assert_eq!(community.tier, PlannerTier::Community);
    assert_eq!(adaptive.tier, PlannerTier::AdaptivePro);
    assert_plan_shape(&community);
    assert_plan_shape(&adaptive);

    println!(
        "AUTOPILOT_DETERMINISM_COMMUNITY={}",
        encode_hex(
            &community
                .canonical_bytes()
                .expect("Community child canonical decision serializes")
        )
    );
    println!(
        "AUTOPILOT_DETERMINISM_ADAPTIVE={}",
        encode_hex(
            &adaptive
                .canonical_bytes()
                .expect("Adaptive child canonical decision serializes")
        )
    );
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[test]
fn fresh_subprocesses_emit_same_community_and_adaptive_canonical_bytes() {
    let first = run_child();
    let second = run_child();

    assert!(!first.community.is_empty());
    assert!(!first.adaptive.is_empty());
    assert_ne!(first.community, first.adaptive);
    assert_eq!(first.community, second.community);
    assert_eq!(first.adaptive, second.adaptive);
}
