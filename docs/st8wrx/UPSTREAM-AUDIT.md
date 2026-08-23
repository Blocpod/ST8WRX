# Buzz baseline audit

Audited snapshot: Buzz Desktop 0.5.18-era source supplied for the ST8WRX import.

## What is newer than the initial ST8WRX assumptions

- Multi-repository projects are no longer only a design: NIP-MP kind 30621 is
  accepted by the relay and supported by `buzz-sdk`, `buzz-cli`, desktop mocks,
  and project navigation.
- Buzz Mesh shared compute ships through `buzz-relay-mesh` and desktop Share
  Compute surfaces. ST8 Compute should extend its receipts and accounting rather
  than replace discovery or inference transport.
- Agent work already has structured activity/provenance surfaces and NIP-AM turn
  metrics. ST8WRX should translate selected signed events into evidence instead
  of inventing parallel telemetry.
- The relay has mature multi-community isolation, Nostr auth, Git smart HTTP,
  NIP-34 events, search, audit chains, media storage, workflows, CLI/MCP agent
  operations, desktop, mobile, and web clients.
- Workflow approval storage/API/UI exists, but executor suspension/resumption is
  still not wired end to end. The first contribution slice therefore uses an
  isolated explicit governance policy rather than pretending that gap is closed.

## Reused machinery

| Existing subsystem | ST8WRX use |
|---|---|
| Signed Nostr events | evidence identity and authorship |
| NIP-MP project + NIP-34 repositories | project scope and Git evidence |
| Agent turn metrics/activity | future AgentWork evidence |
| Buzz Mesh | future compute metering source |
| Workflow traces/approvals | future milestone and policy evidence |
| Audit hash chain | dispute history and operational audit |
| Blossom/S3 media | large content-addressed evidence |
| Search | evidence discovery, not value judgment |

## Known upstream gaps relevant to ST8WRX

- Workflow approval resume is incomplete.
- No production rate limiter is implemented.
- Several workflow actions remain stubbed.
- BSV wallet, settlement, contribution accounting, and economic governance are
  intentionally absent upstream.

## Removed operational baggage

The import removed all inherited publishing workflows: Block/Buzz release taggers,
desktop/mobile candidate promotion, signed and unsigned canaries, GHCR relay and
Sprig publication, and Helm publication. It also removed inherited release state,
Block CODEOWNERS, Block/Builderlab hosted-community commands, upstream screenshot
and production-agent mutation scripts, and the Block-specific `rust-s3` credential
fork override.

The remaining workflows are validation-only:

- `.github/workflows/ci.yml`
- `.github/workflows/benchmark-harbor.yml`
- `.github/workflows/mesh-lifecycle.yml`

No current workflow grants package write permission or publishes artifacts.
