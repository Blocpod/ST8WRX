# ST8WRX

**An AI-native venture network where humans, agents, compute, and ideas build together.**

Build together. Prove what you created.

ST8WRX combines a signed collaboration workspace with project contribution
accounting and a thin BSV proof/settlement layer. The collaboration substrate is
derived from Block's open-source Buzz project; ST8WRX preserves its useful Nostr,
Git, workflow, agent, search, audit, and shared-compute machinery while adding an
independent venture, provenance, and economic architecture.

## Current milestone

The repository currently contains the imported Buzz collaboration baseline plus
the first isolated ST8WRX protocol slice:

- `st8-contribution-protocol` — deterministic contribution records, decisions,
  explicit permanent protocol codes, canonical set handling, and Contribution
  Unit invariants.
- `st8-bsv-provenance` — project-scoped commitments, deterministic Merkle
  batching and proofs, BSV anchor payloads, raw-transaction commitment checks,
  and wallet/broadcaster abstractions.
- `st8-contribution-engine` — signed Buzz/NIP-MP evidence grounding,
  cryptographically signed founder approvals, CU awards, persisted receipts,
  and independent end-to-end verification.
- `st8wrx-contribution` — governance-intent, prepare, finalize, and verify CLI
  for the BRC-100 wallet and ARC boundary.

The protocol implementation does not put blockchain calls in Buzz's synchronous
collaboration path and does not hold wallet secrets.

## Architecture

ST8WRX is organized into five conceptual layers:

1. **Buzz collaboration layer** — signed Nostr events, humans and agents,
   projects, channels, Git, workflows, search, audit, and Buzz Mesh compute.
2. **ST8WRX contribution protocol** — evidence, attribution, impact assessment,
   project acceptance, and non-transferable Contribution Units.
3. **ST8 Compute** — capability registry, metering, receipts, pricing,
   reputation, net balances, and periodic settlement.
4. **BSV proof/economic layer** — project-scoped commitments, Merkle batches,
   BEEF/SPV receipts, settlement, payments, and later narrowly scoped contracts.
5. **ST8 Market** — future discovery and exchange for projects, products,
   agents, compute, APIs, models, datasets, licenses, and bounties.

See [ST8WRX architecture](docs/st8wrx/ARCHITECTURE.md), the
[ST8 Compute protocol and operations](docs/st8wrx/ST8-COMPUTE.md), the
[upstream audit](docs/st8wrx/UPSTREAM-AUDIT.md), and the
[milestone runbook](docs/st8wrx/MILESTONE-1.md).

## Protocol checks

```bash
. ./bin/activate-hermit
cargo test -p st8-contribution-protocol \
  -p st8-bsv-provenance \
  -p st8-contribution-engine
cargo clippy -p st8-contribution-protocol \
  -p st8-bsv-provenance \
  -p st8-contribution-engine --all-targets -- -D warnings
```

The broader upstream build and development commands remain available through the
existing `Justfile`, desktop, mobile, web, relay, and CLI packages. Internal
`buzz-*` crate names, event kinds, environment variables, and `buzz://`
compatibility links intentionally remain where renaming would break protocol or
operational compatibility.

## BSV testnet receipt flow

The first live slice deliberately separates ST8WRX from wallet custody:

```bash
# 1. Verify signed Buzz evidence and emit the exact decision founders sign.
cargo run -p st8-contribution-engine --bin st8wrx-contribution -- \
  intent --input proposal.json --output intent.json

# 2. Sign the emitted Nostr event template with the required independent
#    project authorities and add the full events to proposal.approval_events.

# 3. Verify the signatures and emit the exact testnet locking script.
cargo run -p st8-contribution-engine --bin st8wrx-contribution -- \
  prepare --input proposal.json --output prepared.json

# 4. Ask a BRC-100 wallet to create/sign with noSend, then broadcast through a
#    public BSV provider. Store raw transaction, Atomic BEEF, output index, and
#    provider result in external-result.json. No private key enters ST8WRX.

# 5. Bind and persist the independently verified receipt.
cargo run -p st8-contribution-engine --bin st8wrx-contribution -- \
  finalize --prepared prepared.json --result external-result.json \
  --output receipt.json

# 6. Verify later without trusting the original process.
cargo run -p st8-contribution-engine --bin st8wrx-contribution -- \
  verify --receipt receipt.json
```

The checked-in [Milestone 1 receipt](docs/st8wrx/milestone-1/README.md) grounds a
real 1,000-CU contribution in signed ST8WRX activity, includes two authorized
governance approvals, and independently verifies its mined BSV testnet anchor.

See [BSV tooling](docs/st8wrx/BSV-TOOLING.md) for current standards and the
JSON contracts.

## Contribution Units are not equity or tokens

Contribution Units record project contribution weight. They are initially
non-transferable accounting units—not equity, securities, legal ownership, a
platform token, or a project token. AI may recommend impact; project governance
makes the recorded decision.

## License and upstream attribution

ST8WRX is licensed under the [Apache License 2.0](LICENSE). It incorporates and
modifies software from [Block's Buzz project](https://github.com/block/buzz),
also licensed under Apache-2.0. See [NOTICE](NOTICE) for attribution. ST8WRX is an
independent project and is not affiliated with or endorsed by Block, Inc.
