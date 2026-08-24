//! Durable project/provider ST8 Compute accounting.
//!
//! Signed events are authoritative inputs. These tables are queryable,
//! idempotent projections and an asynchronous settlement-anchor queue;
//! blockchain latency never enters event ingest or compute execution.

use buzz_core::CommunityId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgAdvisoryLock, PgAdvisoryLockGuard};
use sqlx::Postgres;
use sqlx::Row;

use crate::{Db, DbError, Result};

pub use crate::compute_anchor::ComputeAnchorJobRecord;

/// One verified requester authorization ready for persistence.
#[derive(Debug, Clone)]
pub struct NewComputeJob {
    /// Deterministic job ID.
    pub job_id: Vec<u8>,
    /// Kind-30621 project coordinate.
    pub project_id: String,
    /// Raw requester Nostr public key.
    pub requester_pubkey: Vec<u8>,
    /// Raw provider Nostr public key.
    pub provider_pubkey: Vec<u8>,
    /// Persistent Mesh owner ID.
    pub node_owner_id: String,
    /// Deterministic pricing policy ID.
    pub pricing_policy_id: Vec<u8>,
    /// Signed underlying Mesh job request event ID.
    pub request_event_id: Vec<u8>,
    /// Complete provider-signed pricing event.
    pub pricing_event: Value,
    /// Complete requester-signed compute job event.
    pub job_event: Value,
    /// Complete requester-signed Mesh job request event.
    pub request_event: Value,
    /// Deterministic compute job body.
    pub job: Value,
}

/// One independently verified node/provider receipt.
#[derive(Debug, Clone)]
pub struct NewComputeReceipt {
    /// Deterministic receipt ID.
    pub receipt_id: Vec<u8>,
    /// Deterministic job ID.
    pub job_id: Vec<u8>,
    /// Kind-30621 project coordinate.
    pub project_id: String,
    /// Raw requester Nostr public key.
    pub requester_pubkey: Vec<u8>,
    /// Raw provider Nostr public key.
    pub provider_pubkey: Vec<u8>,
    /// Persistent Mesh owner ID.
    pub node_owner_id: String,
    /// Signed result event ID.
    pub result_event_id: Vec<u8>,
    /// Runtime-observed execution start in Unix milliseconds.
    pub started_at_ms: i64,
    /// Runtime-observed execution end in Unix milliseconds.
    pub ended_at_ms: i64,
    /// Deterministically calculated integer satoshis.
    pub cost_sats: i64,
    /// `completed`, `failed`, or `cancelled`.
    pub execution_status: String,
    /// Complete terminal result event.
    pub result_event: Value,
    /// Complete provider-signed receipt event.
    pub receipt_event: Value,
    /// All material required for independent verification.
    pub verified_material: Value,
    /// Public relay projection without private prompt/result bodies.
    pub ledger_projection: Value,
}

/// One verified signed dispute ready for atomic projection.
#[derive(Debug, Clone)]
pub struct NewComputeDispute {
    /// Signed dispute event ID.
    pub dispute_event_id: Vec<u8>,
    /// Exact disputed receipt ID.
    pub receipt_id: Vec<u8>,
    /// Exact job ID.
    pub job_id: Vec<u8>,
    /// Raw authorized signer public key.
    pub signer_pubkey: Vec<u8>,
    /// Complete signed dispute event.
    pub dispute_event: Value,
    /// Verified dispute body.
    pub dispute: Value,
}

/// One verified deterministic settlement and queued asynchronous anchor.
#[derive(Debug, Clone)]
pub struct NewComputeSettlement {
    /// Deterministic settlement ID.
    pub settlement_id: Vec<u8>,
    /// Kind-30621 project coordinate.
    pub project_id: String,
    /// Project-scoped receipt Merkle root.
    pub merkle_root: Vec<u8>,
    /// Exact included receipt IDs.
    pub receipt_ids: Vec<Vec<u8>>,
    /// Gross integer satoshi settlement total.
    pub total_sats: i64,
    /// Complete project-owner-signed settlement event.
    pub settlement_event: Value,
    /// Deterministic settlement body.
    pub settlement: Value,
    /// Public settlement snapshot/projection.
    pub settlement_snapshot: Value,
    /// Independently verifiable wallet-ready anchor material.
    pub prepared_anchor: Value,
}

/// Queryable durable compute receipt projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeReceiptRecord {
    /// Deterministic receipt ID.
    pub receipt_id: Vec<u8>,
    /// Deterministic job ID.
    pub job_id: Vec<u8>,
    /// Project coordinate.
    pub project_id: String,
    /// Requester Nostr public key.
    pub requester_pubkey: Vec<u8>,
    /// Provider Nostr public key.
    pub provider_pubkey: Vec<u8>,
    /// Persistent Mesh owner ID.
    pub node_owner_id: String,
    /// Integer satoshi cost.
    pub cost_sats: i64,
    /// Execution state.
    pub execution_status: String,
    /// Dispute state.
    pub dispute_state: String,
    /// Settlement containing this receipt, when settled.
    pub settlement_id: Option<Vec<u8>>,
    /// Independently verifiable signed material.
    pub verified_material: Value,
    /// Public relay projection.
    pub ledger_projection: Value,
    /// Insertion time.
    pub created_at: DateTime<Utc>,
    /// Last projection update.
    pub updated_at: DateTime<Utc>,
}

/// Queryable deterministic settlement and asynchronous anchor state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeSettlementRecord {
    /// Deterministic settlement ID.
    pub settlement_id: Vec<u8>,
    /// Project coordinate.
    pub project_id: String,
    /// Project-scoped receipt Merkle root.
    pub merkle_root: Vec<u8>,
    /// Number of included receipts.
    pub receipt_count: i32,
    /// Gross integer satoshi total.
    pub total_sats: i64,
    /// Public settlement projection.
    pub settlement_snapshot: Value,
    /// Current asynchronous anchor state.
    pub anchor_state: String,
    /// Complete independently verifiable public receipt when confirmed.
    pub receipt: Option<Value>,
    /// Insertion time.
    pub created_at: DateTime<Utc>,
    /// Last state update.
    pub updated_at: DateTime<Utc>,
}

/// Result of an idempotent compute projection insert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputeInsertOutcome {
    /// A new immutable job or receipt was inserted.
    Inserted,
    /// The same deterministic object already existed.
    AlreadyPresent,
}

impl Db {
    /// Acquires a cross-process lock for one project's compute ledger.
    pub async fn acquire_compute_project_lock(
        &self,
        community_id: CommunityId,
        project_id: &str,
    ) -> Result<PgAdvisoryLockGuard<PoolConnection<Postgres>>> {
        if project_id.trim().is_empty() {
            return Err(DbError::InvalidData("empty compute project ID".into()));
        }
        let lock = PgAdvisoryLock::new(format!(
            "st8-compute-ledger:{}:{project_id}",
            community_id.as_uuid()
        ));
        let connection = self.pool.acquire().await?;
        Ok(lock.acquire(connection).await?)
    }

    /// Idempotently persists one verified requester compute authorization.
    pub async fn insert_compute_job(
        &self,
        community_id: CommunityId,
        job: &NewComputeJob,
    ) -> Result<ComputeInsertOutcome> {
        validate_job(job)?;
        let result = sqlx::query(
            "INSERT INTO st8_compute_jobs
             (community_id, job_id, project_id, requester_pubkey, provider_pubkey,
              node_owner_id, pricing_policy_id, request_event_id, pricing_event,
              job_event, request_event, job)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
             ON CONFLICT (community_id, job_id) DO NOTHING",
        )
        .bind(community_id.as_uuid())
        .bind(&job.job_id)
        .bind(&job.project_id)
        .bind(&job.requester_pubkey)
        .bind(&job.provider_pubkey)
        .bind(&job.node_owner_id)
        .bind(&job.pricing_policy_id)
        .bind(&job.request_event_id)
        .bind(&job.pricing_event)
        .bind(&job.job_event)
        .bind(&job.request_event)
        .bind(&job.job)
        .execute(&self.pool)
        .await?;
        Ok(if result.rows_affected() == 1 {
            ComputeInsertOutcome::Inserted
        } else {
            ComputeInsertOutcome::AlreadyPresent
        })
    }

    /// Loads the complete retained job material by deterministic ID.
    pub async fn get_compute_job_material(
        &self,
        community_id: CommunityId,
        job_id: &[u8],
    ) -> Result<Option<(Value, Value, Value, Value)>> {
        validate_digest("compute job ID", job_id)?;
        let row = sqlx::query(
            "SELECT pricing_event, job_event, request_event, job
               FROM st8_compute_jobs
              WHERE community_id=$1 AND job_id=$2",
        )
        .bind(community_id.as_uuid())
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok((
                row.try_get("pricing_event")?,
                row.try_get("job_event")?,
                row.try_get("request_event")?,
                row.try_get("job")?,
            ))
        })
        .transpose()
    }

    /// Atomically inserts one immutable receipt and advances its job state.
    pub async fn insert_compute_receipt(
        &self,
        community_id: CommunityId,
        receipt: &NewComputeReceipt,
    ) -> Result<ComputeInsertOutcome> {
        validate_receipt(receipt)?;
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "INSERT INTO st8_compute_receipts
             (community_id, receipt_id, job_id, project_id, requester_pubkey,
              provider_pubkey, node_owner_id, result_event_id, started_at_ms,
              ended_at_ms, cost_sats, execution_status, result_event,
              receipt_event, verified_material, ledger_projection)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)
             ON CONFLICT (community_id, receipt_id) DO NOTHING",
        )
        .bind(community_id.as_uuid())
        .bind(&receipt.receipt_id)
        .bind(&receipt.job_id)
        .bind(&receipt.project_id)
        .bind(&receipt.requester_pubkey)
        .bind(&receipt.provider_pubkey)
        .bind(&receipt.node_owner_id)
        .bind(&receipt.result_event_id)
        .bind(receipt.started_at_ms)
        .bind(receipt.ended_at_ms)
        .bind(receipt.cost_sats)
        .bind(&receipt.execution_status)
        .bind(&receipt.result_event)
        .bind(&receipt.receipt_event)
        .bind(&receipt.verified_material)
        .bind(&receipt.ledger_projection)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(ComputeInsertOutcome::AlreadyPresent);
        }
        let updated = sqlx::query(
            "UPDATE st8_compute_jobs
                SET status=$3, updated_at=NOW()
              WHERE community_id=$1 AND job_id=$2 AND status='authorized'",
        )
        .bind(community_id.as_uuid())
        .bind(&receipt.job_id)
        .bind(&receipt.execution_status)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(DbError::InvalidData(
                "compute job is missing or already has a terminal receipt".into(),
            ));
        }
        tx.commit().await?;
        Ok(ComputeInsertOutcome::Inserted)
    }

    /// Lists project receipts newest first.
    pub async fn list_compute_receipts_for_project(
        &self,
        community_id: CommunityId,
        project_id: &str,
        limit: i64,
    ) -> Result<Vec<ComputeReceiptRecord>> {
        if project_id.is_empty() || !(1..=500).contains(&limit) {
            return Err(DbError::InvalidData(
                "invalid compute project or page size".into(),
            ));
        }
        let rows = sqlx::query(
            "SELECT receipt_id, job_id, project_id, requester_pubkey,
                    provider_pubkey, node_owner_id, cost_sats, execution_status,
                    dispute_state, settlement_id, verified_material,
                    ledger_projection, created_at, updated_at
               FROM st8_compute_receipts
              WHERE community_id=$1 AND project_id=$2
              ORDER BY created_at DESC, receipt_id ASC
              LIMIT $3",
        )
        .bind(community_id.as_uuid())
        .bind(project_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(receipt_record).collect()
    }

    /// Lists community receipt projections for relay-event reconciliation.
    pub async fn list_community_compute_receipts(
        &self,
        community_id: CommunityId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ComputeReceiptRecord>> {
        if !(1..=500).contains(&limit) || offset < 0 {
            return Err(DbError::InvalidData("invalid compute receipt page".into()));
        }
        let rows = sqlx::query(
            "SELECT receipt_id, job_id, project_id, requester_pubkey,
                    provider_pubkey, node_owner_id, cost_sats, execution_status,
                    dispute_state, settlement_id, verified_material,
                    ledger_projection, created_at, updated_at
               FROM st8_compute_receipts
              WHERE community_id=$1
              ORDER BY created_at, receipt_id LIMIT $2 OFFSET $3",
        )
        .bind(community_id.as_uuid())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(receipt_record).collect()
    }

    /// Lists durable settlements with their public anchor receipts.
    pub async fn list_community_compute_settlements(
        &self,
        community_id: CommunityId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ComputeSettlementRecord>> {
        if !(1..=500).contains(&limit) || offset < 0 {
            return Err(DbError::InvalidData(
                "invalid compute settlement page".into(),
            ));
        }
        let rows = sqlx::query(
            "SELECT s.settlement_id, s.project_id, s.merkle_root, s.receipt_count,
                    s.total_sats, s.settlement_snapshot, s.anchor_state,
                    j.receipt, s.created_at, s.updated_at
               FROM st8_compute_settlements s
               JOIN st8_compute_anchor_jobs j
                 ON j.community_id=s.community_id AND j.settlement_id=s.settlement_id
              WHERE s.community_id=$1
              ORDER BY s.created_at, s.settlement_id LIMIT $2 OFFSET $3",
        )
        .bind(community_id.as_uuid())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(settlement_record).collect()
    }

    /// Gets one receipt by deterministic ID.
    pub async fn get_compute_receipt(
        &self,
        community_id: CommunityId,
        receipt_id: &[u8],
    ) -> Result<Option<ComputeReceiptRecord>> {
        validate_digest("compute receipt ID", receipt_id)?;
        let row = sqlx::query(
            "SELECT receipt_id, job_id, project_id, requester_pubkey,
                    provider_pubkey, node_owner_id, cost_sats, execution_status,
                    dispute_state, settlement_id, verified_material,
                    ledger_projection, created_at, updated_at
               FROM st8_compute_receipts
              WHERE community_id=$1 AND receipt_id=$2",
        )
        .bind(community_id.as_uuid())
        .bind(receipt_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(receipt_record).transpose()
    }

    /// Returns all terminal receipts considered by one deterministic period.
    pub async fn list_compute_receipts_for_settlement(
        &self,
        community_id: CommunityId,
        project_id: &str,
        period_start_ms: i64,
        period_end_ms: i64,
    ) -> Result<Vec<ComputeReceiptRecord>> {
        if project_id.trim().is_empty() || period_end_ms <= period_start_ms {
            return Err(DbError::InvalidData(
                "invalid compute settlement period".into(),
            ));
        }
        let rows = sqlx::query(
            "SELECT receipt_id, job_id, project_id, requester_pubkey,
                    provider_pubkey, node_owner_id, cost_sats, execution_status,
                    dispute_state, settlement_id, verified_material,
                    ledger_projection, created_at, updated_at
              FROM st8_compute_receipts
              WHERE community_id=$1 AND project_id=$2
                AND settlement_id IS NULL
                AND ended_at_ms >= $3 AND ended_at_ms < $4
              ORDER BY receipt_id
              LIMIT 4097",
        )
        .bind(community_id.as_uuid())
        .bind(project_id)
        .bind(period_start_ms)
        .bind(period_end_ms)
        .fetch_all(&self.pool)
        .await?;
        if rows.len() > 4096 {
            return Err(DbError::InvalidData(
                "compute settlement exceeds receipt limit".into(),
            ));
        }
        rows.into_iter().map(receipt_record).collect()
    }

    /// Atomically persists an authorized dispute and freezes its receipt.
    pub async fn insert_compute_dispute(
        &self,
        community_id: CommunityId,
        dispute: &NewComputeDispute,
    ) -> Result<ComputeInsertOutcome> {
        validate_dispute(dispute)?;
        let mut tx = self.pool.begin().await?;
        let inserted = sqlx::query(
            "INSERT INTO st8_compute_disputes
             (community_id, dispute_event_id, receipt_id, job_id, signer_pubkey,
              dispute_event, dispute)
             VALUES ($1,$2,$3,$4,$5,$6,$7)
             ON CONFLICT (community_id, dispute_event_id) DO NOTHING",
        )
        .bind(community_id.as_uuid())
        .bind(&dispute.dispute_event_id)
        .bind(&dispute.receipt_id)
        .bind(&dispute.job_id)
        .bind(&dispute.signer_pubkey)
        .bind(&dispute.dispute_event)
        .bind(&dispute.dispute)
        .execute(&mut *tx)
        .await?;
        if inserted.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(ComputeInsertOutcome::AlreadyPresent);
        }
        let updated = sqlx::query(
            "UPDATE st8_compute_receipts
                SET dispute_state='disputed',
                    ledger_projection=jsonb_set(ledger_projection, '{dispute_state}',
                                                '\"disputed\"'::jsonb),
                    updated_at=NOW()
              WHERE community_id=$1 AND receipt_id=$2 AND job_id=$3
                AND dispute_state='undisputed' AND settlement_id IS NULL",
        )
        .bind(community_id.as_uuid())
        .bind(&dispute.receipt_id)
        .bind(&dispute.job_id)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(DbError::InvalidData(
                "compute receipt is missing, settled, or already disputed".into(),
            ));
        }
        sqlx::query(
            "UPDATE st8_compute_jobs SET status='disputed', updated_at=NOW()
              WHERE community_id=$1 AND job_id=$2 AND status IN ('completed','failed','cancelled')",
        )
        .bind(community_id.as_uuid())
        .bind(&dispute.job_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(ComputeInsertOutcome::Inserted)
    }

    /// Atomically consumes an exact undisputed receipt set and queues anchoring.
    pub async fn insert_compute_settlement(
        &self,
        community_id: CommunityId,
        settlement: &NewComputeSettlement,
    ) -> Result<ComputeInsertOutcome> {
        validate_settlement(settlement)?;
        let receipt_count = i32::try_from(settlement.receipt_ids.len())
            .map_err(|_| DbError::InvalidData("too many settlement receipts".into()))?;
        let mut tx = self.pool.begin().await?;
        let inserted = sqlx::query(
            "INSERT INTO st8_compute_settlements
             (community_id, settlement_id, project_id, merkle_root, receipt_count,
              total_sats, settlement_event, settlement, settlement_snapshot, prepared_anchor)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
             ON CONFLICT (community_id, settlement_id) DO NOTHING",
        )
        .bind(community_id.as_uuid())
        .bind(&settlement.settlement_id)
        .bind(&settlement.project_id)
        .bind(&settlement.merkle_root)
        .bind(receipt_count)
        .bind(settlement.total_sats)
        .bind(&settlement.settlement_event)
        .bind(&settlement.settlement)
        .bind(&settlement.settlement_snapshot)
        .bind(&settlement.prepared_anchor)
        .execute(&mut *tx)
        .await?;
        if inserted.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(ComputeInsertOutcome::AlreadyPresent);
        }
        let updated = sqlx::query(
            "UPDATE st8_compute_receipts
                SET settlement_id=$4,
                    ledger_projection=jsonb_set(
                        jsonb_set(ledger_projection, '{settlement_id}', to_jsonb(encode($4, 'hex'))),
                        '{settlement_state}', '\"queued\"'::jsonb),
                    updated_at=NOW()
              WHERE community_id=$1 AND project_id=$2 AND receipt_id=ANY($3)
                AND dispute_state='undisputed' AND settlement_id IS NULL",
        )
        .bind(community_id.as_uuid())
        .bind(&settlement.project_id)
        .bind(&settlement.receipt_ids)
        .bind(&settlement.settlement_id)
        .execute(&mut *tx)
        .await?;
        let expected_updates = u64::try_from(settlement.receipt_ids.len())
            .map_err(|_| DbError::InvalidData("too many settlement receipts".into()))?;
        if updated.rows_affected() != expected_updates {
            return Err(DbError::InvalidData(
                "settlement receipt set changed, is disputed, or was already consumed".into(),
            ));
        }
        sqlx::query(
            "UPDATE st8_compute_jobs SET status='settled', updated_at=NOW()
              WHERE community_id=$1 AND job_id IN (
                    SELECT job_id FROM st8_compute_receipts
                     WHERE community_id=$1 AND settlement_id=$2)",
        )
        .bind(community_id.as_uuid())
        .bind(&settlement.settlement_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO st8_compute_anchor_jobs
             (community_id, settlement_id, project_id, prepared_anchor)
             VALUES ($1,$2,$3,$4)",
        )
        .bind(community_id.as_uuid())
        .bind(&settlement.settlement_id)
        .bind(&settlement.project_id)
        .bind(&settlement.prepared_anchor)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(ComputeInsertOutcome::Inserted)
    }
}

fn receipt_record(row: sqlx::postgres::PgRow) -> Result<ComputeReceiptRecord> {
    Ok(ComputeReceiptRecord {
        receipt_id: row.try_get("receipt_id")?,
        job_id: row.try_get("job_id")?,
        project_id: row.try_get("project_id")?,
        requester_pubkey: row.try_get("requester_pubkey")?,
        provider_pubkey: row.try_get("provider_pubkey")?,
        node_owner_id: row.try_get("node_owner_id")?,
        cost_sats: row.try_get("cost_sats")?,
        execution_status: row.try_get("execution_status")?,
        dispute_state: row.try_get("dispute_state")?,
        settlement_id: row.try_get("settlement_id")?,
        verified_material: row.try_get("verified_material")?,
        ledger_projection: row.try_get("ledger_projection")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn settlement_record(row: sqlx::postgres::PgRow) -> Result<ComputeSettlementRecord> {
    Ok(ComputeSettlementRecord {
        settlement_id: row.try_get("settlement_id")?,
        project_id: row.try_get("project_id")?,
        merkle_root: row.try_get("merkle_root")?,
        receipt_count: row.try_get("receipt_count")?,
        total_sats: row.try_get("total_sats")?,
        settlement_snapshot: row.try_get("settlement_snapshot")?,
        anchor_state: row.try_get("anchor_state")?,
        receipt: row.try_get("receipt")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn validate_job(job: &NewComputeJob) -> Result<()> {
    validate_digest("compute job ID", &job.job_id)?;
    validate_digest("compute pricing ID", &job.pricing_policy_id)?;
    validate_digest("compute request event ID", &job.request_event_id)?;
    if job.project_id.is_empty()
        || job.requester_pubkey.len() != 32
        || job.provider_pubkey.len() != 32
        || job.node_owner_id.len() != 64
    {
        return Err(DbError::InvalidData(
            "invalid compute job projection".into(),
        ));
    }
    Ok(())
}

fn validate_receipt(receipt: &NewComputeReceipt) -> Result<()> {
    validate_digest("compute receipt ID", &receipt.receipt_id)?;
    validate_digest("compute job ID", &receipt.job_id)?;
    validate_digest("compute result event ID", &receipt.result_event_id)?;
    if receipt.project_id.is_empty()
        || receipt.requester_pubkey.len() != 32
        || receipt.provider_pubkey.len() != 32
        || receipt.node_owner_id.len() != 64
        || receipt.cost_sats < 0
        || receipt.ended_at_ms < receipt.started_at_ms
        || !matches!(
            receipt.execution_status.as_str(),
            "completed" | "failed" | "cancelled"
        )
    {
        return Err(DbError::InvalidData(
            "invalid compute receipt projection".into(),
        ));
    }
    Ok(())
}

fn validate_dispute(dispute: &NewComputeDispute) -> Result<()> {
    validate_digest("compute dispute event ID", &dispute.dispute_event_id)?;
    validate_digest("compute receipt ID", &dispute.receipt_id)?;
    validate_digest("compute job ID", &dispute.job_id)?;
    if dispute.signer_pubkey.len() != 32 {
        return Err(DbError::InvalidData(
            "invalid compute dispute signer".into(),
        ));
    }
    Ok(())
}

fn validate_settlement(settlement: &NewComputeSettlement) -> Result<()> {
    validate_digest("compute settlement ID", &settlement.settlement_id)?;
    validate_digest("compute receipt Merkle root", &settlement.merkle_root)?;
    if settlement.project_id.trim().is_empty()
        || settlement.total_sats < 0
        || settlement.receipt_ids.is_empty()
        || settlement.receipt_ids.len() > 4096
        || settlement
            .receipt_ids
            .iter()
            .any(|receipt| receipt.len() != 32)
    {
        return Err(DbError::InvalidData("invalid compute settlement".into()));
    }
    let mut ids = settlement.receipt_ids.clone();
    ids.sort();
    ids.dedup();
    if ids.len() != settlement.receipt_ids.len() {
        return Err(DbError::InvalidData(
            "duplicate compute settlement receipt".into(),
        ));
    }
    Ok(())
}

fn validate_digest(name: &str, digest: &[u8]) -> Result<()> {
    if digest.len() != 32 {
        return Err(DbError::InvalidData(format!("invalid {name}")));
    }
    Ok(())
}
