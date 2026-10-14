# Buzz Docker Compose deployment

This is the single-node/VPS deployment bundle. It is intentionally separate from
the root `docker-compose.yml`, which remains local development infrastructure.

## Quick start

```bash
cd deploy/compose
cp .env.example .env
$EDITOR .env       # replace every CHANGE_ME value
./run.sh start
```

For a public VPS with automatic Let's Encrypt certificates:

```bash
cd deploy/compose
BUZZ_COMPOSE_TLS=true ./run.sh start
```

The bootstrap script should eventually replace manual `.env` editing for normal
users. It is responsible for generating stable secrets and, optionally, an owner
keypair.

## Production notes

- Requires Docker Compose v2.24.4 or newer; the TLS override uses Compose's
  `!reset` tag to remove the direct relay port when Caddy terminates HTTPS.
- Default `BUZZ_IMAGE` tracks `ghcr.io/blocpod/st8wrx:main` for early testing. Pin it to `ghcr.io/blocpod/st8wrx:sha-<7>` or a semver release tag for production once available.
- Keep `BUZZ_RELAY_PRIVATE_KEY`, `BUZZ_GIT_HOOK_HMAC_SECRET`, database/Redis,
  and S3 secrets stable across restarts.
- `RELAY_OWNER_PUBKEY` is intentionally not prefixed with `BUZZ_`; it must be a
  64-character hex Nostr pubkey when closed relay mode is enabled.
- `BUZZ_AUTO_MIGRATE` is opt-in. Set `BUZZ_AUTO_MIGRATE=true` or run
  `buzz-admin migrate` before starting the relay when bootstrapping a fresh
  database. Auto-migration requires an image that includes embedded SQLx
  migrations.
- The stack uses Postgres, Redis, MinIO, and a git data volume because
  those are real Buzz dependencies today. Minimal mode can simplify this later.
- The bundled Compose stack fixes the relay endpoint to `http://minio:9000` and
  `BUZZ_S3_ADDRESSING_STYLE=path`: Docker DNS resolves `minio`, not
  `<bucket>.minio`. It is not configurable for an external S3 provider through
  `.env`; use the Helm chart or a custom Compose configuration for providers
  such as new Railway Storage Buckets that require `virtual` addressing.

Run `./run.sh backup-hint` for the backup checklist.

## ST8WRX testnet anchor workers

The image includes `st8-anchor-worker`. Set `ST8_WALLET_URL` in `.env` to an
external **testnet** BRC-100 wallet endpoint reachable from the containers, then
start both durable queues with:

```bash
COMPOSE_PROFILES=st8-anchor ./run.sh start
```

This explicitly enables transaction signing and broadcasting through that wallet.
The contribution and compute queues run independently so waiting for one mined
transaction does not block the other. Workers require mined evidence, poll every
`ST8_ANCHOR_POLL_SECONDS` (default 15), and use persisted database leases,
transactions, receipts, and retry state across restarts. A terminated attempt
with saved transaction bytes can resume after its lease expires. An ambiguous
wallet call without saved bytes is fenced off until the operator imports the
original transaction; see [anchor recovery](../../docs/st8wrx/ANCHOR-RECOVERY.md).
Stop old workers before applying migration 0037 and start only updated workers
afterward. Logs go to container stdout/stderr; use
`docker compose logs contribution-anchor compute-anchor` to inspect failures.
The relay must finish migrations and become healthy before either worker starts.

These workers anchor ledger commitments; they do not pay compute providers.
The bundled worker includes the BSV SDK under the Open BSV license; the image
includes `LICENSE`, `NOTICE`, and that license under `/usr/share/doc/st8wrx`.
See [proof verification](../../docs/st8wrx/PROOF-VERIFICATION.md) for the trust
boundary of saved receipts. Production image publishing remains pending the
[release checklist](../../RELEASING.md); build an image from this checkout and
set `BUZZ_IMAGE` to that image when validating locally.

## Validation

Before sharing an install link publicly, verify a fresh install with:

```bash
cd deploy/compose
cp .env.example .env
$EDITOR .env
./run.sh config
./run.sh start
curl -fsS "http://127.0.0.1:$(grep -E '^BUZZ_HTTP_PORT=' .env | cut -d= -f2-)/_liveness"
./run.sh status
```
