use super::*;
use st8_contribution_engine::{
    ContributionClaimContext, GovernanceDecisionProposal, GovernancePolicyContext,
};

/// Only complete, authorized proposal material can enable an approval action.
fn proposal_is_valid(
    project: &BuzzProjectContext,
    proposal: &Event,
    claims: &[Event],
    policies: &[Event],
    evidence: &[Event],
) -> bool {
    let Ok(claim_id) = unique_tag(proposal, "e") else {
        return false;
    };
    let Ok(policy_id) = unique_tag(proposal, "st8-policy-event") else {
        return false;
    };
    let Some(claim) = claims.iter().find(|event| event.id.to_hex() == claim_id) else {
        return false;
    };
    let Some(policy) = policies.iter().find(|event| event.id.to_hex() == policy_id) else {
        return false;
    };
    let signed_evidence: Vec<_> = tag_values(claim, "e")
        .filter_map(|id| {
            evidence
                .iter()
                .find(|event| event.id.to_hex() == id)
                .cloned()
        })
        .collect();
    if signed_evidence
        .iter()
        .any(|event| !public_evidence(project, event))
    {
        return false;
    }
    let Ok(claim) = ContributionClaimContext::from_event(project, claim, &signed_evidence) else {
        return false;
    };
    let Ok(policy) = GovernancePolicyContext::from_event(project, policy) else {
        return false;
    };
    GovernanceDecisionProposal::from_event(&claim.record, &policy, proposal).is_ok_and(|verified| {
        u64::try_from(verified.intent.decided_at)
            .is_ok_and(|deadline| deadline >= nostr::Timestamp::now().as_secs())
    })
}

async fn dependencies(
    scope: &EconomyScope<'_>,
    ids: BTreeSet<String>,
    kinds: &[u32],
) -> Result<Vec<Event>, String> {
    let ids: Vec<_> = ids.into_iter().filter(|id| digest(id).is_ok()).collect();
    let mut events = Vec::new();
    for batch in ids.chunks(100) {
        let fetched = scope
            .query(json!({"ids":batch,"kinds":kinds,"limit":batch.len()}))
            .await?;
        if fetched.iter().any(|event| {
            !batch.contains(&event.id.to_hex()) || !kinds.contains(&u32::from(event.kind.as_u16()))
        }) {
            return Err("Relay returned unrelated proposal material".into());
        }
        events.extend(fetched);
    }
    Ok(events)
}

pub(super) async fn actionable_proposals(
    scope: &EconomyScope<'_>,
    project: &BuzzProjectContext,
    proposals: &[Event],
) -> Result<(Vec<String>, bool), String> {
    let claims = dependencies(
        scope,
        proposals
            .iter()
            .filter_map(|event| unique_tag(event, "e").ok().map(str::to_owned))
            .collect(),
        &[KIND_ST8_CONTRIBUTION_CLAIM],
    )
    .await?;
    let policies = dependencies(
        scope,
        proposals
            .iter()
            .filter_map(|event| {
                unique_tag(event, "st8-policy-event")
                    .ok()
                    .map(str::to_owned)
            })
            .collect(),
        &[KIND_ST8_GOVERNANCE_POLICY],
    )
    .await?;
    // Bound dependency work even when every proposal references a distinct large claim.
    // Incomplete material remains visible as source history, never actionable.
    let ids: BTreeSet<_> = claims
        .iter()
        .flat_map(|claim| tag_values(claim, "e").map(str::to_owned))
        .collect();
    let truncated = ids.len() > 1000;
    let evidence = dependencies(
        scope,
        ids.into_iter().take(1000).collect(),
        &public_evidence_kinds(),
    )
    .await?;
    Ok((
        proposals
            .iter()
            .filter(|proposal| proposal_is_valid(project, proposal, &claims, &policies, &evidence))
            .map(|event| event.id.to_hex())
            .collect(),
        truncated,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_stranger_proposal_cannot_be_actionable() {
        let owner = Keys::generate();
        let stranger = Keys::generate();
        let project_event =
            buzz_sdk_pkg::build_project("test", None, None, &[], None, Some("listed"))
                .unwrap()
                .sign_with_keys(&owner)
                .unwrap();
        let project = BuzzProjectContext::from_event(&project_event).unwrap();
        let claim = st8_contribution_engine::ContributionClaimBody {
            contributor_kind: st8_contribution_protocol::ContributorKind::Human,
            class: st8_contribution_protocol::ContributionClass::Engineering,
            summary: "Work".into(),
        }
        .event_builder(&project.project, std::slice::from_ref(&project_event))
        .unwrap()
        .sign_with_keys(&owner)
        .unwrap();
        let claim_context = ContributionClaimContext::from_event(
            &project,
            &claim,
            std::slice::from_ref(&project_event),
        )
        .unwrap();
        let policy = st8_contribution_engine::GovernancePolicy {
            project: project.project.clone(),
            version: "v1".into(),
            founders: vec![format!("nostr:{}", owner.public_key().to_hex())],
            normal_threshold: 1,
            large_threshold: 1,
            large_unit_threshold: 100,
        };
        let policy_event = policy
            .event_builder()
            .unwrap()
            .sign_with_keys(&owner)
            .unwrap();
        let intent = policy
            .decision_intent(
                &claim_context.record,
                st8_contribution_protocol::DecisionStatus::Accepted,
                1,
                i64::try_from(nostr::Timestamp::now().as_secs()).unwrap() + 600,
                "Reviewed".into(),
            )
            .unwrap();
        let builder = intent
            .proposal_event_builder_for_claim_and_policy(&claim, &policy_event)
            .unwrap();
        let good = builder.clone().sign_with_keys(&owner).unwrap();
        let bad = builder.sign_with_keys(&stranger).unwrap();
        let valid = |proposal| {
            proposal_is_valid(
                &project,
                proposal,
                std::slice::from_ref(&claim),
                std::slice::from_ref(&policy_event),
                std::slice::from_ref(&project_event),
            )
        };
        assert!(valid(&good));
        assert!(!valid(&bad));
        assert!(!proposal_is_valid(
            &project,
            &good,
            &[claim],
            &[policy_event],
            &[]
        ));
    }
}
