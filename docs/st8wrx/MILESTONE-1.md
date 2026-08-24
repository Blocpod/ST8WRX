# Milestone 1 live runbook

## Exit criteria

- [x] Real signed ST8WRX project, repository, and contribution evidence.
- [x] Deterministic contribution record and persistent project ledger.
- [x] Signed project policy, decision proposal, and two authorized approvals.
- [x] Accepted 1,000-CU award; Contribution Units remain non-transferable.
- [x] Deterministic contribution and project snapshots.
- [x] Asynchronous external BRC-100 wallet signing with no ST8WRX key custody.
- [x] Real BSV testnet transaction containing the exact project commitment.
- [x] Public transaction broadcast and mined testnet confirmation.
- [x] Persisted raw transaction, Atomic BEEF, mined BEEF/BUMP, block, and
  provider/network evidence.
- [x] Independent offline receipt verifier.
- [x] Adversarial project, approval, Merkle, transaction, txid, and BEEF
  substitution checks.

## Live result

- Contribution ID: `b6376bc04df9d0c4cc9bc81e506b32a004199e4f226f62515149a23f9fdc5ade`
- Project ID: `30621:56abd2fea6e0e00d0a9bcee7ec841f355275a87c89ea72b2d95f154bb2877a41:st8wrx-milestone-1`
- Contribution Units: `1000`
- Project snapshot: `f479b7f36b507115cc64cd1b0a5be3bf9fd845b8cd87bba8e6c86506365e3d66`
- Merkle root: `bb9eef492e23269efea4ead5ac4d3f2f759a2b82a87dad2100cee15483080af8`
- Testnet txid: `732609ae6ea8df7883a2229a5af9ab8b32316e7101c108a93c962668aa4538b9`
- Mined block: `1754486` / `0000000000c046761bce820232597af48e915d011a3e3cd4bf6bd78b06f00224`
- Verification state: `mined_spv_verified`
- Public receipt: [`milestone-1/live-receipt.json`](milestone-1/live-receipt.json)

The wallet charged only the 142-satoshi network fee. The commitment output is
zero satoshis. TAAL ARC rejected anonymous submission with HTTP 401, so the
worker used its independent fallback: WhatsOnChain's official small-scale
testnet broadcaster. The receipt preserves the failed ARC attempt, successful
public provider result, exact transaction, and independently observed state.
The wallet-free receipt refresh then persisted the mined BEEF/BUMP and block
material after confirmation.

## Independent verification

```bash
. ./bin/activate-hermit
cargo build -p st8-anchor-worker
target/debug/st8-anchor-worker verify \
  --receipt docs/st8wrx/milestone-1/live-receipt.json
```

Expected result:

```text
verified txid=732609ae6ea8df7883a2229a5af9ab8b32316e7101c108a93c962668aa4538b9 state=mined_spv_verified
```

The verifier performs no wallet call and does not trust the process that built
the receipt. It recomputes Nostr IDs/signatures, project scope, governance
threshold, CU award, snapshots, Merkle inclusion, BSV txid, output commitment,
Atomic BEEF subject, exact network-returned bytes, mined BEEF/BUMP root,
block-header hash, height, and confirmation.
