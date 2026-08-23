#![deny(unsafe_code)]
#![warn(missing_docs)]
//! Project-scoped BSV provenance primitives for ST8WRX.
//!
//! This crate is deterministic and performs no I/O. Wallet access, transaction
//! construction, broadcasting, and header verification are supplied through
//! narrow traits by an integration layer. Project scope is included in every
//! leaf hash, so evidence from one project cannot verify in another project.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const LEAF_DOMAIN: &[u8] = b"ST8WRX\0PROVENANCE_LEAF\0V1";
const NODE_DOMAIN: &[u8] = b"ST8WRX\0MERKLE_NODE\0V1";
const ANCHOR_DOMAIN: &[u8] = b"ST8WRX\0BSV_ANCHOR\0V1";
/// Maximum commitments in one deterministic batch.
pub const MAX_BATCH_SIZE: usize = 4_096;
const MAX_TX_ITEMS: u64 = 100_000;
const MAX_SCRIPT_BYTES: u64 = 10_000_000;

/// A 32-byte protocol digest.
pub type Digest32 = [u8; 32];

/// BSV network selection. Network authority is independent from Buzz identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BsvNetwork {
    /// BSV public test network.
    Testnet,
    /// BSV production network.
    Mainnet,
}

impl BsvNetwork {
    /// Returns the permanent protocol code.
    pub const fn protocol_code(self) -> u16 {
        match self {
            Self::Testnet => 1,
            Self::Mainnet => 2,
        }
    }
}

/// Durable object category committed by an anchor leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitmentKind {
    /// Accepted contribution state snapshot.
    ContributionSnapshot,
    /// Signed project agreement.
    ProjectAgreement,
    /// Accepted milestone.
    Milestone,
    /// Release manifest or artifact snapshot.
    Release,
    /// Net settlement state.
    Settlement,
}

impl CommitmentKind {
    /// Returns the permanent protocol code.
    pub const fn protocol_code(self) -> u16 {
        match self {
            Self::ContributionSnapshot => 1,
            Self::ProjectAgreement => 2,
            Self::Milestone => 3,
            Self::Release => 4,
            Self::Settlement => 5,
        }
    }
}

/// A project-scoped durable commitment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectCommitment {
    /// Stable project identity.
    pub project: String,
    /// Durable object category.
    pub kind: CommitmentKind,
    /// Object identity, normally the deterministic snapshot or record ID.
    pub object_id: Digest32,
    /// Digest of the canonical object state.
    pub state_digest: Digest32,
}

impl ProjectCommitment {
    /// Computes the project-scoped Merkle leaf.
    pub fn leaf_hash(&self) -> Result<Digest32, ProvenanceError> {
        validate_project(&self.project)?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(LEAF_DOMAIN);
        put_text(&mut bytes, &self.project)?;
        bytes.extend_from_slice(&self.kind.protocol_code().to_be_bytes());
        bytes.extend_from_slice(&self.object_id);
        bytes.extend_from_slice(&self.state_digest);
        Ok(hash(&bytes))
    }
}

/// Deterministic, canonicalized Merkle batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerkleBatch {
    project: String,
    commitments: Vec<ProjectCommitment>,
    leaves: Vec<Digest32>,
    root: Digest32,
}

impl MerkleBatch {
    /// Creates a batch after sorting commitments by leaf hash. Duplicate leaves
    /// are rejected rather than silently changing proof cardinality.
    pub fn new(mut commitments: Vec<ProjectCommitment>) -> Result<Self, ProvenanceError> {
        if commitments.is_empty() {
            return Err(ProvenanceError::EmptyBatch);
        }
        if commitments.len() > MAX_BATCH_SIZE {
            return Err(ProvenanceError::BatchTooLarge {
                count: commitments.len(),
                max: MAX_BATCH_SIZE,
            });
        }
        let project = commitments[0].project.clone();
        validate_project(&project)?;
        if commitments.iter().any(|item| item.project != project) {
            return Err(ProvenanceError::MixedProjects);
        }

        let mut keyed = Vec::with_capacity(commitments.len());
        for commitment in commitments.drain(..) {
            keyed.push((commitment.leaf_hash()?, commitment));
        }
        keyed.sort_by_key(|item| item.0);
        if keyed.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(ProvenanceError::DuplicateLeaf);
        }
        let leaves: Vec<_> = keyed.iter().map(|(leaf, _)| *leaf).collect();
        let commitments = keyed.into_iter().map(|(_, item)| item).collect();
        let root = merkle_root(&leaves);
        Ok(Self {
            project,
            commitments,
            leaves,
            root,
        })
    }

    /// Returns the common project identity.
    pub fn project(&self) -> &str {
        &self.project
    }

    /// Returns the canonical Merkle root.
    pub const fn root(&self) -> Digest32 {
        self.root
    }

    /// Returns the number of leaves.
    pub fn len(&self) -> usize {
        self.leaves.len()
    }

    /// Returns whether the batch has no leaves. Constructed batches are never empty.
    pub fn is_empty(&self) -> bool {
        self.leaves.is_empty()
    }

    /// Generates the inclusion proof for a commitment.
    pub fn proof_for(
        &self,
        commitment: &ProjectCommitment,
    ) -> Result<MerkleProof, ProvenanceError> {
        if commitment.project != self.project {
            return Err(ProvenanceError::ProjectMismatch);
        }
        let leaf = commitment.leaf_hash()?;
        let index = self
            .leaves
            .iter()
            .position(|candidate| candidate == &leaf)
            .ok_or(ProvenanceError::CommitmentNotFound)?;
        let mut level = self.leaves.clone();
        let mut cursor = index;
        let mut siblings = Vec::new();
        while level.len() > 1 {
            let sibling = if cursor % 2 == 0 {
                level.get(cursor + 1).copied().unwrap_or(level[cursor])
            } else {
                level[cursor - 1]
            };
            siblings.push(sibling);
            level = next_level(&level);
            cursor /= 2;
        }
        Ok(MerkleProof {
            project: self.project.clone(),
            leaf_hash: leaf,
            leaf_index: u32::try_from(index).map_err(|_| ProvenanceError::MalformedProof)?,
            leaf_count: u32::try_from(self.leaves.len())
                .map_err(|_| ProvenanceError::MalformedProof)?,
            siblings,
        })
    }

    /// Returns canonicalized commitments for receipt construction.
    pub fn commitments(&self) -> &[ProjectCommitment] {
        &self.commitments
    }
}

/// Merkle inclusion proof bound to a project and exact batch cardinality.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MerkleProof {
    /// Stable project identity.
    pub project: String,
    /// Project-scoped leaf hash.
    pub leaf_hash: Digest32,
    /// Zero-based leaf position in the canonical batch.
    pub leaf_index: u32,
    /// Exact leaf count in the batch.
    pub leaf_count: u32,
    /// Bottom-up sibling hashes.
    pub siblings: Vec<Digest32>,
}

impl MerkleProof {
    /// Verifies the commitment, project scope, proof shape, and Merkle root.
    pub fn verify(
        &self,
        commitment: &ProjectCommitment,
        expected_root: Digest32,
    ) -> Result<(), ProvenanceError> {
        if self.project != commitment.project {
            return Err(ProvenanceError::ProjectMismatch);
        }
        if self.leaf_hash != commitment.leaf_hash()? {
            return Err(ProvenanceError::LeafMismatch);
        }
        if self.leaf_count == 0 || self.leaf_index >= self.leaf_count {
            return Err(ProvenanceError::MalformedProof);
        }
        let mut count =
            usize::try_from(self.leaf_count).map_err(|_| ProvenanceError::MalformedProof)?;
        if count > MAX_BATCH_SIZE {
            return Err(ProvenanceError::MalformedProof);
        }
        let mut index =
            usize::try_from(self.leaf_index).map_err(|_| ProvenanceError::MalformedProof)?;
        let expected_depth = proof_depth(count);
        if self.siblings.len() != expected_depth {
            return Err(ProvenanceError::MalformedProof);
        }

        let mut current = self.leaf_hash;
        for sibling in &self.siblings {
            let sibling_index = if index % 2 == 0 {
                index.saturating_add(1)
            } else {
                index - 1
            };
            if sibling_index >= count && sibling != &current {
                return Err(ProvenanceError::MalformedProof);
            }
            current = if index % 2 == 0 {
                hash_node(&current, sibling)
            } else {
                hash_node(sibling, &current)
            };
            index /= 2;
            count = count.div_ceil(2);
        }
        if current != expected_root {
            return Err(ProvenanceError::RootMismatch);
        }
        Ok(())
    }
}

/// Minimal data placed in a BSV `OP_FALSE OP_RETURN` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorPayload {
    /// Target BSV network.
    pub network: BsvNetwork,
    /// Hash of the stable project identifier, avoiding disclosure of private names.
    pub project_digest: Digest32,
    /// Batched project commitment root.
    pub merkle_root: Digest32,
    /// Exact batch leaf count.
    pub leaf_count: u32,
    /// Deterministic ID of the anchored project snapshot.
    pub snapshot_id: Digest32,
}

impl AnchorPayload {
    /// Creates a payload from a project and deterministic batch.
    pub fn new(
        network: BsvNetwork,
        project: &str,
        batch: &MerkleBatch,
        snapshot_id: Digest32,
    ) -> Result<Self, ProvenanceError> {
        validate_project(project)?;
        if project != batch.project() {
            return Err(ProvenanceError::ProjectMismatch);
        }
        Ok(Self {
            network,
            project_digest: hash(project.as_bytes()),
            merkle_root: batch.root(),
            leaf_count: u32::try_from(batch.len()).map_err(|_| ProvenanceError::BatchTooLarge {
                count: batch.len(),
                max: MAX_BATCH_SIZE,
            })?,
            snapshot_id,
        })
    }

    /// Returns deterministic, versioned payload bytes.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(ANCHOR_DOMAIN);
        bytes.extend_from_slice(&self.network.protocol_code().to_be_bytes());
        bytes.extend_from_slice(&self.project_digest);
        bytes.extend_from_slice(&self.merkle_root);
        bytes.extend_from_slice(&self.leaf_count.to_be_bytes());
        bytes.extend_from_slice(&self.snapshot_id);
        bytes
    }

    /// Builds the canonical `OP_FALSE OP_RETURN <payload>` locking script.
    pub fn locking_script(&self) -> Result<Vec<u8>, ProvenanceError> {
        let payload = self.canonical_bytes();
        let mut script = vec![0x00, 0x6a];
        push_data(&mut script, &payload)?;
        Ok(script)
    }

    /// Verifies that a project name corresponds to the committed project digest.
    pub fn verify_project(&self, project: &str) -> Result<(), ProvenanceError> {
        validate_project(project)?;
        if hash(project.as_bytes()) != self.project_digest {
            return Err(ProvenanceError::ProjectMismatch);
        }
        Ok(())
    }
}

/// Transaction material returned by an external wallet adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedAnchorTransaction {
    /// Standard raw BSV transaction bytes.
    pub raw_transaction: Vec<u8>,
    /// Optional Atomic BEEF representation from a BRC-100 wallet.
    pub atomic_beef: Option<Vec<u8>>,
    /// Display-order BSV transaction ID.
    pub txid: Digest32,
    /// Output index containing the anchor payload.
    pub anchor_output_index: u32,
}

/// Wallet/provider boundary. Implementations may use BRC-100, a hardware wallet,
/// or an offline signer; protocol code never receives wallet secrets.
pub trait AnchorTransactionProvider {
    /// Creates and signs an anchor transaction without assuming broadcast.
    fn create_anchor_transaction(
        &self,
        network: BsvNetwork,
        payload: &AnchorPayload,
    ) -> Result<SignedAnchorTransaction, ProvenanceError>;
}

/// A normalized broadcaster response persisted with the receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BroadcastReceipt {
    /// Transaction accepted or already known by the provider.
    pub accepted: bool,
    /// Normalized provider status such as `SEEN_ON_NETWORK`.
    pub status: String,
    /// Provider identifier or endpoint name.
    pub provider: String,
}

/// Asynchronous-network boundary expressed synchronously so this crate remains
/// runtime-neutral. Integration crates may block, spawn, or adapt async clients.
pub trait AnchorBroadcaster {
    /// Broadcasts signed transaction material to the selected BSV network.
    fn broadcast(
        &self,
        network: BsvNetwork,
        transaction: &SignedAnchorTransaction,
    ) -> Result<BroadcastReceipt, ProvenanceError>;
}

/// Verifies txid, raw transaction structure, output index, and exact anchor script.
pub fn verify_anchor_transaction(
    transaction: &SignedAnchorTransaction,
    payload: &AnchorPayload,
) -> Result<(), ProvenanceError> {
    if transaction_id(&transaction.raw_transaction) != transaction.txid {
        return Err(ProvenanceError::TransactionIdMismatch);
    }
    let outputs = parse_outputs(&transaction.raw_transaction)?;
    let index = usize::try_from(transaction.anchor_output_index)
        .map_err(|_| ProvenanceError::AnchorOutputMissing)?;
    let script = outputs
        .get(index)
        .ok_or(ProvenanceError::AnchorOutputMissing)?;
    if script != &payload.locking_script()? {
        return Err(ProvenanceError::AnchorPayloadMismatch);
    }
    Ok(())
}

/// Computes the display-order BSV transaction ID (double SHA-256, byte-reversed).
pub fn transaction_id(raw_transaction: &[u8]) -> Digest32 {
    let first: Digest32 = Sha256::digest(raw_transaction).into();
    let mut second: Digest32 = Sha256::digest(first).into();
    second.reverse();
    second
}

/// Provenance construction and verification errors.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProvenanceError {
    /// A project identifier was empty or invalid.
    #[error("invalid project identifier")]
    InvalidProject,
    /// No commitments were supplied.
    #[error("a Merkle batch cannot be empty")]
    EmptyBatch,
    /// The configured batch limit was exceeded.
    #[error("Merkle batch contains {count} leaves; maximum is {max}")]
    BatchTooLarge {
        /// Attempted count.
        count: usize,
        /// Maximum allowed count.
        max: usize,
    },
    /// Commitments from multiple projects were mixed.
    #[error("all commitments in a batch must belong to one project")]
    MixedProjects,
    /// The same leaf was included more than once.
    #[error("duplicate Merkle leaf")]
    DuplicateLeaf,
    /// Requested commitment is absent from the batch.
    #[error("commitment not found in Merkle batch")]
    CommitmentNotFound,
    /// Project scope did not match.
    #[error("project scope mismatch")]
    ProjectMismatch,
    /// Proof leaf did not match the supplied commitment.
    #[error("Merkle proof leaf mismatch")]
    LeafMismatch,
    /// Proof shape, depth, index, or duplicate-last sibling was invalid.
    #[error("malformed Merkle proof")]
    MalformedProof,
    /// Proof did not resolve to the expected root.
    #[error("Merkle root mismatch")]
    RootMismatch,
    /// A field could not be length encoded.
    #[error("protocol field length overflow")]
    LengthOverflow,
    /// Anchor payload is too large for the supported canonical script.
    #[error("anchor payload is too large")]
    AnchorPayloadTooLarge,
    /// Raw transaction was truncated or structurally invalid.
    #[error("malformed raw BSV transaction")]
    MalformedTransaction,
    /// Stored txid did not match raw transaction bytes.
    #[error("BSV transaction ID mismatch")]
    TransactionIdMismatch,
    /// Anchor output index was absent.
    #[error("anchor output missing from BSV transaction")]
    AnchorOutputMissing,
    /// Anchor output did not contain the exact canonical payload.
    #[error("BSV transaction anchor payload mismatch")]
    AnchorPayloadMismatch,
    /// External wallet/provider/broadcaster failure.
    #[error("external BSV provider failure: {0}")]
    Provider(String),
}

fn validate_project(project: &str) -> Result<(), ProvenanceError> {
    if project.trim().is_empty() || project.as_bytes().contains(&0) {
        return Err(ProvenanceError::InvalidProject);
    }
    Ok(())
}

fn put_text(out: &mut Vec<u8>, value: &str) -> Result<(), ProvenanceError> {
    let len = u32::try_from(value.len()).map_err(|_| ProvenanceError::LengthOverflow)?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn hash(bytes: &[u8]) -> Digest32 {
    Sha256::digest(bytes).into()
}

fn hash_node(left: &Digest32, right: &Digest32) -> Digest32 {
    let mut bytes = Vec::with_capacity(NODE_DOMAIN.len() + 64);
    bytes.extend_from_slice(NODE_DOMAIN);
    bytes.extend_from_slice(left);
    bytes.extend_from_slice(right);
    hash(&bytes)
}

fn merkle_root(leaves: &[Digest32]) -> Digest32 {
    let mut level = leaves.to_vec();
    while level.len() > 1 {
        level = next_level(&level);
    }
    level[0]
}

fn next_level(level: &[Digest32]) -> Vec<Digest32> {
    let mut next = Vec::with_capacity(level.len().div_ceil(2));
    for pair in level.chunks(2) {
        let right = pair.get(1).unwrap_or(&pair[0]);
        next.push(hash_node(&pair[0], right));
    }
    next
}

fn proof_depth(mut count: usize) -> usize {
    let mut depth = 0;
    while count > 1 {
        count = count.div_ceil(2);
        depth += 1;
    }
    depth
}

fn push_data(script: &mut Vec<u8>, payload: &[u8]) -> Result<(), ProvenanceError> {
    match payload.len() {
        0..=75 => script.push(payload.len() as u8),
        76..=255 => {
            script.push(0x4c);
            script.push(payload.len() as u8);
        }
        256..=65_535 => {
            script.push(0x4d);
            script.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        }
        _ => return Err(ProvenanceError::AnchorPayloadTooLarge),
    }
    script.extend_from_slice(payload);
    Ok(())
}

fn parse_outputs(raw: &[u8]) -> Result<Vec<Vec<u8>>, ProvenanceError> {
    let mut cursor = Cursor::new(raw);
    cursor.take(4)?;
    let input_count = cursor.varint()?;
    if input_count > MAX_TX_ITEMS {
        return Err(ProvenanceError::MalformedTransaction);
    }
    for _ in 0..input_count {
        cursor.take(32 + 4)?;
        let script_len = cursor.varint()?;
        if script_len > MAX_SCRIPT_BYTES {
            return Err(ProvenanceError::MalformedTransaction);
        }
        cursor.take(
            usize::try_from(script_len).map_err(|_| ProvenanceError::MalformedTransaction)?,
        )?;
        cursor.take(4)?;
    }
    let output_count = cursor.varint()?;
    if output_count > MAX_TX_ITEMS {
        return Err(ProvenanceError::MalformedTransaction);
    }
    let mut outputs = Vec::with_capacity(
        usize::try_from(output_count).map_err(|_| ProvenanceError::MalformedTransaction)?,
    );
    for _ in 0..output_count {
        cursor.take(8)?;
        let script_len = cursor.varint()?;
        if script_len > MAX_SCRIPT_BYTES {
            return Err(ProvenanceError::MalformedTransaction);
        }
        outputs.push(
            cursor
                .take(
                    usize::try_from(script_len)
                        .map_err(|_| ProvenanceError::MalformedTransaction)?,
                )?
                .to_vec(),
        );
    }
    cursor.take(4)?;
    if cursor.remaining() != 0 {
        return Err(ProvenanceError::MalformedTransaction);
    }
    Ok(outputs)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], ProvenanceError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(ProvenanceError::MalformedTransaction)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(ProvenanceError::MalformedTransaction)?;
        self.offset = end;
        Ok(value)
    }

    fn varint(&mut self) -> Result<u64, ProvenanceError> {
        let prefix = self.take(1)?[0];
        match prefix {
            0x00..=0xfc => Ok(u64::from(prefix)),
            0xfd => {
                let bytes: [u8; 2] = self
                    .take(2)?
                    .try_into()
                    .map_err(|_| ProvenanceError::MalformedTransaction)?;
                let value = u64::from(u16::from_le_bytes(bytes));
                if value < 0xfd {
                    return Err(ProvenanceError::MalformedTransaction);
                }
                Ok(value)
            }
            0xfe => {
                let bytes: [u8; 4] = self
                    .take(4)?
                    .try_into()
                    .map_err(|_| ProvenanceError::MalformedTransaction)?;
                let value = u64::from(u32::from_le_bytes(bytes));
                if value <= u64::from(u16::MAX) {
                    return Err(ProvenanceError::MalformedTransaction);
                }
                Ok(value)
            }
            0xff => {
                let bytes: [u8; 8] = self
                    .take(8)?
                    .try_into()
                    .map_err(|_| ProvenanceError::MalformedTransaction)?;
                let value = u64::from_le_bytes(bytes);
                if value <= u64::from(u32::MAX) {
                    return Err(ProvenanceError::MalformedTransaction);
                }
                Ok(value)
            }
        }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commitment(project: &str, byte: u8) -> ProjectCommitment {
        ProjectCommitment {
            project: project.to_owned(),
            kind: CommitmentKind::ContributionSnapshot,
            object_id: [byte; 32],
            state_digest: [byte.wrapping_add(1); 32],
        }
    }

    fn raw_tx(script: &[u8]) -> Vec<u8> {
        let mut tx = Vec::new();
        tx.extend_from_slice(&1_i32.to_le_bytes());
        tx.push(0); // zero inputs: structurally parseable test fixture, not broadcastable
        tx.push(1); // one output
        tx.extend_from_slice(&0_u64.to_le_bytes());
        tx.push(u8::try_from(script.len()).expect("small fixture script"));
        tx.extend_from_slice(script);
        tx.extend_from_slice(&0_u32.to_le_bytes());
        tx
    }

    #[test]
    fn single_leaf_tree_has_empty_valid_proof() {
        let item = commitment("project:a", 1);
        let batch = MerkleBatch::new(vec![item.clone()]).expect("batch");
        let proof = batch.proof_for(&item).expect("proof");
        assert!(proof.siblings.is_empty());
        proof.verify(&item, batch.root()).expect("verify");
    }

    #[test]
    fn odd_leaf_count_uses_verified_duplicate_last_rule() {
        let items = vec![
            commitment("project:a", 1),
            commitment("project:a", 2),
            commitment("project:a", 3),
        ];
        let batch = MerkleBatch::new(items.clone()).expect("batch");
        for item in items {
            batch
                .proof_for(&item)
                .expect("proof")
                .verify(&item, batch.root())
                .expect("verify");
        }
    }

    #[test]
    fn input_order_does_not_change_root() {
        let a = commitment("project:a", 1);
        let b = commitment("project:a", 2);
        let left = MerkleBatch::new(vec![a.clone(), b.clone()]).expect("left");
        let right = MerkleBatch::new(vec![b, a]).expect("right");
        assert_eq!(left.root(), right.root());
    }

    #[test]
    fn batch_size_limit_is_enforced() {
        let items = (0..=MAX_BATCH_SIZE)
            .map(|index| {
                let mut item = commitment("project:a", 1);
                item.object_id[..8].copy_from_slice(&(index as u64).to_be_bytes());
                item
            })
            .collect();
        assert!(matches!(
            MerkleBatch::new(items),
            Err(ProvenanceError::BatchTooLarge { .. })
        ));
    }

    #[test]
    fn same_object_cannot_be_reused_in_another_project() {
        let item = commitment("project:a", 1);
        let batch = MerkleBatch::new(vec![item.clone()]).expect("batch");
        let proof = batch.proof_for(&item).expect("proof");
        let tampered = commitment("project:b", 1);
        assert_eq!(
            proof.verify(&tampered, batch.root()),
            Err(ProvenanceError::ProjectMismatch)
        );
        assert_ne!(
            item.leaf_hash().expect("a"),
            tampered.leaf_hash().expect("b")
        );
    }

    #[test]
    fn sibling_tampering_is_detected() {
        let a = commitment("project:a", 1);
        let b = commitment("project:a", 2);
        let batch = MerkleBatch::new(vec![a.clone(), b]).expect("batch");
        let mut proof = batch.proof_for(&a).expect("proof");
        proof.siblings[0][0] ^= 1;
        assert_eq!(
            proof.verify(&a, batch.root()),
            Err(ProvenanceError::RootMismatch)
        );
    }

    #[test]
    fn malformed_proof_shape_is_rejected() {
        let a = commitment("project:a", 1);
        let b = commitment("project:a", 2);
        let batch = MerkleBatch::new(vec![a.clone(), b]).expect("batch");
        let mut proof = batch.proof_for(&a).expect("proof");
        proof.siblings.clear();
        assert_eq!(
            proof.verify(&a, batch.root()),
            Err(ProvenanceError::MalformedProof)
        );
    }

    #[test]
    fn raw_transaction_binds_exact_anchor_payload_and_txid() {
        let item = commitment("project:a", 1);
        let batch = MerkleBatch::new(vec![item]).expect("batch");
        let payload =
            AnchorPayload::new(BsvNetwork::Testnet, "project:a", &batch, [9; 32]).expect("payload");
        let raw = raw_tx(&payload.locking_script().expect("script"));
        let transaction = SignedAnchorTransaction {
            txid: transaction_id(&raw),
            raw_transaction: raw,
            atomic_beef: None,
            anchor_output_index: 0,
        };
        verify_anchor_transaction(&transaction, &payload).expect("verify");
    }

    #[test]
    fn anchor_tampering_is_detected() {
        let item = commitment("project:a", 1);
        let batch = MerkleBatch::new(vec![item]).expect("batch");
        let payload =
            AnchorPayload::new(BsvNetwork::Testnet, "project:a", &batch, [9; 32]).expect("payload");
        let raw = raw_tx(&payload.locking_script().expect("script"));
        let mut transaction = SignedAnchorTransaction {
            txid: transaction_id(&raw),
            raw_transaction: raw,
            atomic_beef: None,
            anchor_output_index: 0,
        };
        transaction.raw_transaction[5] ^= 1;
        assert_eq!(
            verify_anchor_transaction(&transaction, &payload),
            Err(ProvenanceError::TransactionIdMismatch)
        );
    }
}
