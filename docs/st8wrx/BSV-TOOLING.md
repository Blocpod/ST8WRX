# Current BSV tooling decisions

Validated against the live Milestone 1 testnet anchor on 2026-08-24.

## Standards and authority boundaries

- [BRC-100](https://bsv.brc.dev/wallet/0100) is the application-to-wallet
  boundary. ST8WRX requests signing; the wallet retains custody and authority.
- [BEEF (BRC-62)](https://bsv.brc.dev/transactions/0062) carries transaction
  ancestry and mined Merkle paths.
- [Atomic BEEF (BRC-95)](https://bsv.brc.dev/transactions/0095) binds one subject
  transaction and its dependency graph. The wallet-returned Atomic BEEF is
  persisted before broadcast.
- BSV keys are separate from Buzz/Nostr contributor and governance identities.
- Contribution Units are internal, non-transferable accounting units, not BSV
  tokens, legal equity, or securities.

## Worker contract

`st8-anchor-worker` verifies a queued `PreparedContributionAnchor`, checks that
the external wallet reports `testnet`, and calls `createAction` with:

- one zero-satoshi output containing the exact ST8WRX locking script;
- synchronous wallet processing so selected inputs are durably retired before
  the call returns;
- deterministic output ordering for verifiable commitment location; and
- wallet-managed input selection, signing, change, and fee payment.

The wallet broadcasts without exposing keys. The worker immediately verifies
the exact commitment and persists txid, raw bytes, and Atomic BEEF before its
independent provider submission/observation. A retry resumes those exact bytes
and cannot create a second wallet spend.

## Broadcast and proof providers

ARC remains the preferred lifecycle API. TAAL's public testnet ARC currently
requires authorization and returned HTTP 401 during the live run. The worker
therefore falls back to WhatsOnChain's documented small-scale
`POST /v1/bsv/test/tx/raw` endpoint and preserves both provider outcomes.

After broadcast, completion requires independent WhatsOnChain reads of:

- the exact raw transaction;
- decoded transaction details and current testnet state.

When the transaction is mined, the worker additionally persists mined BEEF/BUMP
and block fields used to recompute the Merkle root and 80-byte header hash.

The receipt verifier is offline: it consumes persisted public proof material,
not a wallet, API credential, or trusted database connection.

## Live artifact

[`milestone-1/live-receipt.json`](milestone-1/live-receipt.json) contains only
public transaction/evidence/proof material. It contains no wallet credential,
private key, WIF, mnemonic, seed phrase, or `nsec`.
