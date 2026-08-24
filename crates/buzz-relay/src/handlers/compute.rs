//! ST8 Compute event validation and durable project/provider ledger projection.

use std::sync::Arc;

use anyhow::{anyhow, Context};
use buzz_core::kind::{
    KIND_PROJECT, KIND_ST8_COMPUTE_CAPABILITY, KIND_ST8_COMPUTE_DISPUTE, KIND_ST8_COMPUTE_JOB,
    KIND_ST8_COMPUTE_LEDGER_ENTRY, KIND_ST8_COMPUTE_PRICING, KIND_ST8_COMPUTE_RECEIPT,
    KIND_ST8_COMPUTE_SETTLEMENT, KIND_ST8_COMPUTE_SETTLEMENT_ENTRY,
};
use buzz_core::tenant::TenantContext;
use buzz_db::compute::{
    ComputeInsertOutcome, NewComputeDispute, NewComputeJob, NewComputeReceipt, NewComputeSettlement,
};
use buzz_db::EventQuery;
use nostr::{Event, EventBuilder, Kind, Tag};
use st8_compute_engine::{
    ComputeDispute, ComputeJobContext, NodeCapabilityAttestation, NodeReceiptAttestation,
    PreparedComputeSettlementAnchor, PricingContext, SettlementContext, VerifiedComputeReceipt,
};
use st8_compute_protocol::{ComputeJob, ComputeSettlement, ExecutionStatus, PricingPolicy};
use st8_contribution_engine::BuzzProjectContext;

use crate::state::AppState;

/// Validates the stable transport-visible envelope before event storage.
pub fn validate_envelope(event: &Event) -> Result<(), String> {
    let kind = u32::from(event.kind.as_u16());
    match kind {
        KIND_ST8_COMPUTE_CAPABILITY => {
            NodeCapabilityAttestation::from_event(event).map_err(|error| error.to_string())?;
        }
        KIND_ST8_COMPUTE_PRICING => {
            let policy: PricingPolicy =
                serde_json::from_str(&event.content).map_err(|error| error.to_string())?;
            let policy_id = hex::encode(policy.id().map_err(|error| error.to_string())?);
            if event.pubkey.to_hex() != nostr_identity_hex(&policy.provider)?
                || single_tag(event, "a") != Some(policy.project.as_str())
                || single_tag(event, "d") != Some(policy_id.as_str())
                || single_tag(event, "st8-node") != Some(policy.node_owner_id.as_str())
            {
                return Err(
                    "pricing event does not bind provider, project, policy, and node".into(),
                );
            }
        }
        KIND_ST8_COMPUTE_JOB => {
            let job: ComputeJob =
                serde_json::from_str(&event.content).map_err(|error| error.to_string())?;
            let job_id = hex::encode(job.id().map_err(|error| error.to_string())?);
            if event.pubkey.to_hex() != nostr_identity_hex(&job.requester)?
                || single_tag(event, "a") != Some(job.project.as_str())
                || single_tag(event, "d") != Some(job_id.as_str())
                || single_tag(event, "e") != Some(hex::encode(job.request_event_id).as_str())
                || single_tag(event, "st8-node") != Some(job.node_owner_id.as_str())
                || single_tag(event, "st8-pricing")
                    != Some(hex::encode(job.pricing_policy_id).as_str())
            {
                return Err(
                    "compute job does not bind requester, project, request, policy, and node"
                        .into(),
                );
            }
        }
        KIND_ST8_COMPUTE_RECEIPT => {
            let attestation: NodeReceiptAttestation =
                serde_json::from_str(&event.content).map_err(|error| error.to_string())?;
            attestation
                .verify_node_signature()
                .map_err(|error| error.to_string())?;
            let receipt_id = hex::encode(
                attestation
                    .receipt
                    .id()
                    .map_err(|error| error.to_string())?,
            );
            if event.pubkey.to_hex() != nostr_identity_hex(&attestation.receipt.provider)?
                || single_tag(event, "a") != Some(attestation.receipt.project.as_str())
                || single_tag(event, "d") != Some(receipt_id.as_str())
                || single_tag(event, "e")
                    != Some(hex::encode(attestation.receipt.result_event_id).as_str())
                || single_tag(event, "st8-job")
                    != Some(hex::encode(attestation.receipt.job_id).as_str())
                || single_tag(event, "st8-node") != Some(attestation.receipt.node_owner_id.as_str())
            {
                return Err(
                    "compute receipt does not bind provider, project, result, job, and node".into(),
                );
            }
        }
        // Stateful signer/receipt-set checks run after storage.
        KIND_ST8_COMPUTE_DISPUTE | KIND_ST8_COMPUTE_SETTLEMENT => {
            if single_tag(event, "a").is_none() || single_tag(event, "d").is_none() {
                return Err(
                    "compute dispute/settlement is missing project or object identity".into(),
                );
            }
        }
        _ => return Err("not an ST8 Compute event".into()),
    }
    Ok(())
}

/// Projects one newly stored requester authorization.
pub async fn handle_job(
    tenant: &TenantContext,
    event: &Event,
    state: &Arc<AppState>,
) -> anyhow::Result<()> {
    let job: ComputeJob = serde_json::from_str(&event.content)?;
    let request_event = event_by_id(tenant, state, &job.request_event_id)
        .await?
        .context("compute request event is not stored")?;
    let pricing_event = latest_addressable(
        tenant,
        state,
        KIND_ST8_COMPUTE_PRICING,
        &hex::decode(nostr_identity_hex(&job.provider).map_err(|error| anyhow!(error))?)?,
        &hex::encode(job.pricing_policy_id),
    )
    .await?
    .context("exact compute pricing event is not stored")?;
    let project_context = project_context(tenant, state, &job.project).await?;
    let pricing = PricingContext::from_event(&project_context, &pricing_event)?;
    let verified =
        ComputeJobContext::from_events(&project_context, event, &pricing, &request_event)?;
    let inserted = state
        .db
        .insert_compute_job(
            tenant.community(),
            &NewComputeJob {
                job_id: verified.job.id()?.to_vec(),
                project_id: verified.job.project.clone(),
                requester_pubkey: event.pubkey.to_bytes().to_vec(),
                provider_pubkey: pricing_event.pubkey.to_bytes().to_vec(),
                node_owner_id: verified.job.node_owner_id.clone(),
                pricing_policy_id: verified.job.pricing_policy_id.to_vec(),
                request_event_id: verified.job.request_event_id.to_vec(),
                pricing_event: serde_json::to_value(&pricing_event)?,
                job_event: serde_json::to_value(event)?,
                request_event: serde_json::to_value(&request_event)?,
                job: serde_json::to_value(&verified.job)?,
            },
        )
        .await?;
    if inserted == ComputeInsertOutcome::Inserted {
        tracing::info!(job_id = %hex::encode(verified.job.id()?), "projected ST8 Compute job");
    }
    Ok(())
}

/// Projects one newly stored provider/node receipt after full verification.
pub async fn handle_receipt(
    tenant: &TenantContext,
    event: &Event,
    state: &Arc<AppState>,
) -> anyhow::Result<()> {
    let attestation: NodeReceiptAttestation = serde_json::from_str(&event.content)?;
    let job_id = attestation.receipt.job_id;
    let (pricing_json, job_json, request_json, _) = state
        .db
        .get_compute_job_material(tenant.community(), &job_id)
        .await?
        .context("compute job is not projected")?;
    let pricing_event: Event = serde_json::from_value(pricing_json)?;
    let job_event: Event = serde_json::from_value(job_json)?;
    let request_event: Event = serde_json::from_value(request_json)?;
    let result_event = event_by_id(tenant, state, &attestation.receipt.result_event_id)
        .await?
        .context("compute result event is not stored")?;
    let project_context = project_context(tenant, state, &attestation.receipt.project).await?;
    let verified = VerifiedComputeReceipt::from_events(
        project_context,
        &pricing_event,
        &job_event,
        &request_event,
        &result_event,
        event,
    )?;
    let receipt = &verified.attestation.receipt;
    let execution_status = match receipt.status {
        ExecutionStatus::Completed => "completed",
        ExecutionStatus::Failed => "failed",
        ExecutionStatus::Cancelled => "cancelled",
    };
    let receipt_id = receipt.id()?;
    let projection = serde_json::json!({
        "version": 1,
        "receipt_id": hex::encode(receipt_id),
        "job_id": hex::encode(receipt.job_id),
        "project_id": receipt.project,
        "requester": receipt.requester,
        "provider": receipt.provider,
        "node_owner_id": receipt.node_owner_id,
        "workload": receipt.workload,
        "agent": receipt.agent,
        "model": receipt.model,
        "started_at_ms": receipt.started_at_ms,
        "ended_at_ms": receipt.ended_at_ms,
        "usage": receipt.usage,
        "pricing_policy_id": hex::encode(receipt.pricing_policy_id),
        "cost_sats": receipt.price.total_sats,
        "status": receipt.status,
        "result_commitment": hex::encode(receipt.result_commitment),
        "request_event_id": hex::encode(receipt.request_event_id),
        "result_event_id": hex::encode(receipt.result_event_id),
        "agent_metric_event_id": receipt.agent_metric_event_id.map(hex::encode),
        "routing_target": receipt.routing_target,
        "meter_version": receipt.meter_version,
        "dispute_state": "undisputed",
        "settlement_id": null,
        "currency": "BSV_SATOSHIS",
        "contribution_units": null
    });
    let cost_sats = i64::try_from(receipt.price.total_sats)
        .map_err(|_| anyhow!("compute cost exceeds database range"))?;
    let inserted = state
        .db
        .insert_compute_receipt(
            tenant.community(),
            &NewComputeReceipt {
                receipt_id: receipt_id.to_vec(),
                job_id: receipt.job_id.to_vec(),
                project_id: receipt.project.clone(),
                requester_pubkey: hex::decode(
                    nostr_identity_hex(&receipt.requester).map_err(|error| anyhow!(error))?,
                )?,
                provider_pubkey: event.pubkey.to_bytes().to_vec(),
                node_owner_id: receipt.node_owner_id.clone(),
                result_event_id: receipt.result_event_id.to_vec(),
                started_at_ms: receipt.started_at_ms,
                ended_at_ms: receipt.ended_at_ms,
                cost_sats,
                execution_status: execution_status.into(),
                result_event: serde_json::to_value(&result_event)?,
                receipt_event: serde_json::to_value(event)?,
                verified_material: serde_json::to_value(&verified)?,
                ledger_projection: projection,
            },
        )
        .await?;
    if inserted == ComputeInsertOutcome::Inserted {
        tracing::info!(receipt_id = %hex::encode(receipt_id), cost_sats, "projected ST8 Compute receipt");
        let record = state
            .db
            .get_compute_receipt(tenant.community(), &receipt_id)
            .await?
            .context("inserted compute receipt disappeared")?;
        emit_receipt_projection(tenant, state, &record).await?;
    }
    Ok(())
}

/// Freezes one receipt after verifying the dispute signer and exact receipt material.
pub async fn handle_dispute(
    tenant: &TenantContext,
    event: &Event,
    state: &Arc<AppState>,
) -> anyhow::Result<()> {
    let dispute_body: ComputeDispute = serde_json::from_str(&event.content)?;
    let record = state
        .db
        .get_compute_receipt(tenant.community(), &dispute_body.receipt_id)
        .await?
        .context("disputed compute receipt is not projected")?;
    let verified: VerifiedComputeReceipt = serde_json::from_value(record.verified_material)?;
    let project = project_context(tenant, state, &record.project_id).await?;
    let dispute = ComputeDispute::from_event(&project, &verified, event)?;
    let inserted = state
        .db
        .insert_compute_dispute(
            tenant.community(),
            &NewComputeDispute {
                dispute_event_id: event.id.as_bytes().to_vec(),
                receipt_id: dispute.receipt_id.to_vec(),
                job_id: dispute.job_id.to_vec(),
                signer_pubkey: event.pubkey.to_bytes().to_vec(),
                dispute_event: serde_json::to_value(event)?,
                dispute: serde_json::to_value(&dispute)?,
            },
        )
        .await?;
    if inserted == ComputeInsertOutcome::Inserted {
        tracing::info!(receipt_id = %hex::encode(dispute.receipt_id), "froze disputed ST8 Compute receipt");
        let updated = state
            .db
            .get_compute_receipt(tenant.community(), &dispute.receipt_id)
            .await?
            .context("disputed compute receipt disappeared")?;
        emit_receipt_projection(tenant, state, &updated).await?;
    }
    Ok(())
}

/// Verifies and atomically projects one project-owner settlement snapshot.
pub async fn handle_settlement(
    tenant: &TenantContext,
    event: &Event,
    state: &Arc<AppState>,
) -> anyhow::Result<()> {
    let requested: ComputeSettlement = serde_json::from_str(&event.content)?;
    let ledger_lock = state
        .db
        .acquire_compute_project_lock(tenant.community(), &requested.project)
        .await?;
    let start_ms = requested
        .period_start
        .checked_mul(1_000)
        .context("compute settlement start is out of range")?;
    let end_ms = requested
        .period_end
        .checked_mul(1_000)
        .context("compute settlement end is out of range")?;
    let records = state
        .db
        .list_compute_receipts_for_settlement(
            tenant.community(),
            &requested.project,
            start_ms,
            end_ms,
        )
        .await?;
    let receipts = records
        .iter()
        .map(|record| serde_json::from_value(record.verified_material.clone()))
        .collect::<Result<Vec<VerifiedComputeReceipt>, _>>()?;
    let disputed_receipt_ids = records
        .iter()
        .filter(|record| record.dispute_state == "disputed")
        .map(|record| {
            <[u8; 32]>::try_from(record.receipt_id.as_slice())
                .map_err(|_| anyhow!("stored compute receipt ID is malformed"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let project = project_context(tenant, state, &requested.project).await?;
    let settlement =
        SettlementContext::from_event(&project, &receipts, &disputed_receipt_ids, event)?;
    let prepared = PreparedComputeSettlementAnchor::new(
        project,
        receipts,
        disputed_receipt_ids,
        event,
        st8_bsv_provenance::BsvNetwork::Testnet,
    )?;
    let settlement_id = settlement.settlement.id()?;
    let snapshot = serde_json::json!({
        "version": 1,
        "settlement_id": hex::encode(settlement_id),
        "project_id": settlement.settlement.project,
        "period_start": settlement.settlement.period_start,
        "period_end": settlement.settlement.period_end,
        "receipt_ids": settlement.settlement.receipt_ids.iter().map(hex::encode).collect::<Vec<_>>(),
        "disputed_receipt_ids": settlement.settlement.disputed_receipt_ids.iter().map(hex::encode).collect::<Vec<_>>(),
        "receipt_merkle_root": hex::encode(settlement.settlement.receipt_merkle_root),
        "balances": settlement.settlement.balances,
        "total_sats": settlement.settlement.total_sats,
        "currency": "BSV_SATOSHIS",
        "anchor_state": "queued",
        "anchor_payload": prepared.anchor_payload,
        "contribution_units": null
    });
    let total_sats = i64::try_from(settlement.settlement.total_sats)
        .map_err(|_| anyhow!("compute settlement total exceeds database range"))?;
    let outcome = state
        .db
        .insert_compute_settlement(
            tenant.community(),
            &NewComputeSettlement {
                settlement_id: settlement_id.to_vec(),
                project_id: settlement.settlement.project.clone(),
                merkle_root: settlement.settlement.receipt_merkle_root.to_vec(),
                receipt_ids: settlement
                    .settlement
                    .receipt_ids
                    .iter()
                    .map(|id| id.to_vec())
                    .collect(),
                total_sats,
                settlement_event: serde_json::to_value(event)?,
                settlement: serde_json::to_value(&settlement.settlement)?,
                settlement_snapshot: snapshot.clone(),
                prepared_anchor: serde_json::to_value(&prepared)?,
            },
        )
        .await?;
    if outcome == ComputeInsertOutcome::Inserted {
        tracing::info!(settlement_id = %hex::encode(settlement_id), total_sats, "queued ST8 Compute settlement anchor");
        for receipt_id in &settlement.settlement.receipt_ids {
            let record = state
                .db
                .get_compute_receipt(tenant.community(), receipt_id)
                .await?
                .context("settled compute receipt disappeared")?;
            emit_receipt_projection(tenant, state, &record).await?;
        }
        emit_settlement_projection(
            tenant,
            state,
            &settlement_id,
            &settlement.settlement.project,
            &settlement.settlement.receipt_merkle_root,
            &snapshot,
            "queued",
        )
        .await?;
    }
    ledger_lock.release_now().await?;
    Ok(())
}

async fn emit_receipt_projection(
    tenant: &TenantContext,
    state: &Arc<AppState>,
    record: &buzz_db::compute::ComputeReceiptRecord,
) -> anyhow::Result<bool> {
    let receipt = hex::encode(&record.receipt_id);
    let provider = hex::encode(&record.provider_pubkey);
    let content = record.ledger_projection.to_string();
    let relay_pubkey = state.relay_keypair.public_key().to_bytes();
    if latest_addressable(
        tenant,
        state,
        KIND_ST8_COMPUTE_LEDGER_ENTRY,
        &relay_pubkey,
        &receipt,
    )
    .await?
    .is_some_and(|event| event.content == content)
    {
        return Ok(false);
    }
    let event = EventBuilder::new(Kind::Custom(KIND_ST8_COMPUTE_LEDGER_ENTRY as u16), content)
        .tags([
            Tag::parse(["d", receipt.as_str()])?,
            Tag::parse(["a", record.project_id.as_str()])?,
            Tag::parse(["p", provider.as_str()])?,
            Tag::parse(["st8-status", record.execution_status.as_str()])?,
            Tag::parse(["st8-dispute", record.dispute_state.as_str()])?,
        ])
        .sign_with_keys(&state.relay_keypair)?;
    persist_relay_projection(tenant, state, &event, KIND_ST8_COMPUTE_LEDGER_ENTRY).await
}

async fn emit_settlement_projection(
    tenant: &TenantContext,
    state: &Arc<AppState>,
    settlement_id: &[u8; 32],
    project: &str,
    receipt_root: &[u8; 32],
    snapshot: &serde_json::Value,
    anchor_state: &str,
) -> anyhow::Result<bool> {
    let settlement = hex::encode(settlement_id);
    let root = hex::encode(receipt_root);
    let content = snapshot.to_string();
    let relay_pubkey = state.relay_keypair.public_key().to_bytes();
    if latest_addressable(
        tenant,
        state,
        KIND_ST8_COMPUTE_SETTLEMENT_ENTRY,
        &relay_pubkey,
        &settlement,
    )
    .await?
    .is_some_and(|event| event.content == content)
    {
        return Ok(false);
    }
    let event = EventBuilder::new(
        Kind::Custom(KIND_ST8_COMPUTE_SETTLEMENT_ENTRY as u16),
        content,
    )
    .tags([
        Tag::parse(["d", settlement.as_str()])?,
        Tag::parse(["a", project])?,
        Tag::parse(["st8-receipt-root", root.as_str()])?,
        Tag::parse(["st8-anchor-state", anchor_state])?,
    ])
    .sign_with_keys(&state.relay_keypair)?;
    persist_relay_projection(tenant, state, &event, KIND_ST8_COMPUTE_SETTLEMENT_ENTRY).await
}

/// Repairs relay-signed receipt/settlement query projections after restarts or worker updates.
pub async fn reconcile_ledger_events(state: &Arc<AppState>) -> anyhow::Result<usize> {
    const PAGE_SIZE: i64 = 500;
    let communities = state.db.usage_community_hosts().await?;
    let mut emitted = 0usize;
    for community in communities {
        let community_id = buzz_core::CommunityId::from_uuid(community.id);
        let tenant = TenantContext::resolved(community_id, community.host);
        let mut offset = 0i64;
        loop {
            let records = state
                .db
                .list_community_compute_receipts(community_id, PAGE_SIZE, offset)
                .await?;
            let page_len = records.len();
            for record in &records {
                if emit_receipt_projection(&tenant, state, record).await? {
                    emitted = emitted.saturating_add(1);
                }
            }
            if page_len < usize::try_from(PAGE_SIZE)? {
                break;
            }
            offset = offset
                .checked_add(PAGE_SIZE)
                .context("compute receipt reconciliation offset overflow")?;
        }
        let mut offset = 0i64;
        loop {
            let records = state
                .db
                .list_community_compute_settlements(community_id, PAGE_SIZE, offset)
                .await?;
            let page_len = records.len();
            for record in &records {
                let settlement_id: [u8; 32] = record
                    .settlement_id
                    .as_slice()
                    .try_into()
                    .map_err(|_| anyhow!("stored compute settlement ID is malformed"))?;
                let merkle_root: [u8; 32] = record
                    .merkle_root
                    .as_slice()
                    .try_into()
                    .map_err(|_| anyhow!("stored compute receipt root is malformed"))?;
                let snapshot = settlement_projection_content(record);
                if emit_settlement_projection(
                    &tenant,
                    state,
                    &settlement_id,
                    &record.project_id,
                    &merkle_root,
                    &snapshot,
                    &record.anchor_state,
                )
                .await?
                {
                    emitted = emitted.saturating_add(1);
                }
            }
            if page_len < usize::try_from(PAGE_SIZE)? {
                break;
            }
            offset = offset
                .checked_add(PAGE_SIZE)
                .context("compute settlement reconciliation offset overflow")?;
        }
    }
    Ok(emitted)
}

fn settlement_projection_content(
    record: &buzz_db::compute::ComputeSettlementRecord,
) -> serde_json::Value {
    let mut snapshot = record.settlement_snapshot.clone();
    if let Some(object) = snapshot.as_object_mut() {
        object.insert(
            "anchor_state".into(),
            serde_json::Value::String(record.anchor_state.clone()),
        );
        object.insert(
            "anchor_receipt_available".into(),
            serde_json::Value::Bool(record.receipt.is_some()),
        );
        if let Some(receipt) = &record.receipt {
            if let Some(txid) = receipt
                .pointer("/compute/transaction/txid")
                .and_then(serde_json::Value::as_array)
                .and_then(|bytes| {
                    bytes
                        .iter()
                        .map(|byte| byte.as_u64().and_then(|value| u8::try_from(value).ok()))
                        .collect::<Option<Vec<_>>>()
                })
                .filter(|bytes| bytes.len() == 32)
            {
                object.insert(
                    "bsv_txid".into(),
                    serde_json::Value::String(hex::encode(txid)),
                );
            }
            if let Some(verification) = receipt.pointer("/network/verification_state") {
                object.insert("bsv_verification_state".into(), verification.clone());
            }
        }
    }
    snapshot
}

async fn persist_relay_projection(
    tenant: &TenantContext,
    state: &Arc<AppState>,
    event: &Event,
    kind: u32,
) -> anyhow::Result<bool> {
    let (stored, inserted) = state
        .db
        .insert_event(tenant.community(), event, None)
        .await?;
    if inserted {
        super::event::dispatch_persistent_event(
            tenant,
            state,
            &stored,
            kind,
            &state.relay_keypair.public_key().to_hex(),
            None,
        )
        .await;
    }
    Ok(inserted)
}

async fn project_context(
    tenant: &TenantContext,
    state: &AppState,
    project: &str,
) -> anyhow::Result<BuzzProjectContext> {
    let mut parts = project.splitn(3, ':');
    if parts.next() != Some("30621") {
        return Err(anyhow!("invalid compute project coordinate"));
    }
    let owner = parts.next().context("project missing owner")?;
    let slug = parts.next().context("project missing slug")?;
    let event = latest_addressable(tenant, state, KIND_PROJECT, &hex::decode(owner)?, slug)
        .await?
        .context("compute project event is not stored")?;
    Ok(BuzzProjectContext::from_event(&event)?)
}

async fn event_by_id(
    tenant: &TenantContext,
    state: &AppState,
    id: &[u8; 32],
) -> anyhow::Result<Option<Event>> {
    Ok(state
        .db
        .get_event_by_id(tenant.community(), id)
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

fn nostr_identity_hex(identity: &str) -> Result<String, String> {
    let value = identity.strip_prefix("nostr:").unwrap_or(identity);
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid Nostr identity".into());
    }
    Ok(value.to_ascii_lowercase())
}

fn single_tag<'a>(event: &'a Event, name: &'a str) -> Option<&'a str> {
    let mut values = event.tags.iter().filter_map(move |tag| {
        let values = tag.as_slice();
        (values.first().map(String::as_str) == Some(name))
            .then(|| values.get(1).map(String::as_str))
            .flatten()
    });
    let value = values.next()?;
    values.next().is_none().then_some(value)
}
