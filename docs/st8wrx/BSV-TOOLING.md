# Current BSV tooling decisions

Research date: 2026-08-23. Re-check versions before introducing a production
wallet, broadcaster, token, or smart contract.

## Current direction

- [BRC-100](https://bsv.brc.dev/wallet/0100) remains the application-to-wallet
  interface. ST8WRX should connect to a user's wallet rather than own general
  wallet keys.
- [BEEF (BRC-62)](https://bsv.brc.dev/transactions/0062) carries the transaction
  ancestry and Merkle paths needed for SPV validation.
- [Atomic BEEF (BRC-95)](https://bsv.brc.dev/transactions/0095) restricts that
  bundle to one subject transaction and its dependency graph. The V1 receipt
  retains Atomic BEEF when the wallet provides it.
- The current official TypeScript stack exposes `@bsv/sdk` and
  `@bsv/wallet-toolbox`; the [official package map](https://github.com/bsv-blockchain/ts-stack/blob/main/docs/packages/index.md)
  lists SDK primitives, BEEF/SPV, wallet tooling, and network packages.
- The official Rust SDK fork exists at
  [bsv-blockchain/rs-sdk](https://github.com/bsv-blockchain/rs-sdk), but it is
  still early and Open-BSV-licensed. It is intentionally not a dependency of the
  Apache-2.0 protocol crates.
- ARC remains the broadcaster boundary. A successful/known transaction response
  is normalized into `BroadcastReceipt`; provider-specific callbacks and status
  polling belong in an adapter, not the contribution protocol.
- sCrypt remains relevant for later escrow/licensing phases, not for the first
  data commitment. Contract work starts only when bounty escrow is required.

## Wallet contract

`prepared.json` contains `locking_script_hex`. A BRC-100 adapter should:

1. call `createAction` for a zero-satoshi data output using that exact script;
2. use `noSend: true` so wallet custody and signing stay external;
3. retain the returned Atomic BEEF;
4. extract the standard raw subject transaction;
5. broadcast through an ARC-compatible provider; and
6. create `external-result.json`:

```json
{
  "raw_transaction_hex": "...",
  "atomic_beef_hex": "...",
  "anchor_output_index": 0,
  "broadcast": {
    "accepted": true,
    "status": "SEEN_ON_NETWORK",
    "provider": "arc-provider-name"
  }
}
```

The finalize command recomputes the txid from raw bytes; callers do not supply a
trusted txid.

## Live-test requirement

A real testnet broadcast needs a reachable BRC-100 wallet (or another external
signer) with a funded testnet UTXO. ST8WRX never generates, logs, or accepts the
wallet private key. Without that external authority the code can prepare and
fully test the protocol, but it cannot honestly claim a live network broadcast.
