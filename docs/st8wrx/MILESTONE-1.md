# Milestone 1 runbook

## Exit criteria

- [x] Complete upstream-compatible repository build.
- [x] Preserve imported Buzz collaboration source and extension seams.
- [x] Deterministic contribution protocol tests.
- [x] Project-scoped BSV commitment and Merkle proof tests.
- [x] Real signed Buzz/NIP-MP + NIP-34 evidence grounding.
- [x] Explicit project governance decision and CU award.
- [x] Deterministic contribution snapshot.
- [x] Wallet-ready BSV testnet anchor payload.
- [ ] Live funded BSV testnet transaction created and broadcast.
- [x] Persisted receipt and independent verifier.
- [x] Signature, scope, sibling, malformed-proof, snapshot, txid, and payload
  tampering tests.
- [x] Architecture, upstream audit, BSV tooling, attribution, and handoff docs.
- [x] No inherited publishing workflow remains active.

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
