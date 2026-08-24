//! Durable ST8WRX contribution ledger and asynchronous BSV anchor queue.
//!
//! The relay writes verified contribution state here after the signed approval
//! threshold is satisfied. A separate worker claims anchor jobs with
//! `FOR UPDATE SKIP LOCKED`; blockchain latency never enters event ingest.

use std::time::Duration;

use buzz_core::CommunityId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgAdvisoryLock, PgAdvisoryLockGuard};
use sqlx::{Postgres, Row};

use crate::{Db, DbError, Result};

/// Maximum ledger rows returned by one query.
pub const MAX_LEDGER_PAGE: i64 = 500;

/// One verified evidence event persisted beside its contribution.
#[derive(Debug, Clone)]
pub struct NewContributionEvidence {
    /// Raw 32-byte Nostr event ID.
    pub event_id: Vec<u8>,
    /// Nostr kind.
    pub kind: i32,
    /// Complete signed event JSON.
    pub event: Value,
}

/// One verified governance approval persisted beside its contribution.
#[derive(Debug, Clone)]
pub struct NewGovernanceApproval {
    /// Raw 32-byte approver public key.
    pub approver_pubkey: Vec<u8>,
    /// Raw 32-byte approval event ID.
    pub event_id: Vec<u8>,
    /// Complete signed approval event JSON.
    pub event: Value,
    /// Signed event timestamp.
    pub approved_at: DateTime<Utc>,
}

/// Deterministic project-wide snapshot and wallet-ready anchor material.
#[derive(Debug, Clone)]
pub struct NewProjectSnapshot {
    /// Stable 32-byte snapshot ID.
    pub snapshot_id: Vec<u8>,
    /// Project-scoped Merkle root.
    pub merkle_root: Vec<u8>,
    /// Number of committed accepted contribution leaves.
    pub leaf_count: i32,
    /// Complete canonical snapshot representation.
    pub snapshot: Value,
    /// Prepared testnet anchor representation consumed by the wallet worker.
    pub prepared_anchor: Value,
}

/// Fully verified ledger insertion assembled from signed Buzz events.
#[derive(Debug, Clone)]
pub struct NewContributionLedgerEntry {
    /// Deterministic 32-byte contribution ID.
    pub contribution_id: Vec<u8>,
    /// Kind-30621 project coordinate.
    pub project_id: String,
    /// Raw 32-byte contributor public key.
    pub contributor_pubkey: Vec<u8>,
    /// `accepted`, `adjusted`, or `rejected`.
    pub status: String,
    /// Non-transferable Contribution Units.
    pub contribution_units: i64,
    /// Complete signed project event.
    pub project_event: Value,
    /// Complete project-owner-signed policy event.
    pub policy_event: Value,
    /// Complete contributor-signed claim event.
    pub claim_event: Value,
    /// Complete authorized decision proposal event.
    pub decision_proposal_event: Value,
    /// Deterministic contribution record.
    pub record: Value,
    /// Threshold-satisfied governance decision.
    pub decision: Value,
    /// Public relay projection with signed-event references and derived state.
    pub ledger_projection: Value,
    /// Accepted/adjusted contribution snapshot; absent for a rejection.
    pub contribution_snapshot: Option<Value>,
    /// Deterministic contribution snapshot ID; absent for a rejection.
    pub contribution_snapshot_id: Option<Vec<u8>>,
    /// Verified supporting evidence history.
    pub evidence: Vec<NewContributionEvidence>,
    /// Verified governance approval history.
    pub approvals: Vec<NewGovernanceApproval>,
    /// Project-wide snapshot and queued anchor; absent for a rejection.
    pub project_snapshot: Option<NewProjectSnapshot>,
}

/// Result of an idempotent ledger insertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerInsertOutcome {
    /// A new immutable contribution decision was inserted.
    Inserted,
    /// The exact contribution decision already existed.
    AlreadyPresent,
}

/// Queryable durable contribution projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContributionLedgerRecord {
    /// Deterministic contribution ID.
    pub contribution_id: Vec<u8>,
    /// Project coordinate.
    pub project_id: String,
    /// Contributor public key.
    pub contributor_pubkey: Vec<u8>,
    /// Governance status.
    pub status: String,
    /// Non-transferable Contribution Units.
    pub contribution_units: i64,
    /// Deterministic contribution record JSON.
    pub record: Value,
    /// Signed threshold-satisfied decision JSON.
    pub decision: Value,
    /// Public relay projection with evidence and approval history references.
    pub ledger_projection: Value,
    /// Current asynchronous anchor state.
    pub anchor_state: String,
    /// Project snapshot containing this contribution, when accepted/adjusted.
    pub project_snapshot_id: Option<Vec<u8>>,
    /// Persisted independently verifiable receipt, when confirmed.
    pub receipt: Option<Value>,
    /// Insertion time.
    pub created_at: DateTime<Utc>,
    /// Last projection update.
    pub updated_at: DateTime<Utc>,
}

/// Claimed asynchronous anchor work item.
#[derive(Debug, Clone)]
pub struct AnchorJobRecord {
    /// Owning community.
    pub community_id: CommunityId,
    /// Deterministic project snapshot ID.
    pub snapshot_id: Vec<u8>,
    /// Project coordinate.
    pub project_id: String,
    /// Attempt count including this claim.
    pub attempts: i32,
    /// Wallet-ready prepared anchor JSON.
    pub prepared_anchor: Value,
    /// Previously submitted txid when resuming after a crash/network delay.
    pub txid: Option<Vec<u8>>,
    /// Previously submitted raw transaction.
    pub raw_transaction: Option<Vec<u8>>,
    /// Previously returned Atomic BEEF.
    pub atomic_beef: Option<Vec<u8>>,
    /// Previously persisted ARC response.
    pub broadcast_receipt: Option<Value>,
}

impl Db {
    /// Acquires a cross-process project ledger lock on a dedicated writer
    /// connection. Hold the guard while reading the accepted snapshot set and
    /// inserting the next immutable project snapshot.
    pub async fn acquire_contribution_project_lock(
        &self,
        community_id: CommunityId,
        project_id: &str,
    ) -> Result<PgAdvisoryLockGuard<PoolConnection<Postgres>>> {
        if project_id.is_empty() {
            return Err(DbError::InvalidData("empty contribution project ID".into()));
        }
        let lock = PgAdvisoryLock::new(format!(
            "st8-contribution-ledger:{}:{project_id}",
            community_id.as_uuid()
        ));
        let connection = self.pool.acquire().await?;
        Ok(lock.acquire(connection).await?)
    }

    /// Returns whether an immutable contribution decision is already projected.
    pub async fn contribution_exists(
        &self,
        community_id: CommunityId,
        contribution_id: &[u8],
    ) -> Result<bool> {
        if contribution_id.len() != 32 {
            return Err(DbError::InvalidData("invalid contribution ID".into()));
        }
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM st8_contributions
                 WHERE community_id=$1 AND contribution_id=$2
             )",
        )
        .bind(community_id.as_uuid())
        .bind(contribution_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    /// Atomically persists an immutable contribution decision, normalized proof
    /// history, project snapshot, and optional anchor job.
    pub async fn insert_contribution_ledger_entry(
        &self,
        community_id: CommunityId,
        entry: &NewContributionLedgerEntry,
    ) -> Result<LedgerInsertOutcome> {
        validate_entry(entry)?;
        let mut tx = self.pool.begin().await?;
        let project_snapshot_id = entry
            .project_snapshot
            .as_ref()
            .map(|snapshot| snapshot.snapshot_id.as_slice());
        if let Some(snapshot) = &entry.project_snapshot {
            sqlx::query(
                "INSERT INTO st8_project_snapshots
                 (community_id, snapshot_id, project_id, merkle_root, leaf_count, snapshot)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (community_id, snapshot_id) DO NOTHING",
            )
            .bind(community_id.as_uuid())
            .bind(&snapshot.snapshot_id)
            .bind(&entry.project_id)
            .bind(&snapshot.merkle_root)
            .bind(snapshot.leaf_count)
            .bind(&snapshot.snapshot)
            .execute(&mut *tx)
            .await?;
        }
        let anchor_state = if entry.project_snapshot.is_some() {
            "queued"
        } else {
            "not_applicable"
        };
        let inserted = sqlx::query(
            "INSERT INTO st8_contributions
             (community_id, contribution_id, project_id, contributor_pubkey, status,
              contribution_units, project_event, policy_event, claim_event,
              decision_proposal_event, record, decision, ledger_projection,
              contribution_snapshot, contribution_snapshot_id, project_snapshot_id, anchor_state)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)
             ON CONFLICT (community_id, contribution_id) DO NOTHING",
        )
        .bind(community_id.as_uuid())
        .bind(&entry.contribution_id)
        .bind(&entry.project_id)
        .bind(&entry.contributor_pubkey)
        .bind(&entry.status)
        .bind(entry.contribution_units)
        .bind(&entry.project_event)
        .bind(&entry.policy_event)
        .bind(&entry.claim_event)
        .bind(&entry.decision_proposal_event)
        .bind(&entry.record)
        .bind(&entry.decision)
        .bind(&entry.ledger_projection)
        .bind(&entry.contribution_snapshot)
        .bind(&entry.contribution_snapshot_id)
        .bind(project_snapshot_id)
        .bind(anchor_state)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        if inserted == 0 {
            let existing = sqlx::query(
                "SELECT project_id, contributor_pubkey, status, contribution_units,
                        record, decision, ledger_projection
                   FROM st8_contributions
                  WHERE community_id = $1 AND contribution_id = $2",
            )
            .bind(community_id.as_uuid())
            .bind(&entry.contribution_id)
            .fetch_one(&mut *tx)
            .await?;
            let same = existing.try_get::<String, _>("project_id")? == entry.project_id
                && existing.try_get::<Vec<u8>, _>("contributor_pubkey")?
                    == entry.contributor_pubkey
                && existing.try_get::<String, _>("status")? == entry.status
                && existing.try_get::<i64, _>("contribution_units")? == entry.contribution_units
                && existing.try_get::<Value, _>("record")? == entry.record
                && existing.try_get::<Value, _>("decision")? == entry.decision
                && existing.try_get::<Value, _>("ledger_projection")? == entry.ledger_projection;
            tx.rollback().await?;
            return if same {
                Ok(LedgerInsertOutcome::AlreadyPresent)
            } else {
                Err(DbError::InvalidData(
                    "conflicting immutable ST8 contribution ID".into(),
                ))
            };
        }

        for evidence in &entry.evidence {
            sqlx::query(
                "INSERT INTO st8_contribution_evidence
                 (community_id, contribution_id, evidence_event_id, evidence_kind, evidence_event)
                 VALUES ($1,$2,$3,$4,$5)",
            )
            .bind(community_id.as_uuid())
            .bind(&entry.contribution_id)
            .bind(&evidence.event_id)
            .bind(evidence.kind)
            .bind(&evidence.event)
            .execute(&mut *tx)
            .await?;
        }
        for approval in &entry.approvals {
            sqlx::query(
                "INSERT INTO st8_governance_approvals
                 (community_id, contribution_id, approver_pubkey, approval_event_id,
                  approval_event, approved_at)
                 VALUES ($1,$2,$3,$4,$5,$6)",
            )
            .bind(community_id.as_uuid())
            .bind(&entry.contribution_id)
            .bind(&approval.approver_pubkey)
            .bind(&approval.event_id)
            .bind(&approval.event)
            .bind(approval.approved_at)
            .execute(&mut *tx)
            .await?;
        }
        if let Some(snapshot) = &entry.project_snapshot {
            sqlx::query(
                "INSERT INTO st8_anchor_jobs
                 (community_id, snapshot_id, project_id, prepared_anchor)
                 VALUES ($1,$2,$3,$4)
                 ON CONFLICT (community_id, snapshot_id) DO NOTHING",
            )
            .bind(community_id.as_uuid())
            .bind(&snapshot.snapshot_id)
            .bind(&entry.project_id)
            .bind(&snapshot.prepared_anchor)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(LedgerInsertOutcome::Inserted)
    }

    /// Lists contribution-ledger rows for one project, newest first.
    pub async fn list_project_contributions(
        &self,
        community_id: CommunityId,
        project_id: &str,
        limit: i64,
    ) -> Result<Vec<ContributionLedgerRecord>> {
        let limit = limit.clamp(1, MAX_LEDGER_PAGE);
        let rows = sqlx::query(
            "SELECT contribution_id, project_id, contributor_pubkey, status,
                    contribution_units, record, decision, ledger_projection, anchor_state,
                    project_snapshot_id, receipt,
                    created_at, updated_at
               FROM st8_contributions
              WHERE community_id = $1 AND project_id = $2
              ORDER BY created_at DESC, contribution_id DESC LIMIT $3",
        )
        .bind(community_id.as_uuid())
        .bind(project_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(map_ledger_record).collect()
    }

    /// Lists all contribution-ledger rows in one community for relay-signed
    /// event projection reconciliation.
    pub async fn list_community_contributions(
        &self,
        community_id: CommunityId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ContributionLedgerRecord>> {
        let limit = limit.clamp(1, MAX_LEDGER_PAGE);
        let offset = offset.max(0);
        let rows = sqlx::query(
            "SELECT contribution_id, project_id, contributor_pubkey, status,
                    contribution_units, record, decision, ledger_projection, anchor_state,
                    project_snapshot_id, receipt, created_at, updated_at
               FROM st8_contributions
              WHERE community_id = $1
              ORDER BY created_at, contribution_id LIMIT $2 OFFSET $3",
        )
        .bind(community_id.as_uuid())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(map_ledger_record).collect()
    }

    /// Returns every accepted/adjusted contribution snapshot for deterministic
    /// reconstruction of the next project-wide Merkle snapshot.
    pub async fn list_project_accepted_snapshots(
        &self,
        community_id: CommunityId,
        project_id: &str,
    ) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT contribution_snapshot
               FROM st8_contributions
              WHERE community_id=$1 AND project_id=$2
                AND status IN ('accepted','adjusted')
                AND contribution_snapshot IS NOT NULL
              ORDER BY contribution_id",
        )
        .bind(community_id.as_uuid())
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| row.try_get("contribution_snapshot").map_err(DbError::from))
            .collect()
    }

    /// Claims one due anchor job without blocking other workers.
    pub async fn claim_anchor_job(
        &self,
        worker_id: &str,
        lease_duration: Duration,
    ) -> Result<Option<AnchorJobRecord>> {
        if worker_id.trim().is_empty() {
            return Err(DbError::InvalidData("empty anchor worker ID".into()));
        }
        let lease_seconds = i64::try_from(lease_duration.as_secs())
            .map_err(|_| DbError::InvalidData("anchor lease is too long".into()))?;
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT community_id, snapshot_id, project_id, attempts, prepared_anchor,
                    txid, raw_transaction, atomic_beef, broadcast_receipt
               FROM st8_anchor_jobs
              WHERE ((status IN ('queued','failed','broadcast') AND next_attempt_at <= NOW())
                     OR (status = 'leased' AND lease_until < NOW()))
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
        let snapshot_id: Vec<u8> = row.try_get("snapshot_id")?;
        let attempts: i32 = row.try_get("attempts")?;
        sqlx::query(
            "UPDATE st8_anchor_jobs
                SET status='leased', attempts=attempts+1, lease_owner=$3,
                    lease_until=NOW() + make_interval(secs => $4), updated_at=NOW(),
                    last_error=NULL
              WHERE community_id=$1 AND snapshot_id=$2",
        )
        .bind(community_uuid)
        .bind(&snapshot_id)
        .bind(worker_id)
        .bind(lease_seconds as f64)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(AnchorJobRecord {
            community_id: CommunityId::from_uuid(community_uuid),
            snapshot_id,
            project_id: row.try_get("project_id")?,
            attempts: attempts.saturating_add(1),
            prepared_anchor: row.try_get("prepared_anchor")?,
            txid: row.try_get("txid")?,
            raw_transaction: row.try_get("raw_transaction")?,
            atomic_beef: row.try_get("atomic_beef")?,
            broadcast_receipt: row.try_get("broadcast_receipt")?,
        }))
    }

    /// Saves wallet-signed transaction material under the current lease before
    /// broadcast. A crashed worker therefore resumes the same transaction
    /// instead of asking the wallet to fund another one.
    pub async fn save_anchor_transaction(
        &self,
        job: &AnchorJobRecord,
        worker_id: &str,
        txid: &[u8],
        raw_transaction: &[u8],
        atomic_beef: &[u8],
    ) -> Result<()> {
        if txid.len() != 32 || raw_transaction.is_empty() || atomic_beef.is_empty() {
            return Err(DbError::InvalidData("invalid anchor submission".into()));
        }
        let updated = sqlx::query(
            "UPDATE st8_anchor_jobs
                SET txid=$4, raw_transaction=$5, atomic_beef=$6,
                    updated_at=NOW()
              WHERE community_id=$1 AND snapshot_id=$2
                AND status='leased' AND lease_owner=$3
                AND (txid IS NULL OR (txid=$4 AND raw_transaction=$5 AND atomic_beef=$6))",
        )
        .bind(job.community_id.as_uuid())
        .bind(&job.snapshot_id)
        .bind(worker_id)
        .bind(txid)
        .bind(raw_transaction)
        .bind(atomic_beef)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if updated != 1 {
            return Err(DbError::InvalidData(
                "anchor submission conflicts with stored transaction or lost lease".into(),
            ));
        }
        Ok(())
    }

    /// Saves the ARC submission response for an already persisted signed
    /// transaction. Retries may safely rebroadcast identical bytes if this
    /// response was not committed before a crash.
    pub async fn save_anchor_broadcast(
        &self,
        job: &AnchorJobRecord,
        worker_id: &str,
        broadcast_receipt: &Value,
    ) -> Result<()> {
        let updated = sqlx::query(
            "UPDATE st8_anchor_jobs
                SET broadcast_receipt=$4, updated_at=NOW()
              WHERE community_id=$1 AND snapshot_id=$2
                AND status='leased' AND lease_owner=$3 AND txid IS NOT NULL",
        )
        .bind(job.community_id.as_uuid())
        .bind(&job.snapshot_id)
        .bind(worker_id)
        .bind(broadcast_receipt)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if updated != 1 {
            return Err(DbError::InvalidData(
                "anchor broadcast response was not saved under the active lease".into(),
            ));
        }
        Ok(())
    }

    /// Persists an independently verified receipt and marks every contribution
    /// included in the project snapshot as confirmed.
    #[allow(clippy::too_many_arguments)]
    pub async fn confirm_anchor_job(
        &self,
        job: &AnchorJobRecord,
        worker_id: &str,
        txid: &[u8],
        raw_transaction: &[u8],
        atomic_beef: Option<&[u8]>,
        broadcast_receipt: &Value,
        network_evidence: &Value,
        receipt: &Value,
    ) -> Result<()> {
        if txid.len() != 32 || raw_transaction.is_empty() {
            return Err(DbError::InvalidData("invalid confirmed transaction".into()));
        }
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            "UPDATE st8_anchor_jobs
                SET status='confirmed', lease_owner=NULL, lease_until=NULL, txid=$4,
                    raw_transaction=$5, atomic_beef=$6, broadcast_receipt=$7,
                    network_evidence=$8, receipt=$9, updated_at=NOW(), last_error=NULL
              WHERE community_id=$1 AND snapshot_id=$2 AND status='leased' AND lease_owner=$3",
        )
        .bind(job.community_id.as_uuid())
        .bind(&job.snapshot_id)
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
                "anchor lease was lost before confirmation".into(),
            ));
        }
        sqlx::query(
            "UPDATE st8_contributions
                SET anchor_state='confirmed', receipt=$3, updated_at=NOW()
              WHERE community_id=$1 AND project_snapshot_id=$2",
        )
        .bind(job.community_id.as_uuid())
        .bind(&job.snapshot_id)
        .bind(receipt)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Releases a failed anchor job for retry without changing collaboration state.
    pub async fn fail_anchor_job(
        &self,
        job: &AnchorJobRecord,
        worker_id: &str,
        error: &str,
        retry_after: Duration,
    ) -> Result<()> {
        let retry_seconds = i64::try_from(retry_after.as_secs())
            .map_err(|_| DbError::InvalidData("anchor retry delay is too long".into()))?;
        let updated = sqlx::query(
            "UPDATE st8_anchor_jobs
                SET status='failed', lease_owner=NULL, lease_until=NULL,
                    next_attempt_at=NOW() + make_interval(secs => $4),
                    last_error=left($5, 2048), updated_at=NOW()
              WHERE community_id=$1 AND snapshot_id=$2 AND status='leased' AND lease_owner=$3",
        )
        .bind(job.community_id.as_uuid())
        .bind(&job.snapshot_id)
        .bind(worker_id)
        .bind(retry_seconds as f64)
        .bind(error)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if updated != 1 {
            return Err(DbError::InvalidData(
                "anchor lease was lost before failure release".into(),
            ));
        }
        Ok(())
    }
}

fn validate_entry(entry: &NewContributionLedgerEntry) -> Result<()> {
    if entry.contribution_id.len() != 32
        || entry.contributor_pubkey.len() != 32
        || entry.project_id.trim().is_empty()
        || !matches!(entry.status.as_str(), "accepted" | "adjusted" | "rejected")
        || entry.contribution_units < 0
        || entry.evidence.is_empty()
        || entry.approvals.is_empty()
        || !entry.ledger_projection.is_object()
    {
        return Err(DbError::InvalidData(
            "invalid ST8 contribution ledger entry".into(),
        ));
    }
    let rejected = entry.status == "rejected";
    if rejected != (entry.contribution_units == 0)
        || rejected != entry.project_snapshot.is_none()
        || rejected != entry.contribution_snapshot.is_none()
        || rejected != entry.contribution_snapshot_id.is_none()
    {
        return Err(DbError::InvalidData(
            "ST8 decision/snapshot invariants do not match".into(),
        ));
    }
    if entry
        .contribution_snapshot_id
        .as_ref()
        .is_some_and(|digest| digest.len() != 32)
        || entry.evidence.iter().any(|item| item.event_id.len() != 32)
        || entry
            .approvals
            .iter()
            .any(|item| item.approver_pubkey.len() != 32 || item.event_id.len() != 32)
        || entry.project_snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.snapshot_id.len() != 32
                || snapshot.merkle_root.len() != 32
                || !(1..=4096).contains(&snapshot.leaf_count)
        })
    {
        return Err(DbError::InvalidData("invalid ST8 digest length".into()));
    }
    Ok(())
}

fn map_ledger_record(row: sqlx::postgres::PgRow) -> Result<ContributionLedgerRecord> {
    Ok(ContributionLedgerRecord {
        contribution_id: row.try_get("contribution_id")?,
        project_id: row.try_get("project_id")?,
        contributor_pubkey: row.try_get("contributor_pubkey")?,
        status: row.try_get("status")?,
        contribution_units: row.try_get("contribution_units")?,
        record: row.try_get("record")?,
        decision: row.try_get("decision")?,
        ledger_projection: row.try_get("ledger_projection")?,
        anchor_state: row.try_get("anchor_state")?,
        project_snapshot_id: row.try_get("project_snapshot_id")?,
        receipt: row.try_get("receipt")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}
