use super::*;
use buzz_db::compute::NewComputeResolution;
use st8_compute_engine::{ComputeResolution, ComputeResolutionOutcome};

/// Applies the project owner's final arbitration without changing original receipt evidence.
pub async fn handle_resolution(
    tenant: &TenantContext,
    event: &Event,
    state: &Arc<AppState>,
) -> anyhow::Result<()> {
    let body: ComputeResolution = serde_json::from_str(&event.content)?;
    let lock = state
        .db
        .acquire_compute_project_lock(tenant.community(), &body.project)
        .await?;
    let record = state
        .db
        .get_compute_receipt(tenant.community(), &body.receipt_id)
        .await?
        .context("resolved compute receipt is not projected")?;
    let dispute: Event = serde_json::from_value(
        state
            .db
            .get_compute_dispute_event(tenant.community(), &body.dispute_event_id)
            .await?
            .context("exact compute dispute is not projected")?,
    )?;
    let verified: VerifiedComputeReceipt = serde_json::from_value(record.verified_material)?;
    let project = project_context(tenant, state, &record.project_id).await?;
    let resolution = ComputeResolution::from_event(&project, &verified, &dispute, event)?;
    state
        .db
        .resolve_compute_dispute(
            tenant.community(),
            &NewComputeResolution {
                event_id: event.id.as_bytes().to_vec(),
                dispute_event_id: resolution.dispute_event_id.to_vec(),
                receipt_id: resolution.receipt_id.to_vec(),
                void_charge: resolution.outcome == ComputeResolutionOutcome::VoidCharge,
                event: serde_json::to_value(event)?,
            },
        )
        .await?;
    let updated = state
        .db
        .get_compute_receipt(tenant.community(), &resolution.receipt_id)
        .await?
        .context("resolved compute receipt disappeared")?;
    emit_receipt_projection(tenant, state, &updated).await?;
    lock.release_now().await?;
    Ok(())
}
