#![deny(unsafe_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context};
use buzz_db::{Db, DbConfig};
use clap::{Parser, Subcommand};
use st8_anchor_worker::{
    finalize_receipt, resume_signed_transaction, AnchorServices, PersistedContributionReceipt,
    DEFAULT_ARC_TESTNET_URL, DEFAULT_WOC_TESTNET_URL,
};
use st8_contribution_engine::PreparedContributionAnchor;

#[derive(Debug, Parser)]
#[command(name = "st8-anchor-worker")]
#[command(about = "Asynchronous external-wallet BSV testnet anchoring and receipt verification")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Claims and processes one queued project snapshot.
    RunOnce {
        /// PostgreSQL URL; defaults to DATABASE_URL.
        #[arg(long)]
        database_url: Option<String>,
        /// External BRC-100 wallet JSON endpoint; defaults to ST8_WALLET_URL.
        #[arg(long)]
        wallet_url: Option<String>,
        /// Testnet ARC base URL.
        #[arg(long, default_value = DEFAULT_ARC_TESTNET_URL)]
        arc_url: String,
        /// Independent WhatsOnChain testnet API base URL.
        #[arg(long, default_value = DEFAULT_WOC_TESTNET_URL)]
        woc_url: String,
        /// Require mined BUMP/header evidence before completing the job.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        require_mined: bool,
        /// Maximum seconds to wait for network/mined observation.
        #[arg(long, default_value_t = 900)]
        observation_timeout_secs: u64,
        /// Optional public receipt JSON export path (DB persistence is always performed).
        #[arg(long)]
        receipt_out: Option<PathBuf>,
    },
    /// Independently verifies a persisted public receipt without wallet access.
    Verify {
        /// Persisted receipt JSON.
        #[arg(long)]
        receipt: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Verify { receipt } => {
            let receipt: PersistedContributionReceipt =
                serde_json::from_slice(&std::fs::read(&receipt)?)?;
            receipt.verify()?;
            println!(
                "verified txid={} state={}",
                hex::encode(receipt.contribution.transaction.txid),
                receipt.network.verification_state
            );
        }
        Command::RunOnce {
            database_url,
            wallet_url,
            arc_url,
            woc_url,
            require_mined,
            observation_timeout_secs,
            receipt_out,
        } => {
            let database_url = database_url
                .or_else(|| std::env::var("DATABASE_URL").ok())
                .context("DATABASE_URL or --database-url is required")?;
            let wallet_url = wallet_url
                .or_else(|| std::env::var("ST8_WALLET_URL").ok())
                .context("ST8_WALLET_URL or --wallet-url is required")?;
            let config = DbConfig {
                database_url,
                max_connections: 2,
                min_connections: 1,
                ..DbConfig::default()
            };
            let db = Db::new(&config).await?;
            let timeout = Duration::from_secs(observation_timeout_secs.clamp(30, 3_600));
            let worker_id = format!(
                "st8-anchor-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp()
            );
            let lease = timeout.saturating_add(Duration::from_secs(120));
            let Some(job) = db.claim_anchor_job(&worker_id, lease).await? else {
                println!("no_anchor_job=true");
                return Ok(());
            };
            let result = process_job(
                &db,
                &job,
                &worker_id,
                &wallet_url,
                &arc_url,
                &woc_url,
                require_mined,
                timeout,
                receipt_out.as_deref(),
            )
            .await;
            if let Err(error) = &result {
                db.fail_anchor_job(
                    &job,
                    &worker_id,
                    &format!("{error:#}"),
                    Duration::from_secs(30),
                )
                .await
                .context("failed to release anchor job after error")?;
            }
            result?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn process_job(
    db: &Db,
    job: &buzz_db::contribution::AnchorJobRecord,
    worker_id: &str,
    wallet_url: &str,
    arc_url: &str,
    woc_url: &str,
    require_mined: bool,
    timeout: Duration,
    receipt_out: Option<&Path>,
) -> anyhow::Result<()> {
    let prepared: PreparedContributionAnchor = serde_json::from_value(job.prepared_anchor.clone())?;
    prepared.verify()?;
    if prepared.project_snapshot.id()?.as_slice() != job.snapshot_id {
        return Err(anyhow!("anchor job snapshot ID mismatch"));
    }
    let services = AnchorServices::new(wallet_url, arc_url, woc_url)?;
    let (transaction, arc_submission) = match (
        &job.txid,
        &job.raw_transaction,
        &job.atomic_beef,
        &job.broadcast_receipt,
    ) {
        (Some(txid), Some(raw), Some(beef), arc) => {
            let transaction =
                resume_signed_transaction(&prepared, txid, raw.clone(), beef.clone())?;
            let arc_submission = match arc {
                Some(arc) => arc.clone(),
                None => {
                    let arc = services.broadcast(&transaction).await?;
                    db.save_anchor_broadcast(job, worker_id, &arc).await?;
                    arc
                }
            };
            (transaction, arc_submission)
        }
        (None, None, None, None) => {
            let transaction = services.create_signed_transaction(&prepared).await?;
            db.save_anchor_transaction(
                job,
                worker_id,
                &transaction.txid,
                &transaction.raw_transaction,
                transaction
                    .atomic_beef
                    .as_deref()
                    .context("wallet transaction is missing Atomic BEEF")?,
            )
            .await?;
            let arc = services.broadcast(&transaction).await?;
            db.save_anchor_broadcast(job, worker_id, &arc).await?;
            (transaction, arc)
        }
        _ => {
            return Err(anyhow!(
                "anchor job has partial persisted submission material"
            ))
        }
    };
    let network = services
        .observe(&transaction, arc_submission.clone(), require_mined, timeout)
        .await?;
    let receipt = finalize_receipt(prepared, transaction, arc_submission.clone(), network)?;
    let receipt_json = serde_json::to_value(&receipt)?;
    db.confirm_anchor_job(
        job,
        worker_id,
        &receipt.contribution.transaction.txid,
        &receipt.contribution.transaction.raw_transaction,
        receipt.contribution.transaction.atomic_beef.as_deref(),
        &arc_submission,
        &serde_json::to_value(&receipt.network)?,
        &receipt_json,
    )
    .await?;
    if let Some(path) = receipt_out {
        persist_receipt(path, &receipt)?;
    }
    println!(
        "txid={} snapshot_id={} state={}",
        hex::encode(receipt.contribution.transaction.txid),
        hex::encode(receipt.contribution.project_snapshot.id()?),
        receipt.network.verification_state
    );
    Ok(())
}

fn persist_receipt(path: &Path, receipt: &PersistedContributionReceipt) -> anyhow::Result<()> {
    receipt.verify()?;
    let parent = path.parent().context("receipt output has no parent")?;
    std::fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("invalid receipt output name")?;
    let temporary = parent.join(format!(".{name}.tmp"));
    std::fs::write(&temporary, serde_json::to_vec_pretty(receipt)?)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}
