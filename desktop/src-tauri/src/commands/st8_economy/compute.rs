use super::*;
use serde::Deserialize;
use st8_compute_engine::{
    ComputeDispute, ComputeResolution, ComputeResolutionOutcome, NodeReceiptAttestation,
    VerifiedComputeReceipt,
};
use st8_compute_protocol::ComputeJob;

/// Signed dispute operations for an immutable compute receipt.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ComputeAction {
    /// Contest a receipt as its customer or provider.
    Dispute { receipt_id: String, reason: String },
    /// Arbitrate an exact dispute as the project owner.
    Resolve {
        receipt_id: String,
        dispute_event_id: String,
        outcome: ComputeResolutionOutcome,
        reason: String,
    },
}

async fn receipt_material(
    scope: &EconomyScope<'_>,
    project: &BuzzProjectContext,
    receipt_id: &str,
) -> Result<VerifiedComputeReceipt, String> {
    let receipt_digest = digest(receipt_id)?;
    let ledger = scope.query(json!({"kinds":[KIND_ST8_COMPUTE_LEDGER_ENTRY], "#a":[project.project], "#d":[hex::encode(receipt_digest)], "limit":1}))
        .await?.into_iter().next().ok_or("Compute receipt was not found")?;
    let projection: Value = serde_json::from_str(&ledger.content).map_err(error)?;
    let receipt_event = if let Some(id) = projection.get("receipt_event_id").and_then(Value::as_str)
    {
        scope.event(id, KIND_ST8_COMPUTE_RECEIPT).await?
    } else {
        // Historical projections predate the exact source-event pointer.
        let job = projection
            .get("job_id")
            .and_then(Value::as_str)
            .ok_or("Receipt job is missing")?;
        digest(job)?;
        scope.query(json!({"kinds":[KIND_ST8_COMPUTE_RECEIPT], "#a":[project.project], "#st8-job":[job], "limit":100})).await?
            .into_iter().find(|event| serde_json::from_str::<NodeReceiptAttestation>(&event.content).ok()
                .and_then(|attestation| attestation.receipt.id().ok()).is_some_and(|id| id == receipt_digest))
            .ok_or("Exact signed compute receipt was not found in the bounded history")?
    };
    let attestation: NodeReceiptAttestation =
        serde_json::from_str(&receipt_event.content).map_err(error)?;
    let receipt = &attestation.receipt;
    if receipt.id().map_err(error)? != receipt_digest || receipt.project != project.project {
        return Err("Compute receipt does not match the selected project and receipt".into());
    }
    let job_event = scope
        .query(
            json!({"kinds":[KIND_ST8_COMPUTE_JOB], "#a":[project.project],
        "#e":[hex::encode(receipt.request_event_id)], "limit":100}),
        )
        .await?
        .into_iter()
        .find(|event| {
            serde_json::from_str::<ComputeJob>(&event.content)
                .ok()
                .and_then(|job| job.id().ok())
                .is_some_and(|id| id == receipt.job_id)
        })
        .ok_or("Exact signed compute job was not found")?;
    let provider = receipt
        .provider
        .strip_prefix("nostr:")
        .unwrap_or(&receipt.provider);
    digest(provider)?;
    let pricing = scope
        .query(
            json!({"kinds":[KIND_ST8_COMPUTE_PRICING], "authors":[provider],
        "#d":[hex::encode(receipt.pricing_policy_id)], "limit":1}),
        )
        .await?
        .into_iter()
        .next()
        .ok_or("Exact signed pricing policy was not found")?;
    let request = scope
        .event(&hex::encode(receipt.request_event_id), KIND_JOB_REQUEST)
        .await?;
    let result = scope
        .event(&hex::encode(receipt.result_event_id), KIND_JOB_RESULT)
        .await?;
    VerifiedComputeReceipt::from_events(
        project.clone(),
        &pricing,
        &job_event,
        &request,
        &result,
        &receipt_event,
    )
    .map_err(error)
}

/// Verify receipt evidence, then sign and submit a compute dispute operation.
#[tauri::command]
pub async fn st8_compute_action(
    project: String,
    action: ComputeAction,
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
    let receipt_id = match &action {
        ComputeAction::Dispute { receipt_id, .. } | ComputeAction::Resolve { receipt_id, .. } => {
            receipt_id
        }
    };
    let material = receipt_material(&scope, &context, receipt_id).await?;
    let now = nostr::Timestamp::now();
    let timestamp = i64::try_from(now.as_secs()).map_err(error)?;
    let builder = match action {
        ComputeAction::Dispute { reason, .. } => {
            let dispute = ComputeDispute {
                project,
                receipt_id: material.attestation.receipt.id().map_err(error)?,
                job_id: material.attestation.receipt.job_id,
                disputed_at: timestamp,
                reason,
                evidence: Vec::new(),
            };
            let builder = dispute
                .event_builder()
                .map_err(error)?
                .custom_created_at(now);
            let signed = builder.clone().sign_with_keys(&scope.keys).map_err(error)?;
            ComputeDispute::from_event(&context, &material, &signed).map_err(error)?;
            builder
        }
        ComputeAction::Resolve {
            dispute_event_id,
            outcome,
            reason,
            ..
        } => {
            let dispute = scope
                .event(&dispute_event_id, KIND_ST8_COMPUTE_DISPUTE)
                .await?;
            let resolution = ComputeResolution {
                project,
                receipt_id: material.attestation.receipt.id().map_err(error)?,
                dispute_event_id: *dispute.id.as_bytes(),
                outcome,
                resolved_at: timestamp,
                reason,
            };
            let builder = resolution
                .event_builder()
                .map_err(error)?
                .custom_created_at(now);
            let signed = builder.clone().sign_with_keys(&scope.keys).map_err(error)?;
            ComputeResolution::from_event(&context, &material, &dispute, &signed).map_err(error)?;
            builder
        }
    };
    scope.submit(builder).await
}
