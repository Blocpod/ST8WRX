# ST8 Compute protocol and operations

ST8 Compute turns real project work executed through Buzz's existing MeshLLM
substrate into independently verifiable accounting. Compute expense and provider
earnings are integer BSV satoshis. They are separate from non-transferable
Contribution Units and are never represented as a token or equity interest.

## Trust and identity boundaries

- The requester Nostr identity signs the private-input commitment, exact project,
  model, provider, Mesh owner, immutable pricing ID, maximum cost, expiry, and
  replay nonce.
- The agent or provider Nostr identity signs the terminal result commitment.
- The provider Nostr identity signs pricing, capability, and receipt events.
- The persistent Mesh owner Ed25519 key separately signs capability and receipt
  bodies. Its owner ID is SHA-256 of the 32-byte public key.
- The kind-30621 project owner signs settlement state.
- A BRC-100 external wallet funds and signs the BSV transaction. ST8WRX never
  accepts wallet private keys, WIFs, mnemonics, or seed phrases.

Prompts and model outputs are sent only to the configured local Mesh ingress.
Buzz events and ledger rows retain their SHA-256 commitments, model, status,
measurements, identities, and signed linkage—not the private bodies.

## Event kinds

| Kind | Author | Meaning |
| --- | --- | --- |
| 30626 | provider | immutable project/node pricing |
| 30627 | provider + embedded Mesh signature | current node capability |
| 49811 | requester | exact compute authorization |
| 49812 | provider + embedded Mesh signature | deterministic compute receipt |
| 49813 | requester or project owner | receipt dispute/freeze |
| 49814 | project owner | deterministic settlement |
| 49815 | project owner | final receipt-dispute arbitration |
| 30628 | relay only | queryable receipt-ledger projection |
| 30629 | relay only | queryable settlement/anchor projection |

The existing kinds 43001 and 43004 carry project-scoped signed job request and
terminal result evidence. `st8-input`, `st8-output`, and `st8-request` tags bind
private data commitments and the exact request/result relationship.

## Metering and pricing

`st8-compute run` takes pre/post snapshots from MeshLLM `/api/models` and calls
the real `/v1/chat/completions` ingress. A completed receipt is produced only
when exactly one local routing target advances by one attempt and one success,
and its completion-token delta exactly equals the OpenAI response usage. A
remote target, concurrent target changes, counter reset, missing usage, or token
disagreement fails closed. Input/output tokens come from target response usage;
wall time comes from a monotonic host clock. Missing signals are omitted.

Before sending private input, the runtime also requires `/api/status` to prove
that the exact model is ready on this host, the node is private and serving,
proxying is disabled, and there are no remote peers. This prevents a remote
fallback from seeing private input before post-execution attribution can reject
it.

Pricing is immutable and versioned. Each line is
`ceil(quantity * rate_sats / units_per_rate)` using checked integer arithmetic.
The signed job caps total cost. Failed jobs are free unless the signed policy
explicitly makes observed failed usage billable.

## Ledger, disputes, and settlement

Relay projection is idempotent and community/project fenced. Database uniqueness
constraints reject a second job for one request, a second receipt for one job or
result, and reuse of a receipt in another settlement. A valid dispute atomically
freezes an unsettled receipt. Settlement takes a cross-process project advisory
lock and atomically consumes the exact terminal, undisputed receipt set. This
includes failed or cancelled work so a policy that explicitly bills observed
failed usage cannot create charges that are permanently stranded outside a
settlement.

The project owner arbitrates compute disputes, matching the existing settlement
signing authority. Contribution Unit founder thresholds do not authorize compute
arbitration. A resolution binds the exact accepted dispute event and receipt:
`uphold_charge` rejects the dispute and releases the full original charge;
`void_charge` upholds the dispute and permanently excludes the charge. One final
signed decision is allowed per receipt, with no partial adjustment, repricing,
reopening, or reversal after settlement. The original signed receipt and metered
cost remain immutable. Ledger projections retain the signed resolution and
`billable_cost_sats`; provider balances report voided amounts separately from
earnings. Database states `resolved_rejected` and `resolved_upheld` refer to the
**dispute**, respectively, not the charge.

For compatibility with historical settlement signatures, `disputed_receipt_ids`
contains both unresolved disputes and final void decisions. A voided receipt
remains excluded; a released charge can be consumed exactly once. A period with
no billable receipts needs no settlement or blockchain transaction.

Retained inputs are replayed at startup and during the existing reconciliation
interval, in job, receipt, dispute, resolution, settlement order. Missing
dependencies are retried later; malformed inputs cannot block unrelated work.
Database outages stop replay before settlement. Exact duplicate event delivery
retries projection without repeating the input broadcast. Already committed
settlements preserve their original signed input and worker state.

The settlement contains sorted receipt IDs, explicit disputed exclusions,
provider/node balances, integer total satoshis, and a project-scoped receipt
Merkle root. A prepared BSV testnet payload commits the project and exact signed
settlement ID. Normal collaboration and compute execution never wait for the
wallet or blockchain.

## Milestone 1 live proof

The checked-in [`compute-m1`](compute-m1/) artifacts are the public proof from a
real private MeshLLM execution and external-wallet BSV testnet settlement:

- project: `30621:56abd2fea6e0e00d0a9bcee7ec841f355275a87c89ea72b2d95f154bb2877a41:st8wrx-milestone-1`
- requester: `nostr:b9935e323037bbbe9e851a9e5b146f34b586a347d7151ba4801d84b4c10f49d1`
- provider: `nostr:d0f3662fae720bd692fbd0d17bb349bfd34e2da427b5d2b4b5c200e606996c06`
- Mesh owner: `9e2be75f5ec04f38cf4125e51ad4fca8ff88a2b82506d8e06715a93514ac56a5`
- job: `99562e39a1eca96af30715a1ce790af28e47471645b1386ece52cddfb4d8cb22`
- compute receipt: `15f52dd21b90c348fec96563d99cd36983dab0d799d2291a9074a13339aef90f`
- settlement: `ae9837d81d353852e11210be0539a538d1dfc0a2933d9cef995bfa384f9fb3e3`
- receipt Merkle root: `95bc6ed5b1b741fb49513402662d242b93f963ab09f78927ed7e8cb8972f1463`
- billed usage: 27 input tokens, 102 output tokens, 2,140 ms, one job;
  total 6 satoshis
- BSV testnet transaction:
  [`8fbc7c1091b45536c72039abb6f1e82510e77dc2371bc6ab5b823c60ed09ff6a`](https://test.whatsonchain.com/tx/8fbc7c1091b45536c72039abb6f1e82510e77dc2371bc6ab5b823c60ed09ff6a)
- mined proof: block 1,754,555, block Merkle root
  `de7408eeb05803fe553348916c0db513bb19315f720db28aa9014aac1e9d960f`,
  verification state `mined_spv_verified`

The prompt and model output are intentionally absent. Their signed SHA-256
commitments remain in the receipt. The live capability has a bounded expiry and
is now intentionally excluded from fresh discovery; the archived signed
capability and immutable pricing evidence remain in this proof bundle.

## Commands

Build the runtime and anchor worker:

```bash
cargo build --release -p st8-compute-runtime -p st8-anchor-worker
```

The runtime reads Nostr secrets only from environment variables. It never reads
wallet secrets:

```bash
export ST8_PROVIDER_PRIVATE_KEY='<nsec-or-hex>'
export ST8_REQUESTER_PRIVATE_KEY='<nsec-or-hex>'
export ST8_PROJECT_OWNER_PRIVATE_KEY='<nsec-or-hex>'

st8-compute publish-pricing --help
st8-compute serve --help
st8-compute publish-capability --help
st8-compute run --help
st8-compute verify-receipt --help
st8-compute dispute --help
st8-compute resolve --help
st8-compute settle --help
```

The agent-first `buzz` CLI queries the running relay's project-scoped compute
surface. Receipt projections include metering, price, terminal/dispute state,
and settlement state; balances keep satoshi accounting explicitly separate
from Contribution Units:

```bash
buzz compute nodes --project '30621:<owner>:<slug>'
buzz compute jobs --project '30621:<owner>:<slug>'
buzz compute receipts --project '30621:<owner>:<slug>' --status completed
buzz compute balances --project '30621:<owner>:<slug>'
buzz compute settlements --project '30621:<owner>:<slug>'
buzz compute show-receipt <receipt-id>
buzz compute resolve --project '30621:<owner>:<slug>' \
  --receipt-id '<receipt-id>' --dispute-event-id '<dispute-event-id>' \
  --outcome uphold_charge --reason 'The signed authorization and usage agree'

```

The external-wallet worker uses the same `ST8_WALLET_URL` BRC-100 boundary as
Contribution Provenance:

```bash
st8-anchor-worker run-compute-once --receipt-out compute-settlement-receipt.json
st8-anchor-worker verify-compute --receipt compute-settlement-receipt.json
st8-anchor-worker refresh-compute-receipt --receipt compute-settlement-receipt.json
```

The verifier recomputes every Nostr and Mesh signature, project/job/result
binding, runtime measurement price, settlement receipt set and balances,
receipt Merkle root, BSV payload, raw-transaction txid, exact output script,
Atomic BEEF subject, independent testnet observation, and available mined
BUMP/header proof.

## Private answer delivery and interrupted runs

A local run retains the useful answer separately from the public accounting
artifact. Both output directories must already exist:

```bash
st8-compute run \
  --project-event project.json --pricing-event pricing.json \
  --prompt-file private-prompt.txt --model '<hosted-model>' \
  --relay-url 'ws://127.0.0.1:3000' \
  --max-cost-sats 100 --max-output-tokens 512 --expires-in-secs 600 \
  --private-output private-answer.txt --output receipt.json
st8-compute resume --state receipt.state.json
```

The private answer is exclusively created, mode 0600 on Unix, and is never
included in signed events or public receipts. The token budget and remaining
job expiry bound the request. Preflight rejects an unaffordable fixed/minimum
charge plus the requested output allowance; actual input and wall-time charges
still undergo the final signed-cap check. Only literal loopback Mesh endpoints
are accepted and private inference does not follow HTTP redirects.

Signed events and their publication cursor are durably saved before publishing.
Resume requires the original requester/provider identities, republishes saved
pending events, and never repeats inference. An interrupted run without retained
terminal evidence becomes cancelled. Failed or unmeasurable runs retain terminal
job-result evidence without inventing a billable compute receipt. An HTTP
cancellation may not immediately stop native generation. See the
[runtime recovery guide](../../crates/st8-compute-runtime/README.md) for lock
recovery and the remaining local-delivery limitations.

## Recovery and arbitration tests

Run the PostgreSQL-backed replay/resolution tests against an isolated migrated
test database. They cover storage-only crashes, delayed dependencies, pagination,
unauthorized signers, final-decision conflicts, settlement races, and tenant
isolation. The test suite applies migrations and creates unique test communities.

```bash
BUZZ_COMPUTE_REPLAY_TEST_DATABASE_URL='postgres://.../st8_compute_test' \
  cargo test -p buzz-relay --lib handlers::compute::tests -- --ignored
```
