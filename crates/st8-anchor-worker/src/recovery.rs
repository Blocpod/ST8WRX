use super::*;
use st8_bsv_provenance::SignedAnchorTransaction;

async fn connect(database_url: Option<String>) -> anyhow::Result<Db> {
    Db::new(&DbConfig {
        database_url: database_url
            .or_else(|| std::env::var("DATABASE_URL").ok())
            .context("DATABASE_URL or --database-url is required")?,
        max_connections: 2,
        min_connections: 1,
        ..DbConfig::default()
    })
    .await
    .map_err(Into::into)
}

pub(super) async fn list(database_url: Option<String>) -> anyhow::Result<()> {
    let db = connect(database_url).await?;
    println!(
        "{}",
        serde_json::json!({
            "contribution": db.list_anchor_wallet_recoveries(AnchorQueue::Contribution).await?,
            "compute": db.list_anchor_wallet_recoveries(AnchorQueue::Compute).await?,
        })
    );
    Ok(())
}

pub(super) async fn recover(
    database_url: Option<String>,
    queue: &str,
    community: &str,
    anchor: &str,
    transaction: &Path,
) -> anyhow::Result<()> {
    let queue = match queue {
        "contribution" => AnchorQueue::Contribution,
        "compute" => AnchorQueue::Compute,
        _ => return Err(anyhow!("unknown anchor queue")),
    };
    let id = hex::decode(anchor)?;
    if id.len() != 32 {
        return Err(anyhow!("anchor ID must contain exactly 32 bytes"));
    }
    let db = connect(database_url).await?;
    let recovery = db.anchor_wallet_recovery(queue, community, &id).await?;
    let payload = match queue {
        AnchorQueue::Contribution => {
            let prepared: PreparedContributionAnchor =
                serde_json::from_value(recovery.prepared_anchor.clone())?;
            prepared.verify()?;
            if prepared.project_snapshot.id()?.as_slice() != id {
                return Err(anyhow!("snapshot ID mismatch"));
            }
            prepared.anchor_payload
        }
        AnchorQueue::Compute => {
            let prepared: PreparedComputeSettlementAnchor =
                serde_json::from_value(recovery.prepared_anchor.clone())?;
            prepared.verify()?;
            if prepared.settlement.settlement.id()?.as_slice() != id {
                return Err(anyhow!("settlement ID mismatch"));
            }
            prepared.anchor_payload
        }
    };
    let supplied: SignedAnchorTransaction = serde_json::from_slice(&std::fs::read(transaction)?)?;
    let checked = resume_signed_anchor_transaction(
        &payload,
        &supplied.txid,
        supplied.raw_transaction,
        supplied
            .atomic_beef
            .context("recovery requires Atomic BEEF")?,
    )?;
    db.recover_anchor_wallet_transaction(
        queue,
        &recovery,
        &checked.txid,
        &checked.raw_transaction,
        checked
            .atomic_beef
            .as_deref()
            .context("missing verified BEEF")?,
    )
    .await?;
    println!(
        "recovered anchor_id={} txid={} next_action=observe_existing_transaction",
        hex::encode(id),
        hex::encode(checked.txid)
    );
    Ok(())
}
