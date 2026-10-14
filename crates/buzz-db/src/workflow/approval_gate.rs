//! Atomic persistence of a suspended workflow and its approval gate.
use super::*;

/// Persist a pending approval and its run checkpoint in one transaction.
/// Returns the stored token hash, which signed approval commands reference.
pub async fn suspend_approval(
    db: &crate::Db,
    params: CreateApprovalParams<'_>,
    trace: &serde_json::Value,
) -> Result<Vec<u8>> {
    let token_hash = hash_approval_token(params.token);
    let mut tx = db.begin_transaction().await?;
    let changed = sqlx::query("UPDATE workflow_runs SET status = 'waiting_approval', current_step = $1, execution_trace = $2 WHERE community_id = $3 AND id = $4 AND workflow_id = $5 AND status = 'running'")
        .bind(params.step_index).bind(trace).bind(params.community_id.as_uuid()).bind(params.run_id).bind(params.workflow_id)
        .execute(&mut *tx).await?.rows_affected();
    if changed != 1 {
        return Err(DbError::InvalidData("workflow is no longer running".into()));
    }
    sqlx::query("INSERT INTO workflow_approvals (community_id, token, workflow_id, run_id, step_id, step_index, approver_spec, status, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending', $8)")
        .bind(params.community_id.as_uuid()).bind(&token_hash).bind(params.workflow_id).bind(params.run_id).bind(params.step_id).bind(params.step_index).bind(params.approver_spec).bind(params.expires_at)
        .execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(token_hash)
}

/// Find durable approval decisions (including timeouts) not yet applied to runs.
pub async fn pending_approval_transitions(db: &crate::Db) -> Result<Vec<(CommunityId, Uuid)>> {
    let rows = sqlx::query("SELECT r.community_id, r.id FROM workflow_runs r JOIN workflow_approvals a ON a.community_id = r.community_id AND a.run_id = r.id AND a.step_index = r.current_step WHERE r.status = 'waiting_approval' AND (a.status <> 'pending' OR a.expires_at <= NOW() OR r.execution_trace -> -1 ->> 'notification_event_id' IS NULL) ORDER BY (a.status = 'pending' AND a.expires_at > NOW()), r.created_at LIMIT 100")
        .fetch_all(&db.pool).await?;
    rows.into_iter()
        .map(|row| {
            Ok((
                CommunityId::from_uuid(row.try_get("community_id")?),
                row.try_get("id")?,
            ))
        })
        .collect()
}

/// Serialize one approval transition across relay processes without losing it on crash.
pub async fn lock_approval_transition(
    db: &crate::Db,
    community: CommunityId,
    run: Uuid,
) -> Result<Option<sqlx::Transaction<'static, sqlx::Postgres>>> {
    let mut tx = db.begin_transaction().await?;
    let key = format!("workflow-approval:{community}:{run}");
    let acquired: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(key)
            .fetch_one(&mut *tx)
            .await?;
    Ok(if acquired { Some(tx) } else { None })
}

/// Retain notification delivery in the pending gate checkpoint without changing run state.
pub async fn mark_approval_notified(
    db: &crate::Db,
    community: CommunityId,
    run: Uuid,
    step: i32,
    event_id: &str,
) -> Result<()> {
    sqlx::query("UPDATE workflow_runs SET execution_trace = jsonb_set(execution_trace, '{-1,notification_event_id}', to_jsonb($1::text)) WHERE community_id=$2 AND id=$3 AND current_step=$4 AND status='waiting_approval'")
        .bind(event_id).bind(community.as_uuid()).bind(run).bind(step).execute(&db.pool).await?;
    Ok(())
}

/// Atomically cancel a waiting run and expire its pending gate when applicable.
pub async fn cancel_approval_transition(
    db: &crate::Db,
    community: CommunityId,
    run: Uuid,
    step: i32,
    token: &[u8],
    expired: bool,
) -> Result<()> {
    let mut tx = db.begin_transaction().await?;
    let allowed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_approvals WHERE community_id=$1 AND run_id=$2 AND token=$3 AND (status='denied' OR status='expired' OR (status='pending' AND expires_at<=NOW())))")
        .bind(community.as_uuid()).bind(run).bind(token).fetch_one(&mut *tx).await?;
    if !allowed {
        return Ok(());
    }
    sqlx::query("UPDATE workflow_approvals SET status='expired' WHERE community_id=$1 AND token=$2 AND status='pending' AND expires_at<=NOW()")
        .bind(community.as_uuid()).bind(token).execute(&mut *tx).await?;
    let reason = if expired {
        "approval_expired"
    } else {
        "approval_denied"
    };
    sqlx::query("UPDATE workflow_runs SET status='cancelled', completed_at=NOW(), error_code=$1, error_message=$1 WHERE community_id=$2 AND id=$3 AND current_step=$4 AND status='waiting_approval'")
        .bind(reason).bind(community.as_uuid()).bind(run).bind(step).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Save a human decision in the same transaction as its signed command event.
pub async fn decide_approval_in_transaction(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    community: CommunityId,
    token: &[u8],
    granted: bool,
    actor: &[u8],
    note: Option<&str>,
) -> Result<bool> {
    let status = if granted { "granted" } else { "denied" };
    let result = sqlx::query("UPDATE workflow_approvals a SET status=$1::approval_status, approver_pubkey=$2, note=$3, granted_at=CASE WHEN $4 THEN NOW() ELSE granted_at END, denied_at=CASE WHEN $4 THEN denied_at ELSE NOW() END WHERE a.community_id=$5 AND a.token=$6 AND a.status='pending' AND a.expires_at>NOW() AND EXISTS(SELECT 1 FROM workflow_runs r WHERE r.community_id=a.community_id AND r.id=a.run_id AND r.status='waiting_approval' AND r.current_step=a.step_index)")
        .bind(status).bind(actor).bind(note).bind(granted).bind(community.as_uuid()).bind(token).execute(&mut **tx).await?;
    Ok(result.rows_affected() == 1)
}
