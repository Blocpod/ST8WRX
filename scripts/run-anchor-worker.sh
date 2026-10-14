#!/bin/sh
# Each worker claims a durable DB lease; failed jobs keep their persisted retry state.
set -eu

case "${1:-}" in
  run-once|run-compute-once) queue=$1 ;;
  *) echo "Expected run-once or run-compute-once" >&2; exit 2 ;;
esac
: "${DATABASE_URL:?DATABASE_URL must be configured}"
: "${ST8_WALLET_URL:?ST8_WALLET_URL must point to an external testnet wallet}"
poll_seconds=${ST8_ANCHOR_POLL_SECONDS:-15}
case "$poll_seconds" in
  ''|*[!0-9]*|0*) echo "ST8_ANCHOR_POLL_SECONDS must be a positive integer without leading zeroes" >&2; exit 2 ;;
esac
command -v st8-anchor-worker >/dev/null 2>&1 || {
  echo "st8-anchor-worker is not installed" >&2
  exit 2
}

child=
stop() {
  if [ -n "$child" ]; then
    kill -TERM "$child" 2>/dev/null || true
    wait "$child" 2>/dev/null || true
  fi
  exit 0
}
trap stop TERM INT

while :; do
  st8-anchor-worker "$queue" --require-mined true &
  child=$!
  if wait "$child"; then
    :
  else
    echo "Anchor attempt failed; durable queue will retry" >&2
  fi
  child=
  sleep "$poll_seconds" &
  child=$!
  wait "$child"
  child=
done
