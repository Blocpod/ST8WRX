//! ST8WRX signed contribution event validation and durable ledger projection.

use std::sync::Arc;

use anyhow::{anyhow, Context};
use buzz_core::kind::{
    KIND_PROJECT, KIND_ST8_CONTRIBUTION_CLAIM, KIND_ST8_DECISION_PROPOSAL,
    KIND_ST8_GOVERNANCE_APPROVAL, KIND_ST8_GOVERNANCE_POLICY, KIND_ST8_LEDGER_ENTRY,
};
use buzz_core::tenant::TenantContext;
use buzz_db::contribution::{
    LedgerInsertOutcome, NewContributionEvidence, NewContributionLedgerEntry,
    NewGovernanceApproval, NewProjectSnapshot,
};
use buzz_db::EventQuery;
use chrono::{DateTime, Utc};
use nostr::{Event, EventBuilder, Kind, Tag};
use st8_contribution_engine::{
    BuzzProjectContext, ContributionClaimBody, ContributionClaimContext, ContributionSnapshot,
    EngineError, GovernanceDecisionIntent, GovernanceDecisionProposal, GovernancePolicy,
    GovernancePolicyContext, PreparedContributionAnchor, ProjectLedgerSnapshot,
    VerifiedContributionMaterial,
};
use st8_contribution_protocol::DecisionStatus;

use crate::state::AppState;

/// Validates the stable, transport-visible envelope before event storage.
pub fn validate_envelope(event: &Event) -> Result<(), String> {
    let kind = u32::from(event.kind.as_u16());
    match kind {
        KIND_ST8_GOVERNANCE_POLICY => {
            let policy: GovernancePolicy =
                serde_json::from_str(&event.content).map_err(|error| error.to_string())?;
            policy.validate().map_err(|error| error.to_string())?;
            if single_tag(event, "a") != Some(policy.project.as_str())
                || single_tag(event, "d") != Some(policy.version.as_str())
                || project_owner(&policy.project).as_deref() != Some(event.pubkey.to_hex().as_str())
            {
                return Err("policy must be signed by and scoped to the project owner".into());
            }
        }
        KIND_ST8_CONTRIBUTION_CLAIM => {
            let body: ContributionClaimBody =
                serde_json::from_str(&event.content).map_err(|error| error.to_string())?;
            if body.summary.trim().is_empty()
                || single_tag(event, "a").is_none()
                || !unique_hex_tags(event, "e", 1, 256)
            {
                return Err("claim requires one project and unique evidence event IDs".into());
            }
        }
        KIND_ST8_DECISION_PROPOSAL => {
            let intent: GovernanceDecisionIntent =
                serde_json::from_str(&event.content).map_err(|error| error.to_string())?;
            let digest = hex::encode(intent.digest().map_err(|error| error.to_string())?);
            if single_tag(event, "a") != Some(intent.project.as_str())
                || single_tag(event, "st8-policy") != Some(intent.policy_version.as_str())
                || single_tag(event, "st8-policy-event").is_none_or(|value| !is_hex_32(value))
                || single_tag(event, "st8-contribution")
                    != Some(hex::encode(intent.contribution_id).as_str())
                || single_tag(event, "st8-decision") != Some(digest.as_str())
                || !unique_hex_tags(event, "e", 1, 1)
            {
                return Err("decision proposal does not bind one exact contribution claim".into());
            }
        }
        KIND_ST8_GOVERNANCE_APPROVAL => {
            if !is_hex_32(&event.content)
                || single_tag(event, "a").is_none()
                || single_tag(event, "st8-policy").is_none()
                || single_tag(event, "st8-contribution").is_none()
                || single_tag(event, "st8-decision") != Some(event.content.as_str())
                || !unique_hex_tags(event, "e", 1, 1)
            {
                return Err("approval does not bind one exact decision proposal".into());
            }
        }
        _ => return Err("not an ST8 contribution event".into()),
    }
    Ok(())
}

/// Projects a newly stored approval when its signed threshold is satisfied.
/// An insufficient threshold is a normal pending state, not an ingest failure.
pub async fn handle_approval(
    tenant: &TenantContext,
    approval: &Event,
    state: &Arc<AppState>,
) -> anyhow::Result<()> {
    let proposal = linked_event(tenant, approval, state)
        .await?
        .context("decision proposal event is not stored")?;
    if u32::from(proposal.kind.as_u16()) != KIND_ST8_DECISION_PROPOSAL {
        return Err(anyhow!(
            "approval e-tag does not reference a decision proposal"
        ));
    }
    let claim = linked_event(tenant, &proposal, state)
        .await?
        .context("contribution claim event is not stored")?;
    if u32::from(claim.kind.as_u16()) != KIND_ST8_CONTRIBUTION_CLAIM {
        return Err(anyhow!(
            "proposal e-tag does not reference a contribution claim"
        ));
    }

    let project_id = single_tag(&proposal, "a")
        .context("proposal missing project")?
        .to_owned();
    let (project_owner, project_slug) = parse_project_coordinate(&project_id)?;
    let project_event =
        latest_addressable(tenant, state, KIND_PROJECT, &project_owner, &project_slug)
            .await?
            .context("project event is not stored")?;
    let project_context = BuzzProjectContext::from_event(&project_event)?;

    let policy_event_id = single_tag(&proposal, "st8-policy-event")
        .context("proposal missing exact governance policy event")?;
    let policy_event = event_by_hex_id(tenant, state, policy_event_id)
        .await?
        .context("exact governance policy event is not stored")?;
    let policy_context = GovernancePolicyContext::from_event(&project_context, &policy_event)?;

    let evidence_ids = hex_tag_bytes(&claim, "e")?;
    let evidence_refs: Vec<&[u8]> = evidence_ids.iter().map(Vec::as_slice).collect();
    let evidence_events: Vec<Event> = state
        .db
        .get_events_by_ids(tenant.community(), &evidence_refs)
        .await?
        .into_iter()
        .map(|stored| stored.event)
        .collect();
    let claim_context =
        ContributionClaimContext::from_event(&project_context, &claim, &evidence_events)?;
    let decision_proposal =
        GovernanceDecisionProposal::from_event(&claim_context.record, &policy_context, &proposal)?;

    let approval_events = approvals_for_proposal(tenant, state, &proposal).await?;
    let decision = match policy_context.policy.decide(
        &claim_context.record,
        &decision_proposal.intent,
        &approval_events,
    ) {
        Ok(decision) => decision,
        Err(EngineError::InsufficientApprovals { .. }) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    // The accepted-set read and next snapshot insert are one logical critical
    // section. A Postgres advisory guard serializes it across relay processes.
    let ledger_lock = state
        .db
        .acquire_contribution_project_lock(tenant.community(), &project_id)
        .await?;
    let contribution_id = claim_context.record.id()?;
    if state
        .db
        .contribution_exists(tenant.community(), &contribution_id)
        .await?
    {
        ledger_lock.release_now().await?;
        return Ok(());
    }

    let status = match decision.status {
        DecisionStatus::Accepted => "accepted",
        DecisionStatus::Adjusted => "adjusted",
        DecisionStatus::Rejected => "rejected",
    };
    let units = i64::try_from(decision.contribution_units)
        .map_err(|_| anyhow!("contribution units exceed database range"))?;
    let accepted = decision.status != DecisionStatus::Rejected;
    let contribution_snapshot = accepted
        .then(|| ContributionSnapshot::new(claim_context.record.clone(), decision.clone()))
        .transpose()?;

    let (project_snapshot, prepared) = if let Some(snapshot) = &contribution_snapshot {
        let mut snapshots = state
            .db
            .list_project_accepted_snapshots(tenant.community(), &project_id)
            .await?
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<ContributionSnapshot>, _>>()?;
        snapshots.push(snapshot.clone());
        let project_snapshot = ProjectLedgerSnapshot::new(snapshots)?;
        let prepared = PreparedContributionAnchor::new_with_project_snapshot(
            VerifiedContributionMaterial {
                project_context: project_context.clone(),
                evidence_events: evidence_events.clone(),
                claim: claim_context.clone(),
                decision_proposal: decision_proposal.clone(),
                approval_events: approval_events.clone(),
                governance_policy: policy_context.clone(),
            },
            snapshot.clone(),
            project_snapshot.clone(),
            st8_bsv_provenance::BsvNetwork::Testnet,
        )?;
        (Some(project_snapshot), Some(prepared))
    } else {
        (None, None)
    };

    let db_evidence = evidence_events
        .iter()
        .map(
            |event| -> Result<NewContributionEvidence, serde_json::Error> {
                Ok(NewContributionEvidence {
                    event_id: event.id.as_bytes().to_vec(),
                    kind: i32::from(event.kind.as_u16()),
                    event: serde_json::to_value(event)?,
                })
            },
        )
        .collect::<Result<Vec<_>, _>>()?;
    let db_approvals = approval_events
        .iter()
        .map(|event| {
            let approved_at_secs = i64::try_from(event.created_at.as_secs())
                .context("approval timestamp is out of range")?;
            let approved_at = DateTime::<Utc>::from_timestamp(approved_at_secs, 0)
                .context("approval timestamp is out of range")?;
            Ok(NewGovernanceApproval {
                approver_pubkey: event.pubkey.to_bytes().to_vec(),
                event_id: event.id.as_bytes().to_vec(),
                event: serde_json::to_value(event)?,
                approved_at,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let new_project_snapshot = match (&project_snapshot, &prepared) {
        (Some(snapshot), Some(prepared)) => {
            let batch = snapshot.merkle_batch()?;
            Some(NewProjectSnapshot {
                snapshot_id: snapshot.id()?.to_vec(),
                merkle_root: batch.root().to_vec(),
                leaf_count: i32::try_from(batch.len())?,
                snapshot: serde_json::to_value(snapshot)?,
                prepared_anchor: serde_json::to_value(prepared)?,
            })
        }
        (None, None) => None,
        _ => return Err(anyhow!("partial project snapshot state")),
    };
    let mut evidence_event_ids: Vec<String> = evidence_events
        .iter()
        .map(|event| event.id.to_hex())
        .collect();
    evidence_event_ids.sort();
    let mut approval_event_ids: Vec<String> = approval_events
        .iter()
        .map(|event| event.id.to_hex())
        .collect();
    approval_event_ids.sort();
    let contribution_snapshot_id = contribution_snapshot
        .as_ref()
        .map(ContributionSnapshot::id)
        .transpose()?;
    let ledger_projection = serde_json::json!({
        "contribution_id": hex::encode(contribution_id),
        "project_id": project_id,
        "contributor": claim.pubkey.to_hex(),
        "status": status,
        "contribution_units": units,
        "record": claim_context.record,
        "decision": decision,
        "project_event_id": project_event.id.to_hex(),
        "policy_event_id": policy_event.id.to_hex(),
        "claim_event_id": claim.id.to_hex(),
        "decision_proposal_event_id": proposal.id.to_hex(),
        "evidence_event_ids": evidence_event_ids,
        "approval_event_ids": approval_event_ids,
        "contribution_snapshot_id": contribution_snapshot_id.map(hex::encode),
    });
    let entry = NewContributionLedgerEntry {
        contribution_id: contribution_id.to_vec(),
        project_id: project_id.clone(),
        contributor_pubkey: claim.pubkey.to_bytes().to_vec(),
        status: status.into(),
        contribution_units: units,
        project_event: serde_json::to_value(&project_event)?,
        policy_event: serde_json::to_value(&policy_event)?,
        claim_event: serde_json::to_value(&claim)?,
        decision_proposal_event: serde_json::to_value(&proposal)?,
        record: serde_json::to_value(&claim_context.record)?,
        decision: serde_json::to_value(&decision)?,
        ledger_projection,
        contribution_snapshot: contribution_snapshot
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?,
        contribution_snapshot_id: contribution_snapshot_id.map(|digest| digest.to_vec()),
        evidence: db_evidence,
        approvals: db_approvals,
        project_snapshot: new_project_snapshot,
    };
    let outcome = state
        .db
        .insert_contribution_ledger_entry(tenant.community(), &entry)
        .await?;
    if outcome == LedgerInsertOutcome::Inserted {
        emit_ledger_projection(
            tenant,
            state,
            LedgerProjectionInput {
                contribution_id: &entry.contribution_id,
                project_id: &entry.project_id,
                contributor_pubkey: &entry.contributor_pubkey,
                status: &entry.status,
                anchor_state: if entry.project_snapshot.is_some() {
                    "queued"
                } else {
                    "not_applicable"
                },
                ledger_projection: &entry.ledger_projection,
                project_snapshot_id: project_snapshot
                    .as_ref()
                    .map(ProjectLedgerSnapshot::id)
                    .transpose()?,
            },
        )
        .await?;
    }
    ledger_lock.release_now().await?;
    Ok(())
}

/// Replays stored governance approvals whose immutable contribution decision
/// has not yet been projected. This repairs a crash or transient database
/// failure after the approval event itself was durably accepted.
pub async fn reconcile_stored_approvals(state: &Arc<AppState>) -> anyhow::Result<usize> {
    const PAGE_SIZE: i64 = 1_000;
    let communities = state.db.usage_community_hosts().await?;
    let mut projected = 0usize;
    for community in communities {
        let community_id = buzz_core::CommunityId::from_uuid(community.id);
        let tenant = TenantContext::resolved(community_id, community.host);
        let mut offset = 0i64;
        loop {
            let mut query = EventQuery::for_community(community_id);
            query.kinds = Some(vec![KIND_ST8_GOVERNANCE_APPROVAL as i32]);
            query.global_only = true;
            query.limit = Some(PAGE_SIZE);
            query.offset = Some(offset);
            let approvals = state.db.query_events(&query).await?;
            let page_len = approvals.len();
            if page_len == 0 {
                break;
            }
            for stored in approvals {
                let Some(contribution_hex) = single_tag(&stored.event, "st8-contribution") else {
                    tracing::warn!(event_id = %stored.event.id, "stored ST8 approval lacks contribution binding");
                    continue;
                };
                let contribution_id = match hex::decode(contribution_hex) {
                    Ok(id) if id.len() == 32 => id,
                    _ => {
                        tracing::warn!(event_id = %stored.event.id, "stored ST8 approval has invalid contribution binding");
                        continue;
                    }
                };
                if state
                    .db
                    .contribution_exists(community_id, &contribution_id)
                    .await?
                {
                    continue;
                }
                match handle_approval(&tenant, &stored.event, state).await {
                    Ok(()) => {
                        if state
                            .db
                            .contribution_exists(community_id, &contribution_id)
                            .await?
                        {
                            projected += 1;
                        }
                    }
                    Err(error) => tracing::warn!(
                        event_id = %stored.event.id,
                        %community_id,
                        %error,
                        "stored ST8 approval reconciliation failed"
                    ),
                }
            }
            if page_len < usize::try_from(PAGE_SIZE)? {
                break;
            }
            offset = offset
                .checked_add(PAGE_SIZE)
                .context("ST8 approval reconciliation offset overflow")?;
        }
    }
    Ok(projected)
}

/// Repairs the relay-signed query projection after an anchor worker changes
/// durable ledger state (for example `queued` to `confirmed`) or after an
/// earlier projection publication failed.
pub async fn reconcile_ledger_events(state: &Arc<AppState>) -> anyhow::Result<usize> {
    const PAGE_SIZE: i64 = 500;
    let communities = state.db.usage_community_hosts().await?;
    let mut repaired = 0usize;
    for community in communities {
        let community_id = buzz_core::CommunityId::from_uuid(community.id);
        let tenant = TenantContext::resolved(community_id, community.host);
        let mut offset = 0i64;
        loop {
            let records = state
                .db
                .list_community_contributions(community_id, PAGE_SIZE, offset)
                .await?;
            let page_len = records.len();
            if page_len == 0 {
                break;
            }
            for record in records {
                let snapshot_id = record
                    .project_snapshot_id
                    .as_deref()
                    .map(|value| {
                        <[u8; 32]>::try_from(value)
                            .map_err(|_| anyhow!("invalid stored project snapshot ID"))
                    })
                    .transpose()?;
                let expected = projection_content(
                    &record.ledger_projection,
                    &record.anchor_state,
                    snapshot_id,
                );
                let current =
                    latest_ledger_projection(&tenant, state, &hex::encode(&record.contribution_id))
                        .await?;
                let is_current = current
                    .as_ref()
                    .and_then(|event| {
                        serde_json::from_str::<serde_json::Value>(&event.content).ok()
                    })
                    .is_some_and(|content| content == expected);
                if !is_current {
                    emit_ledger_projection(
                        &tenant,
                        state,
                        LedgerProjectionInput {
                            contribution_id: &record.contribution_id,
                            project_id: &record.project_id,
                            contributor_pubkey: &record.contributor_pubkey,
                            status: &record.status,
                            anchor_state: &record.anchor_state,
                            ledger_projection: &record.ledger_projection,
                            project_snapshot_id: snapshot_id,
                        },
                    )
                    .await?;
                    repaired += 1;
                }
            }
            if page_len < usize::try_from(PAGE_SIZE)? {
                break;
            }
            offset = offset
                .checked_add(PAGE_SIZE)
                .context("ST8 ledger projection offset overflow")?;
        }
    }
    Ok(repaired)
}

struct LedgerProjectionInput<'a> {
    contribution_id: &'a [u8],
    project_id: &'a str,
    contributor_pubkey: &'a [u8],
    status: &'a str,
    anchor_state: &'a str,
    ledger_projection: &'a serde_json::Value,
    project_snapshot_id: Option<[u8; 32]>,
}

async fn emit_ledger_projection(
    tenant: &TenantContext,
    state: &Arc<AppState>,
    input: LedgerProjectionInput<'_>,
) -> anyhow::Result<()> {
    let contribution_hex = hex::encode(input.contribution_id);
    let contributor_hex = hex::encode(input.contributor_pubkey);
    let content = projection_content(
        input.ledger_projection,
        input.anchor_state,
        input.project_snapshot_id,
    )
    .to_string();
    let tags = [
        Tag::parse(["d", contribution_hex.as_str()])?,
        Tag::parse(["a", input.project_id])?,
        Tag::parse(["p", contributor_hex.as_str()])?,
        Tag::parse(["st8-status", input.status])?,
    ];
    let event = EventBuilder::new(Kind::Custom(KIND_ST8_LEDGER_ENTRY as u16), content)
        .tags(tags)
        .sign_with_keys(&state.relay_keypair)?;
    let (stored, inserted) = state
        .db
        .insert_event(tenant.community(), &event, None)
        .await?;
    if inserted {
        super::event::dispatch_persistent_event(
            tenant,
            state,
            &stored,
            KIND_ST8_LEDGER_ENTRY,
            &state.relay_keypair.public_key().to_hex(),
            None,
        )
        .await;
    }
    Ok(())
}

fn projection_content(
    ledger_projection: &serde_json::Value,
    anchor_state: &str,
    project_snapshot_id: Option<[u8; 32]>,
) -> serde_json::Value {
    let mut content = ledger_projection.clone();
    if let Some(object) = content.as_object_mut() {
        object.insert("anchor_state".into(), anchor_state.into());
        object.insert(
            "project_snapshot_id".into(),
            project_snapshot_id
                .map(hex::encode)
                .map_or(serde_json::Value::Null, serde_json::Value::String),
        );
    }
    content
}

async fn latest_ledger_projection(
    tenant: &TenantContext,
    state: &AppState,
    contribution_id: &str,
) -> anyhow::Result<Option<Event>> {
    let mut query = EventQuery::for_community(tenant.community());
    query.kinds = Some(vec![KIND_ST8_LEDGER_ENTRY as i32]);
    query.authors = Some(vec![state.relay_keypair.public_key().to_bytes().to_vec()]);
    query.d_tag = Some(contribution_id.to_owned());
    query.global_only = true;
    query.limit = Some(1);
    Ok(state
        .db
        .query_events(&query)
        .await?
        .into_iter()
        .next()
        .map(|stored| stored.event))
}

async fn linked_event(
    tenant: &TenantContext,
    event: &Event,
    state: &AppState,
) -> anyhow::Result<Option<Event>> {
    let event_id = single_tag(event, "e").context("missing unique e-tag")?;
    let id = hex::decode(event_id).context("invalid linked event ID")?;
    Ok(state
        .db
        .get_event_by_id(tenant.community(), &id)
        .await?
        .map(|stored| stored.event))
}

async fn event_by_hex_id(
    tenant: &TenantContext,
    state: &AppState,
    event_id: &str,
) -> anyhow::Result<Option<Event>> {
    if !is_hex_32(event_id) {
        return Err(anyhow!("invalid linked event ID"));
    }
    let id = hex::decode(event_id)?;
    Ok(state
        .db
        .get_event_by_id(tenant.community(), &id)
        .await?
        .map(|stored| stored.event))
}

async fn latest_addressable(
    tenant: &TenantContext,
    state: &AppState,
    kind: u32,
    author: &[u8],
    d_tag: &str,
) -> anyhow::Result<Option<Event>> {
    let mut query = EventQuery::for_community(tenant.community());
    query.kinds = Some(vec![kind as i32]);
    query.authors = Some(vec![author.to_vec()]);
    query.d_tag = Some(d_tag.to_owned());
    query.global_only = true;
    query.limit = Some(1);
    Ok(state
        .db
        .query_events(&query)
        .await?
        .into_iter()
        .next()
        .map(|stored| stored.event))
}

async fn approvals_for_proposal(
    tenant: &TenantContext,
    state: &AppState,
    proposal: &Event,
) -> anyhow::Result<Vec<Event>> {
    let mut query = EventQuery::for_community(tenant.community());
    query.kinds = Some(vec![KIND_ST8_GOVERNANCE_APPROVAL as i32]);
    query.e_tags = Some(vec![proposal.id.to_hex()]);
    query.global_only = true;
    query.limit = Some(256);
    Ok(state
        .db
        .query_events(&query)
        .await?
        .into_iter()
        .map(|stored| stored.event)
        .collect())
}

fn parse_project_coordinate(project: &str) -> anyhow::Result<(Vec<u8>, String)> {
    let mut parts = project.splitn(3, ':');
    if parts.next() != Some("30621") {
        return Err(anyhow!("invalid project coordinate kind"));
    }
    let owner = parts.next().context("project coordinate missing owner")?;
    let slug = parts.next().context("project coordinate missing slug")?;
    if slug.is_empty() || !is_hex_32(owner) {
        return Err(anyhow!("invalid project coordinate"));
    }
    Ok((hex::decode(owner)?, slug.to_owned()))
}

fn project_owner(project: &str) -> Option<String> {
    parse_project_coordinate(project)
        .ok()
        .map(|(owner, _)| hex::encode(owner))
}

fn hex_tag_bytes(event: &Event, name: &str) -> anyhow::Result<Vec<Vec<u8>>> {
    tag_values(event, name)
        .map(|value| {
            if !is_hex_32(value) {
                return Err(anyhow!("invalid {name} tag digest"));
            }
            Ok(hex::decode(value)?)
        })
        .collect()
}

fn unique_hex_tags(event: &Event, name: &str, min: usize, max: usize) -> bool {
    let mut values: Vec<&str> = tag_values(event, name).collect();
    values.sort_unstable();
    values.len() >= min
        && values.len() <= max
        && values.iter().all(|value| is_hex_32(value))
        && !values.windows(2).any(|pair| pair[0] == pair[1])
}

fn is_hex_32(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
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
