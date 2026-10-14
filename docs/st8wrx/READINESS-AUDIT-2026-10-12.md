# ST8WRX readiness audit — 12 October 2026

What this repo does: ST8WRX combines a signed collaboration workspace with project contribution accounting, shared AI compute, and BSV provenance. The inherited collaboration platform is substantial; the ST8 economy is an implemented protocol/CLI milestone with important product and reliability gaps.

**Verdict: suitable for developer-operated demonstrations, but not yet a complete, self-service production venture network.**

Audited checkout: `89690c0` (`ci: reset incomplete sherpa native caches`). This was a repository-wide, risk-based review across approximately 3,002 Rust, TypeScript, TSX, and Dart source files, with deeper tracing of contribution, compute, persistence, verification, workflow, deployment, and CI paths. It was not a line-by-line review of every file. No implementation was changed.

Assumed first release: a small multi-user community with humans and agents, concurrent compute requests, service restarts, contribution approval, and reliable accounting. The vision's 10,000 humans / 50,000 agents is a later scale target; this audit does not certify it. “Complete” below distinguishes a usable private beta from the full paid-compute and marketplace vision.

## What already exists

| Area | Evidence in the current checkout | Readiness boundary |
|---|---|---|
| Collaboration | Relay, community isolation, Nostr authentication, channels, DMs, search, Git hosting, media, moderation, voice, desktop and mobile implementations | Broad implementation; live end-to-end deployment was not exercised in this audit |
| Agents | ACP harness, personas, activity UI, local tools, Kubernetes provider | Implemented; actual remote substrate and model-provider credentials were not exercised |
| Contributions | Signed evidence, governance policy and approvals, CU ledger, project snapshots, CLI, asynchronous anchor queue | Primarily an operator/CLI workflow; growth and recovery need attention |
| Compute accounting | Signed jobs, provider/node attestations, deterministic pricing, receipts, disputes, settlement records, CLI queries | Local private inference milestone; not yet a general paid distributed compute service |
| BSV provenance | External-wallet boundary, persisted transaction material, testnet proof artifacts, offline verification | Testnet anchoring; independent chain authentication and real payouts remain incomplete |
| Operations | Compose, Helm, health checks, metrics, migrations, validation workflows | No repository publishing workflow; no bundled ST8 anchor-worker service |

Do not rebuild rate limiting, project grouping, push infrastructure, custom emoji, or the Kubernetes provider just because an older vision/status page calls them missing. Current source contains these components. Presence of a component does not establish a working ST8WRX production deployment.

## Must fix

### 1. Offline “mined” verification trusts a self-supplied block header

**What this is:** The receipt verifier checks transaction bytes, signatures, a Merkle inclusion path, and a block-header hash. See [st8-anchor-worker verifier](../../crates/st8-anchor-worker/src/lib.rs#L748).

**Problem:** It compares the reconstructed header against hashes supplied inside the same receipt, and accepts positive confirmation counts from that receipt. It does not establish that the header belongs to the accepted BSV chain. A freshly rebuilt verifier accepted a copy of the retained contribution receipt after its block nonce and three corresponding header hashes were altered. The altered header fails its own declared proof-of-work target, yet verification still returned `mined_spv_verified`.

**Fix:** Verify against a trusted header source or validated header chain/checkpoint, with an explicit reorganization/confirmation policy. Until then, label the offline result as internally consistent inclusion evidence relative to a supplied header, rather than independently authenticated chain confirmation.

**If we skip it:** A forged header can receive the strong `mined_spv_verified` label. This does not forge the contributor's signatures, but it defeats the claimed independent confirmation of blockchain inclusion.

### 2. Accepted compute events can permanently miss the accounting ledger

**What this is:** Event storage and compute-ledger updates happen in separate stages. See [ingestion](../../crates/buzz-relay/src/handlers/ingest.rs#L3246), [compute handlers](../../crates/buzz-relay/src/handlers/compute.rs#L102), and [reconciliation](../../crates/buzz-relay/src/handlers/compute.rs#L483).

**Problem:** A database error or crash after event storage can prevent a job, receipt, dispute, or settlement from being projected. The relay logs side-effect errors but accepts the event; retransmitting it returns “duplicate” before retrying the side effect. Compute reconciliation only republishes already-existing ledger rows, so it cannot restore missing rows. Contribution approvals have a separate replay mechanism; compute does not.

**Fix:** Add durable, idempotent replay of stored compute inputs, or atomically queue the ledger update with event storage. Test failure after event insertion, duplicate resubmission, missing dependencies, and restart recovery.

**If we skip it:** A user can receive success while a balance, dispute freeze, or settlement never takes effect.

### 3. Compute jobs need bounded execution and durable failure recovery

**What this is:** `st8-compute run` publishes authorization, invokes inference, meters usage, then publishes a result and receipt. See [runtime](../../crates/st8-compute-runtime/src/main.rs#L457).

**Problem:** The inference request has no explicit timeout or output-token budget. Transport errors, malformed responses, metering mismatches, and an exceeded signed cost cap return before publishing a terminal receipt. Cost enforcement occurs after execution; even a cap below the fixed fee does not prevent inference from starting. Publication can also stop between the two signed input events or between result and receipt.

**Fix:** Validate the minimum affordable request before dispatch; bound output and runtime; persist job state and publication progress; provide idempotent resume and a terminal failure/cancellation path. Keep the rule rejecting over-budget charges.

**If we skip it:** Resources can be consumed without usable accounting, a request can hang, and interrupted jobs can remain authorized indefinitely.

### 4. The compute command discards the useful model answer

**What this is:** The compute runtime receives the model's private answer. See [finalization](../../crates/st8-compute-runtime/src/main.rs#L607) and [command output](../../crates/st8-compute-runtime/src/main.rs#L337).

**Problem:** It hashes the answer, writes a public accounting artifact, and prints identifiers and cost. It exposes no private answer output to the requester. A normal successful invocation therefore produces a receipt without delivering the requested work.

**Fix:** Return the answer through a private response channel or an explicitly requested local output file. Keep prompts and answers out of public events and receipts.

**If we skip it:** The path demonstrates metering but cannot serve as a useful inference command for a user or agent.

### 5. Workflow approvals and several actions are unfinished

**What this is:** The workflow UI and schema expose approval, DM, topic, and reaction steps. See [approval finalization](../../crates/buzz-workflow/src/lib.rs#L229) and [action dispatch](../../crates/buzz-workflow/src/executor.rs#L643).

**Problem:** Approval steps mark the run failed instead of persisting a suspended state. DM and topic actions return `NotImplemented`. The reaction action calls `/api/messages/{id}/reactions`, for which the current relay has no matching route ([reaction helper](../../crates/buzz-workflow/src/executor.rs#L969)).

**Fix:** Persist and resume approvals with authorized approve/reject/expiry handling. Implement actions through the existing signed-event/action-sink path; test actual relay delivery and restart behavior.

**If we skip it:** Users can configure workflows that fail precisely when they need human approval or a promised action. Explicit signed contribution governance is separate and should not be confused with this broken workflow gate.

### 6. Compute disputes freeze money-related records without a resolution path

**What this is:** An authorized requester or project owner can dispute an unsettled receipt. See [dispute persistence](../../crates/buzz-db/src/compute.rs#L476).

**Problem:** The implemented transition changes the receipt to `disputed`, and settlements exclude it. There is no corresponding signed resolution, withdrawal, adjustment, or unfreeze operation in the compute protocol/runtime.

**Fix:** Add an authorized, audited resolution with explicit outcomes and tests against settlement races and duplicate decisions.

**If we skip it:** Even an accidental or successfully resolved off-platform dispute leaves the receipt stranded.

### 7. Contribution accounting stops growing after 4,096 accepted entries per project

**What this is:** Every new accepted contribution rebuilds a snapshot of the project's entire accepted history. See [snapshot assembly](../../crates/buzz-relay/src/handlers/contribution.rs#L177).

**Problem:** That history becomes a single Merkle batch, whose [hard cap is 4,096](../../crates/st8-bsv-provenance/src/lib.rs#L18). The 4,097th accepted contribution cannot complete ledger insertion. Repeated whole-history snapshots also increase storage and processing with every award.

**Fix:** Use bounded snapshot epochs/batches with explicit links to prior committed state, preserving complete independently verifiable history. Add a test crossing 4,096 accepted contributions.

**If we skip it:** A successful long-lived project eventually loses the ability to record new accepted awards. This is less urgent for a small pilot but blocks the intended long-term product.

### 8. Standard CI does not execute the ST8 crate test suites

**What this is:** CI's Rust unit job invokes `just test-unit`. See [CI job](../../.github/workflows/ci.yml#L102) and [explicit package list](../../Justfile#L307).

**Problem:** The nextest path and fallback script enumerate Buzz packages but omit the seven ST8 crates. Workspace membership provides compilation/lint coverage, not execution of their tests. The new contribution/compute database and relay paths also lack dedicated automated lifecycle/race tests in the inspected source. `admin-web` is omitted from the normal CI path filters/jobs; its unit command reports success with no test files.

**Fix:** Run all seven ST8 test suites in both unit-test paths, verify the retained receipts, and add an isolated database/relay gate for projection recovery, settlement/dispute races, tenant isolation, and anchor retry behavior. Add admin build/browser checks to CI.

**If we skip it:** Financial/provenance regressions can ship with green routine checks. Passing pure protocol tests does not prove the database lifecycle.

### 9. The ST8 economy has no end-user client workflow

**What this is:** Contributions and compute accounting have CLI commands and event kinds, while desktop/mobile/web primarily expose the collaboration product. See [contribution CLI](../../crates/buzz-cli/src/commands/contributions.rs#L23), [compute CLI](../../crates/buzz-cli/src/commands/compute.rs#L17), and [desktop event constants](../../desktop/src/shared/constants/kinds.ts).

**Problem:** Source searches and client feature inspection found no ST8 claim submission, founder approval queue, CU ledger, provider earnings, dispute-resolution, or settlement/anchor receipt UI. Desktop “Share compute” is not a complete paid-compute interface; the browser surface is primarily repository browsing and invitations.

**Fix:** Build one complete project-economy flow in desktop first: evidence → claim → signed governance → CU result → proof status, plus compute cost/receipt/dispute views. Then explicitly select mobile/web parity requirements.

**If we skip it:** Ordinary users need CLI commands and hand-managed files to access the features that distinguish ST8WRX from its collaboration baseline.

### 10. Compute settlement is an accounting commitment, not provider payment

**What this is:** Settlement records sum satoshis and queue a BSV anchor. See [settlement persistence](../../crates/buzz-db/src/compute.rs#L535) and [wallet outputs](../../crates/st8-anchor-worker/src/lib.rs#L237).

**Problem:** The wallet is explicitly restricted to testnet and is asked to create a zero-satoshi commitment output. There is no provider payout output or payment lifecycle. Marking a receipt settled/confirmed therefore establishes accounting/anchor state, not that the provider received funds.

**Fix:** Before offering paid compute, implement funding authorization, payee identity/address binding, actual payment outputs, idempotent payment reconciliation, failure handling, and distinct accrued/anchored/paid states. Configure and validate the intended production network separately.

**If we skip it:** Providers accumulate recorded earnings without receiving payment. This can remain explicitly testnet-only for a private beta, but the paid economy is incomplete.

### 11. Billable compute is limited to isolated local inference

**What this is:** Buzz Mesh supports shared compute, but the ST8 billing runtime uses a narrower meter. See [local-provider validation](../../crates/st8-compute-runtime/src/lib.rs#L58) and [counter attribution](../../crates/st8-compute-runtime/src/lib.rs#L151).

**Problem:** Billing requires a private local node, no remote peers/proxying, and exactly one attributable attempt/success between counter snapshots. Two concurrent jobs can invalidate attribution; remote execution is deliberately rejected. The runner also loads both requester and provider signing identities in one process.

**Fix:** For the pilot, explicitly serialize supported local jobs. For the full product, separate requester/provider execution, add authenticated dispatch and per-job metering, and integrate receipts with ordinary agent/Mesh usage. Keep privacy restrictions until remote execution has an explicit consent and trust model.

**If we skip it:** A shared multi-provider paid compute network cannot be delivered by the current milestone, despite having registry and accounting structures.

### 12. A normal deployment does not run the ST8 anchor worker

**What this is:** The worker offers `run-once` and `run-compute-once`. See [worker commands](../../crates/st8-anchor-worker/src/main.rs#L25), [container binaries](../../Dockerfile#L70), and [Compose services](../../deploy/compose/compose.yml).

**Problem:** The main image builds/copies relay, admin, and pairing binaries, not the ST8 worker/runtime. The bundled deployment contains no service or scheduled job invoking the worker. Starting the standard stack alone therefore leaves ST8 anchors queued.

**Fix:** Package the worker, supervise or schedule both queues, configure the external wallet/provider boundary, and alert on queue age, repeated errors, expired leases, and confirmation delays. Test interruption and restart around signing, persistence, and broadcast.

**If we skip it:** Anchoring continues to depend on manual operator commands.

### 13. Independent release and upgrade delivery is unfinished

**What this is:** ST8WRX deliberately removed inherited publication workflows. See [release status](../../RELEASING.md#L1) and [empty updater endpoints](../../desktop/src-tauri/tauri.conf.json#L43).

**Problem:** The repository has validation workflows but no production artifact publication workflow or configured desktop update feed. Compose names an ST8WRX image destination, but that name alone does not demonstrate a published, tested artifact.

**Fix:** Establish ST8WRX-owned build/signing destinations for the platforms being shipped, versioned relay/worker images, a desktop update feed, and migration/rollback procedures. Prove clean installation and upgrade on supported platforms. Enable mobile signing/push credentials for the new application identities where mobile distribution is in scope.

**If we skip it:** Installation, updates, and reproducible deployments remain developer operations. Actual external registry contents and signing accounts were not checked.

## Should fix

### 14. Recovery needs an exercised operating procedure

**What this is:** A working workspace depends on its database, media/Git objects, identities, and external services. The Compose helper currently prints a [backup checklist](../../deploy/compose/run.sh#L38).

**Problem:** The inspected deployment bundle does not establish scheduled backups or demonstrate restoration of a consistent workspace and ST8 ledger/anchor state. A checklist is useful but does not prove recovery after machine loss.

**Fix:** Automate backups appropriate to the deployment and rehearse restoration into an isolated environment. Verify identities, channel history, Git/media references, CU balances, compute receipts, and safe anchor retry behavior; document recovery time and acceptable data loss.

**If we skip it:** A functioning demo can still fail to recover from storage loss. This is an unverified operational requirement, not a claim that the user's external hosting has no backups.

### 15. Status documents no longer describe the current product

**What this is:** The repository combines upstream vision documents with ST8-specific architecture/runbooks.

**Problem:** [README](../../README.md#L10) describes an initial isolated slice, while the source includes relay/database integration and compute. [The upstream audit](UPSTREAM-AUDIT.md#L35) says rate limiting is absent, but [shared Redis admission](../../crates/buzz-relay/src/admission.rs#L17) is implemented. Other vision status tables lag project grouping, remote providers, and push infrastructure.

**Fix:** Publish one versioned readiness matrix with four distinct states: implemented, automated-tested, live-tested, and released. Separate the inherited Buzz vision from the committed ST8WRX release scope.

**If we skip it:** Development effort will be directed toward already-built components while incomplete integration and operations are overlooked.

## Completion sequence

1. **Reliable developer milestone:** fix independent header trust, compute projection recovery, useful result delivery, job termination/retry, and CI coverage. Re-run both stored proof examples and adversarial tests.
2. **Usable private beta:** finish workflow gates/actions, deliver one desktop contribution/compute flow, run the anchor worker automatically, and prove clean setup, upgrades, and restore. Keep the economic scope explicitly testnet-only if payments remain absent.
3. **Paid multi-user compute:** complete dispute outcomes, provider payments, requester/provider separation, per-job concurrent metering, and the contribution snapshot growth path. Validate real independent machines and identities.
4. **Full venture-network vision:** build ST8 Market discovery/listing/exchange flows and an evidence-based reputation system. General CPU/GPU/agent workload enum values and reputation-reference fields are foundations, not finished services. These are future scope, not prerequisites for a narrow collaboration/contribution beta.

Before calling the chosen release fully functional, demonstrate one clean end-to-end scenario: install → create/join community → invite another identity → collaborate and produce evidence → claim → independently approve → show CU ledger → execute compute and deliver its answer → show correct cost → dispute/resolve or settle → anchor and verify → restart → restore from backup without changing balances or paying twice. Include private-channel/community isolation, lost connections, duplicate events, and service failures.

## Validation and limits

| Check run against this checkout | Result |
|---|---|
| Tests across all seven ST8 crates | 58 passed; no failures |
| `buzz-db` library tests | 105 passed; 192 infrastructure-dependent tests ignored |
| Desktop JavaScript tests | 5,397 passed; no failures or skips |
| Desktop TypeScript check | Passed |
| Desktop lint and typography/pubkey guards | Passed; two warnings and two informational diagnostics |
| Web lint/guard and production build | Passed; large-bundle advisory |
| Admin lint/type check/unit command | Passed; unit runner explicitly found no test files; browser tests were not run |
| Mobile static analysis | No issues |
| Mobile tests | 1,661 passed |
| Rust workspace format check | Passed |
| Fresh build of `st8-anchor-worker` | Passed |
| Offline verification of both retained contribution and compute BSV receipts | Passed |
| Adversarial contribution receipt with altered block nonce/hash | Incorrectly accepted as `mined_spv_verified`; the altered header fails its declared proof-of-work target |

The altered receipt was created only under `/tmp`; the checked-in proof artifacts were not modified. This demonstrates finding 1 without wallet access or a network transaction. Test logs are retained under `/tmp/st8wrx-audit-*.log`. No production deployment, wallet funding, payment, or blockchain broadcast was performed.

**Not checked:** every source line; full `just ci`; live Postgres/Redis/relay integration; real desktop/mobile device workflows; remote Kubernetes deployment; live multi-node inference; current external registry/releases; production signing/push credentials; dependency vulnerability feeds; load/performance targets; actual backup restoration. Findings above distinguish direct source evidence from exercised checks and future product scope.

**Verdict:** finish proof authentication, durable compute recovery, and useful compute result delivery first; then complete the user workflow and operated release before presenting ST8WRX as a production venture network.
