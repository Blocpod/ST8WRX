# Milestone 1 runbook

## Exit criteria

- [x] Complete upstream-compatible repository build.
- [x] Preserve imported Buzz collaboration source and extension seams.
- [x] Deterministic contribution protocol tests.
- [x] Project-scoped BSV commitment and Merkle proof tests.
- [x] Real signed Buzz/NIP-MP + NIP-34 evidence grounding tied to foundation
  commit `789f66d370f58a4910d43dc50c8e73e4c8bb5c6b`.
- [x] Explicit project governance decision with two independently signed founder
  approvals and a 1,000-CU award.
- [x] Deterministic contribution snapshot.
- [x] Wallet-ready BSV testnet anchor payload.
- [ ] Live funded BSV testnet transaction created and broadcast.
- [x] Persisted receipt and independent verifier.
- [x] Signature, scope, sibling, malformed-proof, snapshot, txid, and payload
  tampering tests.
- [x] Architecture, upstream audit, BSV tooling, attribution, and handoff docs.
- [x] No inherited publishing workflow remains active.

The signed public proposal, decision intent, and wallet-ready prepared anchor
are preserved under [`milestone-1/`](milestone-1/README.md). The unchecked live
broadcast item is the only exit criterion requiring external funded authority.

## Focused validation

```bash
. ./bin/activate-hermit
cargo fmt --all -- --check
cargo test -p st8-contribution-protocol \
  -p st8-bsv-provenance \
  -p st8-contribution-engine
cargo clippy -p st8-contribution-protocol \
  -p st8-bsv-provenance \
  -p st8-contribution-engine --all-targets -- -D warnings
```

## Live handoff

Provide a reachable, funded BRC-100 testnet wallet service or run the wallet step
locally. Do not send a seed phrase or WIF. Use `st8wrx-contribution prepare`, sign
and broadcast the exact emitted script, then use `finalize` and `verify`.

After the live receipt verifies and the broader imported build is green, Milestone
1 is complete and work moves to the compute accounting slice.
