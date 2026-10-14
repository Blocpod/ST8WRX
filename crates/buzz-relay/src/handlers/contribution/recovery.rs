use super::*;
use std::collections::BTreeSet;

#[cfg(test)]
mod tests;

/// Replays stored governance approvals whose immutable contribution decision
/// has not yet been projected. This repairs a crash or transient database
/// failure after the approval event itself was durably accepted.
pub async fn reconcile_stored_approvals(state: &Arc<AppState>) -> anyhow::Result<usize> {
    const PAGE_SIZE: i64 = 256;
    let communities = state.db.usage_community_hosts().await?;
    let mut projected = 0usize;
    for community in communities {
        let community_id = buzz_core::CommunityId::from_uuid(community.id);
        let tenant = TenantContext::resolved(community_id, community.host);
        let mut query = EventQuery::for_community(community_id);
        query.kinds = Some(vec![KIND_ST8_GOVERNANCE_APPROVAL as i32]);
        query.global_only = true;
        query.limit = Some(PAGE_SIZE);
        loop {
            let approvals = state.db.query_events(&query).await?;
            let page_len = approvals.len();
            if page_len == 0 {
                break;
            }
            for stored in &approvals {
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
            advance(&mut query, &approvals[page_len - 1].event)?;
        }
    }
    Ok(projected)
}

// Stable event ordering avoids skipping retained inputs when newer events arrive.
fn advance(query: &mut EventQuery, event: &Event) -> anyhow::Result<()> {
    query.until = Some(
        DateTime::from_timestamp(i64::try_from(event.created_at.as_secs())?, 0)
            .context("approval timestamp out of range")?,
    );
    query.before_id = Some(event.id.as_bytes().to_vec());
    Ok(())
}

pub(super) async fn approvals_for_proposal(
    tenant: &TenantContext,
    state: &AppState,
    proposal: &Event,
    policy: &GovernancePolicyContext,
    claim: &ContributionClaimContext,
    intent: &GovernanceDecisionIntent,
) -> anyhow::Result<Vec<Event>> {
    let mut query = EventQuery::for_community(tenant.community());
    query.kinds = Some(vec![KIND_ST8_GOVERNANCE_APPROVAL as i32]);
    query.e_tags = Some(vec![proposal.id.to_hex()]);
    query.authors = Some(
        policy
            .policy
            .founders
            .iter()
            .map(|founder| {
                let key = founder
                    .strip_prefix("nostr:")
                    .context("invalid founder identity")?;
                Ok(hex::decode(key)?)
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
    );
    query.global_only = true;
    query.limit = Some(256);
    let mut valid = Vec::new();
    let mut signers = BTreeSet::new();
    loop {
        let page = state.db.query_events(&query).await?;
        for stored in &page {
            let event = &stored.event;
            // A retained but unauthorized, late, or malformed approval cannot veto
            // valid founders. Keep one fully verified event for each signer.
            if single_tag(event, "e") != Some(proposal.id.to_hex().as_str())
                || validate_envelope(event).is_err()
                || signers.contains(&event.pubkey)
            {
                continue;
            }
            match policy
                .policy
                .decide(&claim.record, intent, std::slice::from_ref(event))
            {
                Ok(_) | Err(EngineError::InsufficientApprovals { .. }) => {
                    signers.insert(event.pubkey);
                    valid.push(event.clone());
                }
                Err(_) => {}
            }
        }
        if page.len() < 256 {
            break;
        }
        if let Some(last) = page.last() {
            advance(&mut query, &last.event)?;
        }
    }
    Ok(valid)
}
