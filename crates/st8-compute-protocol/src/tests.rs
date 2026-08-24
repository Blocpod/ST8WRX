use super::*;

fn project() -> String {
    format!("30621:{}:st8-compute", "ab".repeat(32))
}

fn pricing() -> PricingPolicy {
    PricingPolicy {
        project: project(),
        provider: format!("nostr:{}", "cd".repeat(32)),
        node_owner_id: "mesh-owner-1".into(),
        version: "qwen-local-v1".into(),
        fixed_sats: 1,
        rates: vec![
            PriceRate {
                meter: MeterKind::OutputTokens,
                units_per_rate: 1_000,
                rate_sats: 5,
            },
            PriceRate {
                meter: MeterKind::InputTokens,
                units_per_rate: 1_000,
                rate_sats: 2,
            },
        ],
        failed_jobs_billable: false,
    }
}

fn usage() -> Vec<MeteredQuantity> {
    vec![
        MeteredQuantity {
            kind: MeterKind::OutputTokens,
            quantity: 1_001,
            quality: MeasurementQuality::TargetExact,
            source: "openai.usage.completion_tokens".into(),
        },
        MeteredQuantity {
            kind: MeterKind::InputTokens,
            quantity: 500,
            quality: MeasurementQuality::TargetExact,
            source: "openai.usage.prompt_tokens".into(),
        },
    ]
}

#[test]
fn pricing_is_integer_deterministic_and_rounds_each_rule_up() {
    let policy = pricing();
    let price = policy
        .price(ExecutionStatus::Completed, &usage())
        .expect("price");
    assert_eq!(price.fixed_sats, 1);
    assert_eq!(price.lines[0].charge_sats, 1);
    assert_eq!(price.lines[1].charge_sats, 6);
    assert_eq!(price.total_sats, 8);
}

#[test]
fn pricing_id_ignores_rate_order_but_rejects_duplicates() {
    let left = pricing();
    let mut right = left.clone();
    right.rates.reverse();
    assert_eq!(left.id().expect("left"), right.id().expect("right"));
    right.rates.push(right.rates[0].clone());
    assert_eq!(right.id(), Err(ProtocolError::InvalidPricing));
}

#[test]
fn invalid_zero_denominator_pricing_fails_without_panicking() {
    let mut policy = pricing();
    policy.rates[0].units_per_rate = 0;
    assert_eq!(
        policy.price(ExecutionStatus::Completed, &usage()),
        Err(ProtocolError::InvalidPricing)
    );
}

#[test]
fn failed_jobs_are_zero_when_policy_does_not_bill_them() {
    assert_eq!(
        pricing()
            .price(ExecutionStatus::Failed, &usage())
            .expect("price")
            .total_sats,
        0
    );
}

#[test]
fn duplicate_meter_dimensions_are_rejected() {
    let mut duplicate = usage();
    duplicate.push(duplicate[0].clone());
    assert_eq!(
        pricing().price(ExecutionStatus::Completed, &duplicate),
        Err(ProtocolError::InvalidUsage)
    );
}

#[test]
fn settlement_excludes_disputes_and_rejects_replay() {
    let policy = pricing();
    let job = ComputeJob {
        project: project(),
        requester: format!("nostr:{}", "ef".repeat(32)),
        provider: policy.provider.clone(),
        node_owner_id: policy.node_owner_id.clone(),
        workload: WorkloadKind::LlmInference,
        agent: Some(format!("nostr:{}", "12".repeat(32))),
        model: "unsloth/Qwen3.5-4B-GGUF:Q4_K_M".into(),
        pricing_policy_id: policy.id().expect("policy"),
        input_commitment: [1; 32],
        request_event_id: [2; 32],
        max_cost_sats: 100,
        created_at: 1_700_000_000,
        expires_at: 1_700_000_600,
        nonce: [3; 32],
    };
    let receipt = ComputeReceipt {
        job_id: job.id().expect("job"),
        project: job.project.clone(),
        requester: job.requester.clone(),
        provider: job.provider.clone(),
        node_owner_id: job.node_owner_id.clone(),
        workload: job.workload,
        agent: job.agent.clone(),
        model: job.model.clone(),
        pricing_policy_id: job.pricing_policy_id,
        request_event_id: job.request_event_id,
        result_event_id: [4; 32],
        agent_metric_event_id: Some([5; 32]),
        started_at_ms: 1_700_000_001_000,
        ended_at_ms: 1_700_000_002_000,
        usage: usage(),
        price: policy
            .price(ExecutionStatus::Completed, &usage())
            .expect("price"),
        result_commitment: [6; 32],
        status: ExecutionStatus::Completed,
        routing_target: "mesh-endpoint-1".into(),
        meter_version: "mesh-openai-v1".into(),
        evidence: vec![[7; 32]],
    };
    receipt.validate_for(&job, &policy).expect("valid receipt");
    let disputed_id = receipt.id().expect("receipt");
    assert_eq!(
        ComputeSettlement::from_receipts(
            project(),
            1_699_999_000,
            1_700_001_000,
            std::slice::from_ref(&receipt),
            &[disputed_id],
        ),
        Err(ProtocolError::InvalidSettlement)
    );
    let included = ComputeSettlement::from_receipts(
        project(),
        1_699_999_000,
        1_700_001_000,
        std::slice::from_ref(&receipt),
        &[],
    )
    .expect("settlement");
    assert_eq!(
        included.receipt_merkle_root,
        settlement_receipt_root(&project(), &[disputed_id]).expect("root")
    );
    let mut duplicate_evidence = receipt.clone();
    duplicate_evidence.evidence.push([7; 32]);
    assert_eq!(
        duplicate_evidence.id(),
        Err(ProtocolError::ReceiptBindingMismatch)
    );
    assert_eq!(
        ComputeSettlement::from_receipts(
            project(),
            1_699_999_000,
            1_700_001_000,
            &[receipt.clone(), receipt],
            &[],
        ),
        Err(ProtocolError::InvalidSettlement)
    );
}

#[test]
fn settlement_includes_a_valid_billable_failed_receipt() {
    let mut policy = pricing();
    policy.failed_jobs_billable = true;
    let job = ComputeJob {
        project: project(),
        requester: format!("nostr:{}", "ef".repeat(32)),
        provider: policy.provider.clone(),
        node_owner_id: policy.node_owner_id.clone(),
        workload: WorkloadKind::LlmInference,
        agent: None,
        model: "unsloth/Qwen3-0.6B-GGUF:Q4_K_M".into(),
        pricing_policy_id: policy.id().expect("policy"),
        input_commitment: [1; 32],
        request_event_id: [2; 32],
        max_cost_sats: 100,
        created_at: 1_700_000_000,
        expires_at: 1_700_000_600,
        nonce: [3; 32],
    };
    let price = policy
        .price(ExecutionStatus::Failed, &usage())
        .expect("failed price");
    assert!(price.total_sats > 0);
    let receipt = ComputeReceipt {
        job_id: job.id().expect("job"),
        project: job.project.clone(),
        requester: job.requester.clone(),
        provider: job.provider.clone(),
        node_owner_id: job.node_owner_id.clone(),
        workload: job.workload,
        agent: None,
        model: job.model.clone(),
        pricing_policy_id: job.pricing_policy_id,
        request_event_id: job.request_event_id,
        result_event_id: [4; 32],
        agent_metric_event_id: None,
        started_at_ms: 1_700_000_001_000,
        ended_at_ms: 1_700_000_002_000,
        usage: usage(),
        price: price.clone(),
        result_commitment: [6; 32],
        status: ExecutionStatus::Failed,
        routing_target: "mesh-endpoint-1".into(),
        meter_version: "mesh-openai-v1".into(),
        evidence: Vec::new(),
    };
    receipt.validate_for(&job, &policy).expect("valid receipt");
    let settlement =
        ComputeSettlement::from_receipts(project(), 1_699_999_000, 1_700_001_000, &[receipt], &[])
            .expect("failed receipt settlement");
    assert_eq!(settlement.total_sats, price.total_sats);
    assert_eq!(settlement.balances[0].receipt_count, 1);
}
