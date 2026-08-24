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
- Contribution: `b6376bc04df9d0c4cc9bc81e506b32a004199e4f226f62515149a23f9fdc5ade`
- Contribution Units: `1000`
- Approver 1: `nostr:56abd2fea6e0e00d0a9bcee7ec841f355275a87c89ea72b2d95f154bb2877a41`
- Approver 2: `nostr:d0f3662fae720bd692fbd0d17bb349bfd34e2da427b5d2b4b5c200e606996c06`
- Contribution snapshot: `df3a494c86739f3ef3a319f1b772432f3683fd014cb7bfebc11ea3609ec5f72`
- Project snapshot: `f479b7f36b507115cc64cd1b0a5be3bf9fd845b8cd87bba8e6c86506365e3d66`
- Merkle root: `bb9eef492e23269efea4ead5ac4d3f2f759a2b82a87dad2100cee15483080af8`
- BSV testnet txid: `732609ae6ea8df7883a2229a5af9ab8b32316e7101c108a93c962668aa4538b9`
- Mined block: `1754486` / `0000000000c046761bce820232597af48e915d011a3e3cd4bf6bd78b06f00224`
- Verification: `mined_spv_verified`

## Verify

```bash
target/debug/st8-anchor-worker verify \
  --receipt docs/st8wrx/milestone-1/live-receipt.json
```

An existing public receipt can be upgraded after confirmation without wallet
access:

```bash
target/debug/st8-anchor-worker refresh-receipt \
  --receipt docs/st8wrx/milestone-1/live-receipt.json
```

The verifier rejects altered project scope, forged approvals, changed CU or
snapshot state, invalid Merkle siblings/root, changed raw transaction bytes,
txid substitution, altered commitment payload, and inconsistent network state.
