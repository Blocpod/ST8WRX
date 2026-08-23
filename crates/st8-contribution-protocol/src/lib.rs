#![deny(unsafe_code)]
#![warn(missing_docs)]
//! Deterministic, zero-I/O contribution protocol primitives for ST8WRX.
//!
//! Protocol identity never depends on Rust enum declaration order, JSON map
//! ordering, or caller-provided ordering of set-like fields. Every value that
//! enters a digest has an explicit permanent code and a length-delimited binary
//! encoding.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use thiserror::Error;

const RECORD_DOMAIN: &[u8] = b"ST8WRX\0CONTRIBUTION_RECORD\0V1";
const DECISION_DOMAIN: &[u8] = b"ST8WRX\0CONTRIBUTION_DECISION\0V1";

/// A protocol digest.
pub type Digest32 = [u8; 32];

/// Stable contribution categories. The numeric codes are permanent protocol
/// values and must never be reassigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ContributionClass {
    /// Ideas, inventions, or other intellectual input.
    Intellectual,
    /// System and solution architecture.
    Architecture,
    /// Software or hardware engineering.
    Engineering,
    /// Product management, UX, or design.
    ProductDesign,
    /// Work performed by an AI agent.
    AgentWork,
    /// Metered compute resources.
    Compute,
    /// Testing, security review, or assurance.
    TestingSecurityReview,
    /// Research, data, or datasets.
    ResearchData,
    /// Sales, distribution, partnerships, or commercial work.
    CommercialDistribution,
    /// Capital supplied to a project.
    Capital,
}

impl ContributionClass {
    /// Returns the permanent protocol code for this class.
    pub const fn protocol_code(self) -> u16 {
        match self {
            Self::Intellectual => 1,
            Self::Architecture => 2,
            Self::Engineering => 3,
            Self::ProductDesign => 4,
            Self::AgentWork => 5,
            Self::Compute => 6,
            Self::TestingSecurityReview => 7,
            Self::ResearchData => 8,
            Self::CommercialDistribution => 9,
            Self::Capital => 10,
        }
    }
}

/// Stable contributor identity categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ContributorKind {
    /// A human contributor.
    Human,
    /// An AI agent with its own identity.
    Agent,
    /// A compute node or resource provider.
    ComputeNode,
    /// A company, collective, or other organization.
    Organization,
}

impl ContributorKind {
    /// Returns the permanent protocol code for this contributor kind.
    pub const fn protocol_code(self) -> u16 {
        match self {
            Self::Human => 1,
            Self::Agent => 2,
            Self::ComputeNode => 3,
            Self::Organization => 4,
        }
    }
}

/// Stable evidence reference categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EvidenceKind {
    /// A signed Buzz/Nostr event.
    BuzzEvent,
    /// A Git object or commit.
    GitCommit,
    /// A content-addressed artifact.
    Artifact,
    /// A signed compute receipt.
    ComputeReceipt,
    /// A signed agreement or milestone.
    Agreement,
    /// An externally verifiable reference.
    External,
}

impl EvidenceKind {
    /// Returns the permanent protocol code for this evidence kind.
    pub const fn protocol_code(self) -> u16 {
        match self {
            Self::BuzzEvent => 1,
            Self::GitCommit => 2,
            Self::Artifact => 3,
            Self::ComputeReceipt => 4,
            Self::Agreement => 5,
            Self::External => 255,
        }
    }
}

/// A content-addressed reference to evidence. `locator` may point to private
/// storage; only the reference and digest enter protocol identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// Evidence category.
    pub kind: EvidenceKind,
    /// SHA-256 or native 32-byte identity of the evidence.
    pub digest: Digest32,
    /// Stable resolver hint such as `nostr:<event-id>` or `git:<oid>`.
    pub locator: String,
}

impl EvidenceRef {
    fn canonical_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        validate_text("evidence locator", &self.locator)?;
        let mut out = Vec::new();
        put_u16(&mut out, self.kind.protocol_code());
        out.extend_from_slice(&self.digest);
        put_text(&mut out, &self.locator)?;
        Ok(out)
    }
}

/// Evidence-backed contribution proposed to a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContributionRecord {
    /// Project-scoped stable identifier.
    pub project: String,
    /// Contributor identity reference.
    pub contributor: String,
    /// Contributor category.
    pub contributor_kind: ContributorKind,
    /// Contribution category.
    pub class: ContributionClass,
    /// Unix timestamp in seconds supplied by the signed evidence context.
    pub created_at: i64,
    /// Human-readable summary; it is part of protocol identity.
    pub summary: String,
    /// Set-like evidence references. Ordering and duplicates do not affect ID.
    pub evidence_refs: Vec<EvidenceRef>,
}

impl ContributionRecord {
    /// Returns normalized evidence refs sorted by canonical protocol bytes.
    pub fn canonical_evidence(&self) -> Result<Vec<EvidenceRef>, ProtocolError> {
        if self.evidence_refs.is_empty() {
            return Err(ProtocolError::MissingEvidence);
        }
        let mut keyed = Vec::with_capacity(self.evidence_refs.len());
        for evidence in &self.evidence_refs {
            keyed.push((evidence.canonical_bytes()?, evidence.clone()));
        }
        keyed.sort_by(|left, right| left.0.cmp(&right.0));
        keyed.dedup_by(|left, right| left.0 == right.0);
        Ok(keyed.into_iter().map(|(_, evidence)| evidence).collect())
    }

    /// Returns the versioned canonical binary encoding.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        validate_text("project", &self.project)?;
        validate_text("contributor", &self.contributor)?;
        validate_text("summary", &self.summary)?;

        let canonical_evidence = self.canonical_evidence()?;
        let mut out = Vec::new();
        out.extend_from_slice(RECORD_DOMAIN);
        put_text(&mut out, &self.project)?;
        put_text(&mut out, &self.contributor)?;
        put_u16(&mut out, self.contributor_kind.protocol_code());
        put_u16(&mut out, self.class.protocol_code());
        out.extend_from_slice(&self.created_at.to_be_bytes());
        put_text(&mut out, &self.summary)?;
        put_u32(
            &mut out,
            u32::try_from(canonical_evidence.len()).map_err(|_| ProtocolError::LengthOverflow)?,
        );
        for evidence in canonical_evidence {
            let bytes = evidence.canonical_bytes()?;
            put_bytes(&mut out, &bytes)?;
        }
        Ok(out)
    }

    /// Computes the deterministic contribution ID.
    pub fn id(&self) -> Result<Digest32, ProtocolError> {
        Ok(hash(&self.canonical_bytes()?))
    }
}

/// Governance decision state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DecisionStatus {
    /// Contribution accepted as proposed.
    Accepted,
    /// Contribution rejected; it must award zero units.
    Rejected,
    /// Contribution accepted with adjusted units or rationale.
    Adjusted,
}

impl DecisionStatus {
    /// Returns the permanent protocol code for this status.
    pub const fn protocol_code(self) -> u16 {
        match self {
            Self::Accepted => 1,
            Self::Rejected => 2,
            Self::Adjusted => 3,
        }
    }
}

/// A project governance decision about a contribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContributionDecision {
    /// Deterministic contribution ID being decided.
    pub contribution_id: Digest32,
    /// Project identity; must match the contribution.
    pub project: String,
    /// Accepted, rejected, or adjusted.
    pub status: DecisionStatus,
    /// Non-transferable Contribution Units awarded by this decision.
    pub contribution_units: u64,
    /// Versioned project governance policy identifier.
    pub policy_version: String,
    /// Set-like approver identity references.
    pub approvers: Vec<String>,
    /// Unix timestamp in seconds.
    pub decided_at: i64,
    /// Human-readable decision rationale.
    pub rationale: String,
}

impl ContributionDecision {
    /// Returns unique approvers in canonical byte order.
    pub fn canonical_approvers(&self) -> Result<Vec<String>, ProtocolError> {
        if self.approvers.is_empty() {
            return Err(ProtocolError::MissingApprover);
        }
        let mut approvers = BTreeSet::new();
        for approver in &self.approvers {
            validate_text("approver", approver)?;
            approvers.insert(approver.clone());
        }
        Ok(approvers.into_iter().collect())
    }

    /// Validates project binding and unit/status invariants.
    pub fn validate_for(&self, record: &ContributionRecord) -> Result<(), ProtocolError> {
        if self.project != record.project {
            return Err(ProtocolError::ProjectMismatch);
        }
        if self.contribution_id != record.id()? {
            return Err(ProtocolError::ContributionMismatch);
        }
        match self.status {
            DecisionStatus::Rejected if self.contribution_units != 0 => {
                return Err(ProtocolError::RejectedUnits)
            }
            DecisionStatus::Accepted | DecisionStatus::Adjusted if self.contribution_units == 0 => {
                return Err(ProtocolError::AcceptedWithoutUnits)
            }
            _ => {}
        }
        self.validate_common()
    }

    /// Returns the versioned canonical binary encoding.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate_common()?;
        let approvers = self.canonical_approvers()?;
        let mut out = Vec::new();
        out.extend_from_slice(DECISION_DOMAIN);
        out.extend_from_slice(&self.contribution_id);
        put_text(&mut out, &self.project)?;
        put_u16(&mut out, self.status.protocol_code());
        out.extend_from_slice(&self.contribution_units.to_be_bytes());
        put_text(&mut out, &self.policy_version)?;
        put_u32(
            &mut out,
            u32::try_from(approvers.len()).map_err(|_| ProtocolError::LengthOverflow)?,
        );
        for approver in approvers {
            put_text(&mut out, &approver)?;
        }
        out.extend_from_slice(&self.decided_at.to_be_bytes());
        put_text(&mut out, &self.rationale)?;
        Ok(out)
    }

    /// Computes the deterministic decision ID.
    pub fn id(&self) -> Result<Digest32, ProtocolError> {
        Ok(hash(&self.canonical_bytes()?))
    }

    fn validate_common(&self) -> Result<(), ProtocolError> {
        validate_text("project", &self.project)?;
        validate_text("policy version", &self.policy_version)?;
        validate_text("rationale", &self.rationale)?;
        if self.status == DecisionStatus::Rejected && self.contribution_units != 0 {
            return Err(ProtocolError::RejectedUnits);
        }
        if self.status != DecisionStatus::Rejected && self.contribution_units == 0 {
            return Err(ProtocolError::AcceptedWithoutUnits);
        }
        let _ = self.canonical_approvers()?;
        Ok(())
    }
}

/// Protocol validation and encoding errors.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// A required text field was empty or contained NUL.
    #[error("invalid required text field: {0}")]
    InvalidText(&'static str),
    /// A length cannot be represented by the protocol encoding.
    #[error("protocol field length overflow")]
    LengthOverflow,
    /// Contributions require at least one evidence reference.
    #[error("a contribution requires evidence")]
    MissingEvidence,
    /// Decisions require at least one approver.
    #[error("a decision requires an approver")]
    MissingApprover,
    /// A decision attempted to award units to a rejected contribution.
    #[error("a rejected contribution must award zero units")]
    RejectedUnits,
    /// An accepted or adjusted contribution awarded no units.
    #[error("an accepted or adjusted contribution must award units")]
    AcceptedWithoutUnits,
    /// Decision project did not match contribution project.
    #[error("decision project does not match contribution project")]
    ProjectMismatch,
    /// Decision contribution ID did not match the record.
    #[error("decision contribution ID does not match record")]
    ContributionMismatch,
}

fn validate_text(field: &'static str, value: &str) -> Result<(), ProtocolError> {
    if value.trim().is_empty() || value.as_bytes().contains(&0) {
        return Err(ProtocolError::InvalidText(field));
    }
    Ok(())
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_text(out: &mut Vec<u8>, value: &str) -> Result<(), ProtocolError> {
    put_bytes(out, value.as_bytes())
}

fn put_bytes(out: &mut Vec<u8>, value: &[u8]) -> Result<(), ProtocolError> {
    let len = u32::try_from(value.len()).map_err(|_| ProtocolError::LengthOverflow)?;
    put_u32(out, len);
    out.extend_from_slice(value);
    Ok(())
}

fn hash(bytes: &[u8]) -> Digest32 {
    Sha256::digest(bytes).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(byte: u8, locator: &str) -> EvidenceRef {
        EvidenceRef {
            kind: EvidenceKind::BuzzEvent,
            digest: [byte; 32],
            locator: locator.to_owned(),
        }
    }

    fn record(evidence_refs: Vec<EvidenceRef>) -> ContributionRecord {
        ContributionRecord {
            project: "30621:owner:st8wrx".to_owned(),
            contributor: "nostr:alice".to_owned(),
            contributor_kind: ContributorKind::Human,
            class: ContributionClass::Engineering,
            created_at: 1_700_000_000,
            summary: "Implement deterministic contribution IDs".to_owned(),
            evidence_refs,
        }
    }

    fn decision(record: &ContributionRecord, approvers: Vec<String>) -> ContributionDecision {
        ContributionDecision {
            contribution_id: record.id().expect("record id"),
            project: record.project.clone(),
            status: DecisionStatus::Accepted,
            contribution_units: 100,
            policy_version: "founders-v1".to_owned(),
            approvers,
            decided_at: 1_700_000_100,
            rationale: "Accepted after review".to_owned(),
        }
    }

    #[test]
    fn contribution_identity_ignores_evidence_order_and_duplicates() {
        let left = record(vec![evidence(2, "nostr:b"), evidence(1, "nostr:a")]);
        let right = record(vec![
            evidence(1, "nostr:a"),
            evidence(2, "nostr:b"),
            evidence(1, "nostr:a"),
        ]);
        assert_eq!(left.id().expect("left"), right.id().expect("right"));
    }

    #[test]
    fn decision_identity_ignores_approver_order_and_duplicates() {
        let record = record(vec![evidence(1, "nostr:a")]);
        let left = decision(&record, vec!["nostr:bob".into(), "nostr:alice".into()]);
        let right = decision(
            &record,
            vec![
                "nostr:alice".into(),
                "nostr:bob".into(),
                "nostr:alice".into(),
            ],
        );
        assert_eq!(left.id().expect("left"), right.id().expect("right"));
    }

    #[test]
    fn rejected_decision_cannot_award_units() {
        let record = record(vec![evidence(1, "nostr:a")]);
        let mut decision = decision(&record, vec!["nostr:alice".into()]);
        decision.status = DecisionStatus::Rejected;
        assert_eq!(
            decision.validate_for(&record),
            Err(ProtocolError::RejectedUnits)
        );
    }

    #[test]
    fn decision_is_bound_to_project_and_contribution() {
        let record = record(vec![evidence(1, "nostr:a")]);
        let mut decision = decision(&record, vec!["nostr:alice".into()]);
        decision.project = "30621:owner:other".to_owned();
        assert_eq!(
            decision.validate_for(&record),
            Err(ProtocolError::ProjectMismatch)
        );
    }

    #[test]
    fn protocol_codes_are_explicit_and_stable() {
        assert_eq!(ContributionClass::Engineering.protocol_code(), 3);
        assert_eq!(ContributorKind::Agent.protocol_code(), 2);
        assert_eq!(EvidenceKind::ComputeReceipt.protocol_code(), 4);
        assert_eq!(DecisionStatus::Adjusted.protocol_code(), 3);
    }

    #[test]
    fn serde_round_trip_does_not_change_identity() {
        let record = record(vec![evidence(1, "nostr:a")]);
        let json = serde_json::to_string(&record).expect("serialize");
        let decoded: ContributionRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            record.id().expect("original"),
            decoded.id().expect("decoded")
        );
    }
}
