use super::*;
use buzz_core::Keys;
use buzz_sdk::build_project;
use ed25519_dalek::{Signer, SigningKey};
use st8_compute_protocol::{
    ExecutionStatus, MeasurementQuality, MeterKind, MeteredQuantity, PriceRate, WorkloadKind,
};

fn capability_for(key: &SigningKey) -> NodeCapability {
    let public_key = key.verifying_key().to_bytes();
    NodeCapability {
        node_owner_id: hex::encode(Sha256::digest(public_key)),
        node_public_key: public_key,
        provider: format!("nostr:{}", "11".repeat(32)),
        runtime_version: "mesh-llm-v0.75.1".into(),
        cpu_cores: 8,
        ram_bytes: 32_000_000_000,
        accelerators: Vec::new(),
        models: Vec::new(),
        workloads: vec![WorkloadKind::LlmInference],
        ingress_bits_per_second: None,
        egress_bits_per_second: None,
        available_until: 1_800_000_000,
        pricing_policy_id: [7; 32],
        reputation_refs: Vec::new(),
    }
}

fn signed_capability(key: &SigningKey) -> NodeCapabilityAttestation {
    let capability = capability_for(key);
    let mut attestation = NodeCapabilityAttestation {
        capability,
        node_signature: Vec::new(),
    };
    attestation.node_signature = key
        .sign(&attestation.signing_bytes().expect("signing bytes"))
        .to_bytes()
        .to_vec();
    attestation
}

struct ReceiptFixture {
    requester: Keys,
    provider: Keys,
    project: BuzzProjectContext,
    pricing_event: Event,
    job_event: Event,
    request_event: Event,
    result_event: Event,
    receipt_event: Event,
}

fn receipt_fixture(
    request_content_commitment: Digest32,
    result_content_commitment: Digest32,
) -> ReceiptFixture {
    let founder = Keys::generate();
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

fn verify_fixture(fixture: ReceiptFixture) -> Result<VerifiedComputeReceipt, EngineError> {
    VerifiedComputeReceipt::from_events(
        fixture.project,
        &fixture.pricing_event,
        &fixture.job_event,
        &fixture.request_event,
        &fixture.result_event,
        &fixture.receipt_event,
    )
}

#[test]
fn persistent_mesh_owner_signature_verifies() {
    let key = SigningKey::from_bytes(&[3; 32]);
    signed_capability(&key)
        .verify_node_signature()
        .expect("valid node signature");
}

#[test]
fn node_key_substitution_is_rejected() {
    let key = SigningKey::from_bytes(&[3; 32]);
    let attacker = SigningKey::from_bytes(&[4; 32]);
    let mut attestation = signed_capability(&key);
    attestation.capability.node_public_key = attacker.verifying_key().to_bytes();
    assert!(matches!(
        attestation.verify_node_signature(),
        Err(EngineError::NodeIdentityMismatch)
    ));
}

#[test]
fn capability_mutation_after_signing_is_rejected() {
    let key = SigningKey::from_bytes(&[3; 32]);
    let mut attestation = signed_capability(&key);
    attestation.capability.ram_bytes += 1;
    assert!(matches!(
        attestation.verify_node_signature(),
        Err(EngineError::InvalidNodeSignature)
    ));
}

#[test]
fn truncated_node_signature_is_rejected() {
    let key = SigningKey::from_bytes(&[3; 32]);
    let mut attestation = signed_capability(&key);
    attestation.node_signature.pop();
    assert!(matches!(
        attestation.verify_node_signature(),
        Err(EngineError::InvalidNodeSignature)
    ));
}

#[test]
fn full_receipt_chain_verifies() {
    let fixture = receipt_fixture([1; 32], [6; 32]);
    verify_fixture(fixture).expect("full receipt chain");
}

#[test]
fn signed_request_content_and_tag_disagreement_is_rejected() {
    let fixture = receipt_fixture([9; 32], [6; 32]);
    assert!(matches!(
        VerifiedComputeReceipt::from_events(
            fixture.project,
            &fixture.pricing_event,
            &fixture.job_event,
            &fixture.request_event,
            &fixture.result_event,
            &fixture.receipt_event,
        ),
        Err(EngineError::EventBindingMismatch)
    ));
}

#[test]
fn requester_signed_job_with_substituted_provider_tag_is_rejected() {
    let mut fixture = receipt_fixture([1; 32], [6; 32]);
    let job: ComputeJob = serde_json::from_str(&fixture.job_event.content).expect("job");
    let job_id = hex::encode(job.id().expect("job id"));
    let request_id = fixture.request_event.id.to_hex();
    let pricing_id = hex::encode(job.pricing_policy_id);
    fixture.job_event = EventBuilder::new(
        Kind::Custom(KIND_ST8_COMPUTE_JOB as u16),
        fixture.job_event.content.clone(),
    )
    .tags([
        tag(["a", job.project.as_str()]).expect("project tag"),
        tag(["d", job_id.as_str()]).expect("job tag"),
        tag(["e", request_id.as_str()]).expect("request tag"),
        tag(["p", "22".repeat(32).as_str()]).expect("substituted provider tag"),
        tag(["st8-node", job.node_owner_id.as_str()]).expect("node tag"),
        tag(["st8-pricing", pricing_id.as_str()]).expect("pricing tag"),
    ])
    .sign_with_keys(&fixture.requester)
    .expect("job signature");
    assert!(matches!(
        verify_fixture(fixture),
        Err(EngineError::EventBindingMismatch)
    ));
}

#[test]
fn requester_signed_request_with_a_different_model_is_rejected() {
    let mut fixture = receipt_fixture([1; 32], [6; 32]);
    let provider = fixture.provider.public_key().to_hex();
    fixture.request_event = EventBuilder::new(
        Kind::Custom(KIND_JOB_REQUEST as u16),
        serde_json::json!({
            "workload": "llm_inference",
            "model": "attacker/substituted-model",
            "input_commitment": hex::encode([1; 32])
        })
        .to_string(),
    )
    .tags([
        tag(["a", fixture.project.project.as_str()]).expect("project tag"),
        tag(["p", provider.as_str()]).expect("provider tag"),
        tag(["st8-input", hex::encode([1; 32]).as_str()]).expect("input tag"),
    ])
    .sign_with_keys(&fixture.requester)
    .expect("request signature");
    let mut job: ComputeJob = serde_json::from_str(&fixture.job_event.content).expect("job");
    job.request_event_id = *fixture.request_event.id.as_bytes();
    fixture.job_event = ComputeJobContext::event_builder(&job)
        .expect("job builder")
        .sign_with_keys(&fixture.requester)
        .expect("job signature");
    let pricing = PricingContext::from_event(&fixture.project, &fixture.pricing_event)
        .expect("pricing context");
    assert!(matches!(
        ComputeJobContext::from_events(
            &fixture.project,
            &fixture.job_event,
            &pricing,
            &fixture.request_event,
        ),
        Err(EngineError::EventBindingMismatch)
    ));
}

#[test]
fn provider_signed_result_with_substituted_request_link_is_rejected() {
    let mut fixture = receipt_fixture([1; 32], [6; 32]);
    let request_id = fixture.request_event.id.to_hex();
    fixture.result_event = EventBuilder::new(
        Kind::Custom(KIND_JOB_RESULT as u16),
        fixture.result_event.content.clone(),
    )
    .tags([
        tag(["a", fixture.project.project.as_str()]).expect("project tag"),
        tag(["e", "33".repeat(32).as_str()]).expect("substituted request link"),
        tag(["st8-request", request_id.as_str()]).expect("request tag"),
        tag(["st8-output", hex::encode([6; 32]).as_str()]).expect("output tag"),
    ])
    .sign_with_keys(&fixture.provider)
    .expect("result signature");
    let node = SigningKey::from_bytes(&[3; 32]);
    let mut attestation: NodeReceiptAttestation =
        serde_json::from_str(&fixture.receipt_event.content).expect("attestation");
    attestation.receipt.result_event_id = *fixture.result_event.id.as_bytes();
    attestation.node_signature = node
        .sign(&attestation.signing_bytes().expect("receipt signing bytes"))
        .to_bytes()
        .to_vec();
    fixture.receipt_event = attestation
        .event_builder()
        .expect("receipt builder")
        .sign_with_keys(&fixture.provider)
        .expect("receipt signature");
    assert!(matches!(
        verify_fixture(fixture),
        Err(EngineError::EventBindingMismatch)
    ));
}

#[test]
fn provider_signed_result_with_a_different_status_is_rejected() {
    let mut fixture = receipt_fixture([1; 32], [6; 32]);
    let request_id = fixture.request_event.id.to_hex();
    fixture.result_event = EventBuilder::new(
        Kind::Custom(KIND_JOB_RESULT as u16),
        serde_json::json!({
            "status": ExecutionStatus::Failed,
            "model": "unsloth/Qwen3-0.6B-GGUF:Q4_K_M",
            "output_commitment": hex::encode([6; 32])
        })
        .to_string(),
    )
    .tags([
        tag(["a", fixture.project.project.as_str()]).expect("project tag"),
        tag(["e", request_id.as_str()]).expect("request link"),
        tag(["st8-request", request_id.as_str()]).expect("request tag"),
        tag(["st8-output", hex::encode([6; 32]).as_str()]).expect("output tag"),
    ])
    .sign_with_keys(&fixture.provider)
    .expect("result signature");
    let node = SigningKey::from_bytes(&[3; 32]);
    let mut attestation: NodeReceiptAttestation =
        serde_json::from_str(&fixture.receipt_event.content).expect("attestation");
    attestation.receipt.result_event_id = *fixture.result_event.id.as_bytes();
    attestation.node_signature = node
        .sign(&attestation.signing_bytes().expect("receipt signing bytes"))
        .to_bytes()
        .to_vec();
    fixture.receipt_event = attestation
        .event_builder()
        .expect("receipt builder")
        .sign_with_keys(&fixture.provider)
        .expect("receipt signature");
    assert!(matches!(
        verify_fixture(fixture),
        Err(EngineError::EventBindingMismatch)
    ));
}

#[test]
fn provider_signed_receipt_with_substituted_requester_tag_is_rejected() {
    let mut fixture = receipt_fixture([1; 32], [6; 32]);
    let attestation: NodeReceiptAttestation =
        serde_json::from_str(&fixture.receipt_event.content).expect("attestation");
    let receipt_id = hex::encode(attestation.receipt.id().expect("receipt id"));
    let result_id = fixture.result_event.id.to_hex();
    let job_id = hex::encode(attestation.receipt.job_id);
    fixture.receipt_event = EventBuilder::new(
        Kind::Custom(KIND_ST8_COMPUTE_RECEIPT as u16),
        fixture.receipt_event.content.clone(),
    )
    .tags([
        tag(["a", fixture.project.project.as_str()]).expect("project tag"),
        tag(["d", receipt_id.as_str()]).expect("receipt tag"),
        tag(["e", result_id.as_str()]).expect("result tag"),
        tag(["p", "44".repeat(32).as_str()]).expect("substituted requester tag"),
        tag(["st8-job", job_id.as_str()]).expect("job tag"),
        tag(["st8-node", attestation.receipt.node_owner_id.as_str()]).expect("node tag"),
    ])
    .sign_with_keys(&fixture.provider)
    .expect("receipt signature");
    assert!(matches!(
        verify_fixture(fixture),
        Err(EngineError::EventBindingMismatch)
    ));
}

#[test]
fn signed_result_content_and_tag_disagreement_is_rejected() {
    let fixture = receipt_fixture([1; 32], [9; 32]);
    assert!(matches!(
        VerifiedComputeReceipt::from_events(
            fixture.project,
            &fixture.pricing_event,
            &fixture.job_event,
            &fixture.request_event,
            &fixture.result_event,
            &fixture.receipt_event,
        ),
        Err(EngineError::EventBindingMismatch)
    ));
}
