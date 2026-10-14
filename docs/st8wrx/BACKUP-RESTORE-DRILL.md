# Isolated PostgreSQL backup/restore drill

Executed on 2026-10-13 against local disposable ST8 test databases. This proves
database dump/restore for the records below; it does not establish recovery of a
complete deployed installation.

## Result

All three custom-format dumps restored successfully into **new** databases using
`pg_restore --exit-on-error --single-transaction --no-owner --no-privileges`.
Original databases were not modified, renamed, or dropped. Test writers were
paused during the comparisons. No relay, workflow worker, anchor worker, wallet,
or broadcaster was started against a restored database.

| Source database | Fresh destination database | Dump bytes | Result |
| --- | --- | ---: | --- |
| `st8_compute_replay_20261013` | `st8_restore_compute_replay_20261013_8bef9880` | 558,066 | PASS |
| `st8_workflow_readiness_20261013` | `st8_restore_workflow_readiness_20261013_de01e7d7` | 361,217 | PASS |
| `st8_anchor_recovery_20261013` | `st8_restore_anchor_recovery_20261013_076d0aca` | 319,800 | PASS, empty anchor fixture |

The comparison covered all **71 public tables per database**, including event
partitions: row counts and SHA-256 hashes of deterministically sorted complete
JSON rows matched. This includes original signed events, immutable receipts,
dispute resolutions, ledger projections, workflow traces, approval tokens and
timestamps. Parent-table counts include inherited partition rows; table counts
must not be summed to infer the number of unique records.

Sequence state, constraint definitions/validation flags, and non-internal trigger
definitions/enabled flags also matched. A second source fingerprint after each
restore matched its pre-dump fingerprint, detecting any intervening source
writes. Each database retained 37 successful migrations through version 37.
Read-only aggregate queries against restored databases matched their sources.

## Accounting and recovery reads

The compute database restored nine receipts with original cost totaling 18 sats:

| Dispute state | Receipts | Original sats | Settled receipts |
| --- | ---: | ---: | ---: |
| `disputed` | 2 | 4 | 0 |
| `resolved_rejected` (charge upheld) | 2 | 4 | 2 |
| `resolved_upheld` (charge voided) | 1 | 2 | 0 |
| `undisputed` | 4 | 8 | 4 |

Five settlements consumed six receipts totaling 12 sats. Five disputes included
three final resolutions. The voided receipt remained unsettled with its original
receipt bytes and cost preserved. Five compute anchor jobs remained queued with
zero attempts, no saved transaction bytes, and no wallet-request markers.

The workflow database restored 19 runs: four running, three waiting for approval,
four completed, and eight cancelled. Their execution traces contained 15 steps
in total. Its 15 approvals remained two pending, five granted, four denied, and
four expired, with complete persisted fields unchanged. These are SQL read
checks; this drill did not resume those workflows or execute their side effects.

## Populated wallet-fence follow-up

The initial empty-anchor fixture left populated recovery-state coverage open.
A separate follow-up on 2026-10-13 closed that database-storage gap without
modifying any of the three original databases:

| Source database | Fresh destination database | Dump bytes | Result |
| --- | --- | ---: | --- |
| `st8_wallet_fixture_20261013_cf5b88b8` | `st8_wallet_restored_20261013_cf5b88b8` | 469,699 | PASS |

The new source was initialized from the retained empty-anchor dump, then populated
under a new synthetic community with three jobs in **each** queue:

| State per queue | Wallet-request marker | Saved transaction / Atomic BEEF | Broadcast record | Network evidence / completed receipt | Claim eligibility after restore |
| --- | --- | --- | --- | --- | --- |
| Ambiguous failed attempt | Present | Absent | Absent | Absent | Excluded |
| Broadcast awaiting observation | Present | Present | Present | Absent | Eligible to reuse saved bytes |
| Confirmed | Present | Present | Present | Present | Excluded as complete |

All **71 public tables**, sequences, constraints and triggers matched before and
after restore using the complete-row fingerprint procedure above. A second source
fingerprint matched the original. This verifies exact marker timestamps, attempts,
transaction IDs, raw transaction bytes, Atomic BEEF, broadcast JSON, network
evidence and completed receipt JSON. Three contribution snapshots also retained
their 4,097-leaf counts.

Read-only SQL using the worker's claim predicate found one eligible saved-byte
job and zero eligible ambiguous jobs per queue. The existing
`st8-anchor-worker list-wallet-recoveries --database-url ...` binary listed exactly
one ambiguous job per queue in both databases, with identical fields. No worker
claim, wallet request, broadcast or transaction-recovery mutation was run.

**Synthetic boundary:** queue identifiers, prepared metadata, parent snapshot /
settlement records and state transitions were synthetic, schema-valid fixtures.
Saved transaction, Atomic BEEF, broadcast and complete receipt values were copied
unchanged from the retained public contribution and compute proof fixtures. Those
proof files passed offline verification separately. The synthetic queue rows do
not form newly engine-validated anchor material and must never be connected to an
active worker. This drill proves byte preservation and recovery visibility, not a
new mining event, wallet reconciliation, or end-to-end worker execution.

Protected evidence directory: `/tmp/st8wrx-wallet-restore-20261013/` (mode 0700;
files mode 0600). It retains `drill.py`, `fixture.sql`, `results.json`, and
`populated-wallet.dump`. Dump SHA-256:
`f22bcb78ec9da27a235d6d9d990ec0b5e48c7b0c3041c316f3be9721b211aa1e`.
Both new databases and all evidence were retained; no cleanup occurred.

## Retained local evidence

Protected local directory: `/tmp/st8wrx-restore-drill-20261013/` (mode 0700).
Files were created with umask 0077. It contains the three custom-format dumps,
`drill.py`, and `results.json` with client/server versions, timestamps, per-table
counts/checksums and critical read results. Dumps total **1,239,083 bytes**;
dump/restore/comparison took approximately 18.35 seconds. Destination databases
and evidence files were retained; no cleanup was performed.

| Dump | SHA-256 |
| --- | --- |
| `st8_compute_replay_20261013.dump` | `0b37b7de82d6c2264b460f9c37af805247caf2240cd46d575c0d52bcdd61a278` |
| `st8_workflow_readiness_20261013.dump` | `4b1f3bc65cfaa74752cd01eb4cdfdd03f53246fc4882c238e0298f3c4866452e` |
| `st8_anchor_recovery_20261013.dump` | `3b07ea27676be61f63e4218f238db59f1987c103f1b0792f5ec6eb5603f90b72` |

`/tmp` is temporary evidence storage, not a durable backup destination. These
test dumps contain signed source data and workflow approval material; do not
publish them or treat production equivalents as harmless build artifacts.

## Repeat the database step

Quiesce application writers and workers. Use a fresh destination name and a
protected backup directory; never point this procedure at an existing target.
The local drill used Unix socket `/tmp`, role `countergrind`, and the installed
Homebrew PostgreSQL tools. Adapt connection settings for the pilot host without
putting credentials in shell history.

```sh
umask 077
pg_dump --format=custom --no-owner --no-privileges \
  --file "$backup_file" "$source_db"
createdb --template=template0 "$new_restore_db"
pg_restore --exit-on-error --single-transaction --no-owner --no-privileges \
  --dbname "$new_restore_db" "$backup_file"
```

Repeat the row-count/content, sequence, constraint and trigger comparisons before
connecting application processes. For a table, this drill hashes the output of
`COPY (SELECT to_jsonb(t)::text FROM public.<table> t ORDER BY to_jsonb(t)::text)
TO STDOUT` with both sessions using UTC. Missing or changed data fails the drill;
matching aggregate totals alone are insufficient. Recheck the source fingerprint
to detect source changes during the backup window.

## Remaining full-installation boundary

Before pilot recovery is considered proven:

1. Capture one coherent backup set while writes and workers are stopped:
   PostgreSQL, git repository storage, media objects, relay configuration and
   signing keys, required access credentials, and external-wallet recovery
   access. Record component versions and storage locations. This drill used
   `--no-owner --no-privileges`; database roles, ownership and grants require
   their own reviewed restoration step.
2. Repeat the populated wallet-fence preservation checks on the pilot backup.
   The isolated synthetic follow-up above passes storage and claim-eligibility
   checks; it does not establish that the pilot wallet has no later external
   transactions. Keep wallet and broadcast endpoints disabled until the external
   wallet outcome has been reconciled.
3. Restore the set on a separate isolated installation. Verify repository refs
   and object integrity, media content checksums and reads, relay identity and
   tenant boundaries, messages, CU records, receipts, disputes and resolutions.
   SQL-only comparisons do not exercise git/media/key restore or native-client
   access.
4. Exercise application reads and restart recovery using the restored database.
   Reconcile any transaction broadcast after the backup snapshot with the
   external wallet/observer before re-enabling workers: a database restored to
   an earlier point can predate a wallet-request marker. Do not assume an absent
   marker means no spend occurred.

No production backup, production restoration, wallet call, media/git restore,
key recovery, or cross-host application recovery was performed in this drill.
See [private-beta readiness](PRIVATE-BETA.md) and
[anchor recovery](ANCHOR-RECOVERY.md) for the remaining operational workflow.
