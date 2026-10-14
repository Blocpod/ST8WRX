# Receipt verification trust boundary

The anchor worker verifies signed contribution or compute records, accounting,
commitment bytes, transaction IDs, Atomic BEEF subject bytes, mined Merkle
inclusion, the complete 80-byte block-header hash, and proof of work against the
header's compact target. It rejects negative, zero, overflowing, and
above-testnet-limit targets.

The compact-target checks follow the BSV node's [proof-of-work validation](https://github.com/bitcoin-sv/bitcoin-sv/blob/master/src/pow.cpp)
and [testnet consensus parameters](https://github.com/bitcoin-sv/bitcoin-sv/blob/master/src/chainparams.cpp).

## Offline states

- `header_inclusion_verified`: the transaction is included in the supplied
  header's Merkle root and that header satisfies its stated proof of work.
- `network_observation_unverified`: receipt records report network observation;
  an offline verifier cannot authenticate that report.
- `trusted_block_inclusion_verified`: the inclusion checks pass and the header
  hash matches the separate block hash supplied by the caller.

The verifier does **not** establish the best chain, chain ancestry, difficulty
adjustment, current confirmations, or reorganization status from a standalone
header. Receipt-supplied height and confirmation fields are consistency checks,
not authenticated chain facts. A private fork can have valid proof of work.
The trusted block option delegates chain membership to the caller's source;
it does not authenticate other metadata in the receipt.

## Commands

```sh
st8-anchor-worker verify --receipt contribution-receipt.json
st8-anchor-worker verify-compute --receipt compute-receipt.json
```

Both commands accept `--trusted-block-hash HASH`. Obtain HASH independently
from a trusted BSV testnet node or header service. Never copy it from the
untrusted receipt. An unmatched hash, malformed hash, or unmined receipt is
rejected when this option is supplied. Current confirmation/reorg policy
remains the caller's responsibility.

Old V1 receipts containing `mined_spv_verified` and `seen_on_testnet` remain
readable. Those historical labels are **not** returned as verification results;
the commands print the newly derived trust state. Newly observed receipts use
the new labels. Historical milestone JSON artifacts are left intact.

Regression tests verify both retained live receipts, coherent nonce/hash
substitution in both receipt types, malformed targets, PoW comparison boundaries,
legacy label compatibility, and independent block-hash pinning.

## Validation recorded 2026-10-13

The existing local worker binary verified both retained fixtures without any
wallet, broadcast, or network call:

```sh
./target/debug/st8-anchor-worker verify \
  --receipt docs/st8wrx/milestone-1/live-receipt.json
./target/debug/st8-anchor-worker verify-compute \
  --receipt docs/st8wrx/compute-m1/bsv-receipt.json
```

Both returned `header_inclusion_verified`. Contribution transaction:
`732609ae6ea8df7883a2229a5af9ab8b32316e7101c108a93c962668aa4538b9`.
Compute transaction:
`8fbc7c1091b45536c72039abb6f1e82510e77dc2371bc6ab5b823c60ed09ff6a`.

For each fixture, an in-memory adversarial copy flipped the nonce, recomputed
the full header hash, and replaced the matching block hashes in the header,
ARC status, and transaction observation. Passing that copy through
`--receipt /dev/stdin` returned a nonzero exit and
`block header does not satisfy proof-of-work target`. Supplying an independent
mismatched `--trusted-block-hash` (`11` repeated 32 times) also returned nonzero
for both originals. The persisted fixtures were unchanged.

These cases are retained in
`crates/st8-anchor-worker/src/verification_tests.rs`; the anchor suite passed
12 tests during implementation. This final binary check reused build output;
it did not rebuild, establish current chain membership, or test a live wallet.
