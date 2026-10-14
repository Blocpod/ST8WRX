# ST8WRX private-beta readiness

Status as of 2026-10-13: the implementation pass covers desktop contribution
workflows, recoverable accounting, and testnet proof handling. An isolated live
relay pilot now verifies signed contribution accounting over HTTP and WebSocket
and preserves its ledger record across restart. The installed desktop and live
wallet have not yet been exercised together. The original
[readiness audit](READINESS-AUDIT-2026-10-12.md) is a historical baseline.

## What is implemented

| Surface | Current behavior |
| --- | --- |
| Desktop project economy | Open **Contributions & compute** on a project. Create an owner policy, submit evidence-backed claims, propose decisions, approve as an authorized founder, and inspect ledger records. |
| Desktop compute accounting | Inspect receipts and settlement evidence; requester/owner can dispute unsettled charges; project owner can uphold or void a charge with an immutable resolution. |
| Native trust boundary | Validates signed public project evidence, policy authority, exact claim/proposal dependencies, and active community/signer before signing. Invalid proposals are not actionable. |
| Compute persistence | Replays retained inputs in bounded pages and dependency passes. Duplicate ingestion and settlement are idempotent; one invalid input does not discard unrelated accounting. |
| Local compute execution | CLI saves private answers, limits output and request duration, journals exact signed events, and resumes publication without rerunning inference. |
| Workflow execution | Working scoped DMs, topic updates, and reactions; durable approval gates recover grant, denial, expiry, and notification failures after restart. |
| Testnet anchors | Separate supervised contribution/compute queues, durable transaction recovery, proof-of-work/header checks, and explicit offline verification boundaries. |
| Wallet ambiguity | A persisted intent prevents an automatic second wallet request after an uncertain outcome. Operator recovery imports the original transaction. |
| Scale and quality gates | Removed the arbitrary 4,096-leaf ceiling, widened stored leaf counts, and added ST8, workflow, admin, supervisor, and persistence checks to CI. |

The desktop shows bounded recent history, not a complete aggregate balance.
Older proposal dependencies may require the CLI. Claims require evidence authored
by the contributor; a project creator can use their signed project event for an
initial claim. Contribution data is public to the permitted project audience;
private compute answer contents stay in the local output file.

## Pilot walkthrough

1. Start an isolated community using the [deployment instructions](../../deploy/compose/README.md).
   Configure owner identity, authentication, TLS, and persistent storage. Build
   from this checkout; do not assume the example image tag has been published.
2. Connect the native desktop app with the intended beta identities. Create a
   project and open **Contributions & compute**. As owner, define the founder
   policy. As contributor, submit authored public evidence. Propose a decision,
   collect the required founder approvals, and inspect the resulting CU record.
3. Run local compute on a dedicated idle Mesh node using the
   [runtime instructions](../../crates/st8-compute-runtime/README.md). Check the
   private answer, published receipt, and desktop accounting view. Exercise a
   dispute and owner resolution before settlement.
4. Configure an external testnet wallet and explicitly enable the `st8-anchor`
   Compose profile. This makes wallet/broadcast calls. Inspect both queue logs
   and retain the resulting public receipts. Verify them using the
   [proof verification instructions](PROOF-VERIFICATION.md).
5. Restart relay/workers and repeat reads. Simulate an interrupted wallet request
   only with testnet funds and follow [anchor recovery](ANCHOR-RECOVERY.md).

Before upgrading an existing installation, stop old anchor workers, back up its
data, apply migrations 0036/0037, and then start only updated binaries. Migration
0037 quarantines previously attempted anchors without saved transaction bytes;
reconcile these with the external wallet instead of assuming they never spent.

## Validation performed during this implementation

- Desktop economy browser scenarios: claim → proposal → founder approval → CU
  record, and compute dispute → owner void → zero billable charge. Both passed
  against the mock native bridge after an E2E frontend build.
- Full desktop JavaScript unit suite: 5,401 passed, zero failures or skips.
- Native economy tests: four passed; native all-target compilation and strict
  Clippy passed. CLI tests: 371 passed.
- Real disposable PostgreSQL databases: four compute replay/resolution tests,
  six workflow persistence/recovery tests, and one anchor recovery test passed.
  These exercise embedded migrations, tenant boundaries, retries, and conflicts.
- Workflow unit tests: 170 passed; relay workflow tests: 22 passed.
- Compute engine unit tests: 13 passed; database unit tests: 105 passed.
- Runtime tests: 13 passed; anchor worker tests: 12 passed. Contribution/proof
  boundary checks include 4,097 entries and larger Merkle batches.
- Admin dashboard build and ten browser tests passed. Supervisor tests: three
  passed. Compose configuration and CI YAML parsed successfully.
- Workspace Rust formatting, desktop lint/typography/pubkey checks, web checks,
  and the differential file-size gate passed.

Follow-up verification:

- Three isolated PostgreSQL databases were dumped and restored into new
  databases. All 71 tables and their data fingerprints, constraints, triggers,
  and sequences matched; source data remained unchanged. Populated compute and
  workflow records survived. The anchor test database had no retained anchor
  rows; a follow-up isolated fixture drill restored populated ambiguous,
  broadcast, and confirmed jobs in both queues. Saved transaction/BEEF bytes,
  receipts, and wallet-request markers matched, and uncertain jobs remained
  excluded from automatic claims. Queue metadata was synthetic, with saved
  public transaction bytes from retained proof fixtures.
  See [restore drill evidence](BACKUP-RESTORE-DRILL.md).
- Existing binaries passed another 26 compute runtime/engine tests. Saved
  contribution and compute proof receipts passed offline inclusion verification;
  coherent header tampering and wrong independent hash pins were rejected.
- Mobile formatting and analysis passed. The initial full test run exhausted
  disk space; after approved cache relief, a reduced-concurrency rerun passed
  all 1,661 mobile tests. The native desktop suite rerun passed 2,756 tests,
  with zero failures and 18 explicitly ignored tests. It used the retained
  current test executable with two test threads. The earlier focused native
  results remain valid.
- Docker was initially unavailable and has now been started for an isolated
  live-relay check. Standalone executables that predate these changes were not
  used to claim a current deployed pilot.

Review found a contribution approval filtering defect: a malformed or
unauthorized approval could poison the candidate set, and the latest-256 query
could exclude valid approvals. The correction now verifies candidates
individually, deduplicates only valid signers, and pages through retained
approvals. Both PostgreSQL regressions passed after disk relief, including 300
expired founder approvals ahead of the valid threshold. A dedicated CI gate and
independent source review cover the correction.
Periodic recovery of stored approvals already existed; the review did not find
an absent startup/reconciliation loop.

Strict relay Clippy passed after the approval correction, and the current relay
executable was rebuilt successfully for the isolated live pilot.

The live pilot passed project → policy → claim → proposal → two founder
approvals using temporary identities and a fresh database. One founder alone
produced no award; two founders produced one 7-CU entry. A duplicate final
approval retained exactly one ledger entry, snapshot, anchor job, and two
approval records. Authenticated HTTP and NIP-42 WebSocket reads returned the
same signed ledger event after relay restart. Isolated MinIO passed the relay's
object-store conformance probe. No anchor worker or wallet call was used.
See the [live pilot evidence and repeatable commands](LIVE-CONTRIBUTION-PILOT.md).
Temporary relay, Redis, and MinIO services were stopped; test artifacts and
isolated databases were retained for inspection.

The live relay pilot, mock UI coverage, and isolated database tests do not
establish that the installed desktop, deployed relay, external wallet, and chain
observer work together on the intended pilot host. This pass did not run the
entire `just ci`, build a container image,
publish a release, or make new wallet/broadcast calls. CI still must pass before
merging or releasing. See [TESTING.md](../../TESTING.md) for commands.

## Remaining launch requirements

- Exercise the walkthrough above with the actual beta desktop build and
  configured community. Record image/commit, identities, testnet transaction IDs,
  restart behavior, and any failures.
- Prove backup and restore on a separate installation: preserve a consistent
  PostgreSQL dump, git repositories, media objects, relay configuration/keys,
  and external-wallet recovery access. Quiesce writes for a coherent backup;
  restore into isolated storage with anchor workers disabled, then verify
  messages, repository contents, media, CU records, disputes, and receipts before
  enabling any wallet operations. A full restore drill was not performed here.
- Choose the private distribution route. Public signed installers, notarization,
  updater credentials, and production image publishing remain in
  [RELEASING.md](../../RELEASING.md).
- Establish an operator for failed queues, ambiguous wallet requests, and disk
  capacity. Retain public receipts and recovery journals independently of build
  caches.

Paid provider payouts, remote compute delivery, mobile/web economy parity, and a
self-service marketplace are outside this beta. Offline header inclusion does
not establish current chain membership or confirmation depth. Native inference
can continue after client cancellation, and arbitrary workflow side effects do
not have a general exactly-once guarantee across crashes.

## Local disk cleanup

The approved idle `desktop/src-tauri/target` cache was removed with Cargo. Cargo
reported 11.8 GiB of logical files; roughly 8 GiB of physical space became
available. Native verification subsequently reused and grew the root `target`
directory, so that initial recovery is not the current free-space figure. After
native verification, the shared `target` measured about 40 GiB and the drive had
about 2 GiB free. After verification exhausted disk space, the operator approved
removing only `target/debug/incremental` (about 8 GiB). That cache was removed
after confirming no Rust builds were active; free space increased from 2.3 GiB
to 9.2 GiB. Compiled binaries and the rest of `target` were retained. Broad
cleanup remains deferred until verification is complete.
Source, keys, databases, receipts, and project history were preserved.

Rust `target` directories are reproducible build output, not required project
records. Keep a shared build directory during active work. Once all builds stop,
remove unneeded caches using the commands in [TESTING.md](../../TESTING.md);
expect a slower next build. Do not classify databases, private outputs, wallet
recovery journals, or uncommitted checkouts as disposable debugging files.
