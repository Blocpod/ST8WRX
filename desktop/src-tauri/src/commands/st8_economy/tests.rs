use super::*;
use st8_contribution_engine::{ContributionClaimBody, ContributionClaimContext, GovernancePolicy};
use st8_contribution_protocol::{ContributionClass, ContributorKind, DecisionStatus};

#[test]
fn approval_verifies_full_project_policy_claim_and_proposal_material() {
    let owner = Keys::generate();
    let founder = Keys::generate();
    let stranger = Keys::generate();
    let project_event =
        buzz_sdk_pkg::build_project("economy-test", None, None, &[], None, Some("listed"))
            .unwrap()
            .sign_with_keys(&owner)
            .unwrap();
    let project = BuzzProjectContext::from_event(&project_event).unwrap();
    let claim_event = ContributionClaimBody {
        contributor_kind: ContributorKind::Human,
        class: ContributionClass::Engineering,
        summary: "Initial project work".into(),
    }
    .event_builder(&project.project, std::slice::from_ref(&project_event))
    .unwrap()
    .sign_with_keys(&owner)
    .unwrap();
    let claim = ContributionClaimContext::from_event(
        &project,
        &claim_event,
        std::slice::from_ref(&project_event),
    )
    .unwrap();
    let policy = GovernancePolicy {
        project: project.project.clone(),
        version: "v1".into(),
        founders: vec![
            format!("nostr:{}", owner.public_key().to_hex()),
            format!("nostr:{}", founder.public_key().to_hex()),
        ],
        normal_threshold: 2,
        large_threshold: 2,
        large_unit_threshold: 100,
    };
    let policy_event = policy
        .event_builder()
        .unwrap()
        .sign_with_keys(&owner)
        .unwrap();
    let intent = policy
        .decision_intent(
            &claim.record,
            DecisionStatus::Accepted,
            5,
            i64::try_from(nostr::Timestamp::now().as_secs()).unwrap() + 600,
            "Reviewed work".into(),
        )
        .unwrap();
    let proposal = intent
        .proposal_event_builder_for_claim_and_policy(&claim_event, &policy_event)
        .unwrap()
        .sign_with_keys(&owner)
        .unwrap();
    assert!(
        actions::approved_builder(&project, &claim, &policy_event, &proposal, &founder).is_ok(),
        "a valid first approval must work before the threshold is reached"
    );
    assert!(
        actions::approved_builder(&project, &claim, &policy_event, &proposal, &stranger).is_err()
    );
    let forged_policy = policy
        .event_builder()
        .unwrap()
        .sign_with_keys(&stranger)
        .unwrap();
    assert!(
        actions::approved_builder(&project, &claim, &forged_policy, &proposal, &founder).is_err()
    );
    let mut changed = intent;
    changed.contribution_units = 50;
    let changed = EventBuilder::new(proposal.kind, serde_json::to_string(&changed).unwrap())
        .tags(proposal.tags.clone())
        .sign_with_keys(&owner)
        .unwrap();
    assert!(
        actions::approved_builder(&project, &claim, &policy_event, &changed, &founder).is_err(),
        "valid signatures cannot authorize a proposal whose binding tags were altered"
    );
}

#[test]
fn evidence_is_public_and_exactly_project_or_repository_scoped() {
    let owner = Keys::generate();
    let event = buzz_sdk_pkg::build_project("economy-test", None, None, &[], None, Some("listed"))
        .unwrap()
        .sign_with_keys(&owner)
        .unwrap();
    let project = BuzzProjectContext::from_event(&event).unwrap();
    assert!(public_evidence(&project, &event));
    let evidence = |coordinate: &str, private: bool| {
        let mut tags = vec![nostr::Tag::parse(["a", coordinate]).unwrap()];
        if private {
            tags.push(nostr::Tag::parse(["h", "private-channel"]).unwrap());
        }
        EventBuilder::new(nostr::Kind::TextNote, "work")
            .tags(tags)
            .sign_with_keys(&owner)
            .unwrap()
    };
    assert!(public_evidence(
        &project,
        &evidence(&project.project, false)
    ));
    assert!(!public_evidence(
        &project,
        &evidence(&project.project, true)
    ));
    assert!(!public_evidence(
        &project,
        &evidence("30621:other:project", false)
    ));
    assert!(project_parts("30621:bad:project").is_err());
    assert!(project_parts(&format!("30621:{}:", owner.public_key().to_hex())).is_err());
}

#[test]
fn action_wire_types_match_frontend_without_coercing_economic_outcomes() {
    let action = json!({"type":"claim", "evidence_ids":["11".repeat(32)],
        "contributor_kind":"Human", "class":"Engineering", "summary":"work"});
    assert!(serde_json::from_value::<actions::ContributionAction>(action).is_ok());
    for outcome in ["uphold_charge", "void_charge"] {
        assert!(serde_json::from_value::<compute::ComputeAction>(json!({"type":"resolve",
            "receipt_id":"11".repeat(32), "dispute_event_id":"22".repeat(32), "outcome":outcome, "reason":"reviewed"})).is_ok());
    }
    assert!(serde_json::from_value::<compute::ComputeAction>(json!({"type":"resolve",
        "receipt_id":"11".repeat(32), "dispute_event_id":"22".repeat(32), "outcome":"adjust_price", "reason":"reviewed"})).is_err());
}
