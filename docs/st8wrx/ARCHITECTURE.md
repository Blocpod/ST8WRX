# ST8WRX architecture

## Boundary rule

Buzz remains the live collaboration state engine. ST8WRX consumes signed,
content-addressed evidence from that engine and produces durable project economy
state beside it. Blockchain calls never run in a synchronous message, Git push,
workflow, agent, or mesh-compute hot path.

```text
Buzz signed activity
        |
        v
Evidence grounding -----> ContributionRecord
                                |
                                v
Impact recommendation --> Project governance decision
                                |
                                v
                     Contribution Units + snapshot
                                |
                                v
                  project-scoped Merkle commitment
                                |
                                v
                 BRC-100 wallet -> ARC -> BSV testnet
                                |
                                v
                    persisted verifiable receipt
```

## Reused collaboration seams

- Nostr event signatures and IDs from `buzz-core` establish the evidence floor.
- NIP-MP kind 30621 project events and NIP-34 repository coordinates from
  `buzz-sdk` establish project scope without a duplicate project database.
- Git patches/statuses, workflows, agent metrics, audit entries, artifacts, and
  mesh jobs remain the authoritative activity sources.
- Existing Buzz project, channel, search, workflow, Git, agent, and mesh features
  remain intact. Internal crate names and protocol compatibility identifiers are
  not globally renamed.

## Protocol crates

### `st8-contribution-protocol`

Zero-I/O. Every enum entering identity has a permanent explicit integer code.
Strings and byte sequences are length-delimited. Evidence and approver sets are
sorted and deduplicated before hashing. Rejected decisions can award no CU;
accepted/adjusted decisions must award CU and bind to the exact record and
project.

### `st8-bsv-provenance`

Zero-I/O. A leaf commits to domain, project, commitment kind, object ID, and
state digest. Batches are canonicalized by leaf hash, bounded to 4,096 leaves,
and use a documented duplicate-last rule for odd levels. Proof verification
checks shape, index, cardinality, project scope, duplicate-last siblings, and
root. The BSV output commits only hashes, counts, network, and snapshot ID.

Wallet and broadcaster traits separate four authorities:

- Buzz/Nostr identity;
- project governance authority;
- BSV wallet authority; and
- compute node identity.

No private key crosses those boundaries.

### `st8-contribution-engine`

I/O integration layer. It retains the full signed NIP-MP project event and full
signed evidence events so a later verifier can reconstruct evidence references,
project membership, and contributor identity. It applies an explicit founder
threshold policy, preserves the original record even when decisions change,
builds one deterministic project snapshot, and atomically persists the final
receipt.

## On-chain data policy

The anchor output contains no source code, prompt, PII, secret, credential,
private customer data, or raw project name. It contains a domain-separated
project digest, Merkle root, leaf count, testnet/mainnet code, and snapshot ID.
Source and working collaboration state stay in Buzz; large artifacts stay in
content-addressed object storage.

## Verification claim

The V1 receipt verifies:

1. Nostr IDs and Schnorr signatures of the project and evidence events;
2. evidence membership in the signed NIP-MP project or one of its repositories;
3. deterministic contribution identity and project-bound governance decision;
4. the non-zero CU award and approval threshold;
5. canonical snapshot state;
6. project-scoped Merkle inclusion;
7. exact `OP_FALSE OP_RETURN` payload in the raw BSV transaction;
8. the raw transaction's BSV txid; and
9. the persisted broadcaster acceptance response.

Header-chain/Merkle-path confirmation of the BSV transaction is the next receipt
extension. Atomic BEEF is retained when supplied so an SPV verifier can add that
check without changing contribution identity.
