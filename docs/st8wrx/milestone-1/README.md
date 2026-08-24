# Milestone 1 live contribution receipt

This directory contains both the historical deterministic proposal fixture and
the completed live Milestone 1 receipt. The fixture files (`proposal.json`,
`intent.json`, and `prepared.json`) remain useful test vectors but use ephemeral
public identities and are not the live milestone proof.

The authoritative artifact is [`live-receipt.json`](live-receipt.json). It was
created from real signed ST8WRX runtime activity and real persistent project and
governance identities.

## Live identifiers

- Project: `30621:56abd2fea6e0e00d0a9bcee7ec841f355275a87c89ea72b2d95f154bb2877a41:st8wrx-milestone-1`
- Contributor: `nostr:b9935e323037bbbe9e851a9e5b146f34b586a347d7151ba4801d84b4c10f49d1`
- Contribution: `18f2ef177e93c5e51035de80b50b93e11a482fad8dca92c4b5692f3ebf629e9d`
- Contribution Units: `1000`
- Approver 1: `nostr:56abd2fea6e0e00d0a9bcee7ec841f355275a87c89ea72b2d95f154bb2877a41`
- Approver 2: `nostr:d0f3662fae720bd692fbd0d17bb349bfd34e2da427b5d2b4b5c200e606996c06`
- Contribution snapshot: `9eca155f7430abe34cd149c2519215fe3092c7feab5c1da863de6f72f6e63943`
- Project snapshot: `e91b26a0532985937300f2b17ab0ff9f320a5560959e9e733944b8db997f929f`
- Merkle root: `b916db8a3f566df63afdd981eb3031503f6205e9cf3222aa5e70c8deca584b2c`
- BSV testnet txid: `37ad82e23016d2faf45d9ca464025c9ca99c7e6ddf0e09de796e544c67970fff`
- Block height: `1754464`
- Verification: `mined_spv_verified`

## Verify

```bash
target/debug/st8-anchor-worker verify \
  --receipt docs/st8wrx/milestone-1/live-receipt.json
```

The verifier rejects altered project scope, forged approvals, changed CU or
snapshot state, invalid Merkle siblings/root, changed raw transaction bytes,
txid substitution, altered commitment payload, and forged mined BEEF/BUMP.
