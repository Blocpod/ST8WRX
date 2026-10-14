use super::*;

mod fixtures;
use fixtures::{receipt_fixture, ReceiptFixture};
use st8_compute_engine::{ComputeResolution, ComputeResolutionOutcome};

async fn test_state() -> (Arc<AppState>, TenantContext) {
    let database_url = std::env::var("BUZZ_COMPUTE_REPLAY_TEST_DATABASE_URL")
        .expect("set BUZZ_COMPUTE_REPLAY_TEST_DATABASE_URL to an isolated test database");
    let mut config = crate::config::Config::from_env().expect("config");
    config.database_url = database_url.clone();
    config.redis_url = "redis://127.0.0.1:1".into();
    let pool = sqlx::PgPool::connect(&database_url)
        .await
        .expect("test database");
    let db = buzz_db::Db::from_pool(pool.clone());
    db.migrate().await.expect("migrations");
    let host = format!("compute-{}.test", uuid::Uuid::new_v4());
    let community = db
        .ensure_configured_community(&host)
        .await
        .expect("community");
    let tenant = TenantContext::resolved(community.id, host);
    let redis_pool = deadpool_redis::Config::from_url(&config.redis_url)
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .expect("redis pool");
    let pubsub = Arc::new(
        buzz_pubsub::PubSubManager::new(&config.redis_url, redis_pool.clone())
            .await
            .expect("pubsub"),
    );
    let auth = buzz_auth::AuthService::new(config.auth.clone());
    let search = buzz_search::SearchService::new(pool);
    let workflow = Arc::new(buzz_workflow::WorkflowEngine::new(
        db.clone(),
        Default::default(),
    ));
    let media = buzz_media::MediaStorage::new(&config.media).expect("media");
    let (state, _) = AppState::new(
        config,
        db,
        redis_pool,
        None,
        pubsub,
        auth,
        search,
        workflow,
        nostr::Keys::generate(),
        media,
    );
    (Arc::new(state), tenant)
}

async fn store(state: &AppState, tenant: &TenantContext, event: &Event) {
    state
        .db
        .insert_event(tenant.community(), event, None)
        .await
        .expect("store input");
}

async fn store_fixture(state: &AppState, tenant: &TenantContext, fixture: &ReceiptFixture) {
    // Deliberately store in reverse dependency order with no side effects.
    for event in [
        &fixture.receipt_event,
        &fixture.result_event,
        &fixture.job_event,
        &fixture.request_event,
        &fixture.pricing_event,
        &fixture.project.project_event,
    ] {
        store(state, tenant, event).await;
    }
}

fn verified(fixture: &ReceiptFixture) -> VerifiedComputeReceipt {
    VerifiedComputeReceipt::from_events(
        fixture.project.clone(),
        &fixture.pricing_event,
        &fixture.job_event,
        &fixture.request_event,
        &fixture.result_event,
        &fixture.receipt_event,
    )
    .expect("verified fixture")
}

fn dispute_event(fixture: &ReceiptFixture) -> Event {
    let receipt = verified(fixture).attestation.receipt;
    ComputeDispute {
        project: fixture.project.project.clone(),
        receipt_id: receipt.id().unwrap(),
        job_id: receipt.job_id,
        disputed_at: 1_700_000_003,
        reason: "incorrect output".into(),
        evidence: vec![],
    }
    .event_builder()
    .unwrap()
    .sign_with_keys(&fixture.requester)
    .unwrap()
}

fn resolution_event(
    fixture: &ReceiptFixture,
    dispute: &Event,
    outcome: ComputeResolutionOutcome,
) -> Event {
    ComputeResolution {
        project: fixture.project.project.clone(),
        receipt_id: verified(fixture).attestation.receipt.id().unwrap(),
        dispute_event_id: *dispute.id.as_bytes(),
        outcome,
        resolved_at: 1_700_000_004,
        reason: "owner arbitration".into(),
    }
    .event_builder()
    .unwrap()
    .sign_with_keys(&fixture.founder)
    .unwrap()
}

#[tokio::test]
#[ignore = "requires BUZZ_COMPUTE_REPLAY_TEST_DATABASE_URL pointing to an isolated Postgres database"]
async fn compute_resolution_replay_is_final_and_preserves_original_receipts() {
    let (state, tenant) = test_state().await;
    for outcome in [
        ComputeResolutionOutcome::UpholdCharge,
        ComputeResolutionOutcome::VoidCharge,
    ] {
        let fixture = receipt_fixture([1; 32], [6; 32]);
        let second = fixtures::receipt_fixture_for(fixture.founder.clone(), [1; 32], [6; 32]);
        let receipt = verified(&fixture).attestation.receipt;
        let second_receipt = verified(&second).attestation.receipt;
        let receipt_id = receipt.id().unwrap();
        let dispute = dispute_event(&fixture);
        let resolution = resolution_event(&fixture, &dispute, outcome);
        // Store dependent decisions first; recovery must execute causal passes.
        store(&state, &tenant, &resolution).await;
        store(&state, &tenant, &dispute).await;
        store_fixture(&state, &tenant, &fixture).await;
        store_fixture(&state, &tenant, &second).await;
        replay::replay_inputs(&tenant, &state).await.unwrap();
        let record = state
            .db
            .get_compute_receipt(tenant.community(), &receipt_id)
            .await
            .unwrap()
            .unwrap();
        let voided = outcome == ComputeResolutionOutcome::VoidCharge;
        assert_eq!(
            record.dispute_state,
            if voided {
                "resolved_upheld"
            } else {
                "resolved_rejected"
            }
        );
        assert_eq!(
            record.ledger_projection["resolution_event"],
            serde_json::to_value(&resolution).unwrap()
        );
        assert_eq!(record.cost_sats as u64, receipt.price.total_sats);
        assert_eq!(
            serde_json::from_value::<VerifiedComputeReceipt>(record.verified_material)
                .unwrap()
                .attestation
                .receipt,
            receipt
        );
        assert_eq!(
            record.ledger_projection["billable_cost_sats"],
            if voided { 0 } else { receipt.price.total_sats }
        );
        handle_resolution(&tenant, &resolution, &state)
            .await
            .expect("exact duplicate resolution");
        handle_dispute(&tenant, &dispute, &state)
            .await
            .expect("old dispute retry cannot reopen");
        let new_dispute = ComputeDispute {
            project: fixture.project.project.clone(),
            receipt_id,
            job_id: receipt.job_id,
            disputed_at: 1_700_000_010,
            reason: "try reopening".into(),
            evidence: vec![],
        }
        .event_builder()
        .unwrap()
        .sign_with_keys(&fixture.requester)
        .unwrap();
        assert!(handle_dispute(&tenant, &new_dispute, &state).await.is_err());
        let opposite = if voided {
            ComputeResolutionOutcome::UpholdCharge
        } else {
            ComputeResolutionOutcome::VoidCharge
        };
        let conflict = resolution_event(&fixture, &dispute, opposite);
        assert!(handle_resolution(&tenant, &conflict, &state).await.is_err());
        let exclusions = if voided { vec![receipt_id] } else { vec![] };
        let settlement = ComputeSettlement::from_receipts(
            fixture.project.project.clone(),
            1_700_000_000,
            1_700_000_100,
            &[receipt.clone(), second_receipt.clone()],
            &exclusions,
        )
        .unwrap();
        assert_eq!(
            settlement.total_sats,
            second_receipt.price.total_sats + if voided { 0 } else { receipt.price.total_sats }
        );
        let settlement_event = SettlementContext::event_builder(&settlement)
            .unwrap()
            .sign_with_keys(&fixture.founder)
            .unwrap();
        handle_settlement(&tenant, &settlement_event, &state)
            .await
            .unwrap();
        handle_resolution(&tenant, &resolution, &state)
            .await
            .expect("exact resolution retry after settlement");
        let final_record = state
            .db
            .get_compute_receipt(tenant.community(), &receipt_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(final_record.settlement_id.is_none(), voided);
    }
}

#[tokio::test]
#[ignore = "requires BUZZ_COMPUTE_REPLAY_TEST_DATABASE_URL pointing to an isolated Postgres database"]
async fn compute_resolution_conflicting_race_has_one_winner_and_settlement_never_bills_void() {
    let (state, tenant) = test_state().await;
    let fixture = receipt_fixture([1; 32], [6; 32]);
    store_fixture(&state, &tenant, &fixture).await;
    replay::replay_inputs(&tenant, &state).await.unwrap();
    let dispute = dispute_event(&fixture);
    handle_dispute(&tenant, &dispute, &state).await.unwrap();
    let uphold = resolution_event(&fixture, &dispute, ComputeResolutionOutcome::UpholdCharge);
    let void = resolution_event(&fixture, &dispute, ComputeResolutionOutcome::VoidCharge);
    let (a, b) = tokio::join!(
        handle_resolution(&tenant, &uphold, &state),
        handle_resolution(&tenant, &void, &state)
    );
    assert_ne!(
        a.is_ok(),
        b.is_ok(),
        "exactly one final decision may commit"
    );
    let receipt = verified(&fixture).attestation.receipt;
    let settlement = ComputeSettlement::from_receipts(
        fixture.project.project.clone(),
        1_700_000_000,
        1_700_000_100,
        std::slice::from_ref(&receipt),
        &[],
    )
    .unwrap();
    let event = SettlementContext::event_builder(&settlement)
        .unwrap()
        .sign_with_keys(&fixture.founder)
        .unwrap();
    let (settled, void_retry) = tokio::join!(
        handle_settlement(&tenant, &event, &state),
        handle_resolution(&tenant, &void, &state)
    );
    assert_ne!(
        settled.is_ok(),
        void_retry.is_ok(),
        "voided receipt cannot be consumed"
    );
    let record = state
        .db
        .get_compute_receipt(tenant.community(), &receipt.id().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        record.settlement_id.is_some(),
        record.dispute_state == "resolved_rejected"
    );
}

#[tokio::test]
#[ignore = "requires BUZZ_COMPUTE_REPLAY_TEST_DATABASE_URL pointing to an isolated Postgres database"]
async fn compute_replay_recovers_storage_only_inputs_without_resettling() {
    let (state, tenant) = test_state().await;
    let fixture = receipt_fixture([1; 32], [6; 32]);
    let receipt = verified(&fixture).attestation.receipt;
    let receipt_id = receipt.id().expect("receipt ID");
    let settlement = ComputeSettlement::from_receipts(
        fixture.project.project.clone(),
        1_700_000_000,
        1_700_000_100,
        &[receipt],
        &[],
    )
    .expect("settlement");
    let settlement_event = SettlementContext::event_builder(&settlement)
        .expect("builder")
        .sign_with_keys(&fixture.founder)
        .expect("signature");
    store(&state, &tenant, &settlement_event).await;
    store_fixture(&state, &tenant, &fixture).await;
    // A malformed sibling must not block complete events in the same pass.
    for index in 0..501 {
        let invalid = EventBuilder::new(
            Kind::Custom(KIND_ST8_COMPUTE_JOB as u16),
            format!("{{\"invalid\":{index}}}"),
        )
        .custom_created_at(nostr::Timestamp::from(1_900_000_000))
        .sign_with_keys(&fixture.requester)
        .expect("invalid signed fixture");
        store(&state, &tenant, &invalid).await;
    }
    assert!(state
        .db
        .get_compute_receipt(tenant.community(), &receipt_id)
        .await
        .unwrap()
        .is_none());
    replay::replay_inputs(&tenant, &state)
        .await
        .expect("replay");
    let record = state
        .db
        .get_compute_receipt(tenant.community(), &receipt_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        record.settlement_id,
        Some(settlement.id().unwrap().to_vec())
    );
    assert!(
        !emit_receipt_projection(&tenant, &state, &record)
            .await
            .unwrap(),
        "the settled projection must already be the latest, even within one second"
    );
    handle_settlement(&tenant, &settlement_event, &state)
        .await
        .expect("idempotent settlement");
    replay::replay_inputs(&tenant, &state)
        .await
        .expect("restart replay");
    assert_eq!(
        state
            .db
            .list_community_compute_settlements(tenant.community(), 10, 0)
            .await
            .unwrap()
            .len(),
        1
    );
    // A fresh settlement that tries to consume the same receipt is rejected.
    let mut duplicate = settlement.clone();
    duplicate.period_end += 1;
    let duplicate = SettlementContext::event_builder(&duplicate)
        .unwrap()
        .sign_with_keys(&fixture.founder)
        .unwrap();
    assert!(handle_settlement(&tenant, &duplicate, &state)
        .await
        .is_err());
    let other = state
        .db
        .ensure_configured_community(&format!("other-{}.test", uuid::Uuid::new_v4()))
        .await
        .unwrap();
    assert!(state
        .db
        .get_compute_receipt(other.id, &receipt_id)
        .await
        .unwrap()
        .is_none());
    assert!(state
        .db
        .get_compute_settlement_event(other.id, &settlement.id().unwrap())
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
#[ignore = "requires BUZZ_COMPUTE_REPLAY_TEST_DATABASE_URL pointing to an isolated Postgres database"]
async fn compute_replay_recovers_disputes_before_settlement_and_rejects_rogue_signer() {
    let (state, tenant) = test_state().await;
    let fixture = receipt_fixture([1; 32], [6; 32]);
    let receipt = verified(&fixture).attestation.receipt;
    let receipt_id = receipt.id().unwrap();
    let dispute = ComputeDispute {
        project: fixture.project.project.clone(),
        receipt_id,
        job_id: receipt.job_id,
        disputed_at: 1_700_000_003,
        reason: "wrong result".into(),
        evidence: vec![],
    };
    let rogue = dispute
        .event_builder()
        .unwrap()
        .sign_with_keys(&fixture.provider)
        .unwrap();
    store(&state, &tenant, &rogue).await;
    // Missing request/result dependencies survive a failed pass and recover later.
    store(&state, &tenant, &fixture.job_event).await;
    store(&state, &tenant, &fixture.receipt_event).await;
    replay::replay_inputs(&tenant, &state).await.unwrap();
    assert!(state
        .db
        .get_compute_receipt(tenant.community(), &receipt_id)
        .await
        .unwrap()
        .is_none());
    store_fixture(&state, &tenant, &fixture).await;
    replay::replay_inputs(&tenant, &state).await.unwrap();
    assert_eq!(
        state
            .db
            .get_compute_receipt(tenant.community(), &receipt_id)
            .await
            .unwrap()
            .unwrap()
            .dispute_state,
        "undisputed"
    );
    let settlement = ComputeSettlement::from_receipts(
        fixture.project.project.clone(),
        1_700_000_000,
        1_700_000_100,
        &[receipt],
        &[],
    )
    .unwrap();
    let settlement_event = SettlementContext::event_builder(&settlement)
        .unwrap()
        .sign_with_keys(&fixture.founder)
        .unwrap();
    store(&state, &tenant, &settlement_event).await;
    let event = dispute
        .event_builder()
        .unwrap()
        .sign_with_keys(&fixture.requester)
        .unwrap();
    store(&state, &tenant, &event).await;
    replay::replay_inputs(&tenant, &state).await.unwrap();
    let record = state
        .db
        .get_compute_receipt(tenant.community(), &receipt_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.dispute_state, "disputed");
    assert!(
        !emit_receipt_projection(&tenant, &state, &record)
            .await
            .unwrap(),
        "same-second dispute must replace the initial undisputed projection"
    );
    assert!(record.settlement_id.is_none());
    handle_dispute(&tenant, &event, &state)
        .await
        .expect("duplicate dispute");
    assert!(state
        .db
        .list_community_compute_settlements(tenant.community(), 10, 0)
        .await
        .unwrap()
        .is_empty());
}
