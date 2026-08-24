#![deny(unsafe_code)]
#![warn(missing_docs)]
//! Signed Buzz and persistent Mesh-owner verification for ST8 Compute.
//!
//! Nostr signatures establish requester, provider, project-authority, agent,
//! and relay identities. A separate Ed25519 signature made by the existing
//! Mesh owner keystore proves that a capability or receipt came from the named
//! persistent compute node. BSV wallet authority is intentionally absent.

use buzz_core::{
    kind::{
        KIND_JOB_REQUEST, KIND_JOB_RESULT, KIND_ST8_COMPUTE_CAPABILITY, KIND_ST8_COMPUTE_DISPUTE,
        KIND_ST8_COMPUTE_JOB, KIND_ST8_COMPUTE_PRICING, KIND_ST8_COMPUTE_RECEIPT,
        KIND_ST8_COMPUTE_SETTLEMENT,
    },
    verify_event, Event,
};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use nostr::{EventBuilder, Kind, Tag};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use st8_bsv_provenance::{
    verify_anchor_transaction, AnchorPayload, BroadcastReceipt, BsvNetwork, CommitmentKind,
    MerkleBatch, MerkleProof, ProjectCommitment, SignedAnchorTransaction,
};
use st8_compute_protocol::{
    ComputeJob, ComputeReceipt, ComputeSettlement, Digest32, NodeCapability, PricingPolicy,
    ProtocolError,
};
use st8_contribution_engine::BuzzProjectContext;
use thiserror::Error;

const NODE_CAPABILITY_SIGNATURE_DOMAIN: &[u8] = b"ST8WRX\0MESH_NODE_CAPABILITY_SIGNATURE\0V1";
const NODE_RECEIPT_SIGNATURE_DOMAIN: &[u8] = b"ST8WRX\0MESH_NODE_RECEIPT_SIGNATURE\0V1";

/// Capability plus an Ed25519 signature from the persistent Mesh owner key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCapabilityAttestation {
    /// Deterministic capability body.
    pub capability: NodeCapability,
    /// Raw 64-byte Ed25519 signature.
    pub node_signature: Vec<u8>,
}

impl NodeCapabilityAttestation {
    /// Returns the exact domain-separated bytes the Mesh owner signs.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, EngineError> {
        let mut bytes = Vec::from(NODE_CAPABILITY_SIGNATURE_DOMAIN);
        bytes.extend_from_slice(&self.capability.id()?);
        Ok(bytes)
    }

    /// Verifies the Mesh owner ID/public-key binding and signature.
    pub fn verify_node_signature(&self) -> Result<(), EngineError> {
        verify_node_signature(
            &self.capability.node_owner_id,
            &self.capability.node_public_key,
            &self.node_signature,
            &self.signing_bytes()?,
        )
    }

    /// Builds the provider Nostr event carrying the node attestation.
    pub fn event_builder(&self) -> Result<EventBuilder, EngineError> {
        self.verify_node_signature()?;
        let capability_id = hex::encode(self.capability.id()?);
        let tags = [
            tag(["d", self.capability.node_owner_id.as_str()])?,
            tag(["st8-capability", capability_id.as_str()])?,
            tag(["st8-node", self.capability.node_owner_id.as_str()])?,
        ];
        Ok(EventBuilder::new(
            Kind::Custom(KIND_ST8_COMPUTE_CAPABILITY as u16),
            serde_json::to_string(self)?,
        )
        .tags(tags))
    }

    /// Verifies the provider Nostr event and embedded Mesh signature.
    pub fn from_event(event: &Event) -> Result<Self, EngineError> {
        verify_signed_event(event)?;
        if u32::from(event.kind.as_u16()) != KIND_ST8_COMPUTE_CAPABILITY {
            return Err(EngineError::WrongEventKind);
        }
        let attestation: Self = serde_json::from_str(&event.content)?;
        attestation.verify_node_signature()?;
        if event.pubkey.to_hex() != nostr_hex(&attestation.capability.provider)?
            || single_tag(event, "d") != Some(attestation.capability.node_owner_id.as_str())
            || single_tag(event, "st8-node") != Some(attestation.capability.node_owner_id.as_str())
            || single_tag(event, "st8-capability")
                != Some(hex::encode(attestation.capability.id()?).as_str())
        {
            return Err(EngineError::EventBindingMismatch);
        }
        Ok(attestation)
    }
}

/// Verified provider-signed pricing event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingContext {
    /// Full signed pricing event.
    pub event: Event,
    /// Deterministic pricing policy.
    pub policy: PricingPolicy,
}

impl PricingContext {
    /// Builds the provider-signed pricing event.
    pub fn event_builder(policy: &PricingPolicy) -> Result<EventBuilder, EngineError> {
        let id = hex::encode(policy.id()?);
        let tags = [
            tag(["d", id.as_str()])?,
            tag(["a", policy.project.as_str()])?,
            tag(["st8-node", policy.node_owner_id.as_str()])?,
            tag(["st8-pricing-version", policy.version.as_str()])?,
        ];
        Ok(EventBuilder::new(
            Kind::Custom(KIND_ST8_COMPUTE_PRICING as u16),
            serde_json::to_string(policy)?,
        )
        .tags(tags))
    }

    /// Verifies and derives a pricing context from a Nostr event.
    pub fn from_event(project: &BuzzProjectContext, event: &Event) -> Result<Self, EngineError> {
        project.verify()?;
        verify_signed_event(event)?;
        if u32::from(event.kind.as_u16()) != KIND_ST8_COMPUTE_PRICING {
            return Err(EngineError::WrongEventKind);
        }
        let policy: PricingPolicy = serde_json::from_str(&event.content)?;
        let id = hex::encode(policy.id()?);
        if policy.project != project.project
            || event.pubkey.to_hex() != nostr_hex(&policy.provider)?
            || single_tag(event, "d") != Some(id.as_str())
            || single_tag(event, "a") != Some(policy.project.as_str())
            || single_tag(event, "st8-node") != Some(policy.node_owner_id.as_str())
            || single_tag(event, "st8-pricing-version") != Some(policy.version.as_str())
        {
            return Err(EngineError::EventBindingMismatch);
        }
        Ok(Self {
            event: event.clone(),
            policy,
        })
    }
}

/// Verified requester-signed compute job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeJobContext {
    /// Full signed compute job event.
    pub event: Event,
    /// Deterministic project-scoped authorization.
    pub job: ComputeJob,
}

impl ComputeJobContext {
    /// Builds the requester-signed compute authorization event.
    pub fn event_builder(job: &ComputeJob) -> Result<EventBuilder, EngineError> {
        let id = hex::encode(job.id()?);
        let request = hex::encode(job.request_event_id);
        let pricing = hex::encode(job.pricing_policy_id);
        let provider = nostr_hex(&job.provider)?;
        let tags = [
            tag(["a", job.project.as_str()])?,
            tag(["d", id.as_str()])?,
            tag(["e", request.as_str()])?,
            tag(["p", provider.as_str()])?,
            tag(["st8-node", job.node_owner_id.as_str()])?,
            tag(["st8-pricing", pricing.as_str()])?,
        ];
        Ok(EventBuilder::new(
            Kind::Custom(KIND_ST8_COMPUTE_JOB as u16),
            serde_json::to_string(job)?,
        )
        .tags(tags))
    }

    /// Verifies the job, exact pricing event, and signed Mesh job request.
    pub fn from_events(
        project: &BuzzProjectContext,
        job_event: &Event,
        pricing: &PricingContext,
        request_event: &Event,
    ) -> Result<Self, EngineError> {
        project.verify()?;
        verify_signed_event(job_event)?;
        verify_project_evidence(project, request_event)?;
        if u32::from(job_event.kind.as_u16()) != KIND_ST8_COMPUTE_JOB
            || u32::from(request_event.kind.as_u16()) != KIND_JOB_REQUEST
        {
            return Err(EngineError::WrongEventKind);
        }
        let job: ComputeJob = serde_json::from_str(&job_event.content)?;
        let job_id = hex::encode(job.id()?);
        let request_id = request_event.id.to_hex();
        let provider = nostr_hex(&job.provider)?;
        let pricing_id = hex::encode(job.pricing_policy_id);
        let request_body: serde_json::Value = serde_json::from_str(&request_event.content)?;
        if job.project != project.project
            || job.request_event_id != event_digest(request_event)
            || single_digest_tag(request_event, "st8-input") != Some(job.input_commitment)
            || json_digest_field(request_event, "input_commitment") != Some(job.input_commitment)
            || request_body
                .get("workload")
                .and_then(serde_json::Value::as_str)
                != Some(workload_content_code(job.workload))
            || request_body
                .get("model")
                .and_then(serde_json::Value::as_str)
                != Some(job.model.as_str())
            || job.pricing_policy_id != pricing.policy.id()?
            || job_event.pubkey.to_hex() != nostr_hex(&job.requester)?
            || request_event.pubkey != job_event.pubkey
            || single_tag(request_event, "a") != Some(job.project.as_str())
            || single_tag(request_event, "p") != Some(provider.as_str())
            || single_tag(job_event, "a") != Some(job.project.as_str())
            || single_tag(job_event, "d") != Some(job_id.as_str())
            || single_tag(job_event, "e") != Some(request_id.as_str())
            || single_tag(job_event, "p") != Some(provider.as_str())
            || single_tag(job_event, "st8-node") != Some(job.node_owner_id.as_str())
            || single_tag(job_event, "st8-pricing") != Some(pricing_id.as_str())
        {
            return Err(EngineError::EventBindingMismatch);
        }
        Ok(Self {
            event: job_event.clone(),
            job,
        })
    }
}

/// Receipt plus the persistent Mesh owner's Ed25519 signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeReceiptAttestation {
    /// Deterministic compute receipt.
    pub receipt: ComputeReceipt,
    /// Ed25519 public key backing the receipt's node owner ID.
    pub node_public_key: Digest32,
    /// Raw 64-byte Ed25519 signature.
    pub node_signature: Vec<u8>,
}

impl NodeReceiptAttestation {
    /// Returns the exact bytes the Mesh owner signs.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, EngineError> {
        let mut bytes = Vec::from(NODE_RECEIPT_SIGNATURE_DOMAIN);
        bytes.extend_from_slice(&self.receipt.id()?);
        Ok(bytes)
    }

    /// Verifies persistent node identity and receipt signature.
    pub fn verify_node_signature(&self) -> Result<(), EngineError> {
        verify_node_signature(
            &self.receipt.node_owner_id,
            &self.node_public_key,
            &self.node_signature,
            &self.signing_bytes()?,
        )
    }

    /// Builds the provider-signed Nostr receipt event.
    pub fn event_builder(&self) -> Result<EventBuilder, EngineError> {
        self.verify_node_signature()?;
        let receipt = hex::encode(self.receipt.id()?);
        let job = hex::encode(self.receipt.job_id);
        let result = hex::encode(self.receipt.result_event_id);
        let requester = nostr_hex(&self.receipt.requester)?;
        let tags = [
            tag(["a", self.receipt.project.as_str()])?,
            tag(["d", receipt.as_str()])?,
            tag(["e", result.as_str()])?,
            tag(["p", requester.as_str()])?,
            tag(["st8-job", job.as_str()])?,
            tag(["st8-node", self.receipt.node_owner_id.as_str()])?,
        ];
        Ok(EventBuilder::new(
            Kind::Custom(KIND_ST8_COMPUTE_RECEIPT as u16),
            serde_json::to_string(self)?,
        )
        .tags(tags))
    }
}

/// Independently verified signed compute material ready for durable projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedComputeReceipt {
    /// Signed project context.
    pub project: BuzzProjectContext,
    /// Provider-signed pricing.
    pub pricing: PricingContext,
    /// Requester-signed compute authorization.
    pub job: ComputeJobContext,
    /// Signed underlying Mesh job request.
    pub request_event: Event,
    /// Agent/provider-signed terminal result.
    pub result_event: Event,
    /// Provider-signed Nostr receipt event.
    pub receipt_event: Event,
    /// Persistent Mesh-owner attestation carried by `receipt_event`.
    pub attestation: NodeReceiptAttestation,
}

impl VerifiedComputeReceipt {
    /// Verifies every identity, project, job, result, price, and node binding.
    pub fn from_events(
        project: BuzzProjectContext,
        pricing_event: &Event,
        job_event: &Event,
        request_event: &Event,
        result_event: &Event,
        receipt_event: &Event,
    ) -> Result<Self, EngineError> {
        let pricing = PricingContext::from_event(&project, pricing_event)?;
        let job = ComputeJobContext::from_events(&project, job_event, &pricing, request_event)?;
        verify_project_evidence(&project, result_event)?;
        verify_signed_event(receipt_event)?;
        if u32::from(result_event.kind.as_u16()) != KIND_JOB_RESULT
            || u32::from(receipt_event.kind.as_u16()) != KIND_ST8_COMPUTE_RECEIPT
        {
            return Err(EngineError::WrongEventKind);
        }
        let attestation: NodeReceiptAttestation = serde_json::from_str(&receipt_event.content)?;
        attestation.verify_node_signature()?;
        attestation
            .receipt
            .validate_for(&job.job, &pricing.policy)?;
        let receipt_id = hex::encode(attestation.receipt.id()?);
        let result_id = result_event.id.to_hex();
        let request_id = request_event.id.to_hex();
        let job_id = hex::encode(job.job.id()?);
        let requester = nostr_hex(&attestation.receipt.requester)?;
        let result_body: serde_json::Value = serde_json::from_str(&result_event.content)?;
        let result_status = serde_json::to_value(attestation.receipt.status)?;
        let expected_result_author = job
            .job
            .agent
            .as_deref()
            .unwrap_or(job.job.provider.as_str());
        if receipt_event.pubkey.to_hex() != nostr_hex(&attestation.receipt.provider)?
            || attestation.receipt.result_event_id != event_digest(result_event)
            || single_digest_tag(result_event, "st8-output")
                != Some(attestation.receipt.result_commitment)
            || json_digest_field(result_event, "output_commitment")
                != Some(attestation.receipt.result_commitment)
            || result_body.get("status") != Some(&result_status)
            || result_body.get("model").and_then(serde_json::Value::as_str)
                != Some(attestation.receipt.model.as_str())
            || result_event.pubkey.to_hex() != nostr_hex(expected_result_author)?
            || single_tag(result_event, "a") != Some(project.project.as_str())
            || single_tag(result_event, "e") != Some(request_id.as_str())
            || single_tag(result_event, "st8-request") != Some(request_id.as_str())
            || single_tag(receipt_event, "a") != Some(project.project.as_str())
            || single_tag(receipt_event, "d") != Some(receipt_id.as_str())
            || single_tag(receipt_event, "e") != Some(result_id.as_str())
            || single_tag(receipt_event, "p") != Some(requester.as_str())
            || single_tag(receipt_event, "st8-job") != Some(job_id.as_str())
            || single_tag(receipt_event, "st8-node")
                != Some(attestation.receipt.node_owner_id.as_str())
        {
            return Err(EngineError::EventBindingMismatch);
        }
        Ok(Self {
            project,
            pricing,
            job,
            request_event: request_event.clone(),
            result_event: result_event.clone(),
            receipt_event: receipt_event.clone(),
            attestation,
        })
    }

    /// Re-runs independent verification over the retained material.
    pub fn verify(&self) -> Result<(), EngineError> {
        let derived = Self::from_events(
            self.project.clone(),
            &self.pricing.event,
            &self.job.event,
            &self.request_event,
            &self.result_event,
            &self.receipt_event,
        )?;
        if &derived != self {
            return Err(EngineError::EventBindingMismatch);
        }
        Ok(())
    }
}

/// Signed dispute over one exact compute receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeDispute {
    /// Project containing the receipt.
    pub project: String,
    /// Exact receipt ID.
    pub receipt_id: Digest32,
    /// Exact job ID.
    pub job_id: Digest32,
    /// Unix dispute timestamp.
    pub disputed_at: i64,
    /// Human-readable reason committed by the signer.
    pub reason: String,
    /// Optional content-addressed supporting evidence.
    pub evidence: Vec<Digest32>,
}

impl ComputeDispute {
    /// Builds a requester/project-authority signed dispute event.
    pub fn event_builder(&self) -> Result<EventBuilder, EngineError> {
        if self.project.trim().is_empty() || self.reason.trim().is_empty() {
            return Err(EngineError::InvalidDispute);
        }
        let receipt = hex::encode(self.receipt_id);
        let job = hex::encode(self.job_id);
        let tags = [
            tag(["a", self.project.as_str()])?,
            tag(["d", receipt.as_str()])?,
            tag(["e", receipt.as_str()])?,
            tag(["st8-receipt", receipt.as_str()])?,
            tag(["st8-job", job.as_str()])?,
        ];
        Ok(EventBuilder::new(
            Kind::Custom(KIND_ST8_COMPUTE_DISPUTE as u16),
            serde_json::to_string(self)?,
        )
        .tags(tags))
    }

    /// Verifies a dispute signer is the requester or kind-30621 project owner.
    pub fn from_event(
        project: &BuzzProjectContext,
        receipt: &VerifiedComputeReceipt,
        event: &Event,
    ) -> Result<Self, EngineError> {
        verify_signed_event(event)?;
        if u32::from(event.kind.as_u16()) != KIND_ST8_COMPUTE_DISPUTE {
            return Err(EngineError::WrongEventKind);
        }
        let dispute: Self = serde_json::from_str(&event.content)?;
        let receipt_id = receipt.attestation.receipt.id()?;
        let signer = event.pubkey.to_hex();
        let requester = nostr_hex(&receipt.attestation.receipt.requester)?;
        let project_owner = project
            .project
            .split(':')
            .nth(1)
            .ok_or(EngineError::ProjectMismatch)?;
        let receipt_id_hex = hex::encode(receipt_id);
        let job_id_hex = hex::encode(dispute.job_id);
        let receipt_end_second = receipt.attestation.receipt.ended_at_ms.div_euclid(1_000);
        if dispute.project != project.project
            || dispute.receipt_id != receipt_id
            || dispute.job_id != receipt.job.job.id()?
            || dispute.disputed_at < receipt_end_second
            || (signer != requester && signer != project_owner)
            || single_tag(event, "a") != Some(project.project.as_str())
            || single_tag(event, "d") != Some(receipt_id_hex.as_str())
            || single_tag(event, "e") != Some(receipt_id_hex.as_str())
            || single_tag(event, "st8-receipt") != Some(receipt_id_hex.as_str())
            || single_tag(event, "st8-job") != Some(job_id_hex.as_str())
        {
            return Err(EngineError::EventBindingMismatch);
        }
        Ok(dispute)
    }
}

/// Project-owner signed deterministic settlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementContext {
    /// Signed settlement event.
    pub event: Event,
    /// Deterministic settlement state.
    pub settlement: ComputeSettlement,
}

impl SettlementContext {
    /// Builds the kind-30621 owner-signed settlement event.
    pub fn event_builder(settlement: &ComputeSettlement) -> Result<EventBuilder, EngineError> {
        let id = hex::encode(settlement.id()?);
        let tags = [
            tag(["a", settlement.project.as_str()])?,
            tag(["d", id.as_str()])?,
            tag(["st8-settlement", id.as_str()])?,
        ];
        Ok(EventBuilder::new(
            Kind::Custom(KIND_ST8_COMPUTE_SETTLEMENT as u16),
            serde_json::to_string(settlement)?,
        )
        .tags(tags))
    }

    /// Verifies a project-owner signed settlement and its exact receipt set.
    pub fn from_event(
        project: &BuzzProjectContext,
        receipts: &[VerifiedComputeReceipt],
        disputes: &[Digest32],
        event: &Event,
    ) -> Result<Self, EngineError> {
        project.verify()?;
        verify_signed_event(event)?;
        if u32::from(event.kind.as_u16()) != KIND_ST8_COMPUTE_SETTLEMENT
            || event.pubkey != project.project_event.pubkey
        {
            return Err(EngineError::WrongEventKind);
        }
        for receipt in receipts {
            receipt.verify()?;
        }
        let settlement: ComputeSettlement = serde_json::from_str(&event.content)?;
        let receipt_bodies: Vec<_> = receipts
            .iter()
            .map(|receipt| receipt.attestation.receipt.clone())
            .collect();
        let derived = ComputeSettlement::from_receipts(
            project.project.clone(),
            settlement.period_start,
            settlement.period_end,
            &receipt_bodies,
            disputes,
        )?;
        let id = hex::encode(derived.id()?);
        if settlement != derived
            || single_tag(event, "a") != Some(project.project.as_str())
            || single_tag(event, "d") != Some(id.as_str())
            || single_tag(event, "st8-settlement") != Some(id.as_str())
        {
            return Err(EngineError::EventBindingMismatch);
        }
        Ok(Self {
            event: event.clone(),
            settlement,
        })
    }
}

/// Fully verified settlement state waiting for asynchronous external-wallet anchoring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreparedComputeSettlementAnchor {
    /// Signed project definition anchoring authority and scope.
    pub project: BuzzProjectContext,
    /// Fully verified signed receipts considered by the settlement.
    pub receipts: Vec<VerifiedComputeReceipt>,
    /// Receipt IDs frozen by a signed dispute and excluded from balances.
    pub disputed_receipt_ids: Vec<Digest32>,
    /// Project-owner-signed deterministic settlement.
    pub settlement: SettlementContext,
    /// Project-scoped settlement commitment.
    pub commitment: ProjectCommitment,
    /// Proof that the settlement commitment is in the anchored batch.
    pub merkle_proof: MerkleProof,
    /// Exact external-wallet BSV payload.
    pub anchor_payload: AnchorPayload,
}

impl PreparedComputeSettlementAnchor {
    /// Verifies signed inputs and prepares one BSV testnet settlement anchor.
    pub fn new(
        project: BuzzProjectContext,
        receipts: Vec<VerifiedComputeReceipt>,
        disputed_receipt_ids: Vec<Digest32>,
        settlement_event: &Event,
        network: BsvNetwork,
    ) -> Result<Self, EngineError> {
        let settlement = SettlementContext::from_event(
            &project,
            &receipts,
            &disputed_receipt_ids,
            settlement_event,
        )?;
        let settlement_id = settlement.settlement.id()?;
        let commitment = ProjectCommitment {
            project: project.project.clone(),
            kind: CommitmentKind::Settlement,
            object_id: settlement_id,
            state_digest: settlement_id,
        };
        let batch = MerkleBatch::new(vec![commitment.clone()])?;
        let merkle_proof = batch.proof_for(&commitment)?;
        let anchor_payload = AnchorPayload::new(network, &project.project, &batch, settlement_id)?;
        Ok(Self {
            project,
            receipts,
            disputed_receipt_ids,
            settlement,
            commitment,
            merkle_proof,
            anchor_payload,
        })
    }

    /// Independently verifies every receipt, dispute exclusion, settlement, and anchor binding.
    pub fn verify(&self) -> Result<(), EngineError> {
        let derived = Self::new(
            self.project.clone(),
            self.receipts.clone(),
            self.disputed_receipt_ids.clone(),
            &self.settlement.event,
            self.anchor_payload.network,
        )?;
        if &derived != self {
            return Err(EngineError::EventBindingMismatch);
        }
        self.merkle_proof
            .verify(&self.commitment, self.anchor_payload.merkle_root)?;
        self.anchor_payload.verify_project(&self.project.project)?;
        Ok(())
    }

    /// Finalizes public transaction material after external signing and broadcast.
    pub fn finalize(
        self,
        transaction: SignedAnchorTransaction,
        broadcast: BroadcastReceipt,
    ) -> Result<ComputeSettlementAnchorReceipt, EngineError> {
        self.verify()?;
        if self.anchor_payload.network != BsvNetwork::Testnet || !broadcast.accepted {
            return Err(EngineError::InvalidAnchorState);
        }
        verify_anchor_transaction(&transaction, &self.anchor_payload)?;
        let receipt = ComputeSettlementAnchorReceipt {
            version: 1,
            prepared: self,
            transaction,
            broadcast,
        };
        receipt.verify()?;
        Ok(receipt)
    }
}

/// Public settlement receipt independently verifiable without producer trust.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComputeSettlementAnchorReceipt {
    /// Receipt schema version.
    pub version: u16,
    /// Complete signed compute and settlement state.
    pub prepared: PreparedComputeSettlementAnchor,
    /// Exact signed external-wallet transaction.
    pub transaction: SignedAnchorTransaction,
    /// Normalized public network submission result.
    pub broadcast: BroadcastReceipt,
}

impl ComputeSettlementAnchorReceipt {
    /// Verifies signed compute state, settlement aggregation, txid, and exact output commitment.
    pub fn verify(&self) -> Result<(), EngineError> {
        if self.version != 1 || !self.broadcast.accepted {
            return Err(EngineError::InvalidAnchorState);
        }
        self.prepared.verify()?;
        if self.prepared.anchor_payload.network != BsvNetwork::Testnet {
            return Err(EngineError::InvalidAnchorState);
        }
        verify_anchor_transaction(&self.transaction, &self.prepared.anchor_payload)?;
        Ok(())
    }
}

/// Verification errors for signed ST8 Compute material.
#[derive(Debug, Error)]
pub enum EngineError {
    /// Deterministic protocol validation failed.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    /// Contribution/project context verification failed.
    #[error("project context verification failed: {0}")]
    Project(String),
    /// Nostr event ID or signature was invalid.
    #[error("invalid signed Buzz event: {0}")]
    InvalidBuzzEvent(String),
    /// Event kind was not the required ST8 Compute kind.
    #[error("wrong event kind")]
    WrongEventKind,
    /// Event tags/content/signers did not bind the exact protocol object.
    #[error("event binding mismatch")]
    EventBindingMismatch,
    /// A linked event was not grounded in the signed project.
    #[error("event is not grounded in the signed project")]
    UngroundedEvidence,
    /// Nostr identity reference was malformed.
    #[error("invalid Nostr identity")]
    InvalidNostrIdentity,
    /// Mesh owner ID did not match the Ed25519 public key.
    #[error("Mesh owner identity mismatch")]
    NodeIdentityMismatch,
    /// Mesh owner signature was malformed or invalid.
    #[error("invalid Mesh owner signature")]
    InvalidNodeSignature,
    /// Signed project coordinate could not be resolved.
    #[error("project mismatch")]
    ProjectMismatch,
    /// Dispute payload or signer was invalid.
    #[error("invalid compute dispute")]
    InvalidDispute,
    /// Wallet/network receipt did not prove a valid BSV testnet anchor state.
    #[error("invalid settlement anchor state")]
    InvalidAnchorState,
    /// JSON material was malformed.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Event tag could not be constructed.
    #[error("invalid event tag")]
    InvalidTag,
}

impl From<st8_bsv_provenance::ProvenanceError> for EngineError {
    fn from(error: st8_bsv_provenance::ProvenanceError) -> Self {
        Self::Project(error.to_string())
    }
}

impl From<st8_contribution_engine::EngineError> for EngineError {
    fn from(error: st8_contribution_engine::EngineError) -> Self {
        Self::Project(error.to_string())
    }
}

fn verify_node_signature(
    owner_id: &str,
    public_key: &Digest32,
    signature: &[u8],
    message: &[u8],
) -> Result<(), EngineError> {
    let derived_owner = hex::encode(Sha256::digest(public_key));
    if derived_owner != owner_id {
        return Err(EngineError::NodeIdentityMismatch);
    }
    let verifying_key =
        VerifyingKey::from_bytes(public_key).map_err(|_| EngineError::InvalidNodeSignature)?;
    let signature_bytes: [u8; 64] = signature
        .try_into()
        .map_err(|_| EngineError::InvalidNodeSignature)?;
    let signature = Signature::from_bytes(&signature_bytes);
    verifying_key
        .verify(message, &signature)
        .map_err(|_| EngineError::InvalidNodeSignature)
}

fn verify_project_evidence(project: &BuzzProjectContext, event: &Event) -> Result<(), EngineError> {
    verify_signed_event(event)?;
    if !tag_values(event, "a").any(|coordinate| coordinate == project.project) {
        return Err(EngineError::UngroundedEvidence);
    }
    Ok(())
}

fn verify_signed_event(event: &Event) -> Result<(), EngineError> {
    verify_event(event).map_err(|error| EngineError::InvalidBuzzEvent(error.to_string()))
}

fn event_digest(event: &Event) -> Digest32 {
    *event.id.as_bytes()
}

fn single_digest_tag(event: &Event, name: &str) -> Option<Digest32> {
    let value = single_tag(event, name)?;
    let bytes = hex::decode(value).ok()?;
    bytes.try_into().ok()
}

fn json_digest_field(event: &Event, name: &str) -> Option<Digest32> {
    let content: serde_json::Value = serde_json::from_str(&event.content).ok()?;
    let value = content.as_object()?.get(name)?.as_str()?;
    let bytes = hex::decode(value).ok()?;
    bytes.try_into().ok()
}

fn nostr_hex(identity: &str) -> Result<String, EngineError> {
    let value = identity.strip_prefix("nostr:").unwrap_or(identity);
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(EngineError::InvalidNostrIdentity);
    }
    Ok(value.to_ascii_lowercase())
}

fn workload_content_code(workload: st8_compute_protocol::WorkloadKind) -> &'static str {
    use st8_compute_protocol::WorkloadKind;

    match workload {
        WorkloadKind::LlmInference => "llm_inference",
        WorkloadKind::CpuTask => "cpu_task",
        WorkloadKind::GpuTask => "gpu_task",
        WorkloadKind::AgentTask => "agent_task",
    }
}

fn tag<const N: usize>(values: [&str; N]) -> Result<Tag, EngineError> {
    Tag::parse(values).map_err(|_| EngineError::InvalidTag)
}

fn single_tag<'a>(event: &'a Event, name: &'a str) -> Option<&'a str> {
    let mut values = tag_values(event, name);
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn tag_values<'a>(event: &'a Event, name: &'a str) -> impl Iterator<Item = &'a str> {
    event.tags.iter().filter_map(move |tag| {
        let values = tag.as_slice();
        (values.first().map(String::as_str) == Some(name))
            .then(|| values.get(1).map(String::as_str))
            .flatten()
    })
}

#[cfg(test)]
mod tests;
