use super::*;
use nostr::{Keys, Timestamp};
use st8_contribution_protocol::{ContributionClass, ContributorKind};

async fn test_state() -> (Arc<AppState>, TenantContext, sqlx::PgPool) {
    let database_url = std::env::var("BUZZ_CONTRIBUTION_REPLAY_TEST_DATABASE_URL")
        .expect("set BUZZ_CONTRIBUTION_REPLAY_TEST_DATABASE_URL to an isolated test database");
    let mut config = crate::config::Config::from_env().expect("config");
    config.database_url = database_url.clone();
    config.redis_url = "redis://127.0.0.1:1".into();
    let pool = sqlx::PgPool::connect(&database_url)
        .await
        .expect("test database");
    let db = buzz_db::Db::from_pool(pool.clone());
    db.migrate().await.expect("migrations");
    let host = format!("contribution-{}.test", uuid::Uuid::new_v4());
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
    let search = buzz_search::SearchService::new(pool.clone());
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
    (Arc::new(state), tenant, pool)
}

async fn store(state: &AppState, tenant: &TenantContext, event: &Event) {
    state
        .db
        .insert_event(tenant.community(), event, None)
        .await
        .expect("store input");
}

struct Fixture {
    owner: Keys,
    founder: Keys,
    project: BuzzProjectContext,
    policy: GovernancePolicyContext,
    claim: ContributionClaimContext,
    proposal: Event,
    intent: GovernanceDecisionIntent,
}

fn fixture() -> Fixture {
    let owner = Keys::generate();
    let founder = Keys::generate();
    let at = Timestamp::from(1_700_000_000);
    let project_event = buzz_sdk::build_project("recovery", None, None, &[], None, Some("listed"))
        .unwrap()
        .custom_created_at(at)
        .sign_with_keys(&owner)
        .unwrap();
    let project = BuzzProjectContext::from_event(&project_event).unwrap();
    let policy_event = GovernancePolicy {
        project: project.project.clone(),
        version: "v1".into(),
        founders: vec![
            format!("nostr:{}", owner.public_key().to_hex()),
            format!("nostr:{}", founder.public_key().to_hex()),
        ],
        normal_threshold: 2,
        large_threshold: 2,
        large_unit_threshold: 100,
    }
    .event_builder()
    .unwrap()
    .custom_created_at(at)
    .sign_with_keys(&owner)
    .unwrap();
    let policy = GovernancePolicyContext::from_event(&project, &policy_event).unwrap();
    let claim_event = ContributionClaimBody {
        contributor_kind: ContributorKind::Human,
        class: ContributionClass::Engineering,
        summary: "Recoverable work".into(),
    }
    .event_builder(&project.project, std::slice::from_ref(&project_event))
    .unwrap()
    .custom_created_at(at)
    .sign_with_keys(&owner)
    .unwrap();
    let claim = ContributionClaimContext::from_event(
        &project,
        &claim_event,
        std::slice::from_ref(&project_event),
    )
    .unwrap();
    let intent = policy
        .policy
        .decision_intent(
            &claim.record,
            DecisionStatus::Accepted,
            7,
            1_700_000_600,
            "Reviewed".into(),
        )
        .unwrap();
    let proposal = intent
        .proposal_event_builder_for_claim_and_policy(&claim_event, &policy_event)
        .unwrap()
        .custom_created_at(at)
        .sign_with_keys(&owner)
        .unwrap();
    Fixture {
        owner,
        founder,
        project,
        policy,
        claim,
        proposal,
        intent,
    }
}

fn approval(fixture: &Fixture, signer: &Keys, timestamp: u64) -> Event {
    fixture
        .intent
        .approval_event_builder_for_proposal(&fixture.proposal)
        .unwrap()
        .custom_created_at(Timestamp::from(timestamp))
        .sign_with_keys(signer)
        .unwrap()
}

async fn dependencies(state: &AppState, tenant: &TenantContext, fixture: &Fixture) {
    for event in [
        &fixture.proposal,
        &fixture.claim.claim_event,
        &fixture.policy.policy_event,
        &fixture.project.project_event,
    ] {
        store(state, tenant, event).await;
    }
}

async fn counts(pool: &sqlx::PgPool, tenant: &TenantContext) -> (i64, i64, i64, i64) {
    sqlx::query_as(
        "SELECT
        (SELECT count(*) FROM st8_contributions WHERE community_id=$1),
        (SELECT count(*) FROM st8_project_snapshots WHERE community_id=$1),
        (SELECT count(*) FROM st8_anchor_jobs WHERE community_id=$1),
        (SELECT count(*) FROM st8_governance_approvals WHERE community_id=$1)",
    )
    .bind(tenant.community().as_uuid())
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires isolated BUZZ_CONTRIBUTION_REPLAY_TEST_DATABASE_URL"]
async fn stored_approvals_recover_once_after_dependencies_arrive_despite_poisoned_pages() {
    let (state, tenant, pool) = test_state().await;
    let fixture = fixture();
    let first = approval(&fixture, &fixture.owner, 1_700_000_001);
    let second = approval(&fixture, &fixture.founder, 1_700_000_002);
    store(&state, &tenant, &first).await;
    store(&state, &tenant, &second).await;
    // Simulate durable acceptance followed by a crash, before dependencies or
    // side effects are available. Missing material must remain retryable.
    reconcile_stored_approvals(&state).await.unwrap();
    assert_eq!(counts(&pool, &tenant).await, (0, 0, 0, 0));
    dependencies(&state, &tenant, &fixture).await;
    let rogue = approval(&fixture, &Keys::generate(), 1_700_000_003);
    store(&state, &tenant, &rogue).await;
    // More than a full candidate page of correctly signed, envelope-valid but
    // expired founder approvals must not hide the valid older threshold.
    for offset in 0..300 {
        let late = approval(&fixture, &fixture.owner, 1_700_000_601 + offset);
        validate_envelope(&late).unwrap();
        store(&state, &tenant, &late).await;
    }
    let candidates = approvals_for_proposal(
        &tenant,
        &state,
        &fixture.proposal,
        &fixture.policy,
        &fixture.claim,
        &fixture.intent,
    )
    .await
    .unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(candidates
        .iter()
        .all(|event| event.id == first.id || event.id == second.id));
    reconcile_stored_approvals(&state).await.unwrap();
    reconcile_ledger_events(&state).await.unwrap();
    assert_eq!(counts(&pool, &tenant).await, (1, 1, 1, 2));
    let rows = state
        .db
        .list_community_contributions(tenant.community(), 10, 0)
        .await
        .unwrap();
    assert_eq!(rows[0].contribution_units, 7);
    let before = serde_json::to_value(&rows[0].ledger_projection).unwrap();
    let snapshot = rows[0].project_snapshot_id.clone();
    // Duplicate retry and repeated startup/tick passes preserve one immutable
    // award, its snapshot, and its queue item, with no rogue approval retained.
    handle_approval(&tenant, &second, &state).await.unwrap();
    reconcile_stored_approvals(&state).await.unwrap();
    reconcile_ledger_events(&state).await.unwrap();
    assert_eq!(counts(&pool, &tenant).await, (1, 1, 1, 2));
    let rows = state
        .db
        .list_community_contributions(tenant.community(), 10, 0)
        .await
        .unwrap();
    assert_eq!(rows[0].project_snapshot_id, snapshot);
    assert_eq!(
        serde_json::to_value(&rows[0].ledger_projection).unwrap(),
        before
    );
}

#[tokio::test]
#[ignore = "requires isolated BUZZ_CONTRIBUTION_REPLAY_TEST_DATABASE_URL"]
async fn invalid_approvals_never_supply_a_missing_founder_threshold() {
    let (state, tenant, pool) = test_state().await;
    let fixture = fixture();
    dependencies(&state, &tenant, &fixture).await;
    let first = approval(&fixture, &fixture.owner, 1_700_000_001);
    let repeat = approval(&fixture, &fixture.owner, 1_700_000_002);
    let rogue = approval(&fixture, &Keys::generate(), 1_700_000_003);
    let late = approval(&fixture, &fixture.founder, 1_700_000_601);
    for event in [&first, &repeat, &rogue, &late] {
        store(&state, &tenant, event).await;
    }
    handle_approval(&tenant, &first, &state).await.unwrap();
    assert_eq!(counts(&pool, &tenant).await, (0, 0, 0, 0));
    let valid = approval(&fixture, &fixture.founder, 1_700_000_004);
    store(&state, &tenant, &valid).await;
    handle_approval(&tenant, &valid, &state).await.unwrap();
    assert_eq!(counts(&pool, &tenant).await, (1, 1, 1, 2));
}
