//! Operator-only wallet recovery; never clears an ambiguous intent to permit another spend.

use crate::{Db, DbError, Result};
use buzz_core::CommunityId;
use serde_json::Value;
use sqlx::Row;

/// Durable queue whose external wallet action may require operator reconciliation.
#[derive(Clone, Copy)]
pub enum AnchorQueue {
    /// Contribution project snapshot.
    Contribution,
    /// Compute settlement.
    Compute,
}

impl AnchorQueue {
    // Identifiers are closed enum constants; every operator-supplied value is bound.
    fn table(self) -> (&'static str, &'static str) {
        match self {
            Self::Contribution => ("st8_anchor_jobs", "snapshot_id"),
            Self::Compute => ("st8_compute_anchor_jobs", "settlement_id"),
        }
    }
}

/// Exact persisted anchor material loaded by a deployment operator for recovery.
pub struct AnchorRecovery {
    /// Owning community, resolved from the persisted row.
    pub community_id: CommunityId,
    /// Snapshot or settlement digest.
    pub anchor_id: Vec<u8>,
    /// Immutable prepared anchor to validate the recovered transaction against.
    pub prepared_anchor: Value,
}

impl Db {
    /// Durably fences one wallet request before calling the external service.
    /// A second call is rejected even if the first caller crashed or lost its lease.
    pub async fn begin_anchor_wallet_request(
        &self,
        queue: AnchorQueue,
        community: CommunityId,
        id: &[u8],
        worker: &str,
    ) -> Result<()> {
        let (table, key) = queue.table();
        let changed = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {table} SET wallet_requested_at=NOW(), updated_at=NOW()
             WHERE community_id=$1 AND {key}=$2 AND status='leased' AND lease_owner=$3
               AND lease_until>NOW() AND wallet_requested_at IS NULL AND txid IS NULL"
        )))
        .bind(community.as_uuid())
        .bind(id)
        .bind(worker)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(DbError::InvalidData("wallet request already attempted or lease lost; recover the exact external wallet transaction before retrying".into()));
        }
        Ok(())
    }

    /// Lists ambiguous wallet calls across this deployment without exposing wallet secrets.
    pub async fn list_anchor_wallet_recoveries(&self, queue: AnchorQueue) -> Result<Vec<Value>> {
        let (table, key) = queue.table();
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT jsonb_build_object('community_id', community_id, 'anchor_id', encode({key}, 'hex'),
                 'project_id', project_id, 'wallet_requested_at', wallet_requested_at, 'last_error', last_error)
             FROM {table} WHERE wallet_requested_at IS NOT NULL AND txid IS NULL
               AND (status<>'leased' OR lease_until<NOW()) ORDER BY wallet_requested_at LIMIT 1000"
        ))).fetch_all(&self.pool).await?)
    }

    /// Loads an inactive ambiguous request selected explicitly by a deployment operator.
    pub async fn anchor_wallet_recovery(
        &self,
        queue: AnchorQueue,
        community: &str,
        id: &[u8],
    ) -> Result<AnchorRecovery> {
        let community = uuid::Uuid::parse_str(community)
            .map_err(|_| DbError::InvalidData("invalid community UUID".into()))?;
        let (table, key) = queue.table();
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT community_id, prepared_anchor FROM {table} WHERE community_id=$1 AND {key}=$2
             AND wallet_requested_at IS NOT NULL AND txid IS NULL
             AND (status<>'leased' OR lease_until<NOW())"
        )))
        .bind(community)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            DbError::InvalidData(
                "no inactive ambiguous wallet request matches the supplied community and anchor"
                    .into(),
            )
        })?;
        Ok(AnchorRecovery {
            community_id: CommunityId::from_uuid(row.try_get("community_id")?),
            anchor_id: id.to_vec(),
            prepared_anchor: row.try_get("prepared_anchor")?,
        })
    }

    /// Stores already-verified exact transaction bytes and allows observation retries.
    /// Caller must verify the txid, Atomic BEEF, and commitment against `prepared_anchor`.
    pub async fn recover_anchor_wallet_transaction(
        &self,
        queue: AnchorQueue,
        recovery: &AnchorRecovery,
        txid: &[u8],
        raw: &[u8],
        beef: &[u8],
    ) -> Result<()> {
        if txid.len() != 32 || raw.is_empty() || beef.is_empty() {
            return Err(DbError::InvalidData(
                "invalid recovered transaction material".into(),
            ));
        }
        let (table, key) = queue.table();
        let changed = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {table} SET txid=$3, raw_transaction=$4, atomic_beef=$5, status='queued',
               lease_owner=NULL, lease_until=NULL, next_attempt_at=NOW(), last_error=NULL, updated_at=NOW()
             WHERE community_id=$1 AND {key}=$2 AND prepared_anchor=$6
               AND wallet_requested_at IS NOT NULL AND txid IS NULL
               AND (status<>'leased' OR lease_until<NOW())"
        ))).bind(recovery.community_id.as_uuid()).bind(&recovery.anchor_id)
          .bind(txid).bind(raw).bind(beef).bind(&recovery.prepared_anchor)
          .execute(&self.pool).await?.rows_affected();
        if changed != 1 {
            return Err(DbError::InvalidData(
                "wallet recovery conflicts with persisted state or active lease".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    #[ignore = "requires empty disposable BUZZ_ANCHOR_RECOVERY_TEST_DATABASE_URL"]
    async fn wallet_intent_blocks_second_spend_and_recovers_exact_job() {
        let url =
            std::env::var("BUZZ_ANCHOR_RECOVERY_TEST_DATABASE_URL").expect("isolated database URL");
        let pool = sqlx::PgPool::connect(&url).await.unwrap();
        let db = Db::from_pool(pool);
        db.migrate().await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM st8_anchor_jobs)+(SELECT count(*) FROM st8_compute_anchor_jobs)")
            .fetch_one(&db.pool).await.unwrap();
        assert_eq!(count, 0, "use an empty isolated anchor test database");
        let community = db
            .ensure_configured_community(&format!("anchor-{}.test", uuid::Uuid::new_v4()))
            .await
            .unwrap()
            .id;
        let id = [71u8; 32];
        sqlx::query("INSERT INTO st8_project_snapshots (community_id,snapshot_id,project_id,merkle_root,leaf_count,snapshot)
                     VALUES ($1,$2,'project:test',$2,4097,'{}')")
            .bind(community.as_uuid()).bind(id.as_slice()).execute(&db.pool).await.unwrap();
        let persisted_count: i64 = sqlx::query_scalar(
            "SELECT leaf_count FROM st8_project_snapshots WHERE community_id=$1 AND snapshot_id=$2",
        )
        .bind(community.as_uuid())
        .bind(id.as_slice())
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(persisted_count, 4097);
        sqlx::query("INSERT INTO st8_compute_settlements (community_id,settlement_id,project_id,merkle_root,receipt_count,total_sats,settlement_event,settlement,settlement_snapshot,prepared_anchor)
                     VALUES ($1,$2,'project:test',$2,1,0,'{}','{}','{}','{}')")
            .bind(community.as_uuid()).bind(id.as_slice()).execute(&db.pool).await.unwrap();
        for queue in [AnchorQueue::Contribution, AnchorQueue::Compute] {
            let (table, key) = queue.table();
            sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO {table} (community_id,{key},project_id,prepared_anchor) VALUES ($1,$2,'project:test','{{}}')")))
                .bind(community.as_uuid()).bind(id.as_slice()).execute(&db.pool).await.unwrap();
            match queue {
                AnchorQueue::Contribution => {
                    assert!(db
                        .claim_anchor_job("worker", Duration::from_secs(60))
                        .await
                        .unwrap()
                        .is_some());
                }
                AnchorQueue::Compute => {
                    assert!(db
                        .claim_compute_anchor_job("worker", Duration::from_secs(60))
                        .await
                        .unwrap()
                        .is_some());
                }
            }
            db.begin_anchor_wallet_request(queue, community, &id, "worker")
                .await
                .unwrap();
            assert!(db
                .begin_anchor_wallet_request(queue, community, &id, "worker")
                .await
                .is_err());
            assert!(db
                .anchor_wallet_recovery(queue, &community.to_string(), &id)
                .await
                .is_err());
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE {table} SET lease_until=NOW()-interval '1 second' WHERE community_id=$1"
            )))
            .bind(community.as_uuid())
            .execute(&db.pool)
            .await
            .unwrap();
            match queue {
                AnchorQueue::Contribution => {
                    assert!(db
                        .claim_anchor_job("retry", Duration::from_secs(60))
                        .await
                        .unwrap()
                        .is_none());
                }
                AnchorQueue::Compute => {
                    assert!(db
                        .claim_compute_anchor_job("retry", Duration::from_secs(60))
                        .await
                        .unwrap()
                        .is_none());
                }
            }
            assert_eq!(
                db.list_anchor_wallet_recoveries(queue).await.unwrap().len(),
                1
            );
            assert!(db
                .anchor_wallet_recovery(queue, &uuid::Uuid::new_v4().to_string(), &id)
                .await
                .is_err());
            let recovery = db
                .anchor_wallet_recovery(queue, &community.to_string(), &id)
                .await
                .unwrap();
            db.recover_anchor_wallet_transaction(queue, &recovery, &[72; 32], &[1], &[2])
                .await
                .unwrap();
            assert!(db
                .recover_anchor_wallet_transaction(queue, &recovery, &[73; 32], &[1], &[2])
                .await
                .is_err());
            assert!(db
                .list_anchor_wallet_recoveries(queue)
                .await
                .unwrap()
                .is_empty());
            match queue {
                AnchorQueue::Contribution => {
                    assert_eq!(
                        db.claim_anchor_job("retry", Duration::from_secs(60))
                            .await
                            .unwrap()
                            .unwrap()
                            .txid,
                        Some(vec![72; 32])
                    );
                }
                AnchorQueue::Compute => {
                    assert_eq!(
                        db.claim_compute_anchor_job("retry", Duration::from_secs(60))
                            .await
                            .unwrap()
                            .unwrap()
                            .txid,
                        Some(vec![72; 32])
                    );
                }
            }
            assert!(db
                .begin_anchor_wallet_request(queue, community, &id, "retry")
                .await
                .is_err());
        }
        // Delete fenced children while their community still exists.
        for statement in [
            "DELETE FROM st8_anchor_jobs WHERE community_id=$1",
            "DELETE FROM st8_compute_anchor_jobs WHERE community_id=$1",
            "DELETE FROM st8_project_snapshots WHERE community_id=$1",
            "DELETE FROM st8_compute_settlements WHERE community_id=$1",
        ] {
            sqlx::query(statement)
                .bind(community.as_uuid())
                .execute(&db.pool)
                .await
                .unwrap();
        }
        sqlx::query("DELETE FROM communities WHERE id=$1")
            .bind(community.as_uuid())
            .execute(&db.pool)
            .await
            .unwrap();
    }
}
