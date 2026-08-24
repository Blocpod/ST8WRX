#![deny(unsafe_code)]
#![warn(missing_docs)]
//! External-wallet BSV testnet anchoring and independent receipt verification.
//!
//! This crate is BSV-only and uses the Open BSV licensed official SDK. It never
//! accepts or reads private keys, WIFs, mnemonics, or seed phrases.

use std::io::Cursor;
use std::time::Duration;

use anyhow::{anyhow, Context};
use bsv::transaction::beef::Beef;
use bsv::transaction::merkle_path::MerklePath;
use bsv::wallet::interfaces::{CreateActionArgs, CreateActionOptions, CreateActionOutput, Network};
use bsv::wallet::substrates::http_wallet_json::HttpWalletJson;
use bsv::wallet::types::{BooleanDefaultFalse, BooleanDefaultTrue};
use bsv::wallet::WalletInterface;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use st8_bsv_provenance::{
    transaction_id, verify_anchor_transaction, AnchorPayload, BroadcastReceipt, BsvNetwork,
    SignedAnchorTransaction,
};
use st8_compute_engine::{ComputeSettlementAnchorReceipt, PreparedComputeSettlementAnchor};
use st8_contribution_engine::{ContributionAnchorReceipt, PreparedContributionAnchor};

/// Default official TAAL ARC testnet endpoint.
pub const DEFAULT_ARC_TESTNET_URL: &str = "https://arc-test.taal.com/v1";
/// Default independent BSV testnet explorer API.
pub const DEFAULT_WOC_TESTNET_URL: &str = "https://api.whatsonchain.com/v1/bsv/test";

/// Public network observations persisted with a contribution receipt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BsvNetworkEvidence {
    /// Observation time.
    pub observed_at: DateTime<Utc>,
    /// Raw transaction independently returned by WhatsOnChain.
    pub woc_raw_transaction_hex: String,
    /// Mined BEEF/BUMP independently returned by WhatsOnChain, when available.
    pub woc_beef_hex: Option<String>,
    /// Independent WhatsOnChain transaction details.
    pub woc_transaction: Value,
    /// Broadcast-provider submission response, normalized to ARC status fields.
    pub arc_submission: Value,
    /// Latest independently fetched network status, normalized to ARC fields.
    pub arc_status: Value,
    /// WhatsOnChain block details used to verify the header and Merkle root.
    pub woc_block: Option<Value>,
    /// Recomputed state: `seen_on_testnet` or `mined_spv_verified`.
    pub verification_state: String,
}

/// Complete receipt consumed by a process independent from its producer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedContributionReceipt {
    /// Wrapper schema version.
    pub version: u16,
    /// Complete signed evidence/governance/commitment/transaction receipt.
    pub contribution: ContributionAnchorReceipt,
    /// Independently fetched public BSV testnet evidence.
    pub network: BsvNetworkEvidence,
}

impl PersistedContributionReceipt {
    /// Verifies all signed contribution state, Atomic BEEF, raw transaction,
    /// network observations, and available mined SPV/header evidence offline.
    pub fn verify(&self) -> anyhow::Result<()> {
        if self.version != 1 {
            return Err(anyhow!("unsupported persisted receipt version"));
        }
        self.contribution.verify()?;
        if self.contribution.anchor_payload.network != BsvNetwork::Testnet {
            return Err(anyhow!("receipt is not a BSV testnet anchor"));
        }
        verify_transaction_network(&self.contribution.transaction, &self.network)
    }
}

/// Complete independently verifiable ST8 Compute settlement receipt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedComputeSettlementReceipt {
    /// Wrapper schema version.
    pub version: u16,
    /// Signed compute receipts, settlement, commitment, and wallet transaction.
    pub compute: ComputeSettlementAnchorReceipt,
    /// Independently fetched public BSV testnet evidence.
    pub network: BsvNetworkEvidence,
}

impl PersistedComputeSettlementReceipt {
    /// Verifies compute provenance, settlement math, exact anchor bytes, and network/SPV state.
    pub fn verify(&self) -> anyhow::Result<()> {
        if self.version != 1 {
            return Err(anyhow!("unsupported persisted compute receipt version"));
        }
        self.compute.verify()?;
        verify_transaction_network(&self.compute.transaction, &self.network)
    }
}

fn verify_transaction_network(
    transaction: &SignedAnchorTransaction,
    network: &BsvNetworkEvidence,
) -> anyhow::Result<()> {
    let raw_hex = hex::encode(&transaction.raw_transaction);
    if !raw_hex.eq_ignore_ascii_case(&network.woc_raw_transaction_hex) {
        return Err(anyhow!("independent raw transaction mismatch"));
    }
    let txid_hex = hex::encode(transaction.txid);
    verify_json_txid(&network.woc_transaction, &txid_hex)?;
    verify_json_txid(&network.arc_submission, &txid_hex)?;
    verify_json_txid(&network.arc_status, &txid_hex)?;
    let beef_bytes = transaction
        .atomic_beef
        .as_ref()
        .context("receipt is missing Atomic BEEF")?;
    let beef = Beef::from_binary(&mut Cursor::new(beef_bytes)).context("invalid Atomic BEEF")?;
    let beef_transaction = beef
        .into_transaction()
        .context("Atomic BEEF has no subject tx")?;
    if beef_transaction.to_bytes()? != transaction.raw_transaction
        || !beef_transaction.id()?.eq_ignore_ascii_case(&txid_hex)
    {
        return Err(anyhow!("Atomic BEEF subject transaction mismatch"));
    }
    let arc_state = json_string(&network.arc_status, "txStatus")?;
    let derived_state = if arc_state == "MINED" {
        let woc_beef_hex = network
            .woc_beef_hex
            .as_deref()
            .context("mined receipt is missing WhatsOnChain BEEF")?;
        let woc_path = verified_woc_beef_path(transaction, woc_beef_hex)?;
        if !woc_path
            .to_hex()?
            .eq_ignore_ascii_case(json_string(&network.arc_status, "merklePath")?)
        {
            return Err(anyhow!("persisted mined BEEF path mismatch"));
        }
        verify_mined_spv(
            &txid_hex,
            &network.arc_status,
            &network.woc_transaction,
            network
                .woc_block
                .as_ref()
                .context("mined receipt is missing block evidence")?,
        )?;
        "mined_spv_verified"
    } else if matches!(
        arc_state,
        "ANNOUNCED_TO_NETWORK"
            | "REQUESTED_BY_NETWORK"
            | "SENT_TO_NETWORK"
            | "ACCEPTED_BY_NETWORK"
            | "SEEN_ON_NETWORK"
    ) {
        "seen_on_testnet"
    } else {
        return Err(anyhow!("ARC has not proved testnet network acceptance"));
    };
    if network.verification_state != derived_state {
        return Err(anyhow!("stored BSV verification state was not derived"));
    }
    Ok(())
}

/// HTTP clients and endpoints for one anchor worker.
pub struct AnchorServices {
    wallet: Option<HttpWalletJson>,
    client: Client,
    arc_url: String,
    woc_url: String,
}

impl AnchorServices {
    /// Creates services using an external BRC-100 wallet and public testnet endpoints.
    pub fn new(wallet_url: &str, arc_url: &str, woc_url: &str) -> anyhow::Result<Self> {
        if wallet_url.trim().is_empty() {
            return Err(anyhow!("wallet endpoint is required"));
        }
        let mut services = Self::new_network_only(arc_url, woc_url)?;
        services.wallet = Some(HttpWalletJson::new("st8wrx.network", wallet_url));
        Ok(services)
    }

    /// Creates public-network-only services for refreshing or verifying a signed receipt.
    pub fn new_network_only(arc_url: &str, woc_url: &str) -> anyhow::Result<Self> {
        if !arc_url.starts_with("https://") || !woc_url.starts_with("https://") {
            return Err(anyhow!("HTTPS network endpoints are required"));
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("ST8WRX-Milestone1/1")
            .build()?;
        Ok(Self {
            wallet: None,
            client,
            arc_url: arc_url.trim_end_matches('/').to_owned(),
            woc_url: woc_url.trim_end_matches('/').to_owned(),
        })
    }

    /// Asks the external wallet to fund, sign, and synchronously broadcast the exact anchor.
    ///
    /// The wallet must process its own spend rather than returning a `noSend`
    /// action. Otherwise an external broadcast can leave the wallet's durable
    /// UTXO state unaware of the spend, allowing the same input to be selected
    /// again after a restart. The worker still independently submits/observes
    /// the returned bytes and persists the public network proof.
    pub async fn create_signed_transaction(
        &self,
        prepared: &PreparedContributionAnchor,
    ) -> anyhow::Result<SignedAnchorTransaction> {
        prepared.verify()?;
        self.create_signed_anchor_transaction(
            &prepared.anchor_payload,
            "Anchor ST8WRX project snapshot",
            "ST8WRX project commitment",
            "st8wrx-milestone-1",
            &format!("st8wrx:{}", hex::encode(prepared.project_snapshot.id()?)),
        )
        .await
    }

    /// Asks the external wallet to sign the exact deterministic anchor payload.
    pub async fn create_signed_anchor_transaction(
        &self,
        payload: &AnchorPayload,
        description: &str,
        output_description: &str,
        label: &str,
        reference: &str,
    ) -> anyhow::Result<SignedAnchorTransaction> {
        if payload.network != BsvNetwork::Testnet {
            return Err(anyhow!("ST8WRX wallet request must target testnet"));
        }
        let wallet = self
            .wallet
            .as_ref()
            .context("external wallet is unavailable in network-only mode")?;
        let wallet_network = wallet
            .get_network(Some("st8wrx.network"))
            .await
            .context("external BRC-100 wallet getNetwork failed")?;
        if wallet_network.network != Network::Testnet {
            return Err(anyhow!("external wallet is not configured for BSV testnet"));
        }
        let locking_script = payload.locking_script()?;
        let result = wallet
            .create_action(
                CreateActionArgs {
                    description: description.into(),
                    input_beef: None,
                    inputs: Vec::new(),
                    outputs: vec![CreateActionOutput {
                        locking_script: Some(locking_script.clone()),
                        satoshis: 0,
                        output_description: output_description.into(),
                        basket: None,
                        custom_instructions: None,
                        tags: vec!["st8wrx-anchor".into()],
                    }],
                    lock_time: None,
                    version: None,
                    labels: vec![label.into()],
                    options: Some(wallet_action_options()),
                    reference: Some(reference.into()),
                },
                Some("st8wrx.network"),
            )
            .await
            .context("external BRC-100 wallet createAction failed")?;
        if result.signable_transaction.is_some() {
            return Err(anyhow!(
                "wallet returned a partial transaction; wallet-managed inputs must be fully signed"
            ));
        }
        let atomic_beef = result.tx.context("wallet did not return Atomic BEEF")?;
        let beef = Beef::from_binary(&mut Cursor::new(&atomic_beef))
            .context("wallet returned malformed Atomic BEEF")?;
        let transaction = beef
            .into_transaction()
            .context("wallet BEEF has no subject tx")?;
        let raw_transaction = transaction.to_bytes()?;
        let txid = transaction_id(&raw_transaction);
        let txid_hex = hex::encode(txid);
        if result
            .txid
            .as_ref()
            .is_some_and(|wallet_txid| !wallet_txid.eq_ignore_ascii_case(&txid_hex))
        {
            return Err(anyhow!("wallet txid does not match signed bytes"));
        }
        let anchor_output_index = transaction
            .outputs
            .iter()
            .position(|output| output.locking_script.to_binary() == locking_script)
            .context("wallet transaction omitted the exact ST8WRX anchor output")?;
        let signed = SignedAnchorTransaction {
            raw_transaction,
            atomic_beef: Some(atomic_beef),
            txid,
            anchor_output_index: u32::try_from(anchor_output_index)?,
        };
        verify_anchor_transaction(&signed, payload)?;
        Ok(signed)
    }

    /// Submits exact signed bytes to testnet ARC, falling back to the official
    /// WhatsOnChain testnet broadcaster when ARC requires unavailable credentials.
    pub async fn broadcast(&self, transaction: &SignedAnchorTransaction) -> anyhow::Result<Value> {
        let expected_txid = hex::encode(transaction.txid);
        let response = self
            .client
            .post(format!("{}/tx", self.arc_url))
            .header("X-WaitFor", "SEEN_ON_NETWORK")
            .json(&serde_json::json!({"rawTx": hex::encode(&transaction.raw_transaction)}))
            .send()
            .await
            .context("ARC testnet submission failed")?;
        let status = response.status();
        let bytes = response.bytes().await?;
        let body: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("ARC returned non-JSON status {status}"))?;
        if status.is_success() {
            verify_json_txid(&body, &expected_txid)?;
            return Ok(with_provider(body, "TAAL ARC testnet"));
        }
        self.broadcast_with_woc(transaction, status.as_u16(), body)
            .await
    }

    /// Polls ARC plus independent WhatsOnChain data until network-seen or mined.
    pub async fn observe(
        &self,
        transaction: &SignedAnchorTransaction,
        arc_submission: Value,
        require_mined: bool,
        timeout: Duration,
    ) -> anyhow::Result<BsvNetworkEvidence> {
        let txid = hex::encode(transaction.txid);
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let arc_status = self.get_json(format!("{}/tx/{txid}", self.arc_url)).await;
            let raw = self
                .client
                .get(format!("{}/tx/{txid}/hex", self.woc_url))
                .send()
                .await;
            let details = self
                .get_json(format!("{}/tx/hash/{txid}", self.woc_url))
                .await;
            if let (Ok(raw), Ok(woc_transaction)) = (raw, details) {
                if raw.status().is_success() {
                    let woc_raw_transaction_hex = raw.text().await?.trim().to_owned();
                    if hex::decode(&woc_raw_transaction_hex)? != transaction.raw_transaction {
                        return Err(anyhow!("WhatsOnChain returned different transaction bytes"));
                    }
                    let confirmations = json_u64(&woc_transaction, "confirmations").unwrap_or(0);
                    let (arc_status, woc_beef_hex) = if confirmations > 0 {
                        let beef_response = self
                            .client
                            .get(format!("{}/tx/{txid}/beef", self.woc_url))
                            .send()
                            .await?;
                        if beef_response.status().is_success() {
                            let beef_hex = beef_response.text().await?.trim().to_owned();
                            let status =
                                woc_mined_status(transaction, &txid, &woc_transaction, &beef_hex)?;
                            (status, Some(beef_hex))
                        } else {
                            (woc_seen_status(&txid, &woc_transaction), None)
                        }
                    } else {
                        match arc_status {
                            Ok(value) if verify_json_txid(&value, &txid).is_ok() => (value, None),
                            _ => (woc_seen_status(&txid, &woc_transaction), None),
                        }
                    };
                    let status = json_string(&arc_status, "txStatus")?.to_owned();
                    let network_seen = matches!(
                        status.as_str(),
                        "ANNOUNCED_TO_NETWORK"
                            | "REQUESTED_BY_NETWORK"
                            | "SENT_TO_NETWORK"
                            | "ACCEPTED_BY_NETWORK"
                            | "SEEN_ON_NETWORK"
                            | "MINED"
                    );
                    if network_seen && (!require_mined || status == "MINED") {
                        let woc_block = if status == "MINED" {
                            let block_hash = json_string(&arc_status, "blockHash")?;
                            Some(
                                self.get_json(format!("{}/block/hash/{block_hash}", self.woc_url))
                                    .await?,
                            )
                        } else {
                            None
                        };
                        let evidence = BsvNetworkEvidence {
                            observed_at: Utc::now(),
                            woc_raw_transaction_hex,
                            woc_beef_hex,
                            woc_transaction,
                            arc_submission,
                            arc_status,
                            woc_block,
                            verification_state: if status == "MINED" {
                                "mined_spv_verified".into()
                            } else {
                                "seen_on_testnet".into()
                            },
                        };
                        verify_network_material(transaction, &evidence)?;
                        return Ok(evidence);
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(anyhow!("timed out waiting for BSV testnet observation"));
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    async fn get_json(&self, url: String) -> anyhow::Result<Value> {
        let response = self.client.get(url).send().await?;
        let status = response.status();
        let value: Value = response.json().await?;
        if !status.is_success() {
            return Err(anyhow!("network API returned {status}: {value}"));
        }
        Ok(sanitize_json_for_postgres(value))
    }

    async fn broadcast_with_woc(
        &self,
        transaction: &SignedAnchorTransaction,
        arc_status: u16,
        arc_error: Value,
    ) -> anyhow::Result<Value> {
        let expected_txid = hex::encode(transaction.txid);
        if let Ok(existing) = self
            .get_json(format!("{}/tx/hash/{expected_txid}", self.woc_url))
            .await
        {
            verify_json_txid(&existing, &expected_txid)?;
            return Ok(woc_submission(
                &expected_txid,
                true,
                arc_status,
                arc_error,
                existing,
            ));
        }
        let response = self
            .client
            .post(format!("{}/tx/raw", self.woc_url))
            .json(&serde_json::json!({
                "txhex": hex::encode(&transaction.raw_transaction)
            }))
            .send()
            .await
            .context("WhatsOnChain testnet submission failed")?;
        let status = response.status();
        let response_text = response.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "ARC rejected transaction ({arc_status}): {arc_error}; WhatsOnChain rejected transaction ({status}): {response_text}"
            ));
        }
        let observed_txid = response_text.trim().trim_matches('"');
        if !observed_txid.eq_ignore_ascii_case(&expected_txid) {
            return Err(anyhow!("WhatsOnChain broadcast txid mismatch"));
        }
        Ok(woc_submission(
            &expected_txid,
            false,
            arc_status,
            arc_error,
            Value::String(response_text),
        ))
    }
}

fn wallet_action_options() -> CreateActionOptions {
    CreateActionOptions {
        // Leave the default implicit: external wallets reserve an explicit
        // `true` for their administrative originator, while still defaulting
        // ordinary application requests to wallet-managed signing.
        sign_and_process: BooleanDefaultTrue(None),
        // A synchronous wallet broadcast both surfaces rejection immediately
        // and durably retires the selected wallet inputs before this call
        // returns. The worker then independently observes the same bytes.
        accept_delayed_broadcast: BooleanDefaultTrue(Some(false)),
        trust_self: None,
        known_txids: Vec::new(),
        return_txid_only: BooleanDefaultFalse(Some(false)),
        no_send: BooleanDefaultFalse(Some(false)),
        no_send_change: Vec::new(),
        send_with: Vec::new(),
        randomize_outputs: BooleanDefaultTrue(Some(false)),
    }
}

/// Reconstructs and verifies a previously persisted wallet transaction so a
/// retried job never asks the wallet to fund a second transaction.
pub fn resume_signed_transaction(
    prepared: &PreparedContributionAnchor,
    txid: &[u8],
    raw_transaction: Vec<u8>,
    atomic_beef: Vec<u8>,
) -> anyhow::Result<SignedAnchorTransaction> {
    resume_signed_anchor_transaction(&prepared.anchor_payload, txid, raw_transaction, atomic_beef)
}

/// Reconstructs a persisted signed transaction for any exact ST8WRX anchor payload.
pub fn resume_signed_anchor_transaction(
    payload: &AnchorPayload,
    txid: &[u8],
    raw_transaction: Vec<u8>,
    atomic_beef: Vec<u8>,
) -> anyhow::Result<SignedAnchorTransaction> {
    let txid: [u8; 32] = txid
        .try_into()
        .map_err(|_| anyhow!("stored txid is not 32 bytes"))?;
    if transaction_id(&raw_transaction) != txid {
        return Err(anyhow!("stored transaction ID mismatch"));
    }
    let beef = Beef::from_binary(&mut Cursor::new(&atomic_beef))?;
    let subject = beef.into_transaction()?;
    if subject.to_bytes()? != raw_transaction {
        return Err(anyhow!("stored Atomic BEEF subject mismatch"));
    }
    let expected_script = payload.locking_script()?;
    let anchor_output_index = subject
        .outputs
        .iter()
        .position(|output| output.locking_script.to_binary() == expected_script)
        .context("stored transaction omits the ST8WRX anchor")?;
    let transaction = SignedAnchorTransaction {
        raw_transaction,
        atomic_beef: Some(atomic_beef),
        txid,
        anchor_output_index: u32::try_from(anchor_output_index)?,
    };
    verify_anchor_transaction(&transaction, payload)?;
    Ok(transaction)
}

/// Builds and verifies the complete wrapper after public observation.
pub fn finalize_receipt(
    prepared: PreparedContributionAnchor,
    transaction: SignedAnchorTransaction,
    arc_submission: Value,
    network: BsvNetworkEvidence,
) -> anyhow::Result<PersistedContributionReceipt> {
    let status = json_string(&network.arc_status, "txStatus")?.to_owned();
    let provider = network
        .arc_submission
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("TAAL ARC testnet")
        .to_owned();
    let contribution = prepared.finalize(
        transaction,
        BroadcastReceipt {
            accepted: true,
            status,
            provider,
        },
    )?;
    if network.arc_submission != arc_submission {
        return Err(anyhow!("ARC submission evidence changed"));
    }
    let receipt = PersistedContributionReceipt {
        version: 1,
        contribution,
        network,
    };
    receipt.verify()?;
    Ok(receipt)
}

/// Builds and verifies a persisted compute settlement wrapper after observation.
pub fn finalize_compute_receipt(
    prepared: PreparedComputeSettlementAnchor,
    transaction: SignedAnchorTransaction,
    arc_submission: Value,
    network: BsvNetworkEvidence,
) -> anyhow::Result<PersistedComputeSettlementReceipt> {
    let status = json_string(&network.arc_status, "txStatus")?.to_owned();
    let provider = network
        .arc_submission
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("TAAL ARC testnet")
        .to_owned();
    let compute = prepared.finalize(
        transaction,
        BroadcastReceipt {
            accepted: true,
            status,
            provider,
        },
    )?;
    if network.arc_submission != arc_submission {
        return Err(anyhow!("ARC submission evidence changed"));
    }
    let receipt = PersistedComputeSettlementReceipt {
        version: 1,
        compute,
        network,
    };
    receipt.verify()?;
    Ok(receipt)
}

fn with_provider(mut response: Value, provider: &str) -> Value {
    if let Some(object) = response.as_object_mut() {
        object.insert("provider".into(), Value::String(provider.into()));
    }
    response
}

fn woc_submission(
    txid: &str,
    already_known: bool,
    arc_status: u16,
    arc_error: Value,
    response: Value,
) -> Value {
    serde_json::json!({
        "txid": txid,
        "txStatus": "SEEN_ON_NETWORK",
        "provider": "WhatsOnChain testnet",
        "alreadyKnown": already_known,
        "arcAttempt": {"status": arc_status, "response": arc_error},
        "providerResponse": response,
    })
}

fn woc_seen_status(txid: &str, transaction: &Value) -> Value {
    serde_json::json!({
        "txid": txid,
        "txStatus": "SEEN_ON_NETWORK",
        "provider": "WhatsOnChain testnet",
        "confirmations": transaction.get("confirmations").cloned().unwrap_or(Value::from(0)),
        "blockHash": transaction.get("blockhash").cloned().unwrap_or(Value::Null),
        "blockHeight": transaction.get("blockheight").cloned().unwrap_or(Value::Null),
    })
}

fn woc_mined_status(
    transaction: &SignedAnchorTransaction,
    txid: &str,
    details: &Value,
    beef_hex: &str,
) -> anyhow::Result<Value> {
    let path = verified_woc_beef_path(transaction, beef_hex)?;
    let block_height = json_u64(details, "blockheight")?;
    if u64::from(path.block_height) != block_height {
        return Err(anyhow!("WhatsOnChain BEEF block height mismatch"));
    }
    Ok(serde_json::json!({
        "txid": txid,
        "txStatus": "MINED",
        "provider": "WhatsOnChain testnet BEEF",
        "merklePath": path.to_hex()?,
        "blockHash": json_string(details, "blockhash")?,
        "blockHeight": block_height,
    }))
}

fn verified_woc_beef_path(
    transaction: &SignedAnchorTransaction,
    beef_hex: &str,
) -> anyhow::Result<MerklePath> {
    let beef = Beef::from_hex(beef_hex).context("invalid WhatsOnChain BEEF")?;
    let expected_txid = hex::encode(transaction.txid);
    let beef_tx = beef
        .txs
        .iter()
        .find(|candidate| candidate.txid.eq_ignore_ascii_case(&expected_txid))
        .context("WhatsOnChain BEEF omits subject transaction")?;
    let raw = beef_tx
        .tx
        .as_ref()
        .context("WhatsOnChain BEEF subject is txid-only")?
        .to_bytes()?;
    if raw != transaction.raw_transaction {
        return Err(anyhow!("WhatsOnChain BEEF subject bytes mismatch"));
    }
    beef.bumps
        .get(
            beef_tx
                .bump_index
                .context("WhatsOnChain BEEF subject has no BUMP")?,
        )
        .cloned()
        .context("WhatsOnChain BEEF references a missing BUMP")
}

fn sanitize_json_for_postgres(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(text.replace('\0', "\\0")),
        Value::Array(values) => {
            Value::Array(values.into_iter().map(sanitize_json_for_postgres).collect())
        }
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, sanitize_json_for_postgres(value)))
                .collect(),
        ),
        scalar => scalar,
    }
}

fn verify_network_material(
    transaction: &SignedAnchorTransaction,
    network: &BsvNetworkEvidence,
) -> anyhow::Result<()> {
    let txid = hex::encode(transaction.txid);
    if hex::decode(&network.woc_raw_transaction_hex)? != transaction.raw_transaction {
        return Err(anyhow!("independent raw transaction mismatch"));
    }
    verify_json_txid(&network.woc_transaction, &txid)?;
    verify_json_txid(&network.arc_submission, &txid)?;
    verify_json_txid(&network.arc_status, &txid)?;
    if json_string(&network.arc_status, "txStatus")? == "MINED" {
        verify_mined_spv(
            &txid,
            &network.arc_status,
            &network.woc_transaction,
            network
                .woc_block
                .as_ref()
                .context("missing block evidence")?,
        )?;
    }
    Ok(())
}

fn verify_mined_spv(
    txid: &str,
    arc: &Value,
    woc_transaction: &Value,
    block: &Value,
) -> anyhow::Result<()> {
    let bump_hex = json_string(arc, "merklePath")?;
    if bump_hex.is_empty() {
        return Err(anyhow!("mined ARC response has no BUMP Merkle path"));
    }
    let path = MerklePath::from_hex(bump_hex).context("invalid ARC BUMP")?;
    let computed_root = path.compute_root(Some(txid))?;
    let expected_root = json_string(block, "merkleroot")?;
    if !computed_root.eq_ignore_ascii_case(expected_root) {
        return Err(anyhow!("BUMP does not resolve to block Merkle root"));
    }
    let arc_height = json_u64(arc, "blockHeight")?;
    let block_height = json_u64(block, "height")?;
    if u64::from(path.block_height) != arc_height || arc_height != block_height {
        return Err(anyhow!("BUMP block height mismatch"));
    }
    let header_hash = compute_header_hash(block)?;
    let block_hash = json_string(block, "hash")?;
    if !header_hash.eq_ignore_ascii_case(block_hash)
        || !json_string(arc, "blockHash")?.eq_ignore_ascii_case(block_hash)
        || !json_string(woc_transaction, "blockhash")?.eq_ignore_ascii_case(block_hash)
    {
        return Err(anyhow!("block header hash mismatch"));
    }
    if json_u64(block, "confirmations")? == 0 || json_u64(woc_transaction, "confirmations")? == 0 {
        return Err(anyhow!(
            "transaction block is not confirmed on the BSV testnet chain"
        ));
    }
    Ok(())
}

fn compute_header_hash(block: &Value) -> anyhow::Result<String> {
    let version = u32::try_from(json_u64(block, "version")?)?;
    let previous = display_hash_wire_bytes(json_string(block, "previousblockhash")?)?;
    let merkle = display_hash_wire_bytes(json_string(block, "merkleroot")?)?;
    let time = u32::try_from(json_u64(block, "time")?)?;
    let bits_value = block.get("bits").context("block missing bits")?;
    let bits = if let Some(text) = bits_value.as_str() {
        u32::from_str_radix(text.trim_start_matches("0x"), 16)?
    } else {
        u32::try_from(bits_value.as_u64().context("invalid block bits")?)?
    };
    let nonce = u32::try_from(json_u64(block, "nonce")?)?;
    let mut header = Vec::with_capacity(80);
    header.extend_from_slice(&version.to_le_bytes());
    header.extend_from_slice(&previous);
    header.extend_from_slice(&merkle);
    header.extend_from_slice(&time.to_le_bytes());
    header.extend_from_slice(&bits.to_le_bytes());
    header.extend_from_slice(&nonce.to_le_bytes());
    let first = Sha256::digest(&header);
    let mut second: [u8; 32] = Sha256::digest(first).into();
    second.reverse();
    Ok(hex::encode(second))
}

fn display_hash_wire_bytes(value: &str) -> anyhow::Result<Vec<u8>> {
    let mut bytes = hex::decode(value)?;
    if bytes.len() != 32 {
        return Err(anyhow!("header hash field is not 32 bytes"));
    }
    bytes.reverse();
    Ok(bytes)
}

fn verify_json_txid(value: &Value, expected: &str) -> anyhow::Result<()> {
    let observed = value
        .get("txid")
        .or_else(|| value.get("hash"))
        .and_then(Value::as_str)
        .context("network response missing txid")?;
    if !observed.eq_ignore_ascii_case(expected) {
        return Err(anyhow!("network response txid mismatch"));
    }
    Ok(())
}

fn json_string<'a>(value: &'a Value, field: &str) -> anyhow::Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("network response missing {field}"))
}

fn json_u64(value: &Value, field: &str) -> anyhow::Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .with_context(|| format!("network response missing {field}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_header_hash_matches_known_genesis_vector() {
        let block = serde_json::json!({
            "version": 1,
            "previousblockhash": "00".repeat(32),
            "merkleroot": "4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b",
            "time": 1231006505,
            "bits": "1d00ffff",
            "nonce": 2083236893u64,
        });
        assert_eq!(
            compute_header_hash(&block).expect("header hash"),
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
        );
    }

    #[test]
    fn network_txid_substitution_is_rejected() {
        let expected = "11".repeat(32);
        let substituted = serde_json::json!({"txid": "22".repeat(32)});
        assert!(verify_json_txid(&substituted, &expected).is_err());
    }

    #[test]
    fn malformed_header_digest_is_rejected() {
        assert!(display_hash_wire_bytes("abcd").is_err());
    }

    #[test]
    fn wallet_action_is_synchronously_processed_not_left_as_no_send() {
        let options = wallet_action_options();
        assert_eq!(options.sign_and_process.0, None);
        assert_eq!(options.accept_delayed_broadcast.0, Some(false));
        assert_eq!(options.no_send.0, Some(false));
        assert_eq!(options.return_txid_only.0, Some(false));
    }

    #[test]
    fn network_only_services_do_not_expose_a_wallet() {
        let services =
            AnchorServices::new_network_only(DEFAULT_ARC_TESTNET_URL, DEFAULT_WOC_TESTNET_URL)
                .expect("network services");
        assert!(services.wallet.is_none());
    }
}
