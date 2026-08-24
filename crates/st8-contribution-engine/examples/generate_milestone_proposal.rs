//! Generates a public, signed milestone proposal for an existing Git commit.
//!
//! This is a handoff fixture, not a production identity flow. It creates
//! ephemeral Nostr project, contributor, and founder keys in memory; only their
//! signed public events are written. No secret key is serialized or printed.

use buzz_core::{kind::KIND_GIT_PATCH, Keys, Kind};
use buzz_sdk::{build_project, ProjectMemberCoord};
use nostr::{EventBuilder, Tag, Timestamp};
use st8_contribution_engine::{
    BuzzProjectContext, ContributionClaimBody, ContributionClaimContext, ContributionProposal,
    GovernancePolicy,
};
use st8_contribution_protocol::{ContributionClass, ContributorKind, DecisionStatus};
use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let git_commit = args
        .next()
        .ok_or("usage: generate_milestone_proposal <git-commit> <output.json> <created-at>")?;
    let output = PathBuf::from(
        args.next()
            .ok_or("usage: generate_milestone_proposal <git-commit> <output.json> <created-at>")?,
    );
    let created_at: i64 = args
        .next()
        .ok_or("usage: generate_milestone_proposal <git-commit> <output.json> <created-at>")?
        .parse()?;
    if args.next().is_some()
        || git_commit.len() != 40
        || !git_commit
            .chars()
            .all(|character| character.is_ascii_hexdigit())
        || created_at < 0
    {
        return Err("invalid arguments".into());
    }

    let founder_a = Keys::generate();
    let founder_b = Keys::generate();
    let contributor = Keys::generate();
    let repository = format!("30617:{}:st8wrx", contributor.public_key().to_hex());
    let member = ProjectMemberCoord::parse_full(&repository)?;
    let project_event = build_project(
        "st8wrx",
        Some("ST8WRX"),
        Some("Build together. Prove what you created."),
        &[member],
        None,
        Some("listed"),
    )?
    .custom_created_at(Timestamp::from(u64::try_from(created_at)?))
    .sign_with_keys(&founder_a)?;
    let evidence_event = EventBuilder::new(
        Kind::Custom(KIND_GIT_PATCH as u16),
        format!(
            "Established the independent ST8WRX contribution and BSV provenance foundation at Git commit {git_commit}."
        ),
    )
    .tags([
        Tag::parse(["a", repository.as_str()])?,
        Tag::parse(["commit", git_commit.as_str()])?,
        Tag::parse([
            "url",
            "https://github.com/Blocpod/ST8WRX",
        ])?,
    ])
    .custom_created_at(Timestamp::from(u64::try_from(created_at + 1)?))
    .sign_with_keys(&contributor)?;

    let project = format!("30621:{}:st8wrx", founder_a.public_key().to_hex());
    let governance_policy = GovernancePolicy {
        project,
        version: "milestone-1-founders-v1".into(),
        founders: vec![
            format!("nostr:{}", founder_a.public_key().to_hex()),
            format!("nostr:{}", founder_b.public_key().to_hex()),
        ],
        normal_threshold: 1,
        large_threshold: 2,
        large_unit_threshold: 1_000,
    };
    let governance_policy_event = governance_policy
        .event_builder()?
        .custom_created_at(Timestamp::from(u64::try_from(created_at + 2)?))
        .sign_with_keys(&founder_a)?;
    let claim_event = ContributionClaimBody {
        contributor_kind: ContributorKind::Human,
        class: ContributionClass::Engineering,
        summary: format!("Independent ST8WRX protocol foundation at Git commit {git_commit}"),
    }
    .event_builder(
        &governance_policy.project,
        std::slice::from_ref(&evidence_event),
    )?
    .custom_created_at(Timestamp::from(u64::try_from(created_at + 3)?))
    .sign_with_keys(&contributor)?;
    let project_context = BuzzProjectContext::from_event(&project_event)?;
    let claim = ContributionClaimContext::from_event(
        &project_context,
        &claim_event,
        std::slice::from_ref(&evidence_event),
    )?;
    let intent = governance_policy.decision_intent(
        &claim.record,
        DecisionStatus::Accepted,
        1_000,
        created_at + 60,
        "Two project founders accepted the grounded engineering contribution.".into(),
    )?;
    let decision_proposal_event = intent
        .proposal_event_builder_for_claim_and_policy(&claim_event, &governance_policy_event)?
        .custom_created_at(Timestamp::from(u64::try_from(created_at + 4)?))
        .sign_with_keys(&founder_a)?;
    let mut proposal = ContributionProposal {
        project_event,
        evidence_events: vec![evidence_event],
        governance_policy_event,
        claim_event,
        decision_proposal_event,
        approval_events: Vec::new(),
    };
    proposal.approval_events = vec![
        intent
            .approval_event_builder_for_proposal(&proposal.decision_proposal_event)?
            .custom_created_at(Timestamp::from(u64::try_from(created_at + 30)?))
            .sign_with_keys(&founder_a)?,
        intent
            .approval_event_builder_for_proposal(&proposal.decision_proposal_event)?
            .custom_created_at(Timestamp::from(u64::try_from(created_at + 31)?))
            .sign_with_keys(&founder_b)?,
    ];
    let bytes = serde_json::to_vec_pretty(&proposal)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, bytes)?;
    println!("proposal={}", output.display());
    println!("private_keys_written=false");
    Ok(())
}
