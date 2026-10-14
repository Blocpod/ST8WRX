use super::super::*;
use buzz_core::kind::{KIND_JOB_REQUEST, KIND_JOB_RESULT};
use buzz_sdk::build_project;
use ed25519_dalek::{Signer, SigningKey};
use nostr::Keys;
use sha2::{Digest, Sha256};
use st8_compute_protocol::{
    ComputeReceipt, Digest32, MeasurementQuality, MeterKind, MeteredQuantity, PriceRate,
    WorkloadKind,
};

fn tag<const N: usize>(values: [&str; N]) -> Result<Tag, nostr::event::tag::Error> {
    Tag::parse(values)
}

pub(super) struct ReceiptFixture {
    pub(super) founder: Keys,
    pub(super) requester: Keys,
    pub(super) provider: Keys,
    pub(super) project: BuzzProjectContext,
    pub(super) pricing_event: Event,
    pub(super) job_event: Event,
    pub(super) request_event: Event,
    pub(super) result_event: Event,
    pub(super) receipt_event: Event,
}

pub(super) fn receipt_fixture(
    request_content_commitment: Digest32,
    result_content_commitment: Digest32,
) -> ReceiptFixture {
    receipt_fixture_for(
        Keys::generate(),
        request_content_commitment,
        result_content_commitment,
    )
}

pub(super) fn receipt_fixture_for(
    founder: Keys,
    request_content_commitment: Digest32,
    result_content_commitment: Digest32,
) -> ReceiptFixture {
    let requester = Keys::generate();
    let provider = Keys::generate();
    let node = SigningKey::from_bytes(&[3; 32]);
    let project_event = build_project(
        "compute-tests",
        Some("Compute tests"),
        None,
        &[],
        None,
        Some("listed"),
    )
    .expect("project builder")
    .sign_with_keys(&founder)
    .expect("project signature");
    let project = BuzzProjectContext::from_event(&project_event).expect("project context");
    let node_owner_id = hex::encode(Sha256::digest(node.verifying_key().to_bytes()));
    let policy = PricingPolicy {
        project: project.project.clone(),
        provider: format!("nostr:{}", provider.public_key().to_hex()),
        node_owner_id: node_owner_id.clone(),
        version: "test-v1".into(),
        fixed_sats: 1,
        rates: vec![PriceRate {
            meter: MeterKind::Jobs,
            units_per_rate: 1,
            rate_sats: 1,
        }],
        failed_jobs_billable: false,
    };
    let pricing_event = PricingContext::event_builder(&policy)
        .expect("pricing builder")
        .sign_with_keys(&provider)
        .expect("pricing signature");
    let input_commitment = [1; 32];
    let model = "unsloth/Qwen3-0.6B-GGUF:Q4_K_M";
    let request_event = EventBuilder::new(
        Kind::Custom(KIND_JOB_REQUEST as u16),
        serde_json::json!({
            "workload": "llm_inference",
            "model": model,
            "input_commitment": hex::encode(request_content_commitment)
        })
        .to_string(),
    )
    .tags([
        tag(["a", project.project.as_str()]).expect("project tag"),
        tag(["p", provider.public_key().to_hex().as_str()]).expect("provider tag"),
        tag(["st8-input", hex::encode(input_commitment).as_str()]).expect("input tag"),
    ])
    .sign_with_keys(&requester)
    .expect("request signature");
    let job = ComputeJob {
        project: project.project.clone(),
        requester: format!("nostr:{}", requester.public_key().to_hex()),
        provider: policy.provider.clone(),
        node_owner_id,
        workload: WorkloadKind::LlmInference,
        agent: None,
        model: model.into(),
        pricing_policy_id: policy.id().expect("policy id"),
        input_commitment,
        request_event_id: *request_event.id.as_bytes(),
        max_cost_sats: 10,
        created_at: 1_700_000_000,
        expires_at: 1_700_000_600,
        nonce: [2; 32],
    };
    let job_event = ComputeJobContext::event_builder(&job)
        .expect("job builder")
        .sign_with_keys(&requester)
        .expect("job signature");
    let result_commitment = [6; 32];
    let request_id = request_event.id.to_hex();
    let result_event = EventBuilder::new(
        Kind::Custom(KIND_JOB_RESULT as u16),
        serde_json::json!({
            "status": ExecutionStatus::Completed,
            "model": model,
            "output_commitment": hex::encode(result_content_commitment)
        })
        .to_string(),
    )
    .tags([
        tag(["a", project.project.as_str()]).expect("project tag"),
        tag(["e", request_id.as_str()]).expect("request link"),
        tag(["st8-request", request_id.as_str()]).expect("request tag"),
        tag(["st8-output", hex::encode(result_commitment).as_str()]).expect("output tag"),
    ])
    .sign_with_keys(&provider)
    .expect("result signature");
    let usage = vec![MeteredQuantity {
        kind: MeterKind::Jobs,
        quantity: 1,
        quality: MeasurementQuality::HostExact,
        source: "test.runtime.jobs".into(),
    }];
    let receipt = ComputeReceipt {
        job_id: job.id().expect("job id"),
        project: job.project.clone(),
        requester: job.requester.clone(),
        provider: job.provider.clone(),
        node_owner_id: job.node_owner_id.clone(),
        workload: job.workload,
        agent: None,
        model: job.model.clone(),
        pricing_policy_id: job.pricing_policy_id,
        request_event_id: job.request_event_id,
        result_event_id: *result_event.id.as_bytes(),
        agent_metric_event_id: None,
        started_at_ms: 1_700_000_001_000,
        ended_at_ms: 1_700_000_002_000,
        usage: usage.clone(),
        price: policy
            .price(ExecutionStatus::Completed, &usage)
            .expect("price"),
        result_commitment,
        status: ExecutionStatus::Completed,
        routing_target: "local:test-node".into(),
        meter_version: "test-meter-v1".into(),
        evidence: Vec::new(),
    };
    let mut attestation = NodeReceiptAttestation {
        receipt,
        node_public_key: node.verifying_key().to_bytes(),
        node_signature: Vec::new(),
    };
    attestation.node_signature = node
        .sign(&attestation.signing_bytes().expect("receipt signing bytes"))
        .to_bytes()
        .to_vec();
    let receipt_event = attestation
        .event_builder()
        .expect("receipt builder")
        .sign_with_keys(&provider)
        .expect("receipt signature");
    ReceiptFixture {
        founder,
        requester,
        provider,
        project,
        pricing_event,
        job_event,
        request_event,
        result_event,
        receipt_event,
    }
}
