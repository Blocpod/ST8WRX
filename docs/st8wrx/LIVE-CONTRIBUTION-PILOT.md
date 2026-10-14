# Isolated contribution relay pilot

`scripts/st8-contribution-live-pilot.mjs` checks the signed contribution flow
through the real HTTP bridge and WebSocket relay. It uses the desktop's installed
`nostr-tools`, Node's built-in WebSocket, and ephemeral founder keys kept only in
memory. Its saved state contains public event references, never signing keys.

Use a current `cargo build -p buzz-relay --bin buzz-relay` binary. Start it with a
new, dedicated PostgreSQL database and isolated Redis instance. Set a non-default
loopback port, matching `RELAY_URL`, separate health/metrics ports and git scratch
directory, and `BUZZ_AUTO_MIGRATE=true`. This pilot uses the open development
admission mode (`BUZZ_REQUIRE_AUTH_TOKEN=false` and
`BUZZ_REQUIRE_RELAY_MEMBERSHIP=false`); its actual requests still sign NIP-98 and
NIP-42 authentication. Preserve the same relay key and database on restart.
Do not run an anchor worker or point this pilot at a working community.

After starting the isolated relay on `127.0.0.1:3039`:

```sh
. ./bin/activate-hermit
ST8_PILOT_CONFIRM_ISOLATED=1 node scripts/st8-contribution-live-pilot.mjs \
  seed http://127.0.0.1:3039 /tmp/st8-pilot-public-state.json
```

The script creates a project, owner-signed governance policy, evidence-backed
claim, decision proposal, and two founder approvals. It asserts that one approval
cannot award units, a duplicate final approval cannot create a second award,
and HTTP and authenticated WebSocket queries return the same signed ledger
entry awarding seven Contribution Units. It does not transfer money, execute a
wallet call, or broadcast a blockchain transaction.

Stop only this relay process, restart the current binary with the same database
and relay key, then run:

```sh
ST8_PILOT_CONFIRM_ISOLATED=1 node scripts/st8-contribution-live-pilot.mjs \
  verify http://127.0.0.1:3039 /tmp/st8-pilot-public-state.json
```

Verification checks the exact ledger event ID, units, claim, and approval set
remain unchanged. This covers process restart and real protocol boundaries;
it does not exercise the Tauri native bridge, closed-community onboarding,
BSV confirmation, or recovery from a crash during wallet submission.

## Recorded local pilot — October 13, 2026

Executed against the current debug relay built from this checkout, PostgreSQL
`st8_contribution_livepilot_20261013` on `/tmp`, dedicated Redis on port 16379,
and a dedicated MinIO bucket `st8-beta-pilot` on port 19000. Relay ports were
3039/8089/9209. The full startup after adding MinIO passed the object-store A3
conformance probe with 32 concurrent writers and three rounds (two transport
drops reported by the probe). No shared database or storage volume was used.

Build command: `cargo build -j2 -p buzz-relay --bin buzz-relay`.
SHA-256 of `target/debug/buzz-relay`:
`62aa3c137851b45d660cc711cfe1e94f11cbae4763cd0c32a59b366c1e8838e0`.

```sh
. ./bin/activate-hermit
ST8_PILOT_CONFIRM_ISOLATED=1 node scripts/st8-contribution-live-pilot.mjs \
  seed http://127.0.0.1:3039 /tmp/st8-contribution-livepilot-20261013/public-state.json
# Stop/restart only the isolated relay, preserving its generated relay key.
ST8_PILOT_CONFIRM_ISOLATED=1 node scripts/st8-contribution-live-pilot.mjs \
  verify http://127.0.0.1:3039 /tmp/st8-contribution-livepilot-20261013/public-state.json
```

Both modes passed their HTTP and WebSocket assertions. The exact signed ledger
ID remained `cf39717922578c7c7ae8ba51fbe953a5cec14aee3fe7d11f3a0690e53ed330ac`
with seven units. SQL independently counted one contribution, one project
snapshot, two governance approvals, and one queued anchor job. The relay and
owned MinIO container were stopped afterward; fixture artifacts were retained.
The initial seed used the existing object-store probe opt-out while MinIO was
being provisioned; restart verification used the normal enabled probe.
