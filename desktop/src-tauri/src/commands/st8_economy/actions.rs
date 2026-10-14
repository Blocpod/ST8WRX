use super::*;
use serde::Deserialize;
use st8_contribution_engine::{
    ContributionClaimBody, ContributionClaimContext, GovernanceDecisionProposal, GovernancePolicy,
    GovernancePolicyContext,
};
use st8_contribution_protocol::{ContributionClass, ContributorKind, DecisionStatus};

/// Signed contribution operations exposed by the project economy interface.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContributionAction {
    /// Publish a project-owner governance policy.
    Policy {
        version: String,
        founders: Vec<String>,
        normal_threshold: u16,
        large_threshold: u16,
        large_unit_threshold: u64,
    },
    /// Claim credit backed by public project evidence.
    Claim {
        evidence_ids: Vec<String>,
        contributor_kind: ContributorKind,
        class: ContributionClass,
        summary: String,
    },
    /// Propose a decision under an exact signed policy.
    Propose {
        claim_id: String,
        policy_event_id: String,
        status: DecisionStatus,
        units: u64,
        rationale: String,
        decision_window_secs: u32,
    },
    /// Approve the fully verified proposal material.
    Approve { proposal_id: String },
}

async fn claim_material(
    scope: &EconomyScope<'_>,
    project: &BuzzProjectContext,
    claim_id: &str,
) -> Result<(Event, ContributionClaimContext), String> {
    let claim = scope.event(claim_id, KIND_ST8_CONTRIBUTION_CLAIM).await?;
    let ids: Vec<_> = tag_values(&claim, "e").map(str::to_owned).collect();
    let evidence = scope.exact(&ids, &public_evidence_kinds()).await?;
    if evidence
        .iter()
        .any(|event| !public_evidence(project, event))
    {
        return Err(
            "Contribution evidence must be public and belong to this project or its repositories"
                .into(),
        );
    }
    let context =
        ContributionClaimContext::from_event(project, &claim, &evidence).map_err(error)?;
    Ok((claim, context))
}

pub(super) fn approved_builder(
    project: &BuzzProjectContext,
    claim: &ContributionClaimContext,
    policy_event: &Event,
    proposal_event: &Event,
    keys: &Keys,
) -> Result<EventBuilder, String> {
    let policy = GovernancePolicyContext::from_event(project, policy_event).map_err(error)?;
    let proposal = GovernanceDecisionProposal::from_event(&claim.record, &policy, proposal_event)
        .map_err(error)?;
    let builder = proposal
        .intent
        .approval_event_builder_for_proposal(proposal_event)
        .map_err(error)?
        .custom_created_at(nostr::Timestamp::now());
    let approval = builder.clone().sign_with_keys(keys).map_err(error)?;
    // An individual valid approval may not yet satisfy the threshold; all
    // signer, digest, policy, evidence and decision-window checks still run.
    match policy
        .policy
        .decide(&claim.record, &proposal.intent, &[approval])
    {
        Ok(_) | Err(st8_contribution_engine::EngineError::InsufficientApprovals { .. }) => {
            Ok(builder)
        }
        Err(error) => Err(error.to_string()),
    }
}

/// Validate, sign and submit a contribution operation in one captured community.
#[tauri::command]
pub async fn st8_contribution_action(
    project: String,
    action: ContributionAction,
    expected_relay_url: Option<String>,
    expected_signer_pubkey: Option<String>,
    state: State<'_, AppState>,
) -> Result<SubmitEventResponse, String> {
    let scope = EconomyScope::capture(
        &state,
        expected_relay_url.as_deref(),
        expected_signer_pubkey.as_deref(),
    )?;
    let context = scope.project(&project).await?;
    let builder = match action {
        ContributionAction::Policy {
            version,
            founders,
            normal_threshold,
            large_threshold,
            large_unit_threshold,
        } => {
            let founders = founders
                .iter()
                .map(|key| {
                    nostr::PublicKey::parse(key.trim().strip_prefix("nostr:").unwrap_or(key.trim()))
                        .map(|key| format!("nostr:{}", key.to_hex()))
                        .map_err(error)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let policy = GovernancePolicy {
                project,
                version,
                founders,
                normal_threshold,
                large_threshold,
                large_unit_threshold,
            };
            let builder = policy
                .event_builder()
                .map_err(error)?
                .custom_created_at(nostr::Timestamp::now());
            let signed = builder.clone().sign_with_keys(&scope.keys).map_err(error)?;
            GovernancePolicyContext::from_event(&context, &signed).map_err(error)?;
            builder
        }
        ContributionAction::Claim {
            evidence_ids,
            contributor_kind,
            class,
            summary,
        } => {
            let evidence = scope.exact(&evidence_ids, &public_evidence_kinds()).await?;
            if evidence
                .iter()
                .any(|event| !public_evidence(&context, event))
            {
                return Err("Contribution evidence must be public and belong to this project or its repositories".into());
            }
            let body = ContributionClaimBody {
                contributor_kind,
                class,
                summary,
            };
            let builder = body
                .event_builder(&project, &evidence)
                .map_err(error)?
                .custom_created_at(nostr::Timestamp::now());
            let signed = builder.clone().sign_with_keys(&scope.keys).map_err(error)?;
            ContributionClaimContext::from_event(&context, &signed, &evidence).map_err(error)?;
            builder
        }
        ContributionAction::Propose {
            claim_id,
            policy_event_id,
            status,
            units,
            rationale,
            decision_window_secs,
        } => {
            if decision_window_secs == 0 {
                return Err("The decision window must be greater than zero".into());
            }
            let (claim_event, claim) = claim_material(&scope, &context, &claim_id).await?;
            let policy_event = scope
                .event(&policy_event_id, KIND_ST8_GOVERNANCE_POLICY)
                .await?;
            let policy =
                GovernancePolicyContext::from_event(&context, &policy_event).map_err(error)?;
            let now = nostr::Timestamp::now();
            let decided_at = now
                .as_secs()
                .checked_add(u64::from(decision_window_secs))
                .and_then(|timestamp| i64::try_from(timestamp).ok())
                .ok_or("Decision window is too large")?;
            let intent = policy
                .policy
                .decision_intent(&claim.record, status, units, decided_at, rationale)
                .map_err(error)?;
            let builder = intent
                .proposal_event_builder_for_claim_and_policy(&claim_event, &policy_event)
                .map_err(error)?
                .custom_created_at(now);
            let signed = builder.clone().sign_with_keys(&scope.keys).map_err(error)?;
            GovernanceDecisionProposal::from_event(&claim.record, &policy, &signed)
                .map_err(error)?;
            builder
        }
        ContributionAction::Approve { proposal_id } => {
            let proposal = scope
                .event(&proposal_id, KIND_ST8_DECISION_PROPOSAL)
                .await?;
            let (claim_event, claim) =
                claim_material(&scope, &context, unique_tag(&proposal, "e")?).await?;
            let policy = scope
                .event(
                    unique_tag(&proposal, "st8-policy-event")?,
                    KIND_ST8_GOVERNANCE_POLICY,
                )
                .await?;
            if unique_tag(&proposal, "e")? != claim_event.id.to_hex() {
                return Err("Proposal claim does not match".into());
            }
            approved_builder(&context, &claim, &policy, &proposal, &scope.keys)?
        }
    };
    scope.submit(builder).await
}
