#![deny(unsafe_code)]
#![warn(missing_docs)]
//! First ST8WRX contribution vertical slice.
//!
//! The engine consumes already-signed Buzz events, grounds them in a signed
//! NIP-MP project event, applies explicit human project governance, awards
//! non-transferable Contribution Units, prepares a project-scoped Merkle anchor,
//! persists a receipt, and independently re-verifies the complete chain.

use buzz_core::{
    kind::{
        KIND_ST8_CONTRIBUTION_CLAIM, KIND_ST8_DECISION_PROPOSAL, KIND_ST8_GOVERNANCE_APPROVAL,
        KIND_ST8_GOVERNANCE_POLICY,
    },
    verify_event, Event,
};
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
const PROJECT_SNAPSHOT_DOMAIN: &[u8] = b"ST8WRX\0PROJECT_LEDGER_SNAPSHOT\0V1";
const GOVERNANCE_INTENT_DOMAIN: &[u8] = b"ST8WRX\0GOVERNANCE_INTENT\0V1";
const RECEIPT_VERSION: u16 = 1;

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

/// Signed contributor claim body. Identity, project, time, and evidence IDs
/// are derived from the event envelope rather than trusted from this JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContributionClaimBody {
    /// Contributor category.
    pub contributor_kind: ContributorKind,
    /// Contribution category.
    pub class: ContributionClass,
    /// Human-readable contribution summary bound into the resulting record.
    pub summary: String,
}

impl ContributionClaimBody {
    /// Builds the unsigned claim event over exact project-scoped evidence IDs.
    pub fn event_builder(
        &self,
        project: &str,
        evidence_events: &[Event],
    ) -> Result<EventBuilder, EngineError> {
        if evidence_events.is_empty() || self.summary.trim().is_empty() {
            return Err(EngineError::InvalidContributionClaim(
                "claim requires a summary and evidence".into(),
            ));
        }
        let mut evidence_ids: Vec<String> = evidence_events
            .iter()
            .map(|event| event.id.to_hex())
            .collect();
        evidence_ids.sort();
        evidence_ids.dedup();
        if evidence_ids.len() != evidence_events.len() {
            return Err(EngineError::InvalidContributionClaim(
                "duplicate evidence event".into(),
            ));
        }
        let mut tags = Vec::with_capacity(evidence_ids.len() + 1);
        tags.push(
            Tag::parse(["a", project])
                .map_err(|_| EngineError::InvalidContributionClaim("invalid project".into()))?,
        );
        for event_id in evidence_ids {
            tags.push(Tag::parse(["e", event_id.as_str()]).map_err(|_| {
                EngineError::InvalidContributionClaim("invalid evidence id".into())
            })?);
        }
        Ok(EventBuilder::new(
            Kind::Custom(KIND_ST8_CONTRIBUTION_CLAIM as u16),
            serde_json::to_string(self)?,
        )
        .tags(tags))
    }
}

/// Verified contributor claim and the deterministic contribution record it creates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContributionClaimContext {
    /// Full contributor-signed claim event.
    pub claim_event: Event,
    /// Deterministic record derived from the event and its evidence.
    pub record: ContributionRecord,
}

impl ContributionClaimContext {
    /// Verifies a claim and derives its contribution record from signed fields.
    pub fn from_event(
        project_context: &BuzzProjectContext,
        claim_event: &Event,
        evidence_events: &[Event],
    ) -> Result<Self, EngineError> {
        project_context.verify()?;
        verify_event(claim_event)
            .map_err(|error| EngineError::InvalidContributionClaim(error.to_string()))?;
        if u32::from(claim_event.kind.as_u16()) != KIND_ST8_CONTRIBUTION_CLAIM
            || single_tag_value(claim_event, "a") != Some(project_context.project.as_str())
        {
            return Err(EngineError::InvalidContributionClaim(
                "claim kind or project mismatch".into(),
            ));
        }
        let body: ContributionClaimBody = serde_json::from_str(&claim_event.content)
            .map_err(|error| EngineError::InvalidContributionClaim(error.to_string()))?;
        if body.summary.trim().is_empty() {
            return Err(EngineError::InvalidContributionClaim(
                "empty summary".into(),
            ));
        }
        let mut claimed_ids: Vec<&str> = tag_values(claim_event, "e").collect();
        claimed_ids.sort_unstable();
        if claimed_ids.is_empty()
            || claimed_ids.windows(2).any(|pair| pair[0] == pair[1])
            || claimed_ids.len() != evidence_events.len()
        {
            return Err(EngineError::InvalidContributionClaim(
                "evidence set mismatch".into(),
            ));
        }
        let mut actual_ids: Vec<String> = evidence_events
            .iter()
            .map(|event| event.id.to_hex())
            .collect();
        actual_ids.sort();
        if claimed_ids
            .iter()
            .copied()
            .ne(actual_ids.iter().map(String::as_str))
        {
            return Err(EngineError::InvalidContributionClaim(
                "evidence IDs do not match".into(),
            ));
        }
        if evidence_events
            .iter()
            .any(|event| event.pubkey != claim_event.pubkey)
        {
            return Err(EngineError::ContributorMismatch);
        }
        let created_at = i64::try_from(claim_event.created_at.as_secs())
            .map_err(|_| EngineError::InvalidContributionClaim("timestamp overflow".into()))?;
        let record = project_context.contribution_from_events(
            format!("nostr:{}", claim_event.pubkey.to_hex()),
            body.contributor_kind,
            body.class,
            created_at,
            body.summary,
            evidence_events,
        )?;
        Ok(Self {
            claim_event: claim_event.clone(),
            record,
        })
    }

    /// Re-verifies the full claim and derived record.
    pub fn verify(
        &self,
        project_context: &BuzzProjectContext,
        evidence_events: &[Event],
    ) -> Result<(), EngineError> {
        let derived = Self::from_event(project_context, &self.claim_event, evidence_events)?;
        if derived.record != self.record {
            return Err(EngineError::EvidenceMismatch);
        }
        Ok(())
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
            Tag::parse(["st8-contribution", contribution.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
            Tag::parse(["st8-policy", self.policy_version.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
            Tag::parse(["st8-decision", digest.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
        ];
        Ok(EventBuilder::new(Kind::Custom(KIND_ST8_GOVERNANCE_APPROVAL as u16), digest).tags(tags))
    }

    /// Builds an approval event linked to the exact signed decision proposal.
    pub fn approval_event_builder_for_proposal(
        &self,
        proposal_event: &Event,
    ) -> Result<EventBuilder, EngineError> {
        if u32::from(proposal_event.kind.as_u16()) != KIND_ST8_DECISION_PROPOSAL
            || proposal_event.content != serde_json::to_string(self)?
        {
            return Err(EngineError::InvalidDecisionProposal(
                "proposal does not carry this intent".into(),
            ));
        }
        let digest = hex::encode(self.digest()?);
        let contribution = hex::encode(self.contribution_id);
        let tags = [
            Tag::parse(["a", self.project.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
            Tag::parse(["st8-contribution", contribution.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
            Tag::parse(["st8-policy", self.policy_version.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
            Tag::parse(["st8-decision", digest.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
            Tag::parse(["e", proposal_event.id.to_hex().as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
        ];
        Ok(EventBuilder::new(Kind::Custom(KIND_ST8_GOVERNANCE_APPROVAL as u16), digest).tags(tags))
    }

    /// Builds the authorized decision-proposal event that approvals bind.
    pub fn proposal_event_builder(&self) -> Result<EventBuilder, EngineError> {
        let digest = hex::encode(self.digest()?);
        let contribution = hex::encode(self.contribution_id);
        let tags = [
            Tag::parse(["a", self.project.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
            Tag::parse(["st8-contribution", contribution.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
            Tag::parse(["st8-policy", self.policy_version.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
            Tag::parse(["st8-decision", digest.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
        ];
        Ok(EventBuilder::new(
            Kind::Custom(KIND_ST8_DECISION_PROPOSAL as u16),
            serde_json::to_string(self)?,
        )
        .tags(tags))
    }

    /// Builds a decision proposal linked to the exact signed contribution claim
    /// and exact project-owner-signed governance policy event.
    pub fn proposal_event_builder_for_claim_and_policy(
        &self,
        claim_event: &Event,
        policy_event: &Event,
    ) -> Result<EventBuilder, EngineError> {
        if u32::from(claim_event.kind.as_u16()) != KIND_ST8_CONTRIBUTION_CLAIM {
            return Err(EngineError::InvalidContributionClaim(
                "proposal must reference a contribution claim".into(),
            ));
        }
        if u32::from(policy_event.kind.as_u16()) != KIND_ST8_GOVERNANCE_POLICY {
            return Err(EngineError::InvalidGovernancePolicyEvent(
                "proposal must reference a governance policy event".into(),
            ));
        }
        let digest = hex::encode(self.digest()?);
        let contribution = hex::encode(self.contribution_id);
        let tags = [
            Tag::parse(["a", self.project.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
            Tag::parse(["st8-contribution", contribution.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
            Tag::parse(["st8-policy", self.policy_version.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
            Tag::parse(["st8-policy-event", policy_event.id.to_hex().as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
            Tag::parse(["st8-decision", digest.as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
            Tag::parse(["e", claim_event.id.to_hex().as_str()])
                .map_err(|_| EngineError::InvalidGovernanceIntent)?,
        ];
        Ok(EventBuilder::new(
            Kind::Custom(KIND_ST8_DECISION_PROPOSAL as u16),
            serde_json::to_string(self)?,
        )
        .tags(tags))
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
    /// Builds the exact parameterized-replaceable policy event that the
    /// kind-30621 project owner must sign.
    pub fn event_builder(&self) -> Result<EventBuilder, EngineError> {
        self.validate()?;
        let content = serde_json::to_string(self)?;
        let tags = [
            Tag::parse(["d", self.version.as_str()])
                .map_err(|_| EngineError::InvalidGovernancePolicy)?,
            Tag::parse(["a", self.project.as_str()])
                .map_err(|_| EngineError::InvalidGovernancePolicy)?,
        ];
        Ok(EventBuilder::new(Kind::Custom(KIND_ST8_GOVERNANCE_POLICY as u16), content).tags(tags))
    }

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
            if u32::from(event.kind.as_u16()) != KIND_ST8_GOVERNANCE_APPROVAL
                || event.content != expected_digest
                || single_tag_value(event, "a") != Some(intent.project.as_str())
                || single_tag_value(event, "st8-contribution")
                    != Some(expected_contribution.as_str())
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
            let Some(public_key) = founder.strip_prefix("nostr:") else {
                return Err(EngineError::InvalidGovernancePolicy);
            };
            if public_key.len() != 64
                || !public_key
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
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

/// Signed project-owner authorization for one exact governance policy version.
///
/// Retaining the complete event prevents a receipt producer from inventing an
/// authority list or lowering an approval threshold after the fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernancePolicyContext {
    /// Verified policy body.
    pub policy: GovernancePolicy,
    /// Full project-owner-signed policy event.
    pub policy_event: Event,
}

impl GovernancePolicyContext {
    /// Derives and verifies policy authority from the signed project owner.
    pub fn from_event(
        project_context: &BuzzProjectContext,
        policy_event: &Event,
    ) -> Result<Self, EngineError> {
        project_context.verify()?;
        verify_event(policy_event)
            .map_err(|error| EngineError::InvalidGovernancePolicyEvent(error.to_string()))?;
        if u32::from(policy_event.kind.as_u16()) != KIND_ST8_GOVERNANCE_POLICY {
            return Err(EngineError::InvalidGovernancePolicyEvent(
                "unexpected event kind".into(),
            ));
        }
        if policy_event.pubkey != project_context.project_event.pubkey {
            return Err(EngineError::GovernancePolicyNotProjectOwned);
        }
        let policy: GovernancePolicy = serde_json::from_str(&policy_event.content)
            .map_err(|error| EngineError::InvalidGovernancePolicyEvent(error.to_string()))?;
        policy.validate()?;
        if policy.project != project_context.project
            || single_tag_value(policy_event, "a") != Some(policy.project.as_str())
            || single_tag_value(policy_event, "d") != Some(policy.version.as_str())
        {
            return Err(EngineError::ProjectMismatch);
        }
        Ok(Self {
            policy,
            policy_event: policy_event.clone(),
        })
    }

    /// Re-verifies the signature, ownership, tags, and policy body.
    pub fn verify(&self, project_context: &BuzzProjectContext) -> Result<(), EngineError> {
        let derived = Self::from_event(project_context, &self.policy_event)?;
        if derived.policy != self.policy {
            return Err(EngineError::InvalidGovernancePolicyEvent(
                "policy body mismatch".into(),
            ));
        }
        Ok(())
    }
}

/// Verified authorized proposal for one exact contribution decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceDecisionProposal {
    /// Full signed proposal event.
    pub proposal_event: Event,
    /// Exact decision intent carried by that event.
    pub intent: GovernanceDecisionIntent,
}

impl GovernanceDecisionProposal {
    /// Verifies proposal signature, authority, policy, record binding, and tags.
    pub fn from_event(
        record: &ContributionRecord,
        policy_context: &GovernancePolicyContext,
        proposal_event: &Event,
    ) -> Result<Self, EngineError> {
        verify_event(proposal_event)
            .map_err(|error| EngineError::InvalidDecisionProposal(error.to_string()))?;
        if u32::from(proposal_event.kind.as_u16()) != KIND_ST8_DECISION_PROPOSAL {
            return Err(EngineError::InvalidDecisionProposal(
                "unexpected event kind".into(),
            ));
        }
        let intent: GovernanceDecisionIntent = serde_json::from_str(&proposal_event.content)
            .map_err(|error| EngineError::InvalidDecisionProposal(error.to_string()))?;
        let policy = &policy_context.policy;
        let expected = policy.decision_intent(
            record,
            intent.status,
            intent.contribution_units,
            intent.decided_at,
            intent.rationale.clone(),
        )?;
        if intent != expected {
            return Err(EngineError::InvalidDecisionProposal(
                "intent does not match contribution or policy".into(),
            ));
        }
        let proposal_author = format!("nostr:{}", proposal_event.pubkey.to_hex());
        if policy
            .canonical_founders()?
            .binary_search(&proposal_author)
            .is_err()
        {
            return Err(EngineError::UnauthorizedApprover);
        }
        let digest = hex::encode(intent.digest()?);
        let contribution = hex::encode(intent.contribution_id);
        if single_tag_value(proposal_event, "a") != Some(intent.project.as_str())
            || single_tag_value(proposal_event, "st8-contribution") != Some(contribution.as_str())
            || single_tag_value(proposal_event, "st8-policy")
                != Some(intent.policy_version.as_str())
            || single_tag_value(proposal_event, "st8-policy-event")
                != Some(policy_context.policy_event.id.to_hex().as_str())
            || single_tag_value(proposal_event, "st8-decision") != Some(digest.as_str())
        {
            return Err(EngineError::InvalidDecisionProposal(
                "proposal tags do not bind the exact intent".into(),
            ));
        }
        let proposed_at = i64::try_from(proposal_event.created_at.as_secs())
            .map_err(|_| EngineError::InvalidDecisionProposal("timestamp overflow".into()))?;
        if proposed_at < record.created_at || proposed_at > intent.decided_at {
            return Err(EngineError::InvalidDecisionProposal(
                "proposal timestamp is outside the decision window".into(),
            ));
        }
        Ok(Self {
            proposal_event: proposal_event.clone(),
            intent,
        })
    }

    /// Re-verifies this proposal against its contribution and policy.
    pub fn verify(
        &self,
        record: &ContributionRecord,
        policy_context: &GovernancePolicyContext,
    ) -> Result<(), EngineError> {
        let derived = Self::from_event(record, policy_context, &self.proposal_event)?;
        if derived.intent != self.intent {
            return Err(EngineError::InvalidDecisionProposal(
                "proposal intent mismatch".into(),
            ));
        }
        Ok(())
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

/// Deterministic project-wide accepted contribution state committed by one BSV
/// anchor. Rejected contributions remain in the ledger but never become leaves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectLedgerSnapshot {
    /// Project coordinate shared by every accepted contribution.
    pub project: String,
    /// Accepted/adjusted contribution snapshots sorted by contribution ID.
    pub contributions: Vec<ContributionSnapshot>,
}

impl ProjectLedgerSnapshot {
    /// Builds a canonical project snapshot from accepted contribution state.
    pub fn new(mut contributions: Vec<ContributionSnapshot>) -> Result<Self, EngineError> {
        if contributions.is_empty() {
            return Err(EngineError::SnapshotMismatch);
        }
        let project = contributions[0].project.clone();
        for contribution in &contributions {
            if contribution.project != project
                || contribution.record.project != project
                || contribution.decision.status == DecisionStatus::Rejected
                || contribution.decision.contribution_units == 0
            {
                return Err(EngineError::SnapshotMismatch);
            }
            contribution.decision.validate_for(&contribution.record)?;
        }
        let mut keyed = contributions
            .drain(..)
            .map(|contribution| Ok((contribution.record.id()?, contribution)))
            .collect::<Result<Vec<_>, EngineError>>()?;
        keyed.sort_by_key(|(id, _)| *id);
        let mut previous = None;
        for (id, _) in &keyed {
            if previous == Some(*id) {
                return Err(EngineError::SnapshotMismatch);
            }
            previous = Some(*id);
        }
        let contributions = keyed
            .into_iter()
            .map(|(_, contribution)| contribution)
            .collect();
        Ok(Self {
            project,
            contributions,
        })
    }

    /// Returns project-scoped commitments in contribution-ID order.
    pub fn commitments(&self) -> Result<Vec<ProjectCommitment>, EngineError> {
        self.contributions
            .iter()
            .map(ContributionSnapshot::commitment)
            .collect()
    }

    /// Returns the canonical Merkle batch for all accepted contribution state.
    pub fn merkle_batch(&self) -> Result<MerkleBatch, EngineError> {
        Ok(MerkleBatch::new(self.commitments()?)?)
    }

    /// Returns the canonical project snapshot bytes.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, EngineError> {
        let canonical = Self::new(self.contributions.clone())?;
        if canonical.project != self.project || canonical.contributions != self.contributions {
            return Err(EngineError::SnapshotMismatch);
        }
        let mut out = Vec::new();
        out.extend_from_slice(PROJECT_SNAPSHOT_DOMAIN);
        put_bytes(&mut out, self.project.as_bytes())?;
        let count =
            u32::try_from(self.contributions.len()).map_err(|_| ProtocolError::LengthOverflow)?;
        out.extend_from_slice(&count.to_be_bytes());
        for contribution in &self.contributions {
            put_bytes(&mut out, &contribution.canonical_bytes()?)?;
        }
        Ok(out)
    }

    /// Stable project snapshot ID.
    pub fn id(&self) -> Result<Digest32, EngineError> {
        Ok(Sha256::digest(self.canonical_bytes()?).into())
    }

    /// Verifies canonical order, accepted state, and deterministic identity.
    pub fn verify(&self) -> Result<(), EngineError> {
        let _ = self.canonical_bytes()?;
        let batch = self.merkle_batch()?;
        if batch.project() != self.project || batch.len() != self.contributions.len() {
            return Err(EngineError::SnapshotMismatch);
        }
        Ok(())
    }
}

/// File-oriented input for preparing a real contribution anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContributionProposal {
    /// Full signed NIP-MP project event.
    pub project_event: Event,
    /// Full signed Buzz events supporting the contribution.
    pub evidence_events: Vec<Event>,
    /// Full project-owner-signed governance policy event.
    pub governance_policy_event: Event,
    /// Full contributor-signed contribution claim event.
    pub claim_event: Event,
    /// Full authorized governance decision proposal event.
    pub decision_proposal_event: Event,
    /// Full signed project-authority approval events. Approver identities are
    /// derived from verified event authors, never trusted from caller strings.
    #[serde(default)]
    pub approval_events: Vec<Event>,
}

impl ContributionProposal {
    /// Verifies signed Buzz evidence and returns the exact governance intent
    /// founders must sign before the contribution can be prepared.
    pub fn governance_intent(&self) -> Result<GovernanceDecisionIntent, EngineError> {
        let (_, _, _, proposal) = self.verified_contexts()?;
        Ok(proposal.intent)
    }

    /// Verifies the proposal and prepares the deterministic BSV testnet payload.
    pub fn prepare(self) -> Result<PreparedContributionAnchor, EngineError> {
        let (project_context, claim_context, policy_context, proposal) =
            self.verified_contexts()?;
        let decision = policy_context.policy.decide(
            &claim_context.record,
            &proposal.intent,
            &self.approval_events,
        )?;
        let snapshot = ContributionSnapshot::new(claim_context.record.clone(), decision)?;
        PreparedContributionAnchor::new(
            VerifiedContributionMaterial {
                project_context,
                evidence_events: self.evidence_events,
                claim: claim_context,
                decision_proposal: proposal,
                approval_events: self.approval_events,
                governance_policy: policy_context,
            },
            snapshot,
            BsvNetwork::Testnet,
        )
    }

    fn verified_contexts(
        &self,
    ) -> Result<
        (
            BuzzProjectContext,
            ContributionClaimContext,
            GovernancePolicyContext,
            GovernanceDecisionProposal,
        ),
        EngineError,
    > {
        let project_context = BuzzProjectContext::from_event(&self.project_event)?;
        let claim_context = ContributionClaimContext::from_event(
            &project_context,
            &self.claim_event,
            &self.evidence_events,
        )?;
        let policy_context =
            GovernancePolicyContext::from_event(&project_context, &self.governance_policy_event)?;
        let proposal = GovernanceDecisionProposal::from_event(
            &claim_context.record,
            &policy_context,
            &self.decision_proposal_event,
        )?;
        verify_event_link(&proposal.proposal_event, &claim_context.claim_event)?;
        for approval in &self.approval_events {
            verify_event_link(approval, &proposal.proposal_event)?;
        }
        Ok((project_context, claim_context, policy_context, proposal))
    }
}

/// Verified signed inputs used to build deterministic contribution anchor state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedContributionMaterial {
    /// Signed project context.
    pub project_context: BuzzProjectContext,
    /// Full signed contribution evidence.
    pub evidence_events: Vec<Event>,
    /// Contributor-signed claim and derived deterministic record.
    pub claim: ContributionClaimContext,
    /// Authorized signed proposal approvals bind.
    pub decision_proposal: GovernanceDecisionProposal,
    /// Full signed project-governance approval events.
    pub approval_events: Vec<Event>,
    /// Project-owner-signed governance policy.
    pub governance_policy: GovernancePolicyContext,
}

/// Fully verified contribution state waiting for an external wallet and broadcaster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedContributionAnchor {
    /// Signed project context.
    pub project_context: BuzzProjectContext,
    /// Full signed contribution evidence.
    pub evidence_events: Vec<Event>,
    /// Contributor-signed claim and derived deterministic record.
    pub claim: ContributionClaimContext,
    /// Authorized signed proposal approvals bind.
    pub decision_proposal: GovernanceDecisionProposal,
    /// Full signed project-governance approval events.
    pub approval_events: Vec<Event>,
    /// Project-owner-signed governance policy.
    pub governance_policy: GovernancePolicyContext,
    /// Accepted contribution snapshot.
    pub snapshot: ContributionSnapshot,
    /// Project-wide accepted ledger state at anchor time.
    pub project_snapshot: ProjectLedgerSnapshot,
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
        material: VerifiedContributionMaterial,
        snapshot: ContributionSnapshot,
        network: BsvNetwork,
    ) -> Result<Self, EngineError> {
        let project_snapshot = ProjectLedgerSnapshot::new(vec![snapshot.clone()])?;
        Self::new_with_project_snapshot(material, snapshot, project_snapshot, network)
    }

    /// Builds deterministic anchor material for a full project ledger snapshot.
    pub fn new_with_project_snapshot(
        material: VerifiedContributionMaterial,
        snapshot: ContributionSnapshot,
        project_snapshot: ProjectLedgerSnapshot,
        network: BsvNetwork,
    ) -> Result<Self, EngineError> {
        let VerifiedContributionMaterial {
            project_context,
            evidence_events,
            claim,
            decision_proposal,
            approval_events,
            governance_policy,
        } = material;
        if network != BsvNetwork::Testnet {
            return Err(EngineError::FirstSliceRequiresTestnet);
        }
        project_context.verify()?;
        claim.verify(&project_context, &evidence_events)?;
        if claim.record != snapshot.record {
            return Err(EngineError::EvidenceMismatch);
        }
        verify_grounded_record_evidence(&project_context, &snapshot.record, &evidence_events)?;
        governance_policy.verify(&project_context)?;
        decision_proposal.verify(&snapshot.record, &governance_policy)?;
        verify_event_link(&decision_proposal.proposal_event, &claim.claim_event)?;
        for approval in &approval_events {
            verify_event_link(approval, &decision_proposal.proposal_event)?;
        }
        governance_policy.policy.verify_decision(
            &snapshot.record,
            &snapshot.decision,
            &approval_events,
        )?;
        if project_context.project != snapshot.project
            || governance_policy.policy.project != snapshot.project
            || project_snapshot.project != snapshot.project
        {
            return Err(EngineError::ProjectMismatch);
        }
        let commitment = snapshot.commitment()?;
        project_snapshot.verify()?;
        let batch = project_snapshot.merkle_batch()?;
        let merkle_proof = batch.proof_for(&commitment)?;
        let anchor_payload =
            AnchorPayload::new(network, &snapshot.project, &batch, project_snapshot.id()?)?;
        let prepared = Self {
            project_context,
            evidence_events,
            claim,
            decision_proposal,
            approval_events,
            governance_policy,
            snapshot,
            project_snapshot,
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
        self.claim
            .verify(&self.project_context, &self.evidence_events)?;
        self.decision_proposal
            .verify(&self.snapshot.record, &self.governance_policy)?;
        verify_event_link(
            &self.decision_proposal.proposal_event,
            &self.claim.claim_event,
        )?;
        for approval in &self.approval_events {
            verify_event_link(approval, &self.decision_proposal.proposal_event)?;
        }
        verify_grounded_record_evidence(
            &self.project_context,
            &self.snapshot.record,
            &self.evidence_events,
        )?;
        self.governance_policy.verify(&self.project_context)?;
        self.governance_policy.policy.verify_decision(
            &self.snapshot.record,
            &self.snapshot.decision,
            &self.approval_events,
        )?;
        if self.project_context.project != self.snapshot.project
            || self.governance_policy.policy.project != self.snapshot.project
            || self.project_snapshot.project != self.snapshot.project
            || self.commitment != self.snapshot.commitment()?
        {
            return Err(EngineError::SnapshotMismatch);
        }
        self.project_snapshot.verify()?;
        let batch = self.project_snapshot.merkle_batch()?;
        if batch.root() != self.anchor_payload.merkle_root
            || !self
                .project_snapshot
                .contributions
                .iter()
                .any(|candidate| candidate == &self.snapshot)
        {
            return Err(EngineError::SnapshotMismatch);
        }
        self.merkle_proof
            .verify(&self.commitment, self.anchor_payload.merkle_root)?;
        self.anchor_payload.verify_project(&self.snapshot.project)?;
        if self.anchor_payload.network != BsvNetwork::Testnet
            || self.anchor_payload.snapshot_id != self.project_snapshot.id()?
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
            claim: self.claim,
            decision_proposal: self.decision_proposal,
            approval_events: self.approval_events,
            governance_policy: self.governance_policy,
            snapshot: self.snapshot,
            project_snapshot: self.project_snapshot,
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
    /// Contributor-signed claim retained for independent attribution verification.
    pub claim: ContributionClaimContext,
    /// Authorized signed governance proposal retained for approval-chain verification.
    pub decision_proposal: GovernanceDecisionProposal,
    /// Full signed project-governance approvals retained for independent verification.
    pub approval_events: Vec<Event>,
    /// Project-owner-signed governance policy used for acceptance.
    pub governance_policy: GovernancePolicyContext,
    /// Preserved contribution state.
    pub snapshot: ContributionSnapshot,
    /// Full project-wide accepted ledger snapshot committed by the transaction.
    pub project_snapshot: ProjectLedgerSnapshot,
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
        self.claim
            .verify(&self.project_context, &self.evidence_events)?;
        self.decision_proposal
            .verify(&self.snapshot.record, &self.governance_policy)?;
        verify_event_link(
            &self.decision_proposal.proposal_event,
            &self.claim.claim_event,
        )?;
        for approval in &self.approval_events {
            verify_event_link(approval, &self.decision_proposal.proposal_event)?;
        }
        verify_grounded_record_evidence(
            &self.project_context,
            &self.snapshot.record,
            &self.evidence_events,
        )?;
        if self.project_context.project != self.snapshot.project
            || self.governance_policy.policy.project != self.snapshot.project
            || self.commitment.project != self.snapshot.project
            || self.project_snapshot.project != self.snapshot.project
        {
            return Err(EngineError::ProjectMismatch);
        }
        self.governance_policy.verify(&self.project_context)?;
        self.governance_policy.policy.verify_decision(
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
        self.project_snapshot.verify()?;
        let batch = self.project_snapshot.merkle_batch()?;
        if batch.root() != self.anchor_payload.merkle_root
            || !self
                .project_snapshot
                .contributions
                .iter()
                .any(|candidate| candidate == &self.snapshot)
        {
            return Err(EngineError::SnapshotMismatch);
        }
        self.merkle_proof
            .verify(&self.commitment, self.anchor_payload.merkle_root)?;
        self.anchor_payload.verify_project(&self.snapshot.project)?;
        if self.anchor_payload.network != BsvNetwork::Testnet
            || self.anchor_payload.snapshot_id != self.project_snapshot.id()?
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
    /// Signed contribution claim was malformed or inconsistent with evidence.
    #[error("invalid signed contribution claim: {0}")]
    InvalidContributionClaim(String),
    /// Event ID could not be decoded.
    #[error("invalid Buzz event digest")]
    InvalidEventDigest,
    /// Governance policy is internally invalid.
    #[error("invalid governance policy")]
    InvalidGovernancePolicy,
    /// Signed governance policy event was malformed or invalid.
    #[error("invalid signed governance policy event: {0}")]
    InvalidGovernancePolicyEvent(String),
    /// Governance policy was not signed by the kind-30621 project owner.
    #[error("governance policy was not signed by the project owner")]
    GovernancePolicyNotProjectOwned,
    /// Governance intent fields violate protocol invariants.
    #[error("invalid governance decision intent")]
    InvalidGovernanceIntent,
    /// Governance decision proposal was malformed, unauthorized, or inconsistent.
    #[error("invalid signed governance decision proposal: {0}")]
    InvalidDecisionProposal(String),
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

fn verify_event_link(event: &Event, expected: &Event) -> Result<(), EngineError> {
    if single_tag_value(event, "e") != Some(expected.id.to_hex().as_str()) {
        return Err(EngineError::EvidenceMismatch);
    }
    Ok(())
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
            vec![founder_a, founder_b, contributor],
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
        let claim_event = ContributionClaimBody {
            contributor_kind: record.contributor_kind,
            class: record.class,
            summary: record.summary.clone(),
        }
        .event_builder(&record.project, std::slice::from_ref(&event))
        .expect("claim builder")
        .custom_created_at(nostr::Timestamp::from(
            u64::try_from(record.created_at).expect("claim timestamp"),
        ))
        .sign_with_keys(&founders[2])
        .expect("sign claim");
        let claim = ContributionClaimContext::from_event(
            &context,
            &claim_event,
            std::slice::from_ref(&event),
        )
        .expect("claim context");
        let policy_event = policy
            .event_builder()
            .expect("policy builder")
            .sign_with_keys(&founders[0])
            .expect("sign policy");
        let policy_context = GovernancePolicyContext::from_event(&context, &policy_event)
            .expect("signed policy context");
        let intent = policy
            .decision_intent(
                &record,
                DecisionStatus::Accepted,
                500,
                1_800_000_000,
                "Grounded engineering contribution accepted".into(),
            )
            .expect("intent");
        let proposal_event = intent
            .proposal_event_builder_for_claim_and_policy(&claim_event, &policy_event)
            .expect("proposal builder")
            .custom_created_at(nostr::Timestamp::from(
                u64::try_from(record.created_at + 1).expect("proposal timestamp"),
            ))
            .sign_with_keys(&founders[0])
            .expect("sign proposal");
        let proposal =
            GovernanceDecisionProposal::from_event(&record, &policy_context, &proposal_event)
                .expect("proposal context");
        let approvals = vec![intent
            .approval_event_builder_for_proposal(&proposal_event)
            .expect("approval builder")
            .custom_created_at(nostr::Timestamp::from(
                u64::try_from(record.created_at + 2).expect("approval timestamp"),
            ))
            .sign_with_keys(&founders[0])
            .expect("sign approval")];
        let decision = policy
            .decide(&record, &intent, &approvals)
            .expect("decision");
        let snapshot = ContributionSnapshot::new(record, decision).expect("snapshot");
        let prepared = PreparedContributionAnchor::new(
            VerifiedContributionMaterial {
                project_context: context,
                evidence_events: vec![event],
                claim,
                decision_proposal: proposal,
                approval_events: approvals,
                governance_policy: policy_context,
            },
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
    fn policy_authorities_require_canonical_nostr_public_keys() {
        let (_context, _event, mut policy, _record, _founders) = fixture();
        policy.founders[0] = policy.founders[0].trim_start_matches("nostr:").to_owned();
        assert!(matches!(
            policy.validate(),
            Err(EngineError::InvalidGovernancePolicy)
        ));
        policy.founders[0] = format!("nostr:{}", "A".repeat(64));
        assert!(matches!(
            policy.validate(),
            Err(EngineError::InvalidGovernancePolicy)
        ));
    }

    #[test]
    fn proposal_is_bound_to_exact_policy_event_not_only_version() {
        let (context, evidence, policy, record, founders) = fixture();
        let claim_event = ContributionClaimBody {
            contributor_kind: record.contributor_kind,
            class: record.class,
            summary: record.summary.clone(),
        }
        .event_builder(&record.project, std::slice::from_ref(&evidence))
        .expect("claim builder")
        .sign_with_keys(&founders[2])
        .expect("sign claim");
        let original_policy_event = policy
            .event_builder()
            .expect("policy builder")
            .custom_created_at(nostr::Timestamp::from(1))
            .sign_with_keys(&founders[0])
            .expect("sign original policy");
        let replacement_policy_event = policy
            .event_builder()
            .expect("policy builder")
            .custom_created_at(nostr::Timestamp::from(2))
            .sign_with_keys(&founders[0])
            .expect("sign replacement policy");
        let replacement_context =
            GovernancePolicyContext::from_event(&context, &replacement_policy_event)
                .expect("replacement context");
        let intent = policy
            .decision_intent(
                &record,
                DecisionStatus::Accepted,
                500,
                1_800_000_000,
                "exact policy binding".into(),
            )
            .expect("intent");
        let proposal = intent
            .proposal_event_builder_for_claim_and_policy(&claim_event, &original_policy_event)
            .expect("proposal builder")
            .sign_with_keys(&founders[0])
            .expect("sign proposal");
        assert!(matches!(
            GovernanceDecisionProposal::from_event(&record, &replacement_context, &proposal),
            Err(EngineError::InvalidDecisionProposal(_))
        ));
    }

    #[test]
    fn contributor_cannot_claim_another_authors_evidence() {
        let (context, evidence, _policy, record, founders) = fixture();
        let claim = ContributionClaimBody {
            contributor_kind: record.contributor_kind,
            class: record.class,
            summary: record.summary,
        }
        .event_builder(&record.project, std::slice::from_ref(&evidence))
        .expect("claim builder")
        .sign_with_keys(&founders[0])
        .expect("sign claim as another identity");
        assert!(matches!(
            ContributionClaimContext::from_event(&context, &claim, &[evidence]),
            Err(EngineError::ContributorMismatch)
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
