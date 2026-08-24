//! `buzz contributions` signed evidence, governance, and ledger commands.

use std::collections::BTreeSet;

use buzz_core::kind::{
    KIND_PROJECT, KIND_ST8_CONTRIBUTION_CLAIM, KIND_ST8_DECISION_PROPOSAL,
    KIND_ST8_GOVERNANCE_POLICY, KIND_ST8_LEDGER_ENTRY,
};
use nostr::{Event, Timestamp};
use st8_contribution_engine::{
    BuzzProjectContext, ContributionClaimBody, ContributionClaimContext, GovernanceDecisionIntent,
    GovernanceDecisionProposal, GovernancePolicy, GovernancePolicyContext,
};
use st8_contribution_protocol::{ContributionClass, ContributorKind, DecisionStatus};

use crate::client::BuzzClient;
use crate::commands::parse_write_response;
use crate::error::CliError;
use crate::{
    ContributionClassArg, ContributionContributorKind, ContributionDecisionStatus, ContributionsCmd,
};

/// Dispatches a contribution command.
pub async fn dispatch(command: ContributionsCmd, client: &BuzzClient) -> Result<(), CliError> {
    match command {
        ContributionsCmd::SetPolicy {
            project,
            version,
            authorities,
            normal_threshold,
            large_threshold,
            large_unit_threshold,
        } => {
            set_policy(
                client,
                &project,
                &version,
                &authorities,
                normal_threshold,
                large_threshold,
                large_unit_threshold,
            )
            .await
        }
        ContributionsCmd::Claim {
            project,
            evidence_ids,
            contributor_kind,
            class,
            summary,
        } => {
            claim(
                client,
                &project,
                &evidence_ids,
                contributor_kind.into(),
                class.into(),
                &summary,
            )
            .await
        }
        ContributionsCmd::Propose {
            claim,
            policy_version,
            status,
            units,
            rationale,
            decision_window_secs,
        } => {
            propose(
                client,
                &claim,
                &policy_version,
                status.into(),
                units,
                &rationale,
                decision_window_secs,
            )
            .await
        }
        ContributionsCmd::Approve { proposal } => approve(client, &proposal).await,
        ContributionsCmd::List { project, limit } => list(client, &project, limit).await,
        ContributionsCmd::Show { contribution_id } => show(client, &contribution_id).await,
    }
}

impl From<ContributionContributorKind> for ContributorKind {
    fn from(value: ContributionContributorKind) -> Self {
        match value {
            ContributionContributorKind::Human => Self::Human,
            ContributionContributorKind::Agent => Self::Agent,
            ContributionContributorKind::ComputeNode => Self::ComputeNode,
            ContributionContributorKind::Organization => Self::Organization,
        }
    }
}

impl From<ContributionClassArg> for ContributionClass {
    fn from(value: ContributionClassArg) -> Self {
        match value {
            ContributionClassArg::Intellectual => Self::Intellectual,
            ContributionClassArg::Architecture => Self::Architecture,
            ContributionClassArg::Engineering => Self::Engineering,
            ContributionClassArg::ProductDesign => Self::ProductDesign,
            ContributionClassArg::AgentWork => Self::AgentWork,
            ContributionClassArg::Compute => Self::Compute,
            ContributionClassArg::TestingSecurityReview => Self::TestingSecurityReview,
            ContributionClassArg::ResearchData => Self::ResearchData,
            ContributionClassArg::CommercialDistribution => Self::CommercialDistribution,
            ContributionClassArg::Capital => Self::Capital,
        }
    }
}

impl From<ContributionDecisionStatus> for DecisionStatus {
    fn from(value: ContributionDecisionStatus) -> Self {
        match value {
            ContributionDecisionStatus::Accepted => Self::Accepted,
            ContributionDecisionStatus::Rejected => Self::Rejected,
            ContributionDecisionStatus::Adjusted => Self::Adjusted,
        }
    }
}

async fn set_policy(
    client: &BuzzClient,
    project: &str,
    version: &str,
    authorities: &[String],
    normal_threshold: u16,
    large_threshold: u16,
    large_unit_threshold: u64,
) -> Result<(), CliError> {
    let (owner, _) = parse_project_coordinate(project)?;
    if owner != client.keys().public_key().to_hex() {
        return Err(CliError::Usage(
            "governance policy must be signed by the project owner identity".into(),
        ));
    }
    let project_event = fetch_project(client, project).await?;
    BuzzProjectContext::from_event(&project_event).map_err(engine_error)?;
    let founders = authorities
        .iter()
        .map(|authority| normalize_authority(authority))
        .collect::<Result<Vec<_>, _>>()?;
    let policy = GovernancePolicy {
        project: project.to_owned(),
        version: version.to_owned(),
        founders,
        normal_threshold,
        large_threshold,
        large_unit_threshold,
    };
    let event = client.sign_event(policy.event_builder().map_err(engine_error)?)?;
    submit(client, event).await
}

async fn claim(
    client: &BuzzClient,
    project: &str,
    evidence_ids: &[String],
    contributor_kind: ContributorKind,
    class: ContributionClass,
    summary: &str,
) -> Result<(), CliError> {
    let project_event = fetch_project(client, project).await?;
    let project_context = BuzzProjectContext::from_event(&project_event).map_err(engine_error)?;
    let evidence_events = fetch_events(client, evidence_ids).await?;
    let body = ContributionClaimBody {
        contributor_kind,
        class,
        summary: summary.to_owned(),
    };
    let event = client.sign_event(
        body.event_builder(project, &evidence_events)
            .map_err(engine_error)?,
    )?;
    ContributionClaimContext::from_event(&project_context, &event, &evidence_events)
        .map_err(engine_error)?;
    submit(client, event).await
}

async fn propose(
    client: &BuzzClient,
    claim_id: &str,
    policy_version: &str,
    status: DecisionStatus,
    units: u64,
    rationale: &str,
    decision_window_secs: u64,
) -> Result<(), CliError> {
    if decision_window_secs == 0 {
        return Err(CliError::Usage(
            "decision window must be greater than zero".into(),
        ));
    }
    let claim_event = fetch_event(client, claim_id).await?;
    if u32::from(claim_event.kind.as_u16()) != KIND_ST8_CONTRIBUTION_CLAIM {
        return Err(CliError::Usage(
            "--claim must identify a contribution claim event".into(),
        ));
    }
    let project = single_tag(&claim_event, "a")
        .ok_or_else(|| CliError::Other("claim has no unique project tag".into()))?;
    let project_event = fetch_project(client, project).await?;
    let project_context = BuzzProjectContext::from_event(&project_event).map_err(engine_error)?;
    let evidence_ids: Vec<String> = tag_values(&claim_event, "e").map(str::to_owned).collect();
    let evidence_events = fetch_events(client, &evidence_ids).await?;
    let claim_context =
        ContributionClaimContext::from_event(&project_context, &claim_event, &evidence_events)
            .map_err(engine_error)?;
    let policy_event = fetch_policy(client, project, policy_version).await?;
    let policy_context = GovernancePolicyContext::from_event(&project_context, &policy_event)
        .map_err(engine_error)?;
    let caller = format!("nostr:{}", client.keys().public_key().to_hex());
    if policy_context
        .policy
        .founders
        .iter()
        .all(|authority| authority != &caller)
    {
        return Err(CliError::Usage(
            "decision proposals must be signed by an authorized project authority".into(),
        ));
    }
    let now = Timestamp::now().as_secs();
    let decided_at_u64 = now
        .checked_add(decision_window_secs)
        .ok_or_else(|| CliError::Usage("decision window overflows timestamp".into()))?;
    let decided_at = i64::try_from(decided_at_u64)
        .map_err(|_| CliError::Usage("decision timestamp exceeds protocol range".into()))?;
    let intent = policy_context
        .policy
        .decision_intent(
            &claim_context.record,
            status,
            units,
            decided_at,
            rationale.to_owned(),
        )
        .map_err(engine_error)?;
    let event = client.sign_event(
        intent
            .proposal_event_builder_for_claim_and_policy(&claim_event, &policy_event)
            .map_err(engine_error)?,
    )?;
    GovernanceDecisionProposal::from_event(&claim_context.record, &policy_context, &event)
        .map_err(engine_error)?;
    submit(client, event).await
}

async fn approve(client: &BuzzClient, proposal_id: &str) -> Result<(), CliError> {
    let proposal_event = fetch_event(client, proposal_id).await?;
    if u32::from(proposal_event.kind.as_u16()) != KIND_ST8_DECISION_PROPOSAL {
        return Err(CliError::Usage(
            "--proposal must identify a governance decision proposal".into(),
        ));
    }
    let intent: GovernanceDecisionIntent = serde_json::from_str(&proposal_event.content)
        .map_err(|error| CliError::Other(format!("invalid proposal body: {error}")))?;
    let event = client.sign_event(
        intent
            .approval_event_builder_for_proposal(&proposal_event)
            .map_err(engine_error)?,
    )?;
    submit(client, event).await
}

async fn list(client: &BuzzClient, project: &str, limit: u32) -> Result<(), CliError> {
    parse_project_coordinate(project)?;
    if limit == 0 || limit > 1_000 {
        return Err(CliError::Usage("--limit must be between 1 and 1000".into()));
    }
    let filter = serde_json::json!({
        "kinds": [KIND_ST8_LEDGER_ENTRY],
        "#a": [project],
        "limit": limit,
    });
    println!("{}", client.query(&filter).await?);
    Ok(())
}

async fn show(client: &BuzzClient, contribution_id: &str) -> Result<(), CliError> {
    crate::validate::validate_hex64(contribution_id)?;
    let filter = serde_json::json!({
        "kinds": [KIND_ST8_LEDGER_ENTRY],
        "#d": [contribution_id.to_ascii_lowercase()],
        "limit": 1,
    });
    let raw = client.query(&filter).await?;
    if parse_events(&raw)?.is_empty() {
        return Err(CliError::NotFound(format!(
            "contribution {contribution_id} is not in the ledger"
        )));
    }
    println!("{raw}");
    Ok(())
}

async fn submit(client: &BuzzClient, event: Event) -> Result<(), CliError> {
    let raw = client.submit_event(event).await?;
    println!(
        "{}",
        parse_write_response(&raw, "contribution event was rejected by the relay")?
    );
    Ok(())
}

async fn fetch_project(client: &BuzzClient, project: &str) -> Result<Event, CliError> {
    let (owner, slug) = parse_project_coordinate(project)?;
    let filter = serde_json::json!({
        "kinds": [KIND_PROJECT],
        "authors": [owner],
        "#d": [slug],
        "limit": 1,
    });
    let raw = client.query(&filter).await?;
    parse_events(&raw)?
        .into_iter()
        .next()
        .ok_or_else(|| CliError::NotFound(format!("project {project} was not found")))
}

async fn fetch_policy(
    client: &BuzzClient,
    project: &str,
    version: &str,
) -> Result<Event, CliError> {
    let (owner, _) = parse_project_coordinate(project)?;
    let filter = serde_json::json!({
        "kinds": [KIND_ST8_GOVERNANCE_POLICY],
        "authors": [owner],
        "#d": [version],
        "limit": 1,
    });
    let raw = client.query(&filter).await?;
    parse_events(&raw)?
        .into_iter()
        .next()
        .ok_or_else(|| CliError::NotFound(format!("governance policy {version} was not found")))
}

async fn fetch_event(client: &BuzzClient, event_id: &str) -> Result<Event, CliError> {
    fetch_events(client, &[event_id.to_owned()])
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| CliError::NotFound(format!("event {event_id} was not found")))
}

async fn fetch_events(client: &BuzzClient, event_ids: &[String]) -> Result<Vec<Event>, CliError> {
    if event_ids.is_empty() || event_ids.len() > 256 {
        return Err(CliError::Usage(
            "between 1 and 256 event IDs are required".into(),
        ));
    }
    let mut expected = BTreeSet::new();
    for event_id in event_ids {
        crate::validate::validate_hex64(event_id)?;
        if !expected.insert(event_id.to_ascii_lowercase()) {
            return Err(CliError::Usage(format!("duplicate event ID: {event_id}")));
        }
    }
    let ids: Vec<&str> = expected.iter().map(String::as_str).collect();
    let kinds = buzz_core::kind::ALL_KINDS;
    let filter = serde_json::json!({
        "ids": ids,
        "kinds": kinds,
        "limit": event_ids.len(),
    });
    let raw = client.query(&filter).await?;
    let mut events = parse_events(&raw)?;
    let actual: BTreeSet<String> = events.iter().map(|event| event.id.to_hex()).collect();
    if actual != expected {
        let missing: Vec<&str> = expected.difference(&actual).map(String::as_str).collect();
        return Err(CliError::NotFound(format!(
            "signed evidence events were not found: {}",
            missing.join(", ")
        )));
    }
    events.sort_by_key(|event| event.id.to_hex());
    Ok(events)
}

fn parse_events(raw: &str) -> Result<Vec<Event>, CliError> {
    serde_json::from_str(raw)
        .map_err(|error| CliError::Other(format!("relay returned invalid event JSON: {error}")))
}

fn parse_project_coordinate(project: &str) -> Result<(String, String), CliError> {
    let mut parts = project.splitn(3, ':');
    if parts.next() != Some("30621") {
        return Err(CliError::Usage(
            "project must be a full 30621:<owner-pubkey>:<slug> coordinate".into(),
        ));
    }
    let owner = parts
        .next()
        .ok_or_else(|| CliError::Usage("project coordinate is missing its owner".into()))?;
    crate::validate::validate_hex64(owner)?;
    let slug = parts
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CliError::Usage("project coordinate is missing its slug".into()))?;
    Ok((owner.to_ascii_lowercase(), slug.to_owned()))
}

fn normalize_authority(authority: &str) -> Result<String, CliError> {
    let public_key = authority.strip_prefix("nostr:").unwrap_or(authority);
    crate::validate::validate_hex64(public_key)?;
    Ok(format!("nostr:{}", public_key.to_ascii_lowercase()))
}

fn single_tag<'a>(event: &'a Event, name: &'a str) -> Option<&'a str> {
    let mut values = tag_values(event, name);
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn tag_values<'a>(event: &'a Event, name: &'a str) -> impl Iterator<Item = &'a str> {
    event.tags.iter().filter_map(move |tag| {
        let values = tag.as_slice();
        (values.first().map(String::as_str) == Some(name))
            .then(|| values.get(1).map(String::as_str))
            .flatten()
    })
}

fn engine_error(error: impl std::fmt::Display) -> CliError {
    CliError::Usage(error.to_string())
}
