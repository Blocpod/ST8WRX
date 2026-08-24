//! Crash-safe asynchronous ST8 Compute settlement anchor queue operations.

use std::time::Duration;

use buzz_core::CommunityId;
use serde_json::Value;
use sqlx::Row;

use crate::{Db, DbError, Result};

/// Claimed asynchronous compute-settlement anchor work item.
#[derive(Debug, Clone)]
pub struct ComputeAnchorJobRecord {
    /// Owning community.
    pub community_id: CommunityId,
    /// Deterministic settlement ID.
    pub settlement_id: Vec<u8>,
    /// Project coordinate.
    pub project_id: String,
    /// Attempt count including this claim.
    pub attempts: i32,
    /// Wallet-ready prepared settlement anchor.
    pub prepared_anchor: Value,
    /// Previously submitted txid.
    pub txid: Option<Vec<u8>>,
    /// Previously submitted raw transaction.
    pub raw_transaction: Option<Vec<u8>>,
    /// Previously returned Atomic BEEF.
    pub atomic_beef: Option<Vec<u8>>,
    /// Previously persisted broadcast response.
    pub broadcast_receipt: Option<Value>,
}

impl Db {
    /// Claims one due compute-settlement anchor job without blocking peers.
    pub async fn claim_compute_anchor_job(
        &self,
        worker_id: &str,
        lease_duration: Duration,
    ) -> Result<Option<ComputeAnchorJobRecord>> {
        if worker_id.trim().is_empty() {
            return Err(DbError::InvalidData(
                "empty compute anchor worker ID".into(),
            ));
        }
        let lease_seconds = i64::try_from(lease_duration.as_secs())
            .map_err(|_| DbError::InvalidData("compute anchor lease is too long".into()))?;
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT community_id, settlement_id, project_id, attempts, prepared_anchor,
                    txid, raw_transaction, atomic_beef, broadcast_receipt
               FROM st8_compute_anchor_jobs
              WHERE ((status IN ('queued','failed','broadcast') AND next_attempt_at <= NOW())
                     OR (status='leased' AND lease_until < NOW()))
              ORDER BY next_attempt_at, created_at
              FOR UPDATE SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(None);
        };
        let community_uuid = row.try_get("community_id")?;
        let settlement_id: Vec<u8> = row.try_get("settlement_id")?;
        let attempts: i32 = row.try_get("attempts")?;
        sqlx::query(
            "UPDATE st8_compute_anchor_jobs
                SET status='leased', attempts=attempts+1, lease_owner=$3,
                    lease_until=NOW()+make_interval(secs => $4), updated_at=NOW(), last_error=NULL
              WHERE community_id=$1 AND settlement_id=$2",
        )
        .bind(community_uuid)
        .bind(&settlement_id)
        .bind(worker_id)
        .bind(lease_seconds as f64)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE st8_compute_settlements SET anchor_state='leased', updated_at=NOW()
              WHERE community_id=$1 AND settlement_id=$2",
        )
        .bind(community_uuid)
        .bind(&settlement_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(ComputeAnchorJobRecord {
            community_id: CommunityId::from_uuid(community_uuid),
            settlement_id,
            project_id: row.try_get("project_id")?,
            attempts: attempts.saturating_add(1),
            prepared_anchor: row.try_get("prepared_anchor")?,
            txid: row.try_get("txid")?,
            raw_transaction: row.try_get("raw_transaction")?,
            atomic_beef: row.try_get("atomic_beef")?,
            broadcast_receipt: row.try_get("broadcast_receipt")?,
        }))
    }

    /// Persists an external-wallet-signed compute settlement transaction.
    pub async fn save_compute_anchor_transaction(
        &self,
        job: &ComputeAnchorJobRecord,
        worker_id: &str,
        txid: &[u8],
        raw_transaction: &[u8],
        atomic_beef: &[u8],
    ) -> Result<()> {
        if txid.len() != 32 || raw_transaction.is_empty() || atomic_beef.is_empty() {
            return Err(DbError::InvalidData(
                "invalid compute anchor submission".into(),
            ));
        }
        let updated = sqlx::query(
            "UPDATE st8_compute_anchor_jobs SET txid=$4, raw_transaction=$5,
                    atomic_beef=$6, updated_at=NOW()
              WHERE community_id=$1 AND settlement_id=$2
                AND status='leased' AND lease_owner=$3
                AND (txid IS NULL OR (txid=$4 AND raw_transaction=$5 AND atomic_beef=$6))",
        )
        .bind(job.community_id.as_uuid())
        .bind(&job.settlement_id)
        .bind(worker_id)
        .bind(txid)
        .bind(raw_transaction)
        .bind(atomic_beef)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if updated != 1 {
            return Err(DbError::InvalidData(
                "compute anchor transaction conflicts or lease was lost".into(),
            ));
        }
        Ok(())
    }

    /// Persists the public broadcast response for a compute settlement.
    pub async fn save_compute_anchor_broadcast(
        &self,
        job: &ComputeAnchorJobRecord,
        worker_id: &str,
        broadcast_receipt: &Value,
    ) -> Result<()> {
        let updated = sqlx::query(
            "UPDATE st8_compute_anchor_jobs SET broadcast_receipt=$4, updated_at=NOW()
              WHERE community_id=$1 AND settlement_id=$2
                AND status='leased' AND lease_owner=$3 AND txid IS NOT NULL",
        )
        .bind(job.community_id.as_uuid())
        .bind(&job.settlement_id)
        .bind(worker_id)
        .bind(broadcast_receipt)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if updated != 1 {
            return Err(DbError::InvalidData(
                "compute anchor broadcast was not saved under the active lease".into(),
            ));
        }
        sqlx::query(
            "UPDATE st8_compute_settlements SET anchor_state='broadcast', updated_at=NOW()
              WHERE community_id=$1 AND settlement_id=$2",
        )
        .bind(job.community_id.as_uuid())
        .bind(&job.settlement_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Confirms a mined/observed compute settlement and persists its public proof.
    #[allow(clippy::too_many_arguments)]
    pub async fn confirm_compute_anchor_job(
        &self,
        job: &ComputeAnchorJobRecord,
        worker_id: &str,
        txid: &[u8],
        raw_transaction: &[u8],
        atomic_beef: Option<&[u8]>,
        broadcast_receipt: &Value,
        network_evidence: &Value,
        receipt: &Value,
    ) -> Result<()> {
        if txid.len() != 32 || raw_transaction.is_empty() {
            return Err(DbError::InvalidData(
                "invalid confirmed compute transaction".into(),
            ));
        }
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            "UPDATE st8_compute_anchor_jobs
                SET status='confirmed', lease_owner=NULL, lease_until=NULL, txid=$4,
                    raw_transaction=$5, atomic_beef=$6, broadcast_receipt=$7,
                    network_evidence=$8, receipt=$9, updated_at=NOW(), last_error=NULL
              WHERE community_id=$1 AND settlement_id=$2
                AND status='leased' AND lease_owner=$3",
        )
        .bind(job.community_id.as_uuid())
        .bind(&job.settlement_id)
        .bind(worker_id)
        .bind(txid)
        .bind(raw_transaction)
        .bind(atomic_beef)
        .bind(broadcast_receipt)
        .bind(network_evidence)
        .bind(receipt)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if updated != 1 {
            return Err(DbError::InvalidData(
                "compute anchor lease was lost before confirmation".into(),
            ));
        }
        sqlx::query(
            "UPDATE st8_compute_settlements SET anchor_state='confirmed', updated_at=NOW()
              WHERE community_id=$1 AND settlement_id=$2",
        )
        .bind(job.community_id.as_uuid())
        .bind(&job.settlement_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE st8_compute_receipts
                SET ledger_projection=jsonb_set(ledger_projection, '{settlement_state}',
                                                '\"confirmed\"'::jsonb),
                    updated_at=NOW()
              WHERE community_id=$1 AND settlement_id=$2",
        )
        .bind(job.community_id.as_uuid())
        .bind(&job.settlement_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Releases a failed compute anchor for retry without blocking collaboration.
    pub async fn fail_compute_anchor_job(
        &self,
        job: &ComputeAnchorJobRecord,
        worker_id: &str,
        error: &str,
        retry_after: Duration,
    ) -> Result<()> {
        let retry_seconds = i64::try_from(retry_after.as_secs())
            .map_err(|_| DbError::InvalidData("compute anchor retry is too long".into()))?;
        let updated = sqlx::query(
            "UPDATE st8_compute_anchor_jobs
                SET status='failed', lease_owner=NULL, lease_until=NULL,
                    next_attempt_at=NOW()+make_interval(secs => $4),
                    last_error=left($5, 2048), updated_at=NOW()
              WHERE community_id=$1 AND settlement_id=$2
                AND status='leased' AND lease_owner=$3",
        )
        .bind(job.community_id.as_uuid())
        .bind(&job.settlement_id)
        .bind(worker_id)
        .bind(retry_seconds as f64)
        .bind(error)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if updated != 1 {
            return Err(DbError::InvalidData(
                "compute anchor lease was lost before failure release".into(),
            ));
        }
        sqlx::query(
            "UPDATE st8_compute_settlements SET anchor_state='failed', updated_at=NOW()
              WHERE community_id=$1 AND settlement_id=$2",
        )
        .bind(job.community_id.as_uuid())
        .bind(&job.settlement_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
