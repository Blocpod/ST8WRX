# Milestone 1 live runbook

## Exit criteria

- [x] Real signed ST8WRX project, repository, and contribution evidence.
- [x] Deterministic contribution record and persistent project ledger.
- [x] Signed project policy, decision proposal, and two authorized approvals.
- [x] Accepted 1,000-CU award; Contribution Units remain non-transferable.
- [x] Deterministic contribution and project snapshots.
- [x] Asynchronous external BRC-100 wallet signing with no ST8WRX key custody.
- [x] Real BSV testnet transaction containing the exact project commitment.
- [x] Public transaction broadcast and mined confirmation.
- [x] Persisted raw transaction, Atomic BEEF, mined BEEF/BUMP, block, and
  provider evidence.
- [x] Independent offline receipt verifier.
- [x] Adversarial project, approval, Merkle, transaction, txid, and BEEF
  substitution checks.

## Live result

- Contribution ID: `18f2ef177e93c5e51035de80b50b93e11a482fad8dca92c4b5692f3ebf629e9d`
- Project ID: `30621:56abd2fea6e0e00d0a9bcee7ec841f355275a87c89ea72b2d95f154bb2877a41:st8wrx-milestone-1`
- Contribution Units: `1000`
- Project snapshot: `e91b26a0532985937300f2b17ab0ff9f320a5560959e9e733944b8db997f929f`
- Merkle root: `b916db8a3f566df63afdd981eb3031503f6205e9cf3222aa5e70c8deca584b2c`
- Testnet txid: `37ad82e23016d2faf45d9ca464025c9ca99c7e6ddf0e09de796e544c67970fff`
- Mined block: `1754464` / `0000000000839d107533ff09b83d34c43f61c042744d99e7cf0da3d48996b6f6`
- Verification state: `mined_spv_verified`
- Public receipt: [`milestone-1/live-receipt.json`](milestone-1/live-receipt.json)

The wallet charged only the 142-satoshi network fee. The commitment output is
zero satoshis. TAAL ARC rejected anonymous submission with HTTP 401, so the
worker used its independent fallback: WhatsOnChain's official small-scale
testnet broadcaster. The receipt preserves the failed ARC attempt, successful
public provider result, exact transaction, and mined proof.

## Independent verification

```bash
. ./bin/activate-hermit
cargo build -p st8-anchor-worker
target/debug/st8-anchor-worker verify \
  --receipt docs/st8wrx/milestone-1/live-receipt.json
```

Expected result:

```text
verified txid=37ad82e23016d2faf45d9ca464025c9ca99c7e6ddf0e09de796e544c67970fff state=mined_spv_verified
```

The verifier performs no wallet call and does not trust the process that built
the receipt. It recomputes Nostr IDs/signatures, project scope, governance
threshold, CU award, snapshots, Merkle inclusion, BSV txid, output commitment,
Atomic BEEF subject, mined BEEF/BUMP root, block-header hash, and confirmation.
