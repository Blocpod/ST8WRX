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

Zero-I/O project leaves, bounded canonical Merkle batches, strict proofs, BSV
commitment scripts, transaction IDs, and external broadcast receipts. The
on-chain payload contains only domain/version, project digest, Merkle root,
leaf count, network, and snapshot ID.

### `st8-contribution-engine`

Verifies complete signed project/evidence/governance material, derives approvers
from verified Nostr signatures, enforces the project policy threshold, builds
deterministic contribution/project snapshots, and creates the public receipt.

### `st8-anchor-worker`

Claims persisted snapshot jobs under a lease. It asks an external BRC-100
testnet wallet to fund and sign an exact zero-satoshi commitment with `noSend`,
persists the signed transaction before broadcast, and resumes it on every
retry—never asking the wallet to fund twice. It prefers ARC and falls back to
the official WhatsOnChain testnet broadcaster when ARC requires credentials.
It independently fetches the raw transaction, transaction record, mined BEEF,
BUMP, and block header before confirming the job.

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
10. exact Atomic BEEF subject/ancestry material;
11. public testnet provider observation; and
12. mined BEEF/BUMP inclusion, Merkle root, block height, block-header hash, and
    positive BSV testnet confirmation.
