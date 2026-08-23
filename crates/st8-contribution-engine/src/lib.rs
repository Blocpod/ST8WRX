#![deny(unsafe_code)]
#![warn(missing_docs)]
//! First ST8WRX contribution vertical slice.
//!
//! The engine consumes already-signed Buzz events, grounds them in a signed
//! NIP-MP project event, applies explicit human project governance, awards
//! non-transferable Contribution Units, prepares a project-scoped Merkle anchor,
//! persists a receipt, and independently re-verifies the complete chain.

use buzz_core::{verify_event, Event};
use buzz_sdk::validate_project_envelope;
use nostr::{EventBuilder, Kind, Tag};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use st8_bsv_provenance::{
    verify_anchor_transaction, AnchorBroadcaster, AnchorPayload, AnchorTransactionProvider,
    BroadcastReceipt, BsvNetwork, CommitmentKind, Digest32, MerkleBatch, MerkleProof,
    ProjectCommitment, ProvenanceError, SignedAnchorTransaction,
};
use st8_contribution_protocol::{
    ContributionClass, ContributionDecision, ContributionRecord, ContributorKind, DecisionStatus,
    EvidenceKind, EvidenceRef, ProtocolError,
};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use thiserror::Error;

const SNAPSHOT_DOMAIN: &[u8] = b"ST8WRX\0CONTRIBUTION_SNAPSHOT\0V1";
const GOVERNANCE_INTENT_DOMAIN: &[u8] = b"ST8WRX\0GOVERNANCE_INTENT\0V1";
const RECEIPT_VERSION: u16 = 1;
/// Nostr event kind used for append-only ST8WRX project-governance approvals.
pub const KIND_ST8_GOVERNANCE_APPROVAL: u16 = 49_800;

/// Signed NIP-MP project context used to resolve project-scoped evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuzzProjectContext {
    /// NIP-33 coordinate: `30621:<owner-pubkey>:<slug>`.
    pub project: String,
    /// Signed project-event ID used as the membership assertion.
    pub project_event_id: Digest32,
    /// Full signed project event, retained for independent verification.
    pub project_event: Event,
    /// Canonical NIP-34 repository coordinates listed by the project.
    pub repository_coordinates: Vec<String>,
}

impl BuzzProjectContext {
    /// Builds project context from a verified NIP-MP kind-30621 event.
    pub fn from_event(event: &Event) -> Result<Self, EngineError> {
        verify_event(event).map_err(|error| EngineError::InvalidBuzzEvent(error.to_string()))?;
        if event.kind.as_u16() != buzz_core::kind::KIND_PROJECT as u16 {
            return Err(EngineError::NotProjectEvent);
        }
        validate_project_envelope(event.tags.as_slice(), &event.content)
            .map_err(|error| EngineError::InvalidProjectEvent(error.to_string()))?;
        let slug = tag_values(event, "d")
            .next()
            .ok_or(EngineError::InvalidProjectEvent("missing d tag".into()))?;
        let project = format!("30621:{}:{slug}", event.pubkey.to_hex());
        let mut repositories: Vec<String> = tag_values(event, "a").map(str::to_owned).collect();
        repositories.sort();
        repositories.dedup();
        Ok(Self {
            project,
            project_event_id: event_digest(event)?,
            project_event: event.clone(),
            repository_coordinates: repositories,
        })
    }

    /// Re-verifies the retained signature and every derived project-context field.
    pub fn verify(&self) -> Result<(), EngineError> {
        let derived = Self::from_event(&self.project_event)?;
        if derived.project != self.project
            || derived.project_event_id != self.project_event_id
            || derived.repository_coordinates != self.repository_coordinates
        {
            return Err(EngineError::ProjectMismatch);
        }
        Ok(())
    }

    /// Verifies that a signed Buzz event is explicitly grounded in this project
    /// or one of the repositories listed by its signed project event.
    pub fn evidence_ref(&self, event: &Event) -> Result<EvidenceRef, EngineError> {
        verify_event(event).map_err(|error| EngineError::InvalidBuzzEvent(error.to_string()))?;
        let is_project_event = event.kind.as_u16() == buzz_core::kind::KIND_PROJECT as u16
            && project_coordinate(event).as_deref() == Some(self.project.as_str());
        let references_scope = tag_values(event, "a").any(|coordinate| {
            coordinate == self.project
                || self
                    .repository_coordinates
                    .binary_search_by(|candidate| candidate.as_str().cmp(coordinate))
                    .is_ok()
        });
        if !is_project_event && !references_scope {
            return Err(EngineError::UngroundedEvidence);
        }
        let digest = event_digest(event)?;
        Ok(EvidenceRef {
            kind: EvidenceKind::BuzzEvent,
            digest,
            locator: format!("nostr:{}", event.id.to_hex()),
        })
    }

    /// Creates a contribution record grounded in one or more verified Buzz events.
    pub fn contribution_from_events(
        &self,
        contributor: String,
        contributor_kind: ContributorKind,
        class: ContributionClass,
        created_at: i64,
        summary: String,
        events: &[Event],
    ) -> Result<ContributionRecord, EngineError> {
        if events.is_empty() {
            return Err(EngineError::UngroundedEvidence);
        }
        let evidence_refs = events
            .iter()
            .map(|event| self.evidence_ref(event))
            .collect::<Result<Vec<_>, _>>()?;
        let record = ContributionRecord {
            project: self.project.clone(),
            contributor,
            contributor_kind,
            class,
            created_at,
            summary,
            evidence_refs,
        };
        let _ = record.id()?;
        Ok(record)
    }
}

/// Exact governance action that project authorities sign before a decision is
/// accepted. The digest excludes the eventual approver set so each founder can
/// sign the same proposal independently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceDecisionIntent {
    /// Deterministic contribution being decided.
    pub contribution_id: Digest32,
    /// Project identity.
    pub project: String,
    /// Proposed decision state.
    pub status: DecisionStatus,
    /// Proposed non-transferable Contribution Units.
    pub contribution_units: u64,
    /// Governance policy version.
    pub policy_version: String,
    /// Unix timestamp at which the approval set closes.
    pub decided_at: i64,
    /// Human-readable rationale bound into every approval signature.
    pub rationale: String,
}

impl GovernanceDecisionIntent {
    /// Returns the domain-separated canonical bytes signed by project authorities.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, EngineError> {
        if self.project.trim().is_empty()
            || self.policy_version.trim().is_empty()
            || self.rationale.trim().is_empty()
            || self.project.as_bytes().contains(&0)
            || self.policy_version.as_bytes().contains(&0)
            || self.rationale.as_bytes().contains(&0)
        {
            return Err(EngineError::InvalidGovernanceIntent);
        }
        if (self.status == DecisionStatus::Rejected && self.contribution_units != 0)
            || (self.status != DecisionStatus::Rejected && self.contribution_units == 0)
        {
            return Err(EngineError::InvalidGovernanceIntent);
        }
        let mut out = Vec::new();
        out.extend_from_slice(GOVERNANCE_INTENT_DOMAIN);
        out.extend_from_slice(&self.contribution_id);
        put_bytes(&mut out, self.project.as_bytes())?;
        out.extend_from_slice(&self.status.protocol_code().to_be_bytes());
        out.extend_from_slice(&self.contribution_units.to_be_bytes());
        put_bytes(&mut out, self.policy_version.as_bytes())?;
        out.extend_from_slice(&self.decided_at.to_be_bytes());
        put_bytes(&mut out, self.rationale.as_bytes())?;
        Ok(out)
    }

    /// Computes the exact digest carried by every signed approval event.
    pub fn digest(&self) -> Result<Digest32, EngineError> {
        Ok(Sha256::digest(self.canonical_bytes()?).into())
    }

    /// Builds the unsigned Nostr event project authorities sign. The event
    /// binds the intent digest, contribution, project, and policy version.
    pub fn approval_event_builder(&self) -> Result<EventBuilder, EngineError> {
        let digest = hex::encode(self.digest()?);
        let contribution = hex::encode(self.contribution_id);
        let tags = [
            Tag::parse(["a", self.project.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
            Tag::parse(["e", contribution.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
            Tag::parse(["st8-policy", self.policy_version.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
            Tag::parse(["st8-decision", digest.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
        ];
        Ok(EventBuilder::new(Kind::Custom(KIND_ST8_GOVERNANCE_APPROVAL), digest).tags(tags))
    }

    fn from_decision(decision: &ContributionDecision) -> Self {
        Self {
            contribution_id: decision.contribution_id,
            project: decision.project.clone(),
            status: decision.status,
            contribution_units: decision.contribution_units,
            policy_version: decision.policy_version.clone(),
            decided_at: decision.decided_at,
            rationale: decision.rationale.clone(),
        }
    }
}

/// Configurable founder approval policy. It is project governance, not a DAO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernancePolicy {
    /// Project this policy controls.
    pub project: String,
    /// Stable policy version recorded in each decision.
    pub version: String,
    /// Authorized founder/project-authority identities.
    pub founders: Vec<String>,
    /// Approvals required for normal contribution valuations.
    pub normal_threshold: u16,
    /// Approvals required at or above `large_unit_threshold`.
    pub large_threshold: u16,
    /// CU amount that activates the large threshold.
    pub large_unit_threshold: u64,
}

impl GovernancePolicy {
    /// Validates internal policy invariants.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.project.trim().is_empty() || self.version.trim().is_empty() {
            return Err(EngineError::InvalidGovernancePolicy);
        }
        let founders = self.canonical_founders()?;
        let founder_count =
            u16::try_from(founders.len()).map_err(|_| EngineError::InvalidGovernancePolicy)?;
        if self.normal_threshold == 0
            || self.large_threshold == 0
            || self.normal_threshold > founder_count
            || self.large_threshold > founder_count
            || self.large_threshold < self.normal_threshold
        {
            return Err(EngineError::InvalidGovernancePolicy);
        }
        Ok(())
    }

    /// Creates the exact proposal project authorities must sign.
    pub fn decision_intent(
        &self,
        record: &ContributionRecord,
        status: DecisionStatus,
        contribution_units: u64,
        decided_at: i64,
        rationale: String,
    ) -> Result<GovernanceDecisionIntent, EngineError> {
        self.validate()?;
        if record.project != self.project {
            return Err(EngineError::ProjectMismatch);
        }
        let intent = GovernanceDecisionIntent {
            contribution_id: record.id()?,
            project: self.project.clone(),
            status,
            contribution_units,
            policy_version: self.version.clone(),
            decided_at,
            rationale,
        };
        let _ = intent.digest()?;
        Ok(intent)
    }

    /// Creates and validates a decision from signed project-authority approvals
    /// without altering the original contribution record.
    pub fn decide(
        &self,
        record: &ContributionRecord,
        intent: &GovernanceDecisionIntent,
        approval_events: &[Event],
    ) -> Result<ContributionDecision, EngineError> {
        self.validate()?;
        if intent.project != self.project
            || intent.policy_version != self.version
            || intent.contribution_id != record.id()?
        {
            return Err(EngineError::ProjectMismatch);
        }
        let approvers = self.verify_approval_events(record, intent, approval_events)?;
        let decision = ContributionDecision {
            contribution_id: intent.contribution_id,
            project: intent.project.clone(),
            status: intent.status,
            contribution_units: intent.contribution_units,
            policy_version: intent.policy_version.clone(),
            approvers,
            decided_at: intent.decided_at,
            rationale: intent.rationale.clone(),
        };
        self.verify_decision(record, &decision, approval_events)?;
        Ok(decision)
    }

    /// Independently verifies a decision and its signed approval events against
    /// this exact policy version.
    pub fn verify_decision(
        &self,
        record: &ContributionRecord,
        decision: &ContributionDecision,
        approval_events: &[Event],
    ) -> Result<(), EngineError> {
        self.validate()?;
        decision.validate_for(record)?;
        if self.project != record.project || decision.policy_version != self.version {
            return Err(EngineError::ProjectMismatch);
        }
        let intent = GovernanceDecisionIntent::from_decision(decision);
        let approvers = self.verify_approval_events(record, &intent, approval_events)?;
        if approvers != decision.canonical_approvers()? {
            return Err(EngineError::GovernanceApprovalMismatch);
        }
        let required = if decision.contribution_units >= self.large_unit_threshold {
            self.large_threshold
        } else {
            self.normal_threshold
        };
        if approvers.len() < usize::from(required) {
            return Err(EngineError::InsufficientApprovals {
                required,
                actual: approvers.len(),
            });
        }
        Ok(())
    }

    fn verify_approval_events(
        &self,
        record: &ContributionRecord,
        intent: &GovernanceDecisionIntent,
        approval_events: &[Event],
    ) -> Result<Vec<String>, EngineError> {
        if intent.project != self.project
            || intent.policy_version != self.version
            || intent.contribution_id != record.id()?
        {
            return Err(EngineError::ProjectMismatch);
        }
        let expected_digest = hex::encode(intent.digest()?);
        let expected_contribution = hex::encode(intent.contribution_id);
        let founders = self.canonical_founders()?;
        let mut approvers = BTreeSet::new();
        for event in approval_events {
            verify_event(event)
                .map_err(|error| EngineError::InvalidGovernanceApproval(error.to_string()))?;
            if event.kind.as_u16() != KIND_ST8_GOVERNANCE_APPROVAL
                || event.content != expected_digest
                || single_tag_value(event, "a") != Some(intent.project.as_str())
                || single_tag_value(event, "e") != Some(expected_contribution.as_str())
                || single_tag_value(event, "st8-policy") != Some(intent.policy_version.as_str())
                || single_tag_value(event, "st8-decision") != Some(expected_digest.as_str())
            {
                return Err(EngineError::InvalidGovernanceApproval(
                    "approval event does not bind the exact decision intent".into(),
                ));
            }
            let approval_time = i64::try_from(event.created_at.as_secs())
                .map_err(|_| EngineError::InvalidGovernanceApproval("timestamp overflow".into()))?;
            if approval_time < record.created_at {
                return Err(EngineError::GovernanceApprovalBeforeContribution);
            }
            if approval_time > intent.decided_at {
                return Err(EngineError::GovernanceApprovalAfterDecision);
            }
            let approver = format!("nostr:{}", event.pubkey.to_hex());
            if founders.binary_search(&approver).is_err() {
                return Err(EngineError::UnauthorizedApprover);
            }
            approvers.insert(approver);
        }
        Ok(approvers.into_iter().collect())
    }

    fn canonical_founders(&self) -> Result<Vec<String>, EngineError> {
        let mut founders = BTreeSet::new();
        for founder in &self.founders {
            if founder.trim().is_empty() || founder.as_bytes().contains(&0) {
                return Err(EngineError::InvalidGovernancePolicy);
            }
            founders.insert(founder.clone());
        }
        if founders.is_empty() {
            return Err(EngineError::InvalidGovernancePolicy);
        }
        Ok(founders.into_iter().collect())
    }
}

/// Deterministic accepted contribution state committed to BSV.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContributionSnapshot {
    /// Project identity.
    pub project: String,
    /// Preserved original contribution record.
    pub record: ContributionRecord,
    /// Project governance decision.
    pub decision: ContributionDecision,
}

/// File-oriented input for preparing a real contribution anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContributionProposal {
    /// Full signed NIP-MP project event.
    pub project_event: Event,
    /// Full signed Buzz events supporting the contribution.
    pub evidence_events: Vec<Event>,
    /// Explicit project governance policy.
    pub governance_policy: GovernancePolicy,
    /// Contributor identity, normally `nostr:<pubkey-hex>`.
    pub contributor: String,
    /// Contributor identity category.
    pub contributor_kind: ContributorKind,
    /// Contribution category.
    pub class: ContributionClass,
    /// Contribution timestamp in Unix seconds.
    pub created_at: i64,
    /// Contribution summary.
    pub summary: String,
    /// Governance result.
    pub status: DecisionStatus,
    /// Non-transferable Contribution Units awarded.
    pub contribution_units: u64,
    /// Full signed project-authority approval events. Approver identities are
    /// derived from verified event authors, never trusted from caller strings.
    #[serde(default)]
    pub approval_events: Vec<Event>,
    /// Decision timestamp in Unix seconds.
    pub decided_at: i64,
    /// Decision rationale.
    pub rationale: String,
}

impl ContributionProposal {
    /// Verifies signed Buzz evidence and returns the exact governance intent
    /// founders must sign before the contribution can be prepared.
    pub fn governance_intent(&self) -> Result<GovernanceDecisionIntent, EngineError> {
        let (project_context, record) = self.grounded_record()?;
        if self.governance_policy.project != project_context.project {
            return Err(EngineError::ProjectMismatch);
        }
        self.governance_policy.decision_intent(
            &record,
            self.status,
            self.contribution_units,
            self.decided_at,
            self.rationale.clone(),
        )
    }

    /// Verifies the proposal and prepares the deterministic BSV testnet payload.
    pub fn prepare(self) -> Result<PreparedContributionAnchor, EngineError> {
        let (project_context, record) = self.grounded_record()?;
        let intent = self.governance_policy.decision_intent(
            &record,
            self.status,
            self.contribution_units,
            self.decided_at,
            self.rationale.clone(),
        )?;
        let decision = self
            .governance_policy
            .decide(&record, &intent, &self.approval_events)?;
        let snapshot = ContributionSnapshot::new(record, decision)?;
        PreparedContributionAnchor::new(
            project_context,
            self.evidence_events,
            self.approval_events,
            self.governance_policy,
            snapshot,
            BsvNetwork::Testnet,
        )
    }

    fn grounded_record(&self) -> Result<(BuzzProjectContext, ContributionRecord), EngineError> {
        let project_context = BuzzProjectContext::from_event(&self.project_event)?;
        let record = project_context.contribution_from_events(
            self.contributor.clone(),
            self.contributor_kind,
            self.class,
            self.created_at,
            self.summary.clone(),
            &self.evidence_events,
        )?;
        Ok((project_context, record))
    }
}

/// Fully verified contribution state waiting for an external wallet and broadcaster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedContributionAnchor {
    /// Signed project context.
    pub project_context: BuzzProjectContext,
    /// Full signed contribution evidence.
    pub evidence_events: Vec<Event>,
    /// Full signed project-governance approval events.
    pub approval_events: Vec<Event>,
    /// Governance policy.
    pub governance_policy: GovernancePolicy,
    /// Accepted contribution snapshot.
    pub snapshot: ContributionSnapshot,
    /// Project-scoped commitment.
    pub commitment: ProjectCommitment,
    /// Inclusion proof.
    pub merkle_proof: MerkleProof,
    /// Wallet-ready BSV testnet payload.
    pub anchor_payload: AnchorPayload,
}

impl PreparedContributionAnchor {
    /// Builds deterministic anchor material without wallet or network I/O.
    pub fn new(
        project_context: BuzzProjectContext,
        evidence_events: Vec<Event>,
        approval_events: Vec<Event>,
        governance_policy: GovernancePolicy,
        snapshot: ContributionSnapshot,
        network: BsvNetwork,
    ) -> Result<Self, EngineError> {
        if network != BsvNetwork::Testnet {
            return Err(EngineError::FirstSliceRequiresTestnet);
        }
        project_context.verify()?;
        verify_grounded_record_evidence(&project_context, &snapshot.record, &evidence_events)?;
        governance_policy.verify_decision(
            &snapshot.record,
            &snapshot.decision,
            &approval_events,
        )?;
        if project_context.project != snapshot.project
            || governance_policy.project != snapshot.project
        {
            return Err(EngineError::ProjectMismatch);
        }
        let commitment = snapshot.commitment()?;
        let batch = MerkleBatch::new(vec![commitment.clone()])?;
        let merkle_proof = batch.proof_for(&commitment)?;
        let anchor_payload =
            AnchorPayload::new(network, &snapshot.project, &batch, snapshot.id()?)?;
        let prepared = Self {
            project_context,
            evidence_events,
            approval_events,
            governance_policy,
            snapshot,
            commitment,
            merkle_proof,
            anchor_payload,
        };
        prepared.verify()?;
        Ok(prepared)
    }

    /// Independently verifies all pre-transaction state.
    pub fn verify(&self) -> Result<(), EngineError> {
        self.project_context.verify()?;
        verify_grounded_record_evidence(
            &self.project_context,
            &self.snapshot.record,
            &self.evidence_events,
        )?;
        self.governance_policy.verify_decision(
            &self.snapshot.record,
            &self.snapshot.decision,
            &self.approval_events,
        )?;
        if self.project_context.project != self.snapshot.project
            || self.governance_policy.project != self.snapshot.project
            || self.commitment != self.snapshot.commitment()?
        {
            return Err(EngineError::SnapshotMismatch);
        }
        self.merkle_proof
            .verify(&self.commitment, self.anchor_payload.merkle_root)?;
        self.anchor_payload.verify_project(&self.snapshot.project)?;
        if self.anchor_payload.network != BsvNetwork::Testnet
            || self.anchor_payload.snapshot_id != self.snapshot.id()?
            || self.anchor_payload.leaf_count != self.merkle_proof.leaf_count
        {
            return Err(EngineError::SnapshotMismatch);
        }
        Ok(())
    }

    /// Finalizes a receipt with wallet output and a normalized broadcaster response.
    pub fn finalize(
        self,
        transaction: SignedAnchorTransaction,
        broadcast: BroadcastReceipt,
    ) -> Result<ContributionAnchorReceipt, EngineError> {
        self.verify()?;
        verify_anchor_transaction(&transaction, &self.anchor_payload)?;
        if !broadcast.accepted {
            return Err(EngineError::BroadcastRejected(broadcast.status));
        }
        let receipt = ContributionAnchorReceipt {
            version: RECEIPT_VERSION,
            project_context: self.project_context,
            evidence_events: self.evidence_events,
            approval_events: self.approval_events,
            governance_policy: self.governance_policy,
            snapshot: self.snapshot,
            commitment: self.commitment,
            merkle_proof: self.merkle_proof,
            anchor_payload: self.anchor_payload,
            transaction,
            broadcast,
        };
        receipt.verify()?;
        Ok(receipt)
    }
}

impl ContributionSnapshot {
    /// Creates a snapshot after validating record-decision binding.
    pub fn new(
        record: ContributionRecord,
        decision: ContributionDecision,
    ) -> Result<Self, EngineError> {
        decision.validate_for(&record)?;
        Ok(Self {
            project: record.project.clone(),
            record,
            decision,
        })
    }

    /// Canonical state bytes independent from JSON representation.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, EngineError> {
        let record = self.record.canonical_bytes()?;
        let decision = self.decision.canonical_bytes()?;
        let mut out = Vec::new();
        out.extend_from_slice(SNAPSHOT_DOMAIN);
        put_bytes(&mut out, self.project.as_bytes())?;
        put_bytes(&mut out, &record)?;
        put_bytes(&mut out, &decision)?;
        Ok(out)
    }

    /// Digest of canonical contribution state.
    pub fn digest(&self) -> Result<Digest32, EngineError> {
        Ok(Sha256::digest(self.canonical_bytes()?).into())
    }

    /// Stable snapshot ID. V1 deliberately equals the state digest.
    pub fn id(&self) -> Result<Digest32, EngineError> {
        self.digest()
    }

    /// Creates the project-scoped provenance commitment.
    pub fn commitment(&self) -> Result<ProjectCommitment, EngineError> {
        Ok(ProjectCommitment {
            project: self.project.clone(),
            kind: CommitmentKind::ContributionSnapshot,
            object_id: self.id()?,
            state_digest: self.digest()?,
        })
    }
}

/// Complete persisted evidence-to-anchor receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContributionAnchorReceipt {
    /// Receipt schema version.
    pub version: u16,
    /// Signed Buzz project context.
    pub project_context: BuzzProjectContext,
    /// Full signed Buzz evidence retained for independent verification.
    pub evidence_events: Vec<Event>,
    /// Full signed project-governance approvals retained for independent verification.
    pub approval_events: Vec<Event>,
    /// Governance policy used for acceptance.
    pub governance_policy: GovernancePolicy,
    /// Preserved contribution state.
    pub snapshot: ContributionSnapshot,
    /// Project-scoped commitment.
    pub commitment: ProjectCommitment,
    /// Inclusion proof for the commitment.
    pub merkle_proof: MerkleProof,
    /// Canonical BSV anchor payload.
    pub anchor_payload: AnchorPayload,
    /// Signed BSV transaction material.
    pub transaction: SignedAnchorTransaction,
    /// ARC or other broadcaster response.
    pub broadcast: BroadcastReceipt,
}

impl ContributionAnchorReceipt {
    /// Executes the wallet and broadcaster boundaries and constructs a receipt.
    pub fn anchor<P: AnchorTransactionProvider, B: AnchorBroadcaster>(
        prepared: PreparedContributionAnchor,
        provider: &P,
        broadcaster: &B,
    ) -> Result<Self, EngineError> {
        prepared.verify()?;
        let network = prepared.anchor_payload.network;
        let transaction = provider.create_anchor_transaction(network, &prepared.anchor_payload)?;
        verify_anchor_transaction(&transaction, &prepared.anchor_payload)?;
        let broadcast = broadcaster.broadcast(network, &transaction)?;
        prepared.finalize(transaction, broadcast)
    }

    /// Independently verifies contribution identity, project scope, governance,
    /// CU award, snapshot commitment, Merkle inclusion, BSV txid, and payload.
    pub fn verify(&self) -> Result<(), EngineError> {
        if self.version != RECEIPT_VERSION {
            return Err(EngineError::UnsupportedReceiptVersion(self.version));
        }
        self.project_context.verify()?;
        verify_grounded_record_evidence(
            &self.project_context,
            &self.snapshot.record,
            &self.evidence_events,
        )?;
        if self.project_context.project != self.snapshot.project
            || self.governance_policy.project != self.snapshot.project
            || self.commitment.project != self.snapshot.project
        {
            return Err(EngineError::ProjectMismatch);
        }
        self.governance_policy.verify_decision(
            &self.snapshot.record,
            &self.snapshot.decision,
            &self.approval_events,
        )?;
        if self.snapshot.decision.status == DecisionStatus::Rejected
            || self.snapshot.decision.contribution_units == 0
        {
            return Err(EngineError::ReceiptRequiresAcceptedContribution);
        }
        if self.commitment != self.snapshot.commitment()? {
            return Err(EngineError::SnapshotMismatch);
        }
        self.merkle_proof
            .verify(&self.commitment, self.anchor_payload.merkle_root)?;
        self.anchor_payload.verify_project(&self.snapshot.project)?;
        if self.anchor_payload.network != BsvNetwork::Testnet
            || self.anchor_payload.snapshot_id != self.snapshot.id()?
            || self.anchor_payload.leaf_count != self.merkle_proof.leaf_count
        {
            return Err(EngineError::SnapshotMismatch);
        }
        verify_anchor_transaction(&self.transaction, &self.anchor_payload)?;
        if !self.broadcast.accepted
            || self.broadcast.provider.trim().is_empty()
            || self.broadcast.status.trim().is_empty()
        {
            return Err(EngineError::BroadcastRejected(
                self.broadcast.status.clone(),
            ));
        }
        Ok(())
    }

    /// Atomically persists pretty JSON by writing a sibling temporary file and renaming it.
    pub fn persist_json(&self, path: &Path) -> Result<(), EngineError> {
        self.verify()?;
        let parent = path.parent().ok_or(EngineError::InvalidReceiptPath)?;
        fs::create_dir_all(parent)?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(EngineError::InvalidReceiptPath)?;
        let temporary = parent.join(format!(".{file_name}.tmp"));
        let bytes = serde_json::to_vec_pretty(self)?;
        fs::write(&temporary, bytes)?;
        fs::rename(&temporary, path)?;
        Ok(())
    }

    /// Loads and independently verifies a persisted receipt.
    pub fn load_verified(path: &Path) -> Result<Self, EngineError> {
        let bytes = fs::read(path)?;
        let receipt: Self = serde_json::from_slice(&bytes)?;
        receipt.verify()?;
        Ok(receipt)
    }
}

/// Vertical-slice failures.
#[derive(Debug, Error)]
pub enum EngineError {
    /// Signed Buzz event ID or signature was invalid.
    #[error("invalid signed Buzz event: {0}")]
    InvalidBuzzEvent(String),
    /// Expected a kind-30621 NIP-MP project event.
    #[error("expected a Buzz NIP-MP project event")]
    NotProjectEvent,
    /// Project event did not satisfy Buzz's current envelope rules.
    #[error("invalid Buzz project event: {0}")]
    InvalidProjectEvent(String),
    /// Evidence was signed but not tied to the project or a listed repository.
    #[error("Buzz evidence is not grounded in the project")]
    UngroundedEvidence,
    /// Persisted evidence references did not match the contribution.
    #[error("persisted Buzz evidence does not match the contribution record")]
    EvidenceMismatch,
    /// Contributor identity was not among the signed evidence authors.
    #[error("contributor identity does not match signed Buzz evidence")]
    ContributorMismatch,
    /// Event ID could not be decoded.
    #[error("invalid Buzz event digest")]
    InvalidEventDigest,
    /// Governance policy is internally invalid.
    #[error("invalid governance policy")]
    InvalidGovernancePolicy,
    /// Governance intent fields violate protocol invariants.
    #[error("invalid governance decision intent")]
    InvalidGovernanceIntent,
    /// A signed approval event was invalid or did not bind the exact intent.
    #[error("invalid signed governance approval: {0}")]
    InvalidGovernanceApproval(String),
    /// Signed approval authors did not exactly match the persisted decision.
    #[error("signed governance approvals do not match decision approvers")]
    GovernanceApprovalMismatch,
    /// Approval predates the contribution it attempts to accept.
    #[error("governance approval predates the contribution")]
    GovernanceApprovalBeforeContribution,
    /// Approval was signed after the decision's approval window closed.
    #[error("governance approval was signed after the decision timestamp")]
    GovernanceApprovalAfterDecision,
    /// Decision contains an approver not authorized by the policy.
    #[error("decision contains an unauthorized approver")]
    UnauthorizedApprover,
    /// Approval threshold was not met.
    #[error("insufficient approvals: required {required}, got {actual}")]
    InsufficientApprovals {
        /// Required approvals.
        required: u16,
        /// Actual unique approvals.
        actual: usize,
    },
    /// Project scope did not match across objects.
    #[error("project scope mismatch")]
    ProjectMismatch,
    /// Receipt snapshot or anchor fields were inconsistent.
    #[error("snapshot commitment mismatch")]
    SnapshotMismatch,
    /// A first-slice receipt must represent an accepted CU award.
    #[error("receipt requires an accepted contribution with non-zero CU")]
    ReceiptRequiresAcceptedContribution,
    /// First vertical slice intentionally forbids mainnet.
    #[error("first contribution anchor slice requires BSV testnet")]
    FirstSliceRequiresTestnet,
    /// Broadcaster did not accept the transaction.
    #[error("BSV broadcaster rejected the transaction: {0}")]
    BroadcastRejected(String),
    /// Unsupported persisted schema.
    #[error("unsupported receipt version {0}")]
    UnsupportedReceiptVersion(u16),
    /// Persistence target was not a valid file path.
    #[error("invalid receipt path")]
    InvalidReceiptPath,
    /// Contribution protocol error.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    /// BSV provenance protocol error.
    #[error(transparent)]
    Provenance(#[from] ProvenanceError),
    /// Receipt JSON error.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Receipt filesystem error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn project_coordinate(event: &Event) -> Option<String> {
    tag_values(event, "d")
        .next()
        .map(|slug| format!("30621:{}:{slug}", event.pubkey.to_hex()))
}

fn tag_values<'a>(event: &'a Event, name: &'a str) -> impl Iterator<Item = &'a str> {
    event.tags.iter().filter_map(move |tag| {
        let parts = tag.as_slice();
        (parts.first().map(String::as_str) == Some(name))
            .then(|| parts.get(1).map(String::as_str))
            .flatten()
    })
}

fn single_tag_value<'a>(event: &'a Event, name: &'a str) -> Option<&'a str> {
    let mut values = tag_values(event, name);
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn event_digest(event: &Event) -> Result<Digest32, EngineError> {
    let bytes = hex::decode(event.id.to_hex()).map_err(|_| EngineError::InvalidEventDigest)?;
    bytes
        .try_into()
        .map_err(|_| EngineError::InvalidEventDigest)
}

fn put_bytes(out: &mut Vec<u8>, value: &[u8]) -> Result<(), EngineError> {
    let len = u32::try_from(value.len()).map_err(|_| ProtocolError::LengthOverflow)?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(value);
    Ok(())
}

fn verify_grounded_record_evidence(
    context: &BuzzProjectContext,
    record: &ContributionRecord,
    events: &[Event],
) -> Result<(), EngineError> {
    if events.is_empty() {
        return Err(EngineError::UngroundedEvidence);
    }
    let expected: BTreeSet<EvidenceRef> = events
        .iter()
        .map(|event| context.evidence_ref(event))
        .collect::<Result<_, _>>()?;
    let actual: BTreeSet<EvidenceRef> = record.canonical_evidence()?.into_iter().collect();
    if expected != actual {
        return Err(EngineError::EvidenceMismatch);
    }
    let contributor_pubkey = record
        .contributor
        .strip_prefix("nostr:")
        .ok_or(EngineError::ContributorMismatch)?;
    if !events
        .iter()
        .any(|event| event.pubkey.to_hex() == contributor_pubkey)
    {
        return Err(EngineError::ContributorMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_core::{Keys, Kind};
    use buzz_sdk::{build_project, ProjectMemberCoord};
    use nostr::{EventBuilder, JsonUtil, Tag};
    use std::sync::Mutex;
    use tempfile::tempdir;

    fn fixture() -> (
        BuzzProjectContext,
        Event,
        GovernancePolicy,
        ContributionRecord,
        Vec<Keys>,
    ) {
        let founder_a = Keys::generate();
        let founder_b = Keys::generate();
        let contributor = Keys::generate();
        let repo = format!("30617:{}:st8wrx", contributor.public_key().to_hex());
        let member = ProjectMemberCoord::parse_full(&repo).expect("repo coordinate");
        let project_event = build_project(
            "st8wrx",
            Some("ST8WRX"),
            Some("Build together. Prove what you created."),
            &[member],
            None,
            Some("listed"),
        )
        .expect("project builder")
        .sign_with_keys(&founder_a)
        .expect("sign project");
        let context = BuzzProjectContext::from_event(&project_event).expect("project context");

        let evidence_event = EventBuilder::new(
            Kind::Custom(buzz_core::kind::KIND_GIT_PATCH as u16),
            "deterministic contribution protocol patch",
        )
        .tags([Tag::parse(["a", repo.as_str()]).expect("a tag")])
        .sign_with_keys(&contributor)
        .expect("sign evidence");

        let policy = GovernancePolicy {
            project: context.project.clone(),
            version: "founders-v1".into(),
            founders: vec![
                format!("nostr:{}", founder_a.public_key().to_hex()),
                format!("nostr:{}", founder_b.public_key().to_hex()),
            ],
            normal_threshold: 1,
            large_threshold: 2,
            large_unit_threshold: 1_000,
        };
        let record = context
            .contribution_from_events(
                format!("nostr:{}", contributor.public_key().to_hex()),
                ContributorKind::Human,
                ContributionClass::Engineering,
                i64::try_from(evidence_event.created_at.as_secs()).expect("timestamp"),
                "Implemented the deterministic protocol foundation".into(),
                std::slice::from_ref(&evidence_event),
            )
            .expect("record");
        (
            context,
            evidence_event,
            policy,
            record,
            vec![founder_a, founder_b],
        )
    }

    fn signed_approval(
        intent: &GovernanceDecisionIntent,
        founder: &Keys,
        created_at: i64,
    ) -> Event {
        intent
            .approval_event_builder()
            .expect("approval builder")
            .custom_created_at(nostr::Timestamp::from(
                u64::try_from(created_at).expect("positive timestamp"),
            ))
            .sign_with_keys(founder)
            .expect("sign approval")
    }

    struct FixtureProvider;

    impl AnchorTransactionProvider for FixtureProvider {
        fn create_anchor_transaction(
            &self,
            _network: BsvNetwork,
            payload: &AnchorPayload,
        ) -> Result<SignedAnchorTransaction, ProvenanceError> {
            let script = payload.locking_script()?;
            let mut raw = Vec::new();
            raw.extend_from_slice(&1_i32.to_le_bytes());
            raw.push(0);
            raw.push(1);
            raw.extend_from_slice(&0_u64.to_le_bytes());
            raw.push(u8::try_from(script.len()).map_err(|_| {
                ProvenanceError::Provider("fixture script is unexpectedly large".into())
            })?);
            raw.extend_from_slice(&script);
            raw.extend_from_slice(&0_u32.to_le_bytes());
            Ok(SignedAnchorTransaction {
                txid: st8_bsv_provenance::transaction_id(&raw),
                raw_transaction: raw,
                atomic_beef: Some(vec![1, 1, 1, 1]),
                anchor_output_index: 0,
            })
        }
    }

    struct FixtureBroadcaster {
        calls: Mutex<u32>,
    }

    impl AnchorBroadcaster for FixtureBroadcaster {
        fn broadcast(
            &self,
            _network: BsvNetwork,
            _transaction: &SignedAnchorTransaction,
        ) -> Result<BroadcastReceipt, ProvenanceError> {
            let mut calls = self.calls.lock().map_err(|_| {
                ProvenanceError::Provider("fixture broadcaster lock poisoned".into())
            })?;
            *calls += 1;
            Ok(BroadcastReceipt {
                accepted: true,
                status: "SEEN_ON_NETWORK".into(),
                provider: "fixture-arc".into(),
            })
        }
    }

    fn receipt() -> ContributionAnchorReceipt {
        let (context, event, policy, record, founders) = fixture();
        let intent = policy
            .decision_intent(
                &record,
                DecisionStatus::Accepted,
                500,
                1_800_000_000,
                "Grounded engineering contribution accepted".into(),
            )
            .expect("intent");
        let approvals = vec![signed_approval(
            &intent,
            &founders[0],
            record.created_at + 1,
        )];
        let decision = policy
            .decide(&record, &intent, &approvals)
            .expect("decision");
        let snapshot = ContributionSnapshot::new(record, decision).expect("snapshot");
        let prepared = PreparedContributionAnchor::new(
            context,
            vec![event],
            approvals,
            policy,
            snapshot,
            BsvNetwork::Testnet,
        )
        .expect("prepared");
        ContributionAnchorReceipt::anchor(
            prepared,
            &FixtureProvider,
            &FixtureBroadcaster {
                calls: Mutex::new(0),
            },
        )
        .expect("receipt")
    }

    #[test]
    fn signed_buzz_evidence_drives_complete_receipt() {
        receipt().verify().expect("verify");
    }

    #[test]
    fn unrelated_signed_buzz_event_is_rejected() {
        let (context, _event, _policy, _record, _founders) = fixture();
        let keys = Keys::generate();
        let unrelated = EventBuilder::new(Kind::TextNote, "unrelated")
            .tags([])
            .sign_with_keys(&keys)
            .expect("sign");
        assert!(matches!(
            context.evidence_ref(&unrelated),
            Err(EngineError::UngroundedEvidence)
        ));
    }

    #[test]
    fn large_award_requires_large_threshold() {
        let (_context, _event, policy, record, founders) = fixture();
        let intent = policy
            .decision_intent(
                &record,
                DecisionStatus::Accepted,
                1_000,
                1_800_000_000,
                "large award".into(),
            )
            .expect("intent");
        let approvals = vec![signed_approval(
            &intent,
            &founders[0],
            record.created_at + 1,
        )];
        assert!(matches!(
            policy.decide(&record, &intent, &approvals),
            Err(EngineError::InsufficientApprovals { required: 2, .. })
        ));
    }

    #[test]
    fn duplicate_founder_approvals_do_not_meet_large_threshold() {
        let (_context, _event, policy, record, founders) = fixture();
        let intent = policy
            .decision_intent(
                &record,
                DecisionStatus::Accepted,
                1_000,
                1_800_000_000,
                "large award".into(),
            )
            .expect("intent");
        let approval = signed_approval(&intent, &founders[0], record.created_at + 1);
        assert!(matches!(
            policy.decide(&record, &intent, &[approval.clone(), approval]),
            Err(EngineError::InsufficientApprovals {
                required: 2,
                actual: 1
            })
        ));
    }

    #[test]
    fn approval_cannot_be_replayed_for_different_units() {
        let (_context, _event, policy, record, founders) = fixture();
        let approved_intent = policy
            .decision_intent(
                &record,
                DecisionStatus::Accepted,
                500,
                1_800_000_000,
                "approved amount".into(),
            )
            .expect("approved intent");
        let approval = signed_approval(&approved_intent, &founders[0], record.created_at + 1);
        let changed_intent = policy
            .decision_intent(
                &record,
                DecisionStatus::Accepted,
                501,
                1_800_000_000,
                "approved amount".into(),
            )
            .expect("changed intent");
        assert!(matches!(
            policy.decide(&record, &changed_intent, &[approval]),
            Err(EngineError::InvalidGovernanceApproval(_))
        ));
    }

    #[test]
    fn receipt_round_trip_persists_and_independently_verifies() {
        let receipt = receipt();
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("receipt.json");
        receipt.persist_json(&path).expect("persist");
        let loaded = ContributionAnchorReceipt::load_verified(&path).expect("load");
        assert_eq!(receipt, loaded);
    }

    #[test]
    fn project_tampering_fails_receipt_verification() {
        let mut receipt = receipt();
        receipt.snapshot.project = "30621:attacker:other".into();
        assert!(matches!(
            receipt.verify(),
            Err(EngineError::ProjectMismatch)
        ));
    }

    #[test]
    fn merkle_tampering_fails_receipt_verification() {
        let mut receipt = receipt();
        receipt.merkle_proof.leaf_hash[0] ^= 1;
        assert!(matches!(
            receipt.verify(),
            Err(EngineError::Provenance(ProvenanceError::LeafMismatch))
        ));
    }

    #[test]
    fn bsv_transaction_tampering_fails_receipt_verification() {
        let mut receipt = receipt();
        receipt.transaction.raw_transaction[0] ^= 1;
        assert!(matches!(
            receipt.verify(),
            Err(EngineError::Provenance(
                ProvenanceError::TransactionIdMismatch
            ))
        ));
    }

    #[test]
    fn signed_event_tampering_is_rejected() {
        let (context, event, _policy, _record, _founders) = fixture();
        let mut json: serde_json::Value =
            serde_json::from_str(&event.as_json()).expect("event json");
        json["content"] = serde_json::Value::String("tampered".into());
        let tampered = Event::from_json(json.to_string()).expect("parse tampered");
        assert!(matches!(
            context.evidence_ref(&tampered),
            Err(EngineError::InvalidBuzzEvent(_))
        ));
    }

    #[test]
    fn signed_governance_approval_tampering_is_rejected() {
        let mut receipt = receipt();
        let mut json: serde_json::Value =
            serde_json::from_str(&receipt.approval_events[0].as_json()).expect("approval json");
        json["content"] = serde_json::Value::String("00".repeat(32));
        receipt.approval_events[0] =
            Event::from_json(json.to_string()).expect("parse tampered approval");
        assert!(matches!(
            receipt.verify(),
            Err(EngineError::InvalidGovernanceApproval(_))
        ));
    }
}
