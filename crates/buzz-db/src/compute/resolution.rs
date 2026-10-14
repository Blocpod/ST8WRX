use super::*;

/// A verified owner decision on an exact accepted compute dispute.
pub struct NewComputeResolution {
    /// Signed resolution event ID.
    pub event_id: Vec<u8>,
    /// Signed dispute event ID.
    pub dispute_event_id: Vec<u8>,
    /// Deterministic receipt ID.
    pub receipt_id: Vec<u8>,
    /// True permanently voids the original charge; false releases it unchanged.
    pub void_charge: bool,
    /// Complete owner-signed resolution evidence.
    pub event: Value,
}

impl Db {
    /// Returns the exact accepted signed dispute for verification of a resolution.
    pub async fn get_compute_dispute_event(
        &self,
        community_id: CommunityId,
        dispute_event_id: &[u8],
    ) -> Result<Option<Value>> {
        validate_digest("compute dispute event ID", dispute_event_id)?;
        Ok(sqlx::query_scalar(
            "SELECT dispute_event FROM st8_compute_disputes
             WHERE community_id=$1 AND dispute_event_id=$2",
        )
        .bind(community_id.as_uuid())
        .bind(dispute_event_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Applies one final decision atomically; settled receipts and conflicting decisions fail.
    pub async fn resolve_compute_dispute(
        &self,
        community_id: CommunityId,
        resolution: &NewComputeResolution,
    ) -> Result<ComputeInsertOutcome> {
        validate_digest("compute resolution event ID", &resolution.event_id)?;
        validate_digest("compute dispute event ID", &resolution.dispute_event_id)?;
        validate_digest("compute receipt ID", &resolution.receipt_id)?;
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT r.job_id, r.execution_status, r.dispute_state, r.settlement_id,
                    d.resolution_event_id, d.resolution_event
             FROM st8_compute_receipts r JOIN st8_compute_disputes d
               ON d.community_id=r.community_id AND d.receipt_id=r.receipt_id
             WHERE r.community_id=$1 AND r.receipt_id=$2 AND d.dispute_event_id=$3
             FOR UPDATE OF r, d",
        )
        .bind(community_id.as_uuid())
        .bind(&resolution.receipt_id)
        .bind(&resolution.dispute_event_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| DbError::InvalidData("exact compute dispute is not projected".into()))?;
        let existing_id: Option<Vec<u8>> = row.try_get("resolution_event_id")?;
        if let Some(existing_id) = existing_id {
            let existing: Value = row.try_get("resolution_event")?;
            if existing_id != resolution.event_id || existing != resolution.event {
                return Err(DbError::InvalidData(
                    "compute dispute already has a final decision".into(),
                ));
            }
            return Ok(ComputeInsertOutcome::AlreadyPresent);
        }
        let state: String = row.try_get("dispute_state")?;
        let settlement: Option<Vec<u8>> = row.try_get("settlement_id")?;
        if state != "disputed" || settlement.is_some() {
            return Err(DbError::InvalidData(
                "receipt is not an unsettled dispute".into(),
            ));
        }
        let dispute_state = if resolution.void_charge {
            "resolved_upheld"
        } else {
            "resolved_rejected"
        };
        sqlx::query(
            "UPDATE st8_compute_disputes SET resolution_event_id=$3, resolution_event=$4
             WHERE community_id=$1 AND dispute_event_id=$2",
        )
        .bind(community_id.as_uuid())
        .bind(&resolution.dispute_event_id)
        .bind(&resolution.event_id)
        .bind(&resolution.event)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE st8_compute_receipts SET dispute_state=$3,
                 ledger_projection=jsonb_set(jsonb_set(jsonb_set(ledger_projection,
                     '{dispute_state}', to_jsonb($3::text)), '{resolution_event}', $4),
                     '{billable_cost_sats}', to_jsonb(CASE WHEN $5 THEN 0 ELSE cost_sats END)),
                 updated_at=NOW()
             WHERE community_id=$1 AND receipt_id=$2",
        )
        .bind(community_id.as_uuid())
        .bind(&resolution.receipt_id)
        .bind(dispute_state)
        .bind(&resolution.event)
        .bind(resolution.void_charge)
        .execute(&mut *tx)
        .await?;
        let job_id: Vec<u8> = row.try_get("job_id")?;
        let execution_status: String = row.try_get("execution_status")?;
        sqlx::query(
            "UPDATE st8_compute_jobs SET status=$3, updated_at=NOW()
             WHERE community_id=$1 AND job_id=$2 AND status='disputed'",
        )
        .bind(community_id.as_uuid())
        .bind(job_id)
        .bind(execution_status)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(ComputeInsertOutcome::Inserted)
    }
}
