# Milestone 1 contribution handoff

These files are the public, signed pre-transaction state for the first real
ST8WRX contribution slice. The evidence event names Git commit
`789f66d370f58a4910d43dc50c8e73e4c8bb5c6b`, the repository's independent
foundation commit. It is scoped by a signed Buzz NIP-MP project event and NIP-34
repository coordinate.

- `proposal.json` preserves the signed project event, signed Git evidence, the
  two-founder policy, and two independently signed governance approvals.
- `intent.json` records the exact decision intent and unsigned Nostr event
  template each founder signed.
- `prepared.json` is the independently verified contribution, 1,000 CU award,
  project-scoped Merkle proof, and exact BSV testnet locking script waiting for
  an external wallet.

No private key, seed phrase, WIF, `nsec`, or wallet credential is present. The
fixture generator creates ephemeral Nostr keys in memory and writes only signed
public events. It is a milestone evidence fixture, not a production identity or
governance workflow.

## Stable identifiers

- Contribution ID:
  `ada2ce7479748cb4eb931e874e05264ecc2cbab841334e250d6ad15a4c5463c9`
- Governance intent:
  `5130b6ca161540260178aa31c484c2d16e451c90c13b20680bcc15580c566980`
- Snapshot ID:
  `c6f4774e9425c8e3cc813f23f873acf5b8d6373fa0325e397ec8a34e097771a2`
- Merkle root:
  `a5d86d22589fe044d4e218d0bc909daa5d57d75d29227bddfd1f7f88de6ace26`

## Verify and finish

Re-running `prepare` must reproduce the same IDs, root, and locking script:

```bash
cargo run -p st8-contribution-engine --bin st8wrx-contribution -- \
  intent --input docs/st8wrx/milestone-1/proposal.json \
  --output /tmp/st8wrx-intent.json
cargo run -p st8-contribution-engine --bin st8wrx-contribution -- \
  prepare --input docs/st8wrx/milestone-1/proposal.json \
  --output /tmp/st8wrx-prepared.json
```

The remaining live step requires a funded external BRC-100 testnet wallet to
create and sign a transaction containing the exact `locking_script_hex`, then an
ARC-compatible broadcaster. Put only the resulting public transaction material
in `external-result.json`; never provide a private key. Run `finalize` and
`verify` as documented in `../BSV-TOOLING.md`.
