# ST8WRX architecture

## Boundary rule

Buzz remains the live collaboration state engine. ST8WRX consumes signed,
content-addressed project activity and produces durable project-economy state
beside it. Blockchain calls run only in the leased asynchronous anchor worker;
chat, Git, workflow, agent, and project collaboration never wait on a wallet or
network provider.

```text
signed Buzz/Git activity
          |
          v
project evidence -> contribution claim -> signed governance threshold
                                             |
                                             v
                              Contribution Units + ledger projection
                                             |
                                             v
                              deterministic project snapshot/Merkle root
                                             |
                                             v
                       external BRC-100 wallet -> public BSV testnet
                                             |
                                             v
                         persisted receipt -> independent verifier
```

## Reused collaboration seams

- Nostr event signatures and IDs from `buzz-core` establish evidence
  authenticity and contributor identity.
- NIP-MP kind 30621 project events and NIP-34 repository coordinates establish
  project scope without a duplicate project system.
- The relay verifies contribution claims and governance events through the same
  signed-event ingestion path, then materializes accepted/rejected ledger state.
- Git, workflows, agent activity, audit records, artifacts, and mesh jobs remain
  authoritative evidence sources.

## Protocol and runtime crates

### `st8-contribution-protocol`

Zero-I/O canonical contribution identities. Enums have permanent integer codes;
strings and byte sequences are length-delimited; evidence and approver sets are
sorted and deduplicated. Rejected decisions award no CU. Accepted/adjusted
decisions bind the exact record, project, policy, rationale, status, and award.

### `st8-bsv-provenance`

Zero-I/O project leaves, canonical Merkle batches, strict proofs, BSV
commitment scripts, transaction IDs, and external broadcast receipts. The
on-chain payload contains only domain/version, project digest, Merkle root,
leaf count, network, and snapshot ID.

### `st8-contribution-engine`

Verifies complete signed project/evidence/governance material, derives approvers
from verified Nostr signatures, enforces the project policy threshold, builds
deterministic contribution/project snapshots, and creates the public receipt.

### `st8-anchor-worker`

Claims persisted snapshot jobs under a lease. It records a durable wallet-call
intent before requesting synchronous signing/processing from the external BRC-100
testnet wallet, then persists the returned transaction and Atomic BEEF. Retries
reuse those exact bytes. If the wallet call or persistence is interrupted before
transaction bytes are saved, automatic wallet calls stop for that job. The
operator must recover the exact public transaction; a request reference alone is
not treated as proof of wallet idempotency. It prefers ARC and falls back to
the official WhatsOnChain testnet broadcaster when ARC requires credentials.
It independently fetches transaction bytes, mined BEEF/BUMP, and a block header
before completing the job. See [wallet recovery](ANCHOR-RECOVERY.md).

Wallet private keys, WIFs, mnemonics, and seed phrases never enter ST8WRX.

## Independent verification claim

Receipt V1 proves:

1. signed evidence authenticity and contributor attribution;
2. evidence membership in the signed project/repository scope;
3. deterministic contribution identity;
4. signed authorized governance approvals and threshold satisfaction;
5. the signed Contribution Unit award;
6. deterministic contribution and project snapshots;
7. project-scoped Merkle inclusion;
8. the exact commitment output in the raw BSV transaction;
9. the independently recomputed transaction ID;
10. exact Atomic BEEF subject bytes; and
11. mined BEEF/BUMP inclusion, Merkle root, block-header hash, and proof of work
    against the header's stated target.

Provider observations, height, and confirmation counts in a receipt are not
authenticated chain membership. See [verification trust boundaries](PROOF-VERIFICATION.md)
for derived states and independent block-hash pinning.

## Contribution history capacity

Project snapshots retain all accepted contributions. Merkle batch cardinality
is bounded by the V1 format's `u32` count/index fields, rather than an operational
4,096-item ceiling. Existing roots, proof encodings, payloads, and receipts stay
unchanged. Proof verification validates the exact logarithmic sibling count
(at most 32), even for the largest representable count.

The relay continues to enforce inbound event/frame size limits; project history
is assembled from accepted database records, not a client-supplied history vector.
Local receipt-file readers still require memory proportional to file size and
should not be exposed as an unrestricted public parsing service.

This removes the 4,097th-contribution hard stop, not the cost of full-history
rebuilds. Each new award still reconstructs and stores a complete snapshot
(O(n log n) Merkle construction time and O(n) memory per snapshot); incremental
or checkpointed history remains necessary for large sustained workloads.
