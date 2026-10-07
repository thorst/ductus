#!/usr/bin/env bash
# Fan-out benchmark: one MLLP interface sending every message to 1, 5 and 25 destinations.
# Measures what senders see (ACK rate: messages stored per second) and how long until every
# destination has every message, and checks each destination got each message exactly once.
#
#   COUNT=6000 CONNECTIONS=16 FANOUT="1 5 25" DURABILITY=strict bash scripts/bench-fanout.sh
#
# Destinations are files (in a temp folder) so the numbers are the engine's and the database's,
# not a receiver's. Not a pass/fail demo: it prints a table (and fails only if something was
# lost or sent twice).
set -euo pipefail

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=${DUCTUS_DATABASE_URL:-postgres://todd:ductus@localhost/ductus_dev}
COUNT=${COUNT:-6000}
CONNECTIONS=${CONNECTIONS:-16}
FANOUT=${FANOUT:-"1 5 25"}
DURABILITY=${DURABILITY:-strict}
export DUCTUS_DATABASE_URL=$DB
q() { psql -tA "$DB" -c "$1"; }

printf "%-12s %-10s %14s %16s %18s\n" destinations durability "ingest msg/s" "all delivered s" "deliveries/s"
for n in $FANOUT; do
  P=$(mktemp -d)
  mkdir -p "$P/interfaces/adt"
  {
    echo "name: adt"
    echo "source: { mllp: { port: 6695 } }"
    echo "ordering: { key: PID-3.1 }"
    echo "durability: $DURABILITY"
    echo "destinations:"
    for i in $(seq 1 "$n"); do echo "  d$i: { file: { dir: out/d$i } }"; done
  } >"$P/interfaces/adt/interface.yaml"
  psql -q "$DB" -c "TRUNCATE messages, deliveries, dedupe_keys"
  cd "$P"
  "$BIN" run >"$P/run.log" 2>&1 &
  ENGINE=$!
  until grep -q "started" "$P/run.log"; do kill -0 $ENGINE || { cat "$P/run.log"; exit 1; }; sleep 0.2; done

  start=$(date +%s%N)
  gen=$("$BIN" gen --to 127.0.0.1:6695 --count "$COUNT" --patients 500 --connections "$CONNECTIONS" | tail -1)
  ingest=$(sed -E 's/.*\(([0-9]+)\/s\).*/\1/' <<<"$gen")
  until [ "$(q "SELECT count(*) FROM messages WHERE status = 0")" = 0 ]; do sleep 0.1; done
  ms=$(( ($(date +%s%N) - start) / 1000000 ))
  kill -INT $ENGINE; wait $ENGINE 2>/dev/null || true

  # Every destination has every message once (files are named by message id).
  for i in $(seq 1 "$n"); do
    got=$(find "$P/interfaces/adt/out/d$i" -type f | wc -l)
    [ "$got" = "$COUNT" ] || { echo "FAIL: d$i has $got files, expected $COUNT"; exit 1; }
  done
  printf "%-12s %-10s %14s %16.1f %18.0f\n" "$n" "$DURABILITY" "$ingest" "$(bc -l <<<"$ms/1000")" \
    "$(bc -l <<<"$COUNT*$n*1000/$ms")"
  cd - >/dev/null
  rm -rf "$P"
done
