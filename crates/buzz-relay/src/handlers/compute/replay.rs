use super::*;

/// Retries a retained input through the same verification as initial ingestion.
pub(crate) async fn replay_event(
    tenant: &TenantContext,
    event: &Event,
    state: &Arc<AppState>,
) -> anyhow::Result<()> {
    match u32::from(event.kind.as_u16()) {
        KIND_ST8_COMPUTE_JOB => handle_job(tenant, event, state).await,
        KIND_ST8_COMPUTE_RECEIPT => handle_receipt(tenant, event, state).await,
        KIND_ST8_COMPUTE_DISPUTE => handle_dispute(tenant, event, state).await,
        KIND_ST8_COMPUTE_RESOLUTION => handle_resolution(tenant, event, state).await,
        KIND_ST8_COMPUTE_SETTLEMENT => handle_settlement(tenant, event, state).await,
        _ => Ok(()),
    }
}

pub(super) async fn replay_inputs(
    tenant: &TenantContext,
    state: &Arc<AppState>,
) -> anyhow::Result<()> {
    // Complete each prerequisite pass before consuming its dependents. In
    // particular, pending disputes must freeze receipts before settlement.
    for kind in [
        KIND_ST8_COMPUTE_JOB,
        KIND_ST8_COMPUTE_RECEIPT,
        KIND_ST8_COMPUTE_DISPUTE,
        KIND_ST8_COMPUTE_RESOLUTION,
        KIND_ST8_COMPUTE_SETTLEMENT,
    ] {
        let mut query = EventQuery::for_community(tenant.community());
        query.kinds = Some(vec![kind as i32]);
        query.global_only = true;
        query.limit = Some(500);
        loop {
            let events = state.db.query_events(&query).await?;
            for stored in &events {
                if let Err(error) = replay_event(tenant, &stored.event, state).await {
                    // A database outage must stop the pass before settlement
                    // can consume receipts whose pending disputes were skipped.
                    let database_error = error.downcast_ref::<sqlx::Error>().or_else(|| {
                        match error.downcast_ref::<buzz_db::DbError>() {
                            Some(buzz_db::DbError::Sqlx(error)) => Some(error),
                            _ => None,
                        }
                    });
                    if let Some(database_error) = database_error {
                        let constraint_conflict = database_error
                            .as_database_error()
                            .and_then(|error| error.code())
                            .is_some_and(|code| code.starts_with("23"));
                        if !constraint_conflict {
                            return Err(error);
                        }
                    }
                    // Missing dependencies may arrive later. One invalid or
                    // incomplete input must not starve the rest of the ledger.
                    tracing::warn!(event_id = %stored.event.id, kind, %error,
                        "retained ST8 Compute input is not projected; will retry");
                }
            }
            let Some(last) = events.last() else {
                break;
            };
            query.until = Some(
                chrono::DateTime::from_timestamp(
                    i64::try_from(last.event.created_at.as_secs())?,
                    0,
                )
                .context("compute input timestamp is out of range")?,
            );
            query.before_id = Some(last.event.id.as_bytes().to_vec());
            if events.len() < 500 {
                break;
            }
        }
    }
    Ok(())
}
