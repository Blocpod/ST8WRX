# Ambiguous external-wallet recovery

Before a contribution or compute anchor invokes the external wallet, it commits
`wallet_requested_at` under the current lease. A second automatic wallet call
for that job is refused. This matters because synchronous wallet processing may
have broadcast a transaction even when the caller receives a timeout or crashes
before persisting its bytes. A deterministic wallet reference is not proof that
a provider implements idempotency.

After an ambiguous attempt, other queued jobs continue. The affected job is
excluded from automatic claims until its exact transaction is recovered. Normal
retries with persisted transaction bytes continue to rebroadcast/observe those
same bytes without invoking the wallet again.

## Operator procedure

Stop the affected worker before investigating. Wait for the external wallet
request to finish or establish its outcome with the wallet operator. Expired
worker leases alone do not establish that the wallet stopped processing.

```sh
st8-anchor-worker list-wallet-recoveries
```

The command lists inactive ambiguous jobs (up to 1,000 per queue). Each entry
includes `community_id`, `anchor_id`, project, request time, and last error. Jobs
with an active lease are omitted. The deterministic wallet reference is
`st8wrx:<snapshot-id>` for contributions and `st8wrx-compute:<settlement-id>` for
compute settlements.

Obtain the original signed transaction from the external wallet. Export only
its public `SignedAnchorTransaction` JSON (`txid`, `raw_transaction`, and
`atomic_beef` byte arrays, plus `anchor_output_index`). No keys or credentials
belong in this file. Then run:

```sh
st8-anchor-worker recover-wallet-transaction \
  --queue contribution \
  --community-id COMMUNITY_UUID \
  --anchor-id SNAPSHOT_HEX \
  --transaction recovered-transaction.json
```

Use `--queue compute` and the settlement ID for compute jobs. The command verifies
the stored prepared anchor, recomputes the transaction ID, parses Atomic BEEF,
and checks the exact commitment output before updating the inactive job. It
makes no wallet or broadcast call. A conflicting transaction or active lease
is rejected; a successfully recovered job resumes ordinary observation.

If the wallet confirms no transaction was created, the operator must explicitly
arrange the single intended transaction with the external wallet and import its
public bytes. There is intentionally no command to clear the fence and blindly
repeat an uncertain spend.

## Upgrade and remaining guarantees

Stop old workers before applying migration 0037 and deploy only updated workers.
The migration quarantines historical attempts that have no saved transaction,
because those attempts may already have spent wallet funds. New jobs that have
never been attempted remain eligible. Existing saved transactions remain valid.

This prevents automatic duplicate funding across the wallet-call/DB-write crash
window. It does not discover external wallet transactions by itself or prove
that a wallet provider is honest. The operator reconciles ambiguous requests;
confirmed-chain trust remains described in [proof verification](PROOF-VERIFICATION.md).
