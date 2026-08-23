#![deny(unsafe_code)]

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use st8_bsv_provenance::{transaction_id, BroadcastReceipt, SignedAnchorTransaction};
use st8_contribution_engine::{
    ContributionAnchorReceipt, ContributionProposal, GovernanceDecisionIntent,
    PreparedContributionAnchor, KIND_ST8_GOVERNANCE_APPROVAL,
};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Parser)]
#[command(name = "st8wrx-contribution")]
#[command(about = "Sign governance, prepare, finalize, and verify ST8WRX contribution anchors")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Verify evidence and emit the exact governance intent founders must sign.
    Intent {
        /// Contribution proposal JSON; approval_events may be empty.
        #[arg(long)]
        input: PathBuf,
        /// Governance intent and Nostr event template JSON.
        #[arg(long)]
        output: PathBuf,
    },
    /// Verify signed Buzz evidence and emit wallet-ready testnet anchor material.
    Prepare {
        /// Contribution proposal JSON.
        #[arg(long)]
        input: PathBuf,
        /// Prepared anchor JSON.
        #[arg(long)]
        output: PathBuf,
    },
    /// Combine prepared state with wallet/ARC output and persist a verified receipt.
    Finalize {
        /// Prepared anchor JSON.
        #[arg(long)]
        prepared: PathBuf,
        /// External wallet and broadcaster result JSON.
        #[arg(long)]
        result: PathBuf,
        /// Final verified receipt JSON.
        #[arg(long)]
        output: PathBuf,
    },
    /// Independently verify a persisted receipt.
    Verify {
        /// Receipt JSON.
        #[arg(long)]
        receipt: PathBuf,
    },
}

#[derive(Debug, Serialize)]
struct PreparedFile {
    prepared: PreparedContributionAnchor,
    locking_script_hex: String,
}

#[derive(Debug, Serialize)]
struct GovernanceIntentFile {
    intent: GovernanceDecisionIntent,
    digest_hex: String,
    approval_event: ApprovalEventTemplate,
}

#[derive(Debug, Serialize)]
struct ApprovalEventTemplate {
    kind: u16,
    content: String,
    tags: Vec<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ExternalResult {
    raw_transaction_hex: String,
    atomic_beef_hex: Option<String>,
    anchor_output_index: u32,
    broadcast: BroadcastReceipt,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Intent { input, output } => {
            let proposal: ContributionProposal = read_json(&input)?;
            let intent = proposal.governance_intent()?;
            let digest_hex = hex::encode(intent.digest()?);
            let approval_event = ApprovalEventTemplate {
                kind: KIND_ST8_GOVERNANCE_APPROVAL,
                content: digest_hex.clone(),
                tags: vec![
                    vec!["a".into(), intent.project.clone()],
                    vec!["e".into(), hex::encode(intent.contribution_id)],
                    vec!["st8-policy".into(), intent.policy_version.clone()],
                    vec!["st8-decision".into(), digest_hex.clone()],
                ],
            };
            write_json(
                &output,
                &GovernanceIntentFile {
                    intent,
                    digest_hex,
                    approval_event,
                },
            )?;
            println!("intent={}", output.display());
        }
        Command::Prepare { input, output } => {
            let proposal: ContributionProposal = read_json(&input)?;
            let prepared = proposal.prepare()?;
            let locking_script_hex = hex::encode(prepared.anchor_payload.locking_script()?);
            write_json(
                &output,
                &PreparedFile {
                    prepared,
                    locking_script_hex,
                },
            )?;
            println!("prepared={}", output.display());
        }
        Command::Finalize {
            prepared,
            result,
            output,
        } => {
            let prepared_file: PreparedFileIn = read_json(&prepared)?;
            let external: ExternalResult = read_json(&result)?;
            let raw_transaction = hex::decode(external.raw_transaction_hex)?;
            let transaction = SignedAnchorTransaction {
                txid: transaction_id(&raw_transaction),
                raw_transaction,
                atomic_beef: external.atomic_beef_hex.map(hex::decode).transpose()?,
                anchor_output_index: external.anchor_output_index,
            };
            let receipt = prepared_file
                .prepared
                .finalize(transaction, external.broadcast)?;
            receipt.persist_json(&output)?;
            println!("receipt={}", output.display());
        }
        Command::Verify { receipt } => {
            let verified = ContributionAnchorReceipt::load_verified(&receipt)?;
            println!(
                "verified txid={} cu={}",
                hex::encode(verified.transaction.txid),
                verified.snapshot.decision.contribution_units
            );
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct PreparedFileIn {
    prepared: PreparedContributionAnchor,
    #[allow(dead_code)]
    locking_script_hex: String,
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}
